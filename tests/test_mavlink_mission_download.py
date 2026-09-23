"""Reading the vehicle's mission back, and following it while it is flown.

Before this, a mission already on the aircraft (uploaded by QGroundControl, or
by Corvus before a reconnect) was invisible, and the map could not say which
leg was being flown. Three parts are pinned here:

* the download handshake: MISSION_REQUEST_LIST, one MISSION_REQUEST_INT per
  item asked again when it is lost, MISSION_ACK, and a cancel whenever the
  transfer is abandoned so the vehicle's mission manager is not left waiting;
* the reading back of the items into a plan (ArduPilot's home slot removed);
* MISSION_CURRENT / MISSION_ITEM_REACHED turned into a place in the plan, and
  withdrawn when another station replaces the mission.
"""
from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from pymavlink import mavutil

from corvus import mission
from corvus.mavlink_bridge import MavlinkBridge
from corvus.mavlink_missions import MAV_MISSION_OPERATION_CANCELLED
from corvus.state_store import VehicleStateStore

M = mavutil.mavlink
RELATIVE = M.MAV_FRAME_GLOBAL_RELATIVE_ALT


class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return getattr(self, "source_system", 1)

    def get_srcComponent(self) -> int:
        return getattr(self, "source_component", 1)


def wire_item(seq: int, entry: dict[str, Any], frame: int = RELATIVE) -> FakeMessage:
    """A MISSION_ITEM_INT as the vehicle sends it back."""
    params = list(entry.get("params") or [0.0] * 4)
    command = int(entry["command"])
    if command == mission.MAV_CMD_DO_CHANGE_SPEED:
        frame = M.MAV_FRAME_MISSION
    return FakeMessage(
        message_type="MISSION_ITEM_INT", seq=seq, frame=frame, command=command,
        current=0, autocontinue=1,
        param1=params[0], param2=params[1], param3=params[2], param4=params[3],
        x=int(round(entry.get("lat", 0.0) * 1e7)), y=int(round(entry.get("lon", 0.0) * 1e7)),
        z=float(entry.get("alt", 0.0)), mission_type=0,
        target_system=255, target_component=190,
    )


class FakeVehicle:
    """The vehicle's half of the mission protocol, answering on the spot.

    ``drop`` names item sequence numbers whose first request goes unanswered,
    ``lost`` ones that never arrive at all; ``silent`` ignores the list request.
    """

    def __init__(self, bridge: MavlinkBridge, items: list[FakeMessage],
                 drop: set[int] | None = None, lost: set[int] | None = None,
                 silent: bool = False) -> None:
        self.bridge = bridge
        self.items = items
        self.drop = set(drop or ())
        self.lost = set(lost or ())
        self.silent = silent
        self.list_requests = 0
        self.item_requests: list[int] = []
        self.acks: list[int] = []
        self.commands: list[tuple] = []

    def mission_request_list_send(self, _sys: int, _comp: int, mission_type: int = 0) -> None:
        self.list_requests += 1
        if self.silent:
            return
        self.bridge._dispatch(FakeMessage(
            message_type="MISSION_COUNT", count=len(self.items), mission_type=mission_type,
            target_system=255, target_component=190))

    def mission_request_int_send(self, _sys: int, _comp: int, seq: int,
                                 mission_type: int = 0) -> None:
        self.item_requests.append(seq)
        if seq in self.lost:
            return
        if seq in self.drop:
            self.drop.discard(seq)
            return
        self.bridge._dispatch(self.items[seq])

    def mission_ack_send(self, _sys: int, _comp: int, result: int,
                         mission_type: int = 0) -> None:
        self.acks.append(result)

    def command_long_send(self, *args: Any) -> None:
        self.commands.append(args)


def ready_bridge() -> MavlinkBridge:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    return bridge


def attach(bridge: MavlinkBridge, vehicle: FakeVehicle) -> None:
    bridge._conn = SimpleNamespace(mav=vehicle, source_system=255, source_component=190)


