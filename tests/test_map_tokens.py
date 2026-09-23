"""Map service API keys: the credential path from the dialog to the upstream.

A keyed map service (MapTiler, Mapbox) serves nothing without a key the
operator supplies. That key is a credential, so the properties worth pinning
are not about tiles at all:

* it is stored only in ``~/.corvus/config.json``, which is 0600;
* it never comes back out — not through ``GET /api/config``, not through the
  tile-source listing, not in a download job's public snapshot, not in a log
  line;
* it never reaches the browser, because the tiles are proxied by this process,
  which is what makes an XSS in the page unable to steal it;
* it is percent-encoded into the upstream URL, so a key cannot add URL
  structure of its own.

Hermetic and offline: no request ever leaves the process.
"""
from __future__ import annotations

import http.client
import json
import threading

import pytest

from corvus import tile_sources
from corvus.config import (
    MAX_MAP_TOKEN_CHARS,
    CorvusConfig,
    _build_config,
    load_config,
    save_config,
    to_public_dict,
)


# ---------------------------------------------------------------------------
# The registry: which services are keyed, and how they say so
# ---------------------------------------------------------------------------

def test_the_keyed_services_are_the_ones_with_a_token_block() -> None:
    assert tile_sources.keyed_providers() == ["maptiler", "mapbox"]
    for pid in tile_sources.keyed_providers():
        meta = tile_sources.token_meta(pid)
        assert set(meta) == {"label", "signup", "help"}
        # The sign-up URL is shown to the operator so they can go and get a
        # key; an https one is the only kind worth printing.
        assert meta["signup"].startswith("https://")


def test_an_unkeyed_service_reports_no_token_block() -> None:
    for pid in ("esri", "osm", "google", "bing"):
        assert tile_sources.token_meta(pid) is None
    assert tile_sources.token_meta("no-such-provider") is None


def test_needs_token_follows_the_provider_not_the_template() -> None:
    assert tile_sources.needs_token("maptiler_satellite")
    assert tile_sources.needs_token("mapbox_streets")
    assert not tile_sources.needs_token("satellite")
    assert not tile_sources.needs_token("osm")
    assert not tile_sources.needs_token("not-a-source")


# ---------------------------------------------------------------------------
# URL building: a key is data, never structure
# ---------------------------------------------------------------------------

def test_the_key_is_substituted_into_the_upstream() -> None:
    url = tile_sources.build_tile_url(
        tile_sources.TILE_SOURCES["maptiler_satellite"]["upstream"], 10, 3, 4, "abc123")
    assert url == "https://api.maptiler.com/tiles/satellite-v2/10/3/4.jpg?key=abc123"


def test_a_key_cannot_add_url_structure() -> None:
    """The one substitution whose value did not come from the registry.

    A key carrying ``&``, ``#`` or a slash would otherwise append parameters,
    truncate the query, or move the request to another path on the host.
    """
    url = tile_sources.build_tile_url(
        tile_sources.TILE_SOURCES["mapbox_satellite"]["upstream"],
        1, 0, 0, "a&b=c#d/../e ")
    _base, _, query = url.partition("?")
    assert query == "access_token=a%26b%3Dc%23d%2F..%2Fe%20"
    assert query.count("=") == 1, "the key added a second parameter"
    assert "#" not in url and " " not in url


def test_an_unkeyed_template_is_untouched_by_a_token() -> None:
    template = tile_sources.TILE_SOURCES["satellite"]["upstream"]
    with_token = tile_sources.build_tile_url(template, 5, 1, 2, "secret")
    without = tile_sources.build_tile_url(template, 5, 1, 2)
    assert with_token == without
    assert "secret" not in with_token


def test_a_missing_key_does_not_leave_a_placeholder_in_the_url() -> None:
    """The fetch paths refuse first, but a URL with a literal ``{k}`` in it
    would be a request sent to the upstream with a brace in the query."""
    url = tile_sources.build_tile_url(
        tile_sources.TILE_SOURCES["maptiler_topo"]["upstream"], 1, 0, 0, "")
    assert "{k}" not in url and url.endswith("key=")


