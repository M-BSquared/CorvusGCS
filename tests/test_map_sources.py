"""Map source registry + URL token-substitution regression tests.

Covers the map overhaul that (a) vendored MapLibre GL JS locally and
(b) added ``osm`` and ``topo`` sources alongside the existing ArcGIS
satellite/streets/hybrid sources, with attribution on every entry.

These tests are hermetic and offline: no network is ever hit. The URL
token-substitution tests exercise the REAL production code paths
(``CorvusHandler._fetch_upstream_tile`` for the online browser fill and
``TileDownloader._download_one`` for the pre-download worker) with
``urllib.request.urlopen`` monkeypatched to capture the URL, so the
order-agnostic substitution claim is verified on both code paths that
build a tile URL.

The OSM upstream uses slippy ``{z}/{x}/{y}`` order while ArcGIS uses
``{z}/{y}/{x}`` order. The two production substitution sites replace the
tokens in *different* orders (server: z,y,x ; downloader: z,x,y), but both
must produce the same correct final URL because the substituted values
are plain integer strings with no brace characters. These tests are the
regression guard against someone "fixing" one site's replace order and
silently breaking the other source's path order.
"""
from __future__ import annotations

import re
import threading
import urllib.request

import pytest

from corvus import tile_sources
from corvus.tile_sources import TILE_SOURCES, get as sources_get, list_sources


# ---------------------------------------------------------------------------
# 1. Registry shape: every entry has the required keys + a non-empty attribution
# ---------------------------------------------------------------------------

REQUIRED_KEYS = {"label", "provider", "style", "upstream", "maxzoom", "attribution"}


@pytest.mark.parametrize("sid", list(TILE_SOURCES))
def test_entry_has_required_keys_and_nonempty_attribution(sid: str) -> None:
    entry = TILE_SOURCES[sid]
    assert set(entry) == REQUIRED_KEYS, f"{sid}: unexpected keys {set(entry) ^ REQUIRED_KEYS}"
    assert isinstance(entry["label"], str) and entry["label"], f"{sid}: label missing"
    # A template addresses a tile either by {z}/{x}/{y} or by a Bing quadkey
    # ({q}); one of the two must be present or the URL cannot be built.
    upstream = entry["upstream"]
    assert isinstance(upstream, str)
    assert "{z}" in upstream or "{q}" in upstream, f"{sid}: upstream addresses no tile"
    assert isinstance(entry["maxzoom"], int) and entry["maxzoom"] > 0, f"{sid}: bad maxzoom"
    # attribution is the legally-required credit string; empty is a defect.
    attr = entry["attribution"]
    assert isinstance(attr, str) and attr.strip(), f"{sid}: attribution must be a non-empty string"
    assert entry["provider"] in tile_sources.PROVIDERS, f"{sid}: unknown provider"
    assert entry["style"] in tile_sources.STYLE_LABELS, f"{sid}: unknown style"


@pytest.mark.parametrize("sid", list(TILE_SOURCES))
def test_get_returns_copy_and_does_not_mutate_registry(sid: str) -> None:
    entry = sources_get(sid)
    assert entry is not None
    assert entry == dict(TILE_SOURCES[sid])
    entry["attribution"] = "tampered"
    assert sources_get(sid)["attribution"] == TILE_SOURCES[sid]["attribution"]


def test_get_returns_none_for_unknown_source() -> None:
    assert sources_get("does-not-exist") is None


# ---------------------------------------------------------------------------
# 2. Path-order regression guards (do NOT "fix" the order)
# ---------------------------------------------------------------------------

def test_osm_uses_zxy_slippy_order() -> None:
    """OSM serves slippy-order tiles: {z}/{x}/{y}. A regression guard against
    accidentally 'correcting' this to ArcGIS's {z}/{y}/{x} order."""
    assert TILE_SOURCES["osm"]["upstream"] == "https://tile.openstreetmap.org/{z}/{x}/{y}.png"


def test_satellite_uses_zyx_arcgis_order() -> None:
    """ArcGIS REST tile endpoints place path components in {z}/{y}/{x} order
    (ArcGIS-native). A regression guard against flipping this to OSM order."""
    assert TILE_SOURCES["satellite"]["upstream"].endswith("/tile/{z}/{y}/{x}")


