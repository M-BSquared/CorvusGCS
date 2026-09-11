"""Regression tests for the stability/robustness hardening pass.

Each test here pins one failure that the code used to have. They are grouped
by the layer that owned the bug: the state store's clock, the MAVLink bridge's
loops and send paths, the tlog writer's disk handling, and the HTTP layer's
error and timeout behaviour.
"""
from __future__ import annotations

import datetime
import logging
import threading
import time
from types import SimpleNamespace
from typing import Any, Callable

import pytest

from corvus.state_store import VehicleStateStore

mavutil = pytest.importorskip("pymavlink.mavutil")

from corvus.mavlink_bridge import (  # noqa: E402
    RECV_ERROR_LIMIT,
    SHELL_LINE_MAX_CHARS,
    STATUSTEXT_MAX_PARTIALS,
    MavlinkBridge,
)


# ---------------------------------------------------------------------------
# Shared fakes
# ---------------------------------------------------------------------------

class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return getattr(self, "source_system", 1)

    def get_srcComponent(self) -> int:
        return getattr(self, "source_component", 1)


class FakeMav:
    def __init__(self) -> None:
        self.sent: list[tuple[str, tuple]] = []
        self.raises: Exception | None = None

    def __getattr__(self, name: str) -> Callable[..., None]:
        if name.startswith("_"):
            raise AttributeError(name)

        def _send(*args: Any) -> None:
            if self.raises is not None:
                raise self.raises
            self.sent.append((name, args))

        return _send


class FakeConnection:
    def __init__(self) -> None:
        self.mav = FakeMav()
        self.source_system = 254
        self.source_component = 190
        self.target_system = 1
        self.target_component = 1
        self.closed = False

    def recv_match(self, blocking: bool = True, timeout: float = 1) -> Any:
        return None

    def close(self) -> None:
        self.closed = True


def ready_bridge(conn_str: str = "udp:0.0.0.0:14540") -> MavlinkBridge:
    """A bridge wired to a fake connection with a fresh heartbeat."""
    store = VehicleStateStore()
    bridge = MavlinkBridge(store, conn_str)
    bridge._conn = FakeConnection()
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._running.set()
    store.heartbeat()
    return bridge


# ---------------------------------------------------------------------------
# State store: heartbeat staleness must not ride the wall clock
# ---------------------------------------------------------------------------

