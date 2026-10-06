"""The geofence area: validation, storage, the fence transfer and its routes.

The area is drawn on the Safety & Sensors page, kept on this station and put
on the vehicle as an inclusion polygon over the mission protocol with
mission_type FENCE. What is pinned here is the wire format of that transfer
(the type on every message, the corner count in param1, nothing marked
current), that a mission upload is not confused with it, and the parameter
writes each stack needs before the polygon acts.
"""
from __future__ import annotations

import threading
from typing import Any

import pytest
from pymavlink import mavutil

from corvus import ardupilot_safety, geofence, safety_config
from corvus.mavlink_bridge import MavlinkBridge
from corvus.server import CorvusHandler
from corvus.state_store import VehicleStateStore
from tests.test_mavlink_mission import FakeMessage

FENCE = mavutil.mavlink.MAV_MISSION_TYPE_FENCE
SQUARE = [[11.0, 48.0], [11.01, 48.0], [11.01, 48.01], [11.0, 48.01]]


# ---- validation ----

def test_a_square_is_a_valid_area() -> None:
    polygon, error = geofence.validate_polygon(SQUARE)
    assert error == ""
    assert polygon == SQUARE


def test_a_repeated_closing_corner_is_dropped() -> None:
    polygon, _ = geofence.validate_polygon(SQUARE + [SQUARE[0]])
    assert polygon == SQUARE


def test_no_area_is_a_valid_answer() -> None:
    assert geofence.validate_polygon([]) == ([], "")
    assert geofence.validate_polygon(None) == ([], "")


@pytest.mark.parametrize("raw, words", [
    ([[11.0, 48.0], [11.01, 48.0]], "at least 3"),
    ([[11.0, 48.0], [11.01, 48.0], [200.0, 48.0]], "not a position"),
    ([[11.0, 48.0], [11.01, "x"], [11.0, 48.01]], "not a position"),
    ([[True, 48.0], [11.01, 48.0], [11.0, 48.01]], "not a position"),
    ("square", "list"),
    # A bow tie: the two long edges cross in the middle.
    ([[11.0, 48.0], [11.01, 48.01], [11.01, 48.0], [11.0, 48.01]], "cross"),
])
def test_an_unusable_area_is_refused_with_a_reason(raw: Any, words: str) -> None:
    polygon, error = geofence.validate_polygon(raw)
    assert polygon is None
    assert words in error


def test_too_many_corners_are_refused() -> None:
    import math
    ring = [[11.0 + 0.01 * math.cos(a / 100 * 2 * math.pi),
             48.0 + 0.01 * math.sin(a / 100 * 2 * math.pi)] for a in range(100)]
    polygon, error = geofence.validate_polygon(ring)
    assert polygon is None
    assert str(geofence.MAX_VERTICES) in error


def test_the_fence_items_carry_the_corner_count_and_the_position() -> None:
    items = geofence.fence_items(SQUARE)
    assert len(items) == 4
    for item, (lon, lat) in zip(items, SQUARE):
        assert item["command"] == mavutil.mavlink.MAV_CMD_NAV_FENCE_POLYGON_VERTEX_INCLUSION
        assert item["params"][0] == 4.0
        assert (item["lat"], item["lon"]) == (lat, lon)


# ---- storage ----

def test_the_area_survives_a_save_and_a_load(tmp_path) -> None:
    path = str(tmp_path / "geofence.json")
    geofence.save({"polygon": SQUARE, "show_on_map": False}, path)
    assert geofence.load(path) == {"polygon": SQUARE, "show_on_map": False}


def test_a_missing_or_broken_file_is_no_area(tmp_path) -> None:
    assert geofence.load(str(tmp_path / "none.json")) == {"polygon": [], "show_on_map": True}
    broken = tmp_path / "broken.json"
    broken.write_text("{not json")
    assert geofence.load(str(broken))["polygon"] == []
    crossed = tmp_path / "crossed.json"
    crossed.write_text('{"polygon": [[0,0],[1,1],[1,0],[0,1]], "show_on_map": true}')
    assert geofence.load(str(crossed))["polygon"] == []


# ---- the transfer ----

def _bridge() -> MavlinkBridge:
    from tests.test_mavlink_mission import ready_bridge
    return ready_bridge()


class TypedMav:
    """Records what goes out, with the mission_type each message carried."""

    def __init__(self, bridge: MavlinkBridge, ack: int) -> None:
        self.bridge = bridge
        self.ack = ack
        self.counts: list[tuple[int, int]] = []
        self.items: list[tuple] = []

    def mission_count_send(self, _s: int, _c: int, count: int, mission_type: int = 0) -> None:
        self.counts.append((count, mission_type))

        def vehicle() -> None:
            for seq in range(count):
                self.bridge._handle_mission_request(FakeMessage(
                    message_type="MISSION_REQUEST_INT", seq=seq, mission_type=mission_type,
                    target_system=255, target_component=190), as_int=True)
            self.bridge._handle_mission_ack(FakeMessage(
                message_type="MISSION_ACK", type=self.ack, mission_type=mission_type,
                target_system=255, target_component=190))
        threading.Thread(target=vehicle, daemon=True).start()

    def mission_item_int_send(self, *args: Any) -> None:
        self.items.append(args)


