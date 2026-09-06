"""PX4 safety and sensor parameter schema for the Setup -> Safety & Sensors page.

Same split as :mod:`corvus.motor_config`: this module owns the *knowledge* of
which PX4 parameters bound a flight and which ones bring a rangefinder or an
optical-flow sensor online, the bridge only fetches raw values, and the HTTP
layer only serialises. The frontend renders whatever description it is handed
and never hardcodes a PX4 parameter name.

Two kinds of section come out of :func:`build`:

``kind="fields"``
    A plain form — the flight envelope (maximum distance and height), the
    return-to-launch profile, the failsafe action for every loss the autopilot
    can detect, and the battery thresholds that trigger them.

``kind="toggle"``
    A sensor that is *off* until the operator turns it on. Bringing a ground
    lidar or an optical-flow sensor up is not one parameter: a driver has to be
    started **and** the estimator has to be told to fuse what it produces. Doing
    only half of that is the classic PX4 trap — a rangefinder that streams
    perfect distances the EKF ignores. So the toggle carries the exact ordered
    write list for both halves, and one press does the whole chain. The few
    settings that remain genuinely per-airframe (mounting offset, height limits,
    quality gates) stay as ordinary fields underneath.

Version tolerance (AGENTS.md: PX4 v1.16 / v1.17 / v1.18 are the target set, and
a parameter that is absent must never break the page):

* Every field and every driver is *candidate* only. A firmware without, say,
  ``COM_QC_ACT`` shows one field fewer; a board whose firmware carries no
  ``SENS_EN_PAW3902`` simply does not offer that sensor.
* Both the modern estimator controls (``EKF2_RNG_CTRL``, ``EKF2_OF_CTRL``,
  v1.14+) and their predecessors (``EKF2_RNG_AID``, the optical-flow bit in
  ``EKF2_AID_MASK``) are candidates, so the toggle writes whichever the
  connected firmware actually understands.
* An enum value the schema does not know is preserved verbatim as an extra
  "Unknown (n)" option. Silently rewriting an aircraft's RC-loss action because
  this build did not recognise the number would be far worse than showing it.
"""

from __future__ import annotations

from typing import Any

# EKF2_AID_MASK bit 1 — the pre-v1.14 way to switch optical-flow fusion on.
# Kept as a numeric mask because that firmware has no EKF2_OF_CTRL to write.
AID_MASK_FLOW_BIT = 2

ON_OFF_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Off"},
    {"value": 1, "label": "On"},
]

# GF_ACTION — what the vehicle does when it reaches the geofence.
GEOFENCE_ACTION_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "None"},
    {"value": 1, "label": "Warning"},
    {"value": 2, "label": "Hold"},
    {"value": 3, "label": "Return"},
    {"value": 4, "label": "Terminate"},
    {"value": 5, "label": "Land"},
]

GEOFENCE_SOURCE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "GPS"},
    {"value": 1, "label": "Global position"},
]

RTL_TYPE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Home or rally point, direct"},
    {"value": 1, "label": "Mission landing or rally point, direct"},
    {"value": 2, "label": "Mission landing along the mission path"},
    {"value": 3, "label": "Closest destination, direct"},
]

# RTL_CONE_ANG — the cone the vehicle climbs inside on the way home. A free
# number field would be wrong: PX4 accepts only these half-angles.
RTL_CONE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled (climb to return altitude)"},
    {"value": 25, "label": "25°"},
    {"value": 45, "label": "45°"},
    {"value": 65, "label": "65°"},
    {"value": 80, "label": "80°"},
    {"value": 90, "label": "90° (climb only if below)"},
]

# NAV_RCL_ACT / NAV_DLL_ACT share one action set. 4 is deliberately absent —
# PX4 does not define it.
LINK_LOSS_ACTION_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 1, "label": "Hold"},
    {"value": 2, "label": "Return"},
    {"value": 3, "label": "Land"},
    {"value": 5, "label": "Terminate"},
    {"value": 6, "label": "Disarm"},
]

