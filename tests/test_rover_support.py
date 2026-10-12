"""A rover is set up and driven in the same app as an aircraft.

The app is tuned for aircraft, so the unknown vehicle is one. A rover or a boat
is the exception: it has no takeoff, no landing and no altitude, and nothing in
the command layer or the planner may offer them.
"""
from __future__ import annotations

from typing import Any

import pytest
from pymavlink import mavutil

from corvus import autopilot
from corvus.mavlink_bridge import MavlinkBridge
from corvus.server import CorvusHandler
from corvus.state_store import VehicleStateStore

ROVER = mavutil.mavlink.MAV_TYPE_GROUND_ROVER
BOAT = mavutil.mavlink.MAV_TYPE_SURFACE_BOAT
QUAD = mavutil.mavlink.MAV_TYPE_QUADROTOR


@pytest.mark.parametrize(("mav_type", "ground"), [
    (ROVER, True),
    (BOAT, True),
    (QUAD, False),
    (mavutil.mavlink.MAV_TYPE_FIXED_WING, False),
    (mavutil.mavlink.MAV_TYPE_VTOL_TILTROTOR, False),
    (0, False),
    (None, False),
])
def test_only_a_rover_or_a_boat_is_a_ground_vehicle(mav_type: Any, ground: bool) -> None:
    assert autopilot.is_ground_vehicle(mav_type) is ground


@pytest.mark.parametrize("stack", [autopilot.STACK_PX4, autopilot.STACK_ARDUPILOT])
def test_capabilities_say_when_the_vehicle_drives(stack: str) -> None:
    dialect = autopilot.dialect_for_stack(stack)
    assert dialect.capabilities(ROVER)["ground"] is True
    assert dialect.capabilities(ROVER)["vehicle"] == "rover"
    assert dialect.capabilities(QUAD)["ground"] is False


@pytest.mark.parametrize("stack", [autopilot.STACK_PX4, autopilot.STACK_ARDUPILOT])
def test_a_rover_mission_refuses_takeoff_and_land_and_a_copter_does_not(stack: str) -> None:
    dialect = autopilot.dialect_for_stack(stack)
    rover = dialect.mission_refusals(ROVER)
    assert mavutil.mavlink.MAV_CMD_NAV_TAKEOFF in rover
    assert mavutil.mavlink.MAV_CMD_NAV_LAND in rover
    copter = dialect.mission_refusals(QUAD)
    assert mavutil.mavlink.MAV_CMD_NAV_TAKEOFF not in copter
    assert mavutil.mavlink.MAV_CMD_NAV_LAND not in copter


def _bridge(mav_type: int) -> MavlinkBridge:
    store = VehicleStateStore()
    store.heartbeat()
    store.update(connected=True, armed=True)
    bridge = MavlinkBridge(store)
    bridge._mav_type_id = mav_type
    return bridge


def test_the_bridge_refuses_takeoff_and_land_on_a_rover() -> None:
    bridge = _bridge(ROVER)
    assert bridge.takeoff(10.0) is False
    assert "rover" in bridge.get_last_command_error().lower()
    assert bridge.land() is False
    assert "rover" in bridge.get_last_command_error().lower()


def test_a_rover_point_needs_no_altitude_and_gets_none() -> None:
    bridge = _bridge(ROVER)
    assert bridge._validate_fly_to_points([{"lat": 48.1, "lon": 11.6}]) == [(48.1, 11.6, 0.0)]
    assert bridge._validate_fly_to_points(
        [{"lat": 48.1, "lon": 11.6, "alt_agl": 30}]) == [(48.1, 11.6, 0.0)]


def test_an_aircraft_point_still_needs_its_altitude() -> None:
    bridge = _bridge(QUAD)
    assert bridge._validate_fly_to_points([{"lat": 48.1, "lon": 11.6}]) is None
    assert bridge._validate_fly_to_points([{"lat": 48.1, "lon": 11.6, "alt_agl": 30}]) \
        == [(48.1, 11.6, 30.0)]


def test_a_single_rover_point_is_a_reposition_only_once_armed_on_ardupilot() -> None:
    bridge = _bridge(ROVER)
    bridge._dialect = autopilot.dialect_for_stack(autopilot.STACK_ARDUPILOT)
    assert bridge._can_reposition(True, False) is True
    bridge._store.update(armed=False)
    assert bridge._can_reposition(True, False) is False
    bridge._dialect = autopilot.dialect_for_stack(autopilot.STACK_PX4)
    bridge._store.update(armed=True)
    assert bridge._can_reposition(True, False) is False


class _GotoBridge:
    def __init__(self, mav_type: int) -> None:
        self.mav_type = mav_type
        self.points: list[Any] = []

    def fly_to_points(self, points: list[dict[str, float]]) -> bool:
        self.points = points
        return True

    def get_last_command_error(self) -> str:
        return ""


def _goto(mav_type: int, point: dict[str, Any]) -> tuple[_GotoBridge, list[tuple[dict, int]]]:
    bridge = _GotoBridge(mav_type)
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    handler._vehicle_type_id = lambda: bridge.mav_type  # type: ignore[method-assign]
    sent: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: sent.append((data, status))  # type: ignore[method-assign]
    handler._api_mavlink_gotopoints({"points": [point]})
    return bridge, sent


def test_the_goto_endpoint_takes_a_rover_point_without_altitude() -> None:
    bridge, sent = _goto(ROVER, {"lat": 48.1, "lon": 11.6})
    assert sent == [({"ok": True}, 200)]
    assert bridge.points == [{"lat": 48.1, "lon": 11.6, "alt_agl": 0.0}]


def test_the_goto_endpoint_still_wants_an_altitude_for_an_aircraft() -> None:
    bridge, sent = _goto(QUAD, {"lat": 48.1, "lon": 11.6})
    assert sent[0][1] == 400
    assert bridge.points == []
