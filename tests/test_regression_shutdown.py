"""Shutdown cleanliness regression tests for the full backend stop sequence.

Builds a real backend via ``corvus.server.create_server`` on an ephemeral
port with the MAVLink connection faked (no real serial/UDP/SITL), then runs
the exact stop sequence ``serve.py`` drives — ``mavlink.stop()`` ->
``ssh.shutdown()`` -> ``store.shutdown()`` -> ``server.shutdown()`` — plus
``server.server_close()`` to actually close the listening socket (the
production path reclaims it via ``os._exit``; a pytest run cannot, so the
explicit close is the hermetic equivalent).

Asserts: no exception, the MAVLink thread is joined dead, the
``ThreadingHTTPServer`` socket is closed, the sequence is idempotent, and
every tile resource (per-source downloader + caches) is released.
"""
from __future__ import annotations

import http.client
import threading
from typing import Any
from unittest.mock import MagicMock

import pytest

import corvus.server as srv
from corvus.mavlink_bridge import MavlinkBridge
from corvus.tile_cache import TileCache
from corvus.tile_sources import TILE_SOURCES


# ---------------------------------------------------------------------------
# Hermetic create_server: fake the MAVLink conn + redirect the tile cache
# dir to tmp_path so nothing is written under the user's home.
# ---------------------------------------------------------------------------

class _FakeHeartbeat:
    """Minimal heartbeat object returned by the faked wait_heartbeat()."""

    type = 2          # MAV_TYPE_QUADROTOR
    autopilot = 12    # MAV_AUTOPILOT_PX4
    base_mode = 0
    custom_mode = 3 << 16  # POSCTL

    def get_srcSystem(self) -> int:
        return 1

    def get_srcComponent(self) -> int:
        return 1


def _make_fake_conn() -> MagicMock:
    """A MagicMock conn that satisfies _connect() without any network.

    recv_match blocks briefly (via an Event, not time.sleep, so it is
    unaffected by a time.sleep monkeypatch) and returns None so the receive
    loop spins at a low CPU cadence until stop() clears _running.
    """
    conn = MagicMock()
    conn.source_system = 255
    conn.source_component = 190
    conn.wait_heartbeat = lambda blocking=True, timeout=10: _FakeHeartbeat()
    conn.mode_mapping = lambda: {}
    _block = threading.Event()

    def _recv_match(blocking: bool = True, timeout: float = 1) -> Any:
        # 20 ms cadence; wakes promptly when stop() clears _running.
        _block.wait(0.02)
        return None

    conn.recv_match = _recv_match
    conn.closed = False

    def _close() -> None:
        conn.closed = True

    conn.close = _close
    return conn


@pytest.fixture
def backend_server(tmp_path, monkeypatch):
    """A live create_server() backend with MAVLink faked and caches in tmp."""
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")

    # Redirect the tile cache dir away from ~/.corvus/tiles.
    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    monkeypatch.setattr(srv, "default_cache_dir", lambda: str(cache_dir))

    # Fake the MAVLink connection: no real socket, no real heartbeat wait.
    monkeypatch.setattr(
        "corvus.mavlink_bridge.mavutil.mavlink_connection",
        lambda conn_str, timeout=2: _make_fake_conn(),
    )
    # Skip the slow per-message ACK-confirmed stream setup so _connect()
    # returns promptly and the receive loop is running before stop().
    monkeypatch.setattr(MavlinkBridge, "_request_streams", lambda self: None)
    monkeypatch.setattr(MavlinkBridge, "_request_message_intervals", lambda self: None)
    monkeypatch.setattr(MavlinkBridge, "_request_version", lambda self: None)

    server = srv.create_server(port=0, mavlink_conn="udp:127.0.0.1:9999")
    http_thread = threading.Thread(
        target=server.serve_forever, name="corvus-reg-http", daemon=True,
        kwargs={"poll_interval": 0.05},
    )
    http_thread.start()

    # Capture the tile resources before any stop so we can assert on them
    # after the server has cleared its own references.
    caches_snapshot = dict(server.tile_caches) if hasattr(server, "tile_caches") else {}
    downloader = getattr(server, "tile_downloader", None)
    per_source: dict[str, Any] = {}
    if downloader is not None and hasattr(downloader, "_downloaders"):
        per_source = dict(downloader._downloaders)

    try:
        yield server, caches_snapshot, downloader, per_source, http_thread
    finally:
        # Defensive: if a test bailed before its own stop, tear down here.
        try:
            server.mavlink.stop()
        except Exception:
            pass
        try:
            server.shutdown()
        except Exception:
            pass
        try:
            server.server_close()
        except Exception:
            pass
        http_thread.join(timeout=2)
        for c in caches_snapshot.values():
            try:
                c.close()
            except Exception:
                pass


