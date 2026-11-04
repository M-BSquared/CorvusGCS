"""The operator's own elevation models: projections, readers, store and routes.

Hermetic: every grid is written by the test into a tmp dir, the network is
patched away, and nothing under the user's home is touched.
"""
from __future__ import annotations

import http.client
import json
import math
import os
import struct
import threading
import urllib.request
import zlib
from array import array

import pytest

from corvus import crs, dem_tiles
from corvus.custom_dem import CustomDemStore, lonlat_pixel, tile_lonlat
from corvus.dem_import import DemImportError, read_grid


# ---------------------------------------------------------------- projections

def test_transverse_mercator_matches_the_epsg_worked_example() -> None:
    # EPSG Guidance Note 7-2, British National Grid without the datum shift.
    from corvus.crs import _AIRY, _TransverseMercator
    proj = _TransverseMercator(_AIRY, -2.0, 0.9996012717, 400000.0, -100000.0, 49.0)
    east, north = proj.forward(0.5, 50.5)
    assert east == pytest.approx(577274.99, abs=0.05)
    assert north == pytest.approx(69740.50, abs=0.05)


def test_laea_matches_the_epsg_worked_example() -> None:
    east, north = crs.get(3035).from_lonlat(5.0, 50.0)
    assert east == pytest.approx(3962799.45, abs=0.01)
    assert north == pytest.approx(2999718.85, abs=0.01)


def test_utm_central_meridian_and_known_corner() -> None:
    east, _north = crs.get(32632).from_lonlat(9.0, 48.0)
    assert east == pytest.approx(500000.0, abs=1e-6)
    lon, lat = crs.get(25832).to_lonlat(696000.0, 5329000.0)
    assert lon == pytest.approx(11.63188, abs=1e-4)
    assert lat == pytest.approx(48.08411, abs=1e-4)


@pytest.mark.parametrize("epsg,lon,lat", [
    (4326, 11.6, 48.1), (3857, 11.6, 48.1), (32632, 11.6, 48.1), (32733, 18.4, -33.9),
    (25832, 11.6, 48.1), (25833, 13.4, 52.5), (4647, 11.6, 48.1), (26910, -122.4, 37.8),
    (31468, 11.6, 48.1), (27700, -0.13, 51.5), (2154, 2.35, 48.85), (3035, 10.0, 52.0),
    (2056, 7.44, 46.95), (21781, 8.54, 47.37),
])
def test_every_supported_system_round_trips(epsg: int, lon: float, lat: float) -> None:
    system = crs.get(epsg)
    assert system is not None
    x, y = system.from_lonlat(lon, lat)
    back = system.to_lonlat(x, y)
    # The Swiss formulas are approximations both ways, and the old datums
    # undo their shift with the negated parameters: centimetres, not microns.
    tolerance = 2e-5 if epsg in (2056, 21781) else 1e-6 if epsg in (31468, 27700) else 1e-7
    assert back[0] == pytest.approx(lon, abs=tolerance)
    assert back[1] == pytest.approx(lat, abs=tolerance)


def test_swiss_matches_the_swisstopo_example() -> None:
    # swisstopo's worked example for its approximate formulas.
    east, north = crs.get(2056).from_lonlat(8.0 + 43 / 60 + 49.79 / 3600, 46.0 + 2 / 60 + 38.87 / 3600)
    assert east == pytest.approx(2699999.76, abs=0.1)
    assert north == pytest.approx(1099999.97, abs=0.1)


def test_gauss_krueger_datum_shift_is_applied() -> None:
    # DHDN sits about 100 m from WGS84 in Bavaria; a missing shift would
    # put the round trip in the right place and the grid in the wrong one.
    from corvus.crs import _BESSEL, _TransverseMercator
    proj = _TransverseMercator(_BESSEL, 12.0, 1.0, 4500000.0, 0.0)
    shifted = crs.get(31468).from_lonlat(11.6, 48.1)
    unshifted = proj.forward(11.6, 48.1)
    gap = math.hypot(shifted[0] - unshifted[0], shifted[1] - unshifted[1])
    assert 50.0 < gap < 200.0


def test_unknown_systems_are_refused() -> None:
    assert crs.get(9999) is None
    assert crs.get("not a code") is None
    assert crs.get(None) is None


# ---------------------------------------------------------------- readers

