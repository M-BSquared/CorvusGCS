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


@pytest.mark.parametrize("speed", [0.0, -5, 1000, "12", True])
def test_an_out_of_range_speed_on_one_item_is_refused(speed: Any) -> None:
    assert mission.validate_plan(plan(items=[
        {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20, "speed": speed},
    ]))[0] is None


def test_a_speed_pinned_on_one_item_survives_and_the_others_stay_absent() -> None:
    """Absent is not zero and not a default: a point without a speed of its own
    inherits the one in force, which no number on the item can express."""
    cleaned, _error = mission.validate_plan(plan(items=[
        {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20, "speed": 4},
        {"type": "waypoint", "lat": 48.1, "lon": 11.1, "alt": 20},
    ]))

    assert cleaned is not None
    assert cleaned["items"][0]["speed"] == 4.0
    assert "speed" not in cleaned["items"][1]


def test_a_plan_is_refused_when_its_speed_changes_push_it_over_the_ceiling() -> None:
    """The limit is what goes on the wire. Each pinned speed is a command of
    its own, so a plan that fits the page can still overflow the vehicle."""
    items = [
        {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20,
         "speed": 5 + (index % 2)}
        for index in range(mission.MISSION_MAX_ITEMS - 1)
    ]

    cleaned, error = mission.validate_plan({"items": items})

    assert cleaned is None
    assert str(mission.MISSION_MAX_ITEMS) in error


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
# The aircraft a plan is for
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("aircraft", mission.PLAN_AIRCRAFT)
def test_the_plan_aircraft_survives_validation(aircraft: str) -> None:
    cleaned, error = mission.validate_plan(plan(aircraft=aircraft))

    assert error == ""
    assert cleaned is not None
    assert cleaned["aircraft"] == aircraft


@pytest.mark.parametrize("aircraft", [None, "", "zeppelin", 1, ["multirotor"]])
def test_an_unknown_plan_aircraft_is_dropped_rather_than_refusing_the_plan(
        aircraft: Any) -> None:
    cleaned, error = mission.validate_plan(plan(aircraft=aircraft))

    assert error == ""
    assert cleaned is not None
    assert "aircraft" not in cleaned


def test_the_plan_aircraft_never_reaches_the_wire() -> None:
    hold = {"type": "loiter_time", "lat": 48.0, "lon": 11.0, "alt": 30,
            "seconds": 20, "radius": 60}
    as_multicopter, _ = mission.validate_plan(plan(items=[hold], aircraft="multirotor"))
    as_fixed_wing, _ = mission.validate_plan(plan(items=[hold], aircraft="fixed_wing"))

    assert as_multicopter is not None and as_fixed_wing is not None
    assert mission.plan_to_items(as_multicopter) == mission.plan_to_items(as_fixed_wing)


def test_the_plan_aircraft_is_saved_with_the_plan(tmp_path: Any) -> None:
    cleaned, _ = mission.validate_plan(plan(aircraft="multirotor"))
    assert cleaned is not None
    name = mission.write_plan(str(tmp_path), "Kopter", cleaned)

    loaded = mission.read_plan(str(tmp_path), name)

    assert loaded is not None
    assert loaded["aircraft"] == "multirotor"


# ---------------------------------------------------------------------------
# The height a multicopter's landing descends from
# ---------------------------------------------------------------------------

def _landing_plan(aircraft: Any = "multirotor", **land: Any) -> dict[str, Any]:
    cleaned, error = mission.validate_plan(plan(aircraft=aircraft, items=[
        {"type": "takeoff", "lat": 48.08, "lon": 11.64, "alt": 25},
        {"type": "waypoint", "lat": 48.09, "lon": 11.65, "alt": 40},
        {"type": "land", "lat": 48.10, "lon": 11.66, "alt": 0, **land},
    ]))
    assert error == ""
    assert cleaned is not None
    return cleaned


def test_a_landing_keeps_its_descent_height_and_an_unset_one_stays_absent() -> None:
    assert _landing_plan(approach_alt=12)["items"][2]["approach_alt"] == 12.0
    assert "approach_alt" not in _landing_plan()["items"][2]


