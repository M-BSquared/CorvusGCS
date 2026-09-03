"""PX4 NuttShell (NSH) debug-shell channel — unit tests + shutdown audit.

Covers the SERIAL_CONTROL device=SHELL mechanism added to
``corvus/mavlink_bridge.py`` (``send_shell_command`` / ``stop_shell`` /
``_handle_serial_control`` / ``_pull_next_shell_chunk``) and the
``/api/console/command`` shell dispatch added to ``corvus/server.py``.

The bridge-level tests run hermetically against the real ``MavlinkBridge``
with a fake ``conn`` whose ``mav.serial_control_send`` records every call
(mirroring the ``FakeConnection``/``FakeMav`` harness in
``tests/test_mavlink_takeoff.py``). The server-dispatch tests mirror
``tests/test_server_takeoff.py`` (a ``FakeBridge`` + ``object.__new__`` of
``CorvusHandler`` with a stubbed ``_send_json``).

A shutdown audit (AGENTS.md lifecycle rule) confirms ``stop()`` releases the
exclusive NSH shell via ``stop_shell()`` both at the bridge level and through
the full ``create_server`` backend stop sequence (mirroring
``tests/test_regression_shutdown.py``), and that no shell state leaks.
"""
from __future__ import annotations

import threading
import time
from types import SimpleNamespace
from typing import Any, Callable
from unittest.mock import MagicMock

import pytest
from pymavlink import mavutil

from corvus.mavlink_bridge import MavlinkBridge, SHELL_MAX_CHUNKS
from corvus.state_store import VehicleStateStore
from corvus.server import CorvusHandler


# ---------------------------------------------------------------------------
# pymavlink SERIAL_CONTROL constants (verified at import time):
#   SERIAL_CONTROL_DEV_SHELL = 10
#   SERIAL_CONTROL_FLAG_REPLY = 1, FLAG_EXCLUSIVE = 4  -> REPLY|EXCLUSIVE = 5
# ---------------------------------------------------------------------------
DEV_SHELL = mavutil.mavlink.SERIAL_CONTROL_DEV_SHELL
FLAG_REPLY_EXCLUSIVE = (
    mavutil.mavlink.SERIAL_CONTROL_FLAG_REPLY
    | mavutil.mavlink.SERIAL_CONTROL_FLAG_EXCLUSIVE
)
assert DEV_SHELL == 10, "pymavlink SERIAL_CONTROL_DEV_SHELL must be 10"
assert FLAG_REPLY_EXCLUSIVE == 5, "REPLY|EXCLUSIVE must be 5"


# ---------------------------------------------------------------------------
# Fake conn / message harness — mirrors tests/test_mavlink_takeoff.py.
# ---------------------------------------------------------------------------

class SerialControlMsg(SimpleNamespace):
    """A SERIAL_CONTROL message with the fields _handle_serial_control reads."""

    def get_type(self) -> str:
        return "SERIAL_CONTROL"


def shell_reply(
    payload: bytes, device: int = DEV_SHELL, count: int | None = None,
) -> SerialControlMsg:
    """Build a SERIAL_CONTROL reply chunk.

    PX4 sends a fixed 70-byte ``data`` array with ``count`` set to the real
    payload length; the handler slices ``data[:count]``. We mirror that so
    the slicing path is exercised, not bypassed.
    """
    data = payload.ljust(70, b"\x00")
    return SerialControlMsg(
        device=device,
        count=len(payload) if count is None else count,
        data=data,
    )


class _FakeShellMav:
    """Records every serial_control_send(device, flags, timeout, baud, count, data)."""

    def __init__(self) -> None:
        self.serial_sends: list[tuple] = []

    def serial_control_send(self, *args: Any) -> None:
        self.serial_sends.append(args)


class FakeShellConn:
    """Minimal conn: a recording mav + the attrs _pull/stop touch."""

    def __init__(self) -> None:
        self.mav = _FakeShellMav()
        self.source_system = 255
        self.source_component = 190
        self.closed = False

    def close(self) -> None:
        self.closed = True


def ready_bridge() -> MavlinkBridge:
    """A connected bridge: fresh heartbeat (connected + not stale), no thread.

    Mirrors ``ready_bridge`` in tests/test_mavlink_takeoff.py. The caller
    attaches ``bridge._conn`` afterwards.
    """
    store = VehicleStateStore()
    store.heartbeat()  # sets connected=True and a fresh _last_heartbeat
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    return bridge


