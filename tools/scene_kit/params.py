"""The parameter table a scene's simulated autopilot answers with.

Every configuration page in Corvus — Motors, Safety & Sensors, Radio Control,
PID Tuning, the parameter editor — is drawn from PX4 *parameters* read over
MAVLink. So a screenshot of any of those pages is, in the end, a screenshot of
a parameter table. This module owns that table: the curated values that make a
page look like a real aircraft, and the airframe presets that place the motors.

Two rules shape it.

* **Only answer for what the scene means.** ``corvus/motor_config.py`` and its
  siblings emit a field solely when the vehicle returned that parameter, which
  is the same version-tolerance contract a real v1.16 board gets. Filling every
  known name with a zero would render every enum on its first option and every
  limit at nothing — a page that looks broken rather than a page of a vehicle
  that lacks the feature. So the table is curated, and a parameter left out is
  a deliberate "this airframe does not have it".

* **Values are PX4 defaults or plausible field values**, not round numbers
  picked to look tidy. A reader who knows PX4 must not find a number in a
  screenshot that no aircraft would carry.

Types follow Python's: an ``int`` in the tables below goes on the wire as
``MAV_PARAM_TYPE_INT32``, a ``float`` as ``REAL32``. That distinction is
visible in the parameter editor, so it is not cosmetic.
"""
from __future__ import annotations

import math
import random
from dataclasses import dataclass, field
from typing import Any

# ---------------------------------------------------------------------------
# Airframes
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Rotor:
    """One rotor, in PX4's FRD body frame: +x forward, +y right, +z down."""

    x: float
    y: float
    z: float = 0.0
    ccw: bool = True          # spin direction; CA_ROTORn_KM carries it as a sign
    output: int = 0           # 1-based MAIN pin this motor is wired to

    def params(self, index: int, km: float = 0.05, ct: float = 6.5) -> dict[str, Any]:
        """The ``CA_ROTOR<index>_*`` block for this rotor."""
        return {
            f"CA_ROTOR{index}_PX": round(self.x, 4),
            f"CA_ROTOR{index}_PY": round(self.y, 4),
            f"CA_ROTOR{index}_PZ": round(self.z, 4),
            f"CA_ROTOR{index}_KM": km if self.ccw else -km,
            f"CA_ROTOR{index}_CT": ct,
            f"CA_ROTOR{index}_AX": 0.0,
            f"CA_ROTOR{index}_AY": 0.0,
            f"CA_ROTOR{index}_AZ": -1.0,
        }


@dataclass(frozen=True)
class Airframe:
    """A drawable airframe: the geometry plus the preset that selects it."""

    id: str
    label: str
    autostart: int            # SYS_AUTOSTART — the airframe preset
    ca_airframe: int          # CA_AIRFRAME — the geometry class the allocator solves
    rotors: tuple[Rotor, ...]
    extra: dict[str, Any] = field(default_factory=dict)

    def params(self) -> dict[str, Any]:
        out: dict[str, Any] = {
            "SYS_AUTOSTART": self.autostart,
            "CA_AIRFRAME": self.ca_airframe,
            "CA_ROTOR_COUNT": len(self.rotors),
        }
        for i, rotor in enumerate(self.rotors):
            out.update(rotor.params(i))
            # PWM_MAIN_FUNCn encodes "Motor n" as 100 + n, and the page reads
            # the mapping backwards to answer "which pin is motor 3 on?".
            out[f"PWM_MAIN_FUNC{rotor.output}"] = 100 + i + 1
        out.update(self.extra)
        return out


def _ring(count: int, radius: float, offset_deg: float,
          order: tuple[int, ...], spins: tuple[bool, ...]) -> tuple[Rotor, ...]:
    """Place *count* rotors evenly on a circle, PX4 numbering order.

    *order* is the pin each rotor is wired to, *spins* its direction — both
    indexed the way PX4 numbers motors, so a hexa is one more entry rather than
    a second copy of this function.
    """
    rotors = []
    for i in range(count):
        angle = math.radians(offset_deg + (360.0 / count) * i)
        rotors.append(Rotor(
            x=radius * math.cos(angle),
            y=radius * math.sin(angle),
            ccw=spins[i],
            output=order[i],
        ))
    return tuple(rotors)