LOW_BATTERY_ACTION_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Warning only"},
    {"value": 1, "label": "Return"},
    {"value": 2, "label": "Land"},
    {"value": 3, "label": "Return at critical, land at emergency"},
]

POSITION_LOSS_ACTION_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Altitude or Manual"},
    {"value": 1, "label": "Land or Terminate"},
]

ACTUATOR_FAILURE_ACTION_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 1, "label": "Hold"},
    {"value": 2, "label": "Land"},
    {"value": 3, "label": "Return"},
    {"value": 4, "label": "Terminate"},
]

QUADCHUTE_ACTION_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Warning only"},
    {"value": 1, "label": "Return"},
    {"value": 2, "label": "Land"},
    {"value": 3, "label": "Hold"},
]

# EKF2_RNG_CTRL — how the estimator uses the rangefinder. "Conditional" is the
# right default for a ground lidar: height comes from the sensor only when the
# vehicle is low and slow enough for the reading to be trustworthy.
RANGE_CTRL_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 1, "label": "Conditional (range aid)"},
    {"value": 2, "label": "Always"},
]

HEIGHT_REF_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Barometer"},
    {"value": 1, "label": "GPS"},
    {"value": 2, "label": "Range sensor"},
    {"value": 3, "label": "Vision"},
]

# MPC_ALT_MODE — what the altitude stick means once a rangefinder is fused.
ALT_MODE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Altitude following"},
    {"value": 1, "label": "Terrain following"},
    {"value": 2, "label": "Terrain hold"},
]

# SENS_FLOW_ROT — the flow sensor's yaw relative to the airframe.
FLOW_ROTATION_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "No rotation"},
    {"value": 1, "label": "Yaw 45°"},
    {"value": 2, "label": "Yaw 90°"},
    {"value": 3, "label": "Yaw 135°"},
    {"value": 4, "label": "Yaw 180°"},
    {"value": 5, "label": "Yaw 225°"},
    {"value": 6, "label": "Yaw 270°"},
    {"value": 7, "label": "Yaw 315°"},
]

# PX4's serial-port selector, shared by every *_CFG driver parameter. A serial
# rangefinder is "enabled" by naming the port it is wired to, so this list is
# the second half of the two-step picker the toggle renders.
SERIAL_PORT_OPTIONS: list[dict[str, Any]] = [
    {"value": 102, "label": "TELEM 2"},
    {"value": 101, "label": "TELEM 1"},
    {"value": 103, "label": "TELEM 3"},
    {"value": 104, "label": "TELEM/SERIAL 4"},
    {"value": 201, "label": "GPS 1"},
    {"value": 202, "label": "GPS 2"},
    {"value": 203, "label": "GPS 3"},
    {"value": 6, "label": "UART 6"},
    {"value": 301, "label": "Wifi port"},
    {"value": 401, "label": "Ext2"},
]

# Rangefinders started over I2C/SPI/PWM by a plain enable parameter. Several of
# them encode the *model* in the value, so one parameter yields several drivers.
RANGEFINDER_BUS_DRIVERS: list[tuple[str, list[tuple[int, str]]]] = [
    ("SENS_EN_SF1XX", [
        (1, "Lightware SF10/a"),
        (2, "Lightware SF10/b"),
        (3, "Lightware SF10/c"),
        (4, "Lightware SF11/c"),
        (5, "Lightware SF/LW20/b"),
        (6, "Lightware SF/LW20/c"),
        (7, "Lightware SF/LW30/d"),
    ]),
    ("SENS_EN_LL40LS", [
        (1, "Lidar-Lite (PWM)"),
        (2, "Lidar-Lite (I2C)"),
    ]),
    ("SENS_EN_MB12XX", [(1, "Maxbotix MB12xx (I2C)")]),
    ("SENS_EN_VL53L1X", [(1, "ST VL53L1X (I2C)")]),
    ("SENS_EN_PGA460", [(1, "TI PGA460")]),
    ("SENS_EN_GY_US42", [(1, "GY-US42 (I2C)")]),
    ("SENS_EN_TFMINI", [(1, "Benewake TFmini (legacy enable)")]),
]