def _subscribe(bridge: MavlinkBridge) -> list[dict[str, Any]]:
    """Append a console subscriber that records every published entry."""
    records: list[dict[str, Any]] = []
    bridge.add_console_sub(lambda entry: records.append(entry))
    return records


# ---------------------------------------------------------------------------
# 1. send_shell_command — SERIAL_CONTROL framing
# ---------------------------------------------------------------------------

def test_send_shell_command_sends_serial_control_with_correct_fields() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()

    text = "listener sensor_combined"
    ok = bridge.send_shell_command(text)

    assert ok is True
    sends = bridge._conn.mav.serial_sends
    assert len(sends) == 1
    device, flags, timeout, baudrate, count, data = sends[0]
    assert device == DEV_SHELL
    assert flags == FLAG_REPLY_EXCLUSIVE
    assert flags == 5
    assert timeout == 0
    assert baudrate == 0
    payload = (text + "\n").encode("utf-8")
    assert count == len(payload)
    assert data == payload.ljust(70, b"\x00")
    assert len(data) == 70
    # Per-command chunk counter reset so this response can stream again.
    assert bridge._shell_chunks_pulled == 0


def test_send_shell_command_resets_chunk_counter_each_call() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    bridge._shell_chunks_pulled = 42  # pretend a prior command streamed chunks
    assert bridge.send_shell_command("top")
    assert bridge._shell_chunks_pulled == 0


def test_send_shell_command_returns_false_and_sets_error_when_disconnected() -> None:
    # Real disconnected state: no conn, no heartbeat -> _connection_ready() False.
    bridge = MavlinkBridge(VehicleStateStore())
    # A fresh bridge's threading.local error defaults to "".
    assert bridge.get_last_command_error() == ""

    ok = bridge.send_shell_command("listener sensor_combined")

    assert ok is False
    err = bridge.get_last_command_error()
    assert "DISCONNECTED" in err, f"expected a DISCONNECTED error, got {err!r}"
    assert err == "Shell failed: DISCONNECTED"  # pin exact string for documentation
    assert bridge._shell_chunks_pulled == 0  # never reached the reset line


# ---------------------------------------------------------------------------
# 2. _dispatch routing + _handle_serial_control reassembly
# ---------------------------------------------------------------------------

def test_dispatch_routes_serial_control_to_handler() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    records = _subscribe(bridge)

    bridge._dispatch(shell_reply(b"hello\n"))

    shell_lines = [r["text"] for r in records if r["name"] == "SHELL"]
    assert shell_lines == ["hello"]


def test_handle_serial_control_reassembles_two_chunks_into_one_line() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    records = _subscribe(bridge)

    # Two 70-byte chunks forming one logical line "listener: sensor_combined".
    bridge._handle_serial_control(shell_reply(b"listener: sensor_"))
    assert [r for r in records if r["name"] == "SHELL"] == []  # no newline yet
    bridge._handle_serial_control(shell_reply(b"combined\n"))

    published = [r for r in records if r["name"] == "SHELL"]
    assert len(published) == 1
    assert published[0]["text"] == "listener: sensor_combined"
    assert published[0]["level"] == "info"
    # Trailing partial line is gone (the buffer ended on the newline).
    assert bridge._shell_buffer == ""


def test_handle_serial_control_strips_cr_and_applies_backspace() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    records = _subscribe(bridge)

    # 'a' -> backspace removes it -> 'b' -> CR stripped -> 'c' -> CR stripped
    # -> '\n' ends the line. Expected published line: "bc".
    bridge._handle_serial_control(shell_reply(b"a\bb\rc\r\n"))

    shell_lines = [r["text"] for r in records if r["name"] == "SHELL"]
    assert shell_lines == ["bc"]


def test_handle_serial_control_backspace_crosses_chunk_boundary() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    records = _subscribe(bridge)

    # Chunk 1 leaves "ab" in the buffer; chunk 2's leading \b must remove 'b'
    # from the buffer tail (not just from an empty in-chunk chunk list).
    bridge._handle_serial_control(shell_reply(b"ab"))
    bridge._handle_serial_control(shell_reply(b"\bc\n"))

    shell_lines = [r["text"] for r in records if r["name"] == "SHELL"]
    assert shell_lines == ["ac"]


def test_handle_serial_control_ignores_non_shell_device() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    records = _subscribe(bridge)

    # device=0 (e.g. a GPS serial passthrough) must not be treated as NSH shell.
    bridge._handle_serial_control(shell_reply(b"hello\n", device=0))

    assert records == []
    assert bridge._conn.mav.serial_sends == []  # no pull
    assert bridge._shell_buffer == ""            # untouched


