"""The Rust setup engine against the Python setup commands it replaced.

``tests/reference/mavlink_setup_python.py`` keeps calibration, motor test,
autotune and the reboots as they were in Python. Each test runs the same calls
against the same scripted vehicle answers through a bridge on the reference
and a bridge on ``corvus_link.setup.SetupEngine``, and compares the return
values, every command sent, the command errors, the console, the store and
the remembered state.
"""
from __future__ import annotations

import math
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from corvus_link import mavutil  # noqa: E402
from tests.reference import mavlink_setup_python as reference  # noqa: E402

M = mavutil.mavlink
ACCEPTED = M.MAV_RESULT_ACCEPTED
IN_PROGRESS = M.MAV_RESULT_IN_PROGRESS


class ReferenceBridge(reference.VehicleSetupMixin, MavlinkBridge):
    """A bridge whose setup commands are the Python reference."""


for _name in ("_active_calibration", "_autotune_return_mode", "_motors_tested"):
    setattr(ReferenceBridge, _name, None)


def _plain(value: Any) -> Any:
    if isinstance(value, float) and math.isnan(value):
        return "nan"
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    if isinstance(value, frozenset):
        return sorted(value)
    if isinstance(value, dict):
        return {k: _plain(v) for k, v in value.items()}
    return value


def make(kind: type, stack: str = "px4", *, answers: list[int] | None = None, armed: bool = False,
         landed: int = 0, ready: bool = True, mode: str = "", mode_ok: bool = True,
         vehicle: int = M.MAV_TYPE_QUADROTOR) -> tuple[Any, dict]:
    store = VehicleStateStore()
    store.heartbeat()
    store.update(armed=armed, landed_state=landed, mode=mode)
    bridge = kind(store)
    bridge._target_system = 1
    bridge._target_component = 1
    autopilot = M.MAV_AUTOPILOT_ARDUPILOTMEGA if stack == "ardupilot" else M.MAV_AUTOPILOT_PX4
    bridge._latch_dialect(autopilot, vehicle)
    seen: dict[str, list] = {"sent": [], "console": [], "modes": [], "resets": []}
    script = list(answers or [])

    def send(command: int, params: Any = None, **kwargs: Any) -> int:
        seen["sent"].append((command, _plain(list(params or [])), _plain(kwargs)))
        return script.pop(0) if script else ACCEPTED

    def set_mode(name: str) -> bool:
        seen["modes"].append(name)
        if mode_ok:
            store.update(mode=name)
        return mode_ok

    bridge._send_command_and_wait = send
    bridge._connection_ready = lambda: ready
    bridge._console_publish = lambda name, text, level: seen["console"].append((name, text, level))
    bridge.set_mode = set_mode
    bridge._reset_parameter_cache = lambda: seen["resets"].append(True)
    if stack == "ardupilot":
        bridge._mode_values = {"STABILIZE": 0, "LOITER": 5, "AUTOTUNE": 15, "ALT_HOLD": 2}
    return bridge, seen


def observe(bridge: Any, seen: dict, results: list) -> tuple:
    snap = bridge._store.get_snapshot()
    return (_plain(results), seen, bridge.get_last_command_error(),
            snap.get("autotune_state"), snap.get("autotune_progress"), snap.get("warnings"),
            bridge._active_calibration, bridge._autotune_return_mode,
            list(getattr(bridge, "_motors_tested", None) or []))


def outcome(call: Any, *args: Any) -> Any:
    try:
        return call(*args)
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


def both(run: Any, **setup: Any) -> None:
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, seen = make(kind, **setup)
        results.append(observe(bridge, seen, run(bridge)))
    assert results[1] == results[0]


SENSORS = ["gyro", "accel", "mag", "baro", "level", "esc", "airspeed", "accel_simple", "nope"]


@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
@pytest.mark.parametrize("sensor", SENSORS)
@pytest.mark.parametrize("answer", [ACCEPTED, M.MAV_RESULT_DENIED, -1])
def test_calibrate_and_cancel_match(stack: str, sensor: str, answer: int) -> None:
    both(lambda b: [outcome(b.calibrate, sensor), outcome(b.cancel_calibration),
                    b.available_calibrations()],
         stack=stack, answers=[answer, answer])


@pytest.mark.parametrize("setup", [{"armed": True}, {"ready": False}])
@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
def test_calibration_refusals_match(stack: str, setup: dict) -> None:
    both(lambda b: [outcome(b.calibrate, "gyro"), outcome(b.cancel_calibration),
                    outcome(b.accel_calibration_position, "LEVEL")],
         stack=stack, **setup)


@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
@pytest.mark.parametrize("position", ["LEVEL", "LEFT", "NOSEDOWN", "BACK", "SIDEWAYS", 3])
@pytest.mark.parametrize("answer", [ACCEPTED, M.MAV_RESULT_FAILED])
def test_accel_positions_match(stack: str, position: Any, answer: int) -> None:
    both(lambda b: [outcome(b.accel_calibration_position, position)], stack=stack, answers=[answer])


MOTOR_CALLS = [
    (1, 10, 2), (16, 100, 30), (0, 10, 2), (17, 10, 2), (True, 10, 2), (2.0, 10, 2),
    ("3", 10, 2), (4, -1, 2), (4, 101, 2), (4, float("nan"), 2), (4, 12.5, float("nan")),
    (4, 12.5, -3), (4, "50", 1), (4, "x", 1), (5, 0, 0.25), (2**70, 10, 2),
]


