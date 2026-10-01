"""Flight-stack dialects — the places where PX4 and ArduPilot disagree.

Corvus was written against PX4 and, for a long time, against nothing else: the
mode decoder read PX4's packed ``custom_mode``, the takeoff sent an AMSL
altitude and armed *after* the vehicle accepted it, the calibration table was
PX4's ``PREFLIGHT_CALIBRATION`` parameter layout, and the mode selector went
empty the moment an autopilot answered with a flat mode number. Every one of
those is wrong on an ArduPilot vehicle, and every one of them fails quietly —
a mode shown as ``MODE_5``, a 10 m takeoff read as 10 m above *sea level*, a
compass calibration that is acknowledged and never starts.

This module is the single place that knows which stack is on the other end and
what that changes. The bridge, the HTTP layer and the frontend ask it instead
of branching on the autopilot id themselves, so there is one table to correct
when a stack changes its mind rather than a dozen call sites to hunt down.

Scope is the *protocol*: modes, commands, calibration, takeoff order, and the
capability flags that let the UI hide what a stack cannot do. Parameter
**names** differ far more than commands do, and those live with the page that
renders them — see :mod:`corvus.ardupilot_config`.

Verified against PX4 v1.16/v1.17/v1.18 and ArduPilot 4.3-4.6
(Copter/Plane/Rover/Sub).
"""
from __future__ import annotations

import math
import re
import struct
from dataclasses import dataclass
from typing import Any, NamedTuple

from pymavlink import mavutil

# ---------------------------------------------------------------------------
# Stacks
# ---------------------------------------------------------------------------

STACK_PX4 = "px4"
STACK_ARDUPILOT = "ardupilot"
STACK_GENERIC = "generic"

# MAV_AUTOPILOT ids, spelled out for the same reason MAV_AUTOPILOT_MAP is:
# this number is what decides which dialect a flight is flown with, and it must
# not depend on an enum lookup that a dialect XML update could move.
MAV_AUTOPILOT_ARDUPILOTMEGA = 3
MAV_AUTOPILOT_PX4 = 12

# Stacks that identify themselves by name rather than by number — the string
# already latched in the state store, and whatever a log or a test hands us.
_NAME_TO_STACK: dict[str, str] = {
    "px4": STACK_PX4,
    "ardupilotmega": STACK_ARDUPILOT,
    "ardupilot": STACK_ARDUPILOT,
    "apm": STACK_ARDUPILOT,
    "arducopter": STACK_ARDUPILOT,
    "arduplane": STACK_ARDUPILOT,
}


def stack_for_autopilot(value: Any) -> str:
    """Map a MAV_AUTOPILOT id (or its name) to the dialect that speaks it.

    Anything unrecognised is ``STACK_GENERIC`` rather than PX4. Guessing PX4
    for an unknown stack is how a ground station ends up sending PX4's packed
    mode words to firmware that reads them as something else entirely; the
    generic dialect sends only what ``common.xml`` guarantees.
    """
    if isinstance(value, bool):
        return STACK_GENERIC
    if isinstance(value, int):
        if value == MAV_AUTOPILOT_PX4:
            return STACK_PX4
        if value == MAV_AUTOPILOT_ARDUPILOTMEGA:
            return STACK_ARDUPILOT
        return STACK_GENERIC
    if isinstance(value, str):
        return _NAME_TO_STACK.get(value.strip().lower(), STACK_GENERIC)
    return STACK_GENERIC


# ---------------------------------------------------------------------------
# PX4 modes
# ---------------------------------------------------------------------------
#
# Checked against src/modules/commander/px4_custom_mode.h and the DO_SET_MODE
# handler in Commander.cpp at v1.16.2, v1.17.0 and v1.18.0-rc1, and at v1.12.3
# to v1.15.4 for the best-effort range. PX4 only ever appends to these enums, so
# a number keeps its meaning; what moves between releases is which numbers
# exist and which of them a mode change is allowed to ask for.

# PX4 main_mode values (bits 16-23 of custom_mode). 8 is RATTITUDE_LEGACY in
# every supported release and no vehicle state reports it any more; the name is
# kept for logs from firmware old enough to have flown it. 9 (SIMPLE) is
# reserved and never sent. ALTITUDE_CRUISE is new in v1.17.
PX4_MAIN_MODE: dict[int, str] = {
    1: "MANUAL", 2: "ALTCTL", 3: "POSCTL", 4: "AUTO",
    5: "ACRO", 6: "OFFBOARD", 7: "STABILIZED", 8: "RATTITUDE",
    10: "TERMINATION", 11: "ALTITUDE_CRUISE",
}

# PX4 POSCTL sub_mode values (bits 24-31 when main_mode == 3). Orbit and
# Position Slow are reported as POSCTL with a sub mode, so reading the main mode
# alone showed an orbiting vehicle as POSITION. Position Slow is new in v1.15.
PX4_POSCTL_SUBMODE: dict[int, str] = {
    0: "POSCTL", 1: "ORBIT", 2: "POSITION_SLOW",
}

# PX4 AUTO sub_mode values (bits 24-31 when main_mode == 4). Prefix-less so a
# decoded mode name matches the names in PX4_MODE_VALUES and the selector.
# 7 is AUTO_RESERVED_DO_NOT_USE: RTGS was deleted in March 2020 and is kept here
# for older logs only. GUIDED_COURSE is new in v1.18.
PX4_AUTO_SUBMODE: dict[int, str] = {
    1: "READY", 2: "TAKEOFF", 3: "LOITER",
    4: "MISSION", 5: "RTL", 6: "LAND",
    7: "RTGS", 8: "FOLLOWME",
    9: "PRECLAND", 10: "VTOL_TAKEOFF",
    11: "EXTERNAL1", 12: "EXTERNAL2", 13: "EXTERNAL3",
    14: "EXTERNAL4", 15: "EXTERNAL5", 16: "EXTERNAL6",
    17: "EXTERNAL7", 18: "EXTERNAL8", 19: "GUIDED_COURSE",
}

# (base_mode, main_mode, sub_mode) triples keyed by mode name, as pymavlink's
# px4_map spells them. 81/65/29 are the base_mode bytes PX4 itself sends for
# each family; DO_SET_MODE wants them back verbatim. Only modes PX4's
# DO_SET_MODE handler actually switches to are here: Orbit is entered with
# DO_ORBIT (a POSCTL sub mode 1 request lands in plain POSCTL), and Guided
# Course needs heading commands the station does not send.
PX4_MODE_VALUES: dict[str, tuple[int, int, int]] = {
    "MANUAL": (81, 1, 0), "ALTCTL": (81, 2, 0), "POSCTL": (81, 3, 0),
    "POSITION_SLOW": (81, 3, 2), "ALTITUDE_CRUISE": (81, 11, 0),
    "STABILIZED": (81, 7, 0), "ACRO": (65, 5, 0),
    "LOITER": (29, 4, 3), "MISSION": (29, 4, 4), "RTL": (29, 4, 5),
    "LAND": (29, 4, 6), "TAKEOFF": (29, 4, 2), "OFFBOARD": (29, 6, 0),
    "FOLLOWME": (29, 4, 8),
}

PX4_AVAILABLE_MODES: list[str] = [
    "MANUAL", "ALTCTL", "ALTITUDE_CRUISE", "POSCTL", "POSITION_SLOW",
    "STABILIZED", "ACRO",
    "LOITER", "MISSION", "RTL", "LAND", "TAKEOFF",
    "OFFBOARD", "FOLLOWME",
]