def test_handle_serial_control_never_raises_on_bad_message() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    records = _subscribe(bridge)

    # A malformed message (data is not bytes) must be swallowed, not raised.
    bad = SerialControlMsg(device=DEV_SHELL, count=5, data="not-bytes")
    bridge._handle_serial_control(bad)  # must not raise

    assert [r for r in records if r["name"] == "SHELL"] == []


# ---------------------------------------------------------------------------
# 3. Event-driven chunk pulling
# ---------------------------------------------------------------------------

def test_handle_serial_control_count_zero_does_not_pull_next_chunk() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    records = _subscribe(bridge)

    bridge._handle_serial_control(shell_reply(b""))  # count == 0

    assert records == []
    assert bridge._conn.mav.serial_sends == []  # end-of-response: no pull
    assert bridge._shell_chunks_pulled == 0


def test_handle_serial_control_count_positive_pulls_next_chunk() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    records = _subscribe(bridge)

    # No newline in the payload -> nothing published, but count>0 pulls.
    bridge._handle_serial_control(shell_reply(b"abc"))

    assert [r for r in records if r["name"] == "SHELL"] == []
    sends = bridge._conn.mav.serial_sends
    assert len(sends) == 1
    device, flags, timeout, baudrate, count, data = sends[0]
    assert device == DEV_SHELL
    assert flags == FLAG_REPLY_EXCLUSIVE
    assert timeout == 0
    assert baudrate == 0
    assert count == 0
    assert data == b"\x00" * 70
    assert bridge._shell_chunks_pulled == 1


def test_pull_next_shell_chunk_caps_at_shell_max_chunks() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()

    # Already at the cap -> no send, counter unchanged.
    bridge._shell_chunks_pulled = SHELL_MAX_CHUNKS
    bridge._pull_next_shell_chunk()
    assert bridge._conn.mav.serial_sends == []
    assert bridge._shell_chunks_pulled == SHELL_MAX_CHUNKS

    # One below the cap still pulls once and lands exactly on the cap.
    bridge._shell_chunks_pulled = SHELL_MAX_CHUNKS - 1
    bridge._pull_next_shell_chunk()
    assert len(bridge._conn.mav.serial_sends) == 1
    assert bridge._shell_chunks_pulled == SHELL_MAX_CHUNKS

    # One more attempt at the cap is a no-op.
    bridge._pull_next_shell_chunk()
    assert len(bridge._conn.mav.serial_sends) == 1


def test_shell_chunk_cap_via_feeding_replies() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()

    # Feed SHELL_MAX_CHUNKS + 3 replies; pulls must cap at SHELL_MAX_CHUNKS.
    for _ in range(SHELL_MAX_CHUNKS + 3):
        bridge._handle_serial_control(shell_reply(b"x"))

    assert len(bridge._conn.mav.serial_sends) == SHELL_MAX_CHUNKS
    assert bridge._shell_chunks_pulled == SHELL_MAX_CHUNKS


def test_send_shell_command_reset_allows_full_stream_after_prior_cap() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    # A prior command exhausted the cap.
    bridge._shell_chunks_pulled = SHELL_MAX_CHUNKS
    bridge._handle_serial_control(shell_reply(b"x"))
    assert bridge._conn.mav.serial_sends == []  # capped, no pull

    # A fresh send_shell_command resets the counter and pulls resume. The
    # command itself emits one SERIAL_CONTROL (count>0); clear it so the
    # assertion counts only the chunk pulls that follow the reply.
    assert bridge.send_shell_command("top")
    bridge._conn.mav.serial_sends.clear()
    bridge._handle_serial_control(shell_reply(b"out\n"))
    assert len(bridge._conn.mav.serial_sends) == 1  # pull works again


# ---------------------------------------------------------------------------
# 4. stop_shell + shutdown audit (AGENTS.md lifecycle rule)
# ---------------------------------------------------------------------------

def test_stop_shell_clears_buffer_and_sends_release() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    bridge._shell_buffer = "partial line"

    bridge.stop_shell()

    assert bridge._shell_buffer == ""
    sends = bridge._conn.mav.serial_sends
    assert len(sends) == 1
    device, flags, timeout, baudrate, count, data = sends[0]
    assert device == DEV_SHELL
    assert flags == 0
    assert timeout == 0
    assert baudrate == 0
    assert count == 0
    assert data == b"\x00" * 70


def test_stop_shell_is_noop_and_safe_when_disconnected() -> None:
    bridge = MavlinkBridge(VehicleStateStore())  # no conn
    bridge._shell_buffer = "partial"

    # Must not raise and must not attempt a send (there is no conn).
    bridge.stop_shell()

    assert bridge._shell_buffer == ""  # buffer cleared regardless


