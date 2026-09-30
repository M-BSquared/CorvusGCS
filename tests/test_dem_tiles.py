"""Elevation tiles derived from coarser cached ones (corvus/dem_tiles.py).

Offline, an elevation tile that was never downloaded used to be a 404. MapLibre
then filled each hole from whichever coarser tile it had loaded, neighbours
from different ones, and the terrain showed steps and cracks along tile
borders. The backend now answers such a request from the nearest cached
ancestor, resampled to the zoom that was asked for.

What is pinned here: the PNG round trip is exact (a height read wrong is a
mountain in the wrong place), the resampling is continuous across siblings
(no new seams), the ancestor search, the serve route using it for elevation
and only for elevation, and that a derived tile is never written to disk.
Also the two upstream rules that kept 3D terrain from loading online, and the
elevation download reaching down to zoom 0.
"""
from __future__ import annotations

import http.client
import io
import struct
import threading
import urllib.error
import urllib.request
import zlib
from array import array

import pytest

from corvus import dem_tiles
from corvus.server import (
    TILE_UPSTREAM_FAIL_THRESHOLD,
    CorvusHandler,
    CorvusServer,
    _UpstreamBreaker,
)
from corvus.tile_cache import TileCache

SIZE = 256


def _terrarium(height_m: float) -> int:
    """Packed 24-bit Terrarium value for a height."""
    return int(round((height_m + 32768) * 256))


def _height(packed: int) -> float:
    return packed / 256 - 32768


def _grid(fn) -> array:
    """A SIZE x SIZE grid of packed heights from fn(col, row) in metres."""
    return array(dem_tiles._U32, (_terrarium(fn(c, r)) for r in range(SIZE) for c in range(SIZE)))


def _png(values: array, filters: list[int] | None = None, width: int = SIZE,
         height: int = SIZE, alpha: bool = False) -> bytes:
    """Encode with an explicit filter per row, the way real servers mix them."""
    channels = 4 if alpha else 3
    rows = []
    prev = bytes(width * channels)
    for r in range(height):
        line = bytearray()
        for c in range(width):
            v = values[r * width + c]
            line += bytes(((v >> 16) & 255, (v >> 8) & 255, v & 255))
            if alpha:
                line.append(255)
        kind = filters[r % len(filters)] if filters else 0
        rows.append(bytes([kind]) + _filter(kind, bytes(line), prev, channels))
        prev = bytes(line)
    header = struct.pack(">IIBBBBB", width, height, 8, 6 if alpha else 2, 0, 0, 0)

    def chunk(kind: bytes, body: bytes) -> bytes:
        return (struct.pack(">I", len(body)) + kind + body
                + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF))

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(b"".join(rows))) + chunk(b"IEND", b""))


def _filter(kind: int, line: bytes, prev: bytes, bpp: int) -> bytes:
    out = bytearray(len(line))
    for i, x in enumerate(line):
        a = line[i - bpp] if i >= bpp else 0
        b = prev[i]
        c = prev[i - bpp] if i >= bpp else 0
        if kind == 0:
            pred = 0
        elif kind == 1:
            pred = a
        elif kind == 2:
            pred = b
        elif kind == 3:
            pred = (a + b) >> 1
        else:
            pred = dem_tiles._paeth(a, b, c)
        out[i] = (x - pred) & 255
    return bytes(out)


# ---------------------------------------------------------------------------
# The codec
# ---------------------------------------------------------------------------

def test_every_png_filter_decodes_to_the_same_heights() -> None:
    values = _grid(lambda c, r: 600 + 3.25 * c - 1.5 * r + ((c * r) % 7) / 8)
    for filters in ([0], [1], [2], [3], [4], [0, 1, 2, 3, 4]):
        grid = dem_tiles.decode(_png(values, filters))
        assert grid is not None, f"filters {filters} were not decoded"
        assert grid.values == values, f"filters {filters} decoded wrong heights"


def test_alpha_is_ignored_rather_than_read_as_height() -> None:
    values = _grid(lambda c, r: 1234.5)
    grid = dem_tiles.decode(_png(values, [4], alpha=True))
    assert grid is not None and grid.values == values


