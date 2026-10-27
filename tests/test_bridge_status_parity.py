"""The Rust status engine against the Python it replaced.

``tests/reference/mavlink_bridge_status_python.py`` keeps STATUSTEXT
reassembly and publishing, the vehicle events (the kill switch above all) and
the preflight report requests as they were in Python. Each test runs a bridge
on the reference and a bridge on ``corvus_link.bridge.StatusEngine`` through
the same lines (whole and chunked, in and out of order, from several
sources, as bytes and as text, broken and odd) and the same SYS_STATUS
verdicts, on one fake clock, and compares every console line, store update,
warning, resolved warning, request sent and bridge attribute.
"""
from __future__ import annotations

import random
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

import corvus.mavlink_bridge as rust_side  # noqa: E402
import corvus.state_store as state_store  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from corvus_link import mavutil  # noqa: E402
from tests.reference import mavlink_bridge_status_python as reference  # noqa: E402

M = mavutil.mavlink


class ReferenceBridge(reference.StatusMixin, MavlinkBridge):
    """A bridge whose status reporting is the Python reference."""


KINDS = (ReferenceBridge, MavlinkBridge)


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", call(*args, **kwargs))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


class Clock:
    def __init__(self) -> None:
        self.now = 1000.0

    def monotonic(self) -> float:
        return self.now

    def time(self) -> float:
        return 1_760_000_000.0 + self.now

    def sleep(self, seconds: float) -> None:
        self.now += seconds

    def strftime(self, fmt: str, *_: Any) -> str:
        return "12:00:00"


@pytest.fixture
def clock(monkeypatch: pytest.MonkeyPatch) -> Clock:
    fake = Clock()
    for module in (reference, rust_side, state_store):
        monkeypatch.setattr(module, "time", fake)
    return fake


class Line(SimpleNamespace):
    def get_type(self) -> str:
        return "STATUSTEXT"

    def get_srcSystem(self) -> Any:  # noqa: N802 - MAVLink's spelling
        return self._sys

    def get_srcComponent(self) -> Any:  # noqa: N802
        return self._comp


PX4_LINES = [
    "Kill engaged", "Kill disengaged", "Kill-switch engaged", "kill switch disengaged\t",
    "Preflight Fail: Kill switch engaged", "Preflight Fail: Accel Sensor 0 missing",
    "Preflight Fail: No connection to the GCS", "Disarmed by kill-switch", "Disarmed by landing",
    "Disarmed by external command", "[logger] file: /fs/microsd/log/x.ulg", "Takeoff detected",
    "Landing detected", "Armed by external command", "",
]
AP_LINES = [
    "RC7: MotorEStop HIGH", "RC7: MotorEStop LOW", "PreArm: Motors Emergency Stopped",
    "PreArm: Battery 1 below minimum arming voltage", "Arm: Throttle too high", "EKF3 IMU0 is using GPS",
    "PreArm: Motors: Check frame class and type", "Field Elevation Set: 512m",
]


def chunked(rng: random.Random, text: str, sys_id: int, comp: int, chunk_id: int, severity: int,
            as_bytes: bool) -> list[Line]:
    """One long line as MAVLink 2 STATUSTEXT chunks of 50 bytes."""
    raw = text.encode()
    pieces = [raw[i:i + 50] for i in range(0, max(len(raw), 1), 50)]
    if len(pieces[-1]) == 50 and rng.random() < 0.5:
        pieces.append(b"")
    out = []
    for seq, piece in enumerate(pieces):
        body: Any = piece + (b"\x00" if len(piece) < 50 and rng.random() < 0.5 else b"")
        if not as_bytes:
            body = body.decode("utf-8", "replace")
        out.append(Line(_sys=sys_id, _comp=comp, text=body, severity=severity, id=chunk_id, chunk_seq=seq))
    return out