def _typed(ack: int = mavutil.mavlink.MAV_MISSION_ACCEPTED) -> tuple[MavlinkBridge, TypedMav]:
    bridge = _bridge()
    mav = TypedMav(bridge, ack)
    bridge._conn.mav = mav  # type: ignore[union-attr]
    return bridge, mav


def test_a_fence_upload_sends_every_corner_as_a_fence_item() -> None:
    bridge, mav = _typed()
    assert bridge.upload_fence(SQUARE) is True
    assert mav.counts == [(4, FENCE)]
    assert [args[2] for args in mav.items] == [0, 1, 2, 3]
    for args, (lon, lat) in zip(mav.items, SQUARE):
        assert args[3] == mavutil.mavlink.MAV_FRAME_GLOBAL
        assert args[4] == mavutil.mavlink.MAV_CMD_NAV_FENCE_POLYGON_VERTEX_INCLUSION
        assert args[5] == 0, "no fence vertex is the current item"
        assert args[7] == 4.0
        assert args[11:13] == (int(round(lat * 1e7)), int(round(lon * 1e7)))
        assert args[-1] == FENCE
    assert bridge.vehicle_fence() == SQUARE


def test_a_fence_upload_leaves_the_mission_bookkeeping_alone() -> None:
    bridge, _mav = _typed()
    before = bridge._store.get_snapshot().get("mission_revision")
    bridge.upload_fence(SQUARE)
    assert bridge._store.get_snapshot().get("mission_revision") == before


def test_a_request_for_a_mission_item_is_not_served_from_a_fence_upload() -> None:
    bridge, mav = _typed()
    bridge._mission_items = [{"x": 1}]
    bridge._mission_upload_type = FENCE
    bridge._handle_mission_request(FakeMessage(
        message_type="MISSION_REQUEST_INT", seq=0, mission_type=0,
        target_system=255, target_component=190), as_int=True)
    assert mav.items == []


def test_a_refused_fence_upload_reports_and_forgets_nothing() -> None:
    bridge, _mav = _typed(mavutil.mavlink.MAV_MISSION_NO_SPACE)
    assert bridge.upload_fence(SQUARE) is False
    assert bridge.get_last_command_error()
    assert bridge.vehicle_fence() is None


def test_clearing_the_fence_is_a_zero_item_fence_transfer() -> None:
    bridge, mav = _typed()
    assert bridge.clear_fence() is True
    assert mav.counts == [(0, FENCE)]
    assert bridge.vehicle_fence() == []


def test_a_mission_upload_still_goes_out_untyped() -> None:
    """Older fakes and MAVLink 1 links know MISSION_COUNT without mission_type."""
    bridge, mav = _typed()
    assert bridge._upload_mission([]) == mavutil.mavlink.MAV_MISSION_ACCEPTED
    assert mav.counts == [(0, 0)]


def test_the_fence_needs_a_link() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.upload_fence(SQUARE) is False
    assert bridge.clear_fence() is False


# ---- what makes the fence act ----

def test_px4_needs_no_write_to_act_on_a_polygon() -> None:
    doc = safety_config.build({"GF_ACTION": 3.0})
    section = next(s for s in doc["sections"] if s["id"] == "geofence")
    assert section["kind"] == "geofence"
    assert section["enable_writes"] == []
    assert [f["param"] for f in section["fields"]] == ["GF_ACTION"]
    assert section["inactive"] == ""


def test_px4_says_when_the_action_does_nothing() -> None:
    doc = safety_config.build({"GF_ACTION": 0.0})
    section = next(s for s in doc["sections"] if s["id"] == "geofence")
    assert section["inactive"]


def test_the_action_is_on_the_geofence_card_only() -> None:
    """Two controls over one parameter can disagree until the next read."""
    doc = safety_config.build({"GF_ACTION": 3.0, "GF_MAX_HOR_DIST": 100.0})
    owners = [s["id"] for s in doc["sections"]
              for f in s["fields"] if f["param"] == "GF_ACTION"]
    assert owners == ["geofence"]


def test_ardupilot_switches_on_the_fence_and_its_polygon_bit() -> None:
    writes = ardupilot_safety.geofence_enable_writes({"FENCE_ENABLE": 0.0, "FENCE_TYPE": 3.0})
    assert writes == [{"name": "FENCE_TYPE", "value": 7}, {"name": "FENCE_ENABLE", "value": 1}]


def test_ardupilot_writes_nothing_when_the_fence_already_acts() -> None:
    assert ardupilot_safety.geofence_enable_writes({"FENCE_ENABLE": 1.0, "FENCE_TYPE": 7.0}) == []


def test_ardupilot_leaves_a_parameter_alone_that_the_firmware_did_not_answer() -> None:
    assert ardupilot_safety.geofence_enable_writes({"FENCE_ENABLE": 0.0}) == [
        {"name": "FENCE_ENABLE", "value": 1}]