def test_encode_then_decode_is_exact() -> None:
    values = _grid(lambda c, r: -412.75 + 11.0 * c + 7.5 * r)
    grid = dem_tiles.decode(dem_tiles.encode(SIZE, SIZE, values))
    assert grid is not None
    assert grid.values == values


@pytest.mark.parametrize("blob", [
    b"",
    b"<html>captive portal</html>",
    b"\xff\xd8\xff\xe0 a jpeg",
    b"\x89PNG\r\n\x1a\n truncated",
])
def test_anything_that_is_not_a_readable_png_is_declined(blob: bytes) -> None:
    assert dem_tiles.decode(blob) is None


def test_a_sixteen_bit_png_is_declined_not_misread() -> None:
    png = _png(_grid(lambda c, r: 0.0))
    # Rewrite IHDR's bit depth to 16 and fix its CRC.
    ihdr = bytearray(png[8:8 + 25])
    ihdr[8 + 8] = 16
    ihdr[-4:] = struct.pack(">I", zlib.crc32(bytes(ihdr[4:-4])) & 0xFFFFFFFF)
    assert dem_tiles.decode(png[:8] + bytes(ihdr) + png[33:]) is None


# ---------------------------------------------------------------------------
# The resampling
# ---------------------------------------------------------------------------

def test_a_plane_is_resampled_exactly() -> None:
    """Bilinear reproduces a plane, so a derived tile over a constant slope is
    the slope, not an approximation of it (away from the ancestor's edge)."""
    slope = lambda c, r: 800 + 2.0 * c + 1.0 * r  # noqa: E731
    parent = dem_tiles.decode(_png(_grid(slope)))
    child = dem_tiles.resample(parent, 1, 1, 0)   # the north-east quarter
    # Child pixel (i, j) centre in parent pixels: (128 + (i + 0.5) / 2 - 0.5, (j + 0.5) / 2 - 0.5).
    for j in (10, 100, 200):
        for i in (10, 100, 200):
            px = 128 + (i + 0.5) / 2 - 0.5
            py = (j + 0.5) / 2 - 0.5
            want = slope(px, py)
            got = _height(child[j * SIZE + i])
            assert abs(got - want) < 0.01, (i, j, got, want)


def test_siblings_meet_without_a_step() -> None:
    """The last column of one child and the first of its neighbour are one
    sample apart on the same surface: no seam is introduced between tiles cut
    from the same ancestor."""
    parent = dem_tiles.decode(_png(_grid(lambda c, r: 1000 + 40.0 * (c % 17) + 3 * r)))
    west = dem_tiles.resample(parent, 2, 1, 1)
    east = dem_tiles.resample(parent, 2, 2, 1)
    for row in (0, 64, 255):
        edge_w = _height(west[row * SIZE + SIZE - 1])
        edge_e = _height(east[row * SIZE])
        # One child pixel is a quarter of a parent pixel: at most a quarter of
        # the steepest parent step (40 m) between the two.
        assert abs(edge_w - edge_e) <= 10.01, (row, edge_w, edge_e)


def test_overzoom_rejects_an_offset_outside_the_ancestor() -> None:
    png = _png(_grid(lambda c, r: 0.0))
    assert dem_tiles.overzoom(png, 1, 2, 0) is None
    assert dem_tiles.overzoom(png, 0, 0, 0) is None
    assert dem_tiles.overzoom(b"nope", 1, 0, 0) is None


# ---------------------------------------------------------------------------
# The ancestor search
# ---------------------------------------------------------------------------

class _Store:
    """get_tile stand-in: a dict of (z, x, y) -> PNG, counting reads."""

    def __init__(self, tiles: dict) -> None:
        self.tiles = tiles
        self.reads = 0

    def __call__(self, z: int, x: int, y: int) -> bytes | None:
        self.reads += 1
        return self.tiles.get((z, x, y))