def test_only_a_landing_carries_a_descent_height() -> None:
    cleaned, _ = mission.validate_plan(plan(items=[
        {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20, "approach_alt": 5},
    ]))
    assert cleaned is not None
    assert "approach_alt" not in cleaned["items"][0]


def test_a_descent_may_start_as_low_as_the_floor_and_no_lower() -> None:
    # All the way down is a dive; the floor keeps the touchdown itself vertical.
    assert _landing_plan(approach_alt=mission.MISSION_APPROACH_MIN_M)["items"][2][
        "approach_alt"] == mission.MISSION_APPROACH_MIN_M


@pytest.mark.parametrize("approach", [0, 2.9, -101, 1001, "high", float("nan"), True])
def test_a_descent_height_outside_the_altitude_bounds_is_refused(approach: Any) -> None:
    cleaned, error = mission.validate_plan(plan(aircraft="multirotor", items=[
        {"type": "land", "lat": 48.0, "lon": 11.0, "alt": 0, "approach_alt": approach},
    ]))
    assert cleaned is None
    assert "approach_alt" in error


def test_a_multicopter_descends_from_a_waypoint_right_above_the_landing() -> None:
    # PX4 and ArduCopter both fly NAV_LAND in at the height they already
    # have, so the height the descent starts from is a waypoint of its own.
    wire = mission.plan_to_items(_landing_plan(approach_alt=12, speed=3))

    assert [entry["command"] for entry in wire] == [
        mission.MAV_CMD_NAV_TAKEOFF, mission.MAV_CMD_NAV_WAYPOINT,
        mission.MAV_CMD_DO_CHANGE_SPEED, mission.MAV_CMD_NAV_WAYPOINT,
        mission.MAV_CMD_NAV_LAND,
    ], "the landing's speed is flown on the leg into the descent point"
    above, land = wire[3], wire[4]
    assert (above["lat"], above["lon"], above["alt"]) == (land["lat"], land["lon"], 12.0)
    assert above["params"][0] == 0.0, "no hold before the descent"
    assert math.isnan(above["params"][3]), "and no heading asked for"
    assert above["item"] == land["item"] == 2, "progress reads it as the landing"


@pytest.mark.parametrize("aircraft", ["fixed_wing", "vtol", None])
def test_no_descent_point_is_sent_for_anything_but_a_multicopter(aircraft: Any) -> None:
    # A fixed wing sent right above its touchdown would have to dive into it.
    wire = mission.plan_to_items(_landing_plan(aircraft, approach_alt=12))

    assert [entry["command"] for entry in wire] == [
        mission.MAV_CMD_NAV_TAKEOFF, mission.MAV_CMD_NAV_WAYPOINT, mission.MAV_CMD_NAV_LAND,
    ]


def test_an_unset_descent_height_sends_nothing_extra() -> None:
    wire = mission.plan_to_items(_landing_plan())
    assert len(wire) == 3


# ---------------------------------------------------------------------------
# A point's own name
# ---------------------------------------------------------------------------

def test_a_point_name_survives_validation_and_an_unnamed_point_carries_none() -> None:
    cleaned, error = mission.validate_plan(plan(items=[
        {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20, "name": "Nordhang Süd"},
        {"type": "waypoint", "lat": 48.1, "lon": 11.1, "alt": 20},
        {"type": "rtl", "name": "Heim"},
    ]))

    assert error == ""
    assert cleaned is not None
    assert cleaned["items"][0]["name"] == "Nordhang Süd"
    assert "name" not in cleaned["items"][1]
    assert cleaned["items"][2]["name"] == "Heim"


@pytest.mark.parametrize("raw, expected", [
    ("  Ridge  ", "Ridge"),
    ("Ridge\nnorth\tside", "Ridge north side"),
    ("a\x00b\x1bc d", "a b c d"),
    ("", ""),
    ("   ", ""),
    (None, ""),
    (42, ""),
    (["Ridge"], ""),
])
def test_a_point_name_is_cleaned_to_one_line(raw: Any, expected: str) -> None:
    assert mission.clean_point_name(raw) == expected


def test_a_point_name_that_is_not_one_is_dropped_rather_than_refusing_the_plan() -> None:
    # A name is a label. A plan must never fail to upload over one.
    cleaned, error = mission.validate_plan(plan(items=[
        {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20, "name": 7},
        {"type": "waypoint", "lat": 48.1, "lon": 11.1, "alt": 20, "name": " \n "},
    ]))

    assert error == ""
    assert cleaned is not None
    assert all("name" not in item for item in cleaned["items"])