def _lzw_encode(data: bytes) -> bytes:
    """TIFF LZW the way libtiff writes it, for feeding the reader."""
    out = bytearray()
    acc = 0
    nacc = 0

    def width(n: int) -> int:
        return 9 if n < 511 else 10 if n < 1023 else 11 if n < 2047 else 12

    def put(code: int, bits: int) -> None:
        nonlocal acc, nacc
        acc = (acc << bits) | code
        nacc += bits
        while nacc >= 8:
            nacc -= 8
            out.append((acc >> nacc) & 0xFF)
        acc &= (1 << nacc) - 1

    table = {bytes([i]): i for i in range(256)}
    nxt = 258
    decoded = 258
    first = True

    def emit(code: int) -> None:
        nonlocal decoded, first
        put(code, width(decoded))
        if first:
            first = False
        else:
            decoded += 1

    put(256, 9)
    w = b""
    for b in data:
        wc = w + bytes([b])
        if wc in table:
            w = wc
            continue
        emit(table[w])
        table[wc] = nxt
        nxt += 1
        w = bytes([b])
    if w:
        emit(table[w])
    put(257, width(decoded))
    if nacc:
        out.append((acc << (8 - nacc)) & 0xFF)
    assert nxt < 4094, "test data too large for an encoder without clear codes"
    return bytes(out)


