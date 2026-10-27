"""The Rust connection cycle against the Python one it replaced.

``tests/reference/mavlink_bridge_cycle_python.py`` keeps start and stop, the
connect/receive cycle, connect, the vehicle heartbeat wait, the GCS heartbeat
loop, the interruptible sleep and the two deferred workers as they were in
Python, on ``threading.Thread``. Each test runs a bridge on the reference and
a bridge on ``corvus_link.bridge.LinkCycle`` through the same scripted links
and clocks and compares what each did: every store update, every bridge
method it called, in order, the log lines, the frames sent, the attributes
left behind and the errors raised. The threads differ by design (a
``CoreThread`` against a ``threading.Thread``); what runs on them is compared.
"""
from __future__ import annotations

import errno
import logging
import random
import threading
import time
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

import corvus.mavlink_bridge as mb  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from corvus_link import mavutil  # noqa: E402
from tests.reference import mavlink_bridge_cycle_python as reference  # noqa: E402

M = mavutil.mavlink


class ReferenceBridge(reference.LinkCycleMixin, MavlinkBridge):
    """A bridge whose connection cycle is the Python reference."""


KINDS = (ReferenceBridge, MavlinkBridge)


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", call(*args, **kwargs))
    except Exception as exc:  # noqa: BLE001 - compared below
        cause = exc.__cause__
        return ("error", type(exc).__name__, str(exc), type(cause).__name__ if cause else None)


class Logs(logging.Handler):
    def __init__(self) -> None:
        super().__init__(logging.DEBUG)
        self.lines: list[tuple[str, str]] = []

    def emit(self, record: logging.LogRecord) -> None:
        self.lines.append((record.levelname, record.getMessage()))


@pytest.fixture
def logs() -> Any:
    handler = Logs()
    log = logging.getLogger("corvus.mavlink")
    previous = log.level
    log.addHandler(handler)
    log.setLevel(logging.DEBUG)
    yield handler
    log.removeHandler(handler)
    log.setLevel(previous)


RECORDED = [
    "_reset_shell_state", "_reset_parameter_cache", "_reset_param_metadata", "_clear_prearm_reasons",
    "_forget_link_quality", "_forget_battery_cells", "_forget_gnss_integrity", "_forget_remote_id_session",
    "_forget_vehicle_mission", "_claim_serial_exclusive", "_warn_if_onboard_port", "_apply_signing",
    "_send_gcs_heartbeat", "_start_gcs_heartbeat", "_build_mode_mapping", "_request_streams", "_request_version",
    "_request_home", "_start_tlog", "_stop_tlog", "_cancel_pending_commands", "_abort_parameter_operations",
    "_stop_param_metadata", "stop_shell", "_schedule_message_intervals", "_request_message_intervals",
    "_check_gcs_authority", "_send_remote_id",
]


def make(kind: type, conn_str: str = "udp:127.0.0.1:14550", *, stub: tuple[str, ...] = (),
         raising: dict[str, BaseException] | None = None) -> tuple[Any, list]:
    store = VehicleStateStore()
    bridge = kind(store, conn_str)
    log: list = []
    original = store.update

    def update(**fields: Any) -> None:
        log.append(("store", fields))
        original(**fields)

    store.update = update  # type: ignore[method-assign]
    for name in RECORDED:
        real = getattr(bridge, name)

        def record(*args: Any, _name: str = name, _real: Any = real, **kwargs: Any) -> Any:
            log.append(("call", _name, [getattr(a, "label", type(a).__name__) for a in args]))
            if raising and _name in raising:
                raise raising[_name]
            if _name in stub or _name in ("_start_gcs_heartbeat", "_start_tlog", "_apply_signing",
                                          "_claim_serial_exclusive", "_build_mode_mapping", "_request_streams",
                                          "_request_version", "_request_home", "_send_gcs_heartbeat"):
                return None
            return _real(*args, **kwargs)

        setattr(bridge, name, record)
    bridge._console_publish = lambda name, text, level: log.append(("console", name, text, level))
    return bridge, log