def test_the_ardupilot_section_carries_the_writes_and_the_vehicle_labels() -> None:
    doc = ardupilot_safety.build({"FENCE_ENABLE": 0.0, "FENCE_TYPE": 1.0, "FENCE_ACTION": 1.0},
                                 [], "plane")
    section = next(s for s in doc["sections"] if s["id"] == "geofence")
    assert section["enable_writes"]
    assert section["inactive"]
    field = section["fields"][0]
    assert field["param"] == "FENCE_ACTION"
    assert 6 in {int(o["value"]) for o in field["options"]}


def test_the_enable_writes_name_parameters_in_the_schema() -> None:
    names = set(ardupilot_safety.param_names())
    writes = ardupilot_safety.geofence_enable_writes({"FENCE_ENABLE": 0.0, "FENCE_TYPE": 0.0})
    assert {w["name"] for w in writes} <= names


# ---- routes ----

class FenceBridge:
    def __init__(self, ok: bool = True) -> None:
        self.ok = ok
        self.uploaded: list[list[list[float]]] = []
        self.cleared = 0
        self.fence: list[list[float]] | None = None

    def vehicle_fence(self) -> list[list[float]] | None:
        return self.fence

    def upload_fence(self, polygon: list[list[float]]) -> bool:
        self.uploaded.append(polygon)
        if self.ok:
            self.fence = polygon
        return self.ok

    def clear_fence(self) -> bool:
        self.cleared += 1
        if self.ok:
            self.fence = []
        return self.ok

    def get_last_command_error(self) -> str:
        return "Geofence upload: rejected"


def _handler(tmp_path, bridge: Any = None) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    handler.geofence_path = str(tmp_path / "geofence.json")
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def test_the_page_reads_an_empty_area_before_anything_is_drawn(tmp_path) -> None:
    handler, responses = _handler(tmp_path)
    handler._api_geofence()
    payload, status = responses[0]
    assert status == 200
    assert payload["polygon"] == []
    assert payload["show_on_map"] is True
    assert payload["on_vehicle"] is None


def test_saving_keeps_the_area_and_the_toggle(tmp_path) -> None:
    handler, responses = _handler(tmp_path)
    handler._api_geofence_save({"polygon": SQUARE})
    handler._api_geofence_save({"show_on_map": False})
    payload, status = responses[-1]
    assert status == 200
    assert payload["polygon"] == SQUARE
    assert payload["show_on_map"] is False
    assert geofence.load(handler.geofence_path)["polygon"] == SQUARE


@pytest.mark.parametrize("payload", [
    {"polygon": [[0, 0], [1, 1]]},
    {"polygon": [[0, 0], [1, 1], [1, 0], [0, 1]]},
    {"show_on_map": "yes"},
])
def test_a_bad_save_is_refused_and_changes_nothing(tmp_path, payload: dict) -> None:
    handler, responses = _handler(tmp_path)
    handler._api_geofence_save({"polygon": SQUARE})
    handler._api_geofence_save(payload)
    assert responses[-1][1] == 400
    assert geofence.load(handler.geofence_path) == {"polygon": SQUARE, "show_on_map": True}


def test_upload_sends_the_stored_area_and_reports_it_on_board(tmp_path) -> None:
    bridge = FenceBridge()
    handler, responses = _handler(tmp_path, bridge)
    handler._api_geofence_save({"polygon": SQUARE})
    handler._api_geofence_upload({})
    payload, status = responses[-1]
    assert status == 200
    assert bridge.uploaded == [SQUARE]
    assert payload["on_vehicle"] is True


def test_an_area_changed_after_the_upload_is_not_on_board(tmp_path) -> None:
    bridge = FenceBridge()
    handler, responses = _handler(tmp_path, bridge)
    handler._api_geofence_save({"polygon": SQUARE})
    handler._api_geofence_upload({})
    handler._api_geofence_save({"polygon": SQUARE[:3]})
    assert responses[-1][0]["on_vehicle"] is False


def test_upload_without_an_area_or_a_link_is_refused(tmp_path) -> None:
    handler, responses = _handler(tmp_path, FenceBridge())
    handler._api_geofence_upload({})
    assert responses[-1][1] == 400
    handler, responses = _handler(tmp_path, None)
    handler._api_geofence_save({"polygon": SQUARE})
    handler._api_geofence_upload({})
    assert responses[-1][1] == 503


def test_a_refused_upload_says_why(tmp_path) -> None:
    handler, responses = _handler(tmp_path, FenceBridge(ok=False))
    handler._api_geofence_save({"polygon": SQUARE})
    handler._api_geofence_upload({})
    payload, status = responses[-1]
    assert status == 409
    assert "rejected" in payload["error"]


def test_clear_removes_the_vehicle_fence_and_keeps_the_area(tmp_path) -> None:
    bridge = FenceBridge()
    handler, responses = _handler(tmp_path, bridge)
    handler._api_geofence_save({"polygon": SQUARE})
    handler._api_geofence_clear({})
    payload, status = responses[-1]
    assert status == 200
    assert bridge.cleared == 1
    assert payload["polygon"] == SQUARE
    assert payload["vehicle_has_fence"] is False
