"""Regression tests for the link-stability and safety fixes.

Each test here pins one behaviour that was wrong before, grouped by the failure
it prevents: a macOS USB link that could not be identified, a parameter
recovery that flooded the radio it was recovering, a mission upload that held
every other command behind it, a forwarder that subscribed anything that sent
it a byte, a write reported as confirmed after the link had gone, and a
signing key that was read and then ignored.
"""
from __future__ import annotations

import os
import threading
import time
from types import SimpleNamespace
from typing import Any

import pytest
from pymavlink import mavutil

from corvus.mavlink_bridge import (
    FLY_TO_MAX_POINTS,
    MISSION_UPLOAD_QUIET_S,
    PARAM_DOWNLOAD_MAX_ROUNDS,
    PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL,
    MavlinkBridge,
    _PHANTOM_TTY_RE,
)
from corvus.state_store import VehicleStateStore


def bridge(connection: str = "udp:127.0.0.1:14550") -> MavlinkBridge:
    return MavlinkBridge(VehicleStateStore(), connection)


# ---------------------------------------------------------------------------
# macOS serial devices are identified by descriptor, not by name
# ---------------------------------------------------------------------------

MACOS_PORTS = [
    {"device": "/dev/cu.usbmodem14201", "description": "PX4 FMU v6X.x",
     "hwid": "USB VID:PID=26AC:0032 SER=0 LOCATION=0-1"},
    {"device": "/dev/cu.usbserial-D308", "description": "FT231X USB UART",
     "hwid": "USB VID:PID=0403:6015 SER=D308"},
    {"device": "/dev/cu.SLAB_USBtoUART", "description": "CP2102 USB to UART Bridge",
     "hwid": "USB VID:PID=10C4:EA60"},
    {"device": "/dev/cu.usbmodem-nothing", "description": "", "hwid": ""},
]


@pytest.fixture
def macos_ports(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        MavlinkBridge, "list_serial_ports", staticmethod(lambda: list(MACOS_PORTS)),
    )


@pytest.mark.parametrize(
    ("device", "expected"),
    [
        ("/dev/cu.usbmodem14201", "usb"),      # Pixhawk on a USB cable
        ("/dev/tty.usbmodem14201", "usb"),     # the same port, tty node
        ("/dev/cu.usbserial-D308", "sik"),     # FTDI bridge = SiK radio
        ("/dev/cu.SLAB_USBtoUART", "sik"),     # CP210x bridge = SiK clone
        ("/dev/cu.usbmodem-nothing", "unknown"),  # no descriptor: never guess
    ],
)
def test_macos_serial_devices_are_classified_from_their_usb_descriptor(
    macos_ports: None, device: str, expected: str,
) -> None:
    """A macOS device name says nothing; the descriptor behind it does.

    Every one of these used to classify as "unknown", which refused firmware
    flashing over a USB cable on the platform Corvus ships a .dmg for.
    """
    assert bridge(f"serial:{device}:57600").transport() == expected


def test_a_macos_usb_cable_is_flashable(macos_ports: None) -> None:
    assert bridge("serial:/dev/cu.usbmodem14201:115200").is_direct_usb() is True


def test_a_macos_sik_radio_is_not_flashable(macos_ports: None) -> None:
    assert bridge("serial:/dev/cu.usbserial-D308:57600").is_direct_usb() is False


