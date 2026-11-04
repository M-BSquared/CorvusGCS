"""PX4 sensor mounting schema: how the flight controller is turned, and where
the flight controller and the GPS antenna sit on the airframe.

Two different things, which is why :func:`build` returns them apart and two
pages show them:

``orientation``
    The rotation of the board relative to the airframe (``SENS_BOARD_ROT``)
    and the fine trims the level calibration writes. Every accelerometer and
    compass calibration is measured *through* this rotation, so it has to be
    right before calibrating, and changing it afterwards means calibrating
    again. It is shown on the Calibration page.

``positions``
    Lever arms: where the IMU and the GPS antenna sit relative to the centre
    of gravity, in metres, body frame (X forward, Y right, Z down). The
    estimator uses them in flight to remove the motion a sensor sees only
    because it is not at the centre of gravity. No calibration reads them.
    They share their origin with the motor positions (``CA_ROTORn_P*``), so
    they are drawn on the Motors page, on the same airframe.

Version tolerance (AGENTS.md: PX4 v1.16, v1.17 and v1.18):

* ``EKF2_IMU_POS_*`` exists on all three.
* The GPS antenna moved in v1.18: ``EKF2_GPS_POS_X/Y/Z`` became the
  per-receiver ``SENS_GPS0_OFFX/Y/Z`` and ``SENS_GPS1_OFFX/Y/Z`` (the firmware
  migrates the old values on import). Both sets are candidates, and the
  per-receiver one wins when a vehicle answers for both, because that is the
  one v1.18 reads.
* Every field is candidate only: a parameter the vehicle did not answer for is
  one field fewer, never an error.
"""

from __future__ import annotations

from typing import Any

from .param_fields import enum, number, present

# MAV_SENSOR_ORIENTATION. PX4's SENS_BOARD_ROT and ArduPilot's AHRS_ORIENTATION
# both use this numbering, which is why :mod:`corvus.ardupilot_mounting`
# imports the table instead of repeating it. Labels as PX4 v1.16 to v1.18
# document them (src/modules/sensors/sensor_params.c, min -1, max 40).
ROTATION_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "No rotation"},
    {"value": 1, "label": "Yaw 45°"},
    {"value": 2, "label": "Yaw 90°"},
    {"value": 3, "label": "Yaw 135°"},
    {"value": 4, "label": "Yaw 180°"},
    {"value": 5, "label": "Yaw 225°"},
    {"value": 6, "label": "Yaw 270°"},
    {"value": 7, "label": "Yaw 315°"},
    {"value": 8, "label": "Roll 180°"},
    {"value": 9, "label": "Roll 180°, Yaw 45°"},
    {"value": 10, "label": "Roll 180°, Yaw 90°"},
    {"value": 11, "label": "Roll 180°, Yaw 135°"},
    {"value": 12, "label": "Pitch 180°"},
    {"value": 13, "label": "Roll 180°, Yaw 225°"},
    {"value": 14, "label": "Roll 180°, Yaw 270°"},
    {"value": 15, "label": "Roll 180°, Yaw 315°"},
    {"value": 16, "label": "Roll 90°"},
    {"value": 17, "label": "Roll 90°, Yaw 45°"},
    {"value": 18, "label": "Roll 90°, Yaw 90°"},
    {"value": 19, "label": "Roll 90°, Yaw 135°"},
    {"value": 20, "label": "Roll 270°"},
    {"value": 21, "label": "Roll 270°, Yaw 45°"},
    {"value": 22, "label": "Roll 270°, Yaw 90°"},
    {"value": 23, "label": "Roll 270°, Yaw 135°"},
    {"value": 24, "label": "Pitch 90°"},
    {"value": 25, "label": "Pitch 270°"},
    {"value": 26, "label": "Pitch 180°, Yaw 90°"},
    {"value": 27, "label": "Pitch 180°, Yaw 270°"},
    {"value": 28, "label": "Roll 90°, Pitch 90°"},
    {"value": 29, "label": "Roll 180°, Pitch 90°"},
    {"value": 30, "label": "Roll 270°, Pitch 90°"},
    {"value": 31, "label": "Roll 90°, Pitch 180°"},
    {"value": 32, "label": "Roll 270°, Pitch 180°"},
    {"value": 33, "label": "Roll 90°, Pitch 270°"},
    {"value": 34, "label": "Roll 180°, Pitch 270°"},
    {"value": 35, "label": "Roll 270°, Pitch 270°"},
    {"value": 36, "label": "Roll 90°, Pitch 180°, Yaw 90°"},
    {"value": 37, "label": "Roll 90°, Yaw 270°"},
    {"value": 38, "label": "Roll 90°, Pitch 68°, Yaw 293°"},
    {"value": 39, "label": "Pitch 315°"},
    {"value": 40, "label": "Roll 90°, Pitch 315°"},
]

AXES = ("X", "Y", "Z")
AXIS_LABELS = {"X": "Forward", "Y": "Right", "Z": "Down"}

# The one sentence every position field shares: the sign convention is where
# a measured offset goes wrong.
POSITION_HINT = ("Metres from the centre of gravity. Forward, right and down are "
                 "positive, so a GPS on a mast above the frame has a negative Z.")

ORIENTATION_HINT = ("Set the rotation before calibrating. The accelerometer and "
                    "compass calibrations are measured through it, so changing it "
                    "later means calibrating both again, and the level horizon too. "
                    "It takes effect after a reboot.")

