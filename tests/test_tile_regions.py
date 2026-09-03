"""Named downloaded areas ("regions") — storage, the HTTP surface, and the
overlap-safe delete.

An area is what makes an offline tile cache legible: "22,431 tiles cached" is
not something an operator can act on, "Landing site north, z12-17" is. These
tests cover the three places that claim can break:

1. The store (``TileCache``): records live in the same ``.mbtiles`` as the
   tiles they describe, survive reopen, and coexist with the MBTiles tables.
2. The HTTP surface: a download names its area, an unnamed one still gets
   recorded, and the list/rename/remove endpoints behave.
3. The delete: areas overlap by design (a wide low-zoom region with a
   high-zoom site inside it), so deleting one must not punch holes in another.

Hermetic and offline: real SQLite files in ``tmp_path``, a fake downloader,
no network.
"""
from __future__ import annotations

import http.client
import json
import sqlite3
import threading
from typing import Any

import pytest

from corvus.tile_cache import TileCache
from corvus.tile_downloader import enumerate_tiles
from corvus.tile_sources import TILE_SOURCES

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus.server import (  # noqa: E402
    CorvusHandler,
    CorvusServer,
    _TileProgressBus,
    _clean_region_name,
    _default_region_name,
    _delete_region_tiles,
)


# ---------------------------------------------------------------------------
# 1. The store
# ---------------------------------------------------------------------------

@pytest.fixture
def cache(tmp_path):
    c = TileCache(str(tmp_path / "satellite.mbtiles"))
    yield c
    c.close()


def _region(rid: str, **over: Any) -> dict:
    base = {
        "id": rid, "name": f"Area {rid}", "source": "satellite",
        "w": 8.5, "s": 47.3, "e": 8.6, "n": 47.4,
        "minzoom": 12, "maxzoom": 13, "tile_count": 0, "state": "running",
    }
    base.update(over)
    return base


def test_add_and_list_region_round_trips(cache) -> None:
    cache.add_region(_region("a", name="Landing site"))
    regions = cache.list_regions()
    assert len(regions) == 1
    r = regions[0]
    assert r["id"] == "a"
    assert r["name"] == "Landing site"
    assert r["bounds"] == {"w": 8.5, "s": 47.3, "e": 8.6, "n": 47.4}
    assert r["minzoom"] == 12 and r["maxzoom"] == 13
    assert r["state"] == "running"
    # created_at is filled in when the caller omits it, so a record is never
    # undatable in the list.
    assert r["created_at"]


def test_add_region_is_idempotent_by_id(cache) -> None:
    """Re-adding the same id replaces rather than duplicating — the download
    endpoint keys records by job id and may write one more than once."""
    cache.add_region(_region("a", name="First"))
    cache.add_region(_region("a", name="Second"))
    regions = cache.list_regions()
    assert len(regions) == 1
    assert regions[0]["name"] == "Second"


def test_add_region_tolerates_a_malformed_record(cache) -> None:
    """A bad value must degrade to the default, never lose the download."""
    cache.add_region({"id": "a", "name": "", "w": "not-a-number", "minzoom": None})
    r = cache.list_regions()[0]
    assert r["name"] == "Region"        # empty name -> default
    assert r["bounds"]["w"] == 0.0      # uncoercible float -> default
    assert r["minzoom"] == 0


def test_update_region_patches_named_columns_only(cache) -> None:
    cache.add_region(_region("a"))
    assert cache.update_region("a", tile_count=612, state="done") is True
    r = cache.get_region("a")
    assert r["tile_count"] == 612 and r["state"] == "done"
    # An unknown column is ignored rather than crashing or being written.
    assert cache.update_region("a", bogus="x") is False
    assert cache.update_region("missing", state="done") is False