# Rangefinders started by naming a serial port instead of a 0/1 enable.
RANGEFINDER_SERIAL_DRIVERS: list[tuple[str, str]] = [
    ("SENS_TFMINI_CFG", "Benewake TFmini / TF02 (serial)"),
    ("SENS_SF0X_CFG", "Lightware SF02 / SF10 / SF11 (serial)"),
    ("SENS_ULAND_CFG", "Aerotenna uLanding (serial)"),
    ("SENS_CM8JL65_CFG", "Lanbao CM8JL65 (serial)"),
    ("SENS_LEDDAR1_CFG", "LeddarOne (serial)"),
]

FLOW_BUS_DRIVERS: list[tuple[str, list[tuple[int, str]]]] = [
    ("SENS_EN_PMW3901", [(1, "PMW3901 (SPI)")]),
    ("SENS_EN_PAW3902", [(1, "PAW3902 (SPI)")]),
]

# Estimator controls that switch rangefinder fusion on, most modern first. Only
# the first one the firmware answers for is used, so v1.16+ writes
# EKF2_RNG_CTRL and a pre-v1.14 board writes EKF2_RNG_AID.
RANGE_FUSION_CANDIDATES: list[tuple[str, list[dict[str, Any]], float]] = [
    ("EKF2_RNG_CTRL", RANGE_CTRL_OPTIONS, 1.0),
    ("EKF2_RNG_AID", ON_OFF_OPTIONS, 1.0),
]

FLOW_FUSION_CANDIDATES: list[tuple[str, list[dict[str, Any]], float]] = [
    ("EKF2_OF_CTRL", ON_OFF_OPTIONS, 1.0),
]

# The sensor a vehicle reports over MAVLink rather than through a local driver.
# It needs no driver parameter at all — only the estimator half of the chain.
EXTERNAL_DRIVER_ID = "external"


def param_names() -> list[str]:
    """Every parameter the Safety & Sensors page may need, in one flat list.

    Handed to the bridge as a single batched read: the page asks for the whole
    superset once and renders what came back, rather than probing name by name.
    """
    names: list[str] = [
        # Flight limits
        "GF_ACTION", "GF_MAX_HOR_DIST", "GF_MAX_VER_DIST", "GF_SOURCE", "GF_PREDICT",
        "LNDMC_ALT_MAX",
        # Return to launch
        "RTL_TYPE", "RTL_RETURN_ALT", "RTL_DESCEND_ALT", "RTL_CONE_ANG",
        "RTL_MIN_DIST", "RTL_LAND_DELAY", "RTL_LOITER_RAD",
        # Failsafe actions
        "NAV_RCL_ACT", "COM_RC_LOSS_T", "NAV_DLL_ACT", "COM_DL_LOSS_T",
        "COM_LOW_BAT_ACT", "COM_POSCTL_NAVL", "COM_ACT_FAIL_ACT", "COM_QC_ACT",
        "COM_FAIL_ACT_T", "COM_DISARM_LAND", "COM_DISARM_PRFLT",
        # Battery thresholds
        "BAT_LOW_THR", "BAT_CRIT_THR", "BAT_EMERGEN_THR",
        # Rangefinder estimator + geometry
        "EKF2_RNG_CTRL", "EKF2_RNG_AID", "EKF2_HGT_REF", "EKF2_RNG_A_HMAX",
        "EKF2_RNG_A_VMAX", "EKF2_RNG_POS_Z", "EKF2_RNG_PITCH", "EKF2_RNG_DELAY",
        "EKF2_RNG_NOISE", "EKF2_RNG_SFE", "MPC_ALT_MODE",
        # Optical flow estimator + geometry
        "EKF2_OF_CTRL", "EKF2_AID_MASK", "EKF2_OF_DELAY", "EKF2_OF_QMIN",
        "EKF2_OF_QMIN_GND", "EKF2_OF_N_MIN", "EKF2_OF_N_MAX",
        "EKF2_OF_POS_X", "EKF2_OF_POS_Y", "EKF2_OF_POS_Z",
        "SENS_FLOW_ROT", "SENS_FLOW_MINHGT", "SENS_FLOW_MAXHGT", "SENS_FLOW_MAXR",
    ]
    for param, _models in RANGEFINDER_BUS_DRIVERS:
        names.append(param)
    for param, _label in RANGEFINDER_SERIAL_DRIVERS:
        names.append(param)
    for param, _models in FLOW_BUS_DRIVERS:
        names.append(param)
    return names


