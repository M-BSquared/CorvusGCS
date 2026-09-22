"""ArduPilot safety and sensor schema for the Setup -> Safety & Sensors page.

The ArduPilot twin of :mod:`corvus.safety_config`, and it exists because that
module is not a PX4-flavoured description of a universal idea — it is a list of
PX4 parameter names. Not one of them exists on an ArduPilot vehicle, so the
page came up empty: no fence, no return profile, no failsafes, no battery
thresholds. A ground station that cannot show an operator their RC-loss action
is not a ground station for that aircraft.

Same split, same output shape, same rules as the PX4 module: this owns the
*knowledge* of which parameters bound a flight, the bridge only fetches raw
values, the HTTP layer only serialises, and the frontend renders whatever it is
handed. A field whose parameter the connected firmware did not answer for is
dropped, so one schema serves Copter, Plane, Rover and Sub, and ArduPilot 4.3
through 4.6, with no version switch.

Two things about ArduPilot shape the tables below:

*Bitmasks.* ``FENCE_TYPE``, ``FS_OPTIONS`` and ``ARMING_CHECK`` are bit fields,
and rendering one as a number asks an operator to do binary arithmetic on their
aircraft's safety settings. They come out as ``kind="bitmask"``.

*Centimetres.* The Copter return profile is in centimetres and centimetres per
second — ``RTL_ALT`` of 1500 is 15 m. The unit travels with the field rather
than being silently converted, because the value the page writes has to be the
value the parameter holds; a converted display and an unconverted write is how
an aircraft ends up returning at 15 cm.

Vehicle classes: a handful of enums mean different things on a copter and a
plane (``FENCE_ACTION`` above all), so :func:`build` takes the class. Parameters
that exist on only one vehicle need no such care — ``FS_SHORT_ACTN`` is Plane's
and nothing else answers for it.
"""
from __future__ import annotations

import re
from typing import Any

from .param_fields import bitmask, enum, number, option_label, present, section

# ---------------------------------------------------------------------------
# Option tables
# ---------------------------------------------------------------------------

ON_OFF_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 1, "label": "Enabled"},
]

FENCE_TYPE_BITS: list[dict[str, Any]] = [
    {"bit": 0, "label": "Maximum altitude"},
    {"bit": 1, "label": "Circle around home"},
    {"bit": 2, "label": "Polygon"},
    {"bit": 3, "label": "Minimum altitude"},
]

# FENCE_ACTION is the clearest case of one parameter name meaning different
# things per firmware: a copter brakes, a plane goes to guided, a rover holds.
FENCE_ACTION_OPTIONS: dict[str, list[dict[str, Any]]] = {
    "copter": [
        {"value": 0, "label": "Report only"},
        {"value": 1, "label": "RTL, or land if that fails"},
        {"value": 2, "label": "Always land"},
        {"value": 3, "label": "SmartRTL, then RTL, then land"},
        {"value": 4, "label": "Brake, or land if that fails"},
        {"value": 5, "label": "SmartRTL, or land"},
    ],
    "plane": [
        {"value": 0, "label": "Report only"},
        {"value": 1, "label": "RTL"},
        {"value": 6, "label": "Guided"},
        {"value": 7, "label": "Guided with throttle pass-through"},
    ],
    "rover": [
        {"value": 0, "label": "Report only"},
        {"value": 1, "label": "RTL, or hold if that fails"},
        {"value": 2, "label": "Hold"},
        {"value": 3, "label": "SmartRTL, then RTL, then hold"},
        {"value": 4, "label": "SmartRTL, or hold"},
    ],
}

# FS_THR_ENABLE / FS_GCS_ENABLE on Copter: the same action list, chosen twice.
COPTER_FAILSAFE_ACTION_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 1, "label": "Always RTL"},
    {"value": 2, "label": "Continue the mission in Auto, otherwise RTL"},
    {"value": 3, "label": "Always land"},
    {"value": 4, "label": "SmartRTL, or RTL"},
    {"value": 5, "label": "SmartRTL, or land"},
    {"value": 6, "label": "Auto landing sequence, or RTL"},
    {"value": 7, "label": "Brake, or land"},
]

PLANE_SHORT_FAILSAFE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Continue"},
    {"value": 1, "label": "Circle"},
    {"value": 2, "label": "Fly level"},
    {"value": 3, "label": "Return to launch"},
]