# A quad in X: PX4 numbers front-right, rear-left, front-left, rear-right, and
# the diagonal pairs spin against each other. 0.25 m arms is a 500-class
# airframe; the Motors page draws the real distance, so the number matters.
def _quad_x(arm: float) -> tuple[Rotor, ...]:
    d = arm * math.sin(math.radians(45))
    return (
        Rotor(x=d, y=d, ccw=True, output=1),      # 1 front right
        Rotor(x=-d, y=-d, ccw=True, output=2),    # 2 rear left
        Rotor(x=d, y=-d, ccw=False, output=3),    # 3 front left
        Rotor(x=-d, y=d, ccw=False, output=4),    # 4 rear right
    )


AIRFRAMES: dict[str, Airframe] = {
    "quad_x": Airframe(
        id="quad_x", label="Quadrotor X (500 class)",
        autostart=4001, ca_airframe=0, rotors=_quad_x(0.25),
    ),
    "quad_x_heavy": Airframe(
        id="quad_x_heavy", label="Quadrotor X (900 class)",
        autostart=4001, ca_airframe=0, rotors=_quad_x(0.45),
        extra={"MPC_THR_HOVER": 0.55, "MPC_XY_VEL_MAX": 12.0},
    ),
    "hexa_x": Airframe(
        id="hexa_x", label="Hexarotor X",
        autostart=6001, ca_airframe=0,
        rotors=_ring(
            6, 0.33, 30.0,
            order=(1, 2, 3, 4, 5, 6),
            spins=(True, False, True, False, True, False),
        ),
        extra={"MPC_THR_HOVER": 0.45},
    ),
    "octo_x": Airframe(
        id="octo_x", label="Octorotor X",
        autostart=8001, ca_airframe=0,
        rotors=_ring(
            8, 0.40, 22.5,
            order=(1, 2, 3, 4, 5, 6, 7, 8),
            spins=(True, False, True, False, True, False, True, False),
        ),
    ),
    "vtol_quad": Airframe(
        id="vtol_quad", label="Standard VTOL (quad + pusher)",
        autostart=13000, ca_airframe=2, rotors=_quad_x(0.40),
        extra={
            "PWM_MAIN_FUNC5": 105,     # the pusher, on MAIN 5
            "FW_AT_EN": 0,
        },
    ),
    "fixed_wing": Airframe(
        id="fixed_wing", label="Fixed wing (single motor)",
        autostart=2100, ca_airframe=1,
        rotors=(Rotor(x=-0.35, y=0.0, ccw=True, output=1),),
        extra={"FW_AT_EN": 1},
    ),
}

DEFAULT_AIRFRAME = "quad_x"


# ---------------------------------------------------------------------------
# The curated parameter sets, one per page that reads them
# ---------------------------------------------------------------------------

# Output protocol and the board's banks. A board that answers for AUX has an IO
# co-processor; leaving PWM_AUX_* out is how a scene says "FMU only".
OUTPUTS: dict[str, Any] = {
    "PWM_MAIN_TIM0": -4,      # DShot600 on the first timer group
    "PWM_MAIN_TIM1": -4,
    "PWM_MAIN_TIM2": 400,
    "PWM_MAIN_TIM3": 400,
    "DSHOT_CONFIG": 600,
    "PWM_MAIN_MIN": 1000,
    "PWM_MAIN_MAX": 2000,
    "PWM_MAIN_DIS1": 900,
    "PWM_MAIN_DIS2": 900,
    "PWM_MAIN_DIS3": 900,
    "PWM_MAIN_DIS4": 900,
}

