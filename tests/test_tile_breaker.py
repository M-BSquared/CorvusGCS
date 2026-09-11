"""Offline behaviour of the interactive tile cache-fill.

The premise is a laptop in a field with no connection. Panning onto ground that
was not pre-downloaded produces a whole viewport of cache misses at once, and
before the breaker every one of them spent the full upstream timeout failing to
reach the network — so the map turned to treacle exactly when the operator was
looking for something.

These tests pin the breaker's contract:

* a burst of misses costs a handful of attempts, not one per tile;
* one success re-arms it, so walking back into coverage recovers by itself;
* it never turns a servable tile into a failure — a cached tile is served
  whether the breaker is open or shut.

Hermetic: urlopen is monkeypatched, no network, no sleeping on real timeouts.
"""
from __future__ import annotations

import time
import urllib.request

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus.server import (  # noqa: E402
    TILE_UPSTREAM_FAIL_THRESHOLD,
    TILE_UPSTREAM_TIMEOUT_S,
    CorvusHandler,
    _UpstreamBreaker,
)


# ---------------------------------------------------------------------------
# The breaker in isolation
# ---------------------------------------------------------------------------

def test_closed_breaker_allows_every_attempt() -> None:
    b = _UpstreamBreaker(threshold=3, cooldown=60.0)
    assert all(b.allow() for _ in range(20))
    assert not b.is_open()


def test_trips_after_the_threshold_of_consecutive_failures() -> None:
    b = _UpstreamBreaker(threshold=3, cooldown=60.0)
    for _ in range(2):
        b.record_failure()
    assert b.allow(), "must not trip before the threshold"
    b.record_failure()
    assert b.is_open()
    assert not b.allow(), "further attempts are suppressed"


def test_a_success_resets_the_failure_run() -> None:
    """Consecutive failures, not cumulative — an intermittent link must not
    slowly accumulate its way into a tripped breaker."""
    b = _UpstreamBreaker(threshold=3, cooldown=60.0)
    b.record_failure()
    b.record_failure()
    b.record_success()
    b.record_failure()
    b.record_failure()
    assert b.allow(), "the success cleared the earlier failures"


def test_success_closes_an_open_breaker() -> None:
    """Walking back into coverage recovers without a restart."""
    b = _UpstreamBreaker(threshold=1, cooldown=60.0)
    b.record_failure()
    assert b.is_open()
    b.record_success()
    assert not b.is_open()
    assert b.allow()


def test_cooldown_expiry_allows_exactly_one_probe() -> None:
    """After the window, one request goes through to test the water.

    Letting them all through would put a still-offline session straight back to
    paying the timeout on every tile, which is the behaviour being fixed. A
    short real cooldown is used so the re-arm is observable.
    """
    b = _UpstreamBreaker(threshold=1, cooldown=0.05)
    b.record_failure()
    assert not b.allow(), "suppressed during the cooldown"
    # 0.2 and not 0.06: the breaker compares time.monotonic(), whose
    # resolution on Windows is ~15.6 ms, so a 10 ms margin over the
    # cooldown can measure as not-yet-elapsed and the probe never opens.
    time.sleep(0.2)
    assert b.allow(), "first request after the cooldown is the probe"
    assert not b.allow(), "the probe re-arms the window until it reports back"


def test_a_failed_probe_leaves_the_breaker_suppressing() -> None:
    """Still offline after the probe: back to instant failures, not a stampede."""
    b = _UpstreamBreaker(threshold=1, cooldown=0.05)
    b.record_failure()
    # 0.2 and not 0.06: the breaker compares time.monotonic(), whose
    # resolution on Windows is ~15.6 ms, so a 10 ms margin over the
    # cooldown can measure as not-yet-elapsed and the probe never opens.
    time.sleep(0.2)
    assert b.allow()          # the probe
    b.record_failure()        # ...which failed
    assert b.is_open()
    assert not b.allow(), "a failed probe keeps the door shut"


# ---------------------------------------------------------------------------
# The fill path using it
# ---------------------------------------------------------------------------

class _Recorder:
    """urlopen stand-in: counts attempts and fails or succeeds on command."""

    def __init__(self, fail: bool = True) -> None:
        self.calls = 0
        self.fail = fail

    def __call__(self, req, timeout=None):
        self.calls += 1
        assert timeout == TILE_UPSTREAM_TIMEOUT_S, "fill path must use the short timeout"
        if self.fail:
            raise OSError("Network is unreachable")
        return _Resp(b"\x89PNG\r\n\x1a\n tile-bytes")