PLANE_LONG_FAILSAFE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Continue"},
    {"value": 1, "label": "Return to launch"},
    {"value": 2, "label": "Glide"},
    {"value": 3, "label": "Deploy parachute"},
    {"value": 4, "label": "Auto landing sequence"},
]

ROVER_FAILSAFE_ACTION_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Nothing"},
    {"value": 1, "label": "RTL"},
    {"value": 2, "label": "Hold"},
    {"value": 3, "label": "SmartRTL, or RTL"},
    {"value": 4, "label": "SmartRTL, or hold"},
    {"value": 5, "label": "Terminate"},
]

EKF_FAILSAFE_ACTION_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Report only"},
    {"value": 1, "label": "Land"},
    {"value": 2, "label": "Hold altitude"},
    {"value": 3, "label": "Land, even from Stabilize"},
]

EKF_FAILSAFE_THRESHOLD_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "0.0 — off"},
    {"value": 1, "label": "0.6 — strict"},
    {"value": 2, "label": "0.8 — default"},
    {"value": 3, "label": "1.0 — relaxed"},
]

FS_OPTIONS_BITS: list[dict[str, Any]] = [
    {"bit": 0, "label": "Continue the mission if RC is lost"},
    {"bit": 1, "label": "Continue the mission if the GCS link is lost"},
    {"bit": 2, "label": "Continue landing if RC is lost"},
    {"bit": 3, "label": "Continue landing if the GCS link is lost"},
    {"bit": 4, "label": "Continue the mission on a low battery"},
    {"bit": 5, "label": "Continue landing on a low battery"},
    {"bit": 6, "label": "Release the gripper on a battery failsafe"},
]

ARMING_CHECK_BITS: list[dict[str, Any]] = [
    {"bit": 0, "label": "All checks"},
    {"bit": 1, "label": "Barometer"},
    {"bit": 2, "label": "Compass"},
    {"bit": 3, "label": "GPS lock"},
    {"bit": 4, "label": "Inertial sensors"},
    {"bit": 5, "label": "Parameters"},
    {"bit": 6, "label": "RC channels"},
    {"bit": 7, "label": "Board voltage"},
    {"bit": 8, "label": "Battery level"},
    {"bit": 10, "label": "Logging available"},
    {"bit": 11, "label": "Hardware safety switch"},
    {"bit": 12, "label": "GPS configuration"},
    {"bit": 13, "label": "System"},
    {"bit": 14, "label": "Mission"},
    {"bit": 15, "label": "Rangefinder"},
    {"bit": 16, "label": "Camera"},
    {"bit": 17, "label": "Auxiliary authorisation"},
    {"bit": 18, "label": "Visual odometry"},
    {"bit": 19, "label": "FFT"},
]

ARMING_RUDDER_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 1, "label": "Arm only"},
    {"value": 2, "label": "Arm and disarm"},
]

# EK3_SRC1_* — which sensor the primary estimator source set reads each state
# from. Switching a rangefinder or a flow camera on is these two writes plus a
# driver, and doing only the driver half is the classic ArduPilot trap: perfect
# readings the EKF never looks at.
EK3_POSZ_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "None"},
    {"value": 1, "label": "Barometer"},
    {"value": 2, "label": "Rangefinder"},
    {"value": 3, "label": "GPS"},
    {"value": 4, "label": "Beacon"},
    {"value": 6, "label": "External navigation"},
]

EK3_VELXY_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "None"},
    {"value": 3, "label": "GPS"},
    {"value": 4, "label": "Beacon"},
    {"value": 5, "label": "Optical flow"},
    {"value": 6, "label": "External navigation"},
    {"value": 7, "label": "Wheel encoder"},
]

RANGEFINDER_ORIENT_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Forward"},
    {"value": 4, "label": "Back"},
    {"value": 24, "label": "Up"},
    {"value": 25, "label": "Down"},
]

