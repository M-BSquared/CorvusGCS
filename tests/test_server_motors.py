"""The Setup -> Motors endpoints: read, assign, and the bench motor test.

Three jobs, three risk profiles.

``GET /api/motors`` must never break the page: a missing bridge, a bridge that
answers nothing, and a bridge that raises all come back as a renderable 200.

``POST /api/motors/assign`` writes to a real aircraft's output map. It has to
move a motor without stranding it, swap two motors when the operator crosses
them, and refuse rather than silently move a servo off its pin.

``POST /api/motors/test`` spins a motor. It is refused while armed, validated
before it reaches the link, and its stop is never refused.
"""
from __future__ import annotations

from typing import Any

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus import motor_config  # noqa: E402
from corvus.server import CorvusHandler  # noqa: E402


class FakeMotorBridge:
    """Bridge stand-in exposing only what the motors endpoints touch."""

    def __init__(self, values: dict[str, float] | None = None,
                 error: str = "", raises: Exception | None = None,
                 set_ok: bool = True, test_ok: bool = True,
                 stop_ok: bool = True) -> None:
        self.values = dict(values or {})
        self.error = error
        self.raises = raises
        self.set_ok = set_ok
        self.test_ok = test_ok
        self.stop_ok = stop_ok
        self.requested: list[list[str]] = []
        self.set_calls: list[tuple[str, float]] = []
        self.test_calls: list[tuple[int, float, float]] = []
        self.stop_calls = 0

    def fetch_params(self, names: list[str], timeout: float = 4.0) -> dict[str, float]:
        self.requested.append(list(names))
        if self.raises is not None:
            raise self.raises
        return {n: v for n, v in self.values.items() if n in set(names)}

    def set_param(self, name: str, value: float) -> bool:
        self.set_calls.append((name, value))
        if not self.set_ok:
            return False
        self.values[name] = value
        return True

    def motor_test(self, motor: int, throttle_pct: float, duration_s: float) -> bool:
        self.test_calls.append((motor, throttle_pct, duration_s))
        return self.test_ok

    def stop_motor_test(self) -> bool:
        self.stop_calls += 1
        return self.stop_ok

    def get_last_command_error(self) -> str:
        return self.error


def _handler(bridge: Any) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def _quad_values() -> dict[str, float]:
    values = {"SYS_AUTOSTART": 4001.0, "CA_AIRFRAME": 0.0, "CA_ROTOR_COUNT": 4.0}
    for i in range(4):
        values[f"CA_ROTOR{i}_PX"] = 0.15
        values[f"CA_ROTOR{i}_KM"] = 0.05
    for pin in range(1, 9):
        values[f"PWM_MAIN_FUNC{pin}"] = float(100 + pin) if pin <= 4 else 0.0
    return values


# ---- GET /api/motors ----

def test_a_connected_quad_returns_motors_outputs_and_sections() -> None:
    bridge = FakeMotorBridge(_quad_values())
    handler, responses = _handler(bridge)

    handler._api_motors()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is True
    assert payload["rotor_count"] == 4
    assert payload["airframe_preset"] == 4001
    assert [m["output"]["label"] for m in payload["motors"]] == [
        "MAIN 1", "MAIN 2", "MAIN 3", "MAIN 4",
    ]
    assert [f["param"] for f in payload["geometry"]] == ["CA_AIRFRAME", "CA_ROTOR_COUNT"]
    assert payload["airframe_family"] == "multirotor"
    assert "error" not in payload


def test_the_read_asks_for_the_whole_schema_in_one_batch() -> None:
    """One named batch read, not the full ~1300-parameter download."""
    bridge = FakeMotorBridge(_quad_values())
    handler, _ = _handler(bridge)

    handler._api_motors()

    assert len(bridge.requested) == 1
    assert bridge.requested[0] == motor_config.param_names()


def test_without_a_bridge_the_page_still_renders() -> None:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = None
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]

    handler._api_motors()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is False
    assert payload["motors"] == [] and payload["sections"] == []
    assert payload["geometry"] == []


def test_a_vehicle_that_answers_nothing_reports_why_at_200() -> None:
    bridge = FakeMotorBridge({}, error="not connected")
    handler, responses = _handler(bridge)

    handler._api_motors()

    payload, status = responses[0]
    assert status == 200, "a read failure must not 500 the page"
    assert payload["connected"] is False
    assert payload["error"] == "not connected"