# ---------------------------------------------------------------------------
# 3. list_sources() returns exactly the 5 ids with labels
# ---------------------------------------------------------------------------

def test_list_sources_covers_the_registry_in_order() -> None:
    sources = list_sources()
    ids = [s["id"] for s in sources]
    # Exactly the registered set — no more, no less.
    assert set(ids) == set(TILE_SOURCES)
    # list_sources preserves the registry insertion order (a stable source
    # picker order is a mild UX contract); pin it as a regression guard.
    assert ids == list(TILE_SOURCES)
    for s in sources:
        # The exact payload the frontend consumes. `attribution` is part of it
        # because the frontend no longer mirrors the registry — this endpoint
        # is where it gets the credit string from.
        assert set(s) == {"id", "label", "provider", "style", "maxzoom", "attribution"}
        entry = TILE_SOURCES[s["id"]]
        assert s["label"] == entry["label"]
        assert s["maxzoom"] == entry["maxzoom"]
        assert s["provider"] == entry["provider"]
        assert s["style"] == entry["style"]
        assert s["attribution"] == entry["attribution"]


def test_list_sources_contains_topo_and_osm() -> None:
    ids = {s["id"] for s in list_sources()}
    assert {"satellite", "streets", "hybrid", "topo", "osm"} <= ids


# ---------------------------------------------------------------------------
# 4. Attribution correctness (legally required) — OSM + ESRI present
# ---------------------------------------------------------------------------

def test_osm_attribution_present() -> None:
    assert TILE_SOURCES["osm"]["attribution"] == "© OpenStreetMap contributors"


def test_esri_attributions_present_for_all_arcgis_sources() -> None:
    for sid in ("satellite", "streets", "hybrid", "topo"):
        attr = TILE_SOURCES[sid]["attribution"]
        assert "Esri" in attr or "© Esri" in attr, f"{sid}: missing Esri attribution"


# ---------------------------------------------------------------------------
# 5. Token substitution — the REAL production URL builders
# ---------------------------------------------------------------------------
# Both code paths that build a tile URL replace {z}/{x}/{y} (or {z}/{y}/{x})
# inline. The server's online fill (`_fetch_upstream_tile`) and the downloader
# worker (`_download_one`) replace in DIFFERENT orders, but both must produce
# the correct URL for an OSM source AND an ArcGIS source. We capture the URL
# each path hands to urlopen and assert it.

class _FakeResp:
    """Context-manager response returning fixed bytes; never touches network."""

    def __init__(self, data: bytes) -> None:
        self._data = data

    def __enter__(self) -> "_FakeResp":
        return self

    def __exit__(self, *exc) -> bool:
        return False

    def read(self, amt: int | None = None) -> bytes:
        # Mirrors http.client.HTTPResponse.read(amt): the caller reads a
        # bounded number of bytes, not the whole body unconditionally.
        return self._data if amt is None else self._data[:amt]


def _make_url_recorder(monkeypatch, recorded: list, want_request: bool):
    """Patch urlopen to record the URL and return fixed bytes.

    *want_request* selects how the patched site calls urlopen: the server
    builds a ``urllib.request.Request`` and passes it, while the downloader
    passes a plain URL string. Both expose the final URL.
    """

    def fake_urlopen(url_or_req, timeout=None):
        if isinstance(url_or_req, urllib.request.Request):
            recorded.append(url_or_req.full_url)
        else:
            recorded.append(str(url_or_req))
        return _FakeResp(b"\x89PNG\r\n\x1a\n" + b"captured")

    monkeypatch.setattr(urllib.request, "urlopen", fake_urlopen)
    return fake_urlopen


@pytest.fixture
def server_handler():
    """A bare CorvusHandler (no socket) so _fetch_upstream_tile is callable.

    ``corvus.server`` imports pymavlink/paramiko at module load time, so guard
    the import the same way the other tile tests do.
    """
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    from corvus.server import CorvusHandler
    return object.__new__(CorvusHandler)