# Names pymavlink's px4_map still offers that no supported PX4 will fly. RTGS
# (AUTO sub mode 7) is refused with "Unsupported auto mode". RATTITUDE (main
# mode 8) has no branch in the handler: v1.16 and v1.17 answer ACCEPTED and
# change nothing, v1.18 refuses it as "Unsupported main mode". Both still
# decode, so an old log reads right, but neither is ever offered or sent.
PX4_RETIRED_MODES: frozenset[str] = frozenset({"RTGS", "RATTITUDE"})

# The first (major, minor) whose DO_SET_MODE handler switches to the mode.
# Older firmware answers ACCEPTED either way: a Position Slow request before
# v1.15 flies on in plain POSCTL, and Altitude Cruise before v1.17 changes
# nothing at all. An acknowledged mode change that did not happen is the one
# answer the operator cannot see through, so these wait for AUTOPILOT_VERSION.
PX4_MODE_SINCE: dict[str, tuple[int, int]] = {
    "POSITION_SLOW": (1, 15),
    "ALTITUDE_CRUISE": (1, 17),
}

# The word the operator reads for a PX4 mode, as PX4's own documentation names
# it. Display only: a mode change still sends the name on the left.
PX4_MODE_LABELS: dict[str, str] = {
    "ALTCTL": "ALTITUDE", "POSCTL": "POSITION", "LOITER": "HOLD",
    "RTL": "RETURN", "FOLLOWME": "FOLLOW ME", "PRECLAND": "PRECISION LAND",
    "POSITION_SLOW": "POSITION SLOW", "ALTITUDE_CRUISE": "ALTITUDE CRUISE",
    "ORBIT": "ORBIT", "TERMINATION": "TERMINATION",
    "GUIDED_COURSE": "GUIDED COURSE",
}


def px4_mode_supported(name: str, firmware: tuple[int, ...] | None) -> bool:
    """Will a PX4 of this version switch to *name* on DO_SET_MODE?

    ``firmware`` is ``(major, minor, patch)`` from AUTOPILOT_VERSION, or None
    before it has arrived. An unknown version gets only the modes every
    supported release accepts: a mode offered a second late is harmless, one
    offered to firmware that ignores it is not.
    """
    if name in PX4_RETIRED_MODES:
        return False
    since = PX4_MODE_SINCE.get(name)
    if since is None:
        return True
    return firmware is not None and tuple(firmware[:2]) >= since


def _plain_mode_label(name: str) -> str:
    """A mode name with its underscores read as spaces ("ALT_HOLD" -> "ALT HOLD")."""
    return str(name or "").replace("_", " ")


# ---------------------------------------------------------------------------
# ArduPilot modes
# ---------------------------------------------------------------------------
#
# ArduPilot puts a single flat mode number in custom_mode and keeps a separate
# table per vehicle: mode 4 is GUIDED on a copter and ACRO on a plane, so the
# table cannot be chosen without knowing MAV_TYPE. pymavlink ships these tables
# too, but it lags the firmware by a release or two and has nothing for the
# tailsitter MAV_TYPEs, so the numbers are written out here — they are flight
# modes, and a wrong one is a wrong command, not a cosmetic mislabel.

ARDUPILOT_COPTER_MODES: dict[int, str] = {
    0: "STABILIZE", 1: "ACRO", 2: "ALT_HOLD", 3: "AUTO", 4: "GUIDED",
    5: "LOITER", 6: "RTL", 7: "CIRCLE", 8: "POSITION", 9: "LAND",
    10: "OF_LOITER", 11: "DRIFT", 13: "SPORT", 14: "FLIP", 15: "AUTOTUNE",
    16: "POSHOLD", 17: "BRAKE", 18: "THROW", 19: "AVOID_ADSB",
    20: "GUIDED_NOGPS", 21: "SMART_RTL", 22: "FLOWHOLD", 23: "FOLLOW",
    24: "ZIGZAG", 25: "SYSTEMID", 26: "AUTOROTATE", 27: "AUTO_RTL",
    28: "TURTLE",
}

ARDUPILOT_PLANE_MODES: dict[int, str] = {
    0: "MANUAL", 1: "CIRCLE", 2: "STABILIZE", 3: "TRAINING", 4: "ACRO",
    5: "FBWA", 6: "FBWB", 7: "CRUISE", 8: "AUTOTUNE", 10: "AUTO",
    11: "RTL", 12: "LOITER", 13: "TAKEOFF", 14: "AVOID_ADSB", 15: "GUIDED",
    16: "INITIALISING", 17: "QSTABILIZE", 18: "QHOVER", 19: "QLOITER",
    20: "QLAND", 21: "QRTL", 22: "QAUTOTUNE", 23: "QACRO", 24: "THERMAL",
    25: "LOITER_ALT_QLAND", 26: "AUTOLAND",
}

ARDUPILOT_ROVER_MODES: dict[int, str] = {
    0: "MANUAL", 1: "ACRO", 2: "LEARNING", 3: "STEERING", 4: "HOLD",
    5: "LOITER", 6: "FOLLOW", 7: "SIMPLE", 8: "DOCK", 9: "CIRCLE",
    10: "AUTO", 11: "RTL", 12: "SMART_RTL", 15: "GUIDED",
    16: "INITIALISING",
}

ARDUPILOT_SUB_MODES: dict[int, str] = {
    0: "STABILIZE", 1: "ACRO", 2: "ALT_HOLD", 3: "AUTO", 4: "GUIDED",
    5: "LOITER", 7: "CIRCLE", 9: "SURFACE", 16: "POSHOLD", 19: "MANUAL",
    20: "MOTOR_DETECT",
}

ARDUPILOT_TRACKER_MODES: dict[int, str] = {
    0: "MANUAL", 1: "STOP", 2: "SCAN", 3: "SERVO_TEST", 4: "GUIDED",
    10: "AUTO", 16: "INITIALISING",
}

ARDUPILOT_BLIMP_MODES: dict[int, str] = {
    0: "LAND", 1: "MANUAL", 2: "VELOCITY", 3: "LOITER", 4: "RTL",
}

# MAV_TYPE -> which of the tables above. A type that is not listed falls back
# to pymavlink's map and then, failing that, to the copter table: the
# overwhelming majority of unlisted rotary types ArduPilot adds are copters.
_COPTER_TYPES = (2, 3, 4, 13, 14, 15, 29, 35)
_PLANE_TYPES = (1, 19, 20, 21, 22, 23, 24, 25, 28)
_ROVER_TYPES = (10, 11)

ARDUPILOT_MODE_TABLES: dict[int, dict[int, str]] = {}
for _t in _COPTER_TYPES:
    ARDUPILOT_MODE_TABLES[_t] = ARDUPILOT_COPTER_MODES
for _t in _PLANE_TYPES:
    ARDUPILOT_MODE_TABLES[_t] = ARDUPILOT_PLANE_MODES
for _t in _ROVER_TYPES:
    ARDUPILOT_MODE_TABLES[_t] = ARDUPILOT_ROVER_MODES
ARDUPILOT_MODE_TABLES[12] = ARDUPILOT_SUB_MODES        # SUBMARINE
ARDUPILOT_MODE_TABLES[5] = ARDUPILOT_TRACKER_MODES     # ANTENNA_TRACKER
ARDUPILOT_MODE_TABLES[7] = ARDUPILOT_BLIMP_MODES       # AIRSHIP (Blimp)
del _t

