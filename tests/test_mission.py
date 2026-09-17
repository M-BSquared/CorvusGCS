"""Tests for corvus/mission.py — the mission plan model and its file store.

This module is the ONLY gate between an operator's drawing and a route the
aircraft flies, and it is reached from an HTTP endpoint, so most of what is
pinned here is refusal: a plan that is not a plan, a coordinate that is not a
coordinate, a name that is a path. The rest pins the lowering to MAVLink —
which parameter slot each named field lands in, because getting that wrong
would silently turn a 50 m orbit into 50 turns.
"""
from __future__ import annotations

import json
import math
import os
from typing import Any

import pytest

from corvus import mission


def plan(**overrides: Any) -> dict[str, Any]:
    """A minimal valid plan, with overrides merged in."""
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
# validate_plan
# ---------------------------------------------------------------------------

def test_a_minimal_plan_validates_and_keeps_its_order() -> None:
    cleaned, error = mission.validate_plan(plan())

    assert error == ""
    assert cleaned is not None
    assert [item["type"] for item in cleaned["items"]] == ["takeoff", "waypoint"]
    assert cleaned["home"] == {"lat": 48.08, "lon": 11.64}
    assert cleaned["name"] == "Test"


def test_absent_parameters_take_the_documented_default() -> None:
    cleaned, _error = mission.validate_plan(plan(items=[
        {"type": "loiter_turns", "lat": 48.08, "lon": 11.64, "alt": 30},
    ]))

    assert cleaned is not None
    assert cleaned["items"][0]["turns"] == 1.0
    assert cleaned["items"][0]["radius"] == 50.0


def test_a_landing_is_always_on_the_ground() -> None:
    """A touchdown 40 m up is not a landing, whatever the payload says."""
    cleaned, _error = mission.validate_plan(plan(items=[
        {"type": "land", "lat": 48.08, "lon": 11.64, "alt": 40},
    ]))

    assert cleaned is not None
    assert cleaned["items"][0]["alt"] == 0.0


def test_a_return_carries_no_position() -> None:
    cleaned, _error = mission.validate_plan(plan(items=[{"type": "rtl"}]))

    assert cleaned is not None
    assert "lat" not in cleaned["items"][0]
    assert "lon" not in cleaned["items"][0]


def test_an_empty_plan_is_valid_but_empty() -> None:
    """The editor saves a plan the operator is still drawing; only the upload
    endpoint decides that nothing is not enough to fly."""
    cleaned, error = mission.validate_plan({"items": []})

    assert error == ""
    assert cleaned is not None and cleaned["items"] == []


@pytest.mark.parametrize("raw", [
    None,
    [],
    "mission",
    {"items": "not a list"},
    {"items": {}},
])
def test_a_plan_that_is_not_a_plan_is_refused(raw: Any) -> None:
    cleaned, error = mission.validate_plan(raw)

    assert cleaned is None
    assert error