# RNGFND1_TYPE. ArduPilot carries well over forty drivers, most of them for one
# discontinued product; the list below is the set an operator is realistically
# holding, and an unrecognised number renders as "Unknown (n)" rather than
# being snapped to None — which would switch a working sensor off.
RANGEFINDER_TYPE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "None"},
    {"value": 1, "label": "Analog"},
    {"value": 2, "label": "MaxBotix I2C"},
    {"value": 5, "label": "PWM input"},
    {"value": 7, "label": "LightWare I2C"},
    {"value": 10, "label": "LightWare serial"},
    {"value": 15, "label": "TeraRanger I2C"},
    {"value": 16, "label": "LidarLite v3 I2C"},
    {"value": 17, "label": "VL53L0X"},
    {"value": 18, "label": "NMEA echo sounder"},
    {"value": 20, "label": "Benewake TF02"},
    {"value": 21, "label": "Benewake TFmini"},
    {"value": 22, "label": "LidarLite v3HP I2C"},
    {"value": 24, "label": "Blue Robotics Ping"},
    {"value": 25, "label": "DroneCAN"},
    {"value": 26, "label": "Benewake TFmini Plus"},
    {"value": 28, "label": "Benewake TF03"},
    {"value": 29, "label": "VL53L1X (short range)"},
    {"value": 30, "label": "LeddarVu8 serial"},
    {"value": 31, "label": "HC-SR04"},
    {"value": 33, "label": "MSP"},
    {"value": 36, "label": "TeraRanger serial"},
    {"value": 38, "label": "NoopLoop TOFSense"},
]

FLOW_TYPE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "None"},
    {"value": 1, "label": "PX4Flow"},
    {"value": 2, "label": "Pixart (PMW3901 / PMW3902)"},
    {"value": 3, "label": "Bebop"},
    {"value": 4, "label": "CXOF"},
    {"value": 5, "label": "MAVLink"},
    {"value": 6, "label": "DroneCAN"},
    {"value": 7, "label": "MSP"},
    {"value": 8, "label": "UPFLOW"},
]

# ---------------------------------------------------------------------------
# Operator-added parameters
# ---------------------------------------------------------------------------

EXTRA_PARAM_LIMIT = 24
_EXTRA_NAME_RE = re.compile(r"^[A-Z][A-Z0-9_]{0,15}$")


def normalise_extra(names: list[str] | tuple[str, ...] | None) -> list[str]:
    """Clean the operator's extra-parameter list: upper-case, unique, bounded.

    Identical rules to the PX4 module's, because they are rules about what a
    MAVLink parameter name can be, not about which stack owns it.
    """
    if not isinstance(names, (list, tuple)):
        return []
    out: list[str] = []
    seen: set[str] = set()
    for raw in names:
        if not isinstance(raw, str):
            continue
        name = raw.strip().upper()
        if not _EXTRA_NAME_RE.match(name) or name in seen:
            continue
        seen.add(name)
        out.append(name)
        if len(out) >= EXTRA_PARAM_LIMIT:
            break
    return out


def extra_fields(names: list[str], values: dict[str, float]) -> list[dict[str, Any]]:
    """The operator's own parameters, each flagged present or absent."""
    fields: list[dict[str, Any]] = []
    for name in names:
        known = name in values
        fields.append({
            "param": name, "label": name, "kind": "number",
            "value": values.get(name, 0.0), "present": known,
            "hint": "" if known else "This firmware did not answer for this parameter.",
        })
    return fields


# ---------------------------------------------------------------------------
# The parameter superset
# ---------------------------------------------------------------------------

RANGEFINDER_DRIVER_PARAMS: list[str] = [
    "RNGFND1_TYPE", "RNGFND1_ORIENT", "RNGFND1_MIN_CM", "RNGFND1_MAX_CM",
    "RNGFND1_MIN", "RNGFND1_MAX", "RNGFND1_GNDCLEAR", "RNGFND1_ADDR",
    "RNGFND1_PIN", "RNGFND1_SCALING", "RNGFND1_OFFSET", "RNGFND_LANDING",
    "SURFTRAK_MODE",
]

FLOW_DRIVER_PARAMS: list[str] = [
    "FLOW_TYPE", "FLOW_ORIENT_YAW", "FLOW_POS_X", "FLOW_POS_Y", "FLOW_POS_Z",
    "FLOW_FXSCALER", "FLOW_FYSCALER", "FLOW_ADDR",
]


