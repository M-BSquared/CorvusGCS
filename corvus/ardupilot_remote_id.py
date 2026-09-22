"""ArduPilot Remote ID parameter schema for the Setup -> Remote ID page.

The ArduPilot twin of :mod:`corvus.remote_id_config`, and it is the larger of
the two because ArduPilot's ``AP_OpenDroneID`` is a driver the operator wires
up, not a single check. A PX4 board either has a Remote ID module on its
MAVLink stream or it does not; an ArduPilot board has to be told which serial
port or CAN driver the transmitter is on before anything is broadcast at all —
and a ``DID_ENABLE`` of 1 with no port set is the silent failure this page
exists to make visible.

The identity itself still is not here. ``OPEN_DRONE_ID_BASIC_ID``,
``OPEN_DRONE_ID_OPERATOR_ID``, ``OPEN_DRONE_ID_SELF_ID`` and
``OPEN_DRONE_ID_SYSTEM`` come from the ground station over the link, and
ArduPilot stops the aircraft arming when they stop arriving (see
:mod:`corvus.remote_id`). No ``DID_`` parameter holds a serial number.

Same contract as every other page: candidates only, so a 4.3 board without
``DID_BARO_ACC`` renders one field fewer, and a firmware built without
``AP_OpenDroneID`` at all answers for nothing and the section disappears
rather than erroring (AGENTS.md: graceful fallback).
"""

from __future__ import annotations

from typing import Any

from .param_fields import bitmask, enum, number, present, section

ON_OFF_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 1, "label": "Enabled"},
]

# DID_MAVPORT — which MAVLink serial port the transmitter answers on. -1 is
# "none", and it is the value a board ships with, which is why a bare
# DID_ENABLE=1 broadcasts nothing.
MAVPORT_OPTIONS: list[dict[str, Any]] = [
    {"value": -1, "label": "Disabled"},
    {"value": 0, "label": "Serial port 0 (console)"},
    {"value": 1, "label": "Serial port 1"},
    {"value": 2, "label": "Serial port 2"},
    {"value": 3, "label": "Serial port 3"},
    {"value": 4, "label": "Serial port 4"},
    {"value": 5, "label": "Serial port 5"},
    {"value": 6, "label": "Serial port 6"},
]

# DID_CANDRIVER — a DroneCAN Remote ID module instead of a serial one.
CANDRIVER_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 1, "label": "DroneCAN driver 1"},
    {"value": 2, "label": "DroneCAN driver 2"},
]

# DID_OPTIONS — one bit today. Rendered as a bitmask rather than a number for
# the same reason FS_OPTIONS is: the operator should never be adding powers of
# two to configure an arming check.
OPTIONS_BITS: list[dict[str, Any]] = [
    {"bit": 0, "label": "Enforce the arming checks"},
]

_CANDIDATES: tuple[str, ...] = (
    "DID_ENABLE",
    "DID_MAVPORT",
    "DID_CANDRIVER",
    "DID_OPTIONS",
    "DID_BARO_ACC",
)


def param_names() -> list[str]:
    """Every parameter this page may read, in one batch."""
    return list(_CANDIDATES)


def build(values: dict[str, float]) -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the vehicle half of the Remote ID page.

    Same output shape as :func:`corvus.remote_id_config.build`, so the frontend
    renders either stack with the one schema-driven form.
    """
    fields = present([
        enum("DID_ENABLE", "Remote ID", values, ON_OFF_OPTIONS,
             hint="Starts the Remote ID driver. It needs a reboot, and on its own it "
                  "broadcasts nothing — the transmitter's port has to be set below."),
        enum("DID_MAVPORT", "Transmitter serial port", values, MAVPORT_OPTIONS,
             hint="The MAVLink serial port a serial Remote ID module is wired to. "
                  "Disabled here with the driver enabled is the usual reason an "
                  "aircraft broadcasts nothing."),
        enum("DID_CANDRIVER", "DroneCAN transmitter", values, CANDRIVER_OPTIONS,
             hint="Use instead of the serial port when the module is on CAN."),
        bitmask("DID_OPTIONS", "Options", values, OPTIONS_BITS,
                hint="With the arming checks enforced, the aircraft refuses to arm "
                     "while the Remote ID system is unhealthy or the ground station "
                     "has stopped sending the identity."),
        number("DID_BARO_ACC", "Barometer height accuracy", values, unit="m",
               step=0.1, min=0,
               hint="What the broadcast claims for its barometric height. 0 declares "
                    "the accuracy unknown."),
    ])
    sections = [s for s in (
        section("vehicle", "On the aircraft", fields,
                "ArduPilot's Remote ID driver and the transmitter it talks to. The "
                "serial number, the operator registration and the class are not stored "
                "here — they are sent from this ground station over the link, and the "
                "aircraft will not arm once they stop arriving."),
    ) if s is not None]
    return {"sections": sections, "received": len(values)}
