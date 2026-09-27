"""Fly to one point without a mission, and start a mission without re-commanding it.

Seen in PX4 v1.18 SITL (gz_x500): "take off to 3 m, then fly to this point"
went TAKEOFF, LOITER, MISSION, LOITER, with a notification for each step, and
replaced whatever mission was stored on the aircraft. One point for a vehicle
already in the air is now MAV_CMD_DO_REPOSITION, which leaves a holding PX4 in
Hold. And an accepted MISSION_START is no longer followed by a DO_SET_MODE and
an arm: PX4 accepts it only once it is in the mission and armed, and the extra
mode change restarted a short mission that had already finished.
"""
from __future__ import annotations

import math
import threading
from types import SimpleNamespace
from typing import Any

from pymavlink import mavutil

from corvus import autopilot
from corvus.mavlink_bridge import PX4_FALLBACK_MODE_VALUES, MavlinkBridge
from corvus.state_store import VehicleStateStore

ACCEPTED = mavutil.mavlink.MAV_RESULT_ACCEPTED
PX4_MISSION_CUSTOM_MODE = (4 << 24) | (4 << 16)


class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return getattr(self, "source_system", 1)

    def get_srcComponent(self) -> int:
        return getattr(self, "source_component", 1)


class AckingMav:
    """Answers every command with *result* and plays the mission handshake."""

    def __init__(self, bridge: MavlinkBridge) -> None:
        self.bridge = bridge
        self.long: list[tuple] = []
        self.int: list[tuple] = []
        self.counts: list[int] = []
        self.results: dict[int, int] = {}

    def _ack(self, command: int) -> None:
        self.bridge._dispatch(FakeMessage(
            message_type="COMMAND_ACK", command=command,
            result=self.results.get(command, ACCEPTED),
            target_system=255, target_component=190,
        ))
        if command == mavutil.mavlink.MAV_CMD_MISSION_START \
                and self.results.get(command, ACCEPTED) == ACCEPTED:
            # PX4 is in AUTO.MISSION and armed once it accepts.
            self.bridge._dispatch(FakeMessage(
                message_type="HEARTBEAT",
                type=mavutil.mavlink.MAV_TYPE_QUADROTOR,
                autopilot=mavutil.mavlink.MAV_AUTOPILOT_PX4,
                base_mode=mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED
                | mavutil.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED,
                custom_mode=PX4_MISSION_CUSTOM_MODE,
            ))

    def command_long_send(self, *args: Any) -> None:
        self.long.append(args)
        self._ack(int(args[2]))

    def command_int_send(self, *args: Any) -> None:
        self.int.append(args)
        self._ack(int(args[3]))

    def mission_count_send(self, _system: int, _component: int, count: int) -> None:
        self.counts.append(count)

        def run() -> None:
            for seq in range(count):
                self.bridge._handle_mission_request(FakeMessage(
                    message_type="MISSION_REQUEST_INT", seq=seq,
                    target_system=255, target_component=190,
                ), as_int=True)
            self.bridge._handle_mission_ack(FakeMessage(
                message_type="MISSION_ACK", type=mavutil.mavlink.MAV_MISSION_ACCEPTED,
                target_system=255, target_component=190,
            ))
        threading.Thread(target=run, daemon=True).start()

    def mission_item_int_send(self, *args: Any) -> None:
        pass

    def heartbeat_send(self, *args: Any) -> None:
        pass


def bridge_in_the_air(*, armed: bool = True, mode: str = "LOITER") -> MavlinkBridge:
    store = VehicleStateStore()
    store.heartbeat()
    store.update(connected=True, armed=armed, mode=mode, landed_state=2,
                 altitude_agl=3.0, position=[8.5456, 47.3977])
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = SimpleNamespace(source_system=255, source_component=190)
    bridge._conn.mav = AckingMav(bridge)
    bridge._mode_values = dict(PX4_FALLBACK_MODE_VALUES)
    bridge._home_alt_amsl = 488.0
    bridge._mav_type_id = mavutil.mavlink.MAV_TYPE_QUADROTOR
    return bridge


def test_one_point_in_the_air_is_a_reposition_not_a_mission() -> None:
    bridge = bridge_in_the_air()
    mav = bridge._conn.mav

    assert bridge.fly_to_points([{"lat": 47.3980, "lon": 8.5460, "alt_agl": 3.0}])

    assert mav.counts == [], "no mission upload, so the stored mission survives"
    assert mav.long == [], "no mode change, no arm, no MISSION_START"
    assert len(mav.int) == 1
    (_sys, _comp, frame, command, _cur, _auto,
     p1, p2, _p3, p4, x, y, z) = mav.int[0]
    assert command == mavutil.mavlink.MAV_CMD_DO_REPOSITION
    assert frame == mavutil.mavlink.MAV_FRAME_GLOBAL
    assert p1 == -1.0, "the vehicle's own cruise speed"
    assert p2 == 1.0, "MAV_DO_REPOSITION_FLAGS_CHANGE_MODE: into Hold"
    assert math.isnan(p4), "heading left to the vehicle"
    assert (x, y) == (473980000, 85460000)
    assert z == 491.0, "PX4 reads z as AMSL: home 488 m + 3 m AGL"