def param_names() -> list[str]:
    """Every parameter this page may need, across all four vehicle families.

    A single batched read: the page asks for the superset once and renders what
    came back. A copter answers for ``FS_THR_ENABLE`` and not ``FS_SHORT_ACTN``,
    a plane the other way round, and neither is an error.
    """
    names = [
        # Fence
        "FENCE_ENABLE", "FENCE_TYPE", "FENCE_ACTION", "FENCE_ALT_MAX",
        "FENCE_ALT_MIN", "FENCE_RADIUS", "FENCE_MARGIN", "FENCE_TOTAL",
        "FENCE_AUTOENABLE", "FENCE_OPTIONS", "AVOID_ENABLE", "AVOID_MARGIN",
        # Return to launch
        "RTL_ALT", "RTL_ALT_FINAL", "RTL_CLIMB_MIN", "RTL_CONE_SLOPE",
        "RTL_LOIT_TIME", "RTL_SPEED", "RTL_ALT_TYPE", "RTL_RADIUS",
        "RTL_AUTOLAND", "LAND_SPEED", "LAND_ALT_LOW",
        # Failsafes — Copter/Sub
        "FS_THR_ENABLE", "FS_THR_VALUE", "FS_GCS_ENABLE", "FS_GCS_TIMEOUT",
        "FS_OPTIONS", "FS_EKF_ACTION", "FS_EKF_THRESH", "FS_CRASH_CHECK",
        "FS_VIBE_ENABLE", "FS_DR_ENABLE", "FS_DR_TIMEOUT",
        # Failsafes — Plane
        "FS_SHORT_ACTN", "FS_SHORT_TIMEOUT", "FS_LONG_ACTN", "FS_LONG_TIMEOUT",
        "THR_FAILSAFE", "THR_FS_VALUE", "FS_GCS_ENABL",
        # Failsafes — Rover
        "FS_ACTION", "FS_TIMEOUT", "FS_CRASH_CHECK",
        # Arming and disarming
        "ARMING_CHECK", "ARMING_RUDDER", "DISARM_DELAY", "ARMING_REQUIRE",
        # Estimator sources — the half of a sensor that is not the driver
        "EK3_SRC1_POSZ", "EK3_SRC1_VELXY", "EK3_SRC1_POSXY",
        *RANGEFINDER_DRIVER_PARAMS,
        *FLOW_DRIVER_PARAMS,
    ]
    # FS_CRASH_CHECK is on both the Copter and the Rover list above, and a
    # duplicate would be fetched twice on every page load.
    return list(dict.fromkeys(names))


# ---------------------------------------------------------------------------
# Sections
# ---------------------------------------------------------------------------

def _fence_section(values: dict[str, float], vehicle: str) -> dict[str, Any] | None:
    actions = FENCE_ACTION_OPTIONS.get(vehicle, FENCE_ACTION_OPTIONS["copter"])
    return section("limits", "Flight limits", present([
        enum("FENCE_ENABLE", "Fence", values, ON_OFF_OPTIONS),
        bitmask("FENCE_TYPE", "What the fence limits", values, FENCE_TYPE_BITS,
                hint="A fence with no type selected limits nothing, whatever "
                     "FENCE_ENABLE says."),
        enum("FENCE_ACTION", "Action at the fence", values, actions),
        number("FENCE_ALT_MAX", "Maximum altitude", values, unit="m", step=1, min=0),
        number("FENCE_ALT_MIN", "Minimum altitude", values, unit="m", step=1),
        number("FENCE_RADIUS", "Circle radius", values, unit="m", step=1, min=0,
               hint="Distance from home the vehicle may reach."),
        number("FENCE_MARGIN", "Margin", values, unit="m", step=0.5, min=0,
               hint="How far inside the fence the vehicle starts to be pushed back."),
        enum("FENCE_AUTOENABLE", "Enable automatically", values, ON_OFF_OPTIONS),
        enum("AVOID_ENABLE", "Obstacle avoidance", values, ON_OFF_OPTIONS),
        number("AVOID_MARGIN", "Avoidance margin", values, unit="m", step=0.5, min=0),
    ]), hint="The envelope the vehicle is not allowed to leave, measured from "
             "the home position.")