class _Resp:
    def __init__(self, data: bytes) -> None:
        self._data = data

    def __enter__(self) -> "_Resp":
        return self

    def __exit__(self, *exc) -> bool:
        return False

    def read(self, amt: int | None = None) -> bytes:
        # Mirrors http.client.HTTPResponse.read(amt): the caller reads a
        # bounded number of bytes, not the whole body unconditionally.
        return self._data if amt is None else self._data[:amt]


@pytest.fixture
def handler():
    h = object.__new__(CorvusHandler)
    h.tile_breaker = _UpstreamBreaker()
    return h


def test_offline_viewport_stops_hammering_the_network(handler, monkeypatch) -> None:
    """The headline behaviour: a viewport of misses makes a few attempts, not
    one per tile."""
    rec = _Recorder(fail=True)
    monkeypatch.setattr(urllib.request, "urlopen", rec)

    for i in range(40):
        assert handler._fetch_upstream_tile("satellite", 14, 8600 + i, 5750) is None

    assert rec.calls == TILE_UPSTREAM_FAIL_THRESHOLD, (
        f"expected the breaker to stop after {TILE_UPSTREAM_FAIL_THRESHOLD} "
        f"failures, but {rec.calls} network attempts were made"
    )
    assert handler.tile_breaker.is_open()


def test_a_reachable_upstream_is_never_throttled(handler, monkeypatch) -> None:
    """Online, the breaker must be invisible — every tile still fetches."""
    rec = _Recorder(fail=False)
    monkeypatch.setattr(urllib.request, "urlopen", rec)

    for i in range(25):
        assert handler._fetch_upstream_tile("satellite", 14, 8600 + i, 5750) is not None
    assert rec.calls == 25
    assert not handler.tile_breaker.is_open()


def test_recovery_after_the_network_returns(handler, monkeypatch) -> None:
    rec = _Recorder(fail=True)
    monkeypatch.setattr(urllib.request, "urlopen", rec)
    for i in range(10):
        handler._fetch_upstream_tile("satellite", 14, 8600 + i, 5750)
    assert handler.tile_breaker.is_open()

    # Coverage returns; the operator should not have to restart the app.
    handler.tile_breaker.reset()
    rec.fail = False
    assert handler._fetch_upstream_tile("satellite", 14, 1, 1) is not None
    assert not handler.tile_breaker.is_open()


def test_an_empty_200_counts_as_a_failure(handler, monkeypatch) -> None:
    """A reachable-but-broken upstream returning empty bodies must not hold the
    breaker closed forever."""
    def empty(req, timeout=None):
        return _Resp(b"")
    monkeypatch.setattr(urllib.request, "urlopen", empty)
    for _ in range(TILE_UPSTREAM_FAIL_THRESHOLD):
        assert handler._fetch_upstream_tile("satellite", 14, 1, 1) is None
    assert handler.tile_breaker.is_open()


def test_a_handler_without_a_breaker_still_works(handler, monkeypatch) -> None:
    """The breaker is optional wiring; its absence must not break the fill.

    Tests and older embeddings construct a handler without one.
    """
    rec = _Recorder(fail=False)
    monkeypatch.setattr(urllib.request, "urlopen", rec)
    handler.tile_breaker = None
    assert handler._fetch_upstream_tile("satellite", 14, 1, 1) is not None


def test_unknown_source_never_touches_the_network(handler, monkeypatch) -> None:
    rec = _Recorder(fail=True)
    monkeypatch.setattr(urllib.request, "urlopen", rec)
    assert handler._fetch_upstream_tile("not-a-source", 14, 1, 1) is None
    assert rec.calls == 0
    assert not handler.tile_breaker.is_open(), "a bad id must not count against the network"


def test_interactive_timeout_is_short_enough_to_be_interactive() -> None:
    """This is the browser waiting for a map tile. The downloader, where nobody
    is watching, keeps its own longer timeout."""
    from corvus.tile_downloader import _FETCH_TIMEOUT
    assert TILE_UPSTREAM_TIMEOUT_S <= 5
    assert _FETCH_TIMEOUT > TILE_UPSTREAM_TIMEOUT_S


def test_build_tile_resources_wires_a_breaker(tmp_path) -> None:
    """Both entry points get one from the shared helper — the packaged desktop
    app is the build that most needs it."""
    from corvus.server import _build_tile_resources
    caches, _bus, downloader, breaker = _build_tile_resources(str(tmp_path))
    try:
        assert isinstance(breaker, _UpstreamBreaker)
        assert not breaker.is_open()
    finally:
        downloader.shutdown()
        for c in caches.values():
            c.close()
