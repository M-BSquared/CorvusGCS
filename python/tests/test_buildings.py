"""The OSM building overlay 3D mode extrudes.

Three things are pinned here, in the order they can hurt:

1. **Heights.** A building drawn at the wrong height is worse than no
   building: the operator judges clearance against it. The tag preference
   order, the storey fallback, and the clamps are asserted directly.
2. **The request thread never waits on the network.** ``cell`` answers from
   the cache or says "not yet" and returns. A handler that blocked on
   Overpass would hold one of the browser's few connections to this origin
   while the map was trying to pull imagery through them — the failure mode
   is a blank map, not a slow overlay, so it is pinned rather than reviewed.
3. **Offline is a normal answer.** Every path with no network returns
   something the map can draw (nothing), never an exception.

No test here talks to Overpass. The service is pointed at a closed port or a
stub, which is also what an offline field laptop looks like.
"""
from __future__ import annotations

import gzip
import json
import time
import urllib.error

import pytest

from corvus import buildings
from corvus.buildings import (
    CELL_ZOOM,
    BuildingCache,
    BuildingService,
    building_height,
    cell_bounds,
    cells_for_bounds,
    default_cache_path,
    overpass_query,
    to_geojson,
)

# A port nothing listens on: every request fails immediately, which is what
# offline looks like from inside the service.
DEAD_URL = "http://127.0.0.1:9/overpass"


@pytest.fixture()
def service(tmp_path):
    svc = BuildingService(str(tmp_path / "b.sqlite"), url=DEAD_URL, timeout=0.5)
    yield svc
    svc.close()


# ---------------------------------------------------------------------------
# 1. Heights
# ---------------------------------------------------------------------------

def test_explicit_height_tag_wins() -> None:
    assert building_height({"height": "12.5", "building:levels": "40"})[1] == 12.5


def test_height_tag_tolerates_units_and_commas() -> None:
    # OSM is hand-edited; "12 m" and "12,5" are both in the wild.
    assert building_height({"height": "12 m"})[1] == 12.0
    assert building_height({"height": "12,5"})[1] == 12.5


def test_levels_fall_back_to_a_storey_height_plus_a_roof() -> None:
    _, height = building_height({"building:levels": "4"})
    assert height == pytest.approx(4 * buildings.METRES_PER_LEVEL + 1.0)


def test_untagged_building_gets_the_default_height() -> None:
    assert building_height({})[1] == buildings.DEFAULT_HEIGHT_M


def test_absurd_heights_are_clamped_both_ways() -> None:
    # A tagging error must not put a 40 km wall through the viewport, and a
    # zero-height building must not be an invisible polygon.
    assert building_height({"height": "40000"})[1] == buildings.MAX_HEIGHT_M
    assert building_height({"height": "0.1"})[1] == buildings.MIN_HEIGHT_M


def test_min_height_lifts_the_base_but_never_past_the_roof() -> None:
    base, height = building_height({"height": "20", "min_height": "5"})
    assert (base, height) == (5.0, 20.0)
    # A base at or above the roof would extrude nothing, or inside out.
    base, height = building_height({"height": "10", "min_height": "50"})
    assert base < height


def test_building_min_level_is_read_as_storeys() -> None:
    base, _ = building_height({"height": "30", "building:min_level": "2"})
    assert base == pytest.approx(2 * buildings.METRES_PER_LEVEL)


def test_nonsense_tags_do_not_raise() -> None:
    for tags in ({"height": None}, {"height": []}, {"height": "tall"},
                 {"building:levels": "many"}, "not a dict"):
        base, height = building_height(tags)  # type: ignore[arg-type]
        assert height >= buildings.MIN_HEIGHT_M
        assert 0 <= base < height


# ---------------------------------------------------------------------------
# 2. Overpass -> GeoJSON
# ---------------------------------------------------------------------------

def _way(tags: dict, ring: list[tuple[float, float]]) -> dict:
    return {
        "type": "way",
        "id": 1,
        "tags": tags,
        "geometry": [{"lat": lat, "lon": lon} for lon, lat in ring],
    }


SQUARE = [(11.60, 48.00), (11.601, 48.00), (11.601, 48.001), (11.60, 48.001)]


def test_way_becomes_a_closed_polygon_with_only_render_properties() -> None:
    result = to_geojson({"elements": [_way({"building": "yes"}, SQUARE)]})
    assert len(result["features"]) == 1
    feature = result["features"][0]
    ring = feature["geometry"]["coordinates"][0]
    assert ring[0] == ring[-1], "GeoJSON rings must close"
    # Only what the extrusion reads: the payload is cached on a field laptop
    # and every other tag is weight the renderer never looks at.
    assert set(feature["properties"]) == {"height", "min_height"}