def fast(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("corvus.mavlink_missions.MISSION_DOWNLOAD_WAIT_S", 0.05)
    monkeypatch.setattr("corvus.mavlink_missions.MISSION_DOWNLOAD_WAIT_S_SLOW", 0.05)


def sample_plan() -> dict[str, Any]:
    plan, error = mission.validate_plan({
        "name": "Survey",
        "home": {"lat": 48.08, "lon": 11.64},
        "speed": 8,
        "items": [
            {"type": "takeoff", "lat": 48.08, "lon": 11.64, "alt": 25},
            {"type": "waypoint", "lat": 48.081, "lon": 11.641, "alt": 30, "hold": 5,
             "speed": 12},
            {"type": "loiter_turns", "lat": 48.09, "lon": 11.65, "alt": 40,
             "turns": 2, "radius": 60, "direction": -1},
            {"type": "rtl"},
        ],
    })
    assert error == "" and plan is not None
    return plan


def vehicle_items(plan: dict[str, Any]) -> list[FakeMessage]:
    return [wire_item(seq, entry) for seq, entry in enumerate(mission.plan_to_items(plan))]


# ---------------------------------------------------------------------------
# The handshake
# ---------------------------------------------------------------------------

def test_a_downloaded_mission_reads_back_as_the_plan_it_was_uploaded_from(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fast(monkeypatch)
    bridge = ready_bridge()
    plan = sample_plan()
    vehicle = FakeVehicle(bridge, vehicle_items(plan))
    attach(bridge, vehicle)
    bridge._store.update(home=[11.64, 48.08])

    result = bridge.download_mission()

    assert result is not None, bridge.get_last_command_error()
    assert vehicle.list_requests == 1
    assert vehicle.item_requests == list(range(6)), "one request per item, in order"
    assert vehicle.acks == [M.MAV_MISSION_ACCEPTED], "the transfer is closed"
    assert result["count"] == 6
    assert result["skipped"] == [] and result["adjusted"] == []
    back = result["plan"]
    assert [i["type"] for i in back["items"]] == ["takeoff", "waypoint", "loiter_turns", "rtl"]
    assert back["speed"] == 8
    assert back["items"][1]["speed"] == 12 and back["items"][1]["hold"] == 5
    assert back["items"][2]["direction"] == -1.0 and back["items"][2]["radius"] == 60
    assert back["items"] == plan["items"], "uploading what was read puts the same mission back"
    # speed(0) takeoff(0) speed(1) waypoint(1) loiter(2) rtl(3)
    assert result["plan_index"] == [0, 0, 1, 1, 2, 3]
    snap = bridge._store.get_snapshot()
    assert snap["mission_known"] is True and snap["mission_total"] == 6


def test_a_lost_item_is_asked_for_again(monkeypatch: pytest.MonkeyPatch) -> None:
    fast(monkeypatch)
    bridge = ready_bridge()
    vehicle = FakeVehicle(bridge, vehicle_items(sample_plan()), drop={2})
    attach(bridge, vehicle)

    assert bridge.download_mission() is not None
    assert vehicle.item_requests.count(2) == 2


def test_an_item_that_never_arrives_ends_the_download_and_cancels_it(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fast(monkeypatch)
    bridge = ready_bridge()
    vehicle = FakeVehicle(bridge, vehicle_items(sample_plan()), lost={1})
    attach(bridge, vehicle)

    assert bridge.download_mission() is None
    assert "mission item 2 of 6 never arrived" in bridge.get_last_command_error()
    assert vehicle.acks == [MAV_MISSION_OPERATION_CANCELLED]
    assert bridge._store.get_snapshot()["mission_known"] is False


def test_a_vehicle_that_never_answers_the_list_request_is_reported(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fast(monkeypatch)
    bridge = ready_bridge()
    vehicle = FakeVehicle(bridge, [], silent=True)
    attach(bridge, vehicle)

    assert bridge.download_mission() is None
    assert "did not answer" in bridge.get_last_command_error()
    assert vehicle.list_requests >= 2, "the request is asked again before giving up"


def test_an_empty_mission_is_an_empty_plan(monkeypatch: pytest.MonkeyPatch) -> None:
    fast(monkeypatch)
    bridge = ready_bridge()
    vehicle = FakeVehicle(bridge, [])
    attach(bridge, vehicle)

    result = bridge.download_mission()
    assert result is not None
    assert result["plan"]["items"] == [] and result["count"] == 0
    assert vehicle.acks == [], "a count of zero is a complete transfer on its own"


def test_a_mission_longer_than_the_planner_is_refused_before_any_item(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fast(monkeypatch)
    bridge = ready_bridge()
    long = [wire_item(0, {"command": mission.MAV_CMD_NAV_WAYPOINT, "lat": 48, "lon": 11,
                          "alt": 30, "params": [0, 0, 0, 0]})] * (mission.MISSION_MAX_ITEMS + 1)
    vehicle = FakeVehicle(bridge, long)
    attach(bridge, vehicle)

    assert bridge.download_mission() is None
    assert "more than the 255 the planner holds" in bridge.get_last_command_error()
    assert vehicle.item_requests == []
    assert vehicle.acks == [MAV_MISSION_OPERATION_CANCELLED]


def test_a_waiting_abort_interrupts_the_download(monkeypatch: pytest.MonkeyPatch) -> None:
    """An RTL must not wait for the rest of a mission to arrive over a radio."""
    fast(monkeypatch)
    bridge = ready_bridge()
    vehicle = FakeVehicle(bridge, vehicle_items(sample_plan()), lost={0})
    attach(bridge, vehicle)
    monkeypatch.setattr(bridge._operation_lock, "abort_waiting", lambda: True)

    assert bridge.download_mission() is None
    assert "interrupted" in bridge.get_last_command_error()
    assert vehicle.item_requests == [0], "given up on the first wait, not after every retry"
    assert vehicle.acks == [MAV_MISSION_OPERATION_CANCELLED]


def test_an_item_meant_for_another_station_is_not_taken(monkeypatch: pytest.MonkeyPatch) -> None:
    fast(monkeypatch)
    bridge = ready_bridge()
    items = vehicle_items(sample_plan())
    vehicle = FakeVehicle(bridge, items, lost={0})
    attach(bridge, vehicle)
    stray = wire_item(0, mission.plan_to_items(sample_plan())[0])
    stray.target_system = 42

    def request(_sys: int, _comp: int, seq: int, mission_type: int = 0) -> None:
        vehicle.item_requests.append(seq)
        bridge._dispatch(stray)

    vehicle.mission_request_int_send = request  # type: ignore[method-assign]
    assert bridge.download_mission() is None


def test_ardupilots_home_slot_is_left_out_and_a_takeoff_with_no_place_is_drawn_at_home(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fast(monkeypatch)
    bridge = ready_bridge()
    bridge._latch_dialect(M.MAV_AUTOPILOT_ARDUPILOTMEGA, M.MAV_TYPE_QUADROTOR)
    bridge._store.update(home=[11.64, 48.08])
    home_slot = wire_item(0, {"command": mission.MAV_CMD_NAV_WAYPOINT, "lat": 48.08,
                              "lon": 11.64, "alt": 512, "params": [0, 0, 0, 0]},
                          frame=M.MAV_FRAME_GLOBAL)
    takeoff = wire_item(1, {"command": mission.MAV_CMD_NAV_TAKEOFF, "lat": 0, "lon": 0,
                            "alt": 20, "params": [0, 0, 0, 0]})
    waypoint = wire_item(2, {"command": mission.MAV_CMD_NAV_WAYPOINT, "lat": 48.09,
                             "lon": 11.65, "alt": 30, "params": [0, 0, 0, 0]})
    vehicle = FakeVehicle(bridge, [home_slot, takeoff, waypoint])
    attach(bridge, vehicle)

    result = bridge.download_mission()

    assert result is not None
    assert result["count"] == 2, "slot 0 is ArduPilot's home, not an item"
    first = result["plan"]["items"][0]
    assert first["type"] == "takeoff"
    assert (first["lat"], first["lon"]) == (48.08, 11.64)
    assert result["adjusted"][0]["field"] == "position"


# ---------------------------------------------------------------------------
# Progress
# ---------------------------------------------------------------------------

def current(seq: int, total: int = 6, state: int = 3, mission_id: int = 0) -> FakeMessage:
    return FakeMessage(message_type="MISSION_CURRENT", seq=seq, total=total,
                       mission_state=state, mission_mode=1, mission_id=mission_id)


def download(monkeypatch: pytest.MonkeyPatch) -> MavlinkBridge:
    fast(monkeypatch)
    bridge = ready_bridge()
    attach(bridge, FakeVehicle(bridge, vehicle_items(sample_plan())))
    assert bridge.download_mission() is not None
    return bridge


def test_mission_current_is_shown_as_the_plan_item_being_flown_to(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bridge = download(monkeypatch)
    bridge._dispatch(current(3))
    snap = bridge._store.get_snapshot()
    assert snap["mission_seq"] == 3
    assert snap["mission_item"] == 1, "the waypoint, which the speed change ahead of it belongs to"
    assert snap["mission_state"] == "active"
    assert snap["mission_total"] == 6

    bridge._dispatch(FakeMessage(message_type="MISSION_ITEM_REACHED", seq=4))
    snap = bridge._store.get_snapshot()
    assert snap["mission_reached"] == 4 and snap["mission_reached_item"] == 2


def test_a_vehicle_with_no_mission_reports_zero_items() -> None:
    bridge = ready_bridge()
    bridge._dispatch(current(0, total=65535, state=1))
    snap = bridge._store.get_snapshot()
    assert snap["mission_total"] == 0 and snap["mission_state"] == "no_mission"
    assert snap["mission_item"] == -1, "nothing known, nothing claimed"


def test_a_mission_replaced_by_another_station_is_no_longer_claimed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bridge = download(monkeypatch)
    bridge._dispatch(current(2, mission_id=0x1111))   # adopted: Corvus's own transfer
    assert bridge._store.get_snapshot()["mission_item"] == 1
    revision = bridge._store.get_snapshot()["mission_revision"]

    bridge._dispatch(current(2, mission_id=0x2222))   # QGroundControl uploaded
    snap = bridge._store.get_snapshot()
    assert snap["mission_known"] is False
    assert snap["mission_item"] == -1
    assert snap["mission_revision"] > revision


def test_a_different_item_count_is_another_stations_mission(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The signal every stack sends, for a pymavlink that drops mission_id."""
    bridge = download(monkeypatch)
    bridge._dispatch(current(2, total=6))            # confirms the count
    assert bridge._store.get_snapshot()["mission_known"] is True
    bridge._dispatch(current(2, total=9))            # somebody uploaded 9 items
    snap = bridge._store.get_snapshot()
    assert snap["mission_known"] is False and snap["mission_item"] == -1


def test_a_report_of_the_old_count_right_after_an_upload_is_not_a_change(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A MISSION_CURRENT the vehicle sent before the transfer can arrive after it."""
    bridge = download(monkeypatch)
    bridge._dispatch(current(0, total=3))            # stale, still the old mission
    assert bridge._store.get_snapshot()["mission_known"] is True
    monkeypatch.setattr("corvus.mavlink_missions.MISSION_CHANGE_GRACE_S", 0.0)
    bridge._dispatch(current(0, total=3))            # still wrong after the grace
    assert bridge._store.get_snapshot()["mission_known"] is False


def test_ardupilot_progress_is_numbered_without_the_home_slot(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bridge = ready_bridge()
    bridge._latch_dialect(M.MAV_AUTOPILOT_ARDUPILOTMEGA, M.MAV_TYPE_QUADROTOR)
    bridge._note_vehicle_mission([0, 1, 2])
    bridge._dispatch(current(1, total=4, state=0))
    snap = bridge._store.get_snapshot()
    assert snap["mission_seq"] == 0 and snap["mission_item"] == 0
    assert snap["mission_total"] == 3
    assert snap["mission_state"] == "", "a firmware that does not report the state is not guessed"


def test_a_new_link_forgets_what_the_last_one_knew(monkeypatch: pytest.MonkeyPatch) -> None:
    bridge = download(monkeypatch)
    bridge._forget_vehicle_mission()
    bridge._dispatch(current(3))
    assert bridge._store.get_snapshot()["mission_item"] == -1


def test_a_disconnect_stops_claiming_the_mission_is_being_flown() -> None:
    bridge = ready_bridge()
    bridge._dispatch(current(1))
    assert bridge._store.get_snapshot()["mission_state"] == "active"
    bridge._store.set_disconnected()
    assert bridge._store.get_snapshot()["mission_state"] == ""