def test_server_fetch_url_for_osm_source(server_handler, monkeypatch) -> None:
    """Server online-fill builds the OSM URL in slippy {z}/{x}/{y} order."""
    recorded: list[str] = []
    _make_url_recorder(monkeypatch, recorded, want_request=True)
    blob = server_handler._fetch_upstream_tile("osm", 3, 4, 5)
    assert blob is not None
    assert recorded == ["https://tile.openstreetmap.org/3/4/5.png"]


def test_server_fetch_url_for_arcgis_source(server_handler, monkeypatch) -> None:
    """Server online-fill builds the ArcGIS URL in {z}/{y}/{x} order."""
    recorded: list[str] = []
    _make_url_recorder(monkeypatch, recorded, want_request=True)
    blob = server_handler._fetch_upstream_tile("satellite", 3, 4, 5)
    assert blob is not None
    assert recorded and recorded[0].endswith("/tile/3/5/4")


def test_server_fetch_unknown_source_returns_none(server_handler, monkeypatch) -> None:
    recorded: list[str] = []
    _make_url_recorder(monkeypatch, recorded, want_request=True)
    assert server_handler._fetch_upstream_tile("nope", 3, 4, 5) is None
    assert recorded == []  # unknown source short-circuits before urlopen


class _FakeCache:
    """Minimal cache stand-in: records put_tile calls; no SQLite."""

    def __init__(self) -> None:
        self.puts: list[tuple[int, int, int, bytes]] = []

    def put_tile(self, z: int, x: int, y: int, blob: bytes) -> None:
        self.puts.append((z, x, y, blob))


def _make_download_job(template: str) -> dict:
    """Build the minimal job dict TileDownloader._download_one reads."""
    return {
        "job_id": "t", "source": "t", "state": "running",
        "done": 0, "total": 1, "failed": 0, "error": None,
        "_cancel": threading.Event(),       # cleared -> not cancelled
        "_template": template,
        "_lock": threading.Lock(),
        "_on_progress": None,
    }


@pytest.fixture
def downloader():
    """A TileDownloader bound to a fake cache (no SQLite, no network)."""
    from corvus.tile_downloader import TileDownloader
    cache = _FakeCache()
    dl = TileDownloader(cache, max_workers=1)
    return dl, cache


def test_downloader_url_for_osm_template(downloader, monkeypatch) -> None:
    """Downloader worker builds the OSM URL in slippy {z}/{x}/{y} order.

    The downloader replaces tokens in order z,x,y (opposite of the server's
    z,y,x); both must agree on the final URL for OSM's {z}/{x}/{y} template.
    """
    dl, cache = downloader
    recorded: list[str] = []
    _make_url_recorder(monkeypatch, recorded, want_request=False)
    job = _make_download_job(TILE_SOURCES["osm"]["upstream"])
    dl._download_one(job, 3, 4, 5)
    assert recorded == ["https://tile.openstreetmap.org/3/4/5.png"]
    assert cache.puts and cache.puts[0][:3] == (3, 4, 5)


def test_downloader_url_for_arcgis_template(downloader, monkeypatch) -> None:
    """Downloader worker builds the ArcGIS URL in {z}/{y}/{x} order.

    Guards against the opposite-order 'fix' breaking ArcGIS sources: the
    downloader's z,x,y replace order must still yield /tile/3/5/4 for an
    ArcGIS {z}/{y}/{x} template.
    """
    dl, _ = downloader
    recorded: list[str] = []
    _make_url_recorder(monkeypatch, recorded, want_request=False)
    job = _make_download_job(TILE_SOURCES["satellite"]["upstream"])
    dl._download_one(job, 3, 4, 5)
    assert recorded and recorded[0].endswith("/tile/3/5/4")


# ---------------------------------------------------------------------------
# 6. The frontend no longer mirrors the registry
# ---------------------------------------------------------------------------
# src/js/map.js used to carry a hand-copied TILE table of every id, label,
# maxzoom and attribution. It now hydrates that catalogue from
# GET /api/tiles/sources, which reads this registry — so there is exactly one
# place a new source has to be declared, and no way to ship one uncredited.
# These tests guard that direction: the frontend must NOT grow the mirror back,
# and must still reference the vendored MapLibre (no CDN, no remote glyphs).

