"""The flight-stack dialect layer: what PX4 and ArduPilot each do differently.

Corvus was written against PX4 and only PX4. Every one of the differences
tested here used to be a silent wrong answer rather than an error, which is why
they are worth pinning individually:

* a mode decoded with the wrong stack's layout is named ``MODE_5``, or worse,
  named something plausible that belongs to a different aircraft;
* a takeoff altitude in the wrong frame turns "10 m up" into "10 m above sea
  level", which on most land is an instruction to stay put;
* a calibration sent in PX4's parameter slots is ACCEPTED by ArduPilot and
  starts nothing.

None of those raise. They are all tested here.
"""
from __future__ import annotations

import math

import pytest
from pymavlink import mavutil as mv

from corvus import autopilot

CUSTOM = mv.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED


# ---------------------------------------------------------------------------
# Which stack
# ---------------------------------------------------------------------------

@pytest.mark.parametrize(("value", "stack"), [
    (mv.mavlink.MAV_AUTOPILOT_PX4, autopilot.STACK_PX4),
    (mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA, autopilot.STACK_ARDUPILOT),
    (mv.mavlink.MAV_AUTOPILOT_GENERIC, autopilot.STACK_GENERIC),
    ("PX4", autopilot.STACK_PX4),
    ("ARDUPILOTMEGA", autopilot.STACK_ARDUPILOT),
    ("Slugs", autopilot.STACK_GENERIC),
    (None, autopilot.STACK_GENERIC),
    (True, autopilot.STACK_GENERIC),
])
def test_the_autopilot_id_picks_the_dialect(value: object, stack: str) -> None:
    assert autopilot.stack_for_autopilot(value) == stack


def test_an_unknown_stack_is_never_assumed_to_be_px4() -> None:
    """The dangerous default. PX4's packed mode words mean something else
    entirely to firmware that does not use them."""
    dialect = autopilot.dialect_for(99)
    assert dialect.stack == autopilot.STACK_GENERIC
    assert dialect.available_modes(mv.mavlink.MAV_TYPE_QUADROTOR) == []
    assert dialect.mode_table(mv.mavlink.MAV_TYPE_QUADROTOR) == {}


# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------

def test_px4_packs_a_main_mode_and_a_sub_mode() -> None:
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    assert px4.decode_mode((4 << 16) | (4 << 24), 29, 2) == "MISSION"
    assert px4.decode_mode(3 << 16, 81, 2) == "POSCTL"


def _px4_word(main: int, sub: int = 0) -> int:
    return (main << 16) | (sub << 24)


@pytest.mark.parametrize(("main", "sub", "name"), [
    # px4_custom_mode.h, PX4_CUSTOM_SUB_MODE_POSCTL, v1.16 to v1.18. Reading
    # the main mode alone showed both of these as POSITION.
    (3, 0, "POSCTL"),
    (3, 1, "ORBIT"),
    (3, 2, "POSITION_SLOW"),
    (3, 9, "POSCTL_SUBMODE_9"),
    # PX4_CUSTOM_MAIN_MODE_TERMINATION (v1.16+) and ALTITUDE_CRUISE (v1.17+)
    # used to decode as MODE_655360 and MODE_720896.
    (10, 0, "TERMINATION"),
    (11, 0, "ALTITUDE_CRUISE"),
    # PX4_CUSTOM_SUB_MODE_GUIDED_COURSE, new in v1.18.
    (4, 19, "GUIDED_COURSE"),
])
def test_every_mode_px4_reports_is_decoded_by_name(main: int, sub: int, name: str) -> None:
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    assert px4.decode_mode(_px4_word(main, sub), 81, 2) == name


def test_retired_px4_modes_still_decode_for_old_logs() -> None:
    """Main mode 8 and AUTO sub mode 7 are reserved in every supported
    release, but a log from before 2020 can carry them."""
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    assert px4.decode_mode(_px4_word(8), 65, 2) == "RATTITUDE"
    assert px4.decode_mode(_px4_word(4, 7), 29, 2) == "RTGS"


