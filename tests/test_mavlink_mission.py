"""Tests for the Mission planner's MAVLink half.

``fly_to_points`` is one gesture — upload, start, arm — and was the only
mission path the bridge had. A PLANNED mission is drawn, reviewed and walked
out to, so it is split in two: :meth:`upload_mission_plan` puts a route on the
aircraft and stops, and :meth:`start_mission` is the separate decision to fly
it. The most important assertion in this file is the negative one — that an
upload sends no arm command at all.

The rest pins the wire format, because a mission item is 13 fields and nothing
downstream would notice them being shuffled: the frame each item travels in,
the AGL altitude going out unconverted, and the sequence numbers matching the
order the operator drew.

Mirrors the fakes in tests/test_mavlink_takeoff.py.
"""
from __future__ import annotations

import threading
from types import SimpleNamespace
from typing import Any, Callable

import pytest
from pymavlink import mavutil

from corvus import mission
from corvus.mavlink_bridge import (
    MISSION_DO_FRAME,
    PX4_FALLBACK_MODE_VALUES,
    MavlinkBridge,
)
from corvus.state_store import VehicleStateStore

RELATIVE_FRAME = mavutil.mavlink.MAV_FRAME_GLOBAL_RELATIVE_ALT


class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return getattr(self, "source_system", 1)

    def get_srcComponent(self) -> int:
        return getattr(self, "source_component", 1)


class FakeMav:
    """Records the mission handshake and every command that goes out."""

    def __init__(self) -> None:
        self.counts: list[int] = []
        self.items: list[tuple] = []
        self.commands: list[tuple] = []
        self.on_count: Callable[[int], None] | None = None
        self.on_command: Callable[[tuple], None] | None = None

    def mission_count_send(self, _system: int, _component: int, count: int) -> None:
        self.counts.append(count)
        if self.on_count:
            self.on_count(count)

    def mission_item_int_send(self, *args: Any) -> None:
        self.items.append(args)

    def command_long_send(self, *args: Any) -> None:
        self.commands.append(args)
        if self.on_command:
            self.on_command(args)


class FakeConnection:
    def __init__(self) -> None:
        self.mav = FakeMav()
        self.source_system = 255
        self.source_component = 190


def ready_bridge() -> MavlinkBridge:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = FakeConnection()
    return bridge


def item_request(seq: int) -> FakeMessage:
    return FakeMessage(
        message_type="MISSION_REQUEST_INT",
        seq=seq,
        target_system=255,
        target_component=190,
    )


def mission_ack(ack_type: int) -> FakeMessage:
    return FakeMessage(
        message_type="MISSION_ACK",
        type=ack_type,
        target_system=255,
        target_component=190,
    )


def answer_the_handshake(bridge: MavlinkBridge,
                         ack_type: int = mavutil.mavlink.MAV_MISSION_ACCEPTED) -> None:
    """Play the vehicle's side of an upload, on its own thread.

    It has to be a thread: the bridge sends MISSION_COUNT while holding its
    send lock, and serving an item request takes that same lock.
    """
    def respond(count: int) -> None:
        def run() -> None:
            for seq in range(count):
                bridge._handle_mission_request(item_request(seq), as_int=True)
            bridge._handle_mission_ack(mission_ack(ack_type))
        threading.Thread(target=run, daemon=True).start()

    bridge._conn.mav.on_count = respond


def sample_items() -> list[dict[str, Any]]:
    plan, error = mission.validate_plan({
        "home": {"lat": 48.08, "lon": 11.64},
        "items": [
            {"type": "takeoff", "lat": 48.08, "lon": 11.64, "alt": 25},
            {"type": "loiter_turns", "lat": 48.09, "lon": 11.65, "alt": 40,
             "turns": 2, "radius": 60},
            {"type": "rtl"},
        ],
    })
    assert error == "" and plan is not None
    return mission.plan_to_items(plan)


# ---------------------------------------------------------------------------
# upload
# ---------------------------------------------------------------------------

def test_an_accepted_upload_sends_every_item_in_order() -> None:
    bridge = ready_bridge()
    answer_the_handshake(bridge)

    assert bridge.upload_mission_plan(sample_items())

    sent = bridge._conn.mav.items
    assert bridge._conn.mav.counts == [3]
    assert [entry[2] for entry in sent] == [0, 1, 2], "sequence numbers in draw order"
    assert [entry[4] for entry in sent] == [
        mavutil.mavlink.MAV_CMD_NAV_TAKEOFF,
        mavutil.mavlink.MAV_CMD_NAV_LOITER_TURNS,
        mavutil.mavlink.MAV_CMD_NAV_RETURN_TO_LAUNCH,
    ]


def test_an_upload_never_arms_or_changes_mode() -> None:
    """The whole reason upload and start are two methods."""
    bridge = ready_bridge()
    answer_the_handshake(bridge)

    assert bridge.upload_mission_plan(sample_items())

    assert bridge._conn.mav.commands == [], "an upload sends no command at all"