def random_lines(rng: random.Random, stack: str) -> list[Any]:
    pool = PX4_LINES if stack == "px4" else AP_LINES
    out: list[Any] = []
    for _ in range(rng.randint(1, 3)):
        roll = rng.random()
        severity = rng.choice([0, 2, 3, 4, 5, 6, 7])
        if roll < 0.55:
            text = rng.choice(pool)
            body: Any = text if rng.random() < 0.5 else text.encode() + b"\x00" * rng.randint(0, 3)
            out.append(Line(_sys=1, _comp=1, text=body, severity=severity, id=0))
        elif roll < 0.85:
            text = rng.choice(pool) + " " + "x" * rng.randint(30, 140) + rng.choice(["", " ü€", "\t"])
            chunks = chunked(rng, text, rng.choice([1, 1, 7]), rng.choice([1, 191]), rng.randint(1, 90),
                             severity, rng.random() < 0.6)
            if rng.random() < 0.3:
                rng.shuffle(chunks)
            if rng.random() < 0.15 and len(chunks) > 1:
                chunks.pop(rng.randrange(len(chunks)))
            if rng.random() < 0.1:
                chunks.append(chunks[0])
            out.extend(chunks)
        elif roll < 0.95:
            out.append(Line(_sys=1, _comp=1, text=rng.choice([b"\xff\xfe bad \xe2\x82", "a\ud800b", 42, None]),
                            severity=severity, id=rng.choice([0, 3]), chunk_seq=0))
        else:
            out.append(Line(_sys=1, _comp=1, text="odd", severity=rng.choice(["x", None, 2.5, True]), id=0))
    return out


def make(kind: type, stack: str) -> tuple[Any, list]:
    store = VehicleStateStore()
    store.heartbeat()
    store.update(connected=True)
    bridge = kind(store)
    bridge._target_system = 1
    bridge._target_component = 1
    autopilot = M.MAV_AUTOPILOT_PX4 if stack == "px4" else M.MAV_AUTOPILOT_ARDUPILOTMEGA
    bridge._latch_dialect(autopilot, M.MAV_TYPE_QUADROTOR)
    log: list = []

    class Mav:
        def command_long_send(self, *args: Any) -> None:
            log.append(("send", [str(a) for a in args]))

    bridge._conn = SimpleNamespace(mav=Mav())
    original_update = store.update
    original_resolve = store.resolve_warnings
    original_merge = store.merge_warning

    def update(**fields: Any) -> None:
        log.append(("store", fields))
        original_update(**fields)

    def resolve(events: Any) -> Any:
        log.append(("resolve", sorted(events)))
        return original_resolve(events)

    def merge(text: str, level: str, *args: Any, **kwargs: Any) -> Any:
        log.append(("warning", text, level, kwargs.get("event")))
        return original_merge(text, level, *args, **kwargs)

    store.update = update  # type: ignore[method-assign]
    store.resolve_warnings = resolve  # type: ignore[method-assign]
    store.merge_warning = merge  # type: ignore[method-assign]
    bridge._console_publish = lambda name, text, level: log.append(("console", name, text, level))
    return bridge, log


ATTRS = ["_prearm_reasons", "_prearm_reason_at", "_prearm_requested_at", "_kill_evidence_at", "_prearm_last",
         "_prearm_report_started_at", "_prearm_report_kill"]


def attrs(bridge: Any) -> list:
    chunks = sorted((k, v["final_seq"], sorted(v["parts"]), v["severity"]) for k, v in bridge._statustext_chunks.items())
    snap = bridge._store.get_snapshot()
    return ([(n, getattr(bridge, n)) for n in ATTRS] + [("chunks", chunks)]
            + [(k, snap.get(k)) for k in ("kill_switch", "flight_termination", "prearm_reasons")])