@pytest.mark.parametrize(("name", "label"), [
    ("POSCTL", "POSITION"),
    ("ALTCTL", "ALTITUDE"),
    ("LOITER", "HOLD"),
    ("RTL", "RETURN"),
    ("FOLLOWME", "FOLLOW ME"),
    ("PRECLAND", "PRECISION LAND"),
    ("VTOL_TAKEOFF", "VTOL TAKEOFF"),
    ("MISSION", "MISSION"),
    ("ORBIT", "ORBIT"),
    ("POSITION_SLOW", "POSITION SLOW"),
    ("ALTITUDE_CRUISE", "ALTITUDE CRUISE"),
    ("TERMINATION", "TERMINATION"),
    ("GUIDED_COURSE", "GUIDED COURSE"),
    ("", ""),
])
def test_px4_modes_are_shown_by_the_word_px4_uses(name: str, label: str) -> None:
    """POSCTL is what a mode change sends; POSITION is what the operator reads."""
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    assert px4.mode_label(name) == label


@pytest.mark.parametrize(("name", "label"), [
    ("LOITER", "LOITER"),
    ("RTL", "RTL"),
    ("ALT_HOLD", "ALT HOLD"),
    ("SMART_RTL", "SMART RTL"),
    ("FBWA", "FBWA"),
])
def test_ardupilot_keeps_its_own_mode_names(name: str, label: str) -> None:
    """LOITER and RTL are ArduPilot's own modes under ArduPilot's own names.
    PX4's words for them would name a different mode; only the underscores go."""
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    assert apm.mode_label(name) == label


def test_an_unknown_stack_shows_the_mode_it_decoded() -> None:
    generic = autopilot.dialect_for(99)
    assert generic.mode_label(generic.decode_mode(7, 0, 2)) == "MODE 7"


def test_ardupilot_reads_one_flat_number_against_its_vehicle_table() -> None:
    """The same number is a different mode on a different airframe, which is
    why MAV_TYPE has to reach the decoder at all."""
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    assert apm.decode_mode(4, CUSTOM, mv.mavlink.MAV_TYPE_QUADROTOR) == "GUIDED"
    assert apm.decode_mode(4, CUSTOM, mv.mavlink.MAV_TYPE_FIXED_WING) == "ACRO"
    assert apm.decode_mode(4, CUSTOM, mv.mavlink.MAV_TYPE_GROUND_ROVER) == "HOLD"
    assert apm.decode_mode(9, CUSTOM, mv.mavlink.MAV_TYPE_SUBMARINE) == "SURFACE"


def test_ardupilot_without_the_custom_mode_flag_is_not_read_as_a_mode() -> None:
    """A heartbeat that is not announcing a custom mode says nothing about
    which one it is in, and naming it STABILIZE would be an invention."""
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    assert apm.decode_mode(0, 0, mv.mavlink.MAV_TYPE_QUADROTOR) == ""


def test_an_ardupilot_mode_is_commanded_as_param1_flag_param2_number() -> None:
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    table = apm.mode_table(mv.mavlink.MAV_TYPE_QUADROTOR)
    assert table["RTL"] == (CUSTOM, 6, 0)
    assert table["RTL"].params()[:3] == [float(CUSTOM), 6.0, 0.0]


def test_retired_ardupilot_modes_decode_but_are_never_offered() -> None:
    """An old airframe can be sitting in OF_LOITER; nobody should be given a
    button that selects it."""
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    assert apm.decode_mode(10, CUSTOM, mv.mavlink.MAV_TYPE_QUADROTOR) == "OF_LOITER"
    assert "OF_LOITER" not in apm.available_modes(mv.mavlink.MAV_TYPE_QUADROTOR)


def test_a_px4_mode_command_is_the_pymavlink_triple() -> None:
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    table = px4.mode_table(mv.mavlink.MAV_TYPE_QUADROTOR, (1, 18, 0))
    for name, value in mv.px4_map.items():
        if name in autopilot.PX4_RETIRED_MODES:
            continue
        assert table[name] == value, name


def test_px4_modes_newer_than_px4_map_are_the_commanders_own_triples() -> None:
    """Commander.cpp: POSCTL with sub mode 2 is Position Slow, and main mode 11
    is Altitude Cruise. The base_mode only has to carry the custom mode flag."""
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    table = px4.mode_table(mv.mavlink.MAV_TYPE_QUADROTOR, (1, 18, 0))
    assert table["POSITION_SLOW"] == (81, 3, 2)
    assert table["ALTITUDE_CRUISE"] == (81, 11, 0)
    for command in table.values():
        assert command.base_mode & CUSTOM