# Safety & Sensors. Values are a real permissive-but-sane field setup: a 500 m
# geofence, a 120 m ceiling (the EU open-category limit Corvus' own takeoff
# slider stops at), RTL at 60 m, and every loss PX4 can detect given an action.
SAFETY: dict[str, Any] = {
    "GF_ACTION": 2,                # Hold
    "GF_MAX_HOR_DIST": 500.0,
    "GF_MAX_VER_DIST": 120.0,
    "GF_SOURCE": 0,
    "GF_PREDICT": 1,
    "LNDMC_ALT_MAX": 120.0,
    "RTL_TYPE": 0,
    "RTL_RETURN_ALT": 60.0,
    "RTL_DESCEND_ALT": 30.0,
    "RTL_CONE_ANG": 45,
    "RTL_MIN_DIST": 10.0,
    "RTL_LAND_DELAY": 0.0,
    "RTL_LOITER_RAD": 50.0,
    "NAV_RCL_ACT": 2,              # RC loss -> Return
    "COM_RC_LOSS_T": 0.5,
    "NAV_DLL_ACT": 2,              # datalink loss -> Return
    "COM_DL_LOSS_T": 10,
    "COM_LOW_BAT_ACT": 3,          # warning -> return -> land
    "COM_POSCTL_NAVL": 0,
    "COM_ACT_FAIL_ACT": 0,
    "COM_QC_ACT": 0,
    "COM_FAIL_ACT_T": 5.0,
    "COM_DISARM_LAND": 2.0,
    "COM_DISARM_PRFLT": 10.0,
    "BAT_LOW_THR": 0.15,
    "BAT_CRIT_THR": 0.07,
    "BAT_EMERGEN_THR": 0.05,
}

# The distance sensor and optical-flow half of that page. Kept separate so a
# scene can show the switches off (the bare aircraft) or on (driver and
# estimator together, which is the pairing the README calls out).
RANGEFINDER: dict[str, Any] = {
    "SENS_EN_SF1XX": 4,            # Lightware SF11/c
    "EKF2_RNG_CTRL": 1,
    "EKF2_RNG_AID": 1,
    "EKF2_HGT_REF": 2,
    "EKF2_RNG_A_HMAX": 7.0,
    "EKF2_RNG_A_VMAX": 1.0,
    "EKF2_RNG_POS_Z": 0.05,
    "EKF2_RNG_PITCH": 0.0,
    "EKF2_RNG_DELAY": 5.0,
    "EKF2_RNG_NOISE": 0.1,
    "EKF2_RNG_SFE": 0.05,
    "EKF2_RNG_QLTY_T": 1.0,
    "MPC_ALT_MODE": 2,
}

OPTICAL_FLOW: dict[str, Any] = {
    "SENS_EN_PMW3901": 1,
    "EKF2_OF_CTRL": 1,
    "EKF2_OF_DELAY": 20.0,
    "EKF2_OF_QMIN": 1,
    "EKF2_OF_QMIN_GND": 0,
    "EKF2_OF_N_MIN": 0.15,
    "EKF2_OF_N_MAX": 0.5,
    "EKF2_OF_POS_X": 0.0,
    "EKF2_OF_POS_Y": 0.0,
    "EKF2_OF_POS_Z": 0.0,
    "SENS_FLOW_ROT": 0,
    "SENS_FLOW_MINHGT": 0.08,
    "SENS_FLOW_MAXHGT": 25.0,
    "SENS_FLOW_MAXR": 2.5,
}