def test_the_nearest_ancestor_is_used() -> None:
    coarse = _png(_grid(lambda c, r: 100.0))
    fine = _png(_grid(lambda c, r: 900.0))
    store = _Store({(10, 545, 357): coarse, (12, 2181, 1430): fine})
    blob = dem_tiles.DemFallback().tile("terrain", 14, 8725, 5721, store)
    grid = dem_tiles.decode(blob)
    assert grid is not None
    assert _height(grid.values[0]) == pytest.approx(900.0), "z12 is nearer than z10"


def test_no_ancestor_means_no_tile() -> None:
    assert dem_tiles.DemFallback().tile("terrain", 13, 4000, 2800, _Store({})) is None


def test_an_unreadable_cache_is_a_miss_not_a_crash() -> None:
    def broken(z: int, x: int, y: int) -> bytes:
        raise OSError("database is locked")
    assert dem_tiles.DemFallback().tile("terrain", 13, 4000, 2800, broken) is None


def test_a_derived_tile_is_remembered_and_the_memory_is_bounded() -> None:
    parent = _png(_grid(lambda c, r: 500.0))
    store = _Store({(5, 16, 11): parent})
    fallback = dem_tiles.DemFallback(decoded_size=2, derived_size=3)
    first = fallback.tile("terrain", 7, 64, 44, store)
    reads = store.reads
    assert fallback.tile("terrain", 7, 64, 44, store) == first
    assert store.reads == reads, "a second ask is answered from memory"
    for x in range(64, 68):
        fallback.tile("terrain", 7, x, 45, store)
    assert len(fallback._derived) <= 3
    assert len(fallback._decoded) <= 2


def _column(values, col: int) -> list[float]:
    return [_height(values[r * SIZE + col]) for r in range(SIZE)]


def test_a_derived_tile_meets_a_real_neighbour_instead_of_standing_a_wall_off_it() -> None:
    """The edge of a downloaded area: real ground on one side, ground known
    only from a coarse tile on the other. Unblended, the two met in a wall
    hundreds of metres high with the imagery smeared down it."""
    coarse = _png(_grid(lambda c, r: 500.0))     # z10: 500 m everywhere
    real = _png(_grid(lambda c, r: 1300.0))      # z14, the western neighbour
    store = _Store({(10, 545, 357): coarse, (14, 8724, 5721): real})
    grid = dem_tiles.decode(dem_tiles.DemFallback().tile("terrain", 14, 8725, 5721, store))
    west = _column(grid.values, 0)
    east = _column(grid.values, SIZE - 1)
    middle = _column(grid.values, SIZE // 2)
    assert all(abs(h - 1300.0) < 0.01 for h in west[1:-1]), "meets the real tile exactly"
    assert all(abs(h - 500.0) < 0.01 for h in east), "the side with nothing new is left alone"
    assert all(850.0 < h < 950.0 for h in middle), "and it is a slope in between, not a step"


def test_two_derived_neighbours_meet_halfway() -> None:
    """Neighbours derived from ancestors of different resolution each bend
    half way, by the same rule, so they land on the same line."""
    fine = _png(_grid(lambda c, r: 900.0 + 2.0 * r))    # z12 over the western tile
    coarse = _png(_grid(lambda c, r: 300.0 + 1.0 * r))  # z9 over both
    store = _Store({(12, 2181, 1430): fine, (9, 272, 178): coarse})
    fallback = dem_tiles.DemFallback()
    west = dem_tiles.decode(fallback.tile("terrain", 14, 8727, 5721, store))
    east = dem_tiles.decode(fallback.tile("terrain", 14, 8728, 5721, store))
    for row in (8, 128, 247):
        w = _height(west.values[row * SIZE + SIZE - 1])
        e = _height(east.values[row * SIZE])
        assert abs(w - e) < 2.0, (row, w, e)


def test_forget_drops_one_source_only() -> None:
    parent = _png(_grid(lambda c, r: 500.0))
    fallback = dem_tiles.DemFallback()
    fallback.tile("terrain", 7, 64, 44, _Store({(6, 32, 22): parent}))
    fallback.tile("other", 7, 64, 44, _Store({(6, 32, 22): parent}))
    fallback.forget("terrain")
    assert all(k[0] == "other" for k in fallback._derived)
    assert all(k[0] == "other" for k in fallback._decoded)


# ---------------------------------------------------------------------------
# The serve route
# ---------------------------------------------------------------------------

@pytest.fixture
def tile_server(tmp_path, monkeypatch):
    """A live server with real terrain and imagery caches and no network."""
    caches = {
        "terrain": TileCache(str(tmp_path / "terrain.mbtiles")),
        "satellite": TileCache(str(tmp_path / "satellite.mbtiles")),
    }
    saved = (CorvusHandler.tile_caches, CorvusHandler.dem_fallback,
             CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh)
    CorvusHandler.tile_caches = caches
    CorvusHandler.dem_fallback = dem_tiles.DemFallback()
    CorvusHandler.store = None
    CorvusHandler.mavlink = None
    CorvusHandler.ssh = None
    monkeypatch.setattr(CorvusHandler, "_fetch_upstream_tile", lambda *a, **k: None)
    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True,
                              kwargs={"poll_interval": 0.05})
    thread.start()
    try:
        yield server, caches
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        for cache in caches.values():
            cache.close()
        (CorvusHandler.tile_caches, CorvusHandler.dem_fallback,
         CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh) = saved