def test_stop_shell_is_idempotent() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()

    bridge.stop_shell()
    bridge.stop_shell()  # second call must not raise

    assert bridge._shell_buffer == ""
    # Each call sends a release SERIAL_CONTROL; both are flags=0,count=0
    # (PX4 tolerates duplicate releases).
    assert len(bridge._conn.mav.serial_sends) == 2
    for send in bridge._conn.mav.serial_sends:
        device, flags, _timeout, _baud, count, _data = send
        assert device == DEV_SHELL
        assert flags == 0
        assert count == 0


def test_stop_invokes_stop_shell_release() -> None:
    """Shutdown audit: bridge.stop() must release the exclusive NSH shell."""
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    bridge._shell_buffer = "leftover"

    # Capture the conn before stop(): BUG 3 clears self._conn to None so the
    # in-flight command guard bails, so read the recorded sends off the
    # original object instead of bridge._conn.
    conn = bridge._conn
    bridge.stop()  # must not raise

    # stop() called stop_shell() -> exactly one release SERIAL_CONTROL.
    sends = conn.mav.serial_sends
    assert len(sends) == 1
    device, flags, _timeout, _baud, count, data = sends[0]
    assert device == DEV_SHELL
    assert flags == 0
    assert count == 0
    assert data == b"\x00" * 70
    assert bridge._shell_buffer == ""
    assert bridge._conn is None  # BUG 3: stop() clears _conn after closing
    # No thread was started, so stop() had nothing to join — no leak.
    assert bridge._thread is None


def test_stop_does_not_leak_shell_state() -> None:
    """After stop(), the shell line buffer is clear and the conn is closed."""
    bridge = ready_bridge()
    bridge._conn = FakeShellConn()
    bridge._shell_buffer = "junk"
    bridge._shell_chunks_pulled = 50

    # Capture the conn before stop(): BUG 3 clears self._conn to None.
    conn = bridge._conn
    bridge.stop()

    assert bridge._shell_buffer == ""
    assert conn.closed is True
    assert bridge._conn is None  # BUG 3: stop() clears _conn after closing
    assert bridge._thread is None
    assert bridge._hb_thread is None


# ---------------------------------------------------------------------------
# 5. Full backend stop releases the shell (mirrors test_regression_shutdown.py)
# ---------------------------------------------------------------------------

class _FakeHeartbeat:
    type = 2          # MAV_TYPE_QUADROTOR
    autopilot = 12    # MAV_AUTOPILOT_PX4
    base_mode = 0
    custom_mode = 3 << 16  # POSCTL

    def get_srcSystem(self) -> int:
        return 1

    def get_srcComponent(self) -> int:
        return 1


def test_full_backend_stop_releases_shell(tmp_path, monkeypatch) -> None:
    """Shutdown audit: the real stop sequence sends the NSH release and
    leaves no live MAVLink thread. Mirrors tests/test_regression_shutdown.py's
    create_server + faked mavlink_connection harness.
    """
    pytest.importorskip("paramiko")
    import corvus.server as srv

    # Keep tlogs out of ~/.corvus/logs so the test stays hermetic.
    monkeypatch.setattr(
        "corvus.mavlink_bridge.default_log_dir", lambda: str(tmp_path / "logs"),
    )
    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    monkeypatch.setattr(srv, "default_cache_dir", lambda: str(cache_dir))

    captured: dict[str, Any] = {}
    _block = threading.Event()

    def _fake_conn(*_args: Any, **_kwargs: Any) -> Any:
        conn = MagicMock()
        conn.source_system = 255
        conn.source_component = 190
        conn.wait_heartbeat = lambda blocking=True, timeout=10: _FakeHeartbeat()
        conn.mode_mapping = lambda: {}
        conn.recv_match = lambda blocking=True, timeout=1: (_block.wait(0.02) or None)
        conn.close = lambda: None
        captured["conn"] = conn
        return conn

    monkeypatch.setattr(
        "corvus.mavlink_bridge.mavutil.mavlink_connection", _fake_conn,
    )
    # Skip the slow ACK-confirmed stream setup so _connect() returns promptly.
    monkeypatch.setattr(MavlinkBridge, "_request_streams", lambda self: None)
    monkeypatch.setattr(MavlinkBridge, "_request_message_intervals", lambda self: None)
    monkeypatch.setattr(MavlinkBridge, "_request_version", lambda self: None)

    server = srv.create_server(port=0, mavlink_conn="udp:127.0.0.1:9999")
    http_thread = threading.Thread(
        target=server.serve_forever, name="corvus-shell-http", daemon=True,
        kwargs={"poll_interval": 0.05},
    )
    http_thread.start()
    try:
        # Wait for the MAVLink thread to connect so _conn is set before stop().
        deadline = time.monotonic() + 5.0
        while "conn" not in captured and time.monotonic() < deadline:
            time.sleep(0.02)
        assert "conn" in captured, "fake MAVLink conn was never created"
        conn = captured["conn"]

        server.mavlink.stop()  # exercises stop() -> stop_shell() -> release
        http_thread.join(timeout=2)
        server.shutdown()
        server.server_close()

        # The release SERIAL_CONTROL (flags=0, count=0) was sent during stop().
        release_calls = [
            c for c in conn.mav.serial_control_send.call_args_list
            if len(c.args) >= 5 and c.args[0] == DEV_SHELL
            and c.args[1] == 0 and c.args[4] == 0
        ]
        assert release_calls, (
            "stop() did not release the NSH shell "
            "(no flags=0,count=0 SERIAL_CONTROL on device=SHELL)"
        )
        # MAVLink thread joined dead — no leaked thread.
        assert server.mavlink._thread is not None
        assert not server.mavlink._thread.is_alive(), "mavlink thread survived stop()"
    finally:
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


