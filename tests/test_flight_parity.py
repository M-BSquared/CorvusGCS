"""The Rust flight engine against the Python flight commands it replaced.

``tests/reference/mavlink_flight_python.py`` keeps the flight commands and the
mission sequences (fly to points, mission start, reposition) as they were in
Python. Each test plays the same random sessions against a bridge on the
reference and a bridge on ``corvus_link.flight.FlightEngine``: a scripted
vehicle answers every command, accepts or refuses, confirms the new state in
telemetry or does not, an operator abort lands between the steps of a
sequence now and then, and the link drops. Every command sent, every console
line, store update and warning, every return value and command error is
compared.
"""
from __future__ import annotations

import math
import random
import threading
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus import mavlink_flight as rust_side  # noqa: E402
from corvus.autopilot import ModeCommand  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from corvus_link import mavutil  # noqa: E402
from tests.reference import mavlink_flight_python as reference  # noqa: E402

M = mavutil.mavlink
ACCEPTED = M.MAV_RESULT_ACCEPTED


class ReferenceBridge(reference.MissionFlightMixin, reference.FlightCommandsMixin, MavlinkBridge):
    """A bridge whose flight commands are the Python reference."""


KINDS = (ReferenceBridge, MavlinkBridge)
STACKS = {"px4": M.MAV_AUTOPILOT_PX4, "ardupilot": M.MAV_AUTOPILOT_ARDUPILOTMEGA}
FRAMES = {"copter": M.MAV_TYPE_QUADROTOR, "plane": M.MAV_TYPE_FIXED_WING, "rover": M.MAV_TYPE_GROUND_ROVER}