def _get(server, path: str) -> tuple[int, dict, bytes]:
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=10)
    conn.request("GET", path)
    resp = conn.getresponse()
    body = resp.read()
    headers = {k.lower(): v for k, v in resp.getheaders()}
    conn.close()
    return resp.status, headers, body


def test_an_offline_elevation_hole_is_filled_from_its_ancestor(tile_server) -> None:
    server, caches = tile_server
    caches["terrain"].put_tile(12, 2181, 1430, _png(_grid(lambda c, r: 1500.0 + c)))
    status, headers, body = _get(server, "/api/tiles/terrain/14/8725/5721.png")
    assert status == 200
    assert headers["content-type"] == "image/png"
    assert headers["cache-control"] == "max-age=300", \
        "a stand-in must not outlive the link coming back"
    grid = dem_tiles.decode(body)
    assert grid is not None
    assert 1500.0 <= _height(grid.values[0]) <= 1756.0
    assert caches["terrain"].get_tile(14, 8725, 5721) is None, \
        "an interpolation is never stored: it would hide the real tile once online"


def test_a_cached_elevation_tile_is_served_as_itself(tile_server) -> None:
    server, caches = tile_server
    real = _png(_grid(lambda c, r: 42.0))
    caches["terrain"].put_tile(14, 8725, 5721, real)
    status, headers, body = _get(server, "/api/tiles/terrain/14/8725/5721.png")
    assert status == 200
    assert body == real
    assert headers["cache-control"] == "max-age=86400"


def test_elevation_with_nothing_above_it_is_still_a_404(tile_server) -> None:
    server, _ = tile_server
    status, _, _ = _get(server, "/api/tiles/terrain/14/8725/5721.png")
    assert status == 404


def test_imagery_is_never_derived(tile_server) -> None:
    """A blurred photograph is worse than the coarser one MapLibre already
    shows: only heights are filled in."""
    server, caches = tile_server
    caches["satellite"].put_tile(12, 2181, 1430, _png(_grid(lambda c, r: 0.0)))
    status, _, _ = _get(server, "/api/tiles/satellite/14/8725/5721.png")
    assert status == 404


# ---------------------------------------------------------------------------
# The upstream fetch
# ---------------------------------------------------------------------------

class _Resp:
    def __init__(self, data: bytes) -> None:
        self._data = data

    def __enter__(self) -> _Resp:
        return self

    def __exit__(self, *exc: object) -> bool:
        return False

    def read(self, amt: int | None = None) -> bytes:
        return self._data if amt is None else self._data[:amt]


@pytest.fixture
def handler():
    h = object.__new__(CorvusHandler)
    h.tile_breaker = _UpstreamBreaker()
    return h