# ---------------------------------------------------------------------------
# 6. Server dispatch: /api/console/command shell routing.
#    Mirrors tests/test_server_takeoff.py (FakeBridge + object.__new__).
#
#    NOTE: the feature spec describes SHELL_COMMANDS + shell/nsh prefix
#    dispatch inside _api_console_command. See the review report: at audit
#    time this dispatch is NOT yet present in corvus/server.py, so the four
#    routing tests below FAIL with "unknown command" — that is the bug
#    evidence. They are written to the spec so they turn green once the
#    backend agent implements the dispatch.
# ---------------------------------------------------------------------------

class FakeShellBridge:
    """Stand-in bridge for /api/console/command: records shell + takeoff calls."""

    def __init__(self, result: bool = True, error: str = "") -> None:
        self.result = result
        self.error = error
        self.shell_commands: list[str] = []
        self.altitudes: list[float] = []

    def send_shell_command(self, text: str) -> bool:
        self.shell_commands.append(text)
        return self.result

    def get_last_command_error(self) -> str:
        return self.error

    def takeoff(self, altitude: float) -> bool:
        self.altitudes.append(altitude)
        return self.result

    def arm(self, arm: bool) -> bool:
        return self.result


def _handler_with_bridge(
    bridge: FakeShellBridge,
) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def test_console_listener_command_dispatches_to_shell() -> None:
    bridge = FakeShellBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_console_command({"command": "listener sensor_combined"})

    # Case preserved, full original text forwarded to the shell.
    assert bridge.shell_commands == ["listener sensor_combined"]
    assert responses == [({"ok": True, "shell": True}, 200)]


def test_console_shell_prefix_dispatches_to_shell() -> None:
    bridge = FakeShellBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_console_command({"command": "shell listener sensor_combined"})

    # Prefix stripped, remainder forwarded case-preserved.
    assert bridge.shell_commands == ["listener sensor_combined"]
    assert responses == [({"ok": True, "shell": True}, 200)]


def test_console_nsh_prefix_dispatches_to_shell() -> None:
    bridge = FakeShellBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_console_command({"command": "nsh top"})

    assert bridge.shell_commands == ["top"]
    assert responses == [({"ok": True, "shell": True}, 200)]


def test_console_shell_command_disconnected_returns_503() -> None:
    # Per spec: failure -> 503 when "DISCONNECTED" is in the error string.
    bridge = FakeShellBridge(result=False, error="DISCONNECTED")
    handler, responses = _handler_with_bridge(bridge)

    handler._api_console_command({"command": "listener sensor_combined"})

    assert bridge.shell_commands == ["listener sensor_combined"]
    assert responses == [({"ok": False, "error": "DISCONNECTED"}, 503)]


def test_console_takeoff_does_not_call_send_shell_command() -> None:
    """Non-shell commands keep working and never route to the shell."""
    bridge = FakeShellBridge(result=True)
    handler, responses = _handler_with_bridge(bridge)

    handler._api_console_command({"command": "takeoff 15"})

    assert bridge.shell_commands == []
    assert bridge.altitudes == [15.0]
    assert responses == [({"ok": True}, 200)]