# Modes ArduPilot still decodes but no longer lets a GCS select. Offering them
# puts a control on the page whose only outcome is a refusal; decoding them
# still matters, because an old airframe can be sitting in one.
ARDUPILOT_RETIRED_MODES: frozenset[str] = frozenset({
    "POSITION", "OF_LOITER", "LEARNING", "INITIALISING",
})

# The mode a guided takeoff, a fly-to and an offboard position setpoint all
# need ArduPilot to be in first. Sub has GUIDED; the tracker does not fly.
ARDUPILOT_GUIDED_MODE = "GUIDED"


# Vehicle classes, which is the axis ArduPilot's *parameters* vary along — a
# copter has FS_THR_ENABLE where a plane has FS_SHORT_ACTN, and FENCE_ACTION
# means different things on each. Coarser than MAV_TYPE on purpose: this is the
# grouping the firmware itself is built in (ArduCopter, ArduPlane, Rover, Sub).
VEHICLE_COPTER = "copter"
VEHICLE_PLANE = "plane"
VEHICLE_ROVER = "rover"
VEHICLE_SUB = "sub"
VEHICLE_OTHER = "other"

_VEHICLE_CLASSES: dict[int, str] = {}
for _t in _COPTER_TYPES:
    _VEHICLE_CLASSES[_t] = VEHICLE_COPTER
for _t in _PLANE_TYPES:
    _VEHICLE_CLASSES[_t] = VEHICLE_PLANE
for _t in _ROVER_TYPES:
    _VEHICLE_CLASSES[_t] = VEHICLE_ROVER
_VEHICLE_CLASSES[12] = VEHICLE_SUB
del _t


def vehicle_class(mav_type: Any) -> str:
    """MAV_TYPE -> which firmware family's parameters this vehicle carries."""
    try:
        return _VEHICLE_CLASSES.get(int(mav_type), VEHICLE_OTHER)
    except (TypeError, ValueError):
        return VEHICLE_OTHER


# ---------------------------------------------------------------------------
# Command plans
# ---------------------------------------------------------------------------

class ModeCommand(NamedTuple):
    """One DO_SET_MODE payload, already in the shape the wire wants.

    Both stacks use ``MAV_CMD_DO_SET_MODE``; they disagree about what goes in
    params 2 and 3. PX4 wants its packed ``main_mode`` and ``sub_mode`` bytes,
    ArduPilot wants a flat ``custom_mode`` in param2 and ignores param3. Rather
    than have the caller know that, both produce a seven-float payload here.

    A tuple and not a dataclass on purpose: pymavlink's own PX4 mapping is a
    ``(base_mode, main_mode, sub_mode)`` tuple, and staying comparable to one
    means the live mapping and the built-in table can be checked against each
    other without unwrapping either.
    """

    base_mode: int
    custom_mode: int
    custom_sub: int = 0

    def params(self) -> list[float]:
        nan = float("nan")
        return [
            float(self.base_mode), float(self.custom_mode),
            float(self.custom_sub), nan, nan, nan, nan,
        ]


@dataclass(frozen=True)
class CommandPlan:
    """A command to send, or the reason this stack cannot run it.

    ``unsupported`` is the operator-facing sentence, not a flag: a page that
    can say *why* a button is missing is worth far more than one that hides it.
    """

    command: int = 0
    params: tuple[float, ...] = ()
    unsupported: str = ""

    @property
    def ok(self) -> bool:
        return not self.unsupported

    def param_list(self) -> list[float]:
        return list(self.params)


@dataclass(frozen=True)
class VehicleEvent:
    """A STATUSTEXT that reports a change of the vehicle's arming or motor state.

    ``kind`` is ``"kill"``, ``"unkill"`` or ``"disarm"``. ``message`` is the
    line the operator sees on the board and in the toast; the console keeps
    the autopilot's own words. ``level`` can be higher than the severity the
    autopilot sent: PX4 reports a kill on the ground as INFO, and a line that
    says the motors were cut is not routine chatter on any stack.
    """

    kind: str
    level: str
    message: str


# PX4 v1.16 to v1.18 say "Kill engaged" / "Kill disengaged" (Commander.cpp,
# ACTION_KILL). v1.12 to v1.15 said "Kill-switch engaged".
_PX4_KILL = re.compile(r"^kill(?:[- ]switch)? (engaged|disengaged)$", re.IGNORECASE)
# Commander::disarm(): "Disarmed by %s", arm_disarm_reason_str().
_PX4_DISARMED_BY = re.compile(r"^disarmed by (.+)$", re.IGNORECASE)
_PX4_DISARM_REASONS: dict[str, tuple[str, str]] = {
    "kill-switch": ("warning", "the kill switch"),
    "lockdown": ("warning", "lockdown"),
    "failure detector": ("warning", "the failure detector"),
    "failsafe": ("warning", "a failsafe"),
    "auto preflight disarming": ("info", "auto disarm (no takeoff)"),
    "landing": ("info", "auto disarm after landing"),
    "external command": ("info", "command"),
}
# ArduPilot announces every aux switch it has a name for (RC_Channel.cpp):
# "RC7: MotorEStop HIGH" stops the motors, LOW releases them.
_AP_ESTOP = re.compile(r"^RC\d+: MotorEStop (HIGH|LOW)$")
_PX4_KILLED = VehicleEvent("kill", "critical", "Kill switch engaged. Motors stopped")
_PX4_RELEASED = VehicleEvent("unkill", "info", "Kill switch released")
_AP_STOPPED = VehicleEvent("kill", "critical", "Emergency stop engaged. Motors stopped")
_AP_RELEASED = VehicleEvent("unkill", "info", "Emergency stop released")


def _px4_vehicle_event(text: str) -> VehicleEvent | None:
    kill = _PX4_KILL.match(text)
    if kill:
        return _PX4_KILLED if kill.group(1).lower() == "engaged" else _PX4_RELEASED
    disarmed = _PX4_DISARMED_BY.match(text)
    if disarmed:
        reason = disarmed.group(1).strip()
        level, words = _PX4_DISARM_REASONS.get(reason.lower(), ("info", reason))
        return VehicleEvent("disarm", level, f"Disarmed by {words}")
    return None


def _ardupilot_vehicle_event(text: str) -> VehicleEvent | None:
    estop = _AP_ESTOP.match(text)
    if not estop:
        return None
    return _AP_STOPPED if estop.group(1) == "HIGH" else _AP_RELEASED


# The line above is sent once, when the switch moves. A switch that was already
# engaged when the station connected, or whose line was lost on the radio, is
# only named by the preflight check it fails, which is repeated on request.
# PX4 v1.16 to v1.18: manualControlCheck.cpp, and only while disarmed.
# ArduPilot 4.3 to 4.6: AP_Arming::estop_checks.
_PX4_KILL_PREARM = "kill switch engaged"
_AP_ESTOP_PREARM = "motors emergency stopped"


def _px4_prearm_event(reason: str) -> VehicleEvent | None:
    return _PX4_KILLED if reason.strip().lower() == _PX4_KILL_PREARM else None


def _ardupilot_prearm_event(reason: str) -> VehicleEvent | None:
    return _AP_STOPPED if reason.strip().lower() == _AP_ESTOP_PREARM else None