def _enum_options(options: list[dict[str, Any]], value: float) -> list[dict[str, Any]]:
    """Options list guaranteed to contain *value*.

    A firmware may use an enum member this build has never heard of. Snapping
    such a value to the first option would rewrite a real aircraft's RC-loss
    action the moment the operator touched an unrelated field, so the unknown
    number is appended as its own option instead.
    """
    ivalue = int(round(value))
    if any(int(o["value"]) == ivalue for o in options):
        return options
    return options + [{"value": ivalue, "label": f"Unknown ({ivalue})"}]


def _enum(name: str, label: str, values: dict[str, float],
          options: list[dict[str, Any]], **extra: Any) -> dict[str, Any] | None:
    if name not in values:
        return None
    value = values[name]
    field: dict[str, Any] = {
        "param": name, "label": label, "kind": "enum",
        "value": value, "options": _enum_options(options, value),
    }
    field.update(extra)
    return field


def _number(name: str, label: str, values: dict[str, float],
            **extra: Any) -> dict[str, Any] | None:
    if name not in values:
        return None
    field: dict[str, Any] = {
        "param": name, "label": label, "kind": "number", "value": values[name],
    }
    field.update(extra)
    return field


def _present(fields: list[dict[str, Any] | None]) -> list[dict[str, Any]]:
    return [f for f in fields if f is not None]


def _option_label(options: list[dict[str, Any]], value: float) -> str:
    ivalue = int(round(value))
    for option in options:
        if int(option["value"]) == ivalue:
            return str(option["label"])
    return f"Unknown ({ivalue})"


# ---------------------------------------------------------------------------
# Plain form sections
# ---------------------------------------------------------------------------

def _limits_section(values: dict[str, float]) -> dict[str, Any] | None:
    fields = _present([
        _number("GF_MAX_HOR_DIST", "Maximum distance", values, unit="m", step=1, min=0,
                hint="Horizontal distance from home the vehicle may reach. 0 disables the limit."),
        _number("GF_MAX_VER_DIST", "Maximum height", values, unit="m", step=1, min=0,
                hint="Height above home the vehicle may reach. 0 disables the limit."),
        _number("LNDMC_ALT_MAX", "Multicopter altitude ceiling", values, unit="m", step=1,
                hint="Hard altitude limit enforced by the multicopter land detector."),
        _enum("GF_ACTION", "Action at the limit", values, GEOFENCE_ACTION_OPTIONS,
              hint="What the vehicle does when it reaches the distance or height limit."),
        _enum("GF_SOURCE", "Position source", values, GEOFENCE_SOURCE_OPTIONS),
        _enum("GF_PREDICT", "Predict the breach", values, ON_OFF_OPTIONS,
              hint="Act on where the current velocity is taking the vehicle, not only on "
                   "where it already is."),
    ])
    if not fields:
        return None
    return {
        "id": "limits", "title": "Flight limits", "kind": "fields", "fields": fields,
        "hint": "The envelope the vehicle is not allowed to leave, measured from the home "
                "position.",
    }