def _write_tiff(path, width: int, height: int, values: list[float], *,
                compression: int = 1, predictor: int = 1, epsg: int | None = 25832,
                origin=(696000.0, 5329000.0), pixel=(1.0, 1.0), nodata: str | None = None,
                rows_per_strip: int | None = None, pixel_is_point: bool = False,
                bits: int = 32, fmt: int = 3, geo: bool = True) -> None:
    """A little-endian, single-band TIFF with GeoTIFF tags."""
    code = {(3, 32): "f", (1, 16): "H", (2, 16): "h"}[(fmt, bits)]
    rows_per_strip = rows_per_strip or height
    strips = []
    for r0 in range(0, height, rows_per_strip):
        rows = values[r0 * width:min(height, r0 + rows_per_strip) * width]
        if predictor == 2:
            diffed = []
            for r in range(len(rows) // width):
                line = rows[r * width:(r + 1) * width]
                diffed.append(line[0])
                diffed.extend((line[i] - line[i - 1]) & 0xFFFF for i in range(1, width))
            raw = array("H", diffed).tobytes()
        elif predictor == 3:
            raw = bytearray()
            size = 4
            for r in range(len(rows) // width):
                line = struct.pack(">" + "f" * width, *rows[r * width:(r + 1) * width])
                planes = bytearray(width * size)
                for k in range(size):
                    planes[k * width:(k + 1) * width] = line[k::size]
                for i in range(len(planes) - 1, 0, -1):
                    planes[i] = (planes[i] - planes[i - 1]) & 0xFF
                raw += planes
            raw = bytes(raw)
        else:
            raw = array(code, rows).tobytes()
        if compression == 8:
            raw = zlib.compress(raw)
        elif compression == 5:
            raw = _lzw_encode(raw)
        strips.append(raw)

    tags: list[tuple[int, int, list]] = [
        (256, 3, [width]), (257, 3, [height]), (258, 3, [bits]), (259, 3, [compression]),
        (262, 3, [1]), (273, 4, [0] * len(strips)), (277, 3, [1]),
        (278, 3, [rows_per_strip]), (279, 4, [len(s) for s in strips]),
        (284, 3, [1]), (317, 3, [predictor]), (339, 3, [fmt]),
    ]
    if geo:
        tags.append((33550, 12, [pixel[0], pixel[1], 0.0]))
        tags.append((33922, 12, [0.0, 0.0, 0.0, origin[0], origin[1], 0.0]))
        keys = [1, 1, 0, 0]
        entries = [(1024, 0, 1, 1), (1025, 0, 1, 2 if pixel_is_point else 1)]
        if epsg is not None:
            entries.append((3072, 0, 1, epsg))
        for e in entries:
            keys.extend(e)
        keys[3] = len(entries)
        tags.append((34735, 3, keys))
    if nodata is not None:
        tags.append((42113, 2, list(nodata.encode() + b"\x00")))
    tags.sort()

    fmts = {3: "H", 4: "I", 12: "d", 2: "B"}
    ifd_offset = 8
    ifd_size = 2 + 12 * len(tags) + 4
    extra_at = ifd_offset + ifd_size
    extra = bytearray()
    entries_raw = bytearray()
    blobs: dict[int, int] = {}
    for tag, typ, vals in tags:
        data = struct.pack("<" + fmts[typ] * len(vals), *vals)
        if len(data) <= 4:
            entries_raw += struct.pack("<HHI", tag, typ, len(vals)) + data.ljust(4, b"\x00")
        else:
            blobs[tag] = extra_at + len(extra)
            entries_raw += struct.pack("<HHII", tag, typ, len(vals), extra_at + len(extra))
            extra += data
            if len(extra) % 2:
                extra += b"\x00"
    strip_at = extra_at + len(extra)
    offsets = []
    pos = strip_at
    for s in strips:
        offsets.append(pos)
        pos += len(s)
    # Patch the strip offsets in place.
    off_data = struct.pack("<" + "I" * len(offsets), *offsets)
    if 273 in blobs:
        start = blobs[273] - extra_at
        extra[start:start + len(off_data)] = off_data
    else:
        index = [t[0] for t in tags].index(273)
        entries_raw[index * 12 + 8:index * 12 + 12] = off_data
    with open(path, "wb") as fh:
        fh.write(b"II*\x00" + struct.pack("<I", ifd_offset))
        fh.write(struct.pack("<H", len(tags)) + bytes(entries_raw) + b"\x00\x00\x00\x00")
        fh.write(bytes(extra))
        for s in strips:
            fh.write(s)


def _ramp(width: int, height: int) -> list[float]:
    return [500.0 + r + c * 0.5 for r in range(height) for c in range(width)]


@pytest.mark.parametrize("compression,predictor", [(1, 1), (8, 1), (5, 1), (8, 3), (5, 3)])
def test_geotiff_float_compressions(tmp_path, compression: int, predictor: int) -> None:
    path = tmp_path / "dom.tif"
    values = _ramp(32, 24)
    _write_tiff(path, 32, 24, values, compression=compression, predictor=predictor,
                rows_per_strip=7)
    grid = read_grid(str(path), "dom.tif")
    assert (grid.width, grid.height, grid.epsg) == (32, 24, 25832)
    assert (grid.x0, grid.y0, grid.dx, grid.dy) == (696000.0, 5329000.0, 1.0, 1.0)
    assert list(grid.values) == pytest.approx(values)


def test_geotiff_integer_with_horizontal_predictor(tmp_path) -> None:
    path = tmp_path / "dem16.tif"
    values = [int(400 + r * 3 + c) for r in range(16) for c in range(20)]
    _write_tiff(path, 20, 16, values, compression=8, predictor=2, bits=16, fmt=1)
    grid = read_grid(str(path))
    assert list(grid.values) == pytest.approx(values)


def test_geotiff_nodata_and_pixel_is_point(tmp_path) -> None:
    path = tmp_path / "gap.tif"
    values = _ramp(8, 8)
    values[9] = -9999.0
    _write_tiff(path, 8, 8, values, nodata="-9999", pixel_is_point=True, pixel=(2.0, 2.0))
    grid = read_grid(str(path))
    assert math.isnan(grid.values[9])
    # The tie point is a pixel centre: the corner is half a pixel out.
    assert (grid.x0, grid.y0) == (695999.0, 5329001.0)


def test_geotiff_without_georeferencing_is_refused(tmp_path) -> None:
    path = tmp_path / "plain.tif"
    _write_tiff(path, 8, 8, _ramp(8, 8), geo=False)
    with pytest.raises(DemImportError, match="no georeferencing"):
        read_grid(str(path))


def test_geotiff_jpeg_compression_is_named(tmp_path) -> None:
    path = tmp_path / "jpeg.tif"
    _write_tiff(path, 8, 8, _ramp(8, 8))
    data = bytearray(path.read_bytes())
    # Rewrite the compression tag's value from 1 to 7 (JPEG).
    count = struct.unpack("<H", data[8:10])[0]
    for i in range(count):
        at = 10 + 12 * i
        if struct.unpack("<H", data[at:at + 2])[0] == 259:
            data[at + 8:at + 10] = struct.pack("<H", 7)
    path.write_bytes(bytes(data))
    with pytest.raises(DemImportError, match="JPEG"):
        read_grid(str(path))


def test_a_file_refused_as_tiff_is_closed(tmp_path, monkeypatch) -> None:
    from corvus import dem_import
    opened = []

    def tracking_open(*args, **kwargs):
        fh = open(*args, **kwargs)
        opened.append(fh)
        return fh

    monkeypatch.setattr(dem_import, "open", tracking_open, raising=False)
    path = tmp_path / "junk.tif"
    path.write_bytes(b"XX" + bytes(30))
    with pytest.raises(DemImportError, match="not a TIFF"):
        dem_import.read_geotiff(str(path))
    assert opened and all(fh.closed for fh in opened)


def test_xyz_grid_any_order_with_header(tmp_path) -> None:
    path = tmp_path / "tile.txt"
    lines = ["X Y Z"]
    points = [(696000.5 + c, 5328999.5 - r, 540.0 + r + c / 10) for r in range(5) for c in range(6)]
    for x, y, z in reversed(points):
        lines.append(f"{x:.2f} {y:.2f} {z:.2f}")
    path.write_text("\n".join(lines) + "\n")
    grid = read_grid(str(path), "tile.txt")
    assert (grid.width, grid.height) == (6, 5)
    assert (grid.x0, grid.y0, grid.dx, grid.dy) == (696000.0, 5329000.0, 1.0, 1.0)
    assert grid.epsg is None
    assert grid.values[0] == pytest.approx(540.0)
    assert grid.values[5] == pytest.approx(540.5)
    assert grid.values[6] == pytest.approx(541.0)


def test_xyz_missing_points_become_nodata(tmp_path) -> None:
    path = tmp_path / "holes.xyz"
    rows = [f"{c}.5;{10 - r}.5;{100 + c}" for r in range(4) for c in range(4) if (r, c) != (1, 2)]
    path.write_text("\n".join(rows))
    grid = read_grid(str(path))
    assert math.isnan(grid.values[1 * 4 + 2])
    assert grid.values[0] == 100.0


def test_xyz_scattered_points_are_refused(tmp_path) -> None:
    path = tmp_path / "cloud.xyz"
    path.write_text("\n".join(f"{i * 1.37:.3f} {i * 0.91 % 7:.3f} 1" for i in range(60)))
    with pytest.raises(DemImportError, match="regular grid"):
        read_grid(str(path))


def test_esri_ascii_grid(tmp_path) -> None:
    path = tmp_path / "dem.asc"
    path.write_text("ncols 3\nnrows 2\nxllcorner 100\nyllcorner 200\ncellsize 5\n"
                    "NODATA_value -9999\n1 2 3\n4 -9999 6\n")
    grid = read_grid(str(path))
    assert (grid.width, grid.height, grid.x0, grid.y0, grid.dx) == (3, 2, 100.0, 210.0, 5.0)
    assert grid.values[0] == 1.0 and grid.values[5] == 6.0
    assert math.isnan(grid.values[4])


# ---------------------------------------------------------------- store

def _store_with_tile(tmp_path, values=None, size=40, **kw) -> tuple[CustomDemStore, dict]:
    path = tmp_path / "in.tif"
    _write_tiff(path, size, size, values or _ramp(size, size), **kw)
    store = CustomDemStore(str(tmp_path / "custom-dem"))
    model = store.import_file(str(path), "in.tif", "Test field")
    return store, model


def _tile_of(z: int, lon: float, lat: float) -> tuple[int, int]:
    px, py = lonlat_pixel(z, lon, lat)
    return int(px // 256), int(py // 256)


def test_import_describes_the_model(tmp_path) -> None:
    store, model = _store_with_tile(tmp_path)
    try:
        assert model["id"].startswith("dem_")
        assert model["name"] == "Test field"
        assert model["files"][0]["epsg"] == 25832
        assert model["files"][0]["crs_name"] == "ETRS89 / UTM zone 32N"
        w, s, e, n = model["bounds"]
        assert w == pytest.approx(11.63187, abs=1e-4)
        assert n == pytest.approx(48.08411, abs=1e-4)
        assert w < e and s < n
        assert model["maxzoom"] == 17
        assert model["min"] == 500.0
        entry = store.terrain_entries()[0]
        assert entry["encoding"] == "terrarium" and entry["tile_size"] == 256
        assert entry["custom"] is True
    finally:
        store.close()


def test_tile_inside_the_grid_carries_its_heights(tmp_path) -> None:
    flat = [612.25] * (200 * 200)
    store, model = _store_with_tile(tmp_path, values=flat, size=200)
    try:
        w, s, e, n = model["bounds"]
        z = 20
        x, y = _tile_of(z, (w + e) / 2, (s + n) / 2)
        blob, complete = store.tile(model["id"], z, x, y, base=lambda *_: [0.0] * 65536)
        assert complete
        heights = dem_tiles.heights(blob, "terrarium")
        assert min(heights) == pytest.approx(612.25, abs=0.01)
        assert max(heights) == pytest.approx(612.25, abs=0.01)
    finally:
        store.close()


def test_tile_samples_the_right_place(tmp_path) -> None:
    store, model = _store_with_tile(tmp_path)
    try:
        system = crs.get(25832)
        z = 19
        # The centre of grid cell (row 10, col 20).
        lon, lat = system.to_lonlat(696000.0 + 20.5, 5329000.0 - 10.5)
        x, y = _tile_of(z, lon, lat)
        blob, _ = store.tile(model["id"], z, x, y, base=lambda *_: [0.0] * 65536)
        heights = dem_tiles.heights(blob, "terrarium")
        px, py = lonlat_pixel(z, lon, lat)
        i = int(px - x * 256)
        j = int(py - y * 256)
        assert heights[j * 256 + i] == pytest.approx(500.0 + 10 + 20 * 0.5, abs=0.6)
    finally:
        store.close()


def test_outside_the_grid_the_base_fills_in(tmp_path) -> None:
    store, model = _store_with_tile(tmp_path, values=[800.0] * 1600)
    try:
        w, s, e, n = model["bounds"]
        z = 15
        x, y = _tile_of(z, (w + e) / 2, (s + n) / 2)
        blob, complete = store.tile(model["id"], z, x, y, base=lambda *_: [300.0] * 65536)
        assert complete
        heights = dem_tiles.heights(blob, "terrarium")
        assert min(heights) == pytest.approx(300.0, abs=0.01)
        assert max(heights) == pytest.approx(800.0, abs=0.01)
    finally:
        store.close()


def test_without_a_base_the_edge_is_held_and_the_tile_is_not_kept(tmp_path) -> None:
    store, model = _store_with_tile(tmp_path, values=[800.0] * 1600)
    try:
        w, s, e, n = model["bounds"]
        z = 15
        x, y = _tile_of(z, (w + e) / 2, (s + n) / 2)
        blob, complete = store.tile(model["id"], z, x, y, base=None)
        assert not complete
        heights = dem_tiles.heights(blob, "terrarium")
        assert min(heights) == pytest.approx(800.0, abs=0.01)
    finally:
        store.close()


def test_far_from_the_grid_without_a_base_the_edge_is_still_held(tmp_path) -> None:
    store, model = _store_with_tile(tmp_path, values=[800.0] * 1600)
    try:
        w, s, e, n = model["bounds"]
        z = 15
        x, y = _tile_of(z, (w + e) / 2, (s + n) / 2)
        assert not store.covers(model["id"], z, x + 40, y + 7)
        blob, complete = store.tile(model["id"], z, x + 40, y + 7, base=None)
        assert not complete
        heights = dem_tiles.heights(blob, "terrarium")
        assert min(heights) == pytest.approx(800.0, abs=0.01)
        assert max(heights) == pytest.approx(800.0, abs=0.01)
    finally:
        store.close()


def test_an_incomplete_tile_is_reused_briefly_then_cut_again(tmp_path, monkeypatch) -> None:
    from corvus import custom_dem

    store, model = _store_with_tile(tmp_path, values=[800.0] * 1600)
    try:
        w, s, e, n = model["bounds"]
        z = 15
        x, y = _tile_of(z, (w + e) / 2, (s + n) / 2)
        cuts = []
        real_render = custom_dem.render_tile

        def counting(*args, **kwargs):
            cuts.append(1)
            return real_render(*args, **kwargs)

        monkeypatch.setattr(custom_dem, "render_tile", counting)
        now = [1000.0]
        monkeypatch.setattr(custom_dem.time, "monotonic", lambda: now[0])
        first, _ = store.tile(model["id"], z, x, y, base=None)
        second, complete = store.tile(model["id"], z, x, y, base=None)
        assert second == first and not complete
        assert len(cuts) == 1, "the terrain and its shading ask for the same tile"
        now[0] += custom_dem.PROVISIONAL_TTL_S + 1
        store.tile(model["id"], z, x, y, base=None)
        assert len(cuts) == 2, "the stand-in is not kept past its time"
        # The default elevation arriving is never held up by a stand-in.
        _blob, complete = store.tile(model["id"], z, x, y, base=lambda *_: [300.0] * 65536)
        assert complete and len(cuts) == 3
    finally:
        store.close()


def test_coarse_tiles_read_the_overview(tmp_path) -> None:
    # A checkerboard of 0 and 100 m averages to 50 at any coarse zoom; point
    # sampling the full grid would hand back 0s and 100s.
    size = 256
    values = [100.0 * ((r + c) % 2) for r in range(size) for c in range(size)]
    store, model = _store_with_tile(tmp_path, values=values, size=size)
    try:
        w, s, e, n = model["bounds"]
        z = 14
        x, y = _tile_of(z, (w + e) / 2, (s + n) / 2)
        blob, _ = store.tile(model["id"], z, x, y, base=lambda *_: [50.0] * 65536)
        heights = dem_tiles.heights(blob, "terrarium")
        assert max(heights) == pytest.approx(50.0, abs=0.5)
        assert min(heights) == pytest.approx(50.0, abs=0.5)
    finally:
        store.close()


def test_models_survive_a_restart_and_more_files_join(tmp_path) -> None:
    store, model = _store_with_tile(tmp_path)
    second = tmp_path / "next.tif"
    _write_tiff(second, 40, 40, _ramp(40, 40), origin=(696040.0, 5329000.0))
    model = store.import_file(str(second), "next.tif", model_id=model["id"])
    assert len(model["files"]) == 2
    store.close()
    again = CustomDemStore(str(tmp_path / "custom-dem"))
    try:
        listed = again.list_models()
        assert [m["id"] for m in listed] == [model["id"]]
        assert len(listed[0]["files"]) == 2
        assert again.has(model["id"])
    finally:
        again.close()


def test_a_file_without_a_system_asks_for_one(tmp_path) -> None:
    path = tmp_path / "tile.txt"
    path.write_text("\n".join(f"{696000.5 + c} {5328999.5 - r} 540" for r in range(4) for c in range(4)))
    store = CustomDemStore(str(tmp_path / "custom-dem"))
    try:
        with pytest.raises(DemImportError, match="Coordinate system"):
            store.import_file(str(path), "tile.txt")
        model = store.import_file(str(path), "tile.txt", epsg=25832)
        assert model["files"][0]["epsg"] == 25832
        with pytest.raises(DemImportError, match="not a coordinate system"):
            store.import_file(str(path), "tile.txt", epsg=9999)
        # UTM numbers read as degrees are nowhere on Earth.
        with pytest.raises(DemImportError, match="not a place"):
            store.import_file(str(path), "tile.txt", epsg=4326)
    finally:
        store.close()


def test_rename_strips_markup_and_remove_deletes_the_files(tmp_path) -> None:
    store, model = _store_with_tile(tmp_path)
    try:
        assert store.rename(model["id"], "<b>Field</b> & \"hill\"") == "bField/b hill"
        folder = os.path.join(store.root, model["id"])
        assert os.path.isdir(folder)
        assert store.remove(model["id"])
        assert not os.path.exists(folder)
        assert not store.has(model["id"])
        assert store.tile(model["id"], 0, 0, 0) is None
        assert not store.remove(model["id"])
    finally:
        store.close()


def test_close_unmaps_and_refuses_imports(tmp_path) -> None:
    store, model = _store_with_tile(tmp_path)
    store.close()
    assert store.list_models() == []
    with pytest.raises(DemImportError):
        store.import_file(str(tmp_path / "in.tif"), "in.tif")


def test_tile_lonlat_inverts_lonlat_pixel() -> None:
    px, py = lonlat_pixel(17, 11.6385, 48.0795)
    lon, lat = tile_lonlat(17, px, py)
    assert (lon, lat) == pytest.approx((11.6385, 48.0795), abs=1e-9)


# ---------------------------------------------------------------- routes

@pytest.fixture
def dem_server(tmp_path, monkeypatch):
    """A live server with the elevation caches, a custom store and no network."""
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    from corvus import tile_sources
    from corvus.server import CorvusHandler, CorvusServer
    from corvus.tile_cache import TileCache

    def offline(*_a, **_k):
        raise OSError("offline in tests")

    monkeypatch.setattr(urllib.request, "urlopen", offline)
    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    caches = {sid: TileCache(str(cache_dir / f"{sid}.mbtiles"))
              for sid in tile_sources.all_sources()}
    store = CustomDemStore(str(cache_dir / "custom-dem"))
    saved = (CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
             CorvusHandler.tile_caches, CorvusHandler.custom_dem)
    CorvusHandler.store = None
    CorvusHandler.mavlink = None
    CorvusHandler.ssh = None
    CorvusHandler.tile_caches = caches
    CorvusHandler.custom_dem = store
    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    server.tile_caches = caches
    server.custom_dem = store
    thread = threading.Thread(target=server.serve_forever, daemon=True,
                              kwargs={"poll_interval": 0.05})
    thread.start()
    try:
        yield server, caches, store
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        store.close()
        for cache in caches.values():
            cache.close()
        (CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
         CorvusHandler.tile_caches, CorvusHandler.custom_dem) = saved


def _request(server, method: str, path: str, body: bytes | None = None,
             ctype: str = "application/json") -> tuple[int, bytes, dict]:
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=10)
    conn.request(method, path, body, {"Content-Type": ctype} if body is not None else {})
    resp = conn.getresponse()
    data = resp.read()
    headers = dict(resp.getheaders())
    conn.close()
    return resp.status, data, headers


def test_import_list_serve_and_remove_over_http(dem_server, tmp_path) -> None:
    server, caches, store = dem_server
    path = tmp_path / "field.tif"
    _write_tiff(path, 40, 40, [700.0] * 1600)
    status, body, _ = _request(server, "POST", "/api/dem/import?filename=field.tif&crs=auto&name=Field",
                               path.read_bytes(), "application/octet-stream")
    assert status == 200, body
    model = json.loads(body)["model"]
    assert model["name"] == "Field"

    status, body, _ = _request(server, "GET", "/api/dem")
    assert [m["id"] for m in json.loads(body)["models"]] == [model["id"]]

    status, body, _ = _request(server, "GET", "/api/tiles/sources")
    terrain = {t["id"]: t for t in json.loads(body)["terrain"]}
    assert terrain[model["id"]]["label"] == "Field"
    assert terrain[model["id"]]["provider"] == "custom"

    # The free default elevation around it, from the cache (the network is off).
    w, s, e, n = model["bounds"]
    z = 15
    x, y = _tile_of(z, (w + e) / 2, (s + n) / 2)
    caches["terrain"].put_tile(z, x, y, dem_tiles.encode_heights(256, [420.0] * 65536, "terrarium"))
    status, blob, headers = _request(server, "GET", f"/api/tiles/{model['id']}/{z}/{x}/{y}.png")
    assert status == 200
    assert headers["Content-Type"] == "image/png"
    assert "86400" in headers["Cache-Control"]
    heights = dem_tiles.heights(blob, "terrarium")
    assert min(heights) == pytest.approx(420.0, abs=0.01)
    assert max(heights) == pytest.approx(700.0, abs=0.01)

    # Far from the model, with nothing cached and no network, the model's edge
    # is held outward rather than leaving a hole MapLibre would draw at sea
    # level, and the browser is told to ask again soon.
    status, blob, headers = _request(server, "GET", f"/api/tiles/{model['id']}/{z}/{x + 50}/{y}.png")
    assert status == 200
    assert "86400" not in headers["Cache-Control"]
    heights = dem_tiles.heights(blob, "terrarium")
    assert min(heights) == pytest.approx(700.0, abs=0.01)
    assert max(heights) == pytest.approx(700.0, abs=0.01)

    # Where the default elevation is to be had, it still wins out there.
    caches["terrain"].put_tile(z, x + 50, y,
                               dem_tiles.encode_heights(256, [380.0] * 65536, "terrarium"))
    status, blob, headers = _request(server, "GET", f"/api/tiles/{model['id']}/{z}/{x + 50}/{y}.png")
    assert status == 200 and "86400" in headers["Cache-Control"]
    assert max(dem_tiles.heights(blob, "terrarium")) == pytest.approx(380.0, abs=0.01)

    status, body, _ = _request(server, "POST", "/api/dem/rename",
                               json.dumps({"id": model["id"], "name": "Hill"}).encode())
    assert status == 200 and json.loads(body)["name"] == "Hill"
    status, body, _ = _request(server, "POST", "/api/dem/remove",
                               json.dumps({"id": model["id"]}).encode())
    assert status == 200
    status, _, _ = _request(server, "GET", f"/api/tiles/{model['id']}/{z}/{x}/{y}.png")
    assert status == 404


def test_import_refusals_over_http(dem_server, tmp_path) -> None:
    server, _, store = dem_server
    status, body, _ = _request(server, "POST", "/api/dem/import?filename=x.txt",
                               b"not a grid at all\n", "application/octet-stream")
    assert status == 400
    assert json.loads(body)["ok"] is False
    status, body, _ = _request(server, "POST", "/api/dem/import?filename=x.tif&crs=abc",
                               b"II*\x00", "application/octet-stream")
    assert status == 400 and "EPSG" in json.loads(body)["error"]
    # No temporary upload is left behind, whatever the outcome.
    assert not [f for f in os.listdir(store.root) if f.endswith(".part")]


# ---------------------------------------------------------------- terrain or surface

def _town(size: int, houses: list[tuple[int, int]], height: float = 8.0) -> list[float]:
    values = [500.0 + 0.02 * r for r in range(size) for _c in range(size)]
    for x, y in houses:
        for r in range(y, y + 12):
            for c in range(x, x + 15):
                values[r * size + c] += height
    return values


def test_classify_tells_bare_ground_from_buildings() -> None:
    from corvus.custom_dem import classify
    size = 200
    assert classify(_town(size, []), size, size, 1.0) == "terrain"
    houses = [(10 + 40 * i, 10 + 40 * j) for i in range(4) for j in range(4)]
    assert classify(_town(size, houses), size, size, 1.0) == "surface"
    # A steep but continuous slope is still ground: 1.5 m per metre is under the wall limit.
    slope = [1.5 * c for _r in range(size) for c in range(size)]
    assert classify(slope, size, size, 1.0) == "terrain"


def test_classify_declines_coarse_or_tiny_grids() -> None:
    from corvus.custom_dem import classify
    size = 200
    houses = [(10, 10), (100, 100)]
    assert classify(_town(size, houses), size, size, 30.0) == "unknown"
    assert classify([500.0] * 100, 10, 10, 1.0) == "unknown"
    assert classify([math.nan] * (size * size), size, size, 1.0) == "unknown"


def test_model_kind_is_listed_and_mixed_when_files_disagree(tmp_path) -> None:
    size = 200
    flat = tmp_path / "flat.tif"
    town = tmp_path / "town.tif"
    _write_tiff(flat, size, size, _town(size, []), compression=8)
    houses = [(10 + 40 * i, 10 + 40 * j) for i in range(4) for j in range(4)]
    _write_tiff(town, size, size, _town(size, houses), compression=8, origin=(696200.0, 5329000.0))
    store = CustomDemStore(str(tmp_path / "custom-dem"))
    try:
        model = store.import_file(str(flat), "flat.tif")
        assert model["kind"] == "terrain"
        assert store.terrain_entries()[0]["kind"] == "terrain"
        model = store.import_file(str(town), "town.tif", model_id=model["id"])
        assert [f["kind"] for f in model["files"]] == ["terrain", "surface"]
        assert model["kind"] == "mixed"
    finally:
        store.close()


def test_models_from_before_the_kind_get_it_on_load(tmp_path) -> None:
    store, model = _store_with_tile(tmp_path, values=_town(40, []), size=40)
    store.close()
    path = os.path.join(str(tmp_path / "custom-dem"), model["id"], "model.json")
    with open(path, encoding="utf-8") as fh:
        meta = json.load(fh)
    meta.pop("kind")
    for g in meta["grids"]:
        g.pop("kind")
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(meta, fh)
    again = CustomDemStore(str(tmp_path / "custom-dem"))
    try:
        assert again.list_models()[0]["kind"] == "terrain"
        with open(path, encoding="utf-8") as fh:
            assert json.load(fh)["grids"][0]["kind"] == "terrain"
    finally:
        again.close()