# Radio Control. RC_MAP_* is what the transmitter drawing reads for the sticks;
# the switches are the six-switch layout the drawn model is built around.
RC_MODE2: dict[str, Any] = {
    "COM_RC_IN_MODE": 0,           # RC and joystick
    "RC_CHAN_CNT": 16,
    "RC_MAP_ROLL": 1,
    "RC_MAP_PITCH": 2,
    "RC_MAP_THROTTLE": 3,
    "RC_MAP_YAW": 4,
    "RC_MAP_FLTMODE": 5,
    "RC_MAP_ARM_SW": 7,
    "RC_MAP_KILL_SW": 8,
    "RC_MAP_RETURN_SW": 6,
    "RC_MAP_AUX1": 9,
    "RC_MAP_AUX2": 10,
    "RC_MAP_FAILSAFE": 0,
    "RC_RSSI_PWM_CHAN": 0,
    "COM_RC_STICK_OV": 30.0,
    # The six flight-mode slots the mode switch selects between.
    "COM_FLTMODE1": 0,             # Manual
    "COM_FLTMODE2": 2,             # Position
    "COM_FLTMODE3": 1,             # Altitude
    "COM_FLTMODE4": 3,             # Hold
    "COM_FLTMODE5": 4,             # Mission
    "COM_FLTMODE6": 5,             # Return
}


def _rc_channel_calibration(channels: int = 16) -> dict[str, Any]:
    """``RC<n>_MIN/MAX/TRIM/DZ/REV`` for a calibrated transmitter.

    A calibration that came out perfectly symmetric never happens on a real
    handset, so the endpoints carry the few microseconds of asymmetry a real
    calibration leaves behind — that is what makes the channel bars on the
    Radio Control page look measured rather than declared.
    """
    out: dict[str, Any] = {}
    skew = (0, 3, -2, 5, -4, 1, 0, 2, -3, 4, 0, -1, 2, 0, -2, 3, 0, 1)
    for n in range(1, channels + 1):
        s = skew[(n - 1) % len(skew)]
        out[f"RC{n}_MIN"] = 1000.0 + s
        out[f"RC{n}_MAX"] = 2000.0 + s
        out[f"RC{n}_TRIM"] = 1500.0 + s
        out[f"RC{n}_DZ"] = 10.0
        # Channel 2 (pitch) reversed is the common case on a Mode 2 handset.
        out[f"RC{n}_REV"] = -1.0 if n == 2 else 1.0
    return out


# PID Tuning — PX4 v1.18 multirotor defaults, with the rate gains nudged the
# way a tuned 500-class quad ends up.
TUNING: dict[str, Any] = {
    "MC_ROLLRATE_P": 0.15, "MC_ROLLRATE_I": 0.2, "MC_ROLLRATE_D": 0.0036,
    "MC_ROLLRATE_FF": 0.0, "MC_ROLLRATE_K": 1.0, "MC_RR_INT_LIM": 0.3,
    "MC_PITCHRATE_P": 0.15, "MC_PITCHRATE_I": 0.2, "MC_PITCHRATE_D": 0.0036,
    "MC_PITCHRATE_FF": 0.0, "MC_PITCHRATE_K": 1.0, "MC_PR_INT_LIM": 0.3,
    "MC_YAWRATE_P": 0.2, "MC_YAWRATE_I": 0.1, "MC_YAWRATE_D": 0.0,
    "MC_YAWRATE_FF": 0.0, "MC_YAWRATE_K": 1.0, "MC_YR_INT_LIM": 0.3,
    "MC_ROLL_P": 6.5, "MC_PITCH_P": 6.5, "MC_YAW_P": 2.8, "MC_YAW_WEIGHT": 0.4,
    "MC_ROLLRATE_MAX": 220.0, "MC_PITCHRATE_MAX": 220.0, "MC_YAWRATE_MAX": 200.0,
    "MPC_XY_VEL_P_ACC": 1.8, "MPC_XY_VEL_I_ACC": 0.4, "MPC_XY_VEL_D_ACC": 0.2,
    "MPC_XY_VEL_MAX": 12.0,
    "MPC_Z_VEL_P_ACC": 4.0, "MPC_Z_VEL_I_ACC": 2.0, "MPC_Z_VEL_D_ACC": 0.0,
    "MPC_Z_VEL_MAX_UP": 3.0, "MPC_Z_VEL_MAX_DN": 1.5,
    "MPC_TILTMAX_AIR": 45.0, "MPC_THR_HOVER": 0.42,
    "MPC_XY_P": 0.95, "MPC_Z_P": 1.0,
    "MPC_ACC_HOR": 3.0, "MPC_ACC_UP_MAX": 4.0, "MPC_ACC_DOWN_MAX": 3.0,
    "MPC_JERK_AUTO": 4.0, "MPC_JERK_MAX": 8.0,
    "MC_AT_EN": 0, "MC_AT_APPLY": 2, "MC_AT_SYSID_AMP": 0.7, "MC_AT_RISE_TIME": 0.14,
}