def test_regions_survive_reopen(tmp_path) -> None:
    """Areas live in the .mbtiles beside their tiles: copy the file and the
    names travel with it."""
    path = str(tmp_path / "s.mbtiles")
    with TileCache(path) as c:
        c.add_region(_region("a", name="Airfield", state="done"))
    with TileCache(path) as c:
        assert [r["name"] for r in c.list_regions()] == ["Airfield"]


def test_region_table_added_to_a_preexisting_mbtiles(tmp_path) -> None:
    """An .mbtiles written by an older build gains the table on first open."""
    path = str(tmp_path / "old.mbtiles")
    conn = sqlite3.connect(path)
    conn.execute("CREATE TABLE metadata (name TEXT, value TEXT)")
    conn.execute(
        "CREATE TABLE tiles (zoom_level INTEGER, tile_column INTEGER, "
        "tile_row INTEGER, tile_data BLOB)"
    )
    conn.commit()
    conn.close()
    with TileCache(path) as c:
        c.add_region(_region("a"))
        assert len(c.list_regions()) == 1


def test_remove_region_keeps_the_tiles(cache) -> None:
    """Forgetting a record must not silently drop cached imagery — deleting
    tiles is the separate, explicit step."""
    cache.add_region(_region("a"))
    cache.put_tile(12, 100, 200, b"px")
    assert cache.remove_region("a") is True
    assert cache.list_regions() == []
    assert cache.has_tile(12, 100, 200) is True
    assert cache.remove_region("a") is False


def test_delete_tiles_removes_only_what_it_is_given(cache) -> None:
    cache.put_tile(12, 1, 1, b"a")
    cache.put_tile(12, 2, 2, b"b")
    assert cache.delete_tiles([(12, 1, 1)]) == 1
    assert cache.has_tile(12, 1, 1) is False
    assert cache.has_tile(12, 2, 2) is True
    # Deleting a tile that is not there is a no-op, not an error.
    assert cache.delete_tiles([(12, 9, 9)]) == 0


# ---------------------------------------------------------------------------
# 2. Overlap-safe deletion
# ---------------------------------------------------------------------------

WIDE = {"w": 8.4, "s": 47.3, "e": 8.7, "n": 47.5}
INNER = {"w": 8.50, "s": 47.36, "e": 8.55, "n": 47.40}
FAR = {"w": 2.0, "s": 40.0, "e": 2.1, "n": 40.1}


def _fill(cache: TileCache, bounds: dict, lo: int, hi: int) -> set:
    tiles = set(enumerate_tiles((bounds["w"], bounds["s"], bounds["e"], bounds["n"]), lo, hi))
    for z, x, y in tiles:
        cache.put_tile(z, x, y, b"px")
    return tiles


def test_deleting_a_contained_area_keeps_every_tile_the_outer_one_needs(cache) -> None:
    """The case that makes a naive delete dangerous: the inner area's tiles are
    entirely a subset of the outer one's, so NOTHING may be removed."""
    cache.add_region(_region("wide", **WIDE, minzoom=12, maxzoom=13))
    cache.add_region(_region("inner", **INNER, minzoom=12, maxzoom=13))
    wide_tiles = _fill(cache, WIDE, 12, 13)

    removed = _delete_region_tiles(cache, cache.get_region("inner"))
    assert removed == 0
    assert all(cache.has_tile(*t) for t in wide_tiles)


def test_deleting_a_disjoint_area_reclaims_all_of_its_tiles(cache) -> None:
    cache.add_region(_region("wide", **WIDE, minzoom=12, maxzoom=12))
    cache.add_region(_region("far", **FAR, minzoom=12, maxzoom=12))
    wide_tiles = _fill(cache, WIDE, 12, 12)
    far_tiles = _fill(cache, FAR, 12, 12)
    assert not (wide_tiles & far_tiles), "fixture bug: the areas must not overlap"

    removed = _delete_region_tiles(cache, cache.get_region("far"))
    assert removed == len(far_tiles)
    assert not any(cache.has_tile(*t) for t in far_tiles)
    assert all(cache.has_tile(*t) for t in wide_tiles)