def _rtl_section(values: dict[str, float]) -> dict[str, Any] | None:
    return section("rtl", "Return to Launch", present([
        number("RTL_ALT", "Return altitude", values, unit="cm", step=100, min=0,
               hint="Centimetres above home — ArduPilot's own unit. 1500 is 15 m. "
                    "0 returns at the current altitude."),
        number("RTL_ALT_FINAL", "Final altitude", values, unit="cm", step=100,
               hint="Altitude it holds at home before landing. 0 lands."),
        number("RTL_CLIMB_MIN", "Minimum climb", values, unit="cm", step=100, min=0,
               hint="Climbed before turning for home, whatever the return altitude is."),
        number("RTL_CONE_SLOPE", "Return cone slope", values, step=0.1, min=0,
               hint="Close to home the return altitude is reduced along this slope, so "
                    "a vehicle hovering overhead does not climb first."),
        number("RTL_LOIT_TIME", "Hold before landing", values, unit="ms", step=100, min=0),
        number("RTL_SPEED", "Return speed", values, unit="cm/s", step=50, min=0,
               hint="0 uses the normal waypoint speed."),
        number("RTL_RADIUS", "Loiter radius", values, unit="m", step=1,
               hint="Fixed wing only."),
        enum("RTL_AUTOLAND", "Automatic landing sequence", values, ON_OFF_OPTIONS),
        number("LAND_SPEED", "Landing descent rate", values, unit="cm/s", step=10, min=0),
        number("LAND_ALT_LOW", "Slow-descent altitude", values, unit="cm", step=50, min=0),
    ]), hint="The path home, flown on an RTL command and by every failsafe "
             "whose action is Return.")


def _failsafe_section(values: dict[str, float], vehicle: str) -> dict[str, Any] | None:
    return section("failsafe", "Failsafes", present([
        # Copter / Sub
        enum("FS_THR_ENABLE", "RC loss", values, COPTER_FAILSAFE_ACTION_OPTIONS),
        number("FS_THR_VALUE", "RC-loss throttle threshold", values, unit="PWM", step=1,
               hint="A throttle channel below this counts as a lost receiver."),
        enum("FS_GCS_ENABLE", "Ground station link loss", values,
             COPTER_FAILSAFE_ACTION_OPTIONS),
        number("FS_GCS_TIMEOUT", "Link-loss timeout", values, unit="s", step=0.5, min=0),
        # Plane
        enum("THR_FAILSAFE", "Throttle failsafe", values, ON_OFF_OPTIONS),
        number("THR_FS_VALUE", "Throttle failsafe threshold", values, unit="PWM", step=1),
        enum("FS_SHORT_ACTN", "Short failsafe action", values,
             PLANE_SHORT_FAILSAFE_OPTIONS),
        number("FS_SHORT_TIMEOUT", "Short failsafe timeout", values, unit="s",
               step=0.5, min=0),
        enum("FS_LONG_ACTN", "Long failsafe action", values,
             PLANE_LONG_FAILSAFE_OPTIONS),
        number("FS_LONG_TIMEOUT", "Long failsafe timeout", values, unit="s",
               step=0.5, min=0),
        # Rover
        enum("FS_ACTION", "Failsafe action", values, ROVER_FAILSAFE_ACTION_OPTIONS),
        number("FS_TIMEOUT", "Failsafe timeout", values, unit="s", step=0.5, min=0),
        # Shared
        enum("FS_EKF_ACTION", "Estimator failure", values, EKF_FAILSAFE_ACTION_OPTIONS),
        enum("FS_EKF_THRESH", "Estimator failure threshold", values,
             EKF_FAILSAFE_THRESHOLD_OPTIONS),
        enum("FS_CRASH_CHECK", "Crash check", values, ON_OFF_OPTIONS),
        enum("FS_VIBE_ENABLE", "Vibration failsafe", values, ON_OFF_OPTIONS),
        enum("FS_DR_ENABLE", "Dead reckoning on GPS loss", values, ON_OFF_OPTIONS),
        number("FS_DR_TIMEOUT", "Dead-reckoning timeout", values, unit="s",
               step=1, min=0),
        bitmask("FS_OPTIONS", "Failsafe exceptions", values, FS_OPTIONS_BITS,
                hint="Each bit lets one activity carry on through a failsafe that "
                     "would otherwise interrupt it."),
    ]), hint=f"What the {vehicle} does when it loses an input it was relying on. "
             "The battery failsafes are on the Battery & Power page, with the "
             "levels that trigger them — every BATT_ parameter lives there.")


def _arming_section(values: dict[str, float]) -> dict[str, Any] | None:
    return section("arming", "Arming", present([
        bitmask("ARMING_CHECK", "Pre-arm checks", values, ARMING_CHECK_BITS,
                hint="Turning a check off does not fix what it was catching. Bit 0 "
                     "enables every check and overrides the rest."),
        enum("ARMING_RUDDER", "Arm with the rudder stick", values,
             ARMING_RUDDER_OPTIONS),
        number("DISARM_DELAY", "Auto-disarm delay", values, unit="s", step=1, min=0,
               hint="Seconds on the ground at zero throttle before the vehicle "
                    "disarms itself. 0 never does."),
    ]), hint="What the vehicle checks before it will spin a motor, and when it "
             "gives up and disarms again.")