# Everything else an operator sees in the editor and the top bar: the battery,
# the estimator, the serial ports. Not read by a specific page — it is the
# context that makes a parameter list look like an aircraft's.
GENERAL: dict[str, Any] = {
    "BAT1_N_CELLS": 6,
    "BAT1_CAPACITY": 16000.0,
    "BAT1_V_CHARGED": 4.15,
    "BAT1_V_EMPTY": 3.5,
    "BAT1_R_INTERNAL": 0.005,
    "BAT_AVRG_CURRENT": 15.0,
    "MAV_TYPE": 2,
    "MAV_SYS_ID": 1,
    "MAV_0_RATE": 1200,
    "MAV_0_MODE": 0,
    "MAV_1_CONFIG": 102,
    "MAV_1_MODE": 2,
    "SER_TEL1_BAUD": 57600,
    "SER_TEL2_BAUD": 921600,
    "EKF2_MAG_TYPE": 0,
    "EKF2_GPS_CTRL": 7,
    "EKF2_BARO_CTRL": 1,
    "EKF2_IMU_POS_X": 0.0,
    "EKF2_IMU_POS_Y": 0.0,
    "EKF2_IMU_POS_Z": 0.0,
    "COM_ARM_WO_GPS": 0,
    "COM_OBL_RC_ACT": 0,
    "COM_PREARM_MODE": 0,
    "NAV_ACC_RAD": 2.0,
    "NAV_MC_ALT_RAD": 0.8,
    "MIS_TAKEOFF_ALT": 10.0,
    "GPS_1_CONFIG": 201,
    "SDLOG_MODE": 0,
    "SDLOG_PROFILE": 3,
    "CBRK_SUPPLY_CHK": 0,
    "CBRK_USB_CHK": 0,
}

# Calibration state. A vehicle that has never been calibrated shows every
# offset at zero, which is exactly what a reader of a calibration screenshot
# should not see — so the scene ships the offsets a calibrated board carries.
CALIBRATED: dict[str, Any] = {
    "CAL_ACC0_ID": 3014666,
    "CAL_ACC0_XOFF": 0.0421, "CAL_ACC0_YOFF": -0.0136, "CAL_ACC0_ZOFF": 0.1893,
    "CAL_ACC0_XSCALE": 1.0021, "CAL_ACC0_YSCALE": 0.9977, "CAL_ACC0_ZSCALE": 1.0034,
    "CAL_GYRO0_ID": 3014658,
    "CAL_GYRO0_XOFF": -0.0031, "CAL_GYRO0_YOFF": 0.0092, "CAL_GYRO0_ZOFF": 0.0014,
    "CAL_MAG0_ID": 396809,
    "CAL_MAG0_XOFF": 0.0713, "CAL_MAG0_YOFF": -0.1502, "CAL_MAG0_ZOFF": 0.0338,
    "CAL_MAG0_ROT": -1,
    "SENS_BOARD_ROT": 0,
    "SENS_BOARD_X_OFF": 0.0,
    "SENS_BOARD_Y_OFF": 0.0,
    "SENS_BOARD_Z_OFF": 0.0,
}