def test_deleting_the_outer_area_keeps_the_inner_ones_tiles(cache) -> None:
    """The mirror case: removing the wide area must leave the site usable."""
    cache.add_region(_region("wide", **WIDE, minzoom=12, maxzoom=13))
    cache.add_region(_region("inner", **INNER, minzoom=12, maxzoom=13))
    _fill(cache, WIDE, 12, 13)
    inner_tiles = set(enumerate_tiles(
        (INNER["w"], INNER["s"], INNER["e"], INNER["n"]), 12, 13))

    _delete_region_tiles(cache, cache.get_region("wide"))
    assert all(cache.has_tile(*t) for t in inner_tiles)


def test_deleting_the_only_area_reclaims_everything(cache) -> None:
    cache.add_region(_region("only", **FAR, minzoom=12, maxzoom=12))
    tiles = _fill(cache, FAR, 12, 12)
    assert _delete_region_tiles(cache, cache.get_region("only")) == len(tiles)


# ---------------------------------------------------------------------------
# 3. Naming helpers
# ---------------------------------------------------------------------------

def test_clean_region_name_collapses_whitespace_and_truncates() -> None:
    assert _clean_region_name("  Landing site\n  north  ") == "Landing site north"
    assert _clean_region_name("x" * 200) == "x" * 60
    # Anything that is not usable text yields "", so the caller falls back to a
    # generated name instead of rejecting the download.
    assert _clean_region_name(None) == ""
    assert _clean_region_name(42) == ""
    assert _clean_region_name("   ") == ""


def test_default_region_name_is_the_centre_coordinate() -> None:
    """A coordinate, not a timestamp: in the field an area is recognized by
    where it is, and a list of timestamps names nothing."""
    assert _default_region_name((8.5, 47.3, 8.6, 47.4)) == "47.350°N 8.550°E"
    assert _default_region_name((-8.6, -47.4, -8.5, -47.3)) == "47.350°S 8.550°W"


# ---------------------------------------------------------------------------
# 4. The HTTP surface
# ---------------------------------------------------------------------------

class _FakeDownloader:
    """Records start() calls and drives the on_progress callback on demand."""

    def __init__(self) -> None:
        self.jobs: dict[str, Any] = {}
        self._n = 0

    def start(self, source, upstream, bounds, minzoom, maxzoom, on_progress=None) -> str:
        self._n += 1
        jid = f"job-{self._n}"
        self.jobs[jid] = {
            "status": {"job_id": jid, "state": "running", "done": 0, "total": 4, "failed": 0},
            "on_progress": on_progress,
        }
        return jid

    def finish(self, jid: str, state: str = "done", done: int = 4) -> None:
        job = self.jobs[jid]
        job["status"].update(state=state, done=done)
        if job["on_progress"]:
            job["on_progress"](dict(job["status"]))

    def status(self, jid): return dict(self.jobs[jid]["status"]) if jid in self.jobs else None
    def cancel(self, jid): return jid in self.jobs
    def list_jobs(self): return [dict(j["status"]) for j in self.jobs.values()]
    def shutdown(self): pass


@pytest.fixture
def region_server(tmp_path):
    """Live CorvusServer with real caches and a controllable fake downloader."""
    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    caches = {sid: TileCache(str(cache_dir / f"{sid}.mbtiles")) for sid in TILE_SOURCES}
    downloader = _FakeDownloader()

    saved = (
        CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
        CorvusHandler.tile_caches, CorvusHandler.tile_downloader,
        CorvusHandler.tile_progress_bus,
    )
    CorvusHandler.store = None
    CorvusHandler.mavlink = None
    CorvusHandler.ssh = None
    CorvusHandler.tile_caches = caches
    CorvusHandler.tile_downloader = downloader
    CorvusHandler.tile_progress_bus = _TileProgressBus()

    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    server.tile_caches = caches
    server.tile_downloader = downloader
    thread = threading.Thread(
        target=server.serve_forever, name="corvus-test-regions", daemon=True,
        kwargs={"poll_interval": 0.05},
    )
    thread.start()
    try:
        yield server, downloader, caches
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        for c in caches.values():
            c.close()
        (
            CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
            CorvusHandler.tile_caches, CorvusHandler.tile_downloader,
            CorvusHandler.tile_progress_bus,
        ) = saved