@pytest.mark.parametrize("firmware", [None, (1, 12, 3), (1, 16, 0), (1, 17, 0), (1, 18, 0)])
def test_retired_px4_modes_are_never_offered_or_sent(firmware: tuple[int, ...] | None) -> None:
    """RTGS is AUTO_RESERVED_DO_NOT_USE and refused. RATTITUDE is
    RATTITUDE_LEGACY: v1.16 and v1.17 ACK it and change nothing, which is
    worse than a refusal because the operator is told it worked."""
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    for name in ("RTGS", "RATTITUDE"):
        assert name not in px4.available_modes(2, firmware)
        assert name not in px4.mode_table(2, firmware)
        assert name not in px4.adopt_live_mapping(dict(mv.px4_map), 2, firmware)


@pytest.mark.parametrize(("firmware", "slow", "cruise"), [
    # Unknown: only what every supported release switches to.
    (None, False, False),
    # Best effort range. v1.14 lands a sub mode 2 request in plain POSCTL.
    ((1, 14, 3), False, False),
    ((1, 15, 4), True, False),
    # Target range. v1.16 ACKs main mode 11 and does nothing.
    ((1, 16, 2), True, False),
    ((1, 17, 0), True, True),
    ((1, 18, 0), True, True),
])
def test_px4_modes_are_offered_only_by_the_releases_that_fly_them(
    firmware: tuple[int, ...] | None, slow: bool, cruise: bool,
) -> None:
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    offered = px4.available_modes(2, firmware)
    table = px4.mode_table(2, firmware)
    live = px4.adopt_live_mapping(dict(mv.px4_map), 2, firmware)
    for modes in (offered, table, live):
        assert ("POSITION_SLOW" in modes) is slow
        assert ("ALTITUDE_CRUISE" in modes) is cruise
        assert {"MANUAL", "POSCTL", "LOITER", "MISSION", "RTL"} <= set(modes)


def test_px4_modes_that_cannot_be_selected_are_never_offered() -> None:
    """Orbit is started by DO_ORBIT (a POSCTL sub mode 1 request lands in
    POSCTL), and Guided Course is steered by heading commands the station does
    not send. Both decode; neither is a selector entry."""
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    for name in ("ORBIT", "GUIDED_COURSE"):
        assert name not in px4.available_modes(2, (1, 18, 0))
        assert name not in px4.mode_table(2, (1, 18, 0))


def test_the_px4_selector_order_is_manual_first() -> None:
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    offered = px4.available_modes(2, (1, 17, 0))
    assert offered[:5] == ["MANUAL", "ALTCTL", "ALTITUDE_CRUISE", "POSCTL", "POSITION_SLOW"]
    assert set(offered) == set(px4.mode_table(2, (1, 17, 0)))


def test_the_live_mapping_is_adopted_only_in_the_shape_its_stack_sends() -> None:
    """pymavlink answers mode_mapping() in two shapes. Adopting the wrong one
    is how a flat ArduPilot number ended up in param2 of a PX4 command."""
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    flat = mv.mode_mapping_byname(mv.mavlink.MAV_TYPE_QUADROTOR)

    assert px4.adopt_live_mapping(flat, mv.mavlink.MAV_TYPE_QUADROTOR) == {}
    assert px4.adopt_live_mapping(dict(mv.px4_map), 2)["MISSION"] == mv.px4_map["MISSION"]

    adopted = apm.adopt_live_mapping(flat, mv.mavlink.MAV_TYPE_QUADROTOR)
    assert adopted["AUTO"] == (CUSTOM, 3, 0)
    assert apm.adopt_live_mapping(dict(mv.px4_map), 2) == {}


# ---------------------------------------------------------------------------
# Takeoff
# ---------------------------------------------------------------------------

def test_the_two_stacks_want_the_takeoff_staged_in_opposite_orders() -> None:
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4).takeoff_plan(2)
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA).takeoff_plan(2)

    assert px4.order == "takeoff_then_arm"
    assert px4.altitude_frame == "amsl"
    assert px4.guided_mode == ""
    assert math.isnan(px4.min_pitch_deg)

    assert apm.order == "mode_arm_takeoff"
    assert apm.altitude_frame == "relative"
    assert apm.guided_mode == "GUIDED"
    # NaN here is refused outright by ArduPilot Plane, which reads param1 as
    # the minimum climb pitch.
    assert apm.min_pitch_deg == 0.0


def test_only_ardupilot_needs_a_mode_before_a_guided_command() -> None:
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    assert px4.guided_mode(2) == ""
    assert apm.guided_mode(2) == "GUIDED"
    # An antenna tracker does not fly, so there is nothing to put in GUIDED.
    assert apm.guided_mode(mv.mavlink.MAV_TYPE_ANTENNA_TRACKER) == ""