def test_a_long_point_name_is_cut_at_the_limit_counted_in_characters() -> None:
    long = "ü" * (mission.MISSION_POINT_NAME_MAX + 10)
    assert mission.clean_point_name(long) == "ü" * mission.MISSION_POINT_NAME_MAX


def test_a_point_name_never_reaches_the_wire() -> None:
    named, _error = mission.validate_plan(plan(items=[
        {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20, "name": "Ridge"},
    ]))
    bare, _error = mission.validate_plan(plan(items=[
        {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20},
    ]))
    assert named is not None and bare is not None

    lowered = mission.plan_to_items(named)
    assert all("name" not in entry for entry in lowered)
    assert json.dumps(lowered, default=str) == json.dumps(mission.plan_to_items(bare), default=str)


def test_a_point_name_is_saved_with_the_plan(tmp_path: Any) -> None:
    directory = str(tmp_path)
    cleaned, _error = mission.validate_plan(plan(items=[
        {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20, "name": "Fotopunkt 1"},
    ]))
    assert cleaned is not None

    stored = mission.write_plan(directory, "Named", cleaned)
    loaded = mission.read_plan(directory, stored)

    assert loaded is not None
    assert loaded["items"][0]["name"] == "Fotopunkt 1"


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
    assert waypoint["params"][:3] == [10.0, 5.0, 0.0], "hold in p1, accept radius in p2"
    assert math.isnan(waypoint["params"][3]), "no yaw asked for: 0 would mean face north"


def test_takeoff_waypoint_and_land_ask_for_no_heading() -> None:
    """param4 of NAV_TAKEOFF, NAV_WAYPOINT and NAV_LAND is a yaw on PX4, and
    NaN is "use the heading mode". A 0 there turned the aircraft north."""
    cleaned, _error = mission.validate_plan({"items": [
        {"type": "takeoff", "lat": 48.0, "lon": 11.0, "alt": 20},
        {"type": "waypoint", "lat": 48.1, "lon": 11.1, "alt": 30},
        {"type": "land", "lat": 48.2, "lon": 11.2, "alt": 0},
    ]})
    assert cleaned is not None
    for item in mission.plan_to_items(cleaned):
        assert math.isnan(item["params"][3]), item["command"]


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


def test_a_speed_pinned_on_an_item_is_emitted_just_before_it() -> None:
    """The speed is set on the leg INTO the point, so the change has to be
    executed before the aircraft starts flying that leg."""
    cleaned, _error = mission.validate_plan({"items": [
        {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20},
        {"type": "waypoint", "lat": 48.1, "lon": 11.1, "alt": 20, "speed": 4},
    ]})
    assert cleaned is not None

    first, change, second = mission.plan_to_items(cleaned)

    assert first["command"] == mission.MAV_CMD_NAV_WAYPOINT
    assert change["command"] == mission.MAV_CMD_DO_CHANGE_SPEED
    assert change["params"][1] == 4.0
    assert second["command"] == mission.MAV_CMD_NAV_WAYPOINT


def test_a_pinned_speed_holds_until_another_item_changes_it() -> None:
    """PX4 keeps flying at the last DO_CHANGE_SPEED it executed, so repeating
    a speed that is already in force would spend a mission slot saying nothing.
    """
    cleaned, _error = mission.validate_plan(plan(speed=9, items=[
        {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20, "speed": 9},
        {"type": "waypoint", "lat": 48.1, "lon": 11.1, "alt": 20, "speed": 4},
        {"type": "waypoint", "lat": 48.2, "lon": 11.2, "alt": 20, "speed": 4},
        {"type": "waypoint", "lat": 48.3, "lon": 11.3, "alt": 20, "speed": 9},
    ]))
    assert cleaned is not None

    speeds = [item["params"][1] for item in mission.plan_to_items(cleaned)
              if item["command"] == mission.MAV_CMD_DO_CHANGE_SPEED]

    assert speeds == [9.0, 4.0, 9.0]