def test_frontend_map_js_does_not_mirror_the_registry() -> None:
    import pathlib
    src = pathlib.Path(__file__).resolve().parent.parent / "src" / "js" / "map.js"
    text = src.read_text(encoding="utf-8")

    # The bootstrap entry (the layer painted before any fetch resolves) is the
    # single allowed hardcoded source, so its attribution may appear once.
    bootstrap_attr = TILE_SOURCES["satellite"]["attribution"]
    for sid, entry in TILE_SOURCES.items():
        if entry["attribution"] == bootstrap_attr:
            continue
        assert entry["attribution"] not in text, (
            f"map.js hardcodes the attribution for {sid!r}; it must come from "
            "GET /api/tiles/sources instead"
        )
    for sid in TILE_SOURCES:
        if sid == "satellite":
            continue
        assert f'"{sid}"' not in text, (
            f"map.js hardcodes the source id {sid!r}; the catalogue is fetched"
        )

    assert "/api/tiles/sources" in text, "map.js must fetch the source catalogue"
    # The tile credit is a legal requirement, so it must be on the map one way or
    # the other. The constructor flag is off on purpose — the control is added
    # explicitly so it can be placed bottom-left, out of the control rail's
    # corner — which is exactly the substitution this check has to allow without
    # letting the credit be dropped altogether.
    assert (
        "attributionControl: true" in text
        or re.search(r"addControl\(\s*new maplibregl\.AttributionControl\(", text)
    ), "frontend map.js must render the attribution control"
    # No CDN / no remote glyphs remain in the map module.
    assert "unpkg.com/maplibre" not in text, "frontend map.js still references the maplibre CDN"
    assert "demotiles.maplibre.org" not in text, "frontend map.js still references remote glyphs"


def test_sources_endpoint_payload_carries_attribution_for_every_source() -> None:
    """Every source the UI can pick reports its credit string."""
    for s in list_sources():
        assert s["attribution"].strip(), f"{s['id']}: attribution missing from the API payload"


# ---------------------------------------------------------------------------
# 7. Provider grouping (Esri / OpenStreetMap / Google / Bing)
# ---------------------------------------------------------------------------

def test_every_source_belongs_to_exactly_one_provider() -> None:
    seen: list[str] = []
    for prov in tile_sources.PROVIDERS.values():
        seen.extend(prov["sources"])
    assert sorted(seen) == sorted(TILE_SOURCES), "provider grouping must partition the registry"
    assert len(seen) == len(set(seen)), "a source is listed under two providers"


def test_expected_providers_are_registered() -> None:
    assert set(tile_sources.PROVIDERS) == {"esri", "osm", "google", "bing"}
    assert tile_sources.DEFAULT_PROVIDER in tile_sources.PROVIDERS


def test_list_providers_shape() -> None:
    provs = tile_sources.list_providers()
    assert [p["id"] for p in provs] == list(tile_sources.PROVIDERS)
    for p in provs:
        assert set(p) == {"id", "label", "sources"}
        assert p["label"].strip()
        assert p["sources"] == tile_sources.PROVIDERS[p["id"]]["sources"]


def test_list_providers_returns_a_copy() -> None:
    """Mutating the returned source list must not corrupt the registry."""
    provs = tile_sources.list_providers()
    provs[0]["sources"].append("tampered")
    assert "tampered" not in tile_sources.PROVIDERS[provs[0]["id"]]["sources"]


def test_provider_of_and_sources_for_round_trip() -> None:
    for sid, entry in TILE_SOURCES.items():
        pid = tile_sources.provider_of(sid)
        assert pid == entry["provider"]
        assert sid in tile_sources.sources_for(pid)
    assert tile_sources.provider_of("nope") is None
    assert tile_sources.sources_for("nope") == []


def test_resolve_source_keeps_the_style_across_a_provider_switch() -> None:
    """Switching service from Esri Satellite lands on Google Satellite, not on
    an arbitrary Google layer."""
    assert tile_sources.resolve_source("google", "satellite") == "google_satellite"
    assert tile_sources.resolve_source("bing", "streets") == "bing_streets"
    assert tile_sources.resolve_source("esri", "topo") == "topo"


