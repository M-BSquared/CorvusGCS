"""Licensed tile APIs: Google's Map Tiles session and Bing's imagery metadata.

Hermetic: ``urllib.request.urlopen`` is replaced by a recorder, so nothing
leaves the process. What is pinned is what goes out (the request shape, the
key only where the API wants it) and what is trusted coming back (a session
value that cannot add URL structure, a Bing tile address only on a Microsoft
https host).
"""
from __future__ import annotations

import io
import json
import threading
import time
import urllib.error

import pytest

from corvus import tile_sessions, tile_sources


class _Resp:
    def __init__(self, body: bytes) -> None:
        self._body = io.BytesIO(body)

    def read(self, *a: object) -> bytes:
        return self._body.read(*a)

    def __enter__(self) -> _Resp:
        return self

    def __exit__(self, *_a: object) -> bool:
        return False


class _Upstream:
    """Answers by URL prefix and records every request."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, str, dict | None]] = []
        self.answers: dict[str, object] = {}
        self.lock = threading.Lock()

    def __call__(self, req, *a: object, **k: object) -> _Resp:
        url = getattr(req, "full_url", req)
        method = req.get_method() if hasattr(req, "get_method") else "GET"
        body = getattr(req, "data", None)
        with self.lock:
            self.calls.append((method, url, json.loads(body) if body else None))
        for prefix, answer in self.answers.items():
            if url.startswith(prefix):
                if isinstance(answer, Exception):
                    raise answer
                if callable(answer):
                    answer = answer()
                return _Resp(json.dumps(answer).encode())
        raise AssertionError(f"unexpected request {url}")


@pytest.fixture
def upstream(monkeypatch):
    tile_sessions.clear()
    fake = _Upstream()
    monkeypatch.setattr("urllib.request.urlopen", fake)
    yield fake
    tile_sessions.clear()


GOOGLE = "https://tile.googleapis.com/v1/createSession"
BING = "https://dev.virtualearth.net/REST/v1/Imagery/Metadata/"


def _google_ok(session: str = "sess-1", ttl: float = 14 * 86400) -> dict:
    return {"session": session, "expiry": str(int(time.time() + ttl)),
            "tileWidth": 256, "tileHeight": 256, "imageFormat": "jpeg"}


def _bing_ok(url: str = "https://ecn.{subdomain}.tiles.virtualearth.net/tiles/"
                        "a{quadkey}.jpeg?g=14237&mkt={culture}&n=z",
             subdomains: list | None = None) -> dict:
    return {"resourceSets": [{"resources": [{
        "imageUrl": url,
        "imageUrlSubdomains": ["t0", "t1", "t2", "t3"] if subdomains is None else subdomains,
    }]}]}


# ---------------------------------------------------------------------------
# Google Map Tiles API
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("sid, map_type, layers", [
    ("google_satellite", "satellite", None),
    ("google_streets", "roadmap", None),
    ("google_hybrid", "satellite", ["layerRoadmap"]),
    ("google_topo", "terrain", ["layerRoadmap"]),
])
def test_a_google_session_asks_for_the_right_layer(upstream, sid, map_type, layers) -> None:
    upstream.answers[GOOGLE] = _google_ok()
    template = tile_sessions.resolve(sid, "AIzaKEY")
    method, url, body = upstream.calls[0]
    assert method == "POST"
    assert url == GOOGLE + "?key=AIzaKEY"
    assert body["mapType"] == map_type
    assert body.get("layerTypes") == layers

    tile = tile_sources.build_tile_url(template, 12, 2200, 1400, "AIzaKEY")
    assert tile == ("https://tile.googleapis.com/v1/2dtiles/12/2200/1400"
                    "?session=sess-1&key=AIzaKEY")


def test_a_session_is_reused_until_it_runs_out(upstream) -> None:
    upstream.answers[GOOGLE] = _google_ok()
    first = tile_sessions.resolve("google_satellite", "k1")
    second = tile_sessions.resolve("google_satellite", "k1")
    assert first == second
    assert len(upstream.calls) == 1
    # A different layer, or a different key, is a different session.
    tile_sessions.resolve("google_streets", "k1")
    tile_sessions.resolve("google_satellite", "k2")
    assert len(upstream.calls) == 3


def test_invalidate_opens_a_new_session(upstream) -> None:
    upstream.answers[GOOGLE] = _google_ok()
    tile_sessions.resolve("google_satellite", "k1")
    tile_sessions.invalidate("google_satellite", "k1")
    tile_sessions.resolve("google_satellite", "k1")
    assert len(upstream.calls) == 2


def test_concurrent_tiles_open_one_session(upstream) -> None:
    """A first pan asks for a screenful of tiles at once."""
    gate = threading.Event()

    def slow() -> dict:
        gate.wait(2)
        return _google_ok()

    upstream.answers[GOOGLE] = slow
    results: list[str] = []
    threads = [threading.Thread(
        target=lambda: results.append(tile_sessions.resolve("google_satellite", "k")))
        for _ in range(8)]
    for t in threads:
        t.start()
    time.sleep(0.05)
    gate.set()
    for t in threads:
        t.join(3)
    assert len(results) == 8 and len(set(results)) == 1
    assert len(upstream.calls) == 1


def test_a_session_value_cannot_add_url_structure(upstream) -> None:
    upstream.answers[GOOGLE] = _google_ok(session="a&key=evil#{z}/..")
    template = tile_sessions.resolve("google_satellite", "k")
    url = tile_sources.build_tile_url(template, 1, 0, 0, "k")
    _base, _, query = url.partition("?")
    assert query == "session=a%26key%3Devil%23%7Bz%7D%2F..&key=k"


def test_an_expired_session_is_refused(upstream) -> None:
    upstream.answers[GOOGLE] = _google_ok(ttl=60)
    with pytest.raises(tile_sessions.TileSessionError) as err:
        tile_sessions.resolve("google_satellite", "k")
    assert err.value.offline is False


def test_a_refused_key_is_remembered_and_not_offline(upstream) -> None:
    upstream.answers[GOOGLE] = urllib.error.HTTPError(
        GOOGLE, 403, "Forbidden", {}, io.BytesIO(b"{}"))
    for _ in range(3):
        with pytest.raises(tile_sessions.TileSessionError) as err:
            tile_sessions.resolve("google_satellite", "bad")
        assert err.value.offline is False
        assert "403" in str(err.value)
        assert "bad" not in str(err.value)
    assert len(upstream.calls) == 1, "a refused key must not be retried per tile"


def test_no_network_is_reported_as_offline(upstream) -> None:
    upstream.answers[GOOGLE] = urllib.error.URLError("no route to host")
    with pytest.raises(tile_sessions.TileSessionError) as err:
        tile_sessions.resolve("google_satellite", "k")
    assert err.value.offline is True


def test_a_source_without_a_licensed_api_is_a_caller_bug(upstream) -> None:
    with pytest.raises(ValueError):
        tile_sessions.resolve("satellite", "k")
    assert upstream.calls == []


def test_a_google_tile_url_logs_without_key_or_session(upstream) -> None:
    upstream.answers[GOOGLE] = _google_ok(session="SESSIONSECRET")
    template = tile_sessions.resolve("google_satellite", "KEYSECRET")
    url = tile_sources.build_tile_url(template, 3, 1, 2, "KEYSECRET")
    safe = tile_sources.redact_url(url)
    assert "KEYSECRET" not in safe and "SESSIONSECRET" not in safe
    assert tile_sources.redact_url(GOOGLE + "?key=KEYSECRET").endswith("key=REDACTED")


# ---------------------------------------------------------------------------
# Bing Maps REST imagery metadata
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("sid, imagery_set", [
    ("bing_satellite", "Aerial"),
    ("bing_streets", "RoadOnDemand"),
    ("bing_hybrid", "AerialWithLabelsOnDemand"),
])
def test_bing_metadata_becomes_a_quadkey_template(upstream, sid, imagery_set) -> None:
    upstream.answers[BING] = _bing_ok()
    template = tile_sessions.resolve(sid, "BingKey")
    _method, url, _body = upstream.calls[0]
    assert url == f"{BING}{imagery_set}?output=json&uriScheme=https&key=BingKey"

    tile = tile_sources.build_tile_url(template, 3, 4, 5, "BingKey")
    shard = tile_sources.server_shard(4, 5)
    q = tile_sources.quadkey(3, 4, 5)
    assert tile == (f"https://ecn.t{shard}.tiles.virtualearth.net/tiles/"
                    f"a{q}.jpeg?g=14237&mkt=en-US&n=z")
    # The tile itself never carries the key: Bing bills the metadata call.
    assert "BingKey" not in tile


def test_bing_with_unusual_subdomains_uses_the_first(upstream) -> None:
    upstream.answers[BING] = _bing_ok(subdomains=["a", "b"])
    template = tile_sessions.resolve("bing_satellite", "k")
    assert template.startswith("https://ecn.a.tiles.virtualearth.net/")


@pytest.mark.parametrize("image_url", [
    "http://ecn.{subdomain}.tiles.virtualearth.net/tiles/a{quadkey}.jpeg",
    "https://evil.example/tiles/a{quadkey}.jpeg",
    "https://ecn.{subdomain}.tiles.virtualearth.net/tiles/a{quadkey}.jpeg?x={unknown}",
    "https://ecn.{subdomain}.tiles.virtualearth.net/tiles/static.jpeg",
])
def test_a_bing_tile_address_corvus_will_not_use_is_refused(upstream, image_url) -> None:
    upstream.answers[BING] = _bing_ok(url=image_url)
    with pytest.raises(tile_sessions.TileSessionError) as err:
        tile_sessions.resolve("bing_satellite", "k")
    assert err.value.offline is False


@pytest.mark.parametrize("answer", [
    {}, {"resourceSets": []}, {"resourceSets": [{"resources": [{}]}]},
])
def test_a_bing_answer_without_a_tile_address_is_refused(upstream, answer) -> None:
    upstream.answers[BING] = answer
    with pytest.raises(tile_sessions.TileSessionError):
        tile_sessions.resolve("bing_satellite", "k")