def test_non_buildings_and_degenerate_geometry_are_dropped() -> None:
    result = to_geojson({"elements": [
        _way({"highway": "residential"}, SQUARE),          # not a building
        _way({"building": "yes"}, SQUARE[:2]),             # clipped at a cell edge
        {"type": "way", "tags": {"building": "yes"}},      # no geometry at all
    ]})
    assert result["features"] == []


def test_multipolygon_relation_contributes_its_outer_rings() -> None:
    relation = {
        "type": "relation",
        "tags": {"building": "yes", "type": "multipolygon"},
        "members": [
            {"type": "way", "role": "outer",
             "geometry": [{"lat": lat, "lon": lon} for lon, lat in SQUARE]},
            # Courtyards are dropped rather than cut as holes — a filled
            # courtyard is a small error, mis-paired rings are a black smear.
            {"type": "way", "role": "inner",
             "geometry": [{"lat": lat, "lon": lon} for lon, lat in SQUARE]},
        ],
    }
    result = to_geojson({"elements": [relation]})
    assert len(result["features"]) == 1


def test_garbage_input_returns_an_empty_collection() -> None:
    for payload in (None, {}, {"elements": None}, {"elements": ["nope"]}, 42):
        assert to_geojson(payload)["features"] == []  # type: ignore[arg-type]


def test_overpass_query_states_the_bbox_in_overpass_order() -> None:
    # Overpass takes (south, west, north, east); getting this wrong returns an
    # empty set rather than an error, which is invisible until someone looks.
    query = overpass_query(11.0, 48.0, 11.1, 48.1)
    assert "(48.000000,11.000000,48.100000,11.100000)" in query
    assert "out geom;" in query


# ---------------------------------------------------------------------------
# 3. The tile grid
# ---------------------------------------------------------------------------

def test_cell_bounds_tile_the_world_without_gaps() -> None:
    w, s, e, n = cell_bounds(CELL_ZOOM, 17420, 11490)
    right = cell_bounds(CELL_ZOOM, 17421, 11490)
    below = cell_bounds(CELL_ZOOM, 17420, 11491)
    assert e == pytest.approx(right[0])   # shared edge, no gap and no overlap
    assert s == pytest.approx(below[3])
    assert w < e and s < n


def test_cells_for_bounds_covers_the_box_and_survives_nonsense() -> None:
    cells = cells_for_bounds((11.60, 48.06, 11.68, 48.10))
    assert cells and all(z == CELL_ZOOM for z, _, _ in cells)
    assert len(set(cells)) == len(cells), "no cell listed twice"
    assert cells_for_bounds((float("nan"), 0, 1, 1)) == []
    assert cells_for_bounds((1, 1, 0, 0)) == []


def test_cache_path_sits_with_the_tile_caches() -> None:
    # One directory to copy onto the field laptop, governed by the same
    # operator-configurable tile_cache_dir.
    assert default_cache_path("/tmp/tiles").startswith("/tmp/tiles")


# ---------------------------------------------------------------------------
# 4. The cache
# ---------------------------------------------------------------------------

def test_cache_round_trips_and_reports_what_is_on_disk(tmp_path) -> None:
    cache = BuildingCache(str(tmp_path / "b.sqlite"))
    try:
        assert cache.get(15, 1, 2) is None
        cache.put(15, 1, 2, b"payload")
        stored, fetched_at = cache.get(15, 1, 2)
        assert stored == b"payload"
        assert fetched_at <= time.time()
        assert cache.stats() == {"cells": 1, "bytes": 7}
    finally:
        cache.close()


def test_cache_close_is_idempotent_and_never_raises(tmp_path) -> None:
    cache = BuildingCache(str(tmp_path / "b.sqlite"))
    cache.close()
    cache.close()
    # A closed cache reads as a miss rather than an exception: the map draws
    # nothing, which is a missing overlay, not a broken map.
    assert cache.get(15, 1, 2) is None
    assert cache.stats() == {"cells": 0, "bytes": 0}


# ---------------------------------------------------------------------------
# 5. The service: never block, never raise
# ---------------------------------------------------------------------------

def test_uncached_cell_returns_none_immediately(service) -> None:
    """The contract the blank-map bug was about: no network on this thread."""
    started = time.monotonic()
    assert service.cell(CELL_ZOOM, 17420, 11490) is None
    # Well under the 0.5 s request timeout, let alone Overpass' own seconds.
    assert time.monotonic() - started < 0.2