def test_a_raising_bridge_is_reported_not_propagated() -> None:
    bridge = FakeMotorBridge(_quad_values(), raises=RuntimeError("link died mid-read"))
    handler, responses = _handler(bridge)

    handler._api_motors()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is False
    assert "link died mid-read" in payload["error"]


def test_a_firmware_missing_parameters_yields_fewer_fields_not_an_error() -> None:
    """PX4 v1.16/v1.17/v1.18 differ; the page shrinks rather than failing."""
    values = _quad_values()
    del values["CA_AIRFRAME"]
    handler, responses = _handler(FakeMotorBridge(values))

    handler._api_motors()

    payload, _ = responses[0]
    assert payload["connected"] is True
    assert [f["param"] for f in payload["geometry"]] == ["CA_ROTOR_COUNT"]
    assert payload["airframe_family"] == "multirotor"


# ---- POST /api/motors/assign ----

def test_moving_a_motor_to_a_free_pin_clears_the_old_one_first() -> None:
    """A motor briefly on no pin is a safer transient than one on two."""
    bridge = FakeMotorBridge(_quad_values())
    handler, responses = _handler(bridge)

    handler._api_motors_assign({"motor": 1, "bank": "MAIN", "pin": 6})

    payload, status = responses[0]
    assert status == 200 and payload["ok"] is True
    assert bridge.set_calls == [("PWM_MAIN_FUNC1", 0.0), ("PWM_MAIN_FUNC6", 101.0)]


def test_assigning_a_motor_that_had_no_pin_is_a_single_write() -> None:
    values = _quad_values()
    values["PWM_MAIN_FUNC1"] = 0.0
    bridge = FakeMotorBridge(values)
    handler, responses = _handler(bridge)

    handler._api_motors_assign({"motor": 1, "bank": "MAIN", "pin": 7})

    assert responses[0][0]["ok"] is True
    assert bridge.set_calls == [("PWM_MAIN_FUNC7", 101.0)]


def test_targeting_a_pin_that_drives_another_motor_swaps_the_two() -> None:
    """Crossed motors are the whole reason this page exists; make the fix one click."""
    bridge = FakeMotorBridge(_quad_values())
    handler, responses = _handler(bridge)

    handler._api_motors_assign({"motor": 2, "bank": "MAIN", "pin": 3})

    assert responses[0][0]["ok"] is True
    # Motor 2 vacates MAIN 2, which Motor 3 takes; Motor 2 claims MAIN 3.
    assert bridge.set_calls == [("PWM_MAIN_FUNC2", 103.0), ("PWM_MAIN_FUNC3", 102.0)]


def test_a_swap_is_refused_when_the_motor_has_no_pin_to_swap_onto() -> None:
    values = _quad_values()
    values["PWM_MAIN_FUNC2"] = 0.0        # Motor 2 is unassigned
    handler, responses = _handler(FakeMotorBridge(values))

    handler._api_motors_assign({"motor": 2, "bank": "MAIN", "pin": 3})

    payload, status = responses[0]
    assert status == 409 and payload["ok"] is False
    assert "already drives Motor 3" in payload["error"]


def test_a_pin_driving_a_servo_is_refused_by_name_not_overwritten() -> None:
    """Silently moving a servo off its pin is how a control surface stops working."""
    values = _quad_values()
    values["PWM_MAIN_FUNC5"] = 201.0
    bridge = FakeMotorBridge(values)
    handler, responses = _handler(bridge)

    handler._api_motors_assign({"motor": 1, "bank": "MAIN", "pin": 5})

    payload, status = responses[0]
    assert status == 409 and payload["ok"] is False
    assert "Servo 1" in payload["error"]
    assert bridge.set_calls == [], "nothing is written when the target is refused"


def test_unassigning_a_motor_clears_its_pin() -> None:
    bridge = FakeMotorBridge(_quad_values())
    handler, responses = _handler(bridge)

    handler._api_motors_assign({"motor": 3, "output": None})

    assert responses[0][0]["ok"] is True
    assert bridge.set_calls == [("PWM_MAIN_FUNC3", 0.0)]


def test_assigning_a_motor_to_the_pin_it_is_already_on_writes_nothing() -> None:
    bridge = FakeMotorBridge(_quad_values())
    handler, responses = _handler(bridge)

    handler._api_motors_assign({"motor": 1, "bank": "MAIN", "pin": 1})

    assert responses[0][0] == {"ok": True, "writes": [], "note": "already assigned"}
    assert bridge.set_calls == []