def _rtl_section(values: dict[str, float]) -> dict[str, Any] | None:
    fields = _present([
        _enum("RTL_TYPE", "Return type", values, RTL_TYPE_OPTIONS),
        _number("RTL_RETURN_ALT", "Return height", values, unit="m", step=1, min=0,
                hint="Height above home the vehicle climbs to before flying back."),
        _number("RTL_DESCEND_ALT", "Descend height", values, unit="m", step=1, min=0,
                hint="Height above home it descends to once it has arrived."),
        _enum("RTL_CONE_ANG", "Return cone", values, RTL_CONE_OPTIONS,
              hint="Close to home the return height is reduced along this cone, so a "
                   "vehicle hovering overhead does not climb first."),
        _number("RTL_MIN_DIST", "Minimum cone distance", values, unit="m", step=1, min=0),
        _number("RTL_LAND_DELAY", "Loiter before landing", values, unit="s", step=1,
                hint="Seconds to hold at the descend height before landing. -1 loiters "
                     "indefinitely."),
        _number("RTL_LOITER_RAD", "Loiter radius", values, unit="m", step=1,
                hint="Fixed-wing only."),
    ])
    if not fields:
        return None
    return {
        "id": "rtl", "title": "Return to Launch", "kind": "fields", "fields": fields,
        "hint": "The path home, flown on an RTL command and by every failsafe whose action "
                "is Return.",
    }


def _failsafe_section(values: dict[str, float]) -> dict[str, Any] | None:
    fields = _present([
        _enum("NAV_RCL_ACT", "RC loss", values, LINK_LOSS_ACTION_OPTIONS),
        _number("COM_RC_LOSS_T", "RC loss timeout", values, unit="s", step=0.1, min=0),
        _enum("NAV_DLL_ACT", "Data link loss", values, LINK_LOSS_ACTION_OPTIONS),
        _number("COM_DL_LOSS_T", "Data link loss timeout", values, unit="s", step=1, min=0),
        _enum("COM_LOW_BAT_ACT", "Low battery", values, LOW_BATTERY_ACTION_OPTIONS),
        _enum("COM_POSCTL_NAVL", "Position loss", values, POSITION_LOSS_ACTION_OPTIONS,
              hint="Fallback when the position estimate is lost in Position mode."),
        _enum("COM_ACT_FAIL_ACT", "Actuator failure", values, ACTUATOR_FAILURE_ACTION_OPTIONS),
        _enum("COM_QC_ACT", "VTOL quad-chute", values, QUADCHUTE_ACTION_OPTIONS),
        _number("COM_FAIL_ACT_T", "Reaction delay", values, unit="s", step=0.1, min=0,
                hint="Grace period between the failsafe condition and the action, so a "
                     "brief dropout does not trigger a return."),
        _number("COM_DISARM_LAND", "Auto-disarm after landing", values, unit="s", step=0.1,
                hint="Seconds after touchdown before the vehicle disarms itself. "
                     "-1 disables it."),
        _number("COM_DISARM_PRFLT", "Auto-disarm if not taking off", values, unit="s",
                step=1, hint="Seconds armed on the ground before disarming again."),
    ])
    if not fields:
        return None
    return {
        "id": "failsafe", "title": "Failsafe actions", "kind": "fields", "fields": fields,
        "hint": "What the autopilot does on its own when something is lost. Every action "
                "named Return flies the profile above.",
    }


def _battery_section(values: dict[str, float]) -> dict[str, Any] | None:
    fields = _present([
        _number("BAT_LOW_THR", "Low threshold", values, step=0.01, min=0, max=1,
                hint="Remaining capacity, 0–1, at which the low-battery action fires."),
        _number("BAT_CRIT_THR", "Critical threshold", values, step=0.01, min=0, max=1),
        _number("BAT_EMERGEN_THR", "Emergency threshold", values, step=0.01, min=0, max=1),
    ])
    if not fields:
        return None
    return {
        "id": "battery", "title": "Battery thresholds", "kind": "fields", "fields": fields,
        "hint": "The levels the low-battery failsafe above reacts to, as a fraction of a "
                "full pack.",
    }


# ---------------------------------------------------------------------------
# Sensor toggles
# ---------------------------------------------------------------------------

