"""The elevation (DEM) half of the tile registry, and the buildings route.

3D mode reads its heights from elevation tiles that travel the SAME route and
the SAME per-id MBTiles cache as the imagery — that is what makes 3D terrain
work on a field laptop with no network. What this file pins is the seam that
makes that safe: a DEM is servable and cacheable everywhere, and a base layer
everywhere else. Cross those and either a DEM lands in the operator's layer
switcher (a map painted in false colour) or elevation is served as imagery.
"""
from __future__ import annotations

import json

import pytest

from corvus import tile_sources
from corvus.server import CorvusHandler
from corvus.tile_sources import (
    DEFAULT_TERRAIN,
    TERRAIN_SOURCES,
    TILE_SOURCES,
    all_sources,
    is_terrain,
    list_sources,
    list_terrain,
)


# ---------------------------------------------------------------------------
# 1. The two registries stay apart
# ---------------------------------------------------------------------------

def test_terrain_ids_never_shadow_a_base_layer() -> None:
    # They share a route and a cache directory, so a collision would give the
    # two one MBTiles file and serve elevation bytes as imagery.
    assert not (set(TERRAIN_SOURCES) & set(TILE_SOURCES))


def test_a_dem_is_never_offered_as_a_base_layer() -> None:
    # list_sources() feeds the map's layer switcher and the Appearance picker.
    ids = {s["id"] for s in list_sources()}
    assert not (ids & set(TERRAIN_SOURCES))


def test_all_sources_is_the_union_and_is_what_gets_a_cache() -> None:
    combined = all_sources()
    assert set(combined) == set(TILE_SOURCES) | set(TERRAIN_SOURCES)
    # Every entry must be servable: an upstream template that addresses a
    # tile (by {z}/{x}/{y} or by Bing's quadkey), a zoom cap, and a credit.
    for sid, entry in combined.items():
        template = entry["upstream"]
        assert "{z}" in template or "{q}" in template, sid
        assert isinstance(entry["maxzoom"], int) and entry["maxzoom"] > 0, sid
        assert entry["attribution"], sid


def test_is_terrain_classifies_both_kinds() -> None:
    assert is_terrain(DEFAULT_TERRAIN)
    assert not is_terrain("satellite")
    assert not is_terrain("nothing-like-this")


# ---------------------------------------------------------------------------
# 2. The DEM descriptor the frontend builds its raster-dem source from
# ---------------------------------------------------------------------------

def test_list_terrain_shape() -> None:
    entries = list_terrain()
    assert entries, "3D mode needs at least one DEM"
    for entry in entries:
        assert set(entry) == {"id", "label", "encoding", "maxzoom", "attribution"}
        # The height packing. MapLibre takes this verbatim; a wrong value does
        # not fail, it silently renders the wrong mountains.
        assert entry["encoding"] in {"terrarium", "mapbox", "custom"}


def test_the_default_dem_exists_and_is_resolvable() -> None:
    assert DEFAULT_TERRAIN in TERRAIN_SOURCES
    entry = tile_sources.get(DEFAULT_TERRAIN)
    assert entry is not None and entry["encoding"] == "terrarium"


def test_get_resolves_both_kinds_but_still_returns_copies() -> None:
    entry = tile_sources.get(DEFAULT_TERRAIN)
    entry["upstream"] = "tampered"
    assert tile_sources.get(DEFAULT_TERRAIN)["upstream"] != "tampered"
    assert tile_sources.get("satellite") is not None
    assert tile_sources.get("does-not-exist") is None


def test_dem_tiles_are_png_addressed_like_every_other_tile() -> None:
    # The serve route only matches ".png" with three numeric segments, and the
    # height packing needs lossless bytes — a JPEG DEM is noise.
    upstream = TERRAIN_SOURCES[DEFAULT_TERRAIN]["upstream"]
    assert upstream.endswith(".png")
    url = tile_sources.build_tile_url(upstream, 12, 2145, 1436)
    assert "/12/2145/1436.png" in url
    assert "{" not in url


def test_dem_attribution_credits_the_data_not_just_the_host() -> None:
    attribution = TERRAIN_SOURCES[DEFAULT_TERRAIN]["attribution"]
    assert "SRTM" in attribution or "USGS" in attribution


# ---------------------------------------------------------------------------
# 3. The buildings route: always drawable, never an error
# ---------------------------------------------------------------------------

class _Recorder:
    """The slice of CorvusHandler the two building methods touch.

    The real handler is an HTTP connection; both methods under test only ever
    reach for the response-writing surface, so that is all this supplies —
    which is what lets the route be asserted without a socket.
    """

    _api_buildings_serve = CorvusHandler._api_buildings_serve
    _send_buildings = CorvusHandler._send_buildings

    def __init__(self, service):
        self.buildings = service
        self.status = None
        self.headers: dict[str, str] = {}
        self.body = b""
        self.wfile = self

    # -- handler surface --
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


def _serve(service, z=15, x=17420, y=11490):
    recorder = _Recorder(service)
    recorder._api_buildings_serve(z, x, y)
    return recorder


class _StubService:
    def __init__(self, payload):
        self.payload = payload
        self.asked = []

    def cell(self, z, x, y):
        self.asked.append((z, x, y))
        return self.payload


def test_a_cached_cell_is_served_gzipped_and_cacheable() -> None:
    recorder = _serve(_StubService(b"gzipped-bytes"))
    assert recorder.status == 200
    assert recorder.headers["Content-Encoding"] == "gzip"
    assert recorder.headers["Content-Type"] == "application/geo+json"
    assert "max-age" in recorder.headers["Cache-Control"]
    assert recorder.body == b"gzipped-bytes"


def test_a_pending_cell_says_so_and_is_never_browser_cached() -> None:
    # Without no-store the browser would replay "nothing yet" from its own
    # cache long after the worker had filled ours, and the buildings would
    # never appear.
    recorder = _serve(_StubService(None))
    assert recorder.status == 200
    assert recorder.headers["Cache-Control"] == "no-store"
    assert json.loads(recorder.body)["pending"] is True


def test_no_service_still_answers_something_drawable() -> None:
    recorder = _serve(None)
    assert recorder.status == 200
    payload = json.loads(recorder.body)
    assert payload["features"] == []
    # Nothing is coming, so the frontend must not be told to ask again.
    assert "pending" not in payload


def test_an_off_grid_zoom_is_answered_empty_not_fetched() -> None:
    service = _StubService(b"x")
    recorder = _serve(service, z=12)
    assert recorder.status == 200
    assert json.loads(recorder.body)["features"] == []
    assert service.asked == [], "an off-grid zoom must never reach the service"


@pytest.mark.parametrize("x,y", [(-1, 0), (0, -1), (1 << 15, 0), (0, 1 << 15)])
def test_out_of_range_cells_are_answered_empty(x, y) -> None:
    recorder = _serve(_StubService(b"x"), x=x, y=y)
    assert recorder.status == 200
    assert json.loads(recorder.body)["features"] == []


def test_a_raising_service_does_not_500_the_map() -> None:
    class _Broken:
        def cell(self, z, x, y):
            raise RuntimeError("disk on fire")

    recorder = _serve(_Broken())
    assert recorder.status == 200
    assert json.loads(recorder.body)["features"] == []