def test_an_http_refusal_does_not_open_the_shared_breaker(handler, monkeypatch) -> None:
    """A service throttling us answered: the link is up. Opening the breaker
    would stop the elevation tiles too and leave holes in the terrain."""
    def refuse(req, timeout=None):
        raise urllib.error.HTTPError("u", 429, "Too Many Requests", {}, io.BytesIO(b""))
    monkeypatch.setattr(urllib.request, "urlopen", refuse)
    for i in range(TILE_UPSTREAM_FAIL_THRESHOLD * 3):
        assert handler._fetch_upstream_tile("satellite", 14, 8600 + i, 5750) is None
    assert not handler.tile_breaker.is_open()


def test_a_page_that_is_not_an_image_is_not_a_tile(handler, monkeypatch) -> None:
    """A captive portal answers 200 with its login page. Cached, that page
    would be served as a map tile, and as an elevation tile decoded as terrain."""
    monkeypatch.setattr(urllib.request, "urlopen",
                        lambda req, timeout=None: _Resp(b"<html>Sign in to the Wi-Fi</html>"))
    for _ in range(TILE_UPSTREAM_FAIL_THRESHOLD):
        assert handler._fetch_upstream_tile("terrain", 12, 2181, 1430) is None
    assert handler.tile_breaker.is_open(), "a portal in the way means the internet is not there"


@pytest.mark.parametrize("blob", [
    b"\x89PNG\r\n\x1a\n" + b"\x00" * 64,
    b"\xff\xd8\xff\xe0" + b"\x00" * 64,
    b"RIFF\x00\x00\x00\x00WEBPVP8 " + b"\x00" * 64,
])
def test_real_image_formats_come_through(handler, monkeypatch, blob: bytes) -> None:
    monkeypatch.setattr(urllib.request, "urlopen", lambda req, timeout=None: _Resp(blob))
    assert handler._fetch_upstream_tile("satellite", 14, 1, 1) == blob


# ---------------------------------------------------------------------------
# The elevation that rides along with an offline download
# ---------------------------------------------------------------------------

class _Recorder:
    def __init__(self) -> None:
        self.calls: list[tuple] = []

    def start(self, source, upstream, bounds, minzoom, maxzoom, on_progress=None, token=""):
        self.calls.append((source, minzoom, maxzoom))
        return f"job-{len(self.calls)}"


def _companion(bounds, minzoom, maxzoom):
    h = object.__new__(CorvusHandler)
    h.tile_downloader = _Recorder()
    h.tile_caches = {}
    h.tile_progress_bus = None
    h._start_terrain_companion(bounds, minzoom, maxzoom, "Area")
    return h.tile_downloader.calls


def test_elevation_is_downloaded_from_zoom_zero() -> None:
    """A pitched view draws its far ground from coarse elevation, and every
    missing fine tile is derived from a coarse one: an area whose elevation
    started at the imagery's z14 had neither."""
    calls = _companion((11.00, 47.40, 11.05, 47.45), 14, 18)
    assert calls == [("terrain", 0, 15)]


def test_a_huge_area_gives_back_low_zooms_rather_than_fail() -> None:
    from corvus.tile_downloader import MAX_TILES_PER_JOB, tile_count
    bounds = (10.0, 47.0, 12.5, 48.4)
    assert tile_count(bounds, 15, 15) <= MAX_TILES_PER_JOB
    assert tile_count(bounds, 0, 15) > MAX_TILES_PER_JOB
    [(source, lo, hi)] = _companion(bounds, 15, 15)
    assert (source, hi) == ("terrain", 15)
    assert 0 < lo <= 15
    assert tile_count(bounds, lo, hi) <= MAX_TILES_PER_JOB


# ---------------------------------------------------------------------------
# A keyed service's elevation, stood in for by the free one
# ---------------------------------------------------------------------------