@pytest.mark.parametrize("seed", range(20))
@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
def test_a_session_of_lines_and_verdicts_matches(seed: int, stack: str, clock: Clock) -> None:
    results = []
    for kind in KINDS:
        clock.now = 1000.0
        bridge, log = make(kind, stack)
        rng = random.Random(seed)
        for step in range(250):
            clock.now += rng.choice([0.05, 0.3, 1.0, 2.5, 4.0, 11.0])
            roll = rng.random()
            if roll < 0.55:
                for line in random_lines(rng, stack):
                    log.append(("line", step, outcome(bridge._handle_statustext, line)))
            elif roll < 0.85:
                verdict = rng.choice([True, True, False, False, False, None])
                log.append(("prearm", step, verdict, outcome(bridge._note_prearm_state, verdict)))
            elif roll < 0.92:
                bridge._store.update(armed=rng.random() < 0.3)
            elif roll < 0.96:
                bridge._store.update(kill_switch=rng.random() < 0.5)
            else:
                log.append(("cleanup", step, outcome(bridge._cleanup_statustext_chunks)))
            log.append(("attrs", attrs(bridge)))
        results.append(log)
    assert results[1] == results[0]


def test_the_sessions_reach_every_path(clock: Clock) -> None:
    """The comparison above is only as good as what it exercises."""
    seen: set[str] = set()
    for stack in ("px4", "ardupilot"):
        for seed in range(20):
            bridge, log = make(MavlinkBridge, stack)
            rng = random.Random(seed)
            for _ in range(250):
                clock.now += rng.choice([0.05, 0.3, 1.0, 2.5, 4.0, 11.0])
                roll = rng.random()
                if roll < 0.55:
                    for line in random_lines(rng, stack):
                        outcome(bridge._handle_statustext, line)
                elif roll < 0.85:
                    outcome(bridge._note_prearm_state, rng.choice([True, True, False, False, False, None]))
                elif roll < 0.92:
                    bridge._store.update(armed=rng.random() < 0.3)
                elif roll < 0.96:
                    bridge._store.update(kill_switch=rng.random() < 0.5)
            for entry in log:
                if entry[0] == "warning":
                    seen.add(f"warning:{entry[3]}")
                elif entry[0] == "send":
                    seen.add("report requested")
                elif entry[0] == "resolve":
                    seen.add(f"resolve:{entry[1][0]}")
                elif entry[0] == "store" and "kill_switch" in entry[1]:
                    seen.add(f"kill_switch={entry[1]['kill_switch']}")
                elif entry[0] == "store" and entry[1].get("prearm_reasons"):
                    seen.add("reasons")
    for path in ("warning:kill", "warning:unkill", "warning:prearm", "warning:None", "report requested",
                 "resolve:kill", "resolve:prearm", "kill_switch=True", "kill_switch=False", "reasons"):
        assert path in seen, path


@pytest.mark.parametrize("partials", [63, 64, 65, 80])
def test_too_many_open_partials_evict_alike(partials: int, clock: Clock) -> None:
    results = []
    for kind in KINDS:
        clock.now = 1000.0
        bridge, log = make(kind, "px4")
        for chunk_id in range(1, partials + 1):
            clock.now += 0.01 if chunk_id % 7 else -0.005
            bridge._handle_statustext(Line(_sys=1, _comp=1, text="y" * 50, severity=6, id=chunk_id, chunk_seq=0))
        bridge._handle_statustext(Line(_sys=1, _comp=1, text="end", severity=6, id=5, chunk_seq=1))
        results.append((log, sorted(bridge._statustext_chunks)))
    assert results[1] == results[0]


@pytest.mark.parametrize("fails", [False, True])
@pytest.mark.parametrize("swapped", [False, True])
def test_report_requests_match(fails: bool, swapped: bool, clock: Clock) -> None:
    results = []
    for kind in KINDS:
        bridge, log = make(kind, "px4")
        conn = bridge._conn
        if fails:
            conn.mav.command_long_send = lambda *a: (_ for _ in ()).throw(OSError("radio gone"))
        if swapped:
            bridge._conn = SimpleNamespace()
        outcome(bridge._request_prearm_report)
        bridge._conn = None
        outcome(bridge._request_prearm_report)
        results.append(log)
    assert results[1] == results[0]