def _req(server, method: str, path: str, payload: dict | None = None):
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    if payload is None:
        conn.request(method, path)
    else:
        conn.request(method, path, json.dumps(payload), {"Content-Type": "application/json"})
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, json.loads(body) if body else {}


_DOWNLOAD = {
    "source": "satellite",
    "bounds": {"w": 8.5, "s": 47.3, "e": 8.6, "n": 47.4},
    "minzoom": 12, "maxzoom": 13,
}


def test_download_records_a_named_area_immediately(region_server) -> None:
    """The area is recorded when the job starts, not when it finishes, so the
    map can show it as in-progress."""
    server, _, _ = region_server
    status, data = _req(server, "POST", "/api/tiles/download",
                        dict(_DOWNLOAD, name="  Landing site  north "))
    assert status == 200
    assert data["name"] == "Landing site north"

    status, data = _req(server, "GET", "/api/tiles/regions")
    assert status == 200
    assert len(data["regions"]) == 1
    r = data["regions"][0]
    assert r["name"] == "Landing site north"
    assert r["source"] == "satellite"
    assert r["state"] == "running"
    assert r["id"] == "job-1"


def test_download_without_a_name_still_records_the_area(region_server) -> None:
    """An unnamed download must not be invisible in the list."""
    server, _, _ = region_server
    _, data = _req(server, "POST", "/api/tiles/download", _DOWNLOAD)
    assert data["name"] == "47.350°N 8.550°E"
    _, listing = _req(server, "GET", "/api/tiles/regions")
    assert listing["regions"][0]["name"] == "47.350°N 8.550°E"


def test_completion_patches_the_tile_count_and_state(region_server) -> None:
    server, downloader, _ = region_server
    _, data = _req(server, "POST", "/api/tiles/download", dict(_DOWNLOAD, name="Field"))
    downloader.finish(data["job_id"], state="done", done=37)

    _, listing = _req(server, "GET", "/api/tiles/regions")
    r = listing["regions"][0]
    assert r["state"] == "done"
    # `done` counts tiles now present in the cache (fetched plus already-held),
    # so it is exactly how much of the area is available offline.
    assert r["tile_count"] == 37


def test_a_cancelled_download_is_recorded_as_cancelled(region_server) -> None:
    """Partial coverage must not masquerade as a complete area."""
    server, downloader, _ = region_server
    _, data = _req(server, "POST", "/api/tiles/download", dict(_DOWNLOAD, name="Partial"))
    downloader.finish(data["job_id"], state="cancelled", done=9)
    _, listing = _req(server, "GET", "/api/tiles/regions")
    assert listing["regions"][0]["state"] == "cancelled"
    assert listing["regions"][0]["tile_count"] == 9


def test_progress_still_reaches_sse_subscribers(region_server) -> None:
    """Region bookkeeping shares the downloader's single on_progress callback
    with the SSE bus; the bus half must not be displaced by it."""
    server, downloader, _ = region_server
    _, data = _req(server, "POST", "/api/tiles/download", _DOWNLOAD)
    seen: list[dict] = []
    CorvusHandler.tile_progress_bus.subscribe(data["job_id"], seen.append)
    downloader.finish(data["job_id"])
    assert seen and seen[-1]["state"] == "done"


def test_regions_are_listed_across_every_source(region_server) -> None:
    server, _, _ = region_server
    _req(server, "POST", "/api/tiles/download", dict(_DOWNLOAD, name="Esri area"))
    _req(server, "POST", "/api/tiles/download",
         dict(_DOWNLOAD, source="osm", name="OSM area"))
    _, listing = _req(server, "GET", "/api/tiles/regions")
    assert {r["source"] for r in listing["regions"]} == {"satellite", "osm"}