@dataclass(frozen=True)
class TakeoffPlan:
    """How this stack wants a guided takeoff staged.

    PX4 accepts ``MAV_CMD_NAV_TAKEOFF`` from any mode, reads param7 as an
    **AMSL** altitude, and switches itself into AUTO.TAKEOFF; arming comes
    after, because arming first would leave a vehicle armed on the ground if
    the takeoff were refused.

    ArduPilot reads param7 as an altitude **relative to home**, refuses the
    command outside GUIDED, and refuses it while disarmed. So the order
    inverts: mode, then arm, then takeoff — which is also why an ArduPilot
    takeoff has to be able to disarm again if the last step fails.
    """

    order: str                    # "takeoff_then_arm" | "mode_arm_takeoff"
    altitude_frame: str           # "amsl" | "relative"
    guided_mode: str = ""
    min_pitch_deg: float = float("nan")


_NAN = float("nan")


def _cal(*params: float) -> CommandPlan:
    """A PREFLIGHT_CALIBRATION plan, padded to seven params.

    Unused slots are 0, which is what the message defines as "no calibration"
    and what QGC sends. They used to be NaN, which PX4's Commander casts with
    ``(int)``: undefined in C++, INT_MIN on x86 SITL and 0 on ARM, so the same
    command meant different things on the simulator and on the aircraft.
    """
    values = list(params)
    values.extend([0.0] * (7 - len(values)))
    return CommandPlan(
        command=mavutil.mavlink.MAV_CMD_PREFLIGHT_CALIBRATION,
        params=tuple(values[:7]),
    )


# MAV_CMD ids ArduPilot uses for the magnetometer calibration it runs instead
# of PREFLIGHT_CALIBRATION. Not in every pymavlink build's enum set, so they
# are pinned by number with a lookup that tolerates their absence.
def _cmd(name: str, fallback: int) -> int:
    return int(getattr(mavutil.mavlink, name, fallback))


MAV_CMD_DO_START_MAG_CAL = _cmd("MAV_CMD_DO_START_MAG_CAL", 42424)
MAV_CMD_DO_ACCEPT_MAG_CAL = _cmd("MAV_CMD_DO_ACCEPT_MAG_CAL", 42425)
MAV_CMD_DO_CANCEL_MAG_CAL = _cmd("MAV_CMD_DO_CANCEL_MAG_CAL", 42426)
MAV_CMD_ACCELCAL_VEHICLE_POS = _cmd("MAV_CMD_ACCELCAL_VEHICLE_POS", 42429)
MAV_CMD_ACTUATOR_TEST = _cmd("MAV_CMD_ACTUATOR_TEST", 310)


def _actuator_test(motor: int, value: float, timeout_s: float) -> CommandPlan:
    """PX4's motor test: ``MAV_CMD_ACTUATOR_TEST`` on output function MOTORn.

    param1 is the output (thrust 0 to 1, NaN for off), param2 the timeout the
    vehicle counts down (0 hands the output back), param5 the function, where
    MOTOR1 is 1. PX4 caps the timeout at 3 s on its side.
    """
    return CommandPlan(
        command=MAV_CMD_ACTUATOR_TEST,
        params=(value, timeout_s, 0.0, 0.0, float(motor), 0.0, 0.0),
    )


def _do_motor_test(motor: int, throttle_pct: float, timeout_s: float) -> CommandPlan:
    """``MAV_CMD_DO_MOTOR_TEST``: motor, percent throttle (type 0), timeout,
    this motor only (count 0), default order."""
    return CommandPlan(
        command=mavutil.mavlink.MAV_CMD_DO_MOTOR_TEST,
        params=(float(motor), 0.0, float(throttle_pct), float(timeout_s), 0.0, 0.0, 0.0),
    )

# The six positions ArduPilot's accelerometer calibration asks for, in the
# order it asks for them, with the number it wants echoed back. PX4 detects the
# side it is being shown; ArduPilot does not, and waits for this command
# forever if nobody sends it — which is why an ArduPilot accel calibration
# under a PX4-only ground station simply hangs on step one.
ACCELCAL_POSITIONS: dict[str, int] = {
    "level": 1, "left": 2, "right": 3,
    "nosedown": 4, "noseup": 5, "back": 6,
}


# ---------------------------------------------------------------------------
# Parameter value encoding
# ---------------------------------------------------------------------------
#
# PARAM_VALUE and PARAM_SET carry every value in one float32 field, and the two
# stacks disagree about how an integer parameter gets into it. ArduPilot casts
# (the integer 4 travels as 4.0). PX4 copies the integer's four bytes into the
# float unchanged, so 4 travels as 5.6e-45 and -1 as a NaN. Reading PX4 the
# ArduPilot way shows every integer parameter as 0; writing it the ArduPilot way
# stores 1082130432 in BAT1_N_CELLS while the echo still reads back as "4.0",
# so the write even looks confirmed.

PARAM_ENCODING_BYTEWISE = "bytewise"
PARAM_ENCODING_C_CAST = "c_cast"

# MAV_PARAM_TYPE -> (struct code, low, high) for the integer types. REAL32 and
# the 64-bit types are not here: the first needs no conversion and the others
# do not fit the field at all.
_PARAM_INT_TYPES: dict[int, tuple[str, int, int]] = {
    1: ("<B", 0, 0xFF),
    2: ("<b", -0x80, 0x7F),
    3: ("<H", 0, 0xFFFF),
    4: ("<h", -0x8000, 0x7FFF),
    5: ("<I", 0, 0xFFFFFFFF),
    6: ("<i", -0x80000000, 0x7FFFFFFF),
}


def param_type_is_integer(param_type: Any) -> bool:
    """Is *param_type* one of the MAV_PARAM_TYPE integer types?"""
    try:
        return int(param_type) in _PARAM_INT_TYPES
    except (TypeError, ValueError):
        return False


def decode_param_value(
    value: float, param_type: int, encoding: str, raw: bytes | None = None,
) -> float:
    """The parameter's real value from the float32 field it travelled in.

    *raw* is the field's four bytes off the wire when the caller has them. They
    are preferred over *value* because a float that is a NaN bit pattern does
    not survive the trip through a Python float intact, and on PX4 every
    integer from -8388607 to -4194305 is one of those.
    """
    spec = _PARAM_INT_TYPES.get(int(param_type))
    if spec is None or encoding != PARAM_ENCODING_BYTEWISE:
        return float(value)
    if raw is None or len(raw) < 4:
        raw = struct.pack("<f", float(value))
    code = spec[0]
    return float(struct.unpack_from(code, raw[:struct.calcsize(code)])[0])


def encode_param_value(value: float, param_type: int, encoding: str) -> float:
    """The float32 to put in PARAM_SET so the vehicle stores *value*.

    Raises ValueError for a value the parameter cannot hold (a fraction or an
    out of range number for an integer), and for the few integers whose byte
    pattern is a signalling NaN: pymavlink packs the field through a Python
    float, which quietens that NaN and would change the number on the way out.
    """
    spec = _PARAM_INT_TYPES.get(int(param_type))
    if spec is None:
        return float(value)
    number = float(value)
    if not math.isfinite(number) or number != int(number):
        raise ValueError("this parameter takes whole numbers only")
    code, low, high = spec
    whole = int(number)
    if not low <= whole <= high:
        raise ValueError(f"this parameter takes values from {low} to {high}")
    if encoding != PARAM_ENCODING_BYTEWISE:
        return number
    packed = struct.pack(code, whole).ljust(4, b"\x00")
    bits = struct.unpack("<I", packed)[0]
    if (bits >> 23) & 0xFF == 0xFF and bits & 0x7FFFFF and not bits & 0x400000:
        raise ValueError("this value cannot be sent exactly over MAVLink")
    return struct.unpack("<f", packed)[0]