def _bus_drivers(values: dict[str, float],
                 table: list[tuple[str, list[tuple[int, str]]]]) -> list[dict[str, Any]]:
    """Driver entries for every enable parameter the firmware answered for."""
    drivers: list[dict[str, Any]] = []
    for param, models in table:
        if param not in values:
            continue
        for value, label in models:
            drivers.append({
                "id": f"{param}:{value}", "label": label,
                "param": param, "value": float(value), "serial": False,
            })
    return drivers


def _serial_drivers(values: dict[str, float],
                    table: list[tuple[str, str]]) -> list[dict[str, Any]]:
    """Driver entries whose "enable" is the serial port the sensor is wired to."""
    drivers: list[dict[str, Any]] = []
    for param, label in table:
        if param not in values:
            continue
        drivers.append({
            "id": param, "label": label,
            "param": param, "value": None, "serial": True,
        })
    return drivers


def _active_driver(values: dict[str, float],
                   drivers: list[dict[str, Any]]) -> tuple[dict[str, Any] | None, int | None]:
    """The driver currently switched on, and its port when it is a serial one.

    A bus driver matches on its exact model value, because ``SENS_EN_SF1XX`` = 6
    is a different rangefinder from ``SENS_EN_SF1XX`` = 3. A serial driver
    matches on "not 0", and its value *is* the port.
    """
    for driver in drivers:
        param = driver.get("param")
        if not param or param not in values:
            continue
        current = values[param]
        if int(round(current)) == 0:
            continue
        if driver["serial"]:
            return driver, int(round(current))
        if int(round(current)) == int(round(float(driver["value"]))):
            return driver, None
    return None, None


def _driver_params(drivers: list[dict[str, Any]]) -> list[str]:
    """The distinct enable parameters behind a driver list, order preserved."""
    params: list[str] = []
    for driver in drivers:
        param = driver.get("param")
        if param and param not in params:
            params.append(param)
    return params


def _fusion(values: dict[str, float],
            candidates: list[tuple[str, list[dict[str, Any]], float]],
            ) -> tuple[str, list[dict[str, Any]], float, float] | None:
    """The first estimator control the firmware carries: (param, options, on, current)."""
    for param, options, on_value in candidates:
        if param in values:
            return param, options, on_value, values[param]
    return None


def _sensor_toggle(values: dict[str, float], *, label: str,
                   drivers: list[dict[str, Any]],
                   fusion: tuple[str, list[dict[str, Any]], float, float] | None,
                   extra_on: list[dict[str, Any]] | None = None,
                   extra_off: list[dict[str, Any]] | None = None,
                   extra_enabled: bool = False,
                   extra_detail: str = "",
                   ) -> dict[str, Any] | None:
    """Assemble the toggle description for one sensor.

    ``enable``/``disable`` are the writes that are *not* driver-specific — the
    estimator half of the chain. ``clear`` names the driver parameters that are
    currently non-zero, so switching sensor or switching off writes a zero only
    where one is actually needed instead of walking every driver on the board.

    ``extra_*`` is the escape hatch for an estimator that is not a parameter of
    its own — the pre-v1.14 optical-flow bit inside ``EKF2_AID_MASK``. It carries
    its own writes, its own current state and its own state text, because a
    toggle that reported only the driver would call a sensor "on" while the
    estimator was still ignoring it.
    """
    if not drivers and fusion is None and not extra_on:
        return None

    active, port = _active_driver(values, drivers)
    fusion_on = (fusion is not None and int(round(fusion[3])) != 0) or extra_enabled

    enable: list[dict[str, Any]] = []
    disable: list[dict[str, Any]] = []
    if fusion is not None:
        param, _options, on_value, _current = fusion
        enable.append({"param": param, "value": on_value})
        disable.append({"param": param, "value": 0.0})
    enable += list(extra_on or [])
    disable += list(extra_off or [])

    clear = [p for p in _driver_params(drivers)
             if p in values and int(round(values[p])) != 0]

    # An external sensor arrives over MAVLink and has no local driver, so it is
    # offered whenever the estimator half exists at all — otherwise a vehicle
    # streaming DISTANCE_SENSOR could never be fused from this page.
    offered = list(drivers)
    if fusion is not None or extra_on:
        offered.append({
            "id": EXTERNAL_DRIVER_ID, "label": "External / MAVLink",
            "param": None, "value": None, "serial": False,
        })

    if active is not None:
        selected = str(active["id"])
    elif fusion_on:
        selected = EXTERNAL_DRIVER_ID
    else:
        selected = str(offered[0]["id"]) if offered else ""

    if active is not None:
        detail = str(active["label"])
    elif fusion_on:
        detail = "No local driver — expecting the sensor over MAVLink"
    else:
        detail = "No driver enabled"
    if fusion is not None:
        _param, options, _on, current = fusion
        detail += f" · fusion: {_option_label(options, current)}"
    elif extra_detail:
        detail += f" · {extra_detail}"

    toggle: dict[str, Any] = {
        "label": label,
        "enabled": active is not None or fusion_on,
        "detail": detail,
        "drivers": offered,
        "selected": selected,
        "enable": enable,
        "disable": disable,
        "clear": clear,
        # Starting or stopping a sensor driver happens at boot, so the write
        # lands but the sensor does not appear until the autopilot restarts.
        "reboot": bool(clear or any(d.get("param") for d in offered)),
    }
    if any(d["serial"] for d in offered):
        toggle["ports"] = SERIAL_PORT_OPTIONS
        toggle["port"] = port
    return toggle