def test_transcode_keeps_the_heights_across_packings_and_sizes() -> None:
    terrarium = _png(_grid(lambda c, r: 1234.5 + c * 0.5))
    blob = dem_tiles.transcode(terrarium, "terrarium", "mapbox", 512)
    grid = dem_tiles.decode(blob)
    assert (grid.width, grid.height) == (512, 512), "re-cut to the keyed source's tile size"
    mapbox_height = lambda v: v * 0.1 - 10000  # noqa: E731
    # Pixel 2i+1 of the 512 grid sits a quarter pixel past pixel i of the 256.
    for col in (10, 200, 500):
        want = 1234.5 + (min(max((col + 0.5) / 2 - 0.5, 0), 255)) * 0.5
        got = mapbox_height(grid.values[40 * 512 + col])
        assert abs(got - want) < 0.06, (col, got, want)


def test_transcode_declines_what_it_cannot_read() -> None:
    png = _png(_grid(lambda c, r: 0.0))
    assert dem_tiles.transcode(png, "terrarium", "custom", 256) is None
    assert dem_tiles.transcode(b"RIFF....WEBP", "mapbox", "terrarium", 256) is None


@pytest.fixture
def keyed_server(tmp_path, monkeypatch):
    """A live server with the free DEM, MapTiler's DEM, and no network."""
    caches = {
        "terrain": TileCache(str(tmp_path / "terrain.mbtiles")),
        "maptiler_terrain": TileCache(str(tmp_path / "maptiler_terrain.mbtiles")),
    }
    saved = (CorvusHandler.tile_caches, CorvusHandler.dem_fallback,
             CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh)
    CorvusHandler.tile_caches = caches
    CorvusHandler.dem_fallback = dem_tiles.DemFallback()
    CorvusHandler.store = None
    CorvusHandler.mavlink = None
    CorvusHandler.ssh = None
    monkeypatch.setattr(CorvusHandler, "_fetch_upstream_tile", lambda *a, **k: None)
    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True,
                              kwargs={"poll_interval": 0.05})
    thread.start()
    try:
        yield server, caches
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        for cache in caches.values():
            cache.close()
        (CorvusHandler.tile_caches, CorvusHandler.dem_fallback,
         CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh) = saved


def test_a_keyed_dem_never_means_worse_terrain_than_the_free_one(keyed_server) -> None:
    """Ground downloaded with the free elevation, viewed later with a MapTiler
    key set: the keyed tile is not on disk and there is no network, so the
    free heights are served in MapTiler's packing and size rather than a hole."""
    server, caches = keyed_server
    caches["terrain"].put_tile(12, 2181, 1430, _png(_grid(lambda c, r: 2000.0)))
    status, headers, body = _get(server, "/api/tiles/maptiler_terrain/12/2181/1430.png")
    assert status == 200
    assert headers["cache-control"] == "max-age=300"
    grid = dem_tiles.decode(body)
    assert (grid.width, grid.height) == (512, 512)
    assert abs(grid.values[1000] * 0.1 - 10000 - 2000.0) < 0.06
    assert caches["maptiler_terrain"].get_tile(12, 2181, 1430) is None


def test_the_stand_in_also_reaches_below_the_free_dems_own_cache(keyed_server) -> None:
    server, caches = keyed_server
    caches["terrain"].put_tile(10, 545, 357, _png(_grid(lambda c, r: 750.0)))
    status, _, body = _get(server, "/api/tiles/maptiler_terrain/13/4362/2860.png")
    assert status == 200
    grid = dem_tiles.decode(body)
    assert abs(grid.values[0] * 0.1 - 10000 - 750.0) < 0.06


def test_a_keyed_dem_over_unknown_ground_is_still_a_404(keyed_server) -> None:
    server, _ = keyed_server
    status, _, _ = _get(server, "/api/tiles/maptiler_terrain/12/2181/1430.png")
    assert status == 404


# ---------------------------------------------------------------------------
# Copernicus: complete to zoom 12, regional above it
# ---------------------------------------------------------------------------

def test_copernicus_is_keyless_terrarium_at_512_and_regional_above_12() -> None:
    from corvus import tile_sources
    spec = tile_sources.get("copernicus")
    assert spec["encoding"] == "terrarium"
    assert spec["tile_size"] == 512
    assert not tile_sources.needs_token("copernicus")
    assert tile_sources.sparse_above("copernicus") == 12
    assert tile_sources.sparse_above(tile_sources.DEFAULT_TERRAIN) is None
    assert tile_sources.PREFERRED_TERRAIN == "copernicus"


