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

import threading
import urllib.request

import pytest

from corvus import tile_sources
from corvus.tile_sources import TILE_SOURCES, get as sources_get, list_sources


# ---------------------------------------------------------------------------
# 1. Registry shape: every entry has the required keys + a non-empty attribution
# ---------------------------------------------------------------------------

REQUIRED_KEYS = {"label", "upstream", "maxzoom", "attribution"}


@pytest.mark.parametrize("sid", list(TILE_SOURCES))
def test_entry_has_required_keys_and_nonempty_attribution(sid: str) -> None:
    entry = TILE_SOURCES[sid]
    assert set(entry) == REQUIRED_KEYS, f"{sid}: unexpected keys {set(entry) ^ REQUIRED_KEYS}"
    assert isinstance(entry["label"], str) and entry["label"], f"{sid}: label missing"
    assert isinstance(entry["upstream"], str) and "{z}" in entry["upstream"], f"{sid}: upstream has no {{z}} token"
    assert isinstance(entry["maxzoom"], int) and entry["maxzoom"] > 0, f"{sid}: bad maxzoom"
    # attribution is the legally-required credit string; empty is a defect.
    attr = entry["attribution"]
    assert isinstance(attr, str) and attr.strip(), f"{sid}: attribution must be a non-empty string"


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

def test_list_sources_returns_exactly_five_ids_with_labels() -> None:
    sources = list_sources()
    ids = [s["id"] for s in sources]
    # Exactly the registered set — no more, no less.
    assert set(ids) == set(TILE_SOURCES)
    assert len(ids) == 5
    # list_sources preserves the registry insertion order (a stable source
    # picker order is a mild UX contract); pin it as a regression guard.
    assert ids == list(TILE_SOURCES)
    for s in sources:
        assert set(s) == {"id", "label", "maxzoom"}
        assert s["label"] == TILE_SOURCES[s["id"]]["label"]
        assert s["maxzoom"] == TILE_SOURCES[s["id"]]["maxzoom"]


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

    def read(self) -> bytes:
        return self._data


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
# 6. Frontend mirror consistency (src/js/map.js TILE mirrors the registry)
# ---------------------------------------------------------------------------
# A source-text contract guard: the frontend `TILE` must list the same 5 ids as
# the Python registry and carry each registry attribution string, and the map
# must reference the vendored MapLibre (no CDN, no demotiles glyphs). This
# catches a desync the dual-maintenance comment in map.js warns about.

def test_frontend_map_js_mirrors_registry_attributions() -> None:
    import pathlib
    src = pathlib.Path(__file__).resolve().parent.parent / "src" / "js" / "map.js"
    text = src.read_text(encoding="utf-8")
    for sid, entry in TILE_SOURCES.items():
        assert f"{sid}:" in text, f"frontend TILE missing entry for {sid!r}"
        assert entry["attribution"] in text, (
            f"frontend map.js missing attribution for {sid!r}: {entry['attribution']!r}"
        )
    assert 'attributionControl: true' in text, "frontend map.js must enable attributionControl"
    # No CDN / no remote glyphs remain in the map module.
    assert "unpkg.com/maplibre" not in text, "frontend map.js still references the maplibre CDN"
    assert "demotiles.maplibre.org" not in text, "frontend map.js still references remote glyphs"


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