def test_wall_clock_jumping_forward_does_not_fake_a_dead_link(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """NTP correcting the laptop's clock must not drop a healthy link.

    A field laptop boots with no network time and is corrected the moment one
    appears. Measuring heartbeat age on the wall clock turned that correction
    into an instant "heartbeat timeout" for a link that was never late.
    """
    store = VehicleStateStore()
    store.heartbeat()
    assert not store.is_stale(timeout=5.0)

    # The wall clock leaps an hour ahead; the monotonic clock does not.
    monkeypatch.setattr(
        "corvus.state_store.time.time", lambda: time.time() + 3600.0,
    )

    assert not store.is_stale(timeout=5.0)


def test_wall_clock_jumping_backward_does_not_hide_a_dead_link(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The worse half of the same bug: a backwards correction parked the last
    heartbeat in the future, so the staleness test never fired again — a lost
    aircraft that kept its green dot and never reconnected."""
    store = VehicleStateStore()
    store.heartbeat()

    monkeypatch.setattr(
        "corvus.state_store.time.time", lambda: time.time() - 3600.0,
    )
    # Age the (monotonic) heartbeat past every timeout.
    store._last_heartbeat -= 100.0

    assert store.is_stale(timeout=5.0)


def test_heartbeat_returns_the_instant_it_recorded() -> None:
    """The bridge feeds this straight into its jitter window, so the staleness
    timer and the jitter measurement describe the same arrival."""
    store = VehicleStateStore()
    before = time.monotonic()
    stamp = store.heartbeat()
    after = time.monotonic()

    assert isinstance(stamp, float)
    assert before <= stamp <= after
    assert store._last_heartbeat == stamp


# ---------------------------------------------------------------------------
# Bridge: the receive loop must not spin on a failing read
# ---------------------------------------------------------------------------

def test_receive_loop_gives_up_instead_of_spinning_on_a_dead_port(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Unplugging USB makes pyserial raise instantly on every read.

    The loop used to swallow each one and try again with no pause, pegging a
    core until the heartbeat drop timeout finally ended the cycle. Now a run of
    failures ends the cycle itself, so the reconnect backoff takes over.
    """
    bridge = ready_bridge()
    calls = {"n": 0}

    def exploding_recv(blocking: bool = True, timeout: float = 1) -> Any:
        calls["n"] += 1
        raise OSError("device disconnected")

    bridge._conn.recv_match = exploding_recv
    # No real sleeping: the point is the bail-out, not the pacing.
    monkeypatch.setattr(bridge, "_interruptible_sleep", lambda seconds: None)

    with pytest.raises(ConnectionError):
        bridge._receive_loop()

    assert calls["n"] == RECV_ERROR_LIMIT


def test_receive_loop_paces_the_failures_it_does_retry(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Each failed read costs a pause, so the retries are not a busy loop."""
    bridge = ready_bridge()
    naps: list[float] = []

    def exploding_recv(blocking: bool = True, timeout: float = 1) -> Any:
        raise OSError("device disconnected")

    bridge._conn.recv_match = exploding_recv
    monkeypatch.setattr(bridge, "_interruptible_sleep", lambda s: naps.append(s))

    with pytest.raises(ConnectionError):
        bridge._receive_loop()

    # One nap per retried failure; the last failure raises instead of napping.
    assert len(naps) == RECV_ERROR_LIMIT - 1
    assert all(nap > 0 for nap in naps)


def test_a_single_read_failure_does_not_end_the_cycle(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A one-off glitch on a lossy radio is not a disconnect: the error counter
    resets on the next successful read."""
    bridge = ready_bridge()
    script = iter([OSError("glitch"), None, None])

    def flaky_recv(blocking: bool = True, timeout: float = 1) -> Any:
        try:
            item = next(script)
        except StopIteration:
            bridge._running.clear()
            return None
        if isinstance(item, Exception):
            raise item
        return item

    bridge._conn.recv_match = flaky_recv
    monkeypatch.setattr(bridge, "_interruptible_sleep", lambda seconds: None)

    bridge._receive_loop()  # must not raise


# ---------------------------------------------------------------------------
# Bridge: the GCS heartbeat thread must not orphan its successor
# ---------------------------------------------------------------------------

def test_a_dying_heartbeat_loop_does_not_disown_its_replacement() -> None:
    """A reconnect can start a new heartbeat thread while the old one is on its
    way out. Clearing the handle unconditionally orphaned the *new* thread:
    stop() then never joined it, and the next connect started a second loop
    alongside it — two GCS heartbeats at 1 Hz on one link.
    """
    bridge = ready_bridge()
    replacement = threading.Thread(target=lambda: None, name="replacement")
    bridge._hb_thread = replacement

    # The outgoing loop runs with _running clear, so it exits immediately, and
    # finds a handle that is not its own.
    bridge._running.clear()
    bridge._gcs_hb_loop()

    assert bridge._hb_thread is replacement


def test_a_dying_heartbeat_loop_clears_its_own_handle() -> None:
    """The case the clearing was there for still works."""
    bridge = ready_bridge()
    bridge._running.clear()

    def run() -> None:
        bridge._hb_thread = threading.current_thread()
        bridge._gcs_hb_loop()

    thread = threading.Thread(target=run)
    thread.start()
    thread.join(timeout=2)

    assert bridge._hb_thread is None


def test_the_heartbeat_loop_stops_on_the_stop_event() -> None:
    """stop() sets _stop_event before joining; the loop must honour it."""
    bridge = ready_bridge()
    bridge._stop_event.set()

    started = time.monotonic()
    bridge._gcs_hb_loop()

    assert time.monotonic() - started < 1.0


# ---------------------------------------------------------------------------
# Bridge: connect-time waits and backoff must honour shutdown
# ---------------------------------------------------------------------------

def test_the_heartbeat_wait_bails_out_on_shutdown() -> None:
    """stop() during connect must not sit out the remaining 10 s budget."""
    bridge = ready_bridge()
    bridge._stop_event.set()

    started = time.monotonic()
    assert bridge._wait_vehicle_heartbeat(10.0) is None
    assert time.monotonic() - started < 1.0


def test_the_heartbeat_wait_stops_when_the_connection_is_torn_down() -> None:
    bridge = ready_bridge()
    bridge._conn = None

    assert bridge._wait_vehicle_heartbeat(10.0) is None


def test_the_reconnect_backoff_is_interruptible(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The backoff reaches 8 s and stop() joins the thread for 3, so a plain
    sleep let shutdown return with the reconnect loop still parked."""
    bridge = ready_bridge()
    naps: list[float] = []

    def fake_connect() -> None:
        raise ConnectionError("nope")

    monkeypatch.setattr(bridge, "_connect", fake_connect)
    monkeypatch.setattr(
        bridge, "_interruptible_sleep",
        lambda s: (naps.append(s), bridge._running.clear())[0],
    )

    bridge._run()

    assert naps, "the reconnect backoff must go through _interruptible_sleep"


# ---------------------------------------------------------------------------
# Bridge: parameter send paths must report, never raise
# ---------------------------------------------------------------------------

def test_a_param_list_request_on_a_torn_down_link_returns_false() -> None:
    """The send used to dereference self._conn without a guard, so a
    concurrent stop() turned an operator click into an AttributeError out of
    the HTTP handler — a dropped connection instead of an error message."""
    bridge = ready_bridge()
    bridge._conn.mav.raises = OSError("socket closed")

    assert bridge.request_param_list() is False
    assert bridge.get_last_command_error()


def test_a_failed_param_list_request_does_not_wedge_the_state_machine() -> None:
    """Leaving the state at "downloading" with nothing on the wire blocked
    every later parameter operation behind an "in progress" that never was."""
    bridge = ready_bridge()
    bridge._conn.mav.raises = OSError("socket closed")

    bridge.request_param_list()

    assert bridge.param_status()["state"] == "idle"
    # And a retry is accepted rather than refused as a duplicate.
    bridge._conn.mav.raises = None
    assert bridge.request_param_list() is True


@pytest.mark.parametrize("value", [float("nan"), float("inf"), float("-inf")])
def test_a_non_finite_parameter_value_is_refused(value: float) -> None:
    """NaN reaches PX4 as a valid float32 and is stored as one, and the echo
    check could never confirm it (NaN != NaN) — so it reported an unconfirmed
    write for a value that had in fact landed on the vehicle."""
    bridge = ready_bridge()

    assert bridge.set_param("MPC_XY_P", value) is False
    assert "finite" in bridge.get_last_command_error()
    assert bridge._conn.mav.sent == []


def test_a_boolean_parameter_value_is_refused() -> None:
    """bool is an int in Python: True would have been written as 1.0."""
    bridge = ready_bridge()

    assert bridge.set_param("MPC_XY_P", True) is False
    assert bridge._conn.mav.sent == []


def test_an_over_long_parameter_name_is_refused_not_truncated() -> None:
    """PARAM_SET carries char[16]. A silently shortened name addresses a
    different parameter, which on a write is the wrong knob entirely."""
    bridge = ready_bridge()

    assert bridge.set_param("A" * 17, 1.0) is False
    assert "name" in bridge.get_last_command_error()
    assert bridge._conn.mav.sent == []
    assert bridge.request_param("A" * 17) is False


def test_a_sixteen_character_parameter_name_is_still_accepted() -> None:
    """The cap is the field width, not one below it."""
    bridge = ready_bridge()

    assert bridge._encode_param_name("A" * 16) == b"A" * 16
    assert bridge._encode_param_name("A" * 17) is None


# ---------------------------------------------------------------------------
# Bridge: message handling must not be derailed by bad values
# ---------------------------------------------------------------------------

def test_a_nonsense_system_time_does_not_abort_the_rest_of_the_frame() -> None:
    """An autopilot with no GPS lock sends an out-of-range time_unix_usec.
    Converting it raised, which aborted the dispatch and took the boot_ms read
    — the only honest reboot signal — down with it.
    """
    bridge = ready_bridge()
    msg = FakeMessage(
        message_type="SYSTEM_TIME",
        time_unix_usec=2**63,
        time_boot_ms=4242,
    )

    bridge._dispatch(msg)  # must not raise

    assert bridge._store.get_snapshot()["boot_ms"] == 4242


def test_a_valid_system_time_still_reaches_the_state() -> None:
    bridge = ready_bridge()
    stamp = datetime.datetime(2026, 9, 10, 14, 30, 5, tzinfo=datetime.timezone.utc)
    msg = FakeMessage(
        message_type="SYSTEM_TIME",
        time_unix_usec=int(stamp.timestamp() * 1e6),
        time_boot_ms=10,
    )

    bridge._dispatch(msg)

    assert bridge._store.get_snapshot()["time"] == "14:30:05 UTC"


def test_partial_statustexts_are_bounded() -> None:
    """One entry per (system, component, id) with nothing bounding how many
    could pile up inside the 10 s expiry window."""
    bridge = ready_bridge()

    for chunk_id in range(1, STATUSTEXT_MAX_PARTIALS + 20):
        bridge._handle_statustext(FakeMessage(
            message_type="STATUSTEXT",
            text=b"x" * 50,          # exactly a chunk: never final
            severity=6,
            id=chunk_id,
            chunk_seq=1,             # seq 1 with no seq 0: never completes
        ))

    assert len(bridge._statustext_chunks) <= STATUSTEXT_MAX_PARTIALS


def test_a_shell_line_that_never_ends_is_flushed_rather_than_buffered() -> None:
    """A "line" was bounded only by the vehicle sending a newline."""
    bridge = ready_bridge()
    bridge._shell_active = True
    published: list[dict[str, Any]] = []
    bridge.add_console_sub(published.append)

    payload = b"z" * 70
    for _ in range((SHELL_LINE_MAX_CHARS // 70) + 5):
        bridge._handle_serial_control(FakeMessage(
            message_type="SERIAL_CONTROL",
            device=mavutil.mavlink.SERIAL_CONTROL_DEV_SHELL,
            count=len(payload),
            data=payload,
            target_system=254,
            target_component=190,
        ))

    assert len(bridge._shell_buffer) < SHELL_LINE_MAX_CHARS
    assert published, "the flushed text is published, not discarded"


# ---------------------------------------------------------------------------
# tlog: a full disk must not silence the writer
# ---------------------------------------------------------------------------

def test_a_write_failure_is_reported_and_the_file_still_closes(
    tmp_path, caplog: pytest.LogCaptureFixture,
) -> None:
    """A field laptop fills its disk mid-flight. The writer thread died on the
    resulting OSError with nothing logged, so recording stopped silently — and
    stop() then raised before it reached the close, leaking the handle.
    """
    from corvus.tlog import TlogWriter

    writer = TlogWriter(str(tmp_path / "flight.tlog"))
    real_write = writer._file.write

    def failing_write(data: bytes) -> int:
        raise OSError(28, "No space left on device")

    with caplog.at_level(logging.WARNING, logger="corvus.tlog"):
        writer._file.write = failing_write  # type: ignore[method-assign]
        writer.write_frame(b"\xfd\x00")
        # Give the writer thread a moment to hit the failure.
        deadline = time.monotonic() + 2.0
        while not writer._failed and time.monotonic() < deadline:
            time.sleep(0.01)
        assert writer._failed, "the failure must be noticed, not swallowed"
        writer._file.write = real_write  # type: ignore[method-assign]
        writer.stop()

    assert writer._file.closed, "stop() must close the handle even after a failure"
    assert any("tlog" in record.message for record in caplog.records)


def test_a_failed_writer_stops_accepting_frames(tmp_path) -> None:
    """Nothing drains the buffer once the writer is down, so the producer must
    stop filling it."""
    from corvus.tlog import TlogWriter

    writer = TlogWriter(str(tmp_path / "flight.tlog"))
    try:
        writer._failed = True
        writer.write_frame(b"\xfd\x00")
        assert len(writer._buf) == 0
    finally:
        writer.stop()


# ---------------------------------------------------------------------------
# HTTP layer: a failing endpoint must answer, not vanish
# ---------------------------------------------------------------------------

def test_an_endpoint_that_raises_answers_500_instead_of_dropping_the_socket(
    server_with_store, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """http.server does not catch exceptions out of do_GET/do_POST: they
    unwound into socketserver, which printed a traceback and closed the socket
    without writing anything. The browser then saw a network error, so the UI
    could not tell "the backend has a bug" from "the backend is gone" — and
    during a flight those call for very different reactions.
    """
    import http.client

    from corvus.server import CorvusHandler

    def exploding(self) -> None:
        raise RuntimeError("endpoint bug")

    monkeypatch.setattr(CorvusHandler, "_api_version", exploding)

    host, port = server_with_store.server_address
    conn = http.client.HTTPConnection(host, port, timeout=5)
    try:
        conn.request("GET", "/api/version")
        resp = conn.getresponse()
        body = resp.read()
    finally:
        conn.close()

    assert resp.status == 500
    assert b"internal server error" in body


def test_a_working_endpoint_is_untouched_by_the_guard(server_with_store) -> None:
    """The guard must be invisible on the happy path."""
    import http.client
    import json as _json

    host, port = server_with_store.server_address
    conn = http.client.HTTPConnection(host, port, timeout=5)
    try:
        conn.request("GET", "/api/version")
        resp = conn.getresponse()
        payload = _json.loads(resp.read())
    finally:
        conn.close()

    assert resp.status == 200
    assert payload["product"] == "Corvus GCS"


def test_a_client_that_promises_a_body_and_never_sends_it_is_dropped(
    server_with_store,
) -> None:
    """Without a socket timeout every blocking read waits forever. A tab killed
    between its headers and its body left rfile.read(Content-Length) parked on
    a thread that never came back, leaking the thread and the connection for
    the life of the process.
    """
    import socket as _socket

    from corvus.server import CorvusHandler

    assert CorvusHandler.timeout is not None
    assert CorvusHandler.timeout > 15, (
        "the timeout also bounds SSE writes, so it must clear the keep-alive "
        "interval by a wide margin"
    )

    host, port = server_with_store.server_address
    sock = _socket.create_connection((host, port), timeout=5)
    try:
        # Headers promise 1 MB; not one byte of it is ever sent.
        sock.sendall(
            b"POST /api/mavlink/arm HTTP/1.0\r\n"
            b"Content-Type: application/json\r\n"
            b"Content-Length: 1048576\r\n\r\n"
        )
        sock.settimeout(1.0)
        with pytest.raises((_socket.timeout, TimeoutError)):
            # The handler is still parked on its read, so nothing comes back
            # yet — the point is that it is parked on a *bounded* read.
            sock.recv(1)
    finally:
        sock.close()


def test_an_oversized_upstream_tile_is_discarded_not_cached(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A captive portal answers 200 with a login page. Caching that and later
    serving it as a map tile is worse than having no tile at all."""
    import urllib.request

    from corvus.server import TILE_UPSTREAM_MAX_BYTES, CorvusHandler, _UpstreamBreaker

    class _Resp:
        def __enter__(self) -> "_Resp":
            return self

        def __exit__(self, *exc: object) -> bool:
            return False

        def read(self, amt: int | None = None) -> bytes:
            blob = b"<html>login</html>" * TILE_UPSTREAM_MAX_BYTES
            return blob if amt is None else blob[:amt]

    monkeypatch.setattr(urllib.request, "urlopen", lambda *a, **k: _Resp())

    handler = object.__new__(CorvusHandler)
    handler.tile_breaker = _UpstreamBreaker()

    assert handler._fetch_upstream_tile("satellite", 14, 8600, 5750) is None


def test_a_normal_sized_tile_still_comes_through(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The cap is a sanity bound, not a new failure mode for real tiles."""
    import urllib.request

    from corvus.server import CorvusHandler, _UpstreamBreaker

    tile = b"\x89PNG\r\n\x1a\n" + b"\x00" * 20_000

    class _Resp:
        def __enter__(self) -> "_Resp":
            return self

        def __exit__(self, *exc: object) -> bool:
            return False

        def read(self, amt: int | None = None) -> bytes:
            return tile if amt is None else tile[:amt]

    monkeypatch.setattr(urllib.request, "urlopen", lambda *a, **k: _Resp())

    handler = object.__new__(CorvusHandler)
    handler.tile_breaker = _UpstreamBreaker()

    assert handler._fetch_upstream_tile("satellite", 14, 8600, 5750) == tile


def test_a_reset_before_the_request_line_is_logged_quietly(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """The guard around do_GET/do_POST cannot reach this one: the reset lands
    on handle_one_request's read of the request line, before either exists.
    socketserver's default printed forty lines of traceback per closed tab.
    """
    from corvus.server import CorvusServer

    server = object.__new__(CorvusServer)
    with caplog.at_level(logging.DEBUG, logger="corvus.server"):
        try:
            raise ConnectionResetError(54, "Connection reset by peer")
        except ConnectionResetError:
            server.handle_error(None, ("127.0.0.1", 51234))

    assert not [r for r in caplog.records if r.levelno >= logging.WARNING]
    assert any("disconnected" in r.message for r in caplog.records)


def test_a_real_server_error_keeps_its_traceback(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """Quieting the disconnects must not quiet the bugs."""
    from corvus.server import CorvusServer

    server = object.__new__(CorvusServer)
    with caplog.at_level(logging.DEBUG, logger="corvus.server"):
        try:
            raise RuntimeError("a genuine bug")
        except RuntimeError:
            server.handle_error(None, ("127.0.0.1", 51234))

    errors = [r for r in caplog.records if r.levelno >= logging.ERROR]
    assert errors and errors[0].exc_info is not None


# ---------------------------------------------------------------------------
# serve.py: a mistyped argument is an operator error, not a crash
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("argument", ["--port", "eight-thousand", "99999", "0"])
def test_a_bad_port_argument_is_reported_rather_than_raised(argument: str) -> None:
    """`python3 serve.py --port 8137` used to die with an int() traceback out
    of module scope, which reads like a crashed program rather than a typo."""
    import subprocess
    import sys as _sys

    result = subprocess.run(
        [_sys.executable, "serve.py", argument],
        capture_output=True, text=True, timeout=60,
    )

    assert result.returncode == 2
    assert "Traceback" not in (result.stdout + result.stderr)


def test_the_heartbeat_wait_does_not_spin_on_a_transport_that_answers_instantly() -> None:
    """The wait is sliced so shutdown is noticed promptly, which means it now
    retries. A transport that returns "nothing" immediately must not be
    retried in a tight loop for the whole ten-second budget.
    """
    bridge = ready_bridge()
    calls = {"n": 0}

    def instant_none(blocking: bool = True, timeout: float = 1.0) -> Any:
        calls["n"] += 1
        return None

    bridge._conn.wait_heartbeat = instant_none  # type: ignore[attr-defined]

    started = time.monotonic()
    assert bridge._wait_vehicle_heartbeat(10.0) is None

    assert calls["n"] == 1, "an instant None is taken at face value"
    assert time.monotonic() - started < 1.0


def test_the_heartbeat_wait_keeps_waiting_through_a_real_timeout_slice() -> None:
    """A slice that genuinely blocked is a timeout, not an answer: the wait
    must continue until the caller's budget is spent."""
    bridge = ready_bridge()
    calls = {"n": 0}

    def slow_none(blocking: bool = True, timeout: float = 1.0) -> Any:
        calls["n"] += 1
        time.sleep(timeout)
        return None

    bridge._conn.wait_heartbeat = slow_none  # type: ignore[attr-defined]

    assert bridge._wait_vehicle_heartbeat(0.3) is None
    assert calls["n"] >= 1


# ---------------------------------------------------------------------------
# Port choice: a ground station must not squat on the companion-computer link
# ---------------------------------------------------------------------------

def test_the_default_endpoint_is_the_gcs_port_not_the_onboard_one() -> None:
    """14540 is PX4's onboard link, which MAVSDK and MAVROS bind. "udp:" means
    bind, so defaulting there took a socket a companion process on the same
    machine needs — and the loser gets no telemetry at all."""
    from corvus.config import CorvusConfig

    assert CorvusConfig().mavlink_connection == "udp:0.0.0.0:14550"


def test_a_port_already_in_use_says_so(monkeypatch: pytest.MonkeyPatch) -> None:
    """"[Errno 48] Address already in use" is true and useless: it names
    neither the culprit nor the fix."""
    import errno as _errno

    import corvus.mavlink_bridge as mb

    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14550")

    def taken(*args: Any, **kwargs: Any) -> Any:
        raise OSError(_errno.EADDRINUSE, "Address already in use")

    monkeypatch.setattr(mb.mavutil, "mavlink_connection", taken)

    with pytest.raises(ConnectionError) as excinfo:
        bridge._connect()

    message = str(excinfo.value)
    assert "already in use" in message
    assert "MAVROS" in message, "name the programs that actually take it"
    assert bridge._store.get_snapshot()["link_error"] == message


def test_an_unrelated_connect_error_is_not_relabelled(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Only EADDRINUSE gets the port-clash wording."""
    import errno as _errno

    import corvus.mavlink_bridge as mb

    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14550")

    def refused(*args: Any, **kwargs: Any) -> Any:
        raise OSError(_errno.EACCES, "Permission denied")

    monkeypatch.setattr(mb.mavutil, "mavlink_connection", refused)

    with pytest.raises(OSError) as excinfo:
        bridge._connect()

    assert "already in use" not in str(excinfo.value)


# ---------------------------------------------------------------------------
# "Is it flying?" — the vehicle's own answer, not one inferred from altitude
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("landed", [0, 1, 2, 3, 4])
def test_extended_sys_state_reaches_the_store(landed: int) -> None:
    """MAV_LANDED_STATE is the honest signal for the bar's FLYING label. Armed
    does not mean flying — a vehicle sits armed on the ground before every
    takeoff — and altitude cannot tell a low hover from a bad home reference.
    """
    bridge = ready_bridge()

    bridge._dispatch(FakeMessage(
        message_type="EXTENDED_SYS_STATE", landed_state=landed, vtol_state=0,
    ))

    assert bridge._store.get_snapshot()["landed_state"] == landed


def test_a_nonsense_landed_state_is_ignored_rather_than_published() -> None:
    """Out-of-range means a dialect this build does not understand; the bar
    then falls back to height instead of acting on a value nobody defined."""
    bridge = ready_bridge()
    bridge._dispatch(FakeMessage(
        message_type="EXTENDED_SYS_STATE", landed_state=2, vtol_state=0,
    ))

    bridge._dispatch(FakeMessage(
        message_type="EXTENDED_SYS_STATE", landed_state=99, vtol_state=0,
    ))

    assert bridge._store.get_snapshot()["landed_state"] == 2


def test_a_lost_link_forgets_that_it_was_flying() -> None:
    """Holding IN_AIR across a disconnect would leave the bar saying FLYING
    over a vehicle nobody can see any more."""
    store = VehicleStateStore()
    store.heartbeat()
    store.update(landed_state=2)

    store.set_disconnected()

    assert store.get_snapshot()["landed_state"] == 0


def test_the_vehicle_is_asked_for_extended_sys_state_on_both_link_classes() -> None:
    """It is two bytes of payload, and it is what tells the bar the aircraft is
    flying — worth the bandwidth even on a 57 kbps radio."""
    msg_id = mavutil.mavlink.MAVLINK_MSG_ID_EXTENDED_SYS_STATE

    udp = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14550")
    serial = MavlinkBridge(VehicleStateStore(), "serial:/dev/ttyUSB0:57600")

    assert msg_id in udp._message_intervals()
    assert msg_id in serial._message_intervals()


# ---------------------------------------------------------------------------
# The takeoff ceiling, and the two places that have to agree on it
# ---------------------------------------------------------------------------

def test_the_takeoff_ceiling_is_the_operational_one() -> None:
    """50 m was an arbitrary round number that refused ordinary survey and
    inspection heights. 120 m AGL is where the rules actually stop."""
    from corvus.mavlink_bridge import TAKEOFF_ALTITUDE_MAX_M, TAKEOFF_ALTITUDE_MIN_M

    assert TAKEOFF_ALTITUDE_MIN_M == 1.0
    assert TAKEOFF_ALTITUDE_MAX_M == 120.0


def test_the_takeoff_slider_stops_where_the_backend_stops() -> None:
    """Two places carry this number. A slider that offers a height the backend
    rejects is a control whose top end only ever produces a 400."""
    import re
    from pathlib import Path

    from corvus.mavlink_bridge import TAKEOFF_ALTITUDE_MAX_M, TAKEOFF_ALTITUDE_MIN_M

    html = (Path(__file__).resolve().parent.parent / "src" / "index.html").read_text()
    tag = re.search(r'<input[^>]*id="takeoffAlt"[^>]*>', html)
    assert tag, "the takeoff slider must still exist"
    lo = re.search(r'min="(\d+)"', tag.group(0))
    hi = re.search(r'max="(\d+)"', tag.group(0))
    assert lo and hi
    assert float(lo.group(1)) == TAKEOFF_ALTITUDE_MIN_M
    assert float(hi.group(1)) == TAKEOFF_ALTITUDE_MAX_M


# ---------------------------------------------------------------------------
# The remaining position gates
# ---------------------------------------------------------------------------

def _autoack_bridge() -> MavlinkBridge:
    """A bridge whose fake link ACKs commands and missions straight back."""
    bridge = ready_bridge()

    class AckingMav(FakeMav):
        def __getattr__(self, name: str) -> Callable[..., None]:
            inner = FakeMav.__getattr__(self, name)

            def _send(*args: Any) -> None:
                inner(*args)
                # COMMAND_LONG carries the command id at index 2, COMMAND_INT
                # at index 3 (it has an extra frame field up front).
                if name == "command_long_send":
                    self._reply_ack(int(args[2]))
                elif name == "command_int_send":
                    self._reply_ack(int(args[3]))
                elif name == "mission_count_send":
                    bridge._dispatch(FakeMessage(
                        message_type="MISSION_ACK", type=0,
                        target_system=254, target_component=190,
                    ))

            return _send

        def _reply_ack(self, command: int) -> None:
            bridge._dispatch(FakeMessage(
                message_type="COMMAND_ACK", command=command, result=0,
                target_system=254, target_component=190,
            ))

    bridge._conn.mav = AckingMav()
    bridge.TAKEOFF_REFERENCE_WAIT_S = 0.05
    return bridge


def test_fly_to_points_no_longer_needs_an_altitude_reference() -> None:
    """The gate that used to be here was pure ceremony: every mission item is
    built in MAV_FRAME_GLOBAL_RELATIVE_ALT, whose z *is* the AGL number the
    operator typed, so the AMSL value the gate computed was thrown away without
    ever being sent. It refused missions over a conversion they do not need.
    """
    bridge = _autoack_bridge()
    bridge._mode_values = {"MISSION": (29, 4, 4)}
    assert bridge._home_alt_amsl is None
    assert bridge._position_home_alt_amsl is None

    ok = bridge.fly_to_points([
        {"lat": 48.08, "lon": 11.64, "alt_agl": 30.0},
        {"lat": 48.09, "lon": 11.65, "alt_agl": 30.0},
    ])

    assert ok is True
    assert any(name == "mission_count_send" for name, _ in bridge._conn.mav.sent)


def test_fly_to_points_carries_the_typed_agl_into_the_mission() -> None:
    """The altitude that reaches the wire is the operator's AGL number, in the
    relative-altitude frame — which is exactly why no reference is needed."""
    bridge = _autoack_bridge()
    items = bridge._build_mission_item_spec(
        0, mavutil.mavlink.MAV_CMD_NAV_WAYPOINT, 48.08, 11.64, 30.0, 0, 0, 0, 0,
    )
    assert items["frame"] == mavutil.mavlink.MAV_FRAME_GLOBAL_RELATIVE_ALT
    assert items["z"] == 30.0


def test_set_home_keeps_its_refusal_without_a_reference() -> None:
    """Unlike takeoff, this altitude is not a number being converted for the
    wire — it is the altitude home will *have*, and home altitude is what RTL
    descends to. Guessing it moves the landing point vertically as a side
    effect of dragging it sideways.
    """
    bridge = _autoack_bridge()

    assert bridge.set_home(48.08, 11.64) is False
    assert "altitude reference" in bridge.get_last_command_error()


def test_set_home_waits_for_a_late_reference_instead_of_refusing() -> None:
    """Right after connect the reference is usually late, not absent."""
    bridge = _autoack_bridge()
    bridge.TAKEOFF_REFERENCE_WAIT_S = 2.0

    def arrive_late() -> None:
        time.sleep(0.1)
        bridge._dispatch(FakeMessage(
            message_type="HOME_POSITION",
            latitude=480800000, longitude=116400000, altitude=520000,
        ))

    threading.Thread(target=arrive_late, daemon=True).start()

    assert bridge.set_home(48.081, 11.641) is True


def test_the_mission_planner_and_the_bar_agree_on_airborne() -> None:
    """fly_to_points prepends a takeoff item only when on the ground, so its
    reading of "flying" has to match the one the status bar shows."""
    bridge = ready_bridge()

    bridge._store.update(landed_state=2, altitude_agl=0.0)
    assert bridge._is_airborne() is True, "the vehicle's own verdict wins"

    bridge._store.update(landed_state=1, altitude_agl=25.0)
    assert bridge._is_airborne() is False, "an explicit ON_GROUND outranks height"

    bridge._store.update(landed_state=0, altitude_agl=25.0)
    assert bridge._is_airborne() is True, "no report: fall back to height"

    bridge._store.update(landed_state=0, altitude_agl=0.2)
    assert bridge._is_airborne() is False, "0.2 m is ground noise, not flight"
