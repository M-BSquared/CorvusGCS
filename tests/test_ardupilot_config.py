"""The ArduPilot setup-page schemas.

The four setup pages — Motors, Safety & Sensors, PID Tuning, Radio Control —
are each a list of *parameter names*, and not one PX4 name exists on an
ArduPilot vehicle. The pages did not error on one; they came up empty, which is
a worse failure, because an empty Safety page looks like an aircraft with no
failsafes rather than a ground station that cannot read them.

These tests hold the schemas to the same rules the PX4 ones follow: the output
shape is identical (so the frontend cannot tell which stack built a page), a
parameter the firmware did not answer for is one field fewer rather than an
error, and no enum ever silently rewrites a value it does not recognise.
"""
from __future__ import annotations

import pytest
from pymavlink import mavutil as mv

from corvus import (
    ardupilot_motors,
    ardupilot_rc,
    ardupilot_safety,
    ardupilot_tuning,
    motor_config,
    rc_config,
    safety_config,
    tuning_config,
)

MODULES = [
    (ardupilot_safety, safety_config),
    (ardupilot_tuning, tuning_config),
    (ardupilot_rc, rc_config),
    (ardupilot_motors, motor_config),
]


# ---------------------------------------------------------------------------
# Shared rules
# ---------------------------------------------------------------------------

# The per-channel calibration numbers really are the same parameter on both
# stacks — RC3_MIN means the same thing to PX4 and to ArduPilot. They are the
# only legitimate overlap, and naming them here is what lets the check below be
# strict about everything else.
_SHARED_RC_NAMES = frozenset(
    f"RC{n}_{suffix}"
    for n in range(1, rc_config.MAX_CHANNELS + 1)
    for suffix in ("MIN", "MAX", "TRIM", "DZ")
)


@pytest.mark.parametrize(("apm", "px4"), MODULES)
def test_the_two_stacks_share_only_the_parameters_that_really_are_shared(
    apm, px4,
) -> None:
    """The check that proves these are real schemas and not renamed copies.

    A name claimed by both has to mean the same thing on both aircraft, and
    outside the RC endpoints none of them do — which is the whole reason a
    second schema exists.
    """
    shared = set(apm.param_names()) & set(px4.param_names())
    assert not (shared - _SHARED_RC_NAMES), (
        f"parameter names wrongly claimed by both stacks: "
        f"{sorted(shared - _SHARED_RC_NAMES)}"
    )


def test_the_names_that_look_shared_and_are_not() -> None:
    """One underscore apart, and each one failed silently: a write to a
    parameter the vehicle does not have is dropped, so the wizard reported
    success and changed nothing."""
    apm = set(ardupilot_rc.param_names())
    px4 = set(rc_config.param_names())
    assert "RC1_REVERSED" in apm and "RC1_REVERSED" not in px4
    assert "RC1_REV" in px4 and "RC1_REV" not in apm
    assert "RCMAP_ROLL" in apm and "RCMAP_ROLL" not in px4
    assert "RC_MAP_ROLL" in px4 and "RC_MAP_ROLL" not in apm


@pytest.mark.parametrize(("apm", "_px4"), MODULES)
def test_no_parameter_is_requested_twice(apm, _px4) -> None:
    """The list is one batched read; a duplicate is a second request at the
    aircraft for a value already in hand."""
    names = apm.param_names()
    assert len(names) == len(set(names))


@pytest.mark.parametrize(("apm", "_px4"), MODULES)
def test_a_firmware_that_answers_nothing_yields_an_empty_page_not_an_error(
    apm, _px4,
) -> None:
    payload = apm.build({})
    assert isinstance(payload, dict)
    assert payload.get("sections", []) == [] or payload.get("groups", []) == []


# ---------------------------------------------------------------------------
# Safety
# ---------------------------------------------------------------------------

def _all_values(module) -> dict[str, float]:
    return {name: 1.0 for name in module.param_names()}


def test_safety_renders_the_sections_an_operator_needs_before_a_flight() -> None:
    payload = ardupilot_safety.build(_all_values(ardupilot_safety), [], "copter")
    ids = [s["id"] for s in payload["sections"]]
    assert ids == ["limits", "rtl", "failsafe", "arming", "battery",
                   "rangefinder", "flow"]
    assert payload["stack"] == "ardupilot"


def test_the_fence_action_means_something_different_on_each_vehicle() -> None:
    """FENCE_ACTION 4 is "brake or land" on a copter and is not a plane value
    at all. Rendering copter labels at a plane pilot would name an action their
    aircraft cannot take."""
    values = _all_values(ardupilot_safety)
    copter = ardupilot_safety.build(values, [], "copter")
    plane = ardupilot_safety.build(values, [], "plane")

    def action_options(payload):
        limits = next(s for s in payload["sections"] if s["id"] == "limits")
        field = next(f for f in limits["fields"] if f["param"] == "FENCE_ACTION")
        return {int(o["value"]) for o in field["options"]}

    assert 4 in action_options(copter)
    assert 6 in action_options(plane)
    assert 6 not in action_options(copter)