def test_navigation_items_travel_in_the_relative_altitude_frame() -> None:
    """The plan's altitudes are metres above home, which is what that frame
    means — converting them would be converting them twice."""
    bridge = ready_bridge()
    answer_the_handshake(bridge)

    bridge.upload_mission_plan(sample_items())

    sent = bridge._conn.mav.items
    assert [entry[3] for entry in sent] == [RELATIVE_FRAME] * 3
    assert sent[0][13] == pytest.approx(25.0), "takeoff altitude, unconverted"
    assert sent[1][13] == pytest.approx(40.0)


def test_a_speed_change_travels_in_the_mission_frame() -> None:
    """PX4 walks the frame of every item; a DO_ command names no place."""
    bridge = ready_bridge()
    answer_the_handshake(bridge)
    plan, _error = mission.validate_plan({
        "speed": 9,
        "items": [{"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 30}],
    })
    assert plan is not None

    bridge.upload_mission_plan(mission.plan_to_items(plan))

    speed_item, waypoint = bridge._conn.mav.items
    assert speed_item[4] == mavutil.mavlink.MAV_CMD_DO_CHANGE_SPEED
    assert speed_item[3] == MISSION_DO_FRAME
    assert waypoint[3] == RELATIVE_FRAME


def test_coordinates_are_sent_as_scaled_integers() -> None:
    """MISSION_ITEM_INT's 1e7 degrees, not a float32 that loses tens of metres."""
    bridge = ready_bridge()
    answer_the_handshake(bridge)

    bridge.upload_mission_plan(sample_items())

    first = bridge._conn.mav.items[0]
    assert first[11] == 480800000
    assert first[12] == 116400000


def test_the_first_item_is_marked_current_and_the_rest_are_not() -> None:
    bridge = ready_bridge()
    answer_the_handshake(bridge)

    bridge.upload_mission_plan(sample_items())

    assert [entry[5] for entry in bridge._conn.mav.items] == [1, 0, 0]


def test_a_rejected_upload_reports_the_reason() -> None:
    bridge = ready_bridge()
    answer_the_handshake(bridge, mavutil.mavlink.MAV_MISSION_DENIED)

    assert not bridge.upload_mission_plan(sample_items())
    assert "Mission upload failed" in bridge.get_last_command_error()


def test_an_empty_item_list_is_refused_before_the_link() -> None:
    bridge = ready_bridge()

    assert not bridge.upload_mission_plan([])
    assert bridge._conn.mav.counts == []


def test_an_oversized_item_list_is_refused_before_the_link() -> None:
    bridge = ready_bridge()
    items = sample_items() * 200

    assert not bridge.upload_mission_plan(items)
    assert bridge._conn.mav.counts == []
    assert "maximum" in bridge.get_last_command_error()


def test_uploading_without_a_link_fails_rather_than_raising() -> None:
    bridge = MavlinkBridge(VehicleStateStore())

    assert not bridge.upload_mission_plan(sample_items())
    assert "DISCONNECTED" in bridge.get_last_command_error()


# ---------------------------------------------------------------------------
# start and clear
# ---------------------------------------------------------------------------

def test_start_sends_an_explicit_first_and_last_item_index() -> None:
    """PX4's "0 = last item" convention is the one part of MISSION_START that
    differs across v1.16 to v1.18, so it is never relied on."""
    bridge = ready_bridge()

    def respond(args: tuple) -> None:
        command = int(args[2])
        bridge._dispatch(FakeMessage(
            message_type="COMMAND_ACK", command=command,
            result=mavutil.mavlink.MAV_RESULT_ACCEPTED,
            target_system=255, target_component=190,
        ))
        if command == mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM:
            bridge._dispatch(FakeMessage(
                message_type="HEARTBEAT",
                type=mavutil.mavlink.MAV_TYPE_QUADROTOR,
                autopilot=mavutil.mavlink.MAV_AUTOPILOT_PX4,
                base_mode=mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED,
                custom_mode=(4 << 24) | (4 << 16),
            ))

    bridge._conn.mav.on_command = respond
    # The mode table is normally filled from the first HEARTBEAT's
    # mode_mapping; the built-in PX4 fallback is what a link with none uses.
    bridge._mode_values = dict(PX4_FALLBACK_MODE_VALUES)

    assert bridge.start_mission(5)

    start = bridge._conn.mav.commands[0]
    assert int(start[2]) == mavutil.mavlink.MAV_CMD_MISSION_START
    assert start[4] == 0.0, "first item"
    assert start[5] == 4.0, "last item, stated rather than implied"


@pytest.mark.parametrize("count", [0, -3])
def test_start_refuses_a_count_that_names_no_mission(count: int) -> None:
    bridge = ready_bridge()

    assert not bridge.start_mission(count)
    assert bridge._conn.mav.commands == []


def test_clear_uploads_a_zero_item_mission() -> None:
    """The protocol's own way of saying "wipe it", so a refusal still comes
    back as a MISSION_ACK with a reason."""
    bridge = ready_bridge()
    answer_the_handshake(bridge)

    assert bridge.clear_mission()
    assert bridge._conn.mav.counts == [0]
    assert bridge._conn.mav.items == []