def test_resolve_source_falls_back_to_the_provider_default() -> None:
    # OSM has no satellite layer; fall back to its first (only) source.
    assert tile_sources.resolve_source("osm", "satellite") == "osm"
    # No style requested at all -> the provider's default.
    assert tile_sources.resolve_source("google") == "google_satellite"
    # Unknown provider -> None, never a bogus id.
    assert tile_sources.resolve_source("nope", "satellite") is None


# ---------------------------------------------------------------------------
# 8. build_tile_url — the single URL builder (incl. Bing quadkeys)
# ---------------------------------------------------------------------------

def test_quadkey_matches_the_virtualearth_definition() -> None:
    # z=0 addresses the single world tile and has no digits.
    assert tile_sources.quadkey(0, 0, 0) == ""
    # The canonical Microsoft worked example.
    assert tile_sources.quadkey(3, 3, 5) == "213"
    # One digit per zoom level, always.
    assert len(tile_sources.quadkey(12, 1000, 2000)) == 12
    assert set(tile_sources.quadkey(12, 1000, 2000)) <= set("0123")


def test_server_shard_is_deterministic_and_in_range() -> None:
    """A tile must always resolve to the same mirror — a random shard would
    defeat upstream caching and make these URLs untestable."""
    assert tile_sources.server_shard(4, 5) == tile_sources.server_shard(4, 5)
    for x in range(8):
        for y in range(8):
            assert tile_sources.server_shard(x, y) in {"0", "1", "2", "3"}


def test_build_tile_url_substitutes_every_token() -> None:
    url = tile_sources.build_tile_url(TILE_SOURCES["bing_satellite"]["upstream"], 3, 4, 5)
    assert "{q}" not in url and "{s}" not in url
    assert f"/tiles/a{tile_sources.quadkey(3, 4, 5)}.jpeg" in url
    url = tile_sources.build_tile_url(TILE_SOURCES["google_satellite"]["upstream"], 3, 4, 5)
    assert url == f"https://mt{tile_sources.server_shard(4, 5)}.google.com/vt/lyrs=s&hl=en&x=4&y=5&z=3"


@pytest.mark.parametrize("sid", list(TILE_SOURCES))
def test_build_tile_url_leaves_no_placeholder_behind(sid: str) -> None:
    url = tile_sources.build_tile_url(TILE_SOURCES[sid]["upstream"], 7, 11, 13)
    for token in ("{z}", "{x}", "{y}", "{q}", "{s}"):
        assert token not in url, f"{sid}: {token} not substituted"


def test_index_html_loads_vendored_maplibre() -> None:
    import pathlib
    html = pathlib.Path(__file__).resolve().parent.parent / "src" / "index.html"
    text = html.read_text(encoding="utf-8")
    assert 'vendor/maplibre-gl.css' in text, "index.html must load vendored maplibre CSS"
    assert 'vendor/maplibre-gl.min.js' in text, "index.html must load vendored maplibre JS"
    assert "unpkg.com/maplibre" not in text, "index.html still loads maplibre from a CDN"


def test_vendored_maplibre_is_a_real_bundle() -> None:
    """The vendored JS is a genuine MapLibre UMD bundle, not a 404/error page."""
    import pathlib
    js = pathlib.Path(__file__).resolve().parent.parent / "src" / "vendor" / "maplibre-gl.min.js"
    text = js.read_text(encoding="utf-8")
    assert "MapLibre GL JS" in text, "vendored maplibre missing license header"
    assert "@license" in text, "vendored maplibre missing license tag"
    assert "global.maplibregl = factory()" in text, "vendored maplibre missing UMD export"
    assert "return maplibregl" in text, "vendored maplibre missing factory return"

    css = pathlib.Path(__file__).resolve().parent.parent / "src" / "vendor" / "maplibre-gl.css"
    css_text = css.read_text(encoding="utf-8")
    assert ".maplibregl-map" in css_text, "vendored maplibre CSS missing core rule"