def test_an_item_speed_is_emitted_even_when_the_plan_pins_none() -> None:
    cleaned, _error = mission.validate_plan({"items": [
        {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 20, "speed": 6},
    ]})
    assert cleaned is not None

    change, waypoint = mission.plan_to_items(cleaned)

    assert change["command"] == mission.MAV_CMD_DO_CHANGE_SPEED
    assert change["params"][1] == 6.0
    assert (change["lat"], change["lon"], change["alt"]) == (0.0, 0.0, 0.0)
    assert waypoint["command"] == mission.MAV_CMD_NAV_WAYPOINT


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


# ---------------------------------------------------------------------------
# items_to_plan: a mission read off the vehicle, back into a plan
# ---------------------------------------------------------------------------

def _nav(command: int, lat: float = 48.1, lon: float = 11.6, alt: float = 30.0,
         params: list[Any] | None = None, frame: int = 3) -> dict[str, Any]:
    return {"command": command, "frame": frame, "lat": lat, "lon": lon, "alt": alt,
            "params": params if params is not None else [0.0, 0.0, 0.0, None]}


def test_an_item_above_sea_level_is_read_as_height_above_home() -> None:
    out = mission.items_to_plan([_nav(mission.MAV_CMD_NAV_WAYPOINT, alt=545.0, frame=0)],
                                home_alt_amsl=515.0)
    assert out["plan"]["items"][0]["alt"] == 30.0


def test_an_item_above_sea_level_with_no_home_altitude_is_skipped_and_said() -> None:
    out = mission.items_to_plan([_nav(mission.MAV_CMD_NAV_WAYPOINT, frame=5)])
    assert out["plan"]["items"] == []
    assert "sea level" in out["skipped"][0]["reason"]
    assert out["plan_index"] == [None]


def test_an_altitude_above_terrain_is_not_passed_off_as_one_above_home() -> None:
    out = mission.items_to_plan([_nav(mission.MAV_CMD_NAV_WAYPOINT, frame=10)])
    assert out["plan"]["items"] == []
    assert "terrain" in out["skipped"][0]["reason"]


def test_a_command_the_planner_does_not_draw_is_named_not_dropped() -> None:
    out = mission.items_to_plan([
        _nav(mission.MAV_CMD_NAV_WAYPOINT),
        {"command": 183, "frame": 2, "lat": 0, "lon": 0, "alt": 0,
         "params": [9, 1500, 0, 0]},                      # DO_SET_SERVO
        _nav(mission.MAV_CMD_NAV_LAND, alt=0.0),
    ])
    assert [i["type"] for i in out["plan"]["items"]] == ["waypoint", "land"]
    assert out["skipped"] == [{"seq": 1, "command": 183,
                               "reason": "command 183 is not one the planner draws"}]
    assert out["plan_index"] == [0, None, 1]


def test_a_value_outside_the_planners_range_is_held_and_reported() -> None:
    out = mission.items_to_plan([_nav(mission.MAV_CMD_NAV_WAYPOINT,
                                      params=[7200.0, 0.0, 0.0, None])])
    assert out["plan"]["items"][0]["hold"] == mission.MISSION_HOLD_MAX_S
    assert out["adjusted"][0]["field"] == "hold"


def test_a_speed_change_that_changes_nothing_is_not_a_speed() -> None:
    """-1 and 0 are DO_CHANGE_SPEED's "no change"."""
    out = mission.items_to_plan([
        {"command": mission.MAV_CMD_DO_CHANGE_SPEED, "frame": 2, "lat": 0, "lon": 0,
         "alt": 0, "params": [1.0, -1.0, -1.0, 0.0]},
        _nav(mission.MAV_CMD_NAV_WAYPOINT),
    ])
    assert "speed" not in out["plan"]
    assert "speed" not in out["plan"]["items"][0]
    assert out["plan_index"] == [0, 0]


def test_an_orbit_reads_its_direction_from_the_sign_of_the_radius() -> None:
    out = mission.items_to_plan([_nav(mission.MAV_CMD_NAV_LOITER_TIME,
                                      params=[20.0, 0.0, -45.0, None])])
    item = out["plan"]["items"][0]
    assert (item["seconds"], item["radius"], item["direction"]) == (20.0, 45.0, -1.0)