# ---------------------------------------------------------------------------
# Redaction: the log is not a place a key lives
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("source", ["maptiler_satellite", "mapbox_streets"])
def test_a_logged_url_carries_no_key(source: str) -> None:
    url = tile_sources.build_tile_url(
        tile_sources.TILE_SOURCES[source]["upstream"], 9, 5, 6, "pk.s3cret_value")
    safe = tile_sources.redact_url(url)
    assert "s3cret" not in safe
    assert "REDACTED" in safe
    # Everything else about the URL survives, or the line stops being useful
    # for the thing it is logged for.
    assert safe.startswith(url.split("?")[0])


def test_redaction_covers_every_parameter_a_key_travels_under() -> None:
    """Derived from the templates, so a service added later is covered too."""
    for entry in tile_sources.TILE_SOURCES.values():
        template = entry["upstream"]
        if "{k}" not in template:
            continue
        url = tile_sources.build_tile_url(template, 1, 0, 0, "UNIQUESECRET")
        assert "UNIQUESECRET" not in tile_sources.redact_url(url), template


def test_redaction_leaves_an_unkeyed_url_alone() -> None:
    url = tile_sources.build_tile_url(
        tile_sources.TILE_SOURCES["osm"]["upstream"], 3, 1, 2)
    assert tile_sources.redact_url(url) == url


# ---------------------------------------------------------------------------
# Storage: what a config file may hold, and what it hands back
# ---------------------------------------------------------------------------

def test_a_key_is_never_in_the_public_config() -> None:
    """The single redaction point, where the SSH passwords are also stopped."""
    cfg = _build_config({"map_tokens": {"maptiler": "s3cret"}})
    assert cfg.map_tokens == {"maptiler": "s3cret"}
    public = to_public_dict(cfg)
    assert "map_tokens" not in public
    assert "s3cret" not in json.dumps(public)


def test_a_key_round_trips_through_the_file(tmp_path) -> None:
    path = str(tmp_path / "config.json")
    save_config(_build_config({"map_tokens": {"mapbox": "pk.abc"}}), path)
    assert load_config(path).map_tokens == {"mapbox": "pk.abc"}


def test_the_config_file_holding_a_key_is_owner_only(tmp_path) -> None:
    import os
    import stat

    if os.name == "nt":
        pytest.skip("chmod does not carry permission bits on Windows")
    path = str(tmp_path / "config.json")
    save_config(_build_config({"map_tokens": {"mapbox": "pk.abc"}}), path)
    assert stat.S_IMODE(os.stat(path).st_mode) == 0o600