def test_rename_region(region_server) -> None:
    server, _, _ = region_server
    _, data = _req(server, "POST", "/api/tiles/download", _DOWNLOAD)
    status, res = _req(server, "POST", "/api/tiles/regions/rename",
                       {"source": "satellite", "id": data["job_id"], "name": "  Home  field "})
    assert status == 200 and res["name"] == "Home field"
    _, listing = _req(server, "GET", "/api/tiles/regions")
    assert listing["regions"][0]["name"] == "Home field"


@pytest.mark.parametrize("payload,expected", [
    ({"source": "satellite", "name": "x"}, 400),                 # no id
    ({"source": "satellite", "id": "job-1", "name": "  "}, 400),  # empty name
    ({"source": "nope", "id": "job-1", "name": "x"}, 400),        # unknown source
    ({"source": "satellite", "id": "missing", "name": "x"}, 404),  # unknown region
])
def test_rename_rejects_bad_input(region_server, payload, expected) -> None:
    server, _, _ = region_server
    _req(server, "POST", "/api/tiles/download", _DOWNLOAD)
    status, _ = _req(server, "POST", "/api/tiles/regions/rename", payload)
    assert status == expected


def test_remove_keeps_tiles_unless_asked(region_server) -> None:
    server, _, caches = region_server
    _, data = _req(server, "POST", "/api/tiles/download", _DOWNLOAD)
    cache = caches["satellite"]
    tiles = _fill(cache, _DOWNLOAD["bounds"], 12, 13)

    status, res = _req(server, "POST", "/api/tiles/regions/remove",
                       {"source": "satellite", "id": data["job_id"]})
    assert status == 200
    assert res["removed_tiles"] == 0
    assert all(cache.has_tile(*t) for t in tiles)
    _, listing = _req(server, "GET", "/api/tiles/regions")
    assert listing["regions"] == []


def test_remove_with_delete_tiles_reclaims_disk(region_server) -> None:
    server, _, caches = region_server
    _, data = _req(server, "POST", "/api/tiles/download", _DOWNLOAD)
    cache = caches["satellite"]
    tiles = _fill(cache, _DOWNLOAD["bounds"], 12, 13)

    status, res = _req(server, "POST", "/api/tiles/regions/remove",
                       {"source": "satellite", "id": data["job_id"], "delete_tiles": True})
    assert status == 200
    assert res["removed_tiles"] == len(tiles)
    assert not any(cache.has_tile(*t) for t in tiles)


@pytest.mark.parametrize("payload,expected", [
    ({"source": "satellite"}, 400),                    # no id
    ({"id": "job-1"}, 400),                            # no source
    ({"source": "nope", "id": "job-1"}, 400),          # unknown source
    ({"source": "satellite", "id": "missing"}, 404),   # unknown region
])
def test_remove_rejects_bad_input(region_server, payload, expected) -> None:
    server, _, _ = region_server
    _req(server, "POST", "/api/tiles/download", _DOWNLOAD)
    status, _ = _req(server, "POST", "/api/tiles/regions/remove", payload)
    assert status == expected


def test_regions_endpoint_survives_an_unreadable_cache(region_server) -> None:
    """A source whose cache cannot be read contributes nothing rather than
    failing the whole list — the map draws these, so a 500 would blank every
    area the operator does have."""
    server, _, caches = region_server
    _req(server, "POST", "/api/tiles/download", _DOWNLOAD)

    class _Broken:
        def list_regions(self):
            raise sqlite3.DatabaseError("file is not a database")

    CorvusHandler.tile_caches = dict(caches, osm=_Broken())
    try:
        status, listing = _req(server, "GET", "/api/tiles/regions")
        assert status == 200
        assert [r["source"] for r in listing["regions"]] == ["satellite"]
    finally:
        CorvusHandler.tile_caches = caches