def test_a_cached_cell_is_served_without_the_network(service) -> None:
    payload = gzip.compress(b'{"type":"FeatureCollection","features":[]}')
    service.cache.put(CELL_ZOOM, 17420, 11490, payload)
    assert service.cell(CELL_ZOOM, 17420, 11490) == payload


def test_a_stale_cell_is_still_served(service) -> None:
    """Offline, an old footprint is all there will ever be — and a building
    from last year is still a building."""
    payload = gzip.compress(b"{}")
    service.cache.put(CELL_ZOOM, 1, 1, payload)
    service.cache._conn.execute(  # noqa: SLF001 - forcing age is the point
        "UPDATE building_cells SET fetched_at=?", (time.time() - buildings.CELL_TTL_S * 2,))
    service.cache._conn.commit()  # noqa: SLF001
    assert service.cell(CELL_ZOOM, 1, 1) == payload


def test_a_cell_is_only_queued_once(service) -> None:
    key = (CELL_ZOOM, 5, 5)
    service._enqueue(*key)  # noqa: SLF001 - the de-duplication is the contract
    queued = service._queue.qsize()  # noqa: SLF001
    service._enqueue(*key)  # noqa: SLF001
    assert service._queue.qsize() == queued  # noqa: SLF001


def test_offline_backoff_stops_the_queue_filling(service) -> None:
    service._back_off(60.0)  # noqa: SLF001 - simulating a refused upstream
    before = service._queue.qsize()  # noqa: SLF001
    service.cell(CELL_ZOOM, 9, 9)
    assert service._queue.qsize() == before  # noqa: SLF001


def test_workers_are_daemons_so_shutdown_cannot_hang(service) -> None:
    # The reason this is not a ThreadPoolExecutor: that class joins its
    # threads at interpreter exit, so a shutdown during a slow Overpass call
    # would stall the app for the length of the timeout.
    assert service._workers and all(w.daemon for w in service._workers)  # noqa: SLF001


def test_close_is_idempotent(tmp_path) -> None:
    svc = BuildingService(str(tmp_path / "b.sqlite"), url=DEAD_URL, timeout=0.5)
    svc.close()
    svc.close()


def _square_at(lon: float, lat: float, size: float = 0.0002) -> list[tuple[float, float]]:
    return [(lon, lat), (lon + size, lat), (lon + size, lat + size), (lon, lat + size)]


def _centre_of(cell: tuple[int, int, int]) -> tuple[float, float]:
    w, s, e, n = cell_bounds(*cell)
    return ((w + e) / 2, (s + n) / 2)


class _Answer:
    """urlopen stand-in: one canned Overpass body, counting requests."""

    def __init__(self, payload: dict) -> None:
        self.body = json.dumps(payload).encode()
        self.calls = 0

    def __call__(self, request, timeout=None):
        self.calls += 1
        body = self.body

        class _Resp:
            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

            def read(self, amt=None):
                return body if amt is None else body[:amt]

        return _Resp()


def _wait_for(predicate, seconds: float = 5.0) -> bool:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.02)
    return predicate()


BLOCK = (buildings.BLOCK_ZOOM, 8710, 5741)
BLOCK_CELLS = buildings.cells_of_block(*BLOCK)


def test_a_block_is_four_cells_fetched_in_one_query(tmp_path, monkeypatch) -> None:
    """The worker path end to end, with Overpass stubbed at the socket: one
    request fills all four cells, each with the buildings whose centre is in it."""
    elements = []
    for i, cell in enumerate(BLOCK_CELLS):
        lon, lat = _centre_of(cell)
        way = _way({"building": "yes", "height": str(9 + i)}, _square_at(lon, lat))
        way["id"] = 100 + i
        elements.append(way)
    answer = _Answer({"elements": elements})
    monkeypatch.setattr(buildings.urllib.request, "urlopen", answer)
    svc = BuildingService(str(tmp_path / "b.sqlite"), url=DEAD_URL, timeout=0.5)
    try:
        payload, coming = svc.lookup(*BLOCK_CELLS[0])
        assert payload is None and coming
        for cell in BLOCK_CELLS[1:]:
            svc.lookup(*cell)
        assert _wait_for(lambda: all(svc.cache.get(*c) for c in BLOCK_CELLS))
        assert answer.calls == 1, "four cells, one Overpass query"
        for i, cell in enumerate(BLOCK_CELLS):
            parsed = json.loads(gzip.decompress(svc.cell(*cell)))
            assert [f["properties"]["height"] for f in parsed["features"]] == [9.0 + i]
    finally:
        svc.close()