def param_values_match(param_type: int, got: float, wanted: float) -> bool:
    """Does the vehicle hold *wanted*? Exact for integers, float32 exact otherwise.

    Tighter than the echo check in ``set_param`` on purpose: this answers "was
    it applied", and a float parameter holds exactly the float32 it was sent,
    so anything beyond float32 rounding is a different value.
    """
    if not (math.isfinite(got) and math.isfinite(wanted)):
        return False
    if param_type_is_integer(param_type):
        return int(round(got)) == int(round(wanted))
    try:
        as_sent = struct.unpack("<f", struct.pack("<f", wanted))[0]
    except OverflowError:
        return False
    return abs(got - as_sent) <= 1e-6 * max(1.0, abs(as_sent))


# ---------------------------------------------------------------------------
# Dialects
# ---------------------------------------------------------------------------

class Dialect:
    """What one flight stack does. The PX4 behaviour is the base class.

    Subclasses override only what actually differs, so a diff of this file is
    a list of the real incompatibilities rather than two parallel stacks.
    """

    stack: str = STACK_PX4
    label: str = "PX4"

    # Capability flags, surfaced verbatim at GET /api/capabilities so the UI
    # can hide a control rather than offer one the vehicle will refuse.
    supports_shell: bool = True
    supports_esc_calibration: bool = True
    accel_cal_positions_are_prompted: bool = False
    autotune_style: str = "command"       # "command" | "mode" | "none"
    log_format: str = "ulog"              # "ulog" | "dataflash"
    log_suffix: str = ".ulg"
    firmware_vendor: str = "px4"
    # ArduPilot's mission protocol reserves sequence 0 for home and numbers
    # real items from 1; PX4 numbers items from 0 and keeps home elsewhere. An
    # upload that gets this wrong does not fail — the first item is silently
    # eaten as a home position, so the aircraft flies a mission missing its
    # takeoff.
    mission_seq0_is_home: bool = False
    # The flight mode that runs an uploaded mission.
    mission_mode: str = "MISSION"
    # MAV_CMDs this stack will not take as a mission item, with the reason the
    # operator is shown. PX4 v1.16 to v1.18 have no NAV_LOITER_TURNS in their
    # mission at all: mavlink_mission.cpp answers MAV_MISSION_UNSUPPORTED and
    # the whole upload fails. So a plan carrying one is refused before
    # anything is sent, and the planner greys the item out.
    mission_command_refusals: dict[int, str] = {
        mavutil.mavlink.MAV_CMD_NAV_LOITER_TURNS:
            "PX4 does not fly an orbit with a set number of turns in a mission",
    }
    # Whether the vehicle says, after it has accepted an upload, that it will
    # not fly it. PX4's navigator checks a mission once it is stored (no home
    # for a relative altitude, a required landing missing) and reports the
    # verdict in MISSION_CURRENT.mission_state, NO_MISSION for a rejected one,
    # with the reason in a STATUSTEXT. The MISSION_ACK before it says ACCEPTED
    # either way.
    mission_validity_reported: bool = True
    # What to tell the operator about such a rejection when no STATUSTEXT
    # names the reason. PX4 v1.18's feasibility checker has no log publisher
    # and says why in an EVENT only, which the station does not decode.
    mission_rejection_hint: str = (
        "PX4 names the reason in its event log only. The usual ones are a takeoff "
        "or a landing the vehicle's mission settings require, or a fixed wing "
        "landing approach steeper than it allows."
    )
    # How an integer parameter travels in the float32 field. See the
    # "Parameter value encoding" section above.
    param_encoding: str = PARAM_ENCODING_BYTEWISE
    # Where the vehicle keeps its parameter metadata, read over MAVLink FTP
    # (corvus.param_metadata). PX4 builds the file QGroundControl reads into
    # ROMFS: defaults, descriptions, units and ranges for the running firmware.
    param_metadata_path: str = "/etc/extras/parameters.json.xz"
    param_metadata_format: str = "px4-json"
    # Whether a copy kept on the ground station may stand in for that file.
    # PX4's is fixed when the firmware is built, so the same checksum means
    # the same file (see ParamMetadataCache).
    param_metadata_cacheable: bool = True
    # The parameter file an export writes unless the operator picks another
    # (corvus.param_files): QGroundControl's .params is what PX4 users trade.
    param_file_format: str = "qgc"
    # The STATUSTEXT a failing preflight check is reported in starts with one
    # of these, and the reason follows. PX4 v1.16 to v1.18 report the arming
    # checks as events, and still send this text beside every one of them
    # (HealthAndArmingChecks.cpp, the "LEGACY" pass), whenever the report runs.
    prearm_prefixes: tuple[str, ...] = ("Preflight Fail: ",)
    # Informational STATUSTEXT that is housekeeping rather than news: PX4's
    # logger names every log file it opens, on each arm. Kept in the console,
    # left off the notification board.
    routine_statustext_prefixes: tuple[str, ...] = ("[logger] ",)

    def prearm_failure(self, text: str) -> str | None:
        """The reason in a failing preflight check's STATUSTEXT, or None."""
        for prefix in self.prearm_prefixes:
            if text.startswith(prefix):
                return text[len(prefix):].strip() or None
        return None

    def is_routine_statustext(self, text: str) -> bool:
        """Is this informational line housekeeping the board can do without?"""
        return any(text.startswith(p) for p in self.routine_statustext_prefixes)

    def vehicle_event(self, text: str) -> VehicleEvent | None:
        """The kill switch or disarm this STATUSTEXT reports, or None."""
        return _px4_vehicle_event(text)

    def prearm_event(self, reason: str) -> VehicleEvent | None:
        """The kill switch a failing preflight check names, or None.

        ``reason`` is what :meth:`prearm_failure` returned.
        """
        return _px4_prearm_event(reason)

    # --- modes ---------------------------------------------------------

    def decode_mode(self, custom_mode: int, base_mode: int, mav_type: int) -> str:
        """HEARTBEAT -> the mode name an operator would recognise."""
        try:
            custom = int(custom_mode)
        except (TypeError, ValueError):
            return ""
        main_mode = (custom >> 16) & 0xFF
        sub_mode = (custom >> 24) & 0xFF
        if main_mode == 4:
            return PX4_AUTO_SUBMODE.get(sub_mode, f"SUBMODE_{sub_mode}")
        if main_mode == 3:
            return PX4_POSCTL_SUBMODE.get(sub_mode, f"POSCTL_SUBMODE_{sub_mode}")
        return PX4_MAIN_MODE.get(main_mode, f"MODE_{custom}")

    def mode_label(self, name: str) -> str:
        """The mode name as the operator reads it: a word, not a short form.

        Display only. ``name`` stays the stack's own spelling everywhere else,
        because it is what a mode change sends back.
        """
        return PX4_MODE_LABELS.get(str(name or "")) or _plain_mode_label(name)

    def mode_table(
        self, mav_type: int, firmware: tuple[int, ...] | None = None,
    ) -> dict[str, ModeCommand]:
        """Every mode this vehicle can be commanded into, by name.

        ``firmware`` is the ``(major, minor, patch)`` the vehicle reported in
        AUTOPILOT_VERSION, or None before it has.
        """
        return {
            name: ModeCommand(base, main, sub)
            for name, (base, main, sub) in PX4_MODE_VALUES.items()
            if px4_mode_supported(name, firmware)
        }

    def available_modes(
        self, mav_type: int, firmware: tuple[int, ...] | None = None,
    ) -> list[str]:
        """The selector's modes, most manual first, for this firmware."""
        return [name for name in PX4_AVAILABLE_MODES if px4_mode_supported(name, firmware)]

    def adopt_live_mapping(
        self, mapping: dict[str, Any], mav_type: int,
        firmware: tuple[int, ...] | None = None,
    ) -> dict[str, ModeCommand]:
        """Turn pymavlink's ``mode_mapping()`` into commands, or {} if it cannot.

        pymavlink answers in two shapes because the stacks encode a mode
        differently: PX4 gets ``(base_mode, main_mode, sub_mode)`` triples,
        everything else gets a flat integer. The triples are PX4's, so only
        those are adopted here; a flat mapping reaching this method means
        pymavlink guessed a different stack than the heartbeat did, and the
        built-in table is the safer answer.

        px4_map is a fixed table and an old one. It still names RTGS and
        RATTITUDE, which no supported PX4 flies, and has neither Position Slow
        nor Altitude Cruise. So its names go through the same version check as
        the built-in table, and the built-in table fills in what it lacks.
        """
        live = {
            str(name): ModeCommand(int(value[0]), int(value[1]), int(value[2]))
            for name, value in (mapping or {}).items()
            if isinstance(value, tuple) and len(value) >= 3
        }
        if not live:
            return {}
        usable = self.mode_table(mav_type, firmware)
        usable.update(
            (name, command) for name, command in live.items()
            if px4_mode_supported(name, firmware)
        )
        return usable

    # --- flight --------------------------------------------------------

    def takeoff_plan(self, mav_type: int) -> TakeoffPlan:
        return TakeoffPlan(order="takeoff_then_arm", altitude_frame="amsl")

    def guided_mode(self, mav_type: int) -> str:
        """The mode a position command needs the vehicle to be in, or "".

        PX4 accepts a reposition from any mode and switches itself, so there is
        nothing to ask for.
        """
        return ""

    def reposition_altitude_frame(self, mav_type: int) -> str:
        """How ``MAV_CMD_DO_REPOSITION``'s altitude is sent: "amsl" or "relative".

        PX4 v1.16 to v1.18 read the COMMAND_INT z as AMSL whatever frame it is
        tagged with (the receiver converts relative altitudes only for
        SET_POSITION_TARGET_GLOBAL_INT), so the station adds home itself.
        """
        return "amsl"

    def mission_start_params(self, count: int) -> tuple[float, float]:
        """MISSION_START's (first, last) item for *count* uploaded plan items.

        PX4 is given both ends explicitly rather than its "0 = last item"
        shorthand, which is the one part of the command that moved across
        v1.16 to v1.18.
        """
        return 0.0, float(max(0, int(count) - 1))

    def mission_arm_mode(self, mav_type: int) -> str:
        """The mode to arm in before MISSION_START, or "" to arm afterwards.

        PX4's MISSION_START switches to the mission and arms in one step, so
        nothing has to happen first.
        """
        return ""

    # --- calibration ---------------------------------------------------

    _CALIBRATION: dict[str, CommandPlan] = {}

    def calibration(self, sensor: str) -> CommandPlan | None:
        plan = self._CALIBRATION.get(sensor)
        return plan

    def cancel_calibration(self, sensor: str = "") -> CommandPlan:
        """Stop whatever calibration is running.

        PX4's Commander reads an all-zero PREFLIGHT_CALIBRATION as "cancel",
        whichever calibration is in progress.
        """
        return _cal(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0)

    def accel_position(self, position: str) -> CommandPlan:
        return CommandPlan(unsupported=(
            "PX4 detects each calibration position on its own. There is "
            "nothing for the ground station to confirm"
        ))

    # --- motor test ----------------------------------------------------

    def motor_test(self, motor: int, throttle_pct: float, duration_s: float) -> CommandPlan:
        """Spin one motor on the bench.

        Not ``DO_MOTOR_TEST``: PX4 v1.16 to v1.18 have no handler for it and
        answer UNSUPPORTED (Commander.cpp falls through to ``default``). The
        actuator test is the only path to the outputs, and it is what
        QGroundControl's motor sliders send.
        """
        return _actuator_test(motor, float(throttle_pct) / 100.0, float(duration_s))

    def motor_stop(self, motor: int) -> CommandPlan:
        """Stop one motor's test, whether or not one is running."""
        return _actuator_test(motor, _NAN, 0.0)

    # --- autotune ------------------------------------------------------

    def autotune_plan(self, mav_type: int, enable: bool) -> CommandPlan:
        return CommandPlan(
            command=mavutil.mavlink.MAV_CMD_DO_AUTOTUNE_ENABLE,
            params=(1.0 if enable else 0.0, 0.0, _NAN, _NAN, _NAN, _NAN, _NAN),
        )

    def autotune_mode(self, mav_type: int) -> str:
        return ""

    # --- reporting -----------------------------------------------------

    def capabilities(self, mav_type: int = 0) -> dict[str, Any]:
        """The flags the HTTP layer publishes so the UI can adapt."""
        return {
            "stack": self.stack,
            "label": self.label,
            "shell": self.supports_shell,
            "esc_calibration": self.supports_esc_calibration,
            "accel_cal_prompted": self.accel_cal_positions_are_prompted,
            "autotune": self.autotune_style,
            "autotune_mode": self.autotune_mode(mav_type),
            "log_format": self.log_format,
            "log_suffix": self.log_suffix,
            "firmware_vendor": self.firmware_vendor,
            "guided_mode": self.guided_mode(mav_type),
            "mission_mode": self.mission_mode,
            "takeoff_frame": self.takeoff_plan(mav_type).altitude_frame,
            "param_defaults": bool(self.param_metadata_path),
            "param_file_format": self.param_file_format,
            "calibrations": sorted(
                name for name, plan in self._CALIBRATION.items() if plan.ok
            ),
        }