def build_table(
    airframe: str = DEFAULT_AIRFRAME,
    *,
    rangefinder: bool = False,
    optical_flow: bool = False,
    rc: bool = True,
    rc_channels: int = 16,
    overrides: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Assemble the parameter table a scene's vehicle answers with.

    The switches are the ones a screenshot actually turns on and off: which
    airframe is drawn, whether the sensor half of the Safety page is populated,
    whether this vehicle has a transmitter bound at all. Anything finer goes
    through *overrides*, which is applied last and can also *remove* a
    parameter by mapping it to ``None`` — the way a scene says "this firmware
    does not have that one".
    """
    frame = AIRFRAMES.get(airframe)
    if frame is None:
        raise KeyError(
            f"unknown airframe {airframe!r}; known: {', '.join(sorted(AIRFRAMES))}")

    table: dict[str, Any] = {}
    table.update(frame.params())
    table.update(OUTPUTS)
    table.update(SAFETY)
    table.update(TUNING)
    table.update(GENERAL)
    table.update(CALIBRATED)
    if rangefinder:
        table.update(RANGEFINDER)
    if optical_flow:
        table.update(OPTICAL_FLOW)
    if rc:
        table.update(RC_MODE2)
        table.update(_rc_channel_calibration(rc_channels))

    for name, value in (overrides or {}).items():
        if value is None:
            table.pop(name, None)
        else:
            table[name] = value
    return table


# ---------------------------------------------------------------------------
# A full-sized table, for the parameter editor
# ---------------------------------------------------------------------------

# The editor's screenshot is a screenshot of scale: a real PX4 board answers
# with something over a thousand parameters, and a list of ninety does not look
# like one. These families fill the rest out. Each entry is a name template, a
# count, and the value the generated members take — so the list reads like a
# firmware's, with the curated values above still winning where they overlap.
_FILLER_FAMILIES: tuple[tuple[str, int, Any], ...] = (
    ("EKF2_{n}_NOISE", 12, 0.5),
    ("EKF2_ACC_B_NOISE", 1, 0.003),
    ("MPC_{n}_LIM", 8, 1.0),
    ("SENS_{n}_ROT", 6, 0),
    ("CBRK_{n}", 6, 0),
    ("SDLOG_{n}", 6, 0),
)


def _filler(count: int, seed: int = 20260916) -> dict[str, Any]:
    """Synthesise *count* extra parameters with firmware-shaped names.

    These exist to give the editor its real weight. They are deliberately
    recognisable as a family (``EKF2_``, ``MPC_``, ``SENS_``) and deliberately
    not named after any parameter that carries meaning, so nobody reads a value
    off a screenshot and tries it on an aircraft.
    """
    rng = random.Random(seed)
    prefixes = ("EKF2", "MPC", "MC", "FW", "SENS", "COM", "NAV", "MIS", "MAV",
                "SER", "SDLOG", "BAT", "GPS", "RTL", "GF", "LNDMC", "PWM", "CA",
                "UAVCAN", "TRIG", "MNT", "PAYLOAD", "VT", "ASPD", "IMU")
    suffixes = ("GAIN", "LIM", "MAX", "MIN", "TC", "EN", "CFG", "MODE", "RATE",
                "THR", "DELAY", "NOISE", "OFF", "SCALE", "CTRL", "TEST", "ID",
                "TIME", "DIST", "ANG", "ACC", "VEL", "POS", "REF", "MASK")
    out: dict[str, Any] = {}
    while len(out) < count:
        name = f"{rng.choice(prefixes)}_{rng.choice(suffixes)}{rng.randint(0, 9)}"
        if len(name) > 16 or name in out:
            continue
        # Half integers, half reals, so the editor's type column has both.
        out[name] = (rng.randint(0, 6) if rng.random() < 0.45
                     else round(rng.uniform(0.0, 25.0), 3))
    return out


def build_full_table(base: dict[str, Any], total: int = 1180) -> dict[str, Any]:
    """*base*, padded out to roughly a real firmware's parameter count."""
    table = dict(base)
    missing = max(0, total - len(table))
    for name, value in _filler(missing).items():
        table.setdefault(name, value)
    return table