def test_the_reposition_waits_for_no_mode_on_px4() -> None:
    """Even from TAKEOFF (the climb not yet finished) it goes straight out;
    PX4 switches into Hold itself because of the change-mode flag."""
    bridge = bridge_in_the_air(mode="TAKEOFF")
    assert bridge.fly_to_points([{"lat": 47.3980, "lon": 8.5460, "alt_agl": 3.0}])
    assert [int(a[3]) for a in bridge._conn.mav.int] == [
        mavutil.mavlink.MAV_CMD_DO_REPOSITION]


def test_ardupilot_gets_the_reposition_relative_to_home() -> None:
    bridge = bridge_in_the_air(mode="GUIDED")
    bridge._dialect = autopilot.dialect_for_stack(autopilot.STACK_ARDUPILOT)
    bridge._home_alt_amsl = None       # not needed in the relative frame
    mav = bridge._conn.mav

    assert bridge.fly_to_points([{"lat": 47.3980, "lon": 8.5460, "alt_agl": 12.0}])

    frame, command = mav.int[0][2], mav.int[0][3]
    assert command == mavutil.mavlink.MAV_CMD_DO_REPOSITION
    assert frame == mavutil.mavlink.MAV_FRAME_GLOBAL_RELATIVE_ALT
    assert mav.int[0][12] == 12.0


def test_without_an_altitude_reference_the_mission_takes_over() -> None:
    """PX4 needs AMSL for a reposition; the mission's relative frame does not,
    so a missing reference costs the shortcut and not the flight."""
    bridge = bridge_in_the_air()
    bridge._home_alt_amsl = None
    bridge._position_home_alt_amsl = None
    mav = bridge._conn.mav

    assert bridge.fly_to_points([{"lat": 47.3980, "lon": 8.5460, "alt_agl": 3.0}])

    assert mav.int == []
    assert mav.counts == [1], "one waypoint, no takeoff item in the air"


def test_a_vehicle_without_reposition_falls_back_to_the_mission() -> None:
    bridge = bridge_in_the_air()
    mav = bridge._conn.mav
    mav.results[mavutil.mavlink.MAV_CMD_DO_REPOSITION] = \
        mavutil.mavlink.MAV_RESULT_UNSUPPORTED

    assert bridge.fly_to_points([{"lat": 47.3980, "lon": 8.5460, "alt_agl": 3.0}])
    assert mav.counts == [1]


def test_a_refused_reposition_is_reported_not_papered_over() -> None:
    bridge = bridge_in_the_air()
    mav = bridge._conn.mav
    mav.results[mavutil.mavlink.MAV_CMD_DO_REPOSITION] = \
        mavutil.mavlink.MAV_RESULT_DENIED

    assert not bridge.fly_to_points([{"lat": 47.3980, "lon": 8.5460, "alt_agl": 3.0}])
    assert "Fly to point failed" in bridge.get_last_command_error()
    assert mav.counts == [], "a refusal is an answer, not a reason to upload"


def test_several_points_in_the_air_are_still_a_mission() -> None:
    bridge = bridge_in_the_air()
    mav = bridge._conn.mav
    assert bridge.fly_to_points([
        {"lat": 47.3980, "lon": 8.5460, "alt_agl": 3.0},
        {"lat": 47.3985, "lon": 8.5465, "alt_agl": 3.0},
    ])
    assert mav.int == []
    assert mav.counts == [2]


def test_one_point_on_the_ground_is_still_a_mission_with_a_takeoff() -> None:
    bridge = bridge_in_the_air()
    bridge._store.update(landed_state=1, altitude_agl=0.0)
    mav = bridge._conn.mav
    assert bridge.fly_to_points([{"lat": 47.3980, "lon": 8.5460, "alt_agl": 3.0}])
    assert mav.int == []
    assert mav.counts == [2], "takeoff, then the point"


def test_an_accepted_mission_start_is_not_followed_by_a_mode_change_or_arm() -> None:
    bridge = bridge_in_the_air()
    mav = bridge._conn.mav

    assert bridge.fly_to_points([
        {"lat": 47.3980, "lon": 8.5460, "alt_agl": 3.0},
        {"lat": 47.3985, "lon": 8.5465, "alt_agl": 3.0},
    ])

    sent = [int(a[2]) for a in mav.long]
    assert sent == [mavutil.mavlink.MAV_CMD_MISSION_START], (
        "PX4's ACK already means MISSION and armed; a second MISSION "
        "restarted a short mission that had finished"
    )
    assert bridge._store.get_snapshot()["mode"] == "MISSION"