def _rangefinder_section(values: dict[str, float]) -> dict[str, Any] | None:
    drivers = (_bus_drivers(values, RANGEFINDER_BUS_DRIVERS)
               + _serial_drivers(values, RANGEFINDER_SERIAL_DRIVERS))
    toggle = _sensor_toggle(
        values, label="Distance sensor",
        drivers=drivers,
        fusion=_fusion(values, RANGE_FUSION_CANDIDATES),
    )
    if toggle is None:
        return None
    fields = _present([
        _enum("EKF2_RNG_CTRL", "Fusion mode", values, RANGE_CTRL_OPTIONS,
              hint="Conditional uses the rangefinder for height only while the vehicle is "
                   "low and slow; Always trusts it throughout the flight."),
        _enum("EKF2_RNG_AID", "Range aid", values, ON_OFF_OPTIONS),
        _enum("EKF2_HGT_REF", "Primary height source", values, HEIGHT_REF_OPTIONS,
              hint="Leave on Barometer or GPS for outdoor flight; Range sensor is for "
                   "indoor and low-altitude work over a flat floor."),
        _enum("MPC_ALT_MODE", "Altitude control mode", values, ALT_MODE_OPTIONS,
              hint="Terrain following holds a height above the ground the sensor sees, "
                   "not above home."),
        _number("EKF2_RNG_A_HMAX", "Range aid maximum height", values, unit="m", step=0.1),
        _number("EKF2_RNG_A_VMAX", "Range aid maximum speed", values, unit="m/s", step=0.1),
        _number("EKF2_RNG_POS_Z", "Mounting offset, Z", values, unit="m", step=0.01,
                hint="Sensor position below the centre of gravity, positive down."),
        _number("EKF2_RNG_PITCH", "Mounting pitch offset", values, unit="rad", step=0.01),
        _number("EKF2_RNG_DELAY", "Measurement delay", values, unit="ms", step=1),
        _number("EKF2_RNG_NOISE", "Measurement noise", values, unit="m", step=0.01),
        _number("EKF2_RNG_SFE", "Range-dependent noise", values, unit="m/m", step=0.01),
    ])
    return {
        "id": "rangefinder", "title": "Distance sensor", "kind": "toggle",
        "toggle": toggle, "fields": fields,
        "hint": "A downward lidar or sonar. Switching it on starts the driver and tells the "
                "estimator to fuse it — doing only one of the two is the usual reason a "
                "rangefinder reads perfectly and changes nothing.",
    }