def test_an_unknown_enum_value_is_preserved_rather_than_snapped() -> None:
    """Silently rewriting a real aircraft's failsafe action because this build
    did not recognise the number would be far worse than showing the number."""
    values = _all_values(ardupilot_safety)
    values["FENCE_ACTION"] = 42.0
    payload = ardupilot_safety.build(values, [], "copter")
    limits = next(s for s in payload["sections"] if s["id"] == "limits")
    field = next(f for f in limits["fields"] if f["param"] == "FENCE_ACTION")
    assert {"value": 42, "label": "Unknown (42)"} in field["options"]


def test_the_bitmask_fields_come_out_as_bitmasks() -> None:
    """FENCE_TYPE as a number asks an operator to do binary arithmetic on their
    own geofence."""
    payload = ardupilot_safety.build(_all_values(ardupilot_safety), [], "copter")
    limits = next(s for s in payload["sections"] if s["id"] == "limits")
    field = next(f for f in limits["fields"] if f["param"] == "FENCE_TYPE")
    assert field["kind"] == "bitmask"
    assert {b["bit"] for b in field["bits"]} == {0, 1, 2, 3}


def test_a_sensor_toggle_writes_the_driver_and_the_estimator() -> None:
    """The ArduPilot version of the classic trap: a rangefinder streaming
    perfect distances that EK3_SRC1_POSZ never looks at."""
    payload = ardupilot_safety.build(_all_values(ardupilot_safety), [], "copter")
    section = next(s for s in payload["sections"] if s["id"] == "rangefinder")
    toggle = section["toggle"]
    assert {"param": "EK3_SRC1_POSZ", "value": 2.0} in toggle["enable"]
    # Off goes back to the barometer, not to no height source at all.
    assert {"param": "EK3_SRC1_POSZ", "value": 1.0} in toggle["disable"]
    assert any(d["param"] == "RNGFND1_TYPE" for d in toggle["drivers"])


def test_extra_parameters_are_bounded_and_upper_cased() -> None:
    names = ardupilot_safety.normalise_extra(
        ["fence_alt_max", "BAD NAME", "x" * 40, "BATT_ARM_VOLT", "BATT_ARM_VOLT"])
    assert names == ["FENCE_ALT_MAX", "BATT_ARM_VOLT"]
    assert len(ardupilot_safety.normalise_extra(
        [f"P{n}" for n in range(200)])) == ardupilot_safety.EXTRA_PARAM_LIMIT


# ---------------------------------------------------------------------------
# Tuning
# ---------------------------------------------------------------------------

def test_tuning_gives_a_copter_the_whole_cascade() -> None:
    payload = ardupilot_tuning.build(_all_values(ardupilot_tuning), "copter")
    assert [g["id"] for g in payload["groups"]] == [
        "rate", "attitude", "velocity", "position", "autotune"]


def test_a_plane_gets_its_own_loops_and_no_multicopter_ones() -> None:
    values = {name: 1.0 for name in ardupilot_tuning.param_names()
              if name.startswith(("RLL_", "PTCH_", "YAW_", "LIM_", "NAVL1", "TECS"))}
    payload = ardupilot_tuning.build(values, "plane")
    ids = [g["id"] for g in payload["groups"]]
    assert "rate" in ids
    assert "velocity" not in ids       # PSC_* is multicopter only


def test_the_autotune_group_states_it_is_a_mode_not_a_command() -> None:
    payload = ardupilot_tuning.build(_all_values(ardupilot_tuning), "copter")
    group = next(g for g in payload["groups"] if g["id"] == "autotune")
    # There is no module to switch on and no reboot to wait for, so the "module
    # is off" warning the PX4 page shows must never fire here.
    assert group["enabled"] is True
    assert group["enable_param"] is None
    assert len(group["steps"]) >= 4


def test_a_rover_is_told_it_has_no_autotune_rather_than_offered_one() -> None:
    payload = ardupilot_tuning.build(_all_values(ardupilot_tuning), "rover")
    assert not any(g["id"] == "autotune" for g in payload["groups"])


# ---------------------------------------------------------------------------
# Radio
# ---------------------------------------------------------------------------

def test_the_flight_mode_slots_offer_this_airframes_own_modes() -> None:
    """FLTMODE1 holds a flat mode number, and 4 is GUIDED on a copter and ACRO
    on a plane."""
    values = _all_values(ardupilot_rc)
    copter = ardupilot_rc.build(values, 8, mv.mavlink.MAV_TYPE_QUADROTOR)
    plane = ardupilot_rc.build(values, 8, mv.mavlink.MAV_TYPE_FIXED_WING)

    def slot_label(payload, number):
        modes = next(s for s in payload["sections"] if s["id"] == "modes")
        field = next(f for f in modes["fields"] if f["param"] == "FLTMODE1")
        return next(o["label"] for o in field["options"] if int(o["value"]) == number)

    assert slot_label(copter, 4) == "Guided"
    assert slot_label(plane, 4) == "Acro"


