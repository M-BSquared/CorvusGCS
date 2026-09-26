"""The HTTP layer choosing a schema for the stack that is connected.

The four setup pages are each a list of parameter names, and picking the wrong
list does not error — the vehicle simply answers for nothing and the page comes
up blank. So the thing worth testing is the *choice*: which module the endpoint
reads the names out of, and that a bridge which predates the dialect layer still
gets the PX4 ones rather than a 500.

Plus the two endpoints that only exist because of ArduPilot:
``/api/mavlink/capabilities`` and ``/api/calibrate/position``.
"""
from __future__ import annotations

from typing import Any

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from pymavlink import mavutil as mv  # noqa: E402

from corvus import (  # noqa: E402
    ardupilot_motors,
    ardupilot_mounting,
    ardupilot_rc,
    ardupilot_safety,
    ardupilot_tuning,
    autopilot,
    motor_config,
    mounting_config,
    rc_config,
    safety_config,
    tuning_config,
)
from corvus.server import CorvusHandler  # noqa: E402


class FakeBridge:
    """Bridge stand-in exposing only what the setup endpoints touch."""

    def __init__(self, stack: str = autopilot.STACK_PX4,
                 vehicle_type: int = mv.mavlink.MAV_TYPE_QUADROTOR) -> None:
        self.stack = stack
        self.vehicle_type_id = vehicle_type
        self.requested: list[list[str]] = []
        self.calls: list[tuple[str, Any]] = []

    def fetch_params(self, names: list[str], timeout: float = 4.0) -> dict[str, float]:
        self.requested.append(list(names))
        return {}

    def get_last_command_error(self) -> str:
        return ""

    def capabilities(self) -> dict[str, Any]:
        return autopilot.dialect_for_stack(self.stack).capabilities(self.vehicle_type_id)

    def accel_calibration_position(self, position: str) -> bool:
        self.calls.append(("position", position))
        return position != "refused"


class LegacyBridge:
    """A bridge object that predates the dialect layer entirely."""

    def fetch_params(self, names: list[str], timeout: float = 4.0) -> dict[str, float]:
        return {}

    def get_last_command_error(self) -> str:
        return ""


def _handler(bridge: Any, path: str = "/") -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge            # type: ignore[assignment]
    handler.store = None                # type: ignore[assignment]
    handler.path = path                 # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


PAGES = [
    ("_api_safety", "/api/safety", safety_config, ardupilot_safety),
    ("_api_tuning", "/api/tuning", tuning_config, ardupilot_tuning),
    ("_api_rc", "/api/rc", rc_config, ardupilot_rc),
    ("_api_motors", "/api/motors", motor_config, ardupilot_motors),
    ("_api_mounting", "/api/mounting", mounting_config, ardupilot_mounting),
]


@pytest.mark.parametrize(("endpoint", "path", "px4_module", "apm_module"), PAGES)
def test_a_px4_link_is_read_with_px4s_parameter_names(
    endpoint: str, path: str, px4_module: Any, apm_module: Any,
) -> None:
    bridge = FakeBridge(autopilot.STACK_PX4)
    handler, _responses = _handler(bridge, path)
    getattr(handler, endpoint)()
    asked = set(bridge.requested[0])
    assert asked & set(px4_module.param_names())
    assert not (asked & set(apm_module.param_names()) - set(px4_module.param_names()))


@pytest.mark.parametrize(("endpoint", "path", "px4_module", "apm_module"), PAGES)
def test_an_ardupilot_link_is_read_with_ardupilots(
    endpoint: str, path: str, px4_module: Any, apm_module: Any,
) -> None:
    bridge = FakeBridge(autopilot.STACK_ARDUPILOT)
    handler, _responses = _handler(bridge, path)
    getattr(handler, endpoint)()
    asked = set(bridge.requested[0])
    assert asked & set(apm_module.param_names())
    assert not (asked & set(px4_module.param_names()) - set(apm_module.param_names()))


@pytest.mark.parametrize(("endpoint", "path", "px4_module", "_apm"), PAGES)
def test_a_bridge_without_a_stack_still_renders_rather_than_raising(
    endpoint: str, path: str, px4_module: Any, _apm: Any,
) -> None:
    """A plugin or a test may hand the server a bridge-shaped object that
    predates the dialect layer. The setup pages must still come up."""
    handler, responses = _handler(LegacyBridge(), path)
    getattr(handler, endpoint)()
    assert responses and responses[0][1] == 200