@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
@pytest.mark.parametrize("call", MOTOR_CALLS)
@pytest.mark.parametrize("answer", [ACCEPTED, M.MAV_RESULT_DENIED])
def test_motor_tests_match(stack: str, call: tuple, answer: int) -> None:
    both(lambda b: [outcome(b.motor_test, *call)], stack=stack, answers=[answer])


@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
def test_stopping_after_a_sequence_matches(stack: str) -> None:
    def run(b: Any) -> list:
        out = [outcome(b.motor_test, n, 20, 3) for n in (3, 12, 3, 9)]
        out.append(outcome(b.stop_motor_test))
        return out
    both(run, stack=stack, answers=[ACCEPTED] * 4 + [ACCEPTED, M.MAV_RESULT_DENIED])


@pytest.mark.parametrize("setup", [{"armed": True}, {"ready": False}, {}])
def test_motor_refusals_and_stop_without_a_test_match(setup: dict) -> None:
    both(lambda b: [outcome(b.motor_test, 1, 10, 2), outcome(b.stop_motor_test)], **setup)


@pytest.mark.parametrize("axis", ["all", "roll", None])
@pytest.mark.parametrize("enable", [True, False])
@pytest.mark.parametrize("state", [
    {"armed": True, "landed": M.MAV_LANDED_STATE_IN_AIR},
    {"armed": True, "landed": M.MAV_LANDED_STATE_ON_GROUND},
    {"armed": True, "landed": 0},
    {"armed": False},
    {"armed": True, "ready": False},
])
@pytest.mark.parametrize("answer", [ACCEPTED, IN_PROGRESS, M.MAV_RESULT_TEMPORARILY_REJECTED])
def test_px4_autotune_matches(axis: Any, enable: bool, state: dict, answer: int) -> None:
    both(lambda b: [outcome(b.autotune, axis, enable)], answers=[answer], **state)


@pytest.mark.parametrize("vehicle", [M.MAV_TYPE_FIXED_WING, M.MAV_TYPE_GROUND_ROVER])
def test_px4_autotune_on_other_airframes_matches(vehicle: int) -> None:
    both(lambda b: [outcome(b.autotune, "all", True), outcome(b.autotune, "all", False)],
         armed=True, landed=M.MAV_LANDED_STATE_IN_AIR, vehicle=vehicle)


@pytest.mark.parametrize("mode", ["", "ALT_HOLD", "AUTOTUNE", "GUIDED"])
@pytest.mark.parametrize("mode_ok", [True, False])
@pytest.mark.parametrize("vehicle", [M.MAV_TYPE_QUADROTOR, M.MAV_TYPE_FIXED_WING, M.MAV_TYPE_GROUND_ROVER])
def test_ardupilot_autotune_by_mode_matches(mode: str, mode_ok: bool, vehicle: int) -> None:
    both(lambda b: [outcome(b.autotune, "all", True), outcome(b.autotune, "all", False)],
         stack="ardupilot", armed=True, landed=M.MAV_LANDED_STATE_IN_AIR, mode=mode,
         mode_ok=mode_ok, vehicle=vehicle)


def test_ardupilot_autotune_stop_without_any_mode_matches() -> None:
    def run(b: Any) -> list:
        b._mode_values = {"STABILIZE": 0}
        b._autotune_return_mode = "ACRO"
        return [outcome(b.autotune, "all", False)]
    both(run, stack="ardupilot")


def _ack(result: Any, **extra: Any) -> SimpleNamespace:
    return SimpleNamespace(result=result, **extra)


def test_autotune_acks_match() -> None:
    acks = [
        _ack(IN_PROGRESS, progress=12), _ack(IN_PROGRESS, progress=250), _ack(IN_PROGRESS, progress=-4),
        _ack(IN_PROGRESS, progress=None), _ack(IN_PROGRESS, progress="x"), _ack(IN_PROGRESS),
        _ack(ACCEPTED), _ack(ACCEPTED), _ack(IN_PROGRESS, progress=40), _ack(M.MAV_RESULT_FAILED),
        _ack("junk"), _ack(None), _ack(99), _ack("4"),
    ]

    def run(b: Any) -> list:
        out = []
        for ack in acks:
            out.append(outcome(b._handle_autotune_ack, ack))
            snap = b._store.get_snapshot()
            out.append((snap.get("autotune_state"), snap.get("autotune_progress")))
        out.append(outcome(b._handle_autotune_ack, SimpleNamespace()))
        return out
    both(run)


@pytest.mark.parametrize("setup", [{}, {"armed": True}, {"ready": False}])
@pytest.mark.parametrize("answer", [ACCEPTED, M.MAV_RESULT_DENIED, -1])
@pytest.mark.parametrize("stack", ["px4", "ardupilot"])
def test_reboots_match(setup: dict, answer: int, stack: str) -> None:
    both(lambda b: [outcome(b.reboot_autopilot), outcome(b.reboot_to_bootloader)],
         stack=stack, answers=[answer, answer], **setup)


def test_the_cancel_timeout_is_read_from_the_bridge(monkeypatch: pytest.MonkeyPatch) -> None:
    for kind in (ReferenceBridge, MavlinkBridge):
        monkeypatch.setattr(kind, "CALIBRATION_CANCEL_ACK_S", 0.25)
        monkeypatch.setattr(kind, "MOTOR_TEST_MAX_DURATION_S", 4)
    both(lambda b: [outcome(b.cancel_calibration), outcome(b.motor_test, 2, 30, 9)])