def _plain(value: Any) -> Any:
    if isinstance(value, float) and math.isnan(value):
        return "nan"
    if isinstance(value, dict):
        return {k: _plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    if isinstance(value, frozenset):
        return sorted(value)
    return value


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", _plain(call(*args, **kwargs)))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


class Clock:
    """``time`` for both modules: sleeping moves the clock, and now and then
    lets an operator abort register from another thread."""

    def __init__(self, rng: random.Random, lock: Any, abort_rate: float) -> None:
        self.now = 1000.0
        self.rng = rng
        self.lock = lock
        self.abort_rate = abort_rate
        self.sleeps = 0
        self.pending: list[tuple[int, Any]] = []

    def monotonic(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.sleeps += 1
        self.now += seconds
        due = [apply for at, apply in self.pending if at <= self.sleeps]
        self.pending = [(at, apply) for at, apply in self.pending if at > self.sleeps]
        for apply in due:
            apply()
        if self.rng.random() < self.abort_rate:
            self.abort()

    def abort(self) -> None:
        def press() -> None:
            with self.lock._turnstile:
                self.lock._abort_count += 1
                self.lock._abort_log.append((self.lock._abort_count, threading.get_ident()))
        worker = threading.Thread(target=press)
        worker.start()
        worker.join()


class Vehicle:
    """Answers commands from a seeded script and keeps telemetry in step."""

    def __init__(self, bridge: Any, rng: random.Random, log: list, clock: Clock) -> None:
        self.bridge = bridge
        self.rng = rng
        self.log = log
        self.clock = clock
        self.modes = {tuple(_plain(ModeCommand(*v[:3]).params())): name
                      for name, v in bridge._mode_values.items()}

    def _answer(self) -> int:
        return self.rng.choice([ACCEPTED] * 14 + [M.MAV_RESULT_DENIED, M.MAV_RESULT_FAILED,
                                                 M.MAV_RESULT_TEMPORARILY_REJECTED, M.MAV_RESULT_UNSUPPORTED,
                                                 M.MAV_RESULT_IN_PROGRESS, -1, -2])

    def command(self, command: int, params: Any = None, **kwargs: Any) -> int:
        self.log.append(("command_long", command, _plain(list(params or [])), _plain(kwargs)))
        result = self._answer()
        fields: dict[str, Any] = {}
        if command == M.MAV_CMD_COMPONENT_ARM_DISARM:
            fields = {"armed": bool(params[0])}
        elif command == M.MAV_CMD_DO_SET_MODE:
            fields = {"mode": self.modes.get(tuple(_plain(list(params))), "?")}
        elif command == M.MAV_CMD_MISSION_START:
            fields = {"mode": self.bridge._dialect.mission_mode or "", "armed": True}
        if result == ACCEPTED and fields:
            roll = self.rng.random()
            store = self.bridge._store
            if roll < 0.5:
                store.update(**fields)
            elif roll < 0.85:
                lag = self.clock.sleeps + self.rng.randint(1, 80)
                self.clock.pending.append((lag, lambda: store.update(**fields)))
        return result

    def command_int(self, command: int, frame: int, params: Any, **kwargs: Any) -> int:
        self.log.append(("command_int", command, frame, _plain(list(params)), _plain(kwargs)))
        return self._answer()

    def upload(self, items: Any, mission_type: int = 0) -> int:
        self.log.append(("upload", _plain(items), mission_type))
        return self.rng.choice([M.MAV_MISSION_ACCEPTED] * 4 + [M.MAV_MISSION_ERROR, M.MAV_MISSION_DENIED, -1])

    def request_home(self) -> None:
        self.log.append(("request_home",))
        if self.rng.random() < 0.3:
            self.bridge._home_alt_amsl = 488.25


def make(kind: type, stack: str, frame: str, seed: int, abort_rate: float) -> tuple[Any, list, Clock]:
    rng = random.Random(seed)
    store = VehicleStateStore()
    store.heartbeat()
    bridge = kind(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._latch_dialect(STACKS[stack], FRAMES[frame])
    bridge._mode_values = dict(bridge._dialect.mode_table(bridge._mav_type_id, None))
    log: list = []
    clock = Clock(rng, bridge._operation_lock, abort_rate)
    vehicle = Vehicle(bridge, rng, log, clock)

    store.update(
        connected=True,
        armed=rng.random() < 0.4,
        landed_state=rng.choice([None, 0, 1, 2, 3, 4]),
        altitude_agl=rng.choice([0.0, 0.4, 1.6, 25.0, None, "x"]),
        mode=rng.choice(["", *list(bridge._mode_values)[:6]]),
    )
    bridge._home_alt_amsl = rng.choice([None, None, 512.0])
    bridge._position_home_alt_amsl = rng.choice([None, 498.5])

    ready = {"left": rng.randint(40, 4000)}

    def connection_ready() -> bool:
        ready["left"] -= 1
        return ready["left"] > 0 or rng.random() < 0.5

    original_update = store.update

    def update(**fields: Any) -> None:
        log.append(("store", _plain(fields)))
        original_update(**fields)

    class Mav:
        def set_position_target_local_ned_send(self, *args: Any) -> None:
            if rng.random() < 0.1:
                raise OSError("radio gone")
            log.append(("setpoint", _plain(list(args))))

    store.update = update  # type: ignore[method-assign]
    bridge._connection_ready = connection_ready
    bridge._send_command_and_wait = vehicle.command
    bridge._send_command_int_and_wait = vehicle.command_int
    bridge._upload_mission = vehicle.upload
    bridge._request_home = vehicle.request_home
    bridge._note_vehicle_mission = lambda plan: log.append(("note_mission", plan))
    bridge._console_publish = lambda name, text, level: log.append(("console", name, text, level))
    bridge._store_update_warning = lambda text, level, event=None: log.append(("warning", text, level))
    bridge._conn = SimpleNamespace(mav=Mav(), source_system=255, source_component=190)
    # A bridge that was never started reads as stopping, and every telemetry
    # wait would return at once.
    bridge._stop_event.clear()
    return bridge, log, clock


def _point(rng: random.Random) -> Any:
    roll = rng.random()
    if roll < 0.05:
        return rng.choice([None, "x", {"lat": True, "lon": 1.0, "alt_agl": 10}])
    return {"lat": rng.choice([48.0801234, -33.9, 91.0, float("nan"), "48"]) if roll < 0.15
            else round(48 + rng.uniform(-0.01, 0.01), 7),
            "lon": round(11 + rng.uniform(-0.01, 0.01), 7),
            "alt_agl": rng.choice([10, 25.5, 120.0, 40, 15.25, 0.5, 600])}


def _operation(rng: random.Random, bridge: Any) -> tuple[str, tuple, dict]:
    modes = list(bridge._mode_values)
    table = list(bridge._dialect.plugin_commands(bridge._mav_type_id))
    roll = rng.randrange(13)
    if roll == 0:
        return "set_mode", (rng.choice(modes + ["NOPE", 3]) if modes else "NOPE",), {}
    if roll == 1:
        return "arm", (rng.choice([True, True, False, 1, None]),), {}
    if roll == 2:
        return "takeoff", (rng.choice([10, 25.5, 2.4, 40, 10, 15, 0.5, 600, True, "15", "x", float("inf")]),), {}
    if roll == 3:
        return "land", (), {}
    if roll == 4:
        return "rtl", (), {}
    if roll == 5:
        return "set_home", (rng.choice([48.1234567, 91.0, True, "48", float("nan")]),
                            rng.choice([11.7654321, -181.0, None])), {}
    if roll == 6:
        return "hold", (), {}
    if roll == 7:
        command = rng.choice(table + [M.MAV_CMD_NAV_TAKEOFF, 99999]) if table else 99999
        params = rng.choice([None, [], [1.0, 2], [1, 2, 3, 4, 5, 6, 7, 8], ["x"]])
        position = rng.choice([None, (48.1, 11.2, 30.0, "relative"), (48.1, 11.2, 30.0, "amsl"),
                               (95.0, 11.2, 30.0, "relative"), (48.1, 11.2, 30.0), (48.1, 11.2, "x", "amsl")])
        return "plugin_command", (command, params, position), {}
    if roll == 8:
        values = [rng.choice([0.0, 1.5, -2.25, 30.0, float("nan"), True, "1"]) for _ in range(3)]
        return "velocity_setpoint", tuple(values), {
            "yaw_rate_deg": rng.choice([None, 15.0, -400.0]), "body": rng.choice([False, True, 1])}
    if roll in (9, 10, 11):
        points = [_point(rng) for _ in range(rng.choice([1, 1, 2, 3, 4, 0]))]
        return "fly_to_points", (rng.choice([points] * 12 + ["x"]),), {}
    return "start_mission", (rng.choice([1, 4, 3, 7, 0, -2, True, 2.0, "3"]),), {}


def run(kind: type, stack: str, frame: str, seed: int, abort_rate: float,
        monkeypatch: pytest.MonkeyPatch) -> list:
    bridge, log, clock = make(kind, stack, frame, seed, abort_rate)
    monkeypatch.setattr(reference, "time", clock)
    monkeypatch.setattr(rust_side, "time", clock)
    ops = random.Random(seed * 7 + 1)
    for step in range(25):
        name, args, kwargs = _operation(ops, bridge)
        result = outcome(getattr(bridge, name), *args, **kwargs)
        log.append(("result", step, name, result, bridge.get_last_command_error(), clock.sleeps))
        if ops.random() < 0.1:
            bridge._store.update(armed=False, landed_state=1)
    return log


@pytest.mark.parametrize("seed", range(12))
@pytest.mark.parametrize("frame", list(FRAMES))
@pytest.mark.parametrize("stack", list(STACKS))
def test_flight_sessions_match(stack: str, frame: str, seed: int, monkeypatch: pytest.MonkeyPatch) -> None:
    results = [run(kind, stack, frame, seed, 0.004, monkeypatch) for kind in KINDS]
    assert results[1] == results[0]


@pytest.mark.parametrize("seed", range(8))
@pytest.mark.parametrize("stack", list(STACKS))
def test_sequences_interrupted_by_an_abort_match(stack: str, seed: int, monkeypatch: pytest.MonkeyPatch) -> None:
    results = [run(kind, stack, "copter", 100 + seed, 0.2, monkeypatch) for kind in KINDS]
    assert results[1] == results[0]


def test_aborts_do_interrupt_sequences(monkeypatch: pytest.MonkeyPatch) -> None:
    """The abort sessions above must actually stop sequences between steps."""
    stopped = sum("another command took over" in str(entry)
                  for stack in STACKS for seed in range(8)
                  for entry in run(MavlinkBridge, stack, "copter", 100 + seed, 0.2, monkeypatch))
    assert stopped >= 5


def test_the_sessions_reach_every_command(monkeypatch: pytest.MonkeyPatch) -> None:
    """The comparison above is only as good as what it exercises."""
    seen: set[str] = set()
    for stack in STACKS:
        for frame in FRAMES:
            for seed in range(12):
                for entry in run(MavlinkBridge, stack, frame, seed, 0.004, monkeypatch):
                    if entry[0] == "result" and entry[3] == ("ok", True):
                        seen.add(entry[2])
                    if entry[0] == "command_long":
                        seen.add(f"cmd:{entry[1]}")
                    if entry[0] in ("command_int", "upload", "setpoint"):
                        seen.add(entry[0])
    for name in ("set_mode", "arm", "takeoff", "land", "rtl", "set_home", "hold", "plugin_command",
                 "velocity_setpoint", "fly_to_points", "start_mission", "command_int", "upload", "setpoint",
                 f"cmd:{M.MAV_CMD_MISSION_START}", f"cmd:{M.MAV_CMD_NAV_TAKEOFF}"):
        assert name in seen, name
