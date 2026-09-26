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

# PX4 main_mode values (bits 16-23 of custom_mode).
PX4_MAIN_MODE: dict[int, str] = {
    1: "MANUAL", 2: "ALTCTL", 3: "POSCTL", 4: "AUTO",
    5: "ACRO", 6: "OFFBOARD", 7: "STABILIZED", 8: "RATTITUDE",
}

# PX4 AUTO sub_mode values (bits 24-31 when main_mode == 4). Prefix-less so a
# decoded mode name matches the names in PX4_MODE_VALUES and the selector.
PX4_AUTO_SUBMODE: dict[int, str] = {
    1: "READY", 2: "TAKEOFF", 3: "LOITER",
    4: "MISSION", 5: "RTL", 6: "LAND",
    7: "RTGS", 8: "FOLLOWME",
    9: "PRECLAND", 10: "VTOL_TAKEOFF",
    11: "EXTERNAL1", 12: "EXTERNAL2", 13: "EXTERNAL3",
    14: "EXTERNAL4", 15: "EXTERNAL5", 16: "EXTERNAL6",
    17: "EXTERNAL7", 18: "EXTERNAL8",
}

# (base_mode, main_mode, sub_mode) triples keyed by mode name, mirroring
# pymavlink's px4_map. 81/65/29 are the base_mode bytes PX4 itself sends for
# each family; DO_SET_MODE wants them back verbatim.
PX4_MODE_VALUES: dict[str, tuple[int, int, int]] = {
    "MANUAL": (81, 1, 0), "ALTCTL": (81, 2, 0), "POSCTL": (81, 3, 0),
    "STABILIZED": (81, 7, 0), "ACRO": (65, 5, 0), "RATTITUDE": (65, 8, 0),
    "LOITER": (29, 4, 3), "MISSION": (29, 4, 4), "RTL": (29, 4, 5),
    "LAND": (29, 4, 6), "TAKEOFF": (29, 4, 2), "OFFBOARD": (29, 6, 0),
    "RTGS": (29, 4, 7), "FOLLOWME": (29, 4, 8),
}

PX4_AVAILABLE_MODES: list[str] = [
    "MANUAL", "ALTCTL", "POSCTL", "STABILIZED", "ACRO", "RATTITUDE",
    "LOITER", "MISSION", "RTL", "LAND", "TAKEOFF",
    "OFFBOARD", "RTGS", "FOLLOWME",
]


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

    def prearm_failure(self, text: str) -> str | None:
        """The reason in a failing preflight check's STATUSTEXT, or None."""
        for prefix in self.prearm_prefixes:
            if text.startswith(prefix):
                return text[len(prefix):].strip() or None
        return None

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
        return PX4_MAIN_MODE.get(main_mode, f"MODE_{custom}")

    def mode_table(self, mav_type: int) -> dict[str, ModeCommand]:
        """Every mode this vehicle can be commanded into, by name."""
        return {
            name: ModeCommand(base, main, sub)
            for name, (base, main, sub) in PX4_MODE_VALUES.items()
        }

    def available_modes(self, mav_type: int) -> list[str]:
        return list(PX4_AVAILABLE_MODES)

    def adopt_live_mapping(
        self, mapping: dict[str, Any], mav_type: int,
    ) -> dict[str, ModeCommand]:
        """Turn pymavlink's ``mode_mapping()`` into commands, or {} if it cannot.

        pymavlink answers in two shapes because the stacks encode a mode
        differently: PX4 gets ``(base_mode, main_mode, sub_mode)`` triples,
        everything else gets a flat integer. The triples are PX4's, so only
        those are adopted here; a flat mapping reaching this method means
        pymavlink guessed a different stack than the heartbeat did, and the
        built-in table is the safer answer.
        """
        usable = {
            str(name): ModeCommand(int(value[0]), int(value[1]), int(value[2]))
            for name, value in (mapping or {}).items()
            if isinstance(value, tuple) and len(value) >= 3
        }
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

    def mode_table(self, mav_type: int) -> dict[str, ModeCommand]:
        custom_enabled = mavutil.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED
        return {
            name: ModeCommand(custom_enabled, number, 0)
            for number, name in self._table(mav_type).items()
            if name not in ARDUPILOT_RETIRED_MODES
        }

    def available_modes(self, mav_type: int) -> list[str]:
        return sorted(self.mode_table(mav_type))

    def adopt_live_mapping(
        self, mapping: dict[str, Any], mav_type: int,
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

    def decode_mode(self, custom_mode: int, base_mode: int, mav_type: int) -> str:
        try:
            custom = int(custom_mode)
        except (TypeError, ValueError):
            return ""
        return f"MODE_{custom}" if custom else ""

    def mode_table(self, mav_type: int) -> dict[str, ModeCommand]:
        return {}

    def available_modes(self, mav_type: int) -> list[str]:
        return []

    def adopt_live_mapping(
        self, mapping: dict[str, Any], mav_type: int,
    ) -> dict[str, ModeCommand]:
        return {}

    _CALIBRATION: dict[str, CommandPlan] = {
        # Only the parameter positions common.xml itself documents.
        "gyro":  _cal(1.0),
        "baro":  _cal(0.0, 0.0, 1.0),
        "accel": _cal(0.0, 0.0, 0.0, 0.0, 1.0),
        "level": _cal(0.0, 0.0, 0.0, 0.0, 2.0),
    }

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