def test_the_tty_node_of_a_macos_port_resolves_through_its_cu_sibling(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An operator who types the tty node gets the same answer as the cu node.

    macOS gives every port two nodes and pyserial enumerates only the ``cu``
    one, so a lookup that matched the name literally answered "unknown" about
    hardware it could see perfectly well.
    """
    monkeypatch.setattr(
        MavlinkBridge, "list_serial_ports",
        staticmethod(lambda: [dict(MACOS_PORTS[0])]),
    )
    assert bridge("serial:/dev/tty.usbmodem14201:115200").is_direct_usb() is True
    # A name with no descriptor behind it under either spelling stays unknown.
    assert bridge("serial:/dev/tty.nothing-here:115200").transport() == "unknown"


def test_linux_and_windows_classification_is_unchanged(macos_ports: None) -> None:
    assert bridge("serial:/dev/ttyACM0:115200").transport() == "usb"
    assert bridge("serial:/dev/ttyUSB0:57600").transport() == "sik"
    assert bridge("udp:0.0.0.0:14550").transport() == "udp"
    assert bridge("tcp:127.0.0.1:5760").transport() == "tcp"


@pytest.mark.parametrize(
    "device",
    ["/dev/cu.Bluetooth-Incoming-Port", "/dev/cu.debug-console", "/dev/ttyS0"],
)
def test_ports_that_are_never_an_aircraft_are_filtered_from_the_picker(
    device: str,
) -> None:
    """Both macOS entries are present on every Mac whether or not anything is
    plugged in, and the Bluetooth one sorts to the top of the list."""
    assert _PHANTOM_TTY_RE.match(device)


def test_a_real_macos_port_is_not_filtered() -> None:
    assert not _PHANTOM_TTY_RE.match("/dev/cu.usbmodem14201")


def test_macos_glob_fallback_finds_usb_devices(monkeypatch: pytest.MonkeyPatch) -> None:
    """Without pyserial the fallback still has to know macOS device shapes."""
    import serial.tools.list_ports as list_ports
    import glob

    monkeypatch.setattr(list_ports, "comports", lambda: [])
    monkeypatch.setattr(
        glob, "glob",
        lambda pattern: (
            ["/dev/cu.usbmodem14201"] if pattern == "/dev/cu.usbmodem*" else []
        ),
    )
    assert [p["device"] for p in MavlinkBridge.list_serial_ports()] == [
        "/dev/cu.usbmodem14201",
    ]


# ---------------------------------------------------------------------------
# Parameter retransmit does not flood the link it is recovering
# ---------------------------------------------------------------------------

class _RecordingMav:
    def __init__(self) -> None:
        self.reads: list[int] = []

    def param_request_read_send(
        self, target_system: int, target_component: int, name: bytes, index: int,
    ) -> None:
        self.reads.append(index)


class _RecordingConn:
    def __init__(self) -> None:
        self.mav = _RecordingMav()
        self.source_system = 254
        self.source_component = 190

    def close(self) -> None:
        pass


def _priming_for_retransmit(
    b: MavlinkBridge, missing: int, final_round: bool = True,
) -> _RecordingConn:
    """Put *b* in a download that has lost *missing* PARAM_VALUEs.

    With *final_round*, the retransmit budget is already spent but for one
    round, so :meth:`_param_watchdog` issues exactly that round and returns —
    which is what makes "how many requests does ONE round send" observable.
    """
    conn = _RecordingConn()
    b._conn = conn
    b._running.set()
    b._param_download_state = "downloading"
    b._param_count = missing
    b._param_received = 0
    b._param_seen_indices = set()
    b._param_download_started_at = time.monotonic()
    # Far enough in the past that the burst counts as settled.
    b._param_last_value_at = time.monotonic() - 10.0
    if final_round:
        b._param_retransmit_round = PARAM_DOWNLOAD_MAX_ROUNDS - 1
    return conn


def test_a_retransmit_round_is_capped_on_a_serial_link() -> None:
    """A SiK radio must not be handed 1400 requests back to back.

    That is the link on which parameters go missing in the first place, and an
    unbounded round saturated it — pushing the GCS heartbeat out of its window
    and causing more loss than it recovered.
    """
    b = bridge("serial:/dev/ttyUSB0:57600")
    conn = _priming_for_retransmit(b, missing=1400)

    b._param_watchdog()

    assert conn.mav.reads, "the round must actually have been issued"
    assert len(conn.mav.reads) <= PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL
    assert conn.mav.reads == sorted(conn.mav.reads)


def test_retransmit_requests_are_spaced_out(monkeypatch: pytest.MonkeyPatch) -> None:
    """Each request is followed by a pause, so replies interleave with telemetry."""
    b = bridge("serial:/dev/ttyUSB0:57600")
    conn = _priming_for_retransmit(b, missing=10)
    slept: list[float] = []
    monkeypatch.setattr(b, "_interruptible_sleep", lambda s: slept.append(s))

    b._param_watchdog()

    assert len(conn.mav.reads) == 10
    # _param_watchdog's own tick, then one pause per request sent.
    assert len(slept) >= len(conn.mav.reads)
    assert all(gap > 0 for gap in slept[1:])


def test_a_serial_download_is_not_declared_incomplete_at_thirty_seconds() -> None:
    """1400 parameters is ~35 kB; at 57 kbps that cannot finish inside 30 s.

    The single 30 s ceiling marked every serial download incomplete while it
    was still arriving normally.
    """
    b = bridge("serial:/dev/ttyUSB0:57600")
    conn = _priming_for_retransmit(b, missing=1400)
    b._param_download_started_at = time.monotonic() - 45.0

    b._param_watchdog()

    assert conn.mav.reads, "the watchdog should still be recovering, not giving up"


def test_a_udp_download_keeps_its_thirty_second_ceiling() -> None:
    b = bridge("udp:127.0.0.1:14550")
    conn = _priming_for_retransmit(b, missing=1400)
    b._param_download_started_at = time.monotonic() - 45.0

    b._param_watchdog()

    assert b._param_download_state == "incomplete"
    assert conn.mav.reads == [], "a timed-out download must not retransmit"


# ---------------------------------------------------------------------------
# A mission upload cannot park arm / land / RTL behind it
# ---------------------------------------------------------------------------

def test_fly_to_points_refuses_more_points_than_it_can_upload() -> None:
    b = bridge()
    points = [{"lat": 48.0, "lon": 11.0, "alt_agl": 20.0}] * (FLY_TO_MAX_POINTS + 1)

    assert b._validate_fly_to_points(points) is None
    assert "too many points" in b.get_last_command_error()


def test_fly_to_points_accepts_the_maximum() -> None:
    b = bridge()
    points = [{"lat": 48.0, "lon": 11.0, "alt_agl": 20.0}] * FLY_TO_MAX_POINTS

    assert b._validate_fly_to_points(points) is not None


class _CountingConn:
    def __init__(self) -> None:
        self.mav = SimpleNamespace(mission_count_send=lambda *a: None)
        self.source_system = 254
        self.source_component = 190

    def close(self) -> None:
        pass


def test_a_silent_mission_upload_gives_up_on_quiet_not_on_item_count(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A vehicle that stops asking for items ends the upload in seconds.

    The old budget was 2 s per item and was waited out in full, so a handshake
    that died on the first item still held ``_operation_lock`` — and with it
    arm, land and RTL — for as long as the mission was large.
    """
    b = bridge()
    b._conn = _CountingConn()
    items = [{"frame": 3} for _ in range(60)]        # old budget: 125 s

    started = time.monotonic()
    # Pretend the last MISSION_REQUEST was long enough ago to count as quiet.
    original = b._upload_mission

    def _seeded(payload: list[dict[str, Any]]) -> int:
        b._mission_last_request = time.monotonic() - (MISSION_UPLOAD_QUIET_S + 1)
        return original(payload)

    # Seed AFTER _upload_mission resets the clock, by patching the reset away.
    monkeypatch.setattr(
        b, "_upload_mission",
        lambda payload: original(payload),
    )
    result = _run_upload_with_stale_clock(b, items)
    elapsed = time.monotonic() - started

    assert result == -1
    assert elapsed < 5.0, f"upload sat out {elapsed:.1f}s after the vehicle went quiet"


def _run_upload_with_stale_clock(b: MavlinkBridge, items: list[dict[str, Any]]) -> int:
    """Run one upload whose progress clock never advances past the quiet window."""
    done: dict[str, int] = {}

    def _go() -> None:
        done["result"] = b._upload_mission(items)

    thread = threading.Thread(target=_go, daemon=True)
    thread.start()
    # Wind the progress clock back so the upload sees a vehicle that has gone
    # quiet rather than one that simply has not started yet.
    deadline = time.monotonic() + 3.0
    while thread.is_alive() and time.monotonic() < deadline:
        with b._mission_lock:
            if b._pending_mission_ack is not None:
                b._mission_last_request = (
                    time.monotonic() - (MISSION_UPLOAD_QUIET_S + 1)
                )
        time.sleep(0.02)
    thread.join(timeout=3.0)
    return done.get("result", 0)


def test_a_torn_down_link_wakes_a_waiting_mission_upload() -> None:
    """Shutdown must not leave the uploader parked on its own timeout."""
    from corvus.mavlink_bridge import _PendingAck

    b = bridge()
    pending = _PendingAck()
    with b._mission_lock:
        b._pending_mission_ack = pending
        b._mission_items = None          # already cleared by the upload's finally

    b._cancel_pending_commands()

    assert pending.event.is_set()
    assert pending.result == -2


# ---------------------------------------------------------------------------
# set_param does not report a write the link never carried
# ---------------------------------------------------------------------------

def test_a_link_lost_mid_write_is_not_reported_as_a_confirmed_write(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The waiter is woken by an echo and by a teardown alike; only one is a write.

    Taking the second for the first meant a cable pulled mid-write reported the
    parameter as set whenever the stale cache still held the value being
    written — exactly the case where nothing reached the vehicle at all.
    """
    from corvus.mavlink_bridge import ParamEntry

    b = bridge()
    b._store.update(connected=True, armed=False)
    b._store.heartbeat()
    sent = threading.Event()

    class _Mav:
        def param_set_send(self, *args: Any) -> None:
            sent.set()

    class _Conn:
        mav = _Mav()

        def close(self) -> None:
            pass

    b._conn = _Conn()
    b._params["MPC_XY_P"] = ParamEntry("MPC_XY_P", 0.95, 9, 0, 1)

    def _drop_the_link() -> None:
        sent.wait(2.0)
        b._abort_parameter_operations()

    threading.Thread(target=_drop_the_link, daemon=True).start()

    # Writing the value the cache already holds: the echo check alone would
    # have passed on the stale entry.
    assert b.set_param("MPC_XY_P", 0.95) is False
    assert "not connected" in b.get_last_command_error()


# ---------------------------------------------------------------------------
# MAVLink signing is applied, not merely parsed
# ---------------------------------------------------------------------------

class _SigningConn:
    def __init__(self) -> None:
        self.signing: dict[str, Any] = {}

    def setup_signing(self, key: bytes, sign_outgoing: bool = True,
                      allow_unsigned_callback: Any = None) -> None:
        self.signing = {
            "key": key,
            "sign_outgoing": sign_outgoing,
            "allow_unsigned_callback": allow_unsigned_callback,
        }

    def close(self) -> None:
        pass


def _write_key(tmp_path: Any, data: bytes, mode: int = 0o600) -> str:
    path = tmp_path / "signing.key"
    path.write_bytes(data)
    os.chmod(path, mode)
    return str(path)


def test_a_configured_signing_key_is_actually_applied_to_the_link(
    tmp_path: Any, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The key was loaded and validated, then never handed to pymavlink.

    An operator who set the variable believed the link was signed. It was not,
    and nothing said so.
    """
    key = bytes(range(32))
    monkeypatch.setenv("CORVUS_MAVLINK_SIGNING_KEY_FILE", _write_key(tmp_path, key))
    b = bridge()
    conn = _SigningConn()
    b._conn = conn

    b._apply_signing()

    assert conn.signing["key"] == key
    assert conn.signing["sign_outgoing"] is True
    assert conn.signing["allow_unsigned_callback"] is not None


def test_signing_exempts_only_the_messages_that_cannot_be_signed(
    tmp_path: Any, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The SiK modem and ADS-B transponders hold no key; nothing else is exempt."""
    monkeypatch.setenv(
        "CORVUS_MAVLINK_SIGNING_KEY_FILE", _write_key(tmp_path, bytes(range(32))),
    )
    b = bridge()
    conn = _SigningConn()
    b._conn = conn
    b._apply_signing()
    allow = conn.signing["allow_unsigned_callback"]

    assert allow(None, mavutil.mavlink.MAVLINK_MSG_ID_RADIO_STATUS) is True
    assert allow(None, mavutil.mavlink.MAVLINK_MSG_ID_ADSB_VEHICLE) is True
    # The ones that matter: an unsigned command or heartbeat is refused.
    assert allow(None, mavutil.mavlink.MAVLINK_MSG_ID_COMMAND_LONG) is False
    assert allow(None, mavutil.mavlink.MAVLINK_MSG_ID_HEARTBEAT) is False
    assert allow(None, mavutil.mavlink.MAVLINK_MSG_ID_PARAM_VALUE) is False


def test_no_key_configured_leaves_the_link_untouched(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("CORVUS_MAVLINK_SIGNING_KEY_FILE", raising=False)
    b = bridge()
    conn = _SigningConn()
    b._conn = conn

    b._apply_signing()

    assert conn.signing == {}


def test_an_unusable_signing_key_fails_the_connection_rather_than_downgrading(
    tmp_path: Any, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A typo in the path must not silently produce an unsigned link.

    The operator would have no way to tell the difference from the outside.
    """
    monkeypatch.setenv(
        "CORVUS_MAVLINK_SIGNING_KEY_FILE", str(tmp_path / "does-not-exist"),
    )
    b = bridge()
    b._conn = _SigningConn()

    with pytest.raises(ConnectionError):
        b._apply_signing()
    assert "signing key" in b._store.get_snapshot()["link_error"]


def test_a_world_readable_signing_key_is_refused(
    tmp_path: Any, monkeypatch: pytest.MonkeyPatch,
) -> None:
    if os.name == "nt":
        pytest.skip("POSIX file modes only")
    monkeypatch.setenv(
        "CORVUS_MAVLINK_SIGNING_KEY_FILE",
        _write_key(tmp_path, bytes(range(32)), mode=0o644),
    )
    b = bridge()
    b._conn = _SigningConn()

    with pytest.raises(ConnectionError):
        b._apply_signing()


# ---------------------------------------------------------------------------
# A connection string from the config file or the command line is checked
# ---------------------------------------------------------------------------

@pytest.mark.parametrize(
    "conn",
    ["garbage", "", None, 42, "serial:", "http://example.com", "udp"],
)
def test_an_unusable_startup_connection_falls_back_to_the_default(conn: Any) -> None:
    """Only POST /api/mavlink/connect used to validate, because it had a 400 to
    put the answer in. A typo in ~/.corvus/config.json or in argv started the
    app into a reconnect loop that could never succeed — for a bare word,
    pymavlink tried to open it as a serial device."""
    from corvus.server import DEFAULT_MAVLINK_CONNECTION, apply_startup_connection

    b = bridge("serial:/dev/ttyUSB0:57600")

    assert apply_startup_connection(b, conn) == DEFAULT_MAVLINK_CONNECTION
    assert b.connection_string() == DEFAULT_MAVLINK_CONNECTION


@pytest.mark.parametrize(
    "conn",
    [
        "udp:0.0.0.0:14550",
        "udpout:192.168.1.5:14550",
        "tcp:127.0.0.1:5760",
        "serial:/dev/ttyACM0:115200",
        "serial:/dev/cu.usbmodem14201:57600",
        "serial:COM7:57600",
    ],
)
def test_a_usable_startup_connection_is_what_the_bridge_ends_up_on(conn: str) -> None:
    from corvus.server import apply_startup_connection

    b = bridge()

    assert apply_startup_connection(b, conn) == conn
    assert b.connection_string() == conn


def test_the_fallback_says_why_it_fell_back(caplog: Any) -> None:
    """An operator who mistyped the connection has to be able to find out."""
    import logging

    from corvus.server import apply_startup_connection

    with caplog.at_level(logging.ERROR, logger="corvus.server"):
        apply_startup_connection(bridge(), "wifi:drone")

    assert any("wifi:drone" in r.getMessage() for r in caplog.records)


# ---------------------------------------------------------------------------
# An abort command does not queue behind routine work
# ---------------------------------------------------------------------------

def test_an_abort_overtakes_a_queue_of_routine_operations() -> None:
    """RTL must not wait for a parameter upload to finish.

    Every command is serialised through one lock, and a plain lock hands off
    in arrival order — so an operator reaching for RTL mid-upload waited
    behind every remaining write. The one command that must not wait was the
    one waiting.
    """
    from corvus.mavlink_bridge import _PriorityLock

    lock = _PriorityLock()
    order: list[str] = []

    def routine(tag: str) -> None:
        with lock:
            order.append(tag)
            time.sleep(0.05)

    with lock:
        workers = [
            threading.Thread(target=routine, args=(f"routine{i}",), daemon=True)
            for i in range(8)
        ]
        for worker in workers:
            worker.start()
        time.sleep(0.3)          # let the routine queue pile up on the lock

        def abort() -> None:
            with lock.priority():
                order.append("ABORT")

        aborter = threading.Thread(target=abort, daemon=True)
        aborter.start()
        time.sleep(0.3)          # let the abort register as waiting

    aborter.join(timeout=5)
    for worker in workers:
        worker.join(timeout=5)

    assert order.index("ABORT") <= 1, (
        f"the abort waited behind {order.index('ABORT')} routine operations: {order}"
    )
    assert len(order) == 9, "no operation may be lost"


def test_a_reentrant_acquire_never_stands_aside_for_a_waiting_abort() -> None:
    """takeoff() calls arm(); set_mode() calls _send_command_and_wait().

    A thread that already holds the lock must bypass the gate: making it wait
    for a pending abort deadlocks, because that abort is waiting for the lock
    this very thread is holding.
    """
    from corvus.mavlink_bridge import _PriorityLock

    lock = _PriorityLock()
    finished = threading.Event()
    holding = threading.Event()

    def nested_holder() -> None:
        with lock:                       # outer routine
            holding.set()
            time.sleep(0.4)              # an abort registers during this
            with lock:                   # nested routine
                with lock.priority():    # nested abort
                    pass
        finished.set()

    holder = threading.Thread(target=nested_holder, daemon=True)
    holder.start()
    holding.wait(timeout=2)

    aborted = threading.Event()

    def waiting_abort() -> None:
        with lock.priority():
            aborted.set()

    aborter = threading.Thread(target=waiting_abort, daemon=True)
    aborter.start()
    holder.join(timeout=5)
    aborter.join(timeout=5)

    assert finished.is_set(), "re-entrant acquire deadlocked behind a waiting abort"
    assert aborted.is_set(), "the waiting abort never ran"
    assert lock._aborts_waiting == 0, "a nested abort must not register twice"


def test_routine_work_is_not_starved_and_stays_mutually_exclusive() -> None:
    """Standing aside for aborts must not cost the guarantee the lock exists for."""
    from corvus.mavlink_bridge import _PriorityLock

    lock = _PriorityLock()
    completed: list[int] = []
    inside: list[int] = []
    breached: list[str] = []

    def routine() -> None:
        with lock:
            inside.append(1)
            if len(inside) != 1:
                breached.append("two threads held the lock at once")
            time.sleep(0.001)
            inside.pop()
            completed.append(1)

    threads = [threading.Thread(target=routine, daemon=True) for _ in range(40)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=10)

    assert not breached, breached
    assert len(completed) == 40, f"only {len(completed)}/40 completed"


def test_an_exception_inside_an_abort_does_not_wedge_the_lock() -> None:
    """A leaked abort count would block every routine command for good."""
    from corvus.mavlink_bridge import _PriorityLock

    lock = _PriorityLock()

    with pytest.raises(RuntimeError):
        with lock.priority():
            raise RuntimeError("command failed")

    assert lock._aborts_waiting == 0
    ran = threading.Event()

    def routine() -> None:
        with lock:
            ran.set()

    worker = threading.Thread(target=routine, daemon=True)
    worker.start()
    worker.join(timeout=5)
    assert ran.is_set(), "routine work was locked out by a leaked abort count"


def test_the_commands_that_end_a_flight_use_the_priority_tier() -> None:
    """The abort set, pinned so a new command path cannot quietly join the queue."""
    import inspect

    from corvus.mavlink_bridge import MavlinkBridge

    for name in ("land", "rtl", "set_mode", "stop_motor_test", "cancel_calibration"):
        source = inspect.getsource(getattr(MavlinkBridge, name))
        assert "_operation_lock.priority()" in source, (
            f"{name}() is an abort command and must not queue behind routine work"
        )

    # Disarm is priority; arming is ordinary.
    arm_source = inspect.getsource(MavlinkBridge.arm)
    assert "_operation_lock.priority()" in arm_source
    assert "if arm else" in arm_source, "only disarm takes the priority tier"


def test_the_armed_confirmation_wait_happens_outside_the_command_lock() -> None:
    """Six seconds of reading telemetry must not block every other command.

    arm() waits for the vehicle's own report of the new state, which sends
    nothing. Holding the lock across it protected nothing and made arm — and
    takeoff, which nests it — the longest hold in the class.
    """
    b = bridge()
    b._store.update(connected=True, armed=False)
    b._store.heartbeat()

    class _Conn:
        mav = SimpleNamespace(command_long_send=lambda *a: None)
        source_system = 254
        source_component = 190

        def close(self) -> None:
            pass

    b._conn = _Conn()
    # The vehicle accepts at once; the telemetry confirmation is what drags.
    b._send_command_and_wait = lambda *a, **k: mavutil.mavlink.MAV_RESULT_ACCEPTED
    waiting = threading.Event()
    release = threading.Event()

    def slow_confirmation(armed: bool, timeout: float = 0.0) -> bool:
        waiting.set()
        release.wait(3.0)
        return True

    b._wait_for_armed_state = slow_confirmation
    worker = threading.Thread(target=lambda: b.arm(True), daemon=True)
    worker.start()
    assert waiting.wait(timeout=3.0), "arm() never reached its confirmation wait"

    # The real assertion: with arm() parked mid-confirmation, an ordinary
    # command can still take the lock.
    free = threading.Event()

    def other_command() -> None:
        with b._operation_lock:
            free.set()

    threading.Thread(target=other_command, daemon=True).start()
    got_it = free.wait(timeout=2.0)
    release.set()
    worker.join(timeout=3.0)

    assert got_it, (
        "the command lock was held across the armed-state wait, so every "
        "other command was blocked while arm() only read telemetry"
    )
