"""Tests for the /api/mission/* endpoints — the Mission planner's backend.

Two things are pinned here that corvus/mission.py's own tests cannot cover:

* **upload and start are one decision each.** ``start`` is a flag on the upload
  so the two cannot be reordered by a slow link, and a plan uploaded without it
  must leave the aircraft holding a route it has not been told to run. A bug
  that armed a vehicle on a plain upload is the worst thing this page could do.
* **whose fault a failure is.** 503 means the link is gone and the operator
  should try again; 409 means the vehicle answered and refused. The frontend
  shows the message either way, but the status is what a script reads.

Mirrors the handler fakes in tests/test_server_sethome.py.
"""
from __future__ import annotations

from typing import Any

import pytest

from corvus import mission
from corvus.config import CorvusConfig
from corvus.server import CorvusHandler


class FakeBridge:
    def __init__(self, upload: bool = True, start: bool = True,
                 clear: bool = True, error: str = "") -> None:
        self.upload_result = upload
        self.start_result = start
        self.clear_result = clear
        self.error = error
        self.uploaded: list[list[dict[str, Any]]] = []
        self.started: list[int] = []
        self.cleared = 0

    def upload_mission_plan(self, items: list[dict[str, Any]]) -> bool:
        self.uploaded.append(items)
        return self.upload_result

    def start_mission(self, count: int) -> bool:
        self.started.append(count)
        return self.start_result

    def clear_mission(self) -> bool:
        self.cleared += 1
        return self.clear_result

    def get_last_command_error(self) -> str:
        return self.error


def handler(bridge: FakeBridge | None,
            missions_dir: str = "") -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    obj = object.__new__(CorvusHandler)
    obj.mavlink = bridge  # type: ignore[assignment]
    obj.config = CorvusConfig(missions_dir=missions_dir)
    obj.config_path = ""
    responses: list[tuple[dict, int]] = []
    obj._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return obj, responses


def plan(**overrides: Any) -> dict[str, Any]:
    base: dict[str, Any] = {
        "name": "Test",
        "home": {"lat": 48.08, "lon": 11.64},
        "items": [
            {"type": "takeoff", "lat": 48.08, "lon": 11.64, "alt": 25},
            {"type": "waypoint", "lat": 48.09, "lon": 11.65, "alt": 40},
        ],
    }
    base.update(overrides)
    return base


# ---------------------------------------------------------------------------
# upload
# ---------------------------------------------------------------------------

def test_a_plain_upload_never_starts_the_mission() -> None:
    bridge = FakeBridge()
    obj, responses = handler(bridge)

    obj._api_mission_upload({"plan": plan()})

    assert len(bridge.uploaded) == 1
    assert bridge.started == [], "an upload must not arm anything"
    assert responses == [({"ok": True, "items": 2, "started": False}, 200)]


def test_upload_with_start_uploads_then_starts_in_that_order() -> None:
    bridge = FakeBridge()
    obj, responses = handler(bridge)

    obj._api_mission_upload({"plan": plan(), "start": True})

    assert len(bridge.uploaded) == 1
    assert bridge.started == [2], "started with the item count that was uploaded"
    assert responses[0][0]["started"] is True


def test_a_failed_upload_never_reaches_the_start() -> None:
    bridge = FakeBridge(upload=False, error="denied by the autopilot")
    obj, responses = handler(bridge)

    obj._api_mission_upload({"plan": plan(), "start": True})

    assert bridge.started == []
    assert responses[0][1] == 409
    assert responses[0][0]["error"] == "denied by the autopilot"


def test_a_dead_link_reports_503_rather_than_a_refusal() -> None:
    bridge = FakeBridge(upload=False, error="Mission upload failed: DISCONNECTED")
    obj, responses = handler(bridge)

    obj._api_mission_upload({"plan": plan()})

    assert responses[0][1] == 503


def test_an_empty_mission_is_refused_before_the_bridge() -> None:
    bridge = FakeBridge()
    obj, responses = handler(bridge)

    obj._api_mission_upload({"plan": {"items": []}})

    assert responses[0][1] == 400
    assert bridge.uploaded == []


def test_an_invalid_plan_is_refused_with_the_validators_reason() -> None:
    bridge = FakeBridge()
    obj, responses = handler(bridge)

    obj._api_mission_upload({"plan": {"items": [{"type": "teleport"}]}})

    assert responses[0][1] == 400
    assert "teleport" in responses[0][0]["error"]
    assert bridge.uploaded == []


@pytest.mark.parametrize("start", ["yes", 1, [], {}])
def test_start_must_be_a_boolean(start: Any) -> None:
    bridge = FakeBridge()
    obj, responses = handler(bridge)

    obj._api_mission_upload({"plan": plan(), "start": start})

    assert responses[0][1] == 400
    assert bridge.uploaded == []