GPS_HINT = ("The antenna, not the receiver box. With RTK the offset is larger "
            "than the position error, so measure it.")


def _imu_names() -> list[str]:
    return [f"EKF2_IMU_POS_{a}" for a in AXES]


def _gps_names() -> list[str]:
    names = [f"EKF2_GPS_POS_{a}" for a in AXES]
    for receiver in (0, 1):
        names += [f"SENS_GPS{receiver}_OFF{a}" for a in AXES]
    # Whether a second receiver is configured at all: a serial one has a port,
    # a DroneCAN one is matched by device ID.
    names += ["SENS_GPS1_ID", "GPS_2_CONFIG"]
    return names


def param_names() -> list[str]:
    """Every parameter the mounting description may need, in one flat list."""
    return orientation_param_names() + position_param_names()


def orientation_param_names() -> list[str]:
    return ["SENS_BOARD_ROT", "SENS_BOARD_X_OFF", "SENS_BOARD_Y_OFF", "SENS_BOARD_Z_OFF"]


def position_param_names() -> list[str]:
    """The lever-arm parameters alone: what the Motors page adds to its read."""
    return _imu_names() + _gps_names()


def orientation(values: dict[str, float]) -> dict[str, Any] | None:
    """The board rotation and its fine trims, or None when the vehicle has neither."""
    fields = present([
        enum("SENS_BOARD_ROT", "Flight controller rotation", values, ROTATION_OPTIONS,
             reboot=True,
             hint="How the board is turned relative to the airframe, when its arrow "
                  "does not point forward or it is mounted upside down."),
        number("SENS_BOARD_X_OFF", "Roll trim", values, unit="deg", step=0.1),
        number("SENS_BOARD_Y_OFF", "Pitch trim", values, unit="deg", step=0.1,
               hint="The level horizon calibration sets roll and pitch trim. A few "
                    "degrees at most: anything larger is a rotation, not a trim."),
        number("SENS_BOARD_Z_OFF", "Yaw trim", values, unit="deg", step=0.1),
    ])
    if not fields:
        return None
    return {"fields": fields, "hint": ORIENTATION_HINT}


def _axis_fields(values: dict[str, float], names: list[str]) -> list[dict[str, Any]]:
    return present([
        number(name, AXIS_LABELS[axis], values, unit="m", step=0.001, min=-5, max=5)
        for name, axis in zip(names, AXES)
    ])


def _sensor(sensor_id: str, label: str, tag: str, values: dict[str, float],
            names: list[str], hint: str = "") -> dict[str, Any] | None:
    """One placed sensor: its position for the drawing and its fields to edit.

    ``kind`` picks the marker the drawing uses ("fc" or "gps"), ``tag`` is the
    few letters written beside it.
    """
    fields = _axis_fields(values, names)
    if not fields:
        return None
    x, y, z = (float(values.get(n, 0.0)) for n in names)
    out: dict[str, Any] = {
        "id": sensor_id, "label": label, "tag": tag,
        "kind": "fc" if sensor_id == "fc" else "gps",
        "x": x, "y": y, "z": z, "fields": fields,
    }
    if hint:
        out["hint"] = hint
    return out


def _second_receiver(values: dict[str, float]) -> bool:
    """Whether a second GPS is configured, or already has an offset of its own."""
    if int(round(values.get("SENS_GPS1_ID", 0.0))) != 0:
        return True
    if int(round(values.get("GPS_2_CONFIG", 0.0))) != 0:
        return True
    return any(abs(values.get(f"SENS_GPS1_OFF{a}", 0.0)) > 1e-9 for a in AXES)


def positions(values: dict[str, float]) -> list[dict[str, Any]]:
    """The flight controller and every GPS antenna the firmware has an offset for."""
    sensors = [_sensor("fc", "Flight controller", "FC", values, _imu_names(),
                       hint="Where the IMU sits. Keep it close to the centre of "
                            "gravity: every centimetre off it adds acceleration from "
                            "turning to what the accelerometer measures.")]
    per_receiver = [f"SENS_GPS0_OFF{a}" for a in AXES]
    if any(n in values for n in per_receiver):
        sensors.append(_sensor("gps1", "GPS antenna", "GPS", values, per_receiver,
                               hint=GPS_HINT))
        if _second_receiver(values):
            sensors.append(_sensor("gps2", "GPS 2 antenna", "GPS 2", values,
                                   [f"SENS_GPS1_OFF{a}" for a in AXES]))
    else:
        sensors.append(_sensor("gps1", "GPS antenna", "GPS", values,
                               [f"EKF2_GPS_POS_{a}" for a in AXES],
                               hint=GPS_HINT))
    return [s for s in sensors if s is not None]


def build(values: dict[str, float]) -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the mounting description.

    ``orientation``  ``{fields, hint}`` or None: the board rotation, for the
                     Calibration page.
    ``positions``    one entry per placed sensor, ``{id, label, tag, kind, x, y,
                     z, fields, hint}``: the flight controller and each GPS antenna,
                     for the Motors page.
    ``position_hint`` the sign convention every position field shares.
    """
    return {
        "orientation": orientation(values),
        "positions": positions(values),
        "position_hint": POSITION_HINT,
        "received": len(values),
    }
