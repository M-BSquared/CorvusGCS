"""Shutdown-path tests for the extracted stop sequences in serve.py and app.py.

These exercise the REAL production stop functions (``serve._stop_all`` and
``corvus.app._stop_all``) — not a re-implementation — against a hermetic
backend built via ``corvus.server.create_server`` with the MAVLink
connection faked (no network) and tile caches redirected to ``tmp_path``.

They assert: the ordered stop sequence runs without raising, the MAVLink
thread is joined dead, SSH is shut down, ``store.shutdown()`` is invoked,
``server.shutdown()`` returns, the sequence is idempotent, and a failing
step cannot skip subsequent cleanup (each call is individually guarded).
``corvus.app`` imports cleanly without PyQt6 (its Qt imports live inside
``main()``), so the app stop sequence is unit-tested headlessly.
"""
from __future__ import annotations

import threading
from typing import Any
from unittest.mock import MagicMock

import pytest

import corvus.app as app
import corvus.server as srv
import serve
from corvus.mavlink_bridge import MavlinkBridge


# ---------------------------------------------------------------------------
# Hermetic create_server: fake MAVLink conn + tmp tile cache dir. Mirrors
# tests/test_regression_shutdown.py (kept self-contained so this file does
# not depend on another test module's fixtures).
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
    """A MagicMock conn so _connect() succeeds with no network.

    recv_match spins at a 20 ms cadence (via an Event, not time.sleep, so it
    is unaffected by a time.sleep monkeypatch) and returns None until stop()
    clears _running, so the receive loop wakes promptly and the join is fast.
    """
    conn = MagicMock()
    conn.source_system = 255
    conn.source_component = 190
    conn.wait_heartbeat = lambda blocking=True, timeout=10: _FakeHeartbeat()
    conn.mode_mapping = lambda: {}
    _block = threading.Event()

    def _recv_match(blocking: bool = True, timeout: float = 1) -> Any:
        _block.wait(0.02)
        return None

    conn.recv_match = _recv_match
    conn.close = lambda: None
    return conn


@pytest.fixture
def backend(tmp_path, monkeypatch):
    """A live create_server() backend: fake MAVLink, caches in tmp, http running."""
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")

    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    monkeypatch.setattr(srv, "default_cache_dir", lambda: str(cache_dir))
    monkeypatch.setattr(
        "corvus.mavlink_bridge.mavutil.mavlink_connection",
        lambda conn_str, timeout=2: _make_fake_conn(),
    )
    # Skip the slow ACK-confirmed stream setup so _connect() returns promptly
    # and the receive loop is running before stop().
    monkeypatch.setattr(MavlinkBridge, "_request_streams", lambda self: None)
    monkeypatch.setattr(MavlinkBridge, "_request_message_intervals", lambda self: None)
    monkeypatch.setattr(MavlinkBridge, "_request_version", lambda self: None)

    server = srv.create_server(port=0, mavlink_conn="udp:127.0.0.1:9999")
    http_thread = threading.Thread(
        target=server.serve_forever, name="corvus-path-http", daemon=True,
        kwargs={"poll_interval": 0.05},
    )
    http_thread.start()

    try:
        yield server, http_thread
    finally:
        # Defensive teardown if a test bailed before its own stop.
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


def _wrap_order(obj: Any, attr: str, order: list[str], label: str) -> None:
    """Replace obj.attr with a recorder that appends *label* then calls through."""
    original = getattr(obj, attr)

    def _wrapped(*args: Any, **kwargs: Any) -> Any:
        order.append(label)
        return original(*args, **kwargs)

    setattr(obj, attr, _wrapped)


# ---------------------------------------------------------------------------
# 1. serve._stop_all: full ordered clean teardown
# ---------------------------------------------------------------------------

