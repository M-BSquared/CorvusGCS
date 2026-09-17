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


def test_a_fetched_cell_lands_in_the_cache_as_gzipped_geojson(tmp_path) -> None:
    """The worker path end to end, with Overpass stubbed out."""
    svc = BuildingService(str(tmp_path / "b.sqlite"), url=DEAD_URL, timeout=0.5)
    try:
        svc._request = lambda z, x, y: buildings._gzip_json(  # noqa: SLF001
            to_geojson({"elements": [_way({"building": "yes", "height": "9"}, SQUARE)]}))
        svc.cell(CELL_ZOOM, 3, 3)
        deadline = time.monotonic() + 5
        while svc.cache.get(CELL_ZOOM, 3, 3) is None and time.monotonic() < deadline:
            time.sleep(0.02)
        stored = svc.cell(CELL_ZOOM, 3, 3)
        assert stored is not None
        parsed = json.loads(gzip.decompress(stored))
        assert parsed["features"][0]["properties"]["height"] == 9.0
    finally:
        svc.close()