class AnsweringBridge(FakeBridge):
    """A FakeBridge that answers for the names it is given values for."""

    def __init__(self, stack: str, values: dict[str, float]) -> None:
        super().__init__(stack)
        self.values = values

    def fetch_params(self, names: list[str], timeout: float = 4.0) -> dict[str, float]:
        self.requested.append(list(names))
        return {n: v for n, v in self.values.items() if n in names}


def test_the_motors_page_carries_the_lever_arms_of_the_connected_stack() -> None:
    """The flight controller and the GPS are drawn on the motors' airframe,
    so they ride the same read, named the way the connected stack names them."""
    bridge = AnsweringBridge(autopilot.STACK_ARDUPILOT, {
        "FRAME_CLASS": 1.0, "FRAME_TYPE": 1.0,
        "INS_POS1_X": 0.03, "INS_POS1_Y": 0.0, "INS_POS1_Z": 0.0,
        "GPS1_POS_X": -0.1, "GPS1_POS_Y": 0.0, "GPS1_POS_Z": -0.15,
    })
    handler, responses = _handler(bridge, "/api/motors")
    handler._api_motors()
    payload = responses[0][0]
    assert [s["id"] for s in payload["sensors"]] == ["fc", "gps1"]
    assert payload["sensors"][1]["fields"][0]["param"] == "GPS1_POS_X"
    assert payload["frame"]["adjustable"] is False
    assert payload["position_hint"]


def test_the_mounting_endpoint_returns_the_board_rotation() -> None:
    bridge = AnsweringBridge(autopilot.STACK_PX4, {
        "SENS_BOARD_ROT": 4.0, "SENS_BOARD_X_OFF": 0.5,
    })
    handler, responses = _handler(bridge, "/api/mounting")
    handler._api_mounting()
    payload, status = responses[0]
    assert status == 200 and payload["connected"] is True
    assert [f["param"] for f in payload["orientation"]["fields"]] == [
        "SENS_BOARD_ROT", "SENS_BOARD_X_OFF"]


def test_the_mounting_endpoint_without_a_link_still_renders() -> None:
    handler, responses = _handler(None, "/api/mounting")
    handler._api_mounting()
    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is False and payload["orientation"] is None


def test_an_unknown_stack_falls_back_to_px4s_schema() -> None:
    """PX4 is the primary target, and a page built from a superset the vehicle
    answers for none of is the same empty page whichever choice is made."""
    bridge = FakeBridge(autopilot.STACK_GENERIC)
    handler, _responses = _handler(bridge, "/api/safety")
    handler._api_safety()
    assert set(bridge.requested[0]) & set(safety_config.param_names())


# ---------------------------------------------------------------------------
# The two endpoints ArduPilot added
# ---------------------------------------------------------------------------

def test_capabilities_report_the_connected_stack() -> None:
    handler, responses = _handler(FakeBridge(autopilot.STACK_ARDUPILOT))
    handler._api_mavlink_capabilities()
    data, status = responses[0]
    assert status == 200
    assert data["stack"] == "ardupilot"
    assert data["shell"] is False
    assert "motor" not in data["calibrations"]


def test_capabilities_answer_before_a_vehicle_is_connected() -> None:
    """The page asks on load; a missing bridge must be a document, not a 503."""
    handler, responses = _handler(None)
    handler._api_mavlink_capabilities()
    data, status = responses[0]
    assert status == 200
    assert data["modes"] == []


def test_a_calibration_position_reaches_the_bridge() -> None:
    bridge = FakeBridge(autopilot.STACK_ARDUPILOT)
    handler, responses = _handler(bridge)
    handler._api_calibrate_position({"position": "NoseUp"})
    assert bridge.calls == [("position", "noseup")]
    assert responses[0] == ({"ok": True}, 200)


def test_an_unknown_calibration_position_never_reaches_the_bridge() -> None:
    bridge = FakeBridge(autopilot.STACK_ARDUPILOT)
    handler, responses = _handler(bridge)
    handler._api_calibrate_position({"position": "sideways"})
    assert bridge.calls == []
    assert responses[0][1] == 400


def test_a_calibration_position_without_a_vehicle_is_a_503() -> None:
    handler, responses = _handler(None)
    handler._api_calibrate_position({"position": "level"})
    assert responses[0][1] == 503