@pytest.mark.parametrize("value", [
    "has a space",
    '"quoted"',
    "key=abc",
    "abc\n",          # a paste that took the newline
    "ab&c",
    "../../etc",
    "a/b",
    "ünicode",
])
def test_a_value_that_cannot_be_a_key_is_dropped_whole(value: str) -> None:
    """Dropped, never trimmed into something that would then be sent upstream.

    ``abc\\n`` is the exception that proves the rule: the surrounding
    whitespace is stripped first, because that is a paste artefact rather than
    part of the value the operator meant.
    """
    cfg = _build_config({"map_tokens": {"maptiler": value}})
    stored = (cfg.map_tokens or {}).get("maptiler")
    assert stored in (None, value.strip()), value
    if stored is not None:
        assert set(stored) <= set(
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._~-")


def test_an_implausibly_long_value_is_dropped() -> None:
    cfg = _build_config({"map_tokens": {"maptiler": "a" * (MAX_MAP_TOKEN_CHARS + 1)}})
    assert cfg.map_tokens is None


def test_a_malformed_block_never_stops_the_config_loading() -> None:
    for raw in ("not-a-dict", 5, [], {"": "abc"}, {"maptiler": 5}, {"maptiler": ""}):
        assert _build_config({"map_tokens": raw}).map_tokens is None


def test_an_unknown_provider_id_survives_a_downgrade() -> None:
    """A build that does not serve that service must not delete its key."""
    cfg = _build_config({"map_tokens": {"some-future-service": "abc123"}})
    assert cfg.map_tokens == {"some-future-service": "abc123"}


# ---------------------------------------------------------------------------
# The endpoint
# ---------------------------------------------------------------------------

@pytest.fixture
def token_server(tmp_path):
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    from corvus.server import CorvusHandler, CorvusServer

    config_path = str(tmp_path / "config.json")
    cfg = CorvusConfig()
    saved = (
        CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
        CorvusHandler.config, CorvusHandler.config_path,
        CorvusHandler.tile_caches, CorvusHandler.tile_downloader,
    )
    CorvusHandler.store = None
    CorvusHandler.mavlink = None
    CorvusHandler.ssh = None
    CorvusHandler.config = cfg
    CorvusHandler.config_path = config_path
    CorvusHandler.tile_caches = {}
    CorvusHandler.tile_downloader = None

    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    thread = threading.Thread(
        target=server.serve_forever, name="corvus-map-tokens", daemon=True,
        kwargs={"poll_interval": 0.05},
    )
    thread.start()
    try:
        yield server, config_path
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        (
            CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
            CorvusHandler.config, CorvusHandler.config_path,
            CorvusHandler.tile_caches, CorvusHandler.tile_downloader,
        ) = saved


def _call(server, method: str, path: str, payload: dict | None = None):
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    if payload is None:
        conn.request(method, path)
    else:
        conn.request(method, path, json.dumps(payload),
                     {"Content-Type": "application/json"})
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, json.loads(body or b"{}")


def test_setting_a_key_stores_it_and_reports_only_that_it_is_set(token_server) -> None:
    server, config_path = token_server
    status, data = _call(server, "POST", "/api/tiles/token",
                         {"provider": "maptiler", "token": "s3cret_key.123"})
    assert status == 200
    assert data == {"ok": True, "provider": "maptiler", "token_set": True}

    assert load_config(config_path).map_tokens == {"maptiler": "s3cret_key.123"}

    _status, sources = _call(server, "GET", "/api/tiles/sources")
    by_id = {p["id"]: p for p in sources["providers"]}
    assert by_id["maptiler"]["token_set"] is True
    assert by_id["mapbox"]["token_set"] is False
    assert by_id["esri"]["token_required"] is False
    assert "s3cret" not in json.dumps(sources), "the listing must not carry the key"

    _status, config = _call(server, "GET", "/api/config")
    assert "s3cret" not in json.dumps(config)
    assert "map_tokens" not in config


def test_an_empty_token_clears_the_key(token_server) -> None:
    server, config_path = token_server
    _call(server, "POST", "/api/tiles/token", {"provider": "mapbox", "token": "pk.abc"})
    status, data = _call(server, "POST", "/api/tiles/token",
                         {"provider": "mapbox", "token": ""})
    assert status == 200
    assert data["token_set"] is False
    assert load_config(config_path).map_tokens is None


def test_clearing_a_key_that_was_never_set_is_a_success(token_server) -> None:
    """A stale dialog must not report an error for the state already wanted."""
    server, _ = token_server
    status, data = _call(server, "POST", "/api/tiles/token",
                         {"provider": "mapbox", "token": ""})
    assert status == 200 and data["ok"] is True


def test_one_service_key_does_not_disturb_another(token_server) -> None:
    server, config_path = token_server
    _call(server, "POST", "/api/tiles/token", {"provider": "maptiler", "token": "aaa"})
    _call(server, "POST", "/api/tiles/token", {"provider": "mapbox", "token": "bbb"})
    _call(server, "POST", "/api/tiles/token", {"provider": "maptiler", "token": ""})
    assert load_config(config_path).map_tokens == {"mapbox": "bbb"}


def test_a_key_survives_an_unrelated_settings_write(token_server) -> None:
    """The regression the merge in _apply_config_partial exists to stop."""
    server, config_path = token_server
    _call(server, "POST", "/api/tiles/token", {"provider": "maptiler", "token": "keepme"})
    status, _ = _call(server, "POST", "/api/config", {"theme": {"name": "green"}})
    assert status == 200
    cfg = load_config(config_path)
    assert cfg.map_tokens == {"maptiler": "keepme"}
    assert cfg.theme == {"name": "green"}


def test_post_api_config_cannot_set_a_key(token_server) -> None:
    """Credentials have one door. This is not it."""
    server, config_path = token_server
    status, _ = _call(server, "POST", "/api/config",
                      {"map_tokens": {"maptiler": "sneaked-in"}})
    assert status == 200          # the rest of the write still applies
    assert load_config(config_path).map_tokens is None


@pytest.mark.parametrize("payload, fragment", [
    ({"token": "abc"}, "provider"),
    ({"provider": "", "token": "abc"}, "provider"),
    ({"provider": "esri", "token": "abc"}, "keyed map service"),
    ({"provider": "nope", "token": "abc"}, "keyed map service"),
    ({"provider": "maptiler", "token": 5}, "string"),
    ({"provider": "maptiler", "token": 'has "quotes"'}, "does not look like"),
    ({"provider": "maptiler", "token": "a" * 600}, "longer than"),
])
def test_a_refused_key_is_never_stored(token_server, payload, fragment) -> None:
    server, config_path = token_server
    status, data = _call(server, "POST", "/api/tiles/token", payload)
    assert status == 400
    assert data["ok"] is False
    assert fragment in data["error"]
    assert load_config(config_path).map_tokens is None


def test_a_keyed_service_is_listed_even_with_no_key(token_server) -> None:
    """Reported, not hidden — the same rule the capability flags follow.

    A service that vanished from the picker gives the operator nothing to act
    on; one that says "needs an API key" tells them exactly what to do.
    """
    server, _ = token_server
    _status, sources = _call(server, "GET", "/api/tiles/sources")
    providers = {p["id"] for p in sources["providers"]}
    assert {"maptiler", "mapbox"} <= providers
    listed = {s["provider"] for s in sources["sources"]}
    assert {"maptiler", "mapbox"} <= listed


# ---------------------------------------------------------------------------
# The fetch paths: what happens to a keyed service with no key
# ---------------------------------------------------------------------------

@pytest.fixture
def fetch_server(tmp_path):
    """A live server with real caches and a recording upstream."""
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    from corvus.server import CorvusHandler, CorvusServer, _UpstreamBreaker
    from corvus.tile_cache import TileCache

    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    caches = {sid: TileCache(str(cache_dir / f"{sid}.mbtiles"))
              for sid in tile_sources.all_sources()}
    breaker = _UpstreamBreaker()
    config_path = str(tmp_path / "config.json")
    saved = (
        CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
        CorvusHandler.config, CorvusHandler.config_path,
        CorvusHandler.tile_caches, CorvusHandler.tile_downloader,
        CorvusHandler.tile_breaker,
    )
    CorvusHandler.store = None
    CorvusHandler.mavlink = None
    CorvusHandler.ssh = None
    CorvusHandler.config = CorvusConfig()
    CorvusHandler.config_path = config_path
    CorvusHandler.tile_caches = caches
    CorvusHandler.tile_downloader = None
    CorvusHandler.tile_breaker = breaker

    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    thread = threading.Thread(
        target=server.serve_forever, name="corvus-map-token-fetch", daemon=True,
        kwargs={"poll_interval": 0.05},
    )
    thread.start()
    try:
        yield server, breaker
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        for c in caches.values():
            c.close()
        (
            CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
            CorvusHandler.config, CorvusHandler.config_path,
            CorvusHandler.tile_caches, CorvusHandler.tile_downloader,
            CorvusHandler.tile_breaker,
        ) = saved


def _raw(server, path: str) -> tuple[int, bytes]:
    """A tile response is image bytes, not JSON."""
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    conn.request("GET", path)
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, body


def test_a_keyless_service_is_not_fetched_at_all(fetch_server, monkeypatch) -> None:
    """No key means nothing to ask for; the network is not touched."""
    server, _breaker = fetch_server
    calls: list[object] = []
    monkeypatch.setattr("urllib.request.urlopen",
                        lambda *a, **k: calls.append(a) or (_ for _ in ()).throw(
                            AssertionError("upstream must not be called")))
    status, _ = _raw(server, "/api/tiles/maptiler_satellite/5/1/2.png")
    assert status == 404
    assert calls == []


def test_a_keyless_service_does_not_trip_the_shared_breaker(fetch_server) -> None:
    """The regression this ordering exists to prevent.

    The upstream breaker is shared by every source. If a keyless service
    counted as a failure, three panned tiles over a service the operator merely
    clicked past would open the breaker and stop the cache-fill for the map
    they are actually looking at — for the whole cooldown, with no way to tell
    that from being offline.
    """
    server, breaker = fetch_server
    for _ in range(6):
        _raw(server, "/api/tiles/mapbox_satellite/5/1/2.png")
    assert breaker.allow(), "a service with no key must not open the breaker"


def test_the_key_reaches_the_upstream_request(fetch_server, monkeypatch) -> None:
    server, _breaker = fetch_server
    _call(server, "POST", "/api/tiles/token",
          {"provider": "maptiler", "token": "live_key.1"})

    seen: list[str] = []

    class _Resp:
        def read(self, *_a: object) -> bytes:
            return b"\x89PNG\r\n\x1a\n" + b"tile"

        def __enter__(self) -> _Resp:
            return self

        def __exit__(self, *_a: object) -> bool:
            return False

    def fake_urlopen(req, *a: object, **k: object) -> _Resp:
        seen.append(getattr(req, "full_url", req))
        return _Resp()

    monkeypatch.setattr("urllib.request.urlopen", fake_urlopen)
    status, body = _raw(server, "/api/tiles/maptiler_satellite/5/1/2.png")
    assert status == 200
    assert body.startswith(b"\x89PNG")
    assert seen == ["https://api.maptiler.com/tiles/satellite-v2/5/1/2.jpg?key=live_key.1"]


def test_a_download_of_a_keyless_service_is_refused_up_front(token_server) -> None:
    """Thousands of 401s, reported as a job that failed every tile, is not an
    answer the operator can do anything with.

    The recording downloader proves the refusal happens BEFORE the job is
    created, rather than a job being started and then failing.
    """
    from corvus.server import CorvusHandler

    server, _ = token_server
    started: list[tuple] = []

    class _Recording:
        def start(self, *a: object, **k: object) -> str:
            started.append((a, k))
            return "job-1"

        def status(self, _job_id: str) -> dict:
            return {"job_id": "job-1", "state": "running"}

        def list_jobs(self) -> list:
            return []

    saved = CorvusHandler.tile_downloader
    CorvusHandler.tile_downloader = _Recording()
    try:
        status, data = _call(server, "POST", "/api/tiles/download", {
            "source": "mapbox_satellite",
            "bounds": {"w": 11.6, "s": 48.0, "e": 11.7, "n": 48.1},
            "minzoom": 10, "maxzoom": 11,
        })
        assert status == 400
        assert "API key" in data["error"]
        assert started == [], "the job must not be created at all"

        # With a key, the same request is accepted and the key goes with it.
        _call(server, "POST", "/api/tiles/token",
              {"provider": "mapbox", "token": "pk.abc"})
        status, _data = _call(server, "POST", "/api/tiles/download", {
            "source": "mapbox_satellite",
            "bounds": {"w": 11.6, "s": 48.0, "e": 11.7, "n": 48.1},
            "minzoom": 10, "maxzoom": 11,
        })
        assert status == 200
        assert len(started) == 1
        assert started[0][1]["token"] == "pk.abc"
    finally:
        CorvusHandler.tile_downloader = saved


def test_a_download_job_snapshot_never_carries_the_key() -> None:
    """The snapshot is serialized straight to the browser."""
    from corvus.tile_downloader import new_job, TileDownloader

    job = new_job("id", "maptiler_satellite", 4, [], "https://x/{z}?key={k}",
                  None, "s3cret")
    assert job["_token"] == "s3cret"
    snapshot = TileDownloader._snapshot(None, job)
    assert "s3cret" not in json.dumps(snapshot)
    assert not any(k.startswith("_") for k in snapshot)