def _sensor_toggle(values: dict[str, float], *, label: str,
                   driver_param: str, driver_options: list[dict[str, Any]],
                   source_param: str, source_options: list[dict[str, Any]],
                   source_on: float, extra_on: list[dict[str, Any]] | None = None,
                   ) -> dict[str, Any] | None:
    """The on/off chain for one sensor: a driver *and* an estimator source.

    ArduPilot's version of the trap :mod:`corvus.safety_config` describes for
    PX4. A rangefinder with ``RNGFND1_TYPE`` set streams perfect distances that
    ``EK3_SRC1_POSZ`` never looks at, so the toggle writes both halves in order
    — driver first, so fusion is never switched on ahead of the sensor feeding
    it — and one press does the whole chain.
    """
    if driver_param not in values and source_param not in values:
        return None
    driver_value = values.get(driver_param, 0.0)
    driver_on = int(round(driver_value)) != 0
    source_value = values.get(source_param)
    source_on_now = (source_value is not None
                     and int(round(source_value)) == int(round(source_on)))

    enable: list[dict[str, Any]] = []
    disable: list[dict[str, Any]] = []
    if source_param in values:
        enable.append({"param": source_param, "value": float(source_on)})
        # Back to the default rather than to None: an estimator with no height
        # source at all is a worse aircraft than one back on its barometer.
        disable.append({"param": source_param, "value": _source_default(source_param)})
    enable += list(extra_on or [])

    detail = (option_label(driver_options, driver_value)
              if driver_param in values else "No driver parameter")
    if source_value is not None:
        detail += f" · estimator: {option_label(source_options, source_value)}"

    toggle: dict[str, Any] = {
        "label": label,
        "enabled": driver_on and source_on_now,
        "detail": detail,
        "drivers": [
            {"id": f"{driver_param}:{int(o['value'])}", "label": o["label"],
             "param": driver_param, "value": int(o["value"]), "serial": False}
            for o in driver_options if int(o["value"]) != 0
        ] if driver_param in values else [],
        "selected": (f"{driver_param}:{int(round(driver_value))}"
                     if driver_on else ""),
        "enable": enable,
        "disable": disable,
        "clear": [driver_param] if driver_on else [],
        # Starting or stopping a sensor driver happens at boot.
        "reboot": driver_param in values,
    }
    return toggle


def _source_default(source_param: str) -> float:
    """What an estimator source goes back to when its sensor is switched off."""
    return 1.0 if source_param == "EK3_SRC1_POSZ" else 3.0   # Baro / GPS


def _rangefinder_section(values: dict[str, float]) -> dict[str, Any] | None:
    toggle = _sensor_toggle(
        values, label="Distance sensor",
        driver_param="RNGFND1_TYPE", driver_options=RANGEFINDER_TYPE_OPTIONS,
        source_param="EK3_SRC1_POSZ", source_options=EK3_POSZ_OPTIONS,
        source_on=2.0,
        extra_on=([{"param": "RNGFND_LANDING", "value": 1.0}]
                  if "RNGFND_LANDING" in values else None),
    )
    if toggle is None:
        return None
    return {
        "id": "rangefinder", "title": "Distance sensor", "kind": "toggle",
        "group": "sensors", "icon": "radar",
        "short": "A downward lidar or sonar: height above the ground under the aircraft.",
        "toggle": toggle,
        "presets": [],
        "fields": present([
            enum("RNGFND1_ORIENT", "Orientation", values, RANGEFINDER_ORIENT_OPTIONS,
                 hint="Down is what a landing or terrain-following sensor wants."),
            number("RNGFND1_MIN_CM", "Shortest reading", values, unit="cm",
                   step=1, min=0),
            number("RNGFND1_MAX_CM", "Longest reading", values, unit="cm",
                   step=10, min=0),
            number("RNGFND1_MIN", "Shortest reading", values, unit="m", step=0.01,
                   min=0, hint="ArduPilot 4.6 renamed the centimetre pair to metres."),
            number("RNGFND1_MAX", "Longest reading", values, unit="m", step=0.1, min=0),
            number("RNGFND1_GNDCLEAR", "Ground clearance", values, unit="cm",
                   step=1, min=0,
                   hint="Distance the sensor reads with the vehicle on the ground."),
            number("RNGFND1_ADDR", "I2C address", values, step=1, min=0),
            number("RNGFND1_PIN", "Analog or PWM pin", values, step=1),
            number("RNGFND1_SCALING", "Scaling", values, step=0.01),
            number("RNGFND1_OFFSET", "Offset", values, step=0.01),
            enum("RNGFND_LANDING", "Use for landing", values, ON_OFF_OPTIONS),
            enum("SURFTRAK_MODE", "Surface tracking", values, [
                {"value": 0, "label": "Disabled"},
                {"value": 1, "label": "Track the ground"},
            ]),
        ]),
        "hint": "Switching it on starts the driver and points the estimator's height "
                "source at it — doing only one of the two is the usual reason a "
                "rangefinder reads perfectly and changes nothing.",
    }


