"""The map data routes of the Rust core held to the Python server.

Both servers start on identical state folders: the same per source MBTiles
caches, regions and building cells are written into each before they start, so
no answer here needs a network. Every upstream is made unreachable (a dead
proxy), so a miss is a miss on both. Derived elevation PNGs are compared on
decoded pixels, not bytes (the two zlib streams differ).

    cargo build -p corvus_core --bin corvus-core
"""
from __future__ import annotations

import gzip
import http.client
import json
import math
import pathlib
import sys
from array import array

import pytest

pytest.importorskip("corvus_link")
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "tools"))

import route_harness as harness  # noqa: E402

pytestmark = pytest.mark.skipif(harness.core_binary() is None, reason="corvus-core is not built")

REGION_BOUNDS = (11.5, 48.1, 11.52, 48.12)


def _terrain(base: int, slope: int = 3) -> bytes:
    from corvus import dem_tiles

    values = array(dem_tiles._U32, [32768 * 256 + base + slope * (i % 256) + 5 * (i // 256) for i in range(256 * 256)])
    return dem_tiles.encode(256, 256, values)


def _image() -> bytes:
    from corvus import dem_tiles

    return dem_tiles.encode(2, 2, array(dem_tiles._U32, [1, 2, 3, 4]))


def _tile_xy(z: int, lon: float, lat: float) -> tuple[int, int]:
    from corvus import custom_dem

    px, py = custom_dem.lonlat_pixel(z, lon, lat)
    return int(px // 256), int(py // 256)


def _seed_tiles(state_dir: pathlib.Path) -> None:
    from corvus.buildings import BuildingCache
    from corvus.tile_cache import TileCache

    tiles = state_dir / "tiles"
    tiles.mkdir(parents=True, exist_ok=True)
    sat = TileCache(str(tiles / "satellite.mbtiles"))
    sat.put_tile(3, 4, 5, _image())
    sat.put_tile(4, 8, 10, _image())
    sat.add_region({"id": "r1", "name": "Field", "source": "satellite", "w": REGION_BOUNDS[0], "s": REGION_BOUNDS[1],
                    "e": REGION_BOUNDS[2], "n": REGION_BOUNDS[3], "minzoom": 3, "maxzoom": 4, "tile_count": 2,
                    "created_at": "2026-01-02T03:04:05Z", "state": "done"})
    sat.add_region({"id": "r2", "name": "Other", "source": "satellite", "w": 1, "s": 2, "e": 3, "n": 4, "minzoom": 1,
                    "maxzoom": 2, "tile_count": 0, "created_at": "2026-02-02T03:04:05Z", "state": "cancelled"})
    sat.close()
    terrain = TileCache(str(tiles / "terrain.mbtiles"))
    terrain.put_tile(3, 1, 1, _terrain(1000))
    terrain.put_tile(3, 2, 2, _terrain(1500, 1))
    terrain.put_tile(5, 17, 17, _terrain(900, 2))
    x, y = _tile_xy(15, 11.5538, 48.0959)
    terrain.put_tile(15, x, y, _terrain(2000))
    terrain.put_tile(15, x + 1, y, _terrain(2100))
    terrain.close()
    cells = BuildingCache(str(tiles / "buildings.sqlite"))
    from corvus.buildings import _gzip_json

    cells.put(15, 17430, 11376, _gzip_json({"type": "FeatureCollection", "features": [
        {"type": "Feature", "geometry": {"type": "Polygon", "coordinates": [[[11.5, 48.1], [11.5003, 48.1],
                                                                              [11.5003, 48.1003], [11.5, 48.1]]]},
         "properties": {"height": 12.0, "min_height": 0.0}}]}))
    cells.close()


def _seed() -> dict:
    seed = harness.standard_seed()
    seed["config"]["map_tokens"] = {"mapbox": "pk.secret"}
    seed["setup"] = _seed_tiles
    return seed


@pytest.fixture(scope="module")
def pair():
    import os

    saved = {k: os.environ.get(k) for k in ("HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "https_proxy", "http_proxy", "NO_PROXY", "no_proxy")}
    for key in saved:
        os.environ.pop(key, None)
    os.environ["HTTPS_PROXY"] = os.environ["HTTP_PROXY"] = "http://127.0.0.1:9"
    os.environ["https_proxy"] = os.environ["http_proxy"] = "http://127.0.0.1:9"
    os.environ["NO_PROXY"] = os.environ["no_proxy"] = "127.0.0.1,localhost"
    try:
        with harness.Pair(seed=_seed()) as p:
            yield p
    finally:
        for key, value in saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


def raw(server, method: str, path: str, body: bytes | None = None):
    conn = http.client.HTTPConnection("127.0.0.1", server.port, timeout=60)
    conn.request(method, path, body=body, headers={"Content-Type": "application/octet-stream"} if body is not None else {})
    resp = conn.getresponse()
    data = resp.read()
    headers = {k.lower(): v for k, v in resp.getheaders()}
    conn.close()
    return resp.status, headers, data


def both_raw(pair, method: str, path: str, body: bytes | None = None):
    return raw(pair.py, method, path, body), raw(pair.rs, method, path, body)


def pixels(png: bytes):
    from corvus import dem_tiles

    grid = dem_tiles.decode(png)
    assert grid is not None
    return grid.width, grid.height, list(grid.values)


@pytest.mark.parametrize("path", ["/api/tiles/sources", "/api/tiles/jobs", "/api/tiles/regions", "/api/dem"])
def test_the_listings_are_answered_as_python_answers_them(pair, path) -> None:
    diffs = pair.compare_get(path)
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("body", [
    {}, {"provider": ""}, {"provider": 5}, {"provider": "nobody"}, {"provider": "mapbox", "token": 5},
    {"provider": "mapbox", "token": "a b"}, {"provider": "mapbox", "token": "k" * 513},
    {"provider": "maptiler", "token": "  abc.DEF-1_2~3 "}, {"provider": "maptiler"}, {"provider": "maptiler", "token": ""},
])
def test_a_key_request_is_answered_as_python_answers_it(pair, body) -> None:
    diffs = pair.compare_post("/api/tiles/token", body)
    diffs += pair.compare_get("/api/tiles/sources")
    assert not diffs, "\n".join(diffs)


def test_the_stored_keys_are_the_same_in_both_config_files(pair) -> None:
    py = json.loads((pair.py.state_dir / "config.json").read_text(encoding="utf-8"))
    rs = json.loads((pair.rs.state_dir / "config.json").read_text(encoding="utf-8"))
    assert rs.get("map_tokens") == py.get("map_tokens")


AREA = {"w": 11.5, "s": 48.1, "e": 11.502, "n": 48.102}


@pytest.mark.parametrize("body", [
    {}, {"source": 5}, {"source": "nope"},
    {"source": "satellite", "bounds": 5}, {"source": "satellite", "bounds": {"w": 1, "s": 2, "e": 3}},
    {"source": "satellite", "bounds": {"w": "1", "s": 2, "e": 3, "n": 4}},
    {"source": "satellite", "bounds": {"w": True, "s": 2, "e": 3, "n": 4}},
    {"source": "satellite", "bounds": {"w": 300, "s": 2, "e": 3, "n": 4}},
    {"source": "satellite", "bounds": {"w": 1, "s": 20, "e": 3, "n": 4}},
    {"source": "satellite", "bounds": {"w": 5, "s": 2, "e": 3, "n": 4}},
    {"source": "satellite", "bounds": AREA}, {"source": "satellite", "bounds": AREA, "minzoom": "1", "maxzoom": 3},
    {"source": "satellite", "bounds": AREA, "minzoom": 1.5, "maxzoom": 3},
    {"source": "satellite", "bounds": AREA, "minzoom": 5, "maxzoom": 3},
    {"source": "satellite", "bounds": AREA, "minzoom": 1, "maxzoom": 40},
    {"source": "satellite", "bounds": AREA, "minzoom": 1, "maxzoom": 22},
    {"source": "satellite", "bounds": {"w": -180, "s": -85, "e": 180, "n": 85}, "minzoom": 0, "maxzoom": 18},
    {"source": "maptiler_satellite", "bounds": AREA, "minzoom": 1, "maxzoom": 2},
])
def test_a_download_that_cannot_start_is_refused_as_python_refuses_it(pair, body) -> None:
    diffs = pair.compare_post("/api/tiles/download", body)
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("path,body", [
    ("/api/tiles/regions/rename", {"source": "satellite", "id": "r1", "name": "  Landing   site "}),
    ("/api/tiles/regions/rename", {"source": "satellite", "id": "", "name": "x"}),
    ("/api/tiles/regions/rename", {"source": "satellite", "id": "r1", "name": "   "}),
    ("/api/tiles/regions/rename", {"source": "nope", "id": "r1", "name": "x"}),
    ("/api/tiles/regions/rename", {"source": "satellite", "id": "zz", "name": "x"}),
    ("/api/tiles/regions/remove", {"source": "satellite", "id": ""}),
    ("/api/tiles/regions/remove", {"source": "nope", "id": "r2"}),
    ("/api/tiles/regions/remove", {"source": "satellite", "id": "zz"}),
    ("/api/tiles/cancel", {}), ("/api/tiles/cancel", {"id": "nobody"}),
])
def test_a_region_or_job_request_is_answered_as_python_answers_it(pair, path, body) -> None:
    diffs = pair.compare_post(path, body)
    diffs += pair.compare_get("/api/tiles/regions")
    assert not diffs, "\n".join(diffs)


def test_removing_a_region_with_its_tiles_removes_the_same_tiles(pair) -> None:
    diffs = pair.compare_post("/api/tiles/regions/remove", {"source": "satellite", "id": "r1", "delete_tiles": True})
    diffs += pair.compare_get("/api/tiles/regions")
    diffs += pair.compare_get("/api/tiles/sources")
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("path", ["/api/tiles/progress", "/api/tiles/progress?id=nobody"])
def test_the_progress_stream_refuses_what_python_refuses(pair, path) -> None:
    (ps, _ph, pb), (rs, _rh, rb) = both_raw(pair, "GET", path)
    assert ps == rs
    assert json.loads(pb) == json.loads(rb)


@pytest.mark.parametrize("path", [
    "/api/tiles/satellite/3/4/5.png", "/api/tiles/satellite/4/8/10.png", "/api/tiles/nothing/1/0/0.png",
    "/api/tiles/satellite/23/0/0.png", "/api/tiles/satellite/3/8/0.png", "/api/tiles/satellite/3/0/99999999999999999999.png",
    "/api/tiles/satellite/9/1/1.png", "/api/tiles/terrain/9/9/9.png",
])
def test_a_tile_is_served_or_refused_as_python_serves_it(pair, path) -> None:
    (ps, ph, pb), (rs, rh, rb) = both_raw(pair, "GET", path)
    assert ps == rs, (ps, rs, pb[:80], rb[:80])
    assert ph.get("content-type") == rh.get("content-type")
    assert ph.get("cache-control") == rh.get("cache-control")
    if "json" in ph.get("content-type", ""):
        assert json.loads(pb) == json.loads(rb)
    else:
        assert pb == rb


@pytest.mark.parametrize("path", [
    "/api/tiles/terrain/3/1/1.png", "/api/tiles/terrain/4/2/2.png", "/api/tiles/terrain/5/17/17.png",
    "/api/tiles/terrain/6/34/34.png", "/api/tiles/terrain/6/35/34.png", "/api/tiles/terrain/5/16/17.png",
    "/api/tiles/terrain/5/18/17.png", "/api/tiles/terrain/5/17/16.png", "/api/tiles/terrain/5/17/18.png",
    "/api/tiles/terrain/4/3/3.png", "/api/tiles/terrain/2/0/0.png", "/api/tiles/terrain/7/68/68.png",
    "/api/tiles/terrain/5/5/5.png", "/api/tiles/terrain/4/4/4.png",
])
def test_a_missing_elevation_tile_is_derived_like_python_derives_it(pair, path) -> None:
    (ps, ph, pb), (rs, rh, rb) = both_raw(pair, "GET", path)
    assert ps == rs, (ps, rs)
    assert ph.get("content-type") == rh.get("content-type")
    assert ph.get("cache-control") == rh.get("cache-control")
    if ps == 200:
        assert pixels(pb) == pixels(rb)


def test_a_keyed_elevation_tile_is_stood_in_for_like_python_does_it(pair) -> None:
    x, y = _tile_xy(15, 11.5538, 48.0959)
    for path in (f"/api/tiles/mapbox_terrain/15/{x}/{y}.png", "/api/tiles/mapbox_terrain/3/1/1.png",
                 "/api/tiles/mapbox_terrain/4/2/2.png", "/api/tiles/maptiler_terrain/3/1/1.png"):
        (ps, ph, pb), (rs, rh, rb) = both_raw(pair, "GET", path)
        assert ps == rs, (path, ps, rs)
        assert ph.get("cache-control") == rh.get("cache-control"), path
        if ps == 200:
            assert pixels(pb) == pixels(rb), path


def test_a_cached_building_cell_is_served_gzipped_like_python_serves_it(pair) -> None:
    (ps, ph, pb), (rs, rh, rb) = both_raw(pair, "GET", "/api/buildings/15/17430/11376.json")
    assert ps == rs == 200
    for header in ("content-type", "content-encoding", "cache-control"):
        assert ph.get(header) == rh.get(header), header
    assert json.loads(gzip.decompress(pb)) == json.loads(gzip.decompress(rb))
    for path in ("/api/buildings/14/1/1.json", "/api/buildings/15/99999/0.json"):
        (ps, ph, pb), (rs, rh, rb) = both_raw(pair, "GET", path)
        assert ps == rs and json.loads(pb) == json.loads(rb), path
        assert ph.get("cache-control") == rh.get("cache-control")


def test_an_uncached_building_cell_is_pending_until_the_fetch_fails(pair) -> None:
    (ps, ph, pb), (rs, rh, rb) = both_raw(pair, "GET", "/api/buildings/15/20000/12000.json")
    assert ps == rs == 200
    assert json.loads(pb) == json.loads(rb) == {"type": "FeatureCollection", "features": [], "pending": True}
    assert ph.get("cache-control") == rh.get("cache-control") == "no-store"


def test_the_place_search_answers_as_python_answers_it(pair) -> None:
    for path in ("/api/geocode", "/api/geocode?q=", "/api/geocode?q=%20%20"):
        diffs = pair.compare_get(path)
        assert not diffs, "\n".join(diffs)
    (ps, _ph, pb), (rs, _rh, rb) = both_raw(pair, "GET", "/api/geocode?q=Munich&near=11.5,48.1&limit=2")
    assert ps == rs == 200
    assert json.loads(pb) == json.loads(rb) == {"ok": False, "results": [], "error": "No connection to the place-search service."}


def _asc() -> bytes:
    gw, gh = 300, 240
    rows = []
    for r in range(gh):
        rows.append(" ".join(f"{120.0 + 15.0 * math.sin(c / 17.0) + 10.0 * math.cos(r / 11.0):.3f}" for c in range(gw)))
    head = f"ncols {gw}\nnrows {gh}\nxllcorner 690000\nyllcorner 5330000\ncellsize 1\nNODATA_value -9999\n"
    return (head + "\n".join(rows) + "\n").encode()


def _strip(model: dict) -> dict:
    return {k: v for k, v in model.items() if k not in ("id", "created_at", "bytes")}


def test_an_elevation_model_is_imported_listed_served_renamed_and_removed_as_python_does_it(pair) -> None:
    body = _asc()
    query = "?filename=C%3A%5Cdata%5Cground.asc&crs=EPSG:25832&name=%20Field%20%3Cb%3E"
    (ps, _ph, pb), (rs, _rh, rb) = both_raw(pair, "POST", "/api/dem/import" + query, body)
    py, rust = json.loads(pb), json.loads(rb)
    assert ps == rs == 200, (py, rust)
    assert _strip(py["model"]) == _strip(rust["model"])
    ids = {"py": py["model"]["id"], "rs": rust["model"]["id"]}
    for text, expected in (("", 400), ("?crs=wgs", 400), ("?crs=auto", 400), ("?crs=1234", 400)):
        (ps, _ph, pb), (rs, _rh, rb) = both_raw(pair, "POST", "/api/dem/import" + text, body if text else b"")
        assert ps == rs == expected, (text, ps, rs)
        assert json.loads(pb) == json.loads(rb), text
    (ps, _ph, pb), (rs, _rh, rb) = both_raw(pair, "POST", "/api/dem/import?crs=25832&model=dem_0000000000", body)
    assert ps == rs == 400 and json.loads(pb) == json.loads(rb)
    (ps, _ph, pb), (rs, _rh, rb) = both_raw(pair, "POST", "/api/dem/import?crs=25832&filename=junk.asc", b"not a grid\n" * 5)
    assert ps == rs == 400 and json.loads(pb) == json.loads(rb)
    py_list = json.loads(raw(pair.py, "GET", "/api/dem")[2])
    rs_list = json.loads(raw(pair.rs, "GET", "/api/dem")[2])
    assert [_strip(m) for m in py_list["models"]] == [_strip(m) for m in rs_list["models"]]
    assert py_list["available"] is rs_list["available"] is True
    py_src = json.loads(raw(pair.py, "GET", "/api/tiles/sources")[2])["terrain"]
    rs_src = json.loads(raw(pair.rs, "GET", "/api/tiles/sources")[2])["terrain"]
    drop = lambda t: [{k: v for k, v in e.items() if k != "id"} for e in t if e.get("custom")]  # noqa: E731
    assert drop(py_src) == drop(rs_src)

    x, y = _tile_xy(17, 11.5538, 48.0959)
    for dx, dy in ((0, 0), (1, 0), (0, 1), (-3, -3), (40, 40)):
        results = []
        for side, server in (("py", pair.py), ("rs", pair.rs)):
            results.append(raw(server, "GET", f"/api/tiles/{ids[side]}/17/{x + dx}/{y + dy}.png"))
        (ps, ph, pb), (rs, rh, rb) = results
        assert ps == rs, (dx, dy, ps, rs)
        assert ph.get("cache-control") == rh.get("cache-control"), (dx, dy)
        if ps == 200:
            assert pixels(pb) == pixels(rb), (dx, dy)
    for dz in (14, 16):
        px, py_ = _tile_xy(dz, 11.5538, 48.0959)
        a = raw(pair.py, "GET", f"/api/tiles/{ids['py']}/{dz}/{px}/{py_}.png")
        b = raw(pair.rs, "GET", f"/api/tiles/{ids['rs']}/{dz}/{px}/{py_}.png")
        assert a[0] == b[0] and a[1].get("cache-control") == b[1].get("cache-control"), dz
        if a[0] == 200:
            assert pixels(a[2]) == pixels(b[2]), dz

    for side, server in (("py", pair.py), ("rs", pair.rs)):
        status, _h, data = raw(server, "POST", "/api/dem/rename", json.dumps({"id": ids[side], "name": " <New> name "}).encode())
        assert (status, json.loads(data)) == (200, {"ok": True, "name": "New name"}), side
    for payload in ({"id": ids["py"]}, {"id": "dem_0000000000", "name": "x"}, {}):
        (ps, _ph, pb), (rs, _rh, rb) = both_raw(pair, "POST", "/api/dem/rename", json.dumps(payload).encode())
        assert ps == rs and json.loads(pb) == json.loads(rb), payload
    for side, server in (("py", pair.py), ("rs", pair.rs)):
        assert json.loads(raw(server, "POST", "/api/dem/remove", json.dumps({"id": ids[side]}).encode())[2]) == {"ok": True}
        status, _h, data = raw(server, "POST", "/api/dem/remove", json.dumps({"id": ids[side]}).encode())
        assert (status, json.loads(data)) == (404, {"ok": False, "error": "unknown elevation model"})
        assert raw(server, "GET", f"/api/tiles/{ids[side]}/17/{x}/{y}.png")[0] == 404