def test_a_landing_with_no_place_is_drawn_where_the_last_point_was() -> None:
    out = mission.items_to_plan([
        _nav(mission.MAV_CMD_NAV_WAYPOINT, lat=48.2, lon=11.7),
        _nav(mission.MAV_CMD_NAV_LAND, lat=0.0, lon=0.0, alt=0.0),
    ])
    land = out["plan"]["items"][1]
    assert (land["lat"], land["lon"], land["alt"]) == (48.2, 11.7, 0.0)
    assert out["adjusted"][0]["field"] == "position"


def test_every_item_a_plan_can_hold_survives_the_round_trip() -> None:
    plan, _error = mission.validate_plan({"speed": 6, "items": [
        {"type": "takeoff", "lat": 48.0, "lon": 11.0, "alt": 20, "pitch": 12},
        {"type": "waypoint", "lat": 48.1, "lon": 11.1, "alt": 30, "hold": 4,
         "accept_radius": 3, "speed": 9},
        {"type": "loiter_turns", "lat": 48.2, "lon": 11.2, "alt": 30, "turns": 1.5,
         "radius": 70, "direction": -1},
        {"type": "loiter_time", "lat": 48.3, "lon": 11.3, "alt": 35, "seconds": 40,
         "radius": 55},
        {"type": "land", "lat": 48.4, "lon": 11.4},
        {"type": "rtl"},
    ]})
    assert plan is not None
    wire = [dict(item, frame=2 if item["command"] == mission.MAV_CMD_DO_CHANGE_SPEED else 3)
            for item in mission.plan_to_items(plan)]
    out = mission.items_to_plan(wire)
    assert out["plan"]["items"] == plan["items"]
    assert out["plan"]["speed"] == plan["speed"]
    assert out["plan_index"] == [item["item"] for item in mission.plan_to_items(plan)]


# ---------------------------------------------------------------------------
# What travels without a place, and what a stack will not fly
# ---------------------------------------------------------------------------

def test_a_return_and_a_speed_change_name_no_place() -> None:
    """They travel in MAV_FRAME_MISSION. PX4 v1.16 to v1.18 refuse a Return
    in a global frame, which failed every plan that ended with one."""
    assert mission.MAV_CMD_NAV_RETURN_TO_LAUNCH in mission.POSITIONLESS_COMMANDS
    assert mission.MAV_CMD_DO_CHANGE_SPEED in mission.POSITIONLESS_COMMANDS
    for name, spec in mission.ITEM_SPECS.items():
        if spec["position"]:
            assert spec["command"] not in mission.POSITIONLESS_COMMANDS, name


def test_a_stacks_refusals_are_named_in_the_planners_words() -> None:
    refusals = {mission.MAV_CMD_NAV_LOITER_TURNS: "not here", 12345: "not a planner item"}
    assert mission.refused_item_types(refusals) == {"loiter_turns": "not here"}
    assert mission.refused_item_types({}) == {}


def test_a_command_is_labelled_as_the_planner_labels_it() -> None:
    assert mission.label_of_command(mission.MAV_CMD_NAV_LOITER_TURNS) == "Circle"
    assert mission.label_of_command(mission.MAV_CMD_NAV_RETURN_TO_LAUNCH) == "Return"
    assert mission.label_of_command(999) == "command 999"


@pytest.mark.parametrize("value, kept", [
    (True, True), (False, False), (1, False), ("true", False), (None, False),
])
def test_start_at_vehicle_is_kept_only_when_it_is_true(value: Any, kept: bool) -> None:
    raw = plan()
    raw["start_at_vehicle"] = value
    cleaned, error = mission.validate_plan(raw)
    assert error == "" and cleaned is not None
    assert cleaned.get("start_at_vehicle") is (True if kept else None)


def test_start_at_vehicle_never_reaches_the_wire() -> None:
    raw = plan()
    cleaned, _error = mission.validate_plan(raw)
    tied, _error = mission.validate_plan(dict(raw, start_at_vehicle=True))
    assert cleaned is not None and tied is not None
    # Compared as text: an unset yaw is NaN, and NaN is never equal to itself.
    assert json.dumps(mission.plan_to_items(tied)) == json.dumps(mission.plan_to_items(cleaned))