def test_an_oversized_plan_is_refused_by_count() -> None:
    items = [{"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20}] * (
        mission.MISSION_MAX_ITEMS + 1)

    cleaned, error = mission.validate_plan({"items": items})

    assert cleaned is None
    assert str(mission.MISSION_MAX_ITEMS) in error


@pytest.mark.parametrize("item", [
    {"type": "teleport", "lat": 48.0, "lon": 11.0, "alt": 20},
    {"type": None, "lat": 48.0, "lon": 11.0, "alt": 20},
    "waypoint",
    42,
])
def test_an_unknown_item_type_is_refused(item: Any) -> None:
    cleaned, error = mission.validate_plan({"items": [item]})

    assert cleaned is None
    assert error


@pytest.mark.parametrize("lat,lon", [
    (91.0, 11.0), (-91.0, 11.0), (48.0, 181.0), (48.0, -181.0),
    (float("nan"), 11.0), (48.0, float("inf")),
    ("48.0", 11.0), (None, 11.0),
    # bool is a subclass of int; True must never fly as latitude 1.0.
    (True, 11.0), (48.0, False),
])
def test_a_coordinate_that_is_not_one_is_refused(lat: Any, lon: Any) -> None:
    cleaned, _error = mission.validate_plan(
        {"items": [{"type": "waypoint", "lat": lat, "lon": lon, "alt": 20}]})

    assert cleaned is None


@pytest.mark.parametrize("alt", [
    mission.MISSION_ALT_MAX_M + 1,
    mission.MISSION_ALT_MIN_M - 1,
    float("nan"),
    "30",
    None,
    True,
])
def test_an_altitude_outside_the_plan_bounds_is_refused(alt: Any) -> None:
    cleaned, _error = mission.validate_plan(
        {"items": [{"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": alt}]})

    assert cleaned is None


def test_a_negative_altitude_inside_the_bounds_is_accepted() -> None:
    """Home is not always the lowest ground on the route: a leg down into a
    valley is a real flight, not a typo."""
    cleaned, error = mission.validate_plan(
        {"items": [{"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": -40}]})

    assert error == ""
    assert cleaned is not None and cleaned["items"][0]["alt"] == -40.0


@pytest.mark.parametrize("radius", [0.0, mission.MISSION_RADIUS_MAX_M + 1, "50"])
def test_an_impossible_orbit_radius_is_refused(radius: Any) -> None:
    cleaned, _error = mission.validate_plan({"items": [
        {"type": "loiter_turns", "lat": 48.0, "lon": 11.0, "alt": 30, "radius": radius},
    ]})

    assert cleaned is None


def test_home_must_be_an_object_with_real_coordinates() -> None:
    assert mission.validate_plan(plan(home="48.08,11.64"))[0] is None
    assert mission.validate_plan(plan(home={"lat": 48.08}))[0] is None


def test_home_may_be_absent() -> None:
    cleaned, error = mission.validate_plan({"items": []})

    assert error == ""
    assert cleaned is not None and "home" not in cleaned


@pytest.mark.parametrize("speed", [0.0, -5, 1000, "12", True])
def test_an_out_of_range_cruise_speed_is_refused(speed: Any) -> None:
    assert mission.validate_plan(plan(speed=speed))[0] is None


def test_a_valid_cruise_speed_survives() -> None:
    cleaned, _error = mission.validate_plan(plan(speed=12.5))

    assert cleaned is not None and cleaned["speed"] == 12.5


def test_unknown_keys_are_dropped_rather_than_carried() -> None:
    cleaned, _error = mission.validate_plan(plan(
        items=[{"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20,
                "surprise": "payload"}],
        extra="ignored",
    ))

    assert cleaned is not None
    assert "surprise" not in cleaned["items"][0]
    assert "extra" not in cleaned


# ---------------------------------------------------------------------------
# plan_to_items — the lowering to MAVLink
# ---------------------------------------------------------------------------

def test_each_named_field_lands_in_its_documented_parameter_slot() -> None:
    cleaned, _error = mission.validate_plan({"items": [
        {"type": "loiter_turns", "lat": 48.0, "lon": 11.0, "alt": 30,
         "turns": 3, "radius": 80},
        {"type": "loiter_time", "lat": 48.1, "lon": 11.1, "alt": 30,
         "seconds": 45, "radius": 60},
        {"type": "waypoint", "lat": 48.2, "lon": 11.2, "alt": 30,
         "hold": 10, "accept_radius": 5},
    ]})
    assert cleaned is not None
    items = mission.plan_to_items(cleaned)

    turns, hold, waypoint = items
    assert turns["command"] == mission.MAV_CMD_NAV_LOITER_TURNS
    assert turns["params"] == [3.0, 0.0, 80.0, 0.0], "turns in p1, radius in p3"
    assert hold["command"] == mission.MAV_CMD_NAV_LOITER_TIME
    assert hold["params"] == [45.0, 0.0, 60.0, 0.0], "seconds in p1, radius in p3"
    assert waypoint["command"] == mission.MAV_CMD_NAV_WAYPOINT
    assert waypoint["params"] == [10.0, 5.0, 0.0, 0.0], "hold in p1, accept radius in p2"


def test_the_turn_direction_is_carried_as_the_sign_of_the_radius() -> None:
    """PX4 v1.16-v1.18 read MAV_CMD_NAV_LOITER_*'s param3 that way. The plan
    keeps a radius a length, because a negative distance is not something an
    operator should have to type to turn the other way."""
    cleaned, _error = mission.validate_plan({"items": [
        {"type": "loiter_turns", "lat": 48.0, "lon": 11.0, "alt": 30,
         "turns": 2, "radius": 80, "direction": -1},
        {"type": "loiter_time", "lat": 48.0, "lon": 11.0, "alt": 30,
         "seconds": 20, "radius": 60, "direction": 1},
    ]})
    assert cleaned is not None

    counter, clockwise = mission.plan_to_items(cleaned)

    assert counter["params"][2] == -80.0
    assert clockwise["params"][2] == 60.0


def test_direction_defaults_to_clockwise_when_a_plan_names_none() -> None:
    cleaned, _error = mission.validate_plan({"items": [
        {"type": "loiter_turns", "lat": 48.0, "lon": 11.0, "alt": 30, "radius": 80},
    ]})
    assert cleaned is not None

    assert cleaned["items"][0]["direction"] == 1.0
    assert mission.plan_to_items(cleaned)[0]["params"][2] == 80.0


@pytest.mark.parametrize("raw,expected", [
    (1, 1.0), (0.4, 1.0), (0, 1.0),
    (-1, -1.0), (-0.2, -1.0),
])
def test_direction_is_one_of_two_things_not_a_range(raw: Any, expected: float) -> None:
    """A 0 from a hand-edited file must resolve to a direction, not leave the
    orbit with a radius of zero once the sign is applied."""
    cleaned, _error = mission.validate_plan({"items": [
        {"type": "loiter_turns", "lat": 48.0, "lon": 11.0, "alt": 30,
         "radius": 80, "direction": raw},
    ]})
    assert cleaned is not None
    assert cleaned["items"][0]["direction"] == expected
    assert abs(mission.plan_to_items(cleaned)[0]["params"][2]) == 80.0


@pytest.mark.parametrize("direction", [2, -5, "cw", float("nan")])
def test_a_direction_that_is_not_one_is_refused(direction: Any) -> None:
    cleaned, _error = mission.validate_plan({"items": [
        {"type": "loiter_turns", "lat": 48.0, "lon": 11.0, "alt": 30,
         "radius": 80, "direction": direction},
    ]})

    assert cleaned is None


def test_direction_is_not_a_command_parameter_of_its_own() -> None:
    """It rides on the radius, so nothing may land in a fourth slot for it."""
    cleaned, _error = mission.validate_plan({"items": [
        {"type": "loiter_turns", "lat": 48.0, "lon": 11.0, "alt": 30,
         "turns": 2, "radius": 80, "direction": -1},
    ]})
    assert cleaned is not None

    assert mission.plan_to_items(cleaned)[0]["params"] == [2.0, 0.0, -80.0, 0.0]


def test_a_positionless_item_is_lowered_with_zeroed_coordinates() -> None:
    cleaned, _error = mission.validate_plan({"items": [{"type": "rtl"}]})
    assert cleaned is not None

    item = mission.plan_to_items(cleaned)[0]

    assert item["command"] == mission.MAV_CMD_NAV_RETURN_TO_LAUNCH
    assert (item["lat"], item["lon"], item["alt"]) == (0.0, 0.0, 0.0)


def test_a_pinned_speed_is_emitted_before_every_navigation_item() -> None:
    """PX4 applies DO_CHANGE_SPEED when it executes it, so a speed change after
    the takeoff would leave the climb-out at the airframe default."""
    cleaned, _error = mission.validate_plan(plan(speed=9))
    assert cleaned is not None

    items = mission.plan_to_items(cleaned)

    assert items[0]["command"] == mission.MAV_CMD_DO_CHANGE_SPEED
    assert items[0]["params"][0] == 1.0, "1 = ground speed"
    assert items[0]["params"][1] == 9.0
    assert items[0]["params"][2] == -1.0, "-1 = leave the throttle alone"
    assert items[1]["command"] == mission.MAV_CMD_NAV_TAKEOFF


def test_no_speed_item_is_emitted_when_the_plan_pins_none() -> None:
    cleaned, _error = mission.validate_plan(plan())
    assert cleaned is not None

    commands = [item["command"] for item in mission.plan_to_items(cleaned)]

    assert mission.MAV_CMD_DO_CHANGE_SPEED not in commands


# ---------------------------------------------------------------------------
# distances
# ---------------------------------------------------------------------------

def test_leg_distance_matches_a_known_separation() -> None:
    # One degree of latitude is ~111.2 km anywhere on the sphere.
    metres = mission.leg_distance_m({"lat": 48.0, "lon": 11.0},
                                    {"lat": 49.0, "lon": 11.0})

    assert metres == pytest.approx(111195, rel=0.001)


def test_plan_distance_counts_home_and_the_orbit_circumference() -> None:
    cleaned, _error = mission.validate_plan({
        "home": {"lat": 48.0, "lon": 11.0},
        "items": [
            {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 30},
            {"type": "loiter_turns", "lat": 48.0, "lon": 11.0, "alt": 30,
             "turns": 2, "radius": 100},
        ],
    })
    assert cleaned is not None

    # Every point is on top of home, so the only distance is the two orbits.
    assert mission.plan_distance_m(cleaned) == pytest.approx(2 * 2 * math.pi * 100, rel=1e-6)


# ---------------------------------------------------------------------------
# names and the file store
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("raw,expected", [
    ("Ridge run", "Ridge run"),
    ("  padded  ", "padded"),
    ("slash/and\\back", "slash and back"),
    ("../../etc/passwd", "etc passwd"),
    ("...", ""),
    ("", ""),
    (None, ""),
    (42, ""),
    ("a" * 200, "a" * mission.MISSION_NAME_MAX),
])
def test_a_plan_name_can_never_address_a_path(raw: Any, expected: str) -> None:
    assert mission.safe_plan_name(raw) == expected


def test_save_load_list_and_remove_round_trip(tmp_path: Any) -> None:
    directory = str(tmp_path)
    cleaned, _error = mission.validate_plan(plan(name="Ridge run"))
    assert cleaned is not None

    stored = mission.write_plan(directory, "Ridge run", cleaned)
    assert stored == "Ridge run"
    assert os.path.isfile(os.path.join(directory, "Ridge run.json"))

    listed = mission.list_plans(directory)
    assert [entry["name"] for entry in listed] == ["Ridge run"]
    assert listed[0]["items"] == 2

    assert mission.read_plan(directory, "Ridge run") == cleaned

    assert mission.remove_plan(directory, "Ridge run") is True
    assert mission.list_plans(directory) == []


def test_removing_a_plan_that_is_not_there_is_a_success(tmp_path: Any) -> None:
    assert mission.remove_plan(str(tmp_path), "never existed") is True


def test_listing_a_directory_that_does_not_exist_is_empty(tmp_path: Any) -> None:
    assert mission.list_plans(os.path.join(str(tmp_path), "nope")) == []


def test_one_corrupt_file_does_not_hide_the_others(tmp_path: Any) -> None:
    directory = str(tmp_path)
    cleaned, _error = mission.validate_plan(plan(name="Good"))
    assert cleaned is not None
    mission.write_plan(directory, "Good", cleaned)
    (tmp_path / "Broken.json").write_text("{ not json", encoding="utf-8")
    (tmp_path / "Invalid.json").write_text(
        json.dumps({"items": [{"type": "teleport"}]}), encoding="utf-8")

    assert [entry["name"] for entry in mission.list_plans(directory)] == ["Good"]


def test_a_saved_plan_is_revalidated_on_the_way_back_out(tmp_path: Any) -> None:
    """A plan file is a document an operator may copy between machines, so it
    is not trusted just because it is on disk."""
    (tmp_path / "Tampered.json").write_text(
        json.dumps({"items": [{"type": "waypoint", "lat": 999, "lon": 11, "alt": 30}]}),
        encoding="utf-8")

    assert mission.read_plan(str(tmp_path), "Tampered") is None


def test_a_traversal_name_never_leaves_the_missions_directory(tmp_path: Any) -> None:
    outside = tmp_path.parent / "secret.json"
    outside.write_text(json.dumps({"items": []}), encoding="utf-8")

    # Sanitized to "secret", which is looked for INSIDE the directory and is
    # not there — the file one level up is never reached.
    assert mission.read_plan(str(tmp_path), "../secret") is None
    assert mission.read_plan(str(tmp_path), "...") is None


def test_write_plan_falls_back_to_the_plans_own_name(tmp_path: Any) -> None:
    cleaned, _error = mission.validate_plan(plan(name="Fallback"))
    assert cleaned is not None

    assert mission.write_plan(str(tmp_path), "...", cleaned) == "Fallback"


def test_missions_dir_defaults_under_the_corvus_home() -> None:
    assert mission.missions_dir("").endswith(os.path.join(".corvus", "missions"))
    assert mission.missions_dir("/pinned") == "/pinned"