def test_each_stack_names_the_mode_the_flight_keys_switch_to() -> None:
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    generic = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_GENERIC)
    assert px4.stick_mode(2) == "POSCTL"
    assert "POSCTL" in px4.stick_modes(2) and "MISSION" not in px4.stick_modes(2)
    assert apm.stick_mode(mv.mavlink.MAV_TYPE_QUADROTOR) == "POSHOLD"
    assert "LOITER" in apm.stick_modes(mv.mavlink.MAV_TYPE_QUADROTOR)
    assert "AUTO" not in apm.stick_modes(mv.mavlink.MAV_TYPE_QUADROTOR)
    assert apm.stick_mode(mv.mavlink.MAV_TYPE_FIXED_WING) == ""
    assert generic.stick_mode(2) == ""
    assert px4.capabilities(2)["stick_mode"] == "POSCTL"


def test_only_ardupilot_reserves_mission_slot_zero_for_home() -> None:
    assert autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4).mission_seq0_is_home is False
    assert autopilot.dialect_for(
        mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA).mission_seq0_is_home is True


def test_the_mission_mode_is_named_differently_on_each_stack() -> None:
    assert autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4).mission_mode == "MISSION"
    assert autopilot.dialect_for(
        mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA).mission_mode == "AUTO"


# ---------------------------------------------------------------------------
# Calibration
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("sensor", ["gyro", "baro", "accel", "level", "accel_quick"])
def test_the_stacks_agree_on_the_calibrations_they_agree_on(sensor: str) -> None:
    """Gyro, baro and the three accelerometer forms share PX4's parameter
    slots. Pinning that is what makes the divergences below meaningful."""
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4).calibration(sensor)
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA).calibration(sensor)
    assert px4.command == apm.command == mv.mavlink.MAV_CMD_PREFLIGHT_CALIBRATION
    for a, b in zip(px4.params, apm.params, strict=True):
        assert (math.isnan(a) and math.isnan(b)) or a == b


def test_ardupilot_calibrates_the_compass_with_its_own_command() -> None:
    """PX4's param2 means nothing to ArduPilot: it runs an onboard spherical
    fit started by DO_START_MAG_CAL. Sending PX4's payload is ACCEPTED and
    starts nothing."""
    plan = autopilot.dialect_for(
        mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA).calibration("compass")
    assert plan.command == autopilot.MAV_CMD_DO_START_MAG_CAL
    assert plan.params[:5] == (0.0, 0.0, 1.0, 0.0, 0.0)


def test_ardupilot_cancels_a_mag_fit_with_a_different_command() -> None:
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    assert apm.cancel_calibration("compass").command == autopilot.MAV_CMD_DO_CANCEL_MAG_CAL
    assert apm.cancel_calibration("accel").command == mv.mavlink.MAV_CMD_PREFLIGHT_CALIBRATION
    assert apm.cancel_calibration("").command == mv.mavlink.MAV_CMD_PREFLIGHT_CALIBRATION


def test_ardupilot_has_no_mavlink_esc_calibration_and_says_so() -> None:
    plan = autopilot.dialect_for(
        mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA).calibration("motor")
    assert not plan.ok
    assert "ESC_CALIBRATION" in plan.unsupported


def test_only_ardupilot_waits_to_be_told_each_accelerometer_position() -> None:
    """The step with no PX4 equivalent, and the one that made an ArduPilot
    accelerometer calibration hang on step one under a PX4-only wizard."""
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    plan = apm.accel_position("noseup")
    assert plan.ok
    assert plan.command == autopilot.MAV_CMD_ACCELCAL_VEHICLE_POS
    assert plan.params[0] == 5.0

    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4).accel_position("level")
    assert not px4.ok
    assert "detects" in px4.unsupported


def test_an_unknown_calibration_position_is_refused_by_name() -> None:
    plan = autopilot.dialect_for(
        mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA).accel_position("sideways")
    assert not plan.ok
    assert "sideways" in plan.unsupported


# ---------------------------------------------------------------------------
# Autotune and capabilities
# ---------------------------------------------------------------------------

def test_ardupilots_autotune_is_a_mode_and_px4s_is_a_command() -> None:
    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4)
    apm = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    assert px4.autotune_style == "command"
    assert px4.autotune_plan(2, True).command == mv.mavlink.MAV_CMD_DO_AUTOTUNE_ENABLE
    assert apm.autotune_style == "mode"
    assert apm.autotune_mode(mv.mavlink.MAV_TYPE_QUADROTOR) == "AUTOTUNE"
    # Rover has no autotune mode, so the page must say so rather than offer one.
    assert apm.autotune_mode(mv.mavlink.MAV_TYPE_GROUND_ROVER) == ""


