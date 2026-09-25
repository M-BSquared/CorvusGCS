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

The sensors are where ArduPilot differs most from PX4, and the section below
the failsafes explains how.
"""
from __future__ import annotations

import re
from typing import Any

from .param_fields import (
    bitmask,
    enum,
    flag_flow_without_range,
    number,
    option_label,
    present,
    section,
)

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
    {"value": 0, "label": "0.0 (off)"},
    {"value": 1, "label": "0.6 (strict)"},
    {"value": 2, "label": "0.8 (default)"},
    {"value": 3, "label": "1.0 (relaxed)"},
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

# EK3_SRCn_* — which sensor each EKF3 source set reads a state from. Values are
# AP_NavEKF_Source's own.
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

EK3_POSXY_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "None"},
    {"value": 3, "label": "GPS"},
    {"value": 4, "label": "Beacon"},
    {"value": 6, "label": "External navigation"},
]

RANGEFINDER_ORIENT_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Forward"},
    {"value": 4, "label": "Back"},
    {"value": 24, "label": "Up"},
    {"value": 25, "label": "Down"},
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
# Sensors
# ---------------------------------------------------------------------------
#
# Every number here is ArduPilot's own: the RNGFND1_TYPE and FLOW_TYPE values
# from AP_RangeFinder.h and AP_OpticalFlow.cpp, and the port, baud, CAN and
# address settings from the ArduPilot wiki page for each sensor.
#
# A driver on ArduPilot is more than its TYPE value, and that is where a
# rangefinder that reads nothing usually comes from:
#
# * a serial sensor needs a SERIALn port set to the right protocol at the right
#   baud rate, and a serial rangefinder takes the *first* port set to
#   Rangefinder, so a stale second one steals it;
# * a DroneCAN sensor needs a CAN port running DroneCAN;
# * some I2C drivers need the sensor's bus address, and do nothing with 0.
#
# So every driver entry carries those writes and the switch sets all of it,
# and the state the page reports is read back from all of it.
#
# The estimator half is not the PX4 one. ArduPilot uses a downward rangefinder
# for surface tracking and landing on its own, and its wiki is explicit that
# EK3_SRC1_POSZ is *not* to be set to Rangefinder for that; so the rangefinder
# switch leaves the height source alone. Optical flow does need the EKF: it is
# used only by a source set whose horizontal velocity is Optical flow, which
# the flow switch sets up the way the wiki describes.

BUS_ANALOG = "analog"
BUS_PWM = "pwm"
BUS_I2C = "i2c"
BUS_SERIAL = "serial"
BUS_SERIAL_FLOW = "serial_flow"
BUS_MSP = "msp"
BUS_CAN = "can"
BUS_MAVLINK = "mavlink"
BUS_BOARD = "board"

# SERIALn_PROTOCOL values, AP_SerialManager's own.
SERIAL_PROTOCOL_NONE = -1
SERIAL_PROTOCOL_RANGEFINDER = 9
SERIAL_PROTOCOL_OPTICAL_FLOW = 18
SERIAL_PROTOCOL_MSP = 32

SERIAL_PROTOCOL_BY_BUS: dict[str, int] = {
    BUS_SERIAL: SERIAL_PROTOCOL_RANGEFINDER,
    BUS_SERIAL_FLOW: SERIAL_PROTOCOL_OPTICAL_FLOW,
    BUS_MSP: SERIAL_PROTOCOL_MSP,
}

# Protocols whose driver takes the first port set to them. A second port left
# on the same protocol is harmless only while it comes after the chosen one,
# so a switch to a new port clears the others. MSP is not one of these: every
# MSP port is read, and one may be driving an OSD.
EXCLUSIVE_PROTOCOLS = frozenset({SERIAL_PROTOCOL_RANGEFINDER, SERIAL_PROTOCOL_OPTICAL_FLOW})

SERIAL_PORTS = tuple(range(1, 9))

SERIAL_PROTOCOL_NAMES: dict[int, str] = {
    -1: "not used", 1: "MAVLink1", 2: "MAVLink2", 3: "FrSky D", 4: "FrSky SPort",
    5: "GPS", 7: "Alexmos gimbal", 8: "Gimbal", 9: "Rangefinder",
    10: "FrSky passthrough", 11: "Lidar360", 13: "Beacon", 14: "Volz servos",
    15: "SBus out", 16: "ESC telemetry", 17: "Devo telemetry", 18: "Optical flow",
    19: "Robotis servos", 20: "NMEA out", 21: "Wind vane", 22: "SLCAN",
    23: "RC input", 24: "EFI", 25: "LTM", 26: "RunCam", 27: "HoTT telemetry",
    28: "Scripting", 29: "Crossfire VTX", 30: "Generator", 31: "Winch", 32: "MSP",
    33: "DJI FPV", 34: "Airspeed", 35: "ADS-B", 36: "AHRS", 37: "SmartAudio",
    38: "FETtec OneWire", 39: "Torqeedo", 40: "AIS", 41: "CoDev ESC",
    42: "DisplayPort", 43: "MAVLink high latency", 44: "IRC Tramp", 45: "DDS XRCE",
    46: "IMU data", 48: "PPP", 49: "i-BUS telemetry", 50: "IOMCU",
}

# SERIALn_BAUD takes a short code; a larger number is taken as the rate itself.
BAUD_CODES: dict[int, int] = {
    1: 1200, 2: 2400, 4: 4800, 9: 9600, 19: 19200, 38: 38400, 57: 57600,
    111: 111100, 115: 115200, 230: 230400, 256: 256000, 460: 460800,
    500: 500000, 921: 921600, 1500: 1500000, 2000: 2000000,
}

# RNGFND1_TYPE. ArduPilot carries close to fifty drivers; these are the ones
# whose whole setup is documented, so choosing one here sets everything it
# needs. Any other value is shown as it is and never rewritten.
# ``addr`` is the RNGFND1_ADDR the wiki gives; 0 lets the driver use its own.
RANGEFINDER_TYPES: list[dict[str, Any]] = [
    {"value": 1, "label": "Analog voltage", "bus": BUS_ANALOG},
    {"value": 2, "label": "MaxBotix sonar (I2C)", "bus": BUS_I2C, "addr": 0},
    {"value": 3, "label": "Garmin Lidar-Lite (I2C)", "bus": BUS_I2C},
    {"value": 5, "label": "PWM input", "bus": BUS_PWM},
    {"value": 7, "label": "LightWare SF10, SF11, SF20, LW20 (I2C)", "bus": BUS_I2C,
     "addr": 102},
    {"value": 8, "label": "LightWare SF10, SF11, SF20, LW20 (serial)", "bus": BUS_SERIAL,
     "baud": 115},
    {"value": 10, "label": "MAVLink (companion computer)", "bus": BUS_MAVLINK},
    {"value": 12, "label": "LeddarOne (serial)", "bus": BUS_SERIAL, "baud": 115},
    {"value": 14, "label": "TeraRanger Evo (I2C)", "bus": BUS_I2C, "addr": 49},
    {"value": 15, "label": "Garmin Lidar-Lite v3 (I2C)", "bus": BUS_I2C},
    {"value": 16, "label": "ST VL53L0X, VL53L1X (I2C)", "bus": BUS_I2C, "addr": 41},
    {"value": 19, "label": "Benewake TF02 (serial)", "bus": BUS_SERIAL, "baud": 115},
    {"value": 20, "label": "Benewake TFmini, TFmini-S, TFmini Plus (serial)",
     "bus": BUS_SERIAL, "baud": 115},
    {"value": 21, "label": "Garmin Lidar-Lite v3HP (I2C)", "bus": BUS_I2C},
    {"value": 23, "label": "Blue Robotics Ping sonar (serial)", "bus": BUS_SERIAL,
     "baud": 115},
    {"value": 24, "label": "DroneCAN", "bus": BUS_CAN},
    {"value": 25, "label": "Benewake TFmini Plus, TF02-Pro, TF-Luna (I2C)", "bus": BUS_I2C,
     "addr": 16},
    {"value": 27, "label": "Benewake TF02-Pro, TF03, TF-Luna (serial)", "bus": BUS_SERIAL,
     "baud": 115},
    {"value": 32, "label": "MSP (Matek 3901-L0X)", "bus": BUS_MSP, "baud": 115},
    {"value": 37, "label": "Nooploop TOFSense (serial)", "bus": BUS_SERIAL, "baud": 230},
]

FLOW_TYPES: list[dict[str, Any]] = [
    {"value": 1, "label": "PX4Flow (I2C)", "bus": BUS_I2C},
    {"value": 2, "label": "PixArt on the flight controller", "bus": BUS_BOARD},
    {"value": 4, "label": "Cheerson CX-OF (serial)", "bus": BUS_SERIAL_FLOW},
    {"value": 5, "label": "MAVLink (companion computer)", "bus": BUS_MAVLINK},
    {"value": 6, "label": "DroneCAN (HereFlow, H-Flow, ARK Flow)", "bus": BUS_CAN},
    {"value": 7, "label": "MSP (Matek 3901-L0X)", "bus": BUS_MSP, "baud": 115},
    {"value": 8, "label": "UPixels UPFlow (serial)", "bus": BUS_SERIAL_FLOW},
]

RANGEFINDER_TYPE_OPTIONS: list[dict[str, Any]] = [{"value": 0, "label": "None"}] + [
    {"value": t["value"], "label": t["label"]} for t in RANGEFINDER_TYPES
]
FLOW_TYPE_OPTIONS: list[dict[str, Any]] = [{"value": 0, "label": "None"}] + [
    {"value": t["value"], "label": t["label"]} for t in FLOW_TYPES
]

# CAN_Dn_PROTOCOL = 1 is DroneCAN.
CAN_PROTOCOL_DRONECAN = 1

# One EKF3 source set for flight on optical flow, as the ArduPilot wiki gives
# it: no position, flow for velocity, the barometer for height, no vertical
# velocity, the compass for heading.
FLOW_SOURCE_SET: list[tuple[str, int, str]] = [
    ("POSXY", 0, "No position source: flow gives velocity, not position"),
    ("VELXY", 5, "Optical flow is the horizontal velocity source"),
    ("POSZ", 1, "Height from the barometer"),
    ("VELZ", 0, "No vertical velocity source"),
    ("YAW", 1, "Heading from the compass"),
]
EK3_VELXY_FLOW = 5
# EK3_SRC_OPTIONS bit 0 fuses the velocities of every source set at once. The
# wiki's flow setup has it clear.
EK3_FUSE_ALL_VELOCITIES = 1

# 4.6 moved the rangefinder distances to metres. A preset names the metre
# parameter; on an older firmware the centimetre one is written instead.
LENGTH_ALIASES: dict[str, tuple[str, float]] = {
    "RNGFND1_MIN": ("RNGFND1_MIN_CM", 100.0),
    "RNGFND1_MAX": ("RNGFND1_MAX_CM", 100.0),
    "RNGFND1_GNDCLR": ("RNGFND1_GNDCLEAR", 100.0),
}

# Where a sensor is plugged in, by how its driver reaches it. ArduPilot names
# serial ports SERIALn, and which connector that is depends on the board, so
# the text gives the Pixhawk-standard mapping and says to check the board.
SERIAL_WIRING = ("Plug it into the serial port you pick below. ArduPilot numbers "
                 "them SERIAL1, SERIAL2 and so on; on Pixhawk-standard boards SERIAL1 is "
                 "TELEM1, SERIAL2 is TELEM2, SERIAL3 is GPS1 and SERIAL4 is GPS2, but "
                 "check your board's page on ardupilot.org. The sensor's TX goes to the "
                 "port's RX and its RX to the port's TX.")

WIRING: dict[str, str] = {
    BUS_SERIAL: SERIAL_WIRING,
    BUS_SERIAL_FLOW: SERIAL_WIRING,
    BUS_MSP: SERIAL_WIRING,
    BUS_I2C: "Plug it into an I2C connector. On most Pixhawk boards the GPS1 "
             "connector carries I2C as well, and an I2C splitter lets several devices "
             "share one port.",
    BUS_ANALOG: "Connect its output to an analog input pin and set RNGFND1_PIN to that "
                "pin's number from your board's page on ardupilot.org.",
    BUS_PWM: "Connect its PWM output to a pin that can read PWM and set RNGFND1_PIN to "
             "that pin's number from your board's page on ardupilot.org.",
    BUS_MAVLINK: "Nothing plugs into the flight controller for the sensor itself: it "
                 "arrives over MAVLink, usually from a companion computer on a "
                 "telemetry port.",
    BUS_BOARD: "It is built into the flight controller; there is nothing to plug in.",
}


def _dronecan_ports(values: dict[str, float]) -> list[int]:
    """The CAN ports whose driver runs DroneCAN."""
    ports: list[int] = []
    for k in (1, 2):
        driver = values.get(f"CAN_P{k}_DRIVER")
        if driver is None or int(round(driver)) <= 0:
            continue
        protocol = values.get(f"CAN_D{int(round(driver))}_PROTOCOL")
        if protocol is not None and int(round(protocol)) == CAN_PROTOCOL_DRONECAN:
            ports.append(k)
    return ports


def _can_wiring(values: dict[str, float]) -> str:
    """Which CAN connector the sensor belongs on: the one DroneCAN runs on.

    CAN1 when none does yet, because that is the port the switch starts.
    """
    where = " or ".join(f"CAN{k}" for k in _dronecan_ports(values)) or "CAN1"
    return (f"Plug it into the {where} connector, the one that runs DroneCAN. Further "
            "CAN devices can be chained behind it, and the last one on the bus needs "
            "its termination on.")


def _wiring(values: dict[str, float], bus: str) -> str:
    return _can_wiring(values) if bus == BUS_CAN else WIRING[bus]


TYPE_PARAM = {"range": "RNGFND1_TYPE", "flow": "FLOW_TYPE"}
TYPES_BY_KIND = {"range": RANGEFINDER_TYPES, "flow": FLOW_TYPES}


def _spec(kind: str, value: float | None) -> dict[str, Any] | None:
    if value is None:
        return None
    ivalue = int(round(value))
    return next((t for t in TYPES_BY_KIND[kind] if t["value"] == ivalue), None)


def _baud_rate(code: float) -> int:
    """SERIALn_BAUD as bits per second."""
    ivalue = int(round(code))
    return BAUD_CODES.get(ivalue, ivalue)


def _serial_ports(values: dict[str, float]) -> list[dict[str, Any]]:
    """Every SERIALn port the vehicle answered for, labelled with what it does now."""
    ports: list[dict[str, Any]] = []
    for n in SERIAL_PORTS:
        name = f"SERIAL{n}_PROTOCOL"
        if name not in values:
            continue
        protocol = int(round(values[name]))
        use = SERIAL_PROTOCOL_NAMES.get(protocol, f"protocol {protocol}")
        ports.append({"value": n, "label": f"SERIAL{n} ({use})", "protocol": protocol})
    return ports


def _ports_with(ports: list[dict[str, Any]], protocol: int) -> list[int]:
    return [p["value"] for p in ports if p["protocol"] == protocol]


def _gps_configured(values: dict[str, float]) -> bool | None:
    """Whether a GPS is set up: GPS1_TYPE from 4.6, GPS_TYPE before. None if neither."""
    for name in ("GPS1_TYPE", "GPS_TYPE"):
        if name in values:
            return int(round(values[name])) != 0
    return None


def _dronecan(values: dict[str, float]) -> dict[str, Any]:
    """Is DroneCAN running, and if not, which writes start it.

    ``running`` when some CAN port points at a driver whose protocol is
    DroneCAN. Otherwise the first CAN port is started, but only when it is idle
    and its driver is not serving the second port with something else;
    anything else is ``blocked`` with the reason, because rewriting a CAN port
    that already runs another protocol could stop whatever is on it.
    """
    if _dronecan_ports(values):
        return {"running": True, "writes": [], "blocked": ""}
    if "CAN_P1_DRIVER" not in values:
        return {"running": False, "writes": [],
                "blocked": "This flight controller reports no CAN port (CAN_P1_DRIVER)."}
    if int(round(values["CAN_P1_DRIVER"])) != 0:
        return {"running": False, "writes": [],
                "blocked": "CAN port 1 already runs another protocol. Set the CAN ports "
                           "on the Parameters page."}
    protocol = values.get("CAN_D1_PROTOCOL")
    if (protocol is not None and int(round(protocol)) != CAN_PROTOCOL_DRONECAN
            and int(round(values.get("CAN_P2_DRIVER", 0))) == 1):
        return {"running": False, "writes": [],
                "blocked": "CAN driver 1 runs another protocol for CAN port 2. Set the "
                           "CAN ports on the Parameters page."}
    writes = [{"param": "CAN_P1_DRIVER", "value": 1.0}]
    if protocol is not None and int(round(protocol)) != CAN_PROTOCOL_DRONECAN:
        writes.append({"param": "CAN_D1_PROTOCOL", "value": float(CAN_PROTOCOL_DRONECAN)})
    return {"running": False, "writes": writes, "blocked": ""}


CAN_WRITE_LABELS = {
    "CAN_P1_DRIVER": "Start CAN port 1 on driver 1",
    "CAN_D1_PROTOCOL": "Driver 1 speaks DroneCAN",
}


def _serial_rangefinder_conflict(values: dict[str, float]) -> str:
    """Why RNGFND1's serial port cannot be set from here, or ""."""
    spec = _spec("range", values.get("RNGFND2_TYPE"))
    if spec is not None and spec["bus"] == BUS_SERIAL:
        return ("RNGFND2 is a serial rangefinder too, and ArduPilot hands serial ports to "
                "rangefinders in order. Set the ports on the Parameters page.")
    return ""