def attributes(bridge: Any) -> list:
    names = ["_target_system", "_target_component", "_ignored_systems", "_mav_type_id", "_request_sent",
             "_firmware_version", "_home_alt_amsl", "_tlog_armed", "_reconnect_attempt", "_degraded_warned"]
    conn = bridge._conn
    pinned = None if conn is None else (getattr(conn, "target_system", None), getattr(conn, "target_component", None),
                                        getattr(conn, "sysid", None), getattr(conn, "closed", None))
    return [(n, getattr(bridge, n)) for n in names] + [("conn", pinned), ("label", bridge._dialect.label)]


# ------------------------------------------------------------------ connect

class Heartbeat(SimpleNamespace):
    def get_type(self) -> str:
        return "HEARTBEAT"

    def get_srcSystem(self) -> Any:  # noqa: N802 - MAVLink's spelling
        return self._sys

    def get_srcComponent(self) -> Any:  # noqa: N802
        return self._comp


def heartbeat(sys_id: int = 1, comp: int = 1, kind: int = 2, autopilot: int = 12, base_mode: int = 0,
              custom_mode: int = 0) -> Heartbeat:
    return Heartbeat(_sys=sys_id, _comp=comp, type=kind, autopilot=autopilot, base_mode=base_mode,
                     custom_mode=custom_mode, system_status=4, mavlink_version=3)


class FakeLink:
    def __init__(self, beats: list, clock: dict) -> None:
        self.beats = list(beats)
        self.clock = clock
        self.target_system = 1
        self.target_component = 1
        self.sysid_state = {1: object(), 7: object()}
        self.sysid = 0
        self.closed = False
        self.mav = SimpleNamespace()

    def wait_heartbeat(self, blocking: bool = True, timeout: float = 1.0) -> Any:
        self.clock["now"] += timeout if not self.beats or self.beats[0] is None else 0.1
        if not self.beats:
            return None
        beat = self.beats.pop(0)
        if beat == "instant-none":
            return None
        return beat

    def close(self) -> None:
        self.closed = True


CONNECTS: dict[str, dict[str, Any]] = {
    "px4": {"beats": [heartbeat()]},
    "ardupilot-plane": {"beats": [heartbeat(kind=1, autopilot=3, base_mode=129, custom_mode=10)]},
    "gcs-first": {"beats": [heartbeat(sys_id=255, comp=190, kind=6, autopilot=8), heartbeat(sys_id=7, comp=1)]},
    "gimbal-first": {"beats": [heartbeat(comp=154, kind=26, autopilot=8), heartbeat()]},
    "silent": {"beats": []},
    "instant-none": {"beats": ["instant-none"]},
    "source-zero": {"beats": [heartbeat(sys_id=0, comp=0)]},
    "other-vehicle": {"beats": [heartbeat(sys_id=7)], "previous": "main:3"},
    "same-vehicle": {"beats": [heartbeat()], "previous": "main:1"},
    "serial": {"beats": [heartbeat()], "conn": "serial:/dev/ttyACM0:115200"},
    "open-busy": {"open": OSError(errno.EADDRINUSE, "Address already in use")},
    "open-busy-serial": {"open": OSError(errno.EBUSY, "busy"), "conn": "serial:/dev/ttyUSB0:57600"},
    "open-missing-serial": {"open": FileNotFoundError(2, "No such device"), "conn": "serial:/dev/ttyUSB9:57600"},
    "open-refused": {"open": ConnectionRefusedError(61, "refused"), "conn": "tcp:127.0.0.1:5760"},
    "open-other": {"open": ValueError("bad spec")},
    "superseded": {"superseded": True},
    "superseded-serial": {"superseded": True, "conn": "serial:/dev/ttyACM0:115200"},
    "signing-fails": {"beats": [heartbeat()], "raising": {"_apply_signing": ConnectionError("key unusable")}},
}