def _flow_section(values: dict[str, float]) -> dict[str, Any] | None:
    fusion = _fusion(values, FLOW_FUSION_CANDIDATES)
    extra_on: list[dict[str, Any]] = []
    extra_off: list[dict[str, Any]] = []
    # Pre-v1.14 firmware has no EKF2_OF_CTRL: flow fusion is bit 1 of the aiding
    # mask. The exact target values are computed here, from the mask the vehicle
    # currently holds, so the write never clears an unrelated aiding source.
    if fusion is None and "EKF2_AID_MASK" in values:
        mask = int(round(values["EKF2_AID_MASK"]))
        extra_on.append({"param": "EKF2_AID_MASK", "value": float(mask | AID_MASK_FLOW_BIT)})
        extra_off.append({"param": "EKF2_AID_MASK", "value": float(mask & ~AID_MASK_FLOW_BIT)})

    mask_on = bool(extra_on and int(round(values["EKF2_AID_MASK"])) & AID_MASK_FLOW_BIT)

    drivers = _bus_drivers(values, FLOW_BUS_DRIVERS)
    toggle = _sensor_toggle(
        values, label="Optical flow", drivers=drivers, fusion=fusion,
        extra_on=extra_on, extra_off=extra_off,
        extra_enabled=mask_on,
        extra_detail=("fusion: " + ("On" if mask_on else "Off")
                      + " (EKF2_AID_MASK bit 1)") if extra_on else "",
    )
    if toggle is None:
        return None

    fields = _present([
        _enum("SENS_FLOW_ROT", "Sensor rotation", values, FLOW_ROTATION_OPTIONS,
              hint="Yaw of the sensor relative to the airframe's forward axis."),
        _number("SENS_FLOW_MINHGT", "Minimum height", values, unit="m", step=0.01),
        _number("SENS_FLOW_MAXHGT", "Maximum height", values, unit="m", step=0.1),
        _number("SENS_FLOW_MAXR", "Maximum angular rate", values, unit="rad/s", step=0.1),
        _number("EKF2_OF_QMIN", "Minimum quality, in air", values, step=1),
        _number("EKF2_OF_QMIN_GND", "Minimum quality, on ground", values, step=1),
        _number("EKF2_OF_DELAY", "Measurement delay", values, unit="ms", step=1),
        _number("EKF2_OF_N_MIN", "Measurement noise, best case", values, unit="rad/s",
                step=0.01),
        _number("EKF2_OF_N_MAX", "Measurement noise, worst case", values, unit="rad/s",
                step=0.01),
        _number("EKF2_OF_POS_X", "Mounting offset, X", values, unit="m", step=0.01),
        _number("EKF2_OF_POS_Y", "Mounting offset, Y", values, unit="m", step=0.01),
        _number("EKF2_OF_POS_Z", "Mounting offset, Z", values, unit="m", step=0.01),
    ])
    return {
        "id": "flow", "title": "Optical flow", "kind": "toggle",
        "toggle": toggle, "fields": fields,
        "hint": "Horizontal velocity from a downward camera, for position hold without GPS. "
                "It needs a distance sensor: without a height above ground the flow rate "
                "cannot be turned into a speed.",
    }


def build(values: dict[str, float]) -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the Safety & Sensors page description.

    *values* holds only the parameters the vehicle actually answered for, so
    every section shrinks or disappears on a firmware that lacks them. The
    return shape is what ``GET /api/safety`` serialises:

    ``sections`` — ordered list of ``{id, title, hint, kind, fields[, toggle]}``.
    ``kind`` is ``"fields"`` (a plain form) or ``"toggle"`` (a sensor with an
    on/off chain plus its settings). Each field carries the PX4 parameter name
    it writes, so the frontend applies edits through the existing
    ``POST /api/params/set``; each toggle carries the ordered write lists that
    bring the sensor up or take it down.
    """
    sections = [s for s in (
        _limits_section(values),
        _rtl_section(values),
        _failsafe_section(values),
        _battery_section(values),
        _rangefinder_section(values),
        _flow_section(values),
    ) if s is not None]
    return {"sections": sections, "received": len(values)}