def _flow_section(values: dict[str, float]) -> dict[str, Any] | None:
    toggle = _sensor_toggle(
        values, label="Optical flow",
        driver_param="FLOW_TYPE", driver_options=FLOW_TYPE_OPTIONS,
        source_param="EK3_SRC1_VELXY", source_options=EK3_VELXY_OPTIONS,
        source_on=5.0,
    )
    if toggle is None:
        return None
    return {
        "id": "flow", "title": "Optical flow", "kind": "toggle",
        "group": "sensors", "icon": "eye",
        "short": "A downward camera: horizontal velocity without GPS.",
        "toggle": toggle,
        "presets": [],
        "fields": present([
            number("FLOW_ORIENT_YAW", "Mounting yaw", values, unit="cdeg", step=100,
                   hint="Centidegrees — ArduPilot's own unit. 0 is the sensor's "
                        "forward axis aligned with the aircraft's."),
            number("FLOW_POS_X", "Mounting offset, X", values, unit="m", step=0.01),
            number("FLOW_POS_Y", "Mounting offset, Y", values, unit="m", step=0.01),
            number("FLOW_POS_Z", "Mounting offset, Z", values, unit="m", step=0.01,
                   hint="Positive down from the centre of gravity."),
            number("FLOW_FXSCALER", "X scale correction", values, step=1),
            number("FLOW_FYSCALER", "Y scale correction", values, step=1),
            number("FLOW_ADDR", "I2C address", values, step=1, min=0),
            enum("EK3_SRC1_POSXY", "Horizontal position source", values, [
                {"value": 0, "label": "None"},
                {"value": 3, "label": "GPS"},
                {"value": 4, "label": "Beacon"},
                {"value": 6, "label": "External navigation"},
            ], hint="Flow gives velocity, not position. Indoors this is usually None; "
                    "outdoors it stays on GPS."),
        ]),
        "hint": "A downward camera. It needs a distance sensor to scale what it sees, "
                "so switch that on first.",
    }


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def build(values: dict[str, float],
          extra: list[str] | None = None,
          vehicle: str = "copter") -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the Safety & Sensors page description.

    Same return shape as :func:`corvus.safety_config.build`, so the frontend
    renders an ArduPilot aircraft and a PX4 one with the same code: ``sections``
    of ``{id, title, hint, kind, fields[, toggle]}``, plus ``extra`` for the
    parameters the operator added by name.

    *values* holds only the parameters the vehicle actually answered for, so
    every section shrinks or disappears on a firmware that lacks them.
    *vehicle* picks the enum labels that differ per firmware family; an
    unrecognised class falls back to Copter's, which is the most common and
    whose unknown numbers still render as "Unknown (n)" rather than being
    silently rewritten.
    """
    sections = [s for s in (
        _fence_section(values, vehicle),
        _rtl_section(values),
        _failsafe_section(values, vehicle),
        _arming_section(values),
        _rangefinder_section(values),
        _flow_section(values),
    ) if s is not None]

    # A parameter this build already renders is not offered a second time: two
    # controls over one number can disagree until the next read, and the one the
    # operator did not touch is then lying about the aircraft.
    rendered = {
        field["param"]
        for entry in sections
        for field in entry.get("fields", [])
        if field.get("param")
    }
    names = [n for n in (extra or []) if n not in rendered]
    return {
        "stack": "ardupilot",
        "vehicle": vehicle,
        "sections": sections,
        "extra": extra_fields(names, values),
        "extra_limit": EXTRA_PARAM_LIMIT,
    }