def test_capabilities_report_what_the_ui_has_to_hide() -> None:
    caps = autopilot.dialect_for(
        mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA).capabilities(mv.mavlink.MAV_TYPE_QUADROTOR)
    assert caps["stack"] == "ardupilot"
    assert caps["shell"] is False
    assert caps["esc_calibration"] is False
    assert caps["accel_cal_prompted"] is True
    assert caps["log_suffix"] == ".bin"
    assert caps["takeoff_frame"] == "relative"
    assert "motor" not in caps["calibrations"]
    assert "compass" in caps["calibrations"]

    px4 = autopilot.dialect_for(mv.mavlink.MAV_AUTOPILOT_PX4).capabilities(2)
    assert px4["shell"] is True
    assert px4["log_suffix"] == ".ulg"
    assert "motor" in px4["calibrations"]


@pytest.mark.parametrize(("mav_type", "family"), [
    (mv.mavlink.MAV_TYPE_QUADROTOR, "copter"),
    (mv.mavlink.MAV_TYPE_HEXAROTOR, "copter"),
    (mv.mavlink.MAV_TYPE_FIXED_WING, "plane"),
    (mv.mavlink.MAV_TYPE_VTOL_TILTROTOR, "plane"),
    (mv.mavlink.MAV_TYPE_GROUND_ROVER, "rover"),
    (mv.mavlink.MAV_TYPE_SURFACE_BOAT, "rover"),
    (mv.mavlink.MAV_TYPE_SUBMARINE, "sub"),
    (mv.mavlink.MAV_TYPE_ANTENNA_TRACKER, "other"),
])
def test_the_vehicle_class_is_the_axis_ardupilots_parameters_vary_along(
    mav_type: int, family: str,
) -> None:
    assert autopilot.vehicle_class(mav_type) == family


@pytest.mark.parametrize("stack, text, reason", [
    (autopilot.STACK_PX4, "Preflight Fail: Accel 0 uncalibrated", "Accel 0 uncalibrated"),
    (autopilot.STACK_PX4, "PreArm: Compass not calibrated", None),
    (autopilot.STACK_PX4, "Preflight Fail: ", None),
    (autopilot.STACK_ARDUPILOT, "PreArm: Compass not calibrated", "Compass not calibrated"),
    (autopilot.STACK_ARDUPILOT, "Arm: Throttle too high", "Throttle too high"),
    (autopilot.STACK_ARDUPILOT, "Preflight Fail: Accel 0 uncalibrated", None),
    (autopilot.STACK_GENERIC, "PreArm: Compass not calibrated", "Compass not calibrated"),
    (autopilot.STACK_GENERIC, "Takeoff detected", None),
])
def test_each_stack_reads_its_own_preflight_wording(stack: str, text: str,
                                                     reason: str | None) -> None:
    assert autopilot.dialect_for_stack(stack).prearm_failure(text) == reason


def test_px4_refuses_a_circle_in_a_mission_and_ardupilot_flies_one() -> None:
    """PX4 v1.16 to v1.18 answer NAV_LOITER_TURNS with MAV_MISSION_UNSUPPORTED
    (mavlink_mission.cpp); ArduCopter, Plane and Rover fly it as a circle."""
    px4 = autopilot.dialect_for_stack(autopilot.STACK_PX4)
    assert mv.mavlink.MAV_CMD_NAV_LOITER_TURNS in px4.mission_command_refusals
    assert autopilot.dialect_for_stack(autopilot.STACK_ARDUPILOT).mission_command_refusals == {}
    assert autopilot.dialect_for_stack(autopilot.STACK_GENERIC).mission_command_refusals == {}


def test_only_px4_is_waited_on_for_a_verdict_after_an_upload() -> None:
    """PX4 checks a stored mission and reports NO_MISSION for one it will not
    fly; ArduPilot refuses at upload, so there is nothing to wait for."""
    assert autopilot.dialect_for_stack(autopilot.STACK_PX4).mission_validity_reported is True
    assert autopilot.dialect_for_stack(autopilot.STACK_ARDUPILOT).mission_validity_reported is False
    assert autopilot.dialect_for_stack(autopilot.STACK_GENERIC).mission_validity_reported is False