def test_the_channel_cap_follows_the_receiver() -> None:
    payload = ardupilot_rc.build(_all_values(ardupilot_rc), 8)
    channels = next(s for s in payload["sections"] if s["id"] == "channels")
    assert [row["channel"] for row in channels["rows"]] == list(range(1, 9))
    assert payload["channel_limit"] == 8


def test_a_reversed_channel_is_written_to_ardupilots_own_parameter() -> None:
    """RC<n>_REV does not exist on ArduPilot. A write to it is dropped, so the
    wizard reported success and left the channel the wrong way round."""
    writes = ardupilot_rc.calibration_writes([
        {"channel": 2, "min": 1100, "max": 1900, "trim": 1500, "reversed": True},
    ])
    names = [w["name"] for w in writes]
    assert "RC2_REVERSED" in names
    assert "RC2_REV" not in names
    assert next(w["value"] for w in writes if w["name"] == "RC2_REVERSED") == 1.0


def test_the_stick_mapping_is_written_to_rcmap_not_rc_map() -> None:
    writes = ardupilot_rc.calibration_writes(
        [{"channel": 1, "min": 1100, "max": 1900, "trim": 1500}],
        mapping={"RCMAP_ROLL": 1},
    )
    assert {"name": "RCMAP_ROLL", "value": 1.0} in writes
    # Endpoints first, mapping last: a channel pointed at a stick before its
    # travel is known is briefly a stick with the wrong endpoints.
    assert writes[-1]["name"] == "RCMAP_ROLL"


def test_a_px4_mapping_name_is_refused_rather_than_written() -> None:
    with pytest.raises(rc_config.CalibrationError):
        ardupilot_rc.calibration_writes(
            [{"channel": 1, "min": 1100, "max": 1900, "trim": 1500}],
            mapping={"RC_MAP_ROLL": 1},
        )


def test_an_implausible_measurement_is_still_refused_whole() -> None:
    """The validator is shared with the PX4 module, because what makes a
    measurement implausible is a fact about receivers."""
    with pytest.raises(rc_config.CalibrationError, match="moved only"):
        ardupilot_rc.calibration_writes([
            {"channel": 1, "min": 1490, "max": 1510, "trim": 1500},
        ])


# ---------------------------------------------------------------------------
# Motors
# ---------------------------------------------------------------------------

def test_the_motor_count_follows_the_frame_class() -> None:
    """ArduPilot has no motor-count parameter: the count is a property of the
    compiled-in layout."""
    assert ardupilot_motors.rotor_count({"FRAME_CLASS": 1.0}) == 4
    assert ardupilot_motors.rotor_count({"FRAME_CLASS": 2.0}) == 6
    assert ardupilot_motors.rotor_count({"FRAME_CLASS": 3.0}) == 8
    assert ardupilot_motors.rotor_count({}) == 0


def test_motors_carry_no_invented_geometry() -> None:
    """ArduPilot publishes none, and a drawn-from-memory layout that is subtly
    wrong is worse than the page's own dashed ring."""
    values = dict.fromkeys(ardupilot_motors.param_names(), 0.0)
    values["FRAME_CLASS"] = 1.0
    payload = ardupilot_motors.build(values)
    assert len(payload["motors"]) == 4
    for motor in payload["motors"]:
        assert (motor["x"], motor["y"], motor["z"]) == (0.0, 0.0, 0.0)
        assert motor["spin"] is None
    assert payload["fixed_motor_count"] is True


def test_an_output_function_names_the_motor_it_drives() -> None:
    values = dict.fromkeys(ardupilot_motors.param_names(), 0.0)
    values["FRAME_CLASS"] = 1.0
    values["SERVO3_FUNCTION"] = 35.0       # Motor 3
    values["SERVO7_FUNCTION"] = 19.0       # Elevator
    payload = ardupilot_motors.build(values)
    by_pin = {e["pin"]: e for e in payload["outputs"]}
    assert by_pin[3]["motor"] == 3
    assert by_pin[7]["motor"] is None
    assert by_pin[7]["function"] == "Elevator"


def test_the_motor_function_values_are_not_contiguous() -> None:
    """Motors 1-8 run from 33; 9-12 were added later at 82. Base-plus-offset
    arithmetic would claim four functions that belong to something else."""
    assert ardupilot_motors.motor_function_value(1) == 33.0
    assert ardupilot_motors.motor_function_value(8) == 40.0
    assert ardupilot_motors.motor_function_value(9) == 82.0
    assert ardupilot_motors.motor_function_value(13) is None
    assert motor_config.motor_function_value(3) == 103.0


def test_an_output_is_only_addressed_through_the_bank_that_exists() -> None:
    assert ardupilot_motors.function_param("SERVO", 4) == "SERVO4_FUNCTION"
    assert ardupilot_motors.function_param("MAIN", 4) is None
    assert ardupilot_motors.function_param("SERVO", 0) is None
    assert ardupilot_motors.function_param("SERVO", 999) is None
