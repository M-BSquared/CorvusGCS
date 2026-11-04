"""ArduPilot sensor mounting schema: the twin of :mod:`corvus.mounting_config`.

Same two halves and the same output shape, so neither page can tell which
stack built it: the board rotation (``AHRS_ORIENTATION`` and the level trims)
for the Calibration page, and the lever arms of the IMU and the GPS antennas
for the Motors page.

Version tolerance (AGENTS.md: ArduPilot 4.3 to 4.6):

* ``INS_POS1_*`` to ``INS_POS3_*`` exist on all four.
* The GPS antenna was renamed in 4.6, when the per-receiver GPS settings moved
  into their own groups: ``GPS_POS1_X`` became ``GPS1_POS_X`` (and
  ``GPS_TYPE2`` became ``GPS2_TYPE``). Both spellings are candidates, the 4.6
  one wins when a vehicle answers for both.

One thing ArduPilot does differently: it keeps a position per IMU, and a
flight controller carries two or three of them within millimetres of each
other. The flight controller's fields therefore write ``INS_POS1_*`` and carry
the matching ``INS_POS2_*``/``INS_POS3_*`` in ``also``, which the page writes
with the same value. Only an IMU that currently agrees with the first one is
carried along: one that was given a position of its own (an IMU on a separate
board) is left as it is.
"""

from __future__ import annotations

from typing import Any

from .mounting_config import (
    AXES, AXIS_LABELS, GPS_HINT, ORIENTATION_HINT, POSITION_HINT, ROTATION_OPTIONS,
)
from .param_fields import enum, number, present

# AHRS_ORIENTATION carries the MAV_SENSOR_ORIENTATION table plus a few of its
# own (4.3 to 4.6, libraries/AP_AHRS/AP_AHRS.cpp). 41 is internal and never a
# parameter value.
AHRS_ROTATION_OPTIONS: list[dict[str, Any]] = ROTATION_OPTIONS + [
    {"value": 42, "label": "Roll 45°"},
    {"value": 43, "label": "Roll 315°"},
    {"value": 101, "label": "Custom 1 (CUST_ROT1_*)"},
    {"value": 102, "label": "Custom 2 (CUST_ROT2_*)"},
]

IMU_COUNT = 3


def _imu_names(imu: int) -> list[str]:
    return [f"INS_POS{imu}_{a}" for a in AXES]


def _gps_names(receiver: int, *, legacy: bool) -> list[str]:
    if legacy:
        return [f"GPS_POS{receiver}_{a}" for a in AXES]
    return [f"GPS{receiver}_POS_{a}" for a in AXES]


def orientation_param_names() -> list[str]:
    return ["AHRS_ORIENTATION", "AHRS_TRIM_X", "AHRS_TRIM_Y"]


def position_param_names() -> list[str]:
    """The lever-arm parameters alone: what the Motors page adds to its read."""
    names: list[str] = []
    for imu in range(1, IMU_COUNT + 1):
        names += _imu_names(imu)
    for receiver in (1, 2):
        names += _gps_names(receiver, legacy=True) + _gps_names(receiver, legacy=False)
    names += ["GPS_TYPE2", "GPS2_TYPE"]
    return names


def param_names() -> list[str]:
    """Every parameter the mounting description may need, in one flat list."""
    return orientation_param_names() + position_param_names()


def orientation(values: dict[str, float]) -> dict[str, Any] | None:
    """The board rotation and the level trims, or None when neither answered."""
    fields = present([
        enum("AHRS_ORIENTATION", "Flight controller rotation", values,
             AHRS_ROTATION_OPTIONS, reboot=True,
             hint="How the board is turned relative to the airframe, when its arrow "
                  "does not point forward or it is mounted upside down."),
        number("AHRS_TRIM_X", "Roll trim", values, unit="rad", step=0.001,
               min=-0.1745, max=0.1745),
        number("AHRS_TRIM_Y", "Pitch trim", values, unit="rad", step=0.001,
               min=-0.1745, max=0.1745,
               hint="The level calibration sets both trims. 0.1745 rad is 10°, the "
                    "most ArduPilot accepts: anything larger is a rotation."),
    ])
    if not fields:
        return None
    return {"fields": fields, "hint": ORIENTATION_HINT}


def _axis_field(name: str, axis: str, values: dict[str, float],
                also: list[str]) -> dict[str, Any] | None:
    extra: dict[str, Any] = {"unit": "m", "step": 0.001, "min": -5, "max": 5}
    if also:
        extra["also"] = also
    return number(name, AXIS_LABELS[axis], values, **extra)


def _matching_imus(values: dict[str, float], axis: str) -> list[str]:
    """The further IMUs whose position on *axis* agrees with the first IMU's."""
    first = values.get(f"INS_POS1_{axis}")
    if first is None:
        return []
    return [
        f"INS_POS{imu}_{axis}" for imu in range(2, IMU_COUNT + 1)
        if f"INS_POS{imu}_{axis}" in values
        and abs(values[f"INS_POS{imu}_{axis}"] - first) < 1e-6
    ]


def _flight_controller(values: dict[str, float]) -> dict[str, Any] | None:
    fields = present([
        _axis_field(f"INS_POS1_{a}", a, values, _matching_imus(values, a)) for a in AXES
    ])
    if not fields:
        return None
    return {
        "id": "fc", "label": "Flight controller", "tag": "FC", "kind": "fc",
        "x": float(values.get("INS_POS1_X", 0.0)),
        "y": float(values.get("INS_POS1_Y", 0.0)),
        "z": float(values.get("INS_POS1_Z", 0.0)),
        "fields": fields,
        "hint": "Where the IMUs sit. Every IMU of the board that shares the first "
                "one's position is written with it. Keep the board close to the "
                "centre of gravity: every centimetre off it adds acceleration from "
                "turning to what the accelerometer measures.",
    }


def _gps(values: dict[str, float], receiver: int, label: str, tag: str,
         hint: str = "") -> dict[str, Any] | None:
    names = _gps_names(receiver, legacy=False)
    if not any(n in values for n in names):
        names = _gps_names(receiver, legacy=True)
    fields = present([
        _axis_field(name, axis, values, []) for name, axis in zip(names, AXES)
    ])
    if not fields:
        return None
    out: dict[str, Any] = {
        "id": f"gps{receiver}", "label": label, "tag": tag, "kind": "gps",
        "x": float(values.get(names[0], 0.0)),
        "y": float(values.get(names[1], 0.0)),
        "z": float(values.get(names[2], 0.0)),
        "fields": fields,
    }
    if hint:
        out["hint"] = hint
    return out


def _second_receiver(values: dict[str, float]) -> bool:
    kind = values.get("GPS2_TYPE", values.get("GPS_TYPE2", 0.0))
    return int(round(kind)) != 0


def positions(values: dict[str, float]) -> list[dict[str, Any]]:
    """The flight controller and every configured GPS antenna."""
    sensors = [_flight_controller(values), _gps(values, 1, "GPS antenna", "GPS", GPS_HINT)]
    if _second_receiver(values):
        sensors.append(_gps(values, 2, "GPS 2 antenna", "GPS 2"))
    return [s for s in sensors if s is not None]


def build(values: dict[str, float]) -> dict[str, Any]:
    """Same shape as :func:`corvus.mounting_config.build`."""
    return {
        "orientation": orientation(values),
        "positions": positions(values),
        "position_hint": POSITION_HINT,
        "received": len(values),
    }