def _driver_entries(values: dict[str, float], kind: str,
                    ports: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """The picker entries for one sensor, each carrying everything it needs to start.

    A serial entry carries ``port_writes`` (``{port}`` is the SERIAL number the
    operator picks) and ``port_clear`` (the other ports on the same exclusive
    protocol, which would otherwise take the sensor's place). A DroneCAN entry
    carries the CAN writes as ``extra``, or ``blocked`` when the CAN ports
    cannot be changed safely. An I2C entry whose driver needs an address
    carries it too, unless the sensor is already that type and has one.
    """
    type_param = TYPE_PARAM[kind]
    if type_param not in values:
        return []
    current = int(round(values[type_param]))
    addr_param = "RNGFND1_ADDR" if kind == "range" else None
    serial_blocked = _serial_rangefinder_conflict(values) if kind == "range" else ""
    can: dict[str, Any] | None = None

    entries: list[dict[str, Any]] = []
    for spec in TYPES_BY_KIND[kind]:
        value = spec["value"]
        entry: dict[str, Any] = {
            "id": f"{type_param}:{value}", "label": spec["label"],
            "param": type_param, "value": value, "serial": False,
            "wiring": _wiring(values, spec["bus"]),
        }
        extra: list[dict[str, Any]] = []
        protocol = SERIAL_PROTOCOL_BY_BUS.get(spec["bus"])
        if protocol is not None:
            entry["serial"] = True
            entry["port_writes"] = [{"param": "SERIAL{port}_PROTOCOL", "value": float(protocol)}]
            if spec.get("baud"):
                entry["port_writes"].append(
                    {"param": "SERIAL{port}_BAUD", "value": float(spec["baud"])})
            entry["port_clear"] = [
                {"port": n, "param": f"SERIAL{n}_PROTOCOL", "value": float(SERIAL_PROTOCOL_NONE)}
                for n in _ports_with(ports, protocol)
            ] if protocol in EXCLUSIVE_PROTOCOLS else []
            if not ports:
                entry["blocked"] = ("This firmware reports no SERIALn_PROTOCOL parameters, "
                                    "so the port cannot be set from here.")
            elif protocol == SERIAL_PROTOCOL_RANGEFINDER and serial_blocked:
                entry["blocked"] = serial_blocked
        if spec["bus"] == BUS_CAN:
            can = can or _dronecan(values)
            if can["blocked"]:
                entry["blocked"] = can["blocked"]
            extra += can["writes"]
        if addr_param and "addr" in spec and addr_param in values:
            have = int(round(values[addr_param]))
            if current != value or (spec["addr"] and have == 0):
                extra.append({"param": addr_param, "value": float(spec["addr"])})
        if extra:
            entry["extra"] = extra
        entries.append(entry)

    if current != 0 and _spec(kind, current) is None:
        entries.append({
            "id": f"{type_param}:{current}",
            "label": f"{type_param} = {current}, not in this list",
            "param": type_param, "value": current, "serial": False,
        })
    return entries


def _driver_problems(values: dict[str, float], kind: str, spec: dict[str, Any],
                     ports: list[dict[str, Any]]) -> list[str]:
    """What the running driver still lacks, read back from the vehicle."""
    problems: list[str] = []
    protocol = SERIAL_PROTOCOL_BY_BUS.get(spec["bus"])
    if protocol is not None:
        on = _ports_with(ports, protocol)
        if not on:
            name = SERIAL_PROTOCOL_NAMES[protocol]
            problems.append(f"No serial port is set to {name} (SERIALn_PROTOCOL = "
                            f"{protocol}), so the driver has nothing to read.")
        elif spec.get("baud"):
            # An exclusive protocol is read on its first port only; MSP on any.
            read = on[:1] if protocol in EXCLUSIVE_PROTOCOLS else on
            want = _baud_rate(spec["baud"])
            rates = {n: _baud_rate(values[f"SERIAL{n}_BAUD"])
                     for n in read if f"SERIAL{n}_BAUD" in values}
            if rates and all(rate != want for rate in rates.values()):
                n, rate = next(iter(rates.items()))
                problems.append(f"SERIAL{n} runs at {rate} baud, and this sensor talks "
                                f"at {want}.")
    if spec["bus"] == BUS_CAN and not _dronecan(values)["running"]:
        problems.append("No CAN port runs DroneCAN, so nothing reaches the driver.")
    if kind == "range":
        if spec.get("addr") and int(round(values.get("RNGFND1_ADDR", spec["addr"]))) == 0:
            problems.append(f"RNGFND1_ADDR is 0, and this driver needs the sensor's I2C "
                            f"address ({spec['addr']}).")
        if spec["bus"] in (BUS_ANALOG, BUS_PWM) and int(round(values.get("RNGFND1_PIN", 0))) < 0:
            problems.append("RNGFND1_PIN is not set, so the driver has no input pin to read.")
    return problems


def _default_port(ports: list[dict[str, Any]], spec: dict[str, Any] | None) -> int | None:
    """The port the picker opens on: the running sensor's, else the first free one."""
    protocol = SERIAL_PROTOCOL_BY_BUS.get(spec["bus"]) if spec else None
    if protocol is not None:
        on = _ports_with(ports, protocol)
        if on:
            return on[0]
    free = _ports_with(ports, SERIAL_PROTOCOL_NONE)
    return free[0] if free else None


def _flow_estimator(values: dict[str, float]) -> dict[str, Any]:
    """Which EKF3 source set uses optical flow, and the writes that give it one.

    With no GPS configured the primary set becomes the flow set, which is the
    wiki's setup for flight without GPS. With a GPS, set 1 stays on it and flow
    goes into the first free one of sets 2 and 3, to be selected in flight with
    an RC switch, which is the wiki's setup for the two together. A set counts
    as free while it has no horizontal position, velocity or vertical velocity
    source; one that has is somebody's configuration and is not overwritten.
    """
    using = [k for k in (1, 2, 3)
             if int(round(values.get(f"EK3_SRC{k}_VELXY", -1))) == EK3_VELXY_FLOW]
    target: int | None = using[0] if using else None
    writes: list[dict[str, Any]] = []
    blocked = ""
    if target is None:
        if "EK3_SRC1_VELXY" not in values:
            blocked = ("This firmware has no EK3_SRC source sets, so the estimator half "
                       "cannot be set from here.")
        elif _gps_configured(values) is False:
            target = 1
        else:
            target = next((k for k in (2, 3) if all(
                f"EK3_SRC{k}_{s}" in values and int(round(values[f"EK3_SRC{k}_{s}"])) == 0
                for s in ("POSXY", "VELXY", "VELZ"))), None)
            if target is None:
                blocked = ("EKF source sets 2 and 3 are both in use. Point one of them at "
                           "optical flow (EK3_SRCn_VELXY = 5) on the Parameters page.")
        if target is not None:
            for suffix, value, label in FLOW_SOURCE_SET:
                name = f"EK3_SRC{target}_{suffix}"
                if name in values and int(round(values[name])) != value:
                    writes.append({"param": name, "value": float(value), "label": label})
    options = values.get("EK3_SRC_OPTIONS")
    if options is not None and int(round(options)) & EK3_FUSE_ALL_VELOCITIES:
        writes.append({
            "param": "EK3_SRC_OPTIONS",
            "value": float(int(round(options)) & ~EK3_FUSE_ALL_VELOCITIES),
            "label": "Fuse only the selected set's velocities, as the wiki sets it for flow",
        })

    if using:
        note = f"Flow is the horizontal velocity source of EKF source set {using[0]}."
    elif target == 1:
        note = ("No GPS is configured, so On makes flow the primary source: no position, "
                "flow for velocity, the barometer for height and the compass for heading, "
                "as the ArduPilot wiki sets it for flight without GPS.")
    elif target is not None:
        position = "middle" if target == 2 else "high"
        note = (f"GPS stays EKF source set 1, and On sets flow up as source set {target}. "
                "Switch to it in flight with an RC switch set to EKF source "
                f"(RCx_OPTION = 90), {position} position.")
    else:
        note = blocked
    return {"using": using, "target": target, "writes": writes,
            "blocked": blocked, "note": note}


def _flow_estimator_off(values: dict[str, float], using: list[int]) -> list[dict[str, Any]]:
    """Put back every source set that reads optical flow.

    Set 1 returns to its GPS defaults where flow had emptied it. Sets 2 and 3
    lose their horizontal velocity source, which is what they had before.
    """
    writes: list[dict[str, Any]] = []
    for k in using:
        if k == 1:
            writes.append({"param": "EK3_SRC1_VELXY", "value": 3.0})
            for suffix in ("POSXY", "VELZ"):
                name = f"EK3_SRC1_{suffix}"
                if name in values and int(round(values[name])) == 0:
                    writes.append({"param": name, "value": 3.0})
        else:
            writes.append({"param": f"EK3_SRC{k}_VELXY", "value": 0.0})
    return writes


def _toggle(values: dict[str, float], kind: str, *, label: str,
            ports: list[dict[str, Any]], enable: list[dict[str, Any]],
            disable: list[dict[str, Any]], problems: list[str],
            detail_suffix: str) -> dict[str, Any] | None:
    """The shared toggle shape: the picker, the state, and the write lists."""
    type_param = TYPE_PARAM[kind]
    if type_param not in values:
        return None
    current = int(round(values[type_param]))
    spec = _spec(kind, current)
    drivers = _driver_entries(values, kind, ports)

    if current == 0:
        detail = "No driver enabled"
        state = "off"
        problems = []
    elif spec is None:
        detail = f"{type_param} = {current}, not in this list"
        state = "on"
    else:
        detail = str(spec["label"])
        state = "partial" if problems else "on"
    detail += detail_suffix

    toggle: dict[str, Any] = {
        "label": label,
        "enabled": state == "on",
        "state": state,
        "problems": problems,
        "detail": detail,
        "drivers": drivers,
        "selected": f"{type_param}:{current}" if current != 0 else (
            drivers[0]["id"] if drivers else ""),
        "enable": enable,
        "disable": disable,
        "clear": [type_param] if current != 0 else [],
        # Every sensor driver starts at boot.
        "reboot": True,
    }
    if any(d["serial"] for d in drivers):
        toggle["ports"] = [{"value": p["value"], "label": p["label"]} for p in ports]
        toggle["port"] = _default_port(ports, spec)
    return toggle


def _rangefinder_section(values: dict[str, float], ports: list[dict[str, Any]],
                         ) -> dict[str, Any] | None:
    spec = _spec("range", values.get("RNGFND1_TYPE"))
    problems = _driver_problems(values, "range", spec, ports) if spec else []

    enable: list[dict[str, Any]] = []
    if "RNGFND_LANDING" in values:
        enable.append({"param": "RNGFND_LANDING", "value": 1.0})
    # Nothing turns the rangefinder into the height source, but a set that was
    # pointed at it (by hand, or by an older Corvus) must not be left pointing
    # at a sensor that is gone: that is an estimator with no height at all.
    disable = [{"param": f"EK3_SRC{k}_POSZ", "value": 1.0} for k in (1, 2, 3)
               if int(round(values.get(f"EK3_SRC{k}_POSZ", 1))) == 2]

    posz = values.get("EK3_SRC1_POSZ")
    suffix = (f" · height source: {option_label(EK3_POSZ_OPTIONS, posz)}"
              if posz is not None else "")
    toggle = _toggle(values, "range", label="Distance sensor", ports=ports,
                     enable=enable, disable=disable, problems=problems,
                     detail_suffix=suffix)
    if toggle is None:
        return None
    return {
        "id": "rangefinder", "title": "Distance sensor", "kind": "toggle",
        "group": "sensors", "icon": "radar",
        "short": "A downward lidar or sonar: height above the ground under the aircraft.",
        "toggle": toggle,
        "presets": _presets_for("range", values, ports),
        "fields": present([
            enum("RNGFND1_ORIENT", "Orientation", values, RANGEFINDER_ORIENT_OPTIONS,
                 hint="Down is what a landing or terrain-following sensor wants."),
            number("RNGFND1_MIN_CM", "Shortest reading", values, unit="cm",
                   step=1, min=0),
            number("RNGFND1_MAX_CM", "Longest reading", values, unit="cm",
                   step=10, min=0),
            number("RNGFND1_MIN", "Shortest reading", values, unit="m", step=0.01, min=0),
            number("RNGFND1_MAX", "Longest reading", values, unit="m", step=0.1, min=0),
            number("RNGFND1_GNDCLEAR", "Ground clearance", values, unit="cm",
                   step=1, min=0,
                   hint="Distance the sensor reads with the vehicle on the ground."),
            number("RNGFND1_GNDCLR", "Ground clearance", values, unit="m",
                   step=0.01, min=0,
                   hint="Distance the sensor reads with the vehicle on the ground."),
            number("RNGFND1_ADDR", "I2C address", values, step=1, min=0),
            number("RNGFND1_PIN", "Analog or PWM pin", values, step=1),
            number("RNGFND1_SCALING", "Scaling", values, step=0.01),
            number("RNGFND1_OFFSET", "Offset", values, step=0.01),
            number("RNGFND1_POS_X", "Mounting offset, X", values, unit="m", step=0.01),
            number("RNGFND1_POS_Y", "Mounting offset, Y", values, unit="m", step=0.01),
            number("RNGFND1_POS_Z", "Mounting offset, Z", values, unit="m", step=0.01),
            enum("RNGFND_LANDING", "Use for landing", values, ON_OFF_OPTIONS),
            enum("SURFTRAK_MODE", "Surface tracking", values, [
                {"value": 0, "label": "Disabled"},
                {"value": 1, "label": "Track the ground"},
            ]),
            enum("EK3_SRC1_POSZ", "Primary height source", values, EK3_POSZ_OPTIONS,
                 hint="Leave on Barometer. ArduPilot uses a downward rangefinder for "
                      "surface tracking and landing on its own; Rangefinder here is only "
                      "for indoor flight over a flat floor."),
        ]),
        "hint": "Switching it on starts the driver with everything it needs: the serial "
                "port and its baud rate, the CAN port, or the I2C address. ArduPilot then "
                "uses a downward rangefinder on its own for surface tracking and "
                "landing, so the estimator's height source stays on the barometer.",
    }


def _flow_section(values: dict[str, float], ports: list[dict[str, Any]],
                  ) -> dict[str, Any] | None:
    spec = _spec("flow", values.get("FLOW_TYPE"))
    estimator = _flow_estimator(values)
    problems = _driver_problems(values, "flow", spec, ports) if spec else []
    if not estimator["using"]:
        problems.append("No EKF source set uses optical flow (EK3_SRCn_VELXY = 5), so the "
                        "estimator ignores it.")

    enable = [{"param": w["param"], "value": w["value"]} for w in estimator["writes"]]
    disable = _flow_estimator_off(values, estimator["using"])
    suffix = (f" · estimator: source set {estimator['using'][0]}" if estimator["using"]
              else " · estimator: not used")
    toggle = _toggle(values, "flow", label="Optical flow", ports=ports,
                     enable=enable, disable=disable, problems=problems,
                     detail_suffix=suffix)
    if toggle is None:
        return None
    toggle["note"] = estimator["note"]
    return {
        "id": "flow", "title": "Optical flow", "kind": "toggle",
        "group": "sensors", "icon": "scan-line",
        "short": "A downward camera: horizontal velocity without GPS. Needs a distance sensor.",
        "toggle": toggle,
        "presets": _presets_for("flow", values, ports),
        "fields": present([
            number("FLOW_ORIENT_YAW", "Mounting yaw", values, unit="cdeg", step=100,
                   hint="Centidegrees, ArduPilot's own unit. 0 is the sensor's "
                        "forward axis aligned with the aircraft's."),
            number("FLOW_POS_X", "Mounting offset, X", values, unit="m", step=0.01),
            number("FLOW_POS_Y", "Mounting offset, Y", values, unit="m", step=0.01),
            number("FLOW_POS_Z", "Mounting offset, Z", values, unit="m", step=0.01,
                   hint="Positive down from the centre of gravity."),
            number("FLOW_FXSCALER", "X scale correction", values, step=1),
            number("FLOW_FYSCALER", "Y scale correction", values, step=1),
            number("FLOW_ADDR", "I2C address", values, step=1, min=0),
            number("EK3_FLOW_DELAY", "Measurement delay", values, unit="ms", step=1, min=0),
            enum("EK3_SRC1_POSXY", "Position source, set 1", values, EK3_POSXY_OPTIONS),
            enum("EK3_SRC1_VELXY", "Velocity source, set 1", values, EK3_VELXY_OPTIONS),
            enum("EK3_SRC2_VELXY", "Velocity source, set 2", values, EK3_VELXY_OPTIONS,
                 hint="Set 2 and 3 are selected in flight with an RC switch set to "
                      "EKF source (RCx_OPTION = 90)."),
        ]),
        "hint": "A downward camera for horizontal velocity. It needs a distance sensor to "
                "scale what it sees, so switch that on first; a copter flying on flow "
                "will not climb above that sensor's longest reading. Calibrate it in "
                "flight with an RC switch set to optical flow calibration "
                "(RCx_OPTION = 158).",
    }


# ---------------------------------------------------------------------------
# Hardware presets
# ---------------------------------------------------------------------------
#
# The same idea as the PX4 catalogue, with ArduPilot's own numbers from each
# sensor's wiki page (or, for the H-Flow, Holybro's ArduPilot setup page).
# ``serial`` is the SERIALn_PROTOCOL the module needs on the port the operator
# picks; ``can`` means a CAN port has to run DroneCAN; ``flow_estimator``
# means the EKF source set is set up the way the flow switch would.

def _benewake_preset(pid: str, label: str, model: str, *, type_value: int,
                     minimum: float, maximum: float, summary: str,
                     note: str) -> dict[str, Any]:
    return {
        "id": pid, "label": label, "vendor": "Benewake", "model": model,
        "bus": "UART", "provides": ["range"],
        "serial": SERIAL_PROTOCOL_RANGEFINDER, "baud": 115,
        "summary": summary, "note": note,
        "params": [
            ("RNGFND1_TYPE", type_value, "The Benewake serial driver for this model"),
            ("RNGFND1_ORIENT", 25, "It faces down"),
            ("RNGFND1_MIN", minimum, "Shortest reading, from the ArduPilot wiki"),
            ("RNGFND1_MAX", maximum, "Longest reading, from the ArduPilot wiki"),
            ("RNGFND1_GNDCLR", 0.1, "What it reads on the ground. Measure yours"),
        ],
        "reboot": True,
    }


SENSOR_PRESETS: list[dict[str, Any]] = [
    {
        "id": "holybro-h-flow", "label": "Holybro H-Flow", "vendor": "Holybro",
        "model": "19006", "bus": "DroneCAN", "provides": ["flow", "range"],
        "can": True, "flow_estimator": True,
        "summary": "PAA3905E1 optical flow and an AFBR-S50LV85D distance sensor on one "
                   "board, over a single CAN cable.",
        "note": "Mount it with the connectors pointing to the back of the vehicle, and "
                "set FLOW_POS_X, _Y and _Z to where it sits relative to the centre of "
                "gravity. The values are Holybro's ArduPilot setup, except the height "
                "source: ArduPilot's own wiki keeps that on the barometer.",
        "params": [
            ("RNGFND1_TYPE", 24, "Read the distance sensor from the DroneCAN node"),
            ("RNGFND1_ORIENT", 25, "It faces down"),
            ("RNGFND1_MAX", 30.0, "Longest reading, from Holybro's setup"),
            ("FLOW_TYPE", 6, "Read optical flow from the same node"),
        ],
        "reboot": True,
    },
    {
        "id": "matek-3901-l0x", "label": "Matek 3901-L0X", "vendor": "Matek Systems",
        "model": "3901-L0X", "bus": "UART (MSP)", "provides": ["flow", "range"],
        "serial": SERIAL_PROTOCOL_MSP, "baud": 115, "flow_estimator": True,
        "summary": "PMW3901 optical flow and a VL53L0X lidar on one UART speaking MSP.",
        "note": "Mount it lens down with the small arrow pointing forward. Its lidar "
                "reaches about 1.2 m and less outdoors, so the ArduPilot wiki recommends "
                "a longer range lidar for height.",
        "params": [
            ("RNGFND1_TYPE", 32, "Read the lidar over MSP"),
            ("RNGFND1_ORIENT", 25, "It faces down"),
            ("RNGFND1_MAX", 1.2, "Longest reading, from the ArduPilot wiki"),
            ("FLOW_TYPE", 7, "Read optical flow over MSP"),
            ("FLOW_FXSCALER", -800, "Scale correction the wiki gives for this module"),
            ("FLOW_FYSCALER", -800, "Scale correction the wiki gives for this module"),
        ],
        "reboot": True,
    },
    _benewake_preset(
        "benewake-tfmini-s", "Benewake TFmini-S", "TFmini-S", type_value=20,
        minimum=0.1, maximum=6.0,
        summary="Time-of-flight rangefinder, 0.1 to 12 m, on one UART at 115200 baud.",
        note="Leave it in UART mode. The longest reading is the wiki's outdoor value; "
             "indoors it can be raised to 10 m.",
    ),
    _benewake_preset(
        "benewake-tfmini-plus", "Benewake TFmini Plus", "TFmini Plus", type_value=20,
        minimum=0.1, maximum=6.0,
        summary="Time-of-flight rangefinder, 0.1 to 12 m, on one UART at 115200 baud.",
        note="Leave it in UART mode. The longest reading is the wiki's outdoor value; "
             "indoors it can be raised to 10 m.",
    ),
    _benewake_preset(
        "benewake-tf03", "Benewake TF03", "TF03", type_value=27,
        minimum=1.0, maximum=50.0,
        summary="Long range time-of-flight rangefinder on one UART at 115200 baud.",
        note="Keep it in UART mode. ArduPilot reads its CAN mode as well, but that is a "
             "different driver (RNGFND1_TYPE = 34) and not what this preset sets.",
    ),
]


def _param_for(name: str, value: float,
               values: dict[str, float]) -> tuple[str, float] | None:
    """The parameter this firmware stores *name* in, with the value in its unit."""
    if name in values:
        return name, float(value)
    alias = LENGTH_ALIASES.get(name)
    if alias and alias[0] in values:
        return alias[0], float(round(value * alias[1]))
    return None


def _preset_type(preset: dict[str, Any], kind: str) -> int | None:
    type_param = TYPE_PARAM[kind]
    return next((int(v) for p, v, _why in preset.get("params", []) if p == type_param), None)


def _resolve_preset(preset: dict[str, Any], kind: str, values: dict[str, float],
                    ports: list[dict[str, Any]]) -> dict[str, Any]:
    """One catalogue entry, turned into what this vehicle can do with it.

    Same contract as the PX4 resolver: every write is listed with its reason,
    a parameter this firmware lacks is named under ``missing``, and a module
    whose driver cannot be started from here is offered with the reason and
    nothing to apply.
    """
    reason = ""
    for covered in preset.get("provides", []):
        if TYPE_PARAM[covered] not in values:
            reason = (f"This firmware does not carry {TYPE_PARAM[covered]}, so this "
                      "module cannot be started from here.")
    protocol = preset.get("serial")
    if not reason and protocol is not None:
        if not ports:
            reason = ("This firmware reports no SERIALn_PROTOCOL parameters, so the port "
                      "cannot be set from here.")
        elif protocol == SERIAL_PROTOCOL_RANGEFINDER:
            reason = _serial_rangefinder_conflict(values)
    can = _dronecan(values) if preset.get("can") else None
    if not reason and can is not None and can["blocked"]:
        reason = can["blocked"]
    supported = not reason

    writes: list[dict[str, Any]] = []
    missing: list[str] = []
    clear: list[dict[str, Any]] = []
    note = str(preset.get("note", ""))
    if supported:
        if protocol is not None:
            name = SERIAL_PROTOCOL_NAMES[protocol]
            writes.append({"param": "SERIAL{port}_PROTOCOL", "value": float(protocol),
                           "port_param": True,
                           "label": f"The port the module is wired to speaks {name}"})
            if preset.get("baud"):
                writes.append({"param": "SERIAL{port}_BAUD", "value": float(preset["baud"]),
                               "port_param": True,
                               "label": f"{_baud_rate(preset['baud'])} baud"})
            if protocol in EXCLUSIVE_PROTOCOLS:
                clear = [{"port": n, "param": f"SERIAL{n}_PROTOCOL",
                          "value": float(SERIAL_PROTOCOL_NONE)}
                         for n in _ports_with(ports, protocol)]
        if can is not None:
            writes += [dict(w, label=CAN_WRITE_LABELS[w["param"]]) for w in can["writes"]]
        for param, value, label in preset.get("params", []):
            resolved = _param_for(param, value, values)
            if resolved is None:
                missing.append(param)
            else:
                writes.append({"param": resolved[0], "value": resolved[1], "label": label})
        if preset.get("flow_estimator"):
            estimator = _flow_estimator(values)
            writes += estimator["writes"]
            if estimator["note"]:
                note = f"{note} {estimator['note']}".strip()

    type_value = _preset_type(preset, kind)
    current = values.get(TYPE_PARAM[kind])
    return {
        "id": preset["id"],
        "label": preset["label"],
        "vendor": preset.get("vendor", ""),
        "model": preset.get("model", ""),
        "bus": preset.get("bus", ""),
        "summary": preset.get("summary", ""),
        "note": note,
        "wiring": (_can_wiring(values) if preset.get("can")
                   else SERIAL_WIRING if protocol is not None else ""),
        "serial": protocol is not None,
        "supported": supported,
        "unsupported": reason,
        "driver": f"{TYPE_PARAM[kind]}:{type_value}" if type_value is not None else "",
        "writes": writes,
        "kept": [],
        "clear": clear,
        "missing": missing,
        "active": (type_value is not None and current is not None
                   and int(round(current)) == type_value),
        "reboot": bool(preset.get("reboot")) and supported,
    }


def _presets_for(kind: str, values: dict[str, float],
                 ports: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [_resolve_preset(p, kind, values, ports)
            for p in SENSOR_PRESETS if kind in p.get("provides", [])]


# ---------------------------------------------------------------------------
# The parameter superset
# ---------------------------------------------------------------------------

RANGEFINDER_DRIVER_PARAMS: list[str] = [
    "RNGFND1_TYPE", "RNGFND1_ORIENT", "RNGFND1_MIN_CM", "RNGFND1_MAX_CM",
    "RNGFND1_MIN", "RNGFND1_MAX", "RNGFND1_GNDCLEAR", "RNGFND1_GNDCLR",
    "RNGFND1_ADDR", "RNGFND1_PIN", "RNGFND1_SCALING", "RNGFND1_OFFSET",
    "RNGFND1_POS_X", "RNGFND1_POS_Y", "RNGFND1_POS_Z",
    "RNGFND_LANDING", "SURFTRAK_MODE", "RNGFND2_TYPE",
]

FLOW_DRIVER_PARAMS: list[str] = [
    "FLOW_TYPE", "FLOW_ORIENT_YAW", "FLOW_POS_X", "FLOW_POS_Y", "FLOW_POS_Z",
    "FLOW_FXSCALER", "FLOW_FYSCALER", "FLOW_ADDR", "EK3_FLOW_DELAY",
]

# What a sensor driver can depend on: the serial ports, the CAN ports, and the
# GPS that decides which EKF source set flow goes into.
SENSOR_SUPPORT_PARAMS: list[str] = [
    *(f"SERIAL{n}_{p}" for n in SERIAL_PORTS for p in ("PROTOCOL", "BAUD")),
    "CAN_P1_DRIVER", "CAN_P2_DRIVER", "CAN_D1_PROTOCOL", "CAN_D2_PROTOCOL",
    "GPS_TYPE", "GPS1_TYPE",
]

EK3_SOURCE_PARAMS: list[str] = [
    *(f"EK3_SRC{k}_{s}" for k in (1, 2, 3)
      for s in ("POSXY", "VELXY", "POSZ", "VELZ", "YAW")),
    "EK3_SRC_OPTIONS",
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
        # Sensors, and everything their drivers and the estimator depend on
        *RANGEFINDER_DRIVER_PARAMS,
        *FLOW_DRIVER_PARAMS,
        *EK3_SOURCE_PARAMS,
        *SENSOR_SUPPORT_PARAMS,
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
               hint="Centimetres above home, ArduPilot's own unit. 1500 is 15 m. "
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
             "levels that trigger them. Every BATT_ parameter lives there.")


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
    parameters the operator added by name, and ``received``.

    *values* holds only the parameters the vehicle actually answered for, so
    every section shrinks or disappears on a firmware that lacks them.
    *vehicle* picks the enum labels that differ per firmware family; an
    unrecognised class falls back to Copter's, which is the most common and
    whose unknown numbers still render as "Unknown (n)" rather than being
    silently rewritten.
    """
    ports = _serial_ports(values)
    sections = [s for s in (
        _fence_section(values, vehicle),
        _rtl_section(values),
        _failsafe_section(values, vehicle),
        _arming_section(values),
        _rangefinder_section(values, ports),
        _flow_section(values, ports),
    ) if s is not None]
    flag_flow_without_range(sections)

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
        "received": len(values),
    }