def test_a_building_on_a_cell_edge_lands_in_exactly_one_cell() -> None:
    """Fetched per cell, a building straddling the edge came back in both and
    was drawn twice, flickering where the two copies fought."""
    west, east = BLOCK_CELLS[0], BLOCK_CELLS[1]
    edge = cell_bounds(*west)[2]
    lat = _centre_of(west)[1]
    straddler = _way({"building": "yes"}, [(edge - 0.0003, lat), (edge + 0.0001, lat),
                                           (edge + 0.0001, lat + 0.0002),
                                           (edge - 0.0003, lat + 0.0002)])
    split = buildings.split_by_cell(to_geojson({"elements": [straddler]}), BLOCK_CELLS)
    counts = {cell: len(fc["features"]) for cell, fc in split.items()}
    assert counts[west] == 1 and counts[east] == 0
    assert sum(counts.values()) == 1


def test_a_building_that_belongs_to_a_neighbouring_block_is_left_to_it() -> None:
    outside = _centre_of((CELL_ZOOM, BLOCK_CELLS[0][1] - 1, BLOCK_CELLS[0][2]))
    way = _way({"building": "yes"}, _square_at(*outside))
    split = buildings.split_by_cell(to_geojson({"elements": [way]}), BLOCK_CELLS)
    assert all(not fc["features"] for fc in split.values())
    assert set(split) == set(BLOCK_CELLS), "empty cells are recorded as looked at"


def test_the_same_element_twice_is_one_building() -> None:
    way = _way({"building": "yes"}, SQUARE)
    assert len(to_geojson({"elements": [way, dict(way)]})["features"]) == 1


@pytest.mark.parametrize("remark,failed", [
    ("runtime error: Query timed out in \"query\" at line 1 after 26 seconds.", True),
    ("runtime error: Query run out of memory using about 2048 MB of RAM.", True),
    ("", False),
    (None, False),
])
def test_an_answer_overpass_cut_short_is_a_failure(remark, failed) -> None:
    payload = {"elements": []}
    if remark is not None:
        payload["remark"] = remark
    assert buildings.overpass_failed(payload) is failed


def test_a_cut_short_answer_is_never_cached(tmp_path, monkeypatch) -> None:
    """Overpass answers 200 with the buildings it had when time ran out. Cached,
    that was a block with half its buildings for a month."""
    lon, lat = _centre_of(BLOCK_CELLS[0])
    answer = _Answer({
        "elements": [_way({"building": "yes"}, _square_at(lon, lat))],
        "remark": "runtime error: Query timed out in \"query\" at line 1 after 61 seconds.",
    })
    monkeypatch.setattr(buildings.urllib.request, "urlopen", answer)
    svc = BuildingService(str(tmp_path / "b.sqlite"), url=DEAD_URL, timeout=0.5)
    try:
        assert svc._request(*BLOCK) is None  # noqa: SLF001
        assert all(svc.cache.get(*cell) is None for cell in BLOCK_CELLS)
    finally:
        svc.close()


@pytest.mark.parametrize("code", [403, 500, 502])
def test_any_refusal_from_overpass_pauses_the_queue(tmp_path, monkeypatch, code) -> None:
    """Only 429 and 504 used to back off. A 403 or a 5xx released the block
    at once, the map's next re-ask queued the same failing query again, and a
    view kept a shared service busy refusing it."""
    def refuse(request, timeout=None):
        raise urllib.error.HTTPError(DEAD_URL, code, "refused", None, None)

    monkeypatch.setattr(buildings.urllib.request, "urlopen", refuse)
    svc = BuildingService(str(tmp_path / "b.sqlite"), url=DEAD_URL, timeout=0.5)
    try:
        assert svc._request(*BLOCK) is None  # noqa: SLF001
        assert svc.lookup(*BLOCK_CELLS[0]) == (None, False)
    finally:
        svc.close()


def test_offline_the_service_says_nothing_is_coming(service) -> None:
    """The map used to poll a cell nobody would fetch, give up, and never ask
    again for the rest of the session."""
    service._back_off(60.0)  # noqa: SLF001
    assert service.lookup(CELL_ZOOM, 17420, 11490) == (None, False)


def test_a_download_queues_its_blocks_nearest_the_centre_first(service) -> None:
    service._back_off(60.0)  # noqa: SLF001 - hold the workers off the list
    bounds = (11.60, 48.06, 11.68, 48.10)
    queued = service.prefetch(bounds)
    blocks = list(service._bulk)  # noqa: SLF001
    assert queued == len(blocks) > 1
    assert len(set(blocks)) == len(blocks), "no block queued twice"
    centre = buildings.block_of(*buildings.cell_of(11.64, 48.08))
    assert blocks[0] == centre
    assert service.stats()["queued"] == queued
    assert service.prefetch(bounds) == 0, "asking again queues nothing new"