class PX4Dialect(Dialect):
    """PX4 v1.16-v1.18. The behaviour the base class already describes."""

    _CALIBRATION: dict[str, CommandPlan] = {
        # MAV_CMD_PREFLIGHT_CALIBRATION (241), verified against v1.16-v1.18
        # (Commander.cpp). Unset params are 0; the selected one carries the
        # value PX4 switches on.
        "gyro":        _cal(1.0),
        "compass":     _cal(0.0, 1.0),
        "baro":        _cal(0.0, 0.0, 1.0),
        "accel":       _cal(0.0, 0.0, 0.0, 0.0, 1.0),
        "level":       _cal(0.0, 0.0, 0.0, 0.0, 2.0),
        "accel_quick": _cal(0.0, 0.0, 0.0, 0.0, 4.0),
        "airspeed":    _cal(0.0, 0.0, 0.0, 0.0, 0.0, 1.0),
        # Motor/ESC calibration — props MUST be off; motors run to full PWM.
        "motor":       _cal(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0),
    }


class ArduPilotDialect(Dialect):
    """ArduPilot 4.3-4.6 — Copter, Plane, Rover/Boat, Sub, Tracker, Blimp."""

    stack = STACK_ARDUPILOT
    label = "ArduPilot"

    # ArduPilot has no NSH. SERIAL_CONTROL exists on the wire but carries a
    # passthrough to a *peripheral* port, not a console, so offering a shell
    # would open a terminal that can never answer.
    supports_shell = False
    # ESC calibration is a parameter (ESC_CALIBRATION) plus a reboot, not a
    # command. Doing that behind a button would reboot an operator's aircraft
    # into a mode where the next power-up spins motors; it stays manual.
    supports_esc_calibration = False
    accel_cal_positions_are_prompted = True
    autotune_style = "mode"
    log_format = "dataflash"
    log_suffix = ".bin"
    firmware_vendor = "ardupilot"
    mission_seq0_is_home = True
    mission_mode = "AUTO"
    # ArduCopter, Plane and Rover fly NAV_LOITER_TURNS as a circle of the
    # item's radius, and AP_Mission refuses at upload what it cannot store.
    mission_command_refusals: dict[int, str] = {}
    mission_validity_reported = False
    mission_rejection_hint = ""
    param_encoding = PARAM_ENCODING_C_CAST
    # Generated on request since ArduPilot 4.1; the query adds each
    # parameter's default. There are no descriptions on board, only defaults.
    param_metadata_path = "@PARAM/param.pck?withdefaults=1"
    param_metadata_format = "ardupilot-pck"
    # Generated from the live parameters, and some defaults follow the frame
    # the vehicle is set up as, so a stored copy would be stale by design.
    param_metadata_cacheable = False
    param_file_format = "mission-planner"
    # ArduPilot only ever reports in text: "PreArm: ..." for a check, and
    # "Arm: ..." for the reason an arm command was refused.
    prearm_prefixes = ("PreArm: ", "Arm: ")
    routine_statustext_prefixes = ()

    def vehicle_event(self, text: str) -> VehicleEvent | None:
        return _ardupilot_vehicle_event(text)

    def prearm_event(self, reason: str) -> VehicleEvent | None:
        return _ardupilot_prearm_event(reason)

    # --- modes ---------------------------------------------------------

    @staticmethod
    def _table(mav_type: int) -> dict[int, str]:
        try:
            key = int(mav_type)
        except (TypeError, ValueError):
            key = 0
        table = ARDUPILOT_MODE_TABLES.get(key)
        if table is not None:
            return table
        live = mavutil.mode_mapping_bynumber(key)
        if live:
            return dict(live)
        return ARDUPILOT_COPTER_MODES

    def decode_mode(self, custom_mode: int, base_mode: int, mav_type: int) -> str:
        try:
            custom = int(custom_mode)
        except (TypeError, ValueError):
            return ""
        # A vehicle that is not in a custom mode is in none of these tables.
        # base_mode is the only thing that says so, and reading the table
        # anyway would name a manual-flag heartbeat "STABILIZE".
        try:
            flags = int(base_mode)
        except (TypeError, ValueError):
            flags = mavutil.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED
        if not flags & mavutil.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED:
            return "MANUAL" if flags & mavutil.mavlink.MAV_MODE_FLAG_MANUAL_INPUT_ENABLED else ""
        return self._table(mav_type).get(custom, f"MODE_{custom}")

    def mode_label(self, name: str) -> str:
        # ALT_HOLD, FBWA and QLOITER are the names on ArduPilot's own pages and
        # in its FLTMODE parameters, so they are kept; only the underscores go.
        return _plain_mode_label(name)

    def mode_table(
        self, mav_type: int, firmware: tuple[int, ...] | None = None,
    ) -> dict[str, ModeCommand]:
        custom_enabled = mavutil.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED
        return {
            name: ModeCommand(custom_enabled, number, 0)
            for number, name in self._table(mav_type).items()
            if name not in ARDUPILOT_RETIRED_MODES
        }

    def available_modes(
        self, mav_type: int, firmware: tuple[int, ...] | None = None,
    ) -> list[str]:
        return sorted(self.mode_table(mav_type))

    def adopt_live_mapping(
        self, mapping: dict[str, Any], mav_type: int,
        firmware: tuple[int, ...] | None = None,
    ) -> dict[str, ModeCommand]:
        """Take pymavlink's flat ``{name: number}`` table for this vehicle.

        This is the shape Corvus used to throw away — it is exactly what
        ArduPilot needs, once param2 rather than the packed PX4 word is where
        the number goes. The live mapping is preferred over the built-in table
        because pymavlink reads it from the heartbeat's own MAV_TYPE, so a
        vehicle whose type this module has not heard of still gets real modes.
        """
        custom_enabled = mavutil.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED
        usable: dict[str, ModeCommand] = {}
        for name, value in (mapping or {}).items():
            if isinstance(value, bool) or not isinstance(value, int):
                continue
            text = str(name)
            if text in ARDUPILOT_RETIRED_MODES:
                continue
            usable[text] = ModeCommand(custom_enabled, int(value), 0)
        return usable

    # --- flight --------------------------------------------------------

    def takeoff_plan(self, mav_type: int) -> TakeoffPlan:
        return TakeoffPlan(
            order="mode_arm_takeoff",
            altitude_frame="relative",
            guided_mode=ARDUPILOT_GUIDED_MODE,
            # Plane reads param1 as the minimum climb pitch. 0 is what Mission
            # Planner sends and what ArduPilot reads as "use TKOFF_LVL_PITCH";
            # NaN is rejected outright, which is how a fixed-wing takeoff under
            # the PX4 payload failed with nothing but DENIED to show for it.
            min_pitch_deg=0.0,
        )

    def guided_mode(self, mav_type: int) -> str:
        if int(mav_type or 0) == 5:      # antenna tracker: nothing to fly
            return ""
        return ARDUPILOT_GUIDED_MODE

    def reposition_altitude_frame(self, mav_type: int) -> str:
        """ArduPilot takes DO_REPOSITION in MAV_FRAME_GLOBAL_RELATIVE_ALT."""
        return "relative"

    def mission_start_params(self, count: int) -> tuple[float, float]:
        """Always (0, 0): ArduPilot 4.6 answers any other pair with DENIED.

        Copter, Plane and Rover 4.6 reject a first/last item outright ("not
        supported"); 4.3 to 4.5 ignore both fields. Zero is the one value every
        supported release runs the mission with.
        """
        return 0.0, 0.0

    def mission_arm_mode(self, mav_type: int) -> str:
        """Where an ArduPilot vehicle arms before its mission starts.

        Copter refuses to arm in AUTO unless AUTO_OPTIONS allows it, and it is
        off by default, so a copter (and a rover or sub, the same way
        QGroundControl does it) arms in GUIDED and MISSION_START then switches
        it to AUTO. A plane arms in AUTO itself: a tiltrotor armed in GUIDED
        would spin its motors up in the forward flight position.
        """
        if vehicle_class(mav_type) == VEHICLE_PLANE:
            return self.mission_mode
        return self.guided_mode(mav_type)

    # --- calibration ---------------------------------------------------

    _CALIBRATION: dict[str, CommandPlan] = {
        # ArduPilot's PREFLIGHT_CALIBRATION handler agrees with PX4 on gyro
        # (param1), baro (param3) and the three accelerometer forms (param5 =
        # 1 full / 2 level trim / 4 simple), and on nothing else.
        "gyro":        _cal(1.0),
        "baro":        _cal(0.0, 0.0, 1.0),
        "accel":       _cal(0.0, 0.0, 0.0, 0.0, 1.0),
        "level":       _cal(0.0, 0.0, 0.0, 0.0, 2.0),
        "accel_quick": _cal(0.0, 0.0, 0.0, 0.0, 4.0),
        # Airspeed rides on the same ground-pressure calibration as the baro;
        # there is no separate param6 switch the way PX4 has one.
        "airspeed":    _cal(0.0, 0.0, 1.0),
        # The compass is the real divergence. PX4's param2 means nothing to
        # ArduPilot, which runs an onboard spherical fit started by its own
        # command: param1 = 0 (every compass), param2 = 0 (no retry-on-fail),
        # param3 = 1 (save automatically when it converges), param4 = 0 (no
        # delay), param5 = 0 (do not autoreboot).
        "compass": CommandPlan(
            command=MAV_CMD_DO_START_MAG_CAL,
            params=(0.0, 0.0, 1.0, 0.0, 0.0, _NAN, _NAN),
        ),
        "motor": CommandPlan(unsupported=(
            "ArduPilot calibrates ESCs by setting ESC_CALIBRATION to 3 and "
            "rebooting with the throttle high. It is not a MAVLink command, "
            "so Corvus will not do it behind a button"
        )),
    }

    def cancel_calibration(self, sensor: str = "") -> CommandPlan:
        """Stop a running calibration.

        A magnetometer fit is cancelled by its own command; everything else is
        the all-zero PREFLIGHT_CALIBRATION, which ArduPilot reads as "stop RC
        calibration" and otherwise ignores harmlessly.
        """
        if sensor == "compass":
            return CommandPlan(
                command=MAV_CMD_DO_CANCEL_MAG_CAL,
                params=(0.0, 0.0, 0.0, _NAN, _NAN, _NAN, _NAN),
            )
        return _cal(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0)

    def accel_position(self, position: str) -> CommandPlan:
        """Tell ArduPilot the aircraft is now in the position it asked for."""
        value = ACCELCAL_POSITIONS.get(str(position or "").lower())
        if value is None:
            return CommandPlan(unsupported=f"unknown calibration position: {position}")
        return CommandPlan(
            command=MAV_CMD_ACCELCAL_VEHICLE_POS,
            params=(float(value), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
        )

    # --- motor test ----------------------------------------------------

    def motor_test(self, motor: int, throttle_pct: float, duration_s: float) -> CommandPlan:
        """ArduPilot has no actuator test; DO_MOTOR_TEST is its motor test."""
        return _do_motor_test(motor, throttle_pct, duration_s)

    def motor_stop(self, motor: int) -> CommandPlan:
        return _do_motor_test(motor, 0.0, 0.0)

    # --- autotune ------------------------------------------------------

    def autotune_mode(self, mav_type: int) -> str:
        """The flight mode that *is* the autotune on this vehicle.

        Copter and Plane both have one; Plane's quadplane tune is QAUTOTUNE and
        is left to the operator, because which of the two a quadplane wants
        depends on which half of the airframe is being tuned.
        """
        table = self._table(mav_type)
        names = set(table.values())
        if "AUTOTUNE" in names:
            return "AUTOTUNE"
        return ""

    def autotune_plan(self, mav_type: int, enable: bool) -> CommandPlan:
        mode = self.autotune_mode(mav_type)
        if not mode:
            return CommandPlan(unsupported=(
                "this ArduPilot vehicle has no autotune mode"
            ))
        # Reported, not sent: the bridge switches modes for ArduPilot rather
        # than sending DO_AUTOTUNE_ENABLE, which Copter does not implement.
        return CommandPlan(unsupported="")


class GenericDialect(Dialect):
    """An autopilot that is neither, talked to in common.xml only.

    Mode selection is the one thing that cannot be done generically — there is
    no portable encoding of "which mode" — so it is offered only when
    pymavlink recognised the vehicle well enough to hand over a real table.
    """

    stack = STACK_GENERIC
    label = "Generic"

    supports_shell = False
    supports_esc_calibration = False
    accel_cal_positions_are_prompted = False
    autotune_style = "none"
    mission_mode = ""
    # Nothing is known to be refused, and nothing reports a verdict: the
    # vehicle's own MISSION_ACK is the only answer.
    mission_command_refusals: dict[int, str] = {}
    mission_validity_reported = False
    mission_rejection_hint = ""
    log_format = "unknown"
    log_suffix = ".log"
    firmware_vendor = ""
    # common.xml's own default: the value is cast. An autopilot that packs
    # bytewise says so in AUTOPILOT_VERSION, and the bridge honours that.
    param_encoding = PARAM_ENCODING_C_CAST
    param_metadata_path = ""
    param_metadata_format = ""
    param_metadata_cacheable = False
    # Either wording: text is all there is to go on.
    prearm_prefixes = ("Preflight Fail: ", "PreArm: ")
    routine_statustext_prefixes = ()

    def vehicle_event(self, text: str) -> VehicleEvent | None:
        return _px4_vehicle_event(text) or _ardupilot_vehicle_event(text)

    def prearm_event(self, reason: str) -> VehicleEvent | None:
        return _px4_prearm_event(reason) or _ardupilot_prearm_event(reason)

    def decode_mode(self, custom_mode: int, base_mode: int, mav_type: int) -> str:
        try:
            custom = int(custom_mode)
        except (TypeError, ValueError):
            return ""
        return f"MODE_{custom}" if custom else ""

    def mode_label(self, name: str) -> str:
        return _plain_mode_label(name)

    def mode_table(
        self, mav_type: int, firmware: tuple[int, ...] | None = None,
    ) -> dict[str, ModeCommand]:
        return {}

    def available_modes(
        self, mav_type: int, firmware: tuple[int, ...] | None = None,
    ) -> list[str]:
        return []

    def adopt_live_mapping(
        self, mapping: dict[str, Any], mav_type: int,
        firmware: tuple[int, ...] | None = None,
    ) -> dict[str, ModeCommand]:
        return {}

    _CALIBRATION: dict[str, CommandPlan] = {
        # Only the parameter positions common.xml itself documents.
        "gyro":  _cal(1.0),
        "baro":  _cal(0.0, 0.0, 1.0),
        "accel": _cal(0.0, 0.0, 0.0, 0.0, 1.0),
        "level": _cal(0.0, 0.0, 0.0, 0.0, 2.0),
    }

    def motor_test(self, motor: int, throttle_pct: float, duration_s: float) -> CommandPlan:
        """The common.xml motor test; the actuator test is PX4's own."""
        return _do_motor_test(motor, throttle_pct, duration_s)

    def motor_stop(self, motor: int) -> CommandPlan:
        return _do_motor_test(motor, 0.0, 0.0)

    def autotune_plan(self, mav_type: int, enable: bool) -> CommandPlan:
        return CommandPlan(unsupported=(
            "Corvus does not know how this autopilot runs an autotune"
        ))


_DIALECTS: dict[str, Dialect] = {
    STACK_PX4: PX4Dialect(),
    STACK_ARDUPILOT: ArduPilotDialect(),
    STACK_GENERIC: GenericDialect(),
}


def dialect_for_stack(stack: str) -> Dialect:
    """The dialect for a stack name, defaulting to the generic one."""
    return _DIALECTS.get(stack, _DIALECTS[STACK_GENERIC])


def dialect_for(autopilot: Any) -> Dialect:
    """The dialect for a MAV_AUTOPILOT id, or for the name already stored."""
    return dialect_for_stack(stack_for_autopilot(autopilot))