def test_uploading_without_a_link_is_a_400_not_a_crash() -> None:
    obj, responses = handler(None)

    obj._api_mission_upload({"plan": plan()})

    assert responses[0][1] == 400
    assert responses[0][0]["error"] == "not connected"


def test_the_uploaded_items_are_the_lowered_plan() -> None:
    bridge = FakeBridge()
    obj, _responses = handler(bridge)

    obj._api_mission_upload({"plan": plan(speed=8)})

    commands = [item["command"] for item in bridge.uploaded[0]]
    assert commands == [
        mission.MAV_CMD_DO_CHANGE_SPEED,
        mission.MAV_CMD_NAV_TAKEOFF,
        mission.MAV_CMD_NAV_WAYPOINT,
    ]


# ---------------------------------------------------------------------------
# start / clear
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("count", [0, -1, "2", None, True, 2.5])
def test_start_refuses_an_item_count_that_is_not_one(count: Any) -> None:
    bridge = FakeBridge()
    obj, responses = handler(bridge)

    obj._api_mission_start({"items": count})

    assert responses[0][1] == 400
    assert bridge.started == []


def test_start_refuses_a_count_past_the_upload_ceiling() -> None:
    bridge = FakeBridge()
    obj, responses = handler(bridge)

    obj._api_mission_start({"items": 100000})

    assert responses[0][1] == 400
    assert bridge.started == []


def test_start_forwards_the_count_and_reports_success() -> None:
    bridge = FakeBridge()
    obj, responses = handler(bridge)

    obj._api_mission_start({"items": 7})

    assert bridge.started == [7]
    assert responses == [({"ok": True}, 200)]


def test_clear_wipes_the_vehicles_mission() -> None:
    bridge = FakeBridge()
    obj, responses = handler(bridge)

    obj._api_mission_clear({})

    assert bridge.cleared == 1
    assert responses == [({"ok": True}, 200)]


def test_clear_reports_a_refusal_as_409() -> None:
    bridge = FakeBridge(clear=False, error="Mission clear failed: DENIED")
    obj, responses = handler(bridge)

    obj._api_mission_clear({})

    assert responses[0][1] == 409


# ---------------------------------------------------------------------------
# the plan store
# ---------------------------------------------------------------------------

def test_save_list_load_and_remove_through_the_endpoints(tmp_path: Any) -> None:
    obj, responses = handler(None, missions_dir=str(tmp_path))

    obj._api_mission_plans_save({"name": "Ridge run", "plan": plan()})
    assert responses[-1][0]["ok"] is True
    assert responses[-1][0]["name"] == "Ridge run"

    obj._api_mission_plans()
    listed = responses[-1][0]["plans"]
    assert [entry["name"] for entry in listed] == ["Ridge run"]

    obj._api_mission_plans_load({"name": "Ridge run"})
    loaded = responses[-1][0]["plan"]
    assert [item["type"] for item in loaded["items"]] == ["takeoff", "waypoint"]

    obj._api_mission_plans_remove({"name": "Ridge run"})
    assert responses[-1][0] == {"ok": True}

    obj._api_mission_plans()
    assert responses[-1][0]["plans"] == []


def test_saving_an_invalid_plan_writes_nothing(tmp_path: Any) -> None:
    obj, responses = handler(None, missions_dir=str(tmp_path))

    obj._api_mission_plans_save({"name": "Bad", "plan": {"items": [{"type": "x"}]}})

    assert responses[-1][1] == 400
    assert list(tmp_path.iterdir()) == []


def test_loading_a_mission_that_is_not_there_is_a_404(tmp_path: Any) -> None:
    obj, responses = handler(None, missions_dir=str(tmp_path))

    obj._api_mission_plans_load({"name": "Nothing"})

    assert responses[-1][1] == 404


@pytest.mark.parametrize("name", ["", "...", None, 42])
def test_a_name_that_sanitizes_to_nothing_is_a_400(tmp_path: Any, name: Any) -> None:
    obj, responses = handler(None, missions_dir=str(tmp_path))

    obj._api_mission_plans_load({"name": name})

    assert responses[-1][1] == 400


def test_a_traversal_name_is_sanitized_before_it_reaches_the_disk(tmp_path: Any) -> None:
    obj, responses = handler(None, missions_dir=str(tmp_path))

    obj._api_mission_plans_save({"name": "../../escape", "plan": plan()})

    assert responses[-1][0]["name"] == "escape"
    assert [p.name for p in tmp_path.iterdir()] == ["escape.json"]
