"""First setup: is the connected controller one nobody has configured yet?

The first setup wizard (``src/js/first-setup.js``) walks the operator through
the pages that already exist. What this module adds is the part that needs the
vehicle: reading a handful of parameters to decide whether a controller is
fresh, and the table of airframes the wizard offers for the one step no other
page covers (choosing what kind of aircraft this is).

Detection reads facts, not a flag Corvus set earlier, so a board configured by
another ground station is recognised as configured and a wiped board as fresh:

``airframe``
    PX4 ``SYS_AUTOSTART`` is 0 until an airframe is chosen. ArduPilot Copter
    ships with ``FRAME_CLASS`` 1, so it can never be called unset there; its
    class and type are still offered, and read back.
``accel`` / ``gyro`` / ``mag``
    PX4 stores the device id a calibration was made for (``CAL_ACC0_ID`` and
    friends); 0 means none was ever done. ArduPilot has no such id that stays
    zero, so its accelerometer and compass are judged by the offsets a
    calibration writes, all zero meaning never calibrated. ArduPilot's gyro
    calibrates itself at every boot and is not asked about.

Version tolerance (AGENTS.md: PX4 v1.16, v1.17 and v1.18): every parameter is a
candidate only. One the vehicle does not answer for makes that fact
``unknown``, never an error, and ``unknown`` never counts towards a prompt.

``needs_setup`` is the fact ("this looks fresh"); ``prompt`` adds the policy
(offer the wizard unprompted). A simulator over UDP or TCP is never prompted:
PX4 SITL and ArduPilot SITL carry no calibration the way a board does, and a
wizard on every simulator start would be noise. It can still be opened by hand.
"""

from __future__ import annotations

from typing import Any

from . import autopilot

DONE = "done"
TODO = "todo"
UNKNOWN = "unknown"
NOT_REQUIRED = "not_required"

# Links where a fresh board can be sitting on the other end of the cable.
PHYSICAL_TRANSPORTS: frozenset[str] = frozenset({"usb", "sik"})

PX4_PARAMS: list[str] = [
    "SYS_AUTOSTART", "CAL_ACC0_ID", "CAL_GYRO0_ID", "CAL_MAG0_ID", "SYS_HAS_MAG",
]
ARDUPILOT_PARAMS: list[str] = [
    "FRAME_CLASS", "FRAME_TYPE",
    "INS_ACCOFFS_X", "INS_ACCOFFS_Y", "INS_ACCOFFS_Z",
    "COMPASS_OFS_X", "COMPASS_OFS_Y", "COMPASS_OFS_Z",
    "COMPASS_DEV_ID", "COMPASS_USE",
]

# SYS_AUTOSTART presets, the generic airframes of PX4's airframe reference. Ids
# are the same on v1.16, v1.17 and v1.18 (the rover ids moved in v1.15, which is
# before the primary target). Vendor frames are deliberately absent: a vendor
# airframe is a specific product with tuned gains, and the Parameters page can
# set one for whoever owns it.
PX4_PRESETS: list[dict[str, Any]] = [
    {"id": 4001, "group": "Multirotor", "label": "Quadcopter X"},
    {"id": 5001, "group": "Multirotor", "label": "Quadcopter +"},
    {"id": 6001, "group": "Multirotor", "label": "Hexarotor X"},
    {"id": 7001, "group": "Multirotor", "label": "Hexarotor +"},
    {"id": 8001, "group": "Multirotor", "label": "Octocopter X"},
    {"id": 9001, "group": "Multirotor", "label": "Octocopter +"},
    {"id": 2100, "group": "Fixed wing", "label": "Standard plane"},
    {"id": 3000, "group": "Fixed wing", "label": "Flying wing"},
    {"id": 13000, "group": "VTOL", "label": "Standard VTOL"},
    {"id": 13200, "group": "VTOL", "label": "Tailsitter VTOL"},
    {"id": 13100, "group": "VTOL", "label": "Tiltrotor VTOL"},
    {"id": 50000, "group": "Rover", "label": "Rover, differential"},
    {"id": 51000, "group": "Rover", "label": "Rover, Ackermann"},
]

# ArduPilot picks the vehicle by firmware, so only the Copter has a frame to
# choose here: FRAME_CLASS and FRAME_TYPE. Plane and Rover are set up on the
# pages that follow.
ARDUPILOT_COPTER_PRESETS: list[dict[str, Any]] = [
    {"id": 101, "group": "Multirotor", "label": "Quadcopter X",
     "writes": {"FRAME_CLASS": 1, "FRAME_TYPE": 1}},
    {"id": 100, "group": "Multirotor", "label": "Quadcopter +",
     "writes": {"FRAME_CLASS": 1, "FRAME_TYPE": 0}},
    {"id": 201, "group": "Multirotor", "label": "Hexacopter X",
     "writes": {"FRAME_CLASS": 2, "FRAME_TYPE": 1}},
    {"id": 200, "group": "Multirotor", "label": "Hexacopter +",
     "writes": {"FRAME_CLASS": 2, "FRAME_TYPE": 0}},
    {"id": 301, "group": "Multirotor", "label": "Octocopter X",
     "writes": {"FRAME_CLASS": 3, "FRAME_TYPE": 1}},
    {"id": 300, "group": "Multirotor", "label": "Octocopter +",
     "writes": {"FRAME_CLASS": 3, "FRAME_TYPE": 0}},
]


def param_names(stack: str) -> list[str]:
    """Every parameter the assessment reads on *stack*."""
    if stack == autopilot.STACK_ARDUPILOT:
        return list(ARDUPILOT_PARAMS)
    return list(PX4_PARAMS)