@pytest.fixture
def copernicus_server(tmp_path, monkeypatch):
    caches = {
        "terrain": TileCache(str(tmp_path / "terrain.mbtiles")),
        "copernicus": TileCache(str(tmp_path / "copernicus.mbtiles")),
    }
    saved = (CorvusHandler.tile_caches, CorvusHandler.dem_fallback,
             CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh)
    CorvusHandler.tile_caches = caches
    CorvusHandler.dem_fallback = dem_tiles.DemFallback()
    CorvusHandler.store = None
    CorvusHandler.mavlink = None
    CorvusHandler.ssh = None
    monkeypatch.setattr(CorvusHandler, "_fetch_upstream_tile", lambda *a, **k: None)
    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True,
                              kwargs={"poll_interval": 0.05})
    thread.start()
    try:
        yield server, caches
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        for cache in caches.values():
            cache.close()
        (CorvusHandler.tile_caches, CorvusHandler.dem_fallback,
         CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh) = saved


def test_above_its_complete_levels_a_missing_copernicus_tile_is_left_to_the_parent(
        copernicus_server) -> None:
    """Outside a national model there is nothing finer than zoom 12, and
    MapLibre draws a 404 there from the zoom 12 parent. Standing in the free
    model instead would draw coarser data at a zoom where the parent is sharper."""
    server, caches = copernicus_server
    caches["terrain"].put_tile(12, 2181, 1430, _png(_grid(lambda c, r: 700.0)))
    status, _, _ = _get(server, "/api/tiles/copernicus/13/4362/2860.png")
    assert status == 404


def test_at_its_complete_levels_a_missing_copernicus_tile_still_gets_the_free_one(
        copernicus_server) -> None:
    server, caches = copernicus_server
    caches["terrain"].put_tile(12, 2181, 1430, _png(_grid(lambda c, r: 700.0)))
    status, _, body = _get(server, "/api/tiles/copernicus/12/2181/1430.png")
    assert status == 200
    grid = dem_tiles.decode(body)
    assert grid.width == 512
    assert abs(grid.values[0] / 256 - 32768 - 700.0) < 0.01


def test_a_regional_404_is_remembered_for_the_ground_under_it(monkeypatch) -> None:
    from corvus.server import _AbsentTiles
    calls = []

    def refuse(req, timeout=None):
        calls.append(req.full_url)
        raise urllib.error.HTTPError(req.full_url, 404, "Not Found", {}, io.BytesIO(b""))

    monkeypatch.setattr(urllib.request, "urlopen", refuse)
    h = object.__new__(CorvusHandler)
    h.tile_breaker = _UpstreamBreaker()
    h.tile_absent = _AbsentTiles()
    assert h._fetch_upstream_tile("copernicus", 13, 4450, 3550) is None
    assert len(calls) == 1
    # Its children, and itself again, are answered without a round trip.
    for z, x, y in ((13, 4450, 3550), (14, 8900, 7100), (16, 35601, 28403)):
        assert h._fetch_upstream_tile("copernicus", z, x, y) is None
    assert len(calls) == 1
    assert not h.tile_breaker.is_open(), "a tile that does not exist says nothing about the link"
    # Its neighbour is still asked for.
    h._fetch_upstream_tile("copernicus", 13, 4451, 3550)
    assert len(calls) == 2


def test_a_copernicus_download_stops_at_its_complete_levels() -> None:
    calls = []

    class _Rec:
        def start(self, source, upstream, bounds, minzoom, maxzoom, on_progress=None, token=""):
            calls.append((source, minzoom, maxzoom))
            return "job"

    h = object.__new__(CorvusHandler)
    h.tile_downloader = _Rec()
    h.tile_caches = {}
    h.tile_progress_bus = None
    h._start_terrain_companion((11.0, 47.4, 11.05, 47.45), 12, 17, "Area", "copernicus")
    assert calls == [("copernicus", 0, 12)]