def test_serve_stop_all_runs_full_ordered_sequence(backend) -> None:
    server, http_thread = backend
    order: list[str] = []
    _wrap_order(server.mavlink, "stop", order, "mavlink")
    _wrap_order(server.ssh, "shutdown", order, "ssh")
    _wrap_order(server.store, "shutdown", order, "store")
    _wrap_order(server, "shutdown", order, "server")

    # Must not raise.
    serve._stop_all(server)

    # The exact order the shutdown path must follow.
    assert order == ["mavlink", "ssh", "store", "server"]
    # store.shutdown() was invoked (recorded in order).
    assert "store" in order
    # MAVLink thread joined dead.
    assert server.mavlink._thread is not None
    assert not server.mavlink._thread.is_alive(), "mavlink thread survived stop()"
    # GCS heartbeat thread also torn down.
    if server.mavlink._hb_thread is not None:
        assert not server.mavlink._hb_thread.is_alive()
    # SSH sessions cleared by shutdown().
    assert server.ssh.list_sessions() == []
    # server.shutdown() returned (http serve loop stopped); thread finishes.
    http_thread.join(timeout=2)
    assert not http_thread.is_alive(), "http thread survived server.shutdown()"


# ---------------------------------------------------------------------------
# 2. corvus.app._stop_all: full ordered clean teardown (headless, no Qt)
# ---------------------------------------------------------------------------

def test_app_stop_all_runs_full_ordered_sequence(backend) -> None:
    server, http_thread = backend
    order: list[str] = []
    _wrap_order(server.mavlink, "stop", order, "mavlink")
    _wrap_order(server.ssh, "shutdown", order, "ssh")
    _wrap_order(server.store, "shutdown", order, "store")
    _wrap_order(server, "shutdown", order, "server")

    # Must not raise.
    app._stop_all(server)

    assert order == ["mavlink", "ssh", "store", "server"]
    assert not server.mavlink._thread.is_alive(), "mavlink thread survived stop()"
    assert server.ssh.list_sessions() == []
    http_thread.join(timeout=2)
    assert not http_thread.is_alive(), "http thread survived server.shutdown()"


# ---------------------------------------------------------------------------
# 3. Idempotency: the stop sequence is safe to call twice
# ---------------------------------------------------------------------------

def test_serve_stop_all_is_idempotent(backend) -> None:
    server, http_thread = backend

    serve._stop_all(server)           # first pass must not raise
    http_thread.join(timeout=2)
    # Second pass must not raise even though everything is already torn down.
    serve._stop_all(server)
    # A third pass via app._stop_all is also safe (same underlying idempotent
    # methods: socketserver.shutdown, TileCache.close, MavlinkBridge.stop).
    app._stop_all(server)

    assert not server.mavlink._thread.is_alive()


# ---------------------------------------------------------------------------
# 4. Exception safety: a failing step cannot skip subsequent cleanup
#    (serve._stop_all)
# ---------------------------------------------------------------------------

def test_serve_stop_all_continues_when_a_step_raises(backend) -> None:
    server, http_thread = backend
    calls: list[str] = []
    original_stop = server.mavlink.stop

    def boom(*args: Any, **kwargs: Any) -> None:
        calls.append("mavlink")
        raise RuntimeError("boom")

    server.mavlink.stop = boom
    _wrap_order(server.ssh, "shutdown", calls, "ssh")
    _wrap_order(server.store, "shutdown", calls, "store")
    _wrap_order(server, "shutdown", calls, "server")

    try:
        # Must not raise despite mavlink.stop() blowing up.
        serve._stop_all(server)
        # ssh/store/server still ran in order after the failing mavlink step.
        assert calls == ["mavlink", "ssh", "store", "server"]
    finally:
        # Re-join the real mavlink thread so the test leaves no spinning thread.
        server.mavlink.stop = original_stop
        try:
            original_stop()
        except Exception:
            pass

    http_thread.join(timeout=2)
    assert not server.mavlink._thread.is_alive()


# ---------------------------------------------------------------------------
# 5. Exception safety (corvus.app._stop_all)
# ---------------------------------------------------------------------------

def test_app_stop_all_continues_when_a_step_raises(backend) -> None:
    server, http_thread = backend
    calls: list[str] = []
    original_stop = server.mavlink.stop

    def boom(*args: Any, **kwargs: Any) -> None:
        calls.append("mavlink")
        raise RuntimeError("boom")

    server.mavlink.stop = boom
    _wrap_order(server.ssh, "shutdown", calls, "ssh")
    _wrap_order(server.store, "shutdown", calls, "store")
    _wrap_order(server, "shutdown", calls, "server")

    try:
        app._stop_all(server)  # must not raise
        assert calls == ["mavlink", "ssh", "store", "server"]
    finally:
        server.mavlink.stop = original_stop
        try:
            original_stop()
        except Exception:
            pass

    http_thread.join(timeout=2)
    assert not server.mavlink._thread.is_alive()
