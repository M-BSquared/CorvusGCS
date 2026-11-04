"""How a Return is flown, as the mission planner draws it.

A Return is not flown at a height the plan sets: the vehicle climbs, comes home
and lands or circles at heights of its own, held in parameters whose names and
units differ per stack. Each stack's Safety schema turns them into one shape,
and ``GET /api/mission/return`` picks the schema for the connected stack. What
is pinned here is that shape, per stack and per unit, and the choice.
"""
from __future__ import annotations

from typing import Any

import pytest

from corvus import ardupilot_safety, autopilot, safety_config

# ---------------------------------------------------------------------------
# PX4
# ---------------------------------------------------------------------------


def test_px4_climbs_to_its_return_height_and_waits_at_its_descend_height() -> None:
    profile = safety_config.return_profile(
        {"RTL_RETURN_ALT": 60.0, "RTL_DESCEND_ALT": 30.0, "RTL_LAND_DELAY": 0.5})
    assert profile == {"known": True, "climb_to": 60.0, "arrive_at": 30.0, "hold": False}


def test_a_px4_land_delay_of_minus_one_never_lands() -> None:
    profile = safety_config.return_profile(
        {"RTL_RETURN_ALT": 60.0, "RTL_DESCEND_ALT": 30.0, "RTL_LAND_DELAY": -1.0})
    assert profile["hold"] is True


def test_a_px4_vehicle_that_answered_nothing_is_not_known() -> None:
    assert safety_config.return_profile({}) == {
        "known": False, "climb_to": None, "arrive_at": None, "hold": False}


def test_px4_reads_only_the_three_return_parameters() -> None:
    assert safety_config.return_param_names() == [
        "RTL_RETURN_ALT", "RTL_DESCEND_ALT", "RTL_LAND_DELAY"]


# ---------------------------------------------------------------------------
# ArduPilot
# ---------------------------------------------------------------------------


def test_a_copter_keeps_its_return_heights_in_centimetres() -> None:
    profile = ardupilot_safety.return_profile(
        {"RTL_ALT": 1500.0, "RTL_ALT_FINAL": 0.0}, "copter")
    assert profile == {"known": True, "climb_to": 15.0, "arrive_at": None, "hold": False}


def test_a_copter_with_a_final_altitude_hovers_there_instead_of_landing() -> None:
    profile = ardupilot_safety.return_profile(
        {"RTL_ALT": 1500.0, "RTL_ALT_FINAL": 500.0}, "copter")
    assert (profile["arrive_at"], profile["hold"]) == (5.0, True)


def test_a_copter_return_altitude_of_zero_returns_where_it_is() -> None:
    profile = ardupilot_safety.return_profile({"RTL_ALT": 0.0, "RTL_ALT_FINAL": 0.0}, "copter")
    assert profile["climb_to"] is None


@pytest.mark.parametrize(("values", "expected"), [
    ({"RTL_ALTITUDE": 100.0}, 100.0),                            # metres, the newer name
    ({"ALT_HOLD_RTL": 8000.0}, 80.0),                            # centimetres, the older one
    ({"ALT_HOLD_RTL": -1.0}, None),                              # -1: where it is
    ({"RTL_ALTITUDE": 120.0, "ALT_HOLD_RTL": 5000.0}, 120.0),    # the newer name wins
])
def test_a_plane_flies_home_at_its_return_altitude_and_circles(
        values: dict[str, float], expected: Any) -> None:
    profile = ardupilot_safety.return_profile(values, "plane")
    assert profile["climb_to"] == expected
    assert profile["arrive_at"] is None and profile["hold"] is False


def test_each_ardupilot_vehicle_reads_its_own_return_parameters() -> None:
    assert ardupilot_safety.return_param_names("copter") == ["RTL_ALT", "RTL_ALT_FINAL"]
    assert ardupilot_safety.return_param_names("plane") == ["RTL_ALTITUDE", "ALT_HOLD_RTL"]
    assert ardupilot_safety.return_param_names("rover") == ["RTL_ALT", "RTL_ALT_FINAL"]


# ---------------------------------------------------------------------------
# The endpoint
# ---------------------------------------------------------------------------

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus.server import CorvusHandler  # noqa: E402

COPTER = 2
FIXED_WING = 1


class FakeBridge:
    """Bridge stand-in: answers a fixed set of parameters and records the ask."""

    def __init__(self, stack: str, vehicle_type: int, values: dict[str, float]) -> None:
        self.stack = stack
        self.vehicle_type_id = vehicle_type
        self.values = values
        self.requested: list[list[str]] = []

    def fetch_params(self, names: list[str], timeout: float = 4.0) -> dict[str, float]:
        self.requested.append(list(names))
        return {name: self.values[name] for name in names if name in self.values}


def _ask(bridge: Any) -> dict[str, Any]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge            # type: ignore[assignment]
    handler.store = None                # type: ignore[assignment]
    handler.path = "/api/mission/return"  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    handler._api_mission_return()
    data, status = responses[0]
    assert status == 200
    return data


def test_a_px4_vehicle_is_asked_with_px4s_names() -> None:
    bridge = FakeBridge(autopilot.STACK_PX4, COPTER,
                        {"RTL_RETURN_ALT": 60.0, "RTL_DESCEND_ALT": 30.0, "RTL_LAND_DELAY": 0.0})
    data = _ask(bridge)
    assert bridge.requested == [safety_config.return_param_names()]
    assert (data["connected"], data["climb_to"], data["arrive_at"]) == (True, 60.0, 30.0)


def test_an_ardupilot_plane_is_asked_with_a_planes_names() -> None:
    bridge = FakeBridge(autopilot.STACK_ARDUPILOT, FIXED_WING, {"RTL_ALTITUDE": 90.0})
    data = _ask(bridge)
    assert bridge.requested == [ardupilot_safety.return_param_names("plane")]
    assert data["climb_to"] == 90.0


def test_a_vehicle_that_answers_nothing_is_not_connected() -> None:
    data = _ask(FakeBridge(autopilot.STACK_PX4, COPTER, {}))
    assert data["connected"] is False


def test_nothing_connected_is_an_answer_not_an_error() -> None:
    assert _ask(None) == {"connected": False}