def presets(stack: str, vehicle: str) -> list[dict[str, Any]]:
    """The airframes offered for the connected *stack* and *vehicle*.

    *vehicle* is the firmware family of the dialect layer ("Copter", "Plane"
    and so on, see :func:`corvus.ardupilot_firmware.vehicle_for_mav_type`).
    PX4 offers the same list whatever it reports, because a fresh board's
    heartbeat says nothing true about the airframe it has not been given yet.
    """
    if stack == autopilot.STACK_ARDUPILOT:
        if vehicle == "Copter":
            return [_public(p) for p in ARDUPILOT_COPTER_PRESETS]
        return []
    return [_public(p) for p in PX4_PRESETS]


def _public(preset: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in preset.items() if k != "writes"}


def writes_for(stack: str, vehicle: str, preset_id: int) -> list[tuple[str, float]] | None:
    """The parameter writes that choose preset *preset_id*, or None if unknown.

    The table above is the whole allow list: the endpoint never writes a value
    the browser supplied, only one of these, so a request cannot reach a
    parameter this module does not name (AGENTS.md: never set a parameter the
    schema for the connected firmware does not contain).

    PX4 gets ``SYS_AUTOCONFIG = 1`` after ``SYS_AUTOSTART``, the way QGroundControl
    does it: without it the board keeps the previous airframe's parameter
    defaults and only the id changes.
    """
    if stack == autopilot.STACK_ARDUPILOT:
        if vehicle != "Copter":
            return None
        for preset in ARDUPILOT_COPTER_PRESETS:
            if preset["id"] == preset_id:
                return [(name, float(value)) for name, value in preset["writes"].items()]
        return None
    if any(p["id"] == preset_id for p in PX4_PRESETS):
        return [("SYS_AUTOSTART", float(preset_id)), ("SYS_AUTOCONFIG", 1.0)]
    return None


def _calibrated_by_id(values: dict[str, float], name: str) -> str:
    if name not in values:
        return UNKNOWN
    return DONE if int(round(values[name])) != 0 else TODO


def _calibrated_by_offsets(values: dict[str, float], names: tuple[str, ...]) -> str:
    seen = [values[n] for n in names if n in values]
    if not seen:
        return UNKNOWN
    return DONE if any(abs(v) > 1e-9 for v in seen) else TODO


def facts(stack: str, values: dict[str, float]) -> dict[str, str]:
    """The four facts the wizard's steps are marked from."""
    if stack == autopilot.STACK_ARDUPILOT:
        accel = _calibrated_by_offsets(
            values, ("INS_ACCOFFS_X", "INS_ACCOFFS_Y", "INS_ACCOFFS_Z"))
        has_compass = (
            "COMPASS_DEV_ID" in values and int(round(values["COMPASS_DEV_ID"])) != 0
            and int(round(values.get("COMPASS_USE", 1.0))) != 0
        )
        if has_compass:
            mag = _calibrated_by_offsets(
                values, ("COMPASS_OFS_X", "COMPASS_OFS_Y", "COMPASS_OFS_Z"))
        else:
            mag = NOT_REQUIRED if "COMPASS_DEV_ID" in values else UNKNOWN
        frame = values.get("FRAME_CLASS")
        return {
            "airframe": UNKNOWN if frame is None else (DONE if int(round(frame)) != 0 else TODO),
            "accel": accel,
            "gyro": NOT_REQUIRED,
            "mag": mag,
        }
    autostart = values.get("SYS_AUTOSTART")
    mag = _calibrated_by_id(values, "CAL_MAG0_ID")
    if "SYS_HAS_MAG" in values and int(round(values["SYS_HAS_MAG"])) == 0:
        mag = NOT_REQUIRED
    return {
        "airframe": UNKNOWN if autostart is None else (DONE if int(round(autostart)) != 0 else TODO),
        "accel": _calibrated_by_id(values, "CAL_ACC0_ID"),
        "gyro": _calibrated_by_id(values, "CAL_GYRO0_ID"),
        "mag": mag,
    }


def assess(stack: str, vehicle: str, transport: str, values: dict[str, float]) -> dict[str, Any]:
    """Judge one controller from the parameters read off it.

    A controller is fresh when its airframe is unset (PX4), or when nothing it
    carries has ever been calibrated. One missing calibration on an otherwise
    configured board is "incomplete", shown in the wizard but not worth an
    unprompted dialog on every connect.
    """
    found = facts(stack, values)
    reasons: list[str] = []
    if found["airframe"] == TODO and stack != autopilot.STACK_ARDUPILOT:
        reasons.append("No airframe is selected.")
    judged = [found[k] for k in ("accel", "gyro", "mag") if found[k] in (DONE, TODO)]
    if judged and all(v == TODO for v in judged):
        reasons.append("No sensor has been calibrated.")
    needs = bool(reasons)
    return {
        "stack": stack,
        "vehicle": vehicle,
        "transport": transport,
        "facts": found,
        "reasons": reasons,
        "needs_setup": needs,
        "prompt": needs and transport in PHYSICAL_TRANSPORTS,
        "presets": presets(stack, vehicle),
        "preset_current": _current_preset(stack, values),
    }


def _current_preset(stack: str, values: dict[str, float]) -> int | None:
    """The preset id the vehicle holds now, or None when it matches none."""
    if stack == autopilot.STACK_ARDUPILOT:
        if "FRAME_CLASS" not in values or "FRAME_TYPE" not in values:
            return None
        pair = (int(round(values["FRAME_CLASS"])), int(round(values["FRAME_TYPE"])))
        for preset in ARDUPILOT_COPTER_PRESETS:
            w = preset["writes"]
            if (w["FRAME_CLASS"], w["FRAME_TYPE"]) == pair:
                return int(preset["id"])
        return None
    value = values.get("SYS_AUTOSTART")
    return int(round(value)) if value is not None and int(round(value)) != 0 else None