@pytest.mark.parametrize("case", list(CONNECTS))
def test_connect_matches(case: str, logs: Logs, monkeypatch: pytest.MonkeyPatch) -> None:
    spec = CONNECTS[case]
    clock = {"now": 100.0}
    monkeypatch.setattr(time, "monotonic", lambda: clock["now"])
    results = []
    for kind in KINDS:
        clock["now"] = 100.0
        logs.lines.clear()
        bridge, log = make(kind, spec.get("conn", "udp:127.0.0.1:14550"), raising=spec.get("raising"))
        if spec.get("previous"):
            bridge._store.update(vehicle_id=spec["previous"], connected=True)
        opened: list = []

        def open_link(*args: Any, _bridge: Any = bridge, _opened: list = opened, **kwargs: Any) -> Any:
            _opened.append((args, sorted(kwargs.items())))
            if "open" in spec:
                raise spec["open"]
            if spec.get("superseded"):
                _bridge._run_generation += 1
            return FakeLink(spec.get("beats", []), clock)

        monkeypatch.setattr(mb.mavutil, "mavlink_connection", open_link)
        if spec.get("superseded"):
            bridge._cycle.generation = bridge._run_generation
        try:
            result = outcome(bridge._connect)
        finally:
            bridge._cycle.generation = None
        results.append((result, opened, log, logs.lines[:], attributes(bridge),
                        bridge._store.get_snapshot().get("link_error")))
    assert results[1] == results[0]


# ------------------------------------------------------------ the cycle

CYCLES: dict[str, list[Any]] = {
    "connects-then-drops": ["ok", ConnectionError("heartbeat timeout"), "stop"],
    "fails-to-connect": [ConnectionError("No heartbeat received"), ConnectionError("No heartbeat received"), "stop"],
    "clean-end": ["ok", None, "stop"],
    "superseded": ["supersede"],
    "error-after-stop": ["stopped-error"],
    "keyboard": [KeyboardInterrupt()],
}