def _full_stop(server) -> None:
    """The exact stop sequence serve.py drives, plus server_close()."""
    server.mavlink.stop()
    server.ssh.shutdown()
    server.store.shutdown()
    server.shutdown()
    # serve.py reclaims the listening socket via os._exit(0); in-process we
    # must close it explicitly to actually release the fd.
    server.server_close()


# ---------------------------------------------------------------------------
# 1. Full stop sequence: no exception, mavlink thread joined, socket closed
# ---------------------------------------------------------------------------

def test_full_stop_sequence_is_clean(backend_server) -> None:
    server, caches, downloader, per_source, http_thread = backend_server

    # The MAVLink thread is alive once create_server() has started it.
    assert server.mavlink._thread is not None
    assert server.mavlink._thread.is_alive()
    # The HTTP server is listening (socket open, non-negative fileno).
    assert server.socket.fileno() >= 0

    # Smoke-check the server actually served before we tear it down.
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    try:
        conn.request("GET", "/api/version")
        resp = conn.getresponse()
        assert resp.status == 200
        resp.read()
    finally:
        conn.close()

    _full_stop(server)  # must not raise
    http_thread.join(timeout=2)

    # MAVLink thread is no longer alive after stop().
    assert server.mavlink._thread is not None  # the object remains
    assert not server.mavlink._thread.is_alive(), "mavlink thread survived stop()"
    # GCS heartbeat thread also torn down.
    if server.mavlink._hb_thread is not None:
        assert not server.mavlink._hb_thread.is_alive(), "gcs-hb thread survived stop()"
    # The ThreadingHTTPServer listening socket is closed.
    assert server.socket.fileno() == -1, "server socket not closed"


# ---------------------------------------------------------------------------
# 2. Idempotency: the stop sequence is safe to call twice
# ---------------------------------------------------------------------------

def test_stop_sequence_is_idempotent(backend_server) -> None:
    server, caches, downloader, per_source, http_thread = backend_server

    _full_stop(server)
    http_thread.join(timeout=2)
    # Second pass must not raise even though everything is already torn down.
    _full_stop(server)
    # A third pass via the individual calls is also safe.
    server.mavlink.stop()
    server.ssh.shutdown()
    server.store.shutdown()
    server.shutdown()
    server.server_close()
    assert not server.mavlink._thread.is_alive()
    assert server.socket.fileno() == -1


# ---------------------------------------------------------------------------
# 3. Tile resources: downloader shut down, caches closed
# ---------------------------------------------------------------------------

def test_stop_sequence_releases_tile_resources(backend_server) -> None:
    server, caches, downloader, per_source, http_thread = backend_server

    # Preconditions: the prior wave wired the tile resources onto the server.
    assert caches, "server.tile_caches should hold one cache per source"
    assert set(caches) == set(TILE_SOURCES)
    if downloader is not None:
        assert per_source, "downloader pool should hold one TileDownloader per source"
        for sid, dl in per_source.items():
            assert dl._shutdown is False, f"{sid} downloader should be running pre-stop"

    _full_stop(server)
    http_thread.join(timeout=2)

    # The server released its references (shut down + cleared).
    assert getattr(server, "tile_downloader", None) is None, (
        "server.tile_downloader should be cleared after shutdown"
    )
    assert getattr(server, "tile_caches", {}) == {}, (
        "server.tile_caches should be cleared after shutdown"
    )

    # Every per-source cache was actually closed (SQLite handle released).
    for sid, cache in caches.items():
        assert isinstance(cache, TileCache)
        assert cache._closed is True, f"{sid} cache not closed after shutdown"

    # Every per-source downloader's shutdown flag flipped by the pool teardown.
    for sid, dl in per_source.items():
        assert dl._shutdown is True, f"{sid} downloader not shut down"