def test_a_download_skips_what_is_already_on_disk_and_respects_its_cap(service) -> None:
    service._back_off(60.0)  # noqa: SLF001
    for cell in BLOCK_CELLS:
        service.cache.put(*cell, gzip.compress(b"{}"))
    w, s, e, n = cell_bounds(*BLOCK)
    assert service.prefetch((w + 1e-6, s + 1e-6, e - 1e-6, n - 1e-6)) == 0
    assert service.prefetch((11.0, 47.0, 12.0, 48.0), limit=5) == 5


def test_a_download_over_a_huge_area_does_not_list_the_whole_area(service, monkeypatch) -> None:
    """Framed on a zoomed-out view, a download's area is millions of cells.
    Every one of them was listed and sorted to keep the nearest 256, and the
    backend ran out of memory before it got there."""
    service._back_off(60.0)  # noqa: SLF001
    monkeypatch.setattr(buildings, "cells_for_bounds",
                        lambda *_a, **_k: pytest.fail("the whole area was listed"))
    started = time.monotonic()
    assert service.prefetch((-180.0, -85.0, 180.0, 85.0)) == buildings.PREFETCH_MAX_BLOCKS
    assert time.monotonic() - started < 2.0
    assert service._bulk[0] == buildings.block_of(*buildings.cell_of(0.0, 0.0))  # noqa: SLF001


def test_the_outward_walk_covers_the_area_once_nearest_first() -> None:
    bounds = (11.0, 47.0, 12.0, 48.0)
    blocks = list(buildings.blocks_outward(bounds))
    expected = {buildings.block_of(*cell) for cell in cells_for_bounds(bounds)}
    assert len(blocks) == len(set(blocks)) and set(blocks) == expected
    _, cx, cy = buildings.cell_of(11.5, 47.5, buildings.BLOCK_ZOOM)
    rings = [max(abs(x - cx), abs(y - cy)) for _z, x, y in blocks]
    assert rings == sorted(rings), "never a farther ring before a nearer one"


def test_a_view_is_served_before_a_download(service) -> None:
    service._back_off(60.0)  # noqa: SLF001
    service.prefetch((11.60, 48.06, 11.68, 48.10))
    service._offline_until = 0.0  # noqa: SLF001
    service._queue.put_nowait(BLOCK)  # noqa: SLF001
    assert service._next() == BLOCK  # noqa: SLF001


def test_a_failed_download_block_is_retried_a_few_times_then_let_go(service) -> None:
    service._back_off(60.0)  # noqa: SLF001
    service.prefetch((11.60, 48.06, 11.61, 48.061), limit=1)
    block = service._bulk.popleft()  # noqa: SLF001
    for _ in range(buildings.BULK_MAX_TRIES - 1):
        service._settle(block, fetched=False)  # noqa: SLF001
        assert service._bulk[-1] == block  # noqa: SLF001
        service._bulk.pop()  # noqa: SLF001
    service._settle(block, fetched=False)  # noqa: SLF001
    assert block not in service._bulk  # noqa: SLF001
    assert block not in service._queued  # noqa: SLF001


def test_the_route_says_unavailable_when_nothing_is_coming(tmp_path) -> None:
    """Offline the map must stop asking until the view changes, not poll a
    cell no worker will fetch and then give up on it for good."""
    from corvus.server import CorvusHandler

    svc = BuildingService(str(tmp_path / "b.sqlite"), url=DEAD_URL, timeout=0.5)
    svc._back_off(60.0)  # noqa: SLF001

    class _Rec:
        _api_buildings_serve = CorvusHandler._api_buildings_serve
        _send_buildings = CorvusHandler._send_buildings

        def __init__(self):
            self.buildings = svc
            self.headers = {}
            self.body = b""
            self.wfile = self

        def send_response(self, status):
            self.status = status

        def send_header(self, name, value):
            self.headers[name] = value

        def end_headers(self):
            pass

        def write(self, data):
            self.body += data

        def _send_cors(self):
            pass

    try:
        rec = _Rec()
        rec._api_buildings_serve(CELL_ZOOM, 17420, 11490)
        payload = json.loads(rec.body)
        assert payload.get("unavailable") is True and "pending" not in payload
        assert rec.headers["Cache-Control"] == "no-store"
    finally:
        svc.close()