@pytest.mark.parametrize("case", list(CYCLES))
def test_run_cycles_match(case: str, logs: Logs, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(random, "uniform", lambda a, b: b / 2)
    results = []
    for kind in KINDS:
        logs.lines.clear()
        bridge, log = make(kind, stub=("_schedule_message_intervals", "_cancel_pending_commands",
                                       "_abort_parameter_operations", "_reset_shell_state", "_stop_tlog"))
        script = list(CYCLES[case])
        own = SimpleNamespace(closed=False)
        own.close = lambda own=own: setattr(own, "closed", True)

        def connect(_bridge: Any = bridge, _log: list = log, _script: list = script, _own: Any = own) -> None:
            step = _script.pop(0) if _script else "stop"
            _log.append(("connect", repr(step)))
            if step == "stop":
                _bridge._running.clear()
                raise ConnectionError("stopped")
            if step == "supersede":
                _bridge._cycle.conn = _own
                _bridge._run_generation += 1
                raise ConnectionError("replaced")
            if step == "stopped-error":
                _bridge._running.clear()
                raise OSError("closed under us")
            if isinstance(step, BaseException):
                raise step
            _bridge._conn = SimpleNamespace(close=lambda: _log.append(("close",)))

        def receive(_bridge: Any = bridge, _log: list = log, _script: list = script) -> None:
            step = _script.pop(0) if _script else None
            _log.append(("receive", repr(step)))
            if isinstance(step, BaseException):
                raise step

        bridge._connect = connect
        bridge._receive_loop = receive
        bridge._interruptible_sleep = lambda s, _log=log: _log.append(("sleep", s))
        bridge._running.set()
        bridge._run_generation = 3
        result = outcome(bridge._run, 3) if case != "keyboard" else None
        if case == "keyboard":
            with pytest.raises(KeyboardInterrupt):
                bridge._run(3)
        results.append((result, log, logs.lines[:], bridge._reconnect_attempt, own.closed,
                        bridge._store.get_snapshot().get("link_status"),
                        bridge._store.get_snapshot().get("link_error")))
        bridge._cycle.generation = None
    assert results[1] == results[0]


@pytest.mark.parametrize("attempt", [-3, 0, 1, 2, 3, 4, 5, 6, 10, 60, 1023, 1024])
@pytest.mark.parametrize("jitter", [-0.25, 0.0, 0.25])
def test_reconnect_delays_match(attempt: int, jitter: float, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(random, "uniform", lambda a, b: jitter)
    results = [outcome(kind(VehicleStateStore())._reconnect_delay, attempt) for kind in KINDS]
    assert results[1] == results[0]


@pytest.mark.parametrize("attempt", [1025, 5000])
def test_the_backoff_no_longer_overflows_where_the_python_did(attempt: int, monkeypatch: pytest.MonkeyPatch) -> None:
    """The one place the engine departs from the reference on purpose: from
    attempt 1025 the Python raised OverflowError and the link thread died;
    the engine stays at the cap. See test_mavlink_serial for the regression."""
    monkeypatch.setattr(random, "uniform", lambda a, b: 0.0)
    assert outcome(ReferenceBridge(VehicleStateStore())._reconnect_delay, attempt)[:2] == ("error", "OverflowError")
    assert MavlinkBridge(VehicleStateStore())._reconnect_delay(attempt) == 8.0


def test_a_superseded_thread_never_connects(monkeypatch: pytest.MonkeyPatch) -> None:
    for kind in KINDS:
        bridge = kind(VehicleStateStore(), "udp:127.0.0.1:0")
        monkeypatch.setattr(bridge, "_connect", lambda: pytest.fail("an old cycle connected"))
        bridge._running.set()
        bridge._run_generation = 5
        bridge._run(4)
        bridge._running.clear()


# ---------------------------------------------------- heartbeat wait

WAITS: dict[str, list[Any]] = {
    "first": [heartbeat()],
    "after-gcs": [heartbeat(sys_id=255, comp=190, kind=6, autopilot=8), None, heartbeat(sys_id=4)],
    "nothing": [],
    "instant-none": ["instant-none"],
    "only-peripherals": [heartbeat(comp=154, kind=26, autopilot=8)] * 30,
}


@pytest.mark.parametrize("case", list(WAITS))
@pytest.mark.parametrize("timeout", [0.0, 0.5, 2.5, 10.0])
@pytest.mark.parametrize("stopping", [False, True])
def test_heartbeat_waits_match(case: str, timeout: float, stopping: bool, logs: Logs,
                               monkeypatch: pytest.MonkeyPatch) -> None:
    clock = {"now": 100.0}
    monkeypatch.setattr(time, "monotonic", lambda: clock["now"])
    results = []
    for kind in KINDS:
        clock["now"] = 100.0
        logs.lines.clear()
        bridge = kind(VehicleStateStore())
        link = FakeLink(WAITS[case], clock)
        bridge._conn = link
        if stopping:
            bridge._stop_event.set()
        hb = outcome(bridge._wait_vehicle_heartbeat, timeout)
        results.append((hb[0], getattr(hb[1], "_sys", hb[1]), clock["now"], len(link.beats), logs.lines[:]))
    assert results[1] == results[0]


# ------------------------------------------------- the GCS heartbeat

class HbLink:
    def __init__(self, fail: set[int], log: list) -> None:
        self.calls = 0
        self.fail = fail
        self.log = log
        self.mav = SimpleNamespace(heartbeat_send=self.send)

    def send(self, *args: Any) -> None:
        self.calls += 1
        self.log.append(("heartbeat", args))
        if self.calls in self.fail:
            raise OSError("send on closed socket")


@pytest.mark.parametrize("fail", [set(), {2, 3}, {1, 2, 3, 4, 5, 6}])
@pytest.mark.parametrize("known", [False, True])
@pytest.mark.parametrize("swap_at", [0, 3])
def test_gcs_heartbeat_loops_match(fail: set[int], known: bool, swap_at: int) -> None:
    results = []
    for kind in KINDS:
        bridge, log = make(kind, stub=("_send_remote_id",))
        bridge._store.update(connected=known)
        bridge._conn = HbLink(fail, log)
        naps: list[float] = []

        def nap(seconds: float, _bridge: Any = bridge, _naps: list = naps, _log: list = log) -> None:
            _naps.append(seconds)
            if len(_naps) == swap_at:
                _bridge._conn = HbLink(set(), _log)
            if len(_naps) >= 8:
                _bridge._running.clear()

        bridge._interruptible_sleep = nap
        bridge._running.set()
        bridge._hb_thread = "someone else"
        bridge._gcs_hb_loop()
        results.append((log, naps, bridge._hb_thread))
    assert results[1] == results[0]


@pytest.mark.parametrize("swapped", [False, True])
@pytest.mark.parametrize("fails", [False, True])
def test_single_gcs_heartbeats_match(swapped: bool, fails: bool, logs: Logs) -> None:
    results = []
    for kind in KINDS:
        logs.lines.clear()
        bridge = kind(VehicleStateStore())
        log: list = []
        link = HbLink({1} if fails else set(), log)
        bridge._conn = SimpleNamespace() if swapped else link
        results.append((bridge._send_gcs_heartbeat(link), bridge._send_gcs_heartbeat(None), log, logs.lines[:]))
    assert results[1] == results[0]


@pytest.mark.parametrize("seconds", [0, 0.05, 0.1, 0.25, 1, 4.0])
@pytest.mark.parametrize("stop_after", [None, 0, 2])
def test_interruptible_sleeps_match(seconds: float, stop_after: int | None, monkeypatch: pytest.MonkeyPatch) -> None:
    results = []
    for kind in KINDS:
        bridge = kind(VehicleStateStore())
        slices: list[float] = []

        def sleep(s: float, _bridge: Any = bridge, _slices: list = slices) -> None:
            _slices.append(round(s, 9))
            if stop_after is not None and len(_slices) >= stop_after:
                _bridge._stop_event.set()

        monkeypatch.setattr(time, "sleep", sleep)
        if stop_after == 0:
            bridge._stop_event.set()
        bridge._interruptible_sleep(seconds)
        monkeypatch.undo()
        results.append(slices)
    assert results[1] == results[0]


# --------------------------------------------------------- the workers

@pytest.mark.parametrize("running", [True, False])
@pytest.mark.parametrize("raising", [None, "_request_message_intervals", "_check_gcs_authority"])
def test_the_interval_worker_matches(running: bool, raising: str | None, logs: Logs) -> None:
    results = []
    for kind in KINDS:
        logs.lines.clear()
        bridge, log = make(kind, raising={raising: RuntimeError("no ack")} if raising else None,
                           stub=("_request_message_intervals", "_check_gcs_authority"))
        bridge._conn = SimpleNamespace()
        if running:
            bridge._running.set()
        bridge._interruptible_sleep = lambda s, _log=log: _log.append(("sleep", s))
        bridge._schedule_message_intervals()
        worker = bridge._intervals_thread
        worker.join(timeout=5)
        assert not worker.is_alive()
        bridge._schedule_message_intervals()
        bridge._intervals_thread.join(timeout=5)
        results.append((log, logs.lines[:], worker.name))
    assert results[1] == results[0]


@pytest.mark.parametrize("case", ["no-version", "has-version", "not-running", "sends-fail", "conn-gone"])
def test_the_version_retry_matches(case: str) -> None:
    results = []
    for kind in KINDS:
        bridge, log = make(kind)
        sent: list = []

        def send(name: str, _sent: list = sent) -> Any:
            def call(*args: Any) -> None:
                _sent.append((name, [str(a) for a in args]))
                if case == "sends-fail":
                    raise OSError("radio gone")
            return call

        bridge._conn = SimpleNamespace(mav=SimpleNamespace(command_long_send=send("command_long"),
                                                           autopilot_version_request_send=send("request")))
        if case != "not-running":
            bridge._running.set()
        if case == "has-version":
            bridge._store.update(px4_version="v1.18.0")

        def nap(seconds: float, _bridge: Any = bridge) -> None:
            if case == "conn-gone":
                _bridge._conn = None

        bridge._interruptible_sleep = nap
        bridge._schedule_version_retry()
        worker = bridge._version_retry_thread
        if worker is not None:
            worker.join(timeout=5)
            assert not worker.is_alive()
        results.append((sent, worker is None))
    assert results[1] == results[0]


# ------------------------------------------------------- start and stop

class Handle:
    def __init__(self, name: str, log: list, alive: bool = True) -> None:
        self.name = name
        self.log = log
        self.alive = alive

    def is_alive(self) -> bool:
        return self.alive

    def join(self, timeout: float | None = None) -> None:
        self.log.append(("join", self.name, timeout))


@pytest.mark.parametrize("with_conn", [False, True])
@pytest.mark.parametrize("shell_raises", [False, True])
def test_stop_tears_down_in_the_same_order(with_conn: bool, shell_raises: bool) -> None:
    results = []
    for kind in KINDS:
        bridge, log = make(kind, stub=("_stop_tlog", "_cancel_pending_commands", "_abort_parameter_operations",
                                       "_stop_param_metadata", "stop_shell"),
                           raising={"stop_shell": RuntimeError("shell gone")} if shell_raises else None)
        for attr in ("_param_upload_thread", "_param_watchdog_thread", "_version_retry_thread",
                     "_intervals_thread", "_thread", "_hb_thread"):
            setattr(bridge, attr, Handle(attr, log))
        if with_conn:
            bridge._conn = SimpleNamespace(close=lambda _log=log: _log.append(("close",)))
        bridge._running.set()
        outcome_ = outcome(bridge.stop)
        left = [attr for attr in ("_param_upload_thread", "_param_watchdog_thread", "_version_retry_thread",
                                  "_intervals_thread", "_thread", "_hb_thread") if getattr(bridge, attr) is not None]
        results.append((outcome_, log, left, bridge._conn, bridge._running.is_set(), bridge._stop_event.is_set()))
    assert results[1] == results[0]


def test_start_is_idempotent_and_stop_ends_every_thread(monkeypatch: pytest.MonkeyPatch) -> None:
    """A real start against a port that will not open: the cycle retries until
    stop(), and stop() leaves no link thread running."""
    def refuse(*_a: Any, **_k: Any) -> Any:
        raise OSError(errno.EADDRINUSE, "Address already in use")

    monkeypatch.setattr(mb.mavutil, "mavlink_connection", refuse)
    results = []
    for kind in KINDS:
        bridge = kind(VehicleStateStore(), "udp:127.0.0.1:14599")
        bridge.start()
        first = bridge._thread
        bridge.start()
        same = bridge._thread is first
        deadline = time.monotonic() + 3
        while bridge._store.get_snapshot().get("link_status") != "reconnecting" and time.monotonic() < deadline:
            time.sleep(0.02)
        bridge.stop()
        workers = [bridge._thread, bridge._hb_thread, bridge._intervals_thread, bridge._version_retry_thread]
        results.append((same, first.name, bridge._store.get_snapshot().get("link_status"),
                        [w is not None and w.is_alive() for w in workers]))
    assert results[1] == results[0]
    assert results[0][1] == "mavlink" and not any(results[0][3])
    names = {t.name for t in threading.enumerate()}
    assert not names & {"mavlink", "gcs-hb", "mavlink-intervals", "px4-ver-retry"}