def test_a_pin_this_board_does_not_have_is_refused() -> None:
    bridge = FakeMotorBridge(_quad_values())   # MAIN only, 8 pins answered
    handler, responses = _handler(bridge)

    handler._api_motors_assign({"motor": 1, "bank": "AUX", "pin": 2})

    payload, status = responses[0]
    assert status == 400 and "no output AUX 2" in payload["error"]
    assert bridge.set_calls == []


def test_a_failed_write_reports_what_had_already_been_applied() -> None:
    """A half-applied assignment must be visible, not silently reported as done."""
    bridge = FakeMotorBridge(_quad_values(), set_ok=False, error="cannot set parameter while armed")
    handler, responses = _handler(bridge)

    handler._api_motors_assign({"motor": 1, "bank": "MAIN", "pin": 6})

    payload, status = responses[0]
    assert status == 409 and payload["ok"] is False
    assert payload["applied"] == []
    assert payload["failed"] == "PWM_MAIN_FUNC1"


@pytest.mark.parametrize("body", [
    {"motor": 0, "bank": "MAIN", "pin": 1},
    {"motor": 17, "bank": "MAIN", "pin": 1},
    {"motor": True, "bank": "MAIN", "pin": 1},
    {"motor": "1", "bank": "MAIN", "pin": 1},
    {"motor": 1, "bank": "MAIN"},
    {"motor": 1, "pin": 1},
    {"motor": 1, "bank": "MAIN", "pin": True},
])
def test_assign_rejects_a_malformed_body_before_touching_the_link(body: dict) -> None:
    bridge = FakeMotorBridge(_quad_values())
    handler, responses = _handler(bridge)

    handler._api_motors_assign(body)

    assert responses[0][1] == 400
    assert bridge.set_calls == [] and bridge.requested == []


def test_assign_without_a_bridge_is_a_503() -> None:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = None
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]

    handler._api_motors_assign({"motor": 1, "bank": "MAIN", "pin": 1})

    assert responses[0][1] == 503


# ---- POST /api/motors/test ----

def test_a_motor_test_reaches_the_bridge_with_motor_throttle_and_duration() -> None:
    bridge = FakeMotorBridge(_quad_values())
    handler, responses = _handler(bridge)

    handler._api_motors_test({"motor": 3, "throttle": 20, "duration": 1.5})

    assert responses[0] == ({"ok": True, "motor": 3, "throttle": 20.0}, 200)
    assert bridge.test_calls == [(3, 20.0, 1.5)]


def test_a_motor_test_has_conservative_defaults() -> None:
    """Omitted throttle/duration must not mean full power forever."""
    bridge = FakeMotorBridge(_quad_values())
    handler, _ = _handler(bridge)

    handler._api_motors_test({"motor": 1})

    motor, throttle, duration = bridge.test_calls[0]
    assert (motor, throttle) == (1, 15.0)
    assert 0 < duration <= 5


@pytest.mark.parametrize("body", [
    {"motor": "1"},
    {"motor": True},
    {"motor": 1, "throttle": "20"},
    {"motor": 1, "throttle": 101},
    {"motor": 1, "throttle": -1},
    {"motor": 1, "duration": "2"},
])
def test_motor_test_rejects_a_malformed_body_before_touching_the_link(body: dict) -> None:
    bridge = FakeMotorBridge(_quad_values())
    handler, responses = _handler(bridge)

    handler._api_motors_test(body)

    assert responses[0][1] == 400
    assert bridge.test_calls == []


def test_a_refused_motor_test_reports_the_bridge_reason() -> None:
    bridge = FakeMotorBridge(_quad_values(), test_ok=False,
                             error="cannot test motors while armed")
    handler, responses = _handler(bridge)

    handler._api_motors_test({"motor": 1})

    payload, status = responses[0]
    assert status == 409 and payload["error"] == "cannot test motors while armed"


def test_stopping_a_motor_test_is_a_plain_ok() -> None:
    bridge = FakeMotorBridge(_quad_values())
    handler, responses = _handler(bridge)

    handler._api_motors_test_stop({})

    assert responses[0] == ({"ok": True}, 200)
    assert bridge.stop_calls == 1
