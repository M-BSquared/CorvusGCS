"""ArduPilot battery schema for the Setup -> Battery & Power page.

The ArduPilot twin of :mod:`corvus.battery_config`, and it exists for the same
reason :mod:`corvus.ardupilot_safety` does: that module is not a PX4-flavoured
description of a universal idea, it is a list of PX4 parameter names, and not
one of them exists on an ArduPilot vehicle. Same output shape, same candidate
rule — a parameter the firmware did not answer for is one field fewer — so the
frontend cannot tell which stack built the page.

Two differences from PX4 are worth stating, because they are what the page's
diagram has to cope with:

*No cell count.* ArduPilot has no ``BATT_N_CELLS``. An analog monitor measures
the pack and nothing tells it how many cells are in there, which is exactly why
Corvus's own estimator carries a cell count of its own (see
:mod:`corvus.battery`) — on this stack it is the only place one exists.

*Absolute volts, not fractions.* ``BATT_LOW_VOLT`` is 21.0 V, where PX4's
``BAT_LOW_THR`` is 0.15 of a pack. Both travel to the frontend unconverted,
each tagged with what it is, because the value the page writes has to be the
value the parameter holds.

The prefix rule from :mod:`corvus.battery_config` applies here too: every
``BATT*`` parameter belongs to this page. On this stack that includes the two
battery failsafe *actions*, since ArduPilot keeps them under ``BATT_FS_*``
rather than with the rest of its failsafes.
"""
from __future__ import annotations

from typing import Any

from .param_fields import enum, number, option_label, present, section

# BATT_MONITOR. The list is long because ArduPilot supports a long list of
# hardware; what matters on the page is that the operator can see whether the
# autopilot is reading a power module, an ESC telemetry stream or nothing at
# all. A value this build does not name still renders as "Unknown (n)" rather
# than being rewritten (see param_fields.enum_options).
MONITOR_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 3, "label": "Analog voltage only"},
    {"value": 4, "label": "Analog voltage and current"},
    {"value": 5, "label": "Solo"},
    {"value": 6, "label": "Bebop"},
    {"value": 7, "label": "SMBus generic"},
    {"value": 8, "label": "DroneCAN BatteryInfo"},
    {"value": 9, "label": "ESC telemetry"},
    {"value": 10, "label": "Sum of selected monitors"},
    {"value": 11, "label": "Fuel flow"},
    {"value": 12, "label": "Fuel level (PWM)"},
    {"value": 14, "label": "SMBus SUI6"},
    {"value": 15, "label": "NeoDesign"},
    {"value": 16, "label": "SMBus Maxell"},
    {"value": 17, "label": "Generator (electrical)"},
    {"value": 18, "label": "Generator (fuel)"},
    {"value": 19, "label": "Rotoye"},
    {"value": 20, "label": "MPPT"},
    {"value": 21, "label": "INA2xx"},
    {"value": 22, "label": "LTC2946"},
    {"value": 23, "label": "Torqeedo"},
    {"value": 24, "label": "Fuel level (analog)"},
    {"value": 25, "label": "Synthetic current and voltage"},
    {"value": 27, "label": "EFI"},
    {"value": 28, "label": "AD7091R5"},
    {"value": 29, "label": "Scripting"},
]

# BATT_FS_LOW_ACT / BATT_FS_CRT_ACT. These moved here from
# :mod:`corvus.ardupilot_safety` with the rest of the battery parameters; the
# lists are per vehicle because "land" is not an option a rover has.
ACTION_OPTIONS: dict[str, list[dict[str, Any]]] = {
    "copter": [
        {"value": 0, "label": "Nothing"},
        {"value": 1, "label": "Land"},
        {"value": 2, "label": "RTL"},
        {"value": 3, "label": "SmartRTL, or RTL"},
        {"value": 4, "label": "SmartRTL, or land"},
        {"value": 5, "label": "Terminate"},
        {"value": 6, "label": "Auto landing sequence, or RTL"},
        {"value": 7, "label": "Brake, or land"},
    ],
    "plane": [
        {"value": 0, "label": "Nothing"},
        {"value": 1, "label": "RTL"},
        {"value": 2, "label": "Land"},
        {"value": 3, "label": "Terminate"},
        {"value": 4, "label": "QLand"},
        {"value": 6, "label": "Loiter, then QLand"},
    ],
    "rover": [
        {"value": 0, "label": "Nothing"},
        {"value": 1, "label": "RTL"},
        {"value": 2, "label": "Hold"},
        {"value": 3, "label": "SmartRTL, or RTL"},
        {"value": 4, "label": "SmartRTL, or hold"},
        {"value": 5, "label": "Terminate"},
    ],
}

# BATT_FS_VOLTSRC — which voltage the thresholds are read against.
FS_SOURCE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 1, "label": "Raw voltage"},
    {"value": 2, "label": "Voltage compensated for current draw"},
]

# The instances the page offers. ArduPilot supports more, but a third monitor
# on a GCS page is a row nobody has ever filled in; a vehicle that carries one
# still reports it over MAVLink.
INSTANCES = ("BATT", "BATT2")

_PACK_SUFFIXES = ("MONITOR", "CAPACITY", "LOW_VOLT", "CRT_VOLT", "LOW_MAH",
                  "CRT_MAH", "FS_LOW_ACT", "FS_CRT_ACT", "FS_VOLTSRC",
                  "LOW_TIMER", "ARM_VOLT", "ARM_MAH", "VOLT_PIN", "CURR_PIN",
                  "VOLT_MULT", "AMP_PERVLT", "AMP_OFFSET", "WATT_MAX")


def param_names() -> list[str]:
    """Every parameter the Battery & Power page may need, in one flat list."""
    return [f"{prefix}_{suffix}"
            for prefix in INSTANCES
            for suffix in _PACK_SUFFIXES]


def _pack_fields(prefix: str, values: dict[str, float]) -> list[dict[str, Any]]:
    return present([
        enum(f"{prefix}_MONITOR", "Monitor", values, MONITOR_OPTIONS,
             hint="What the autopilot reads the pack with. A change here needs "
                  "a reboot before the rest of these fields appear."),
        number(f"{prefix}_CAPACITY", "Capacity", values, unit="mAh", step=50,
               min=0,
               hint="The pack's rated capacity. Without it ArduPilot has no "
                    "capacity figure and reports no remaining percentage at all."),
        number(f"{prefix}_WATT_MAX", "Maximum power draw", values, unit="W",
               step=10, min=0,
               hint="Throttle is limited to hold the pack under this. 0 is off."),
    ])


def _sensing_fields(prefix: str, values: dict[str, float]) -> list[dict[str, Any]]:
    return present([
        number(f"{prefix}_VOLT_PIN", "Voltage pin", values, step=1, min=-1,
               hint="The board's analog pin for voltage. -1 on a monitor that "
                    "is not analog."),
        number(f"{prefix}_CURR_PIN", "Current pin", values, step=1, min=-1),
        number(f"{prefix}_VOLT_MULT", "Voltage multiplier", values, step=0.01,
               min=0,
               hint="Calibrate against a meter on the pack: a multiplier that "
                    "is 3% out is 3% of every reading on this page."),
        number(f"{prefix}_AMP_PERVLT", "Amps per volt", values, unit="A/V",
               step=0.01, min=0),
        number(f"{prefix}_AMP_OFFSET", "Current offset", values, unit="V",
               step=0.001, min=0,
               hint="The voltage the current sensor reads at zero amps."),
    ])


def _level_fields(prefix: str, values: dict[str, float],
                  vehicle: str) -> list[dict[str, Any]]:
    actions = ACTION_OPTIONS.get(vehicle, ACTION_OPTIONS["copter"])
    return present([
        number(f"{prefix}_LOW_VOLT", "Low voltage", values, unit="V", step=0.1,
               min=0,
               hint="Pack volts, not per cell. On a 6S LiPo, 21.0 V is 3.5 V a cell."),
        number(f"{prefix}_CRT_VOLT", "Critical voltage", values, unit="V",
               step=0.1, min=0),
        number(f"{prefix}_LOW_MAH", "Low capacity remaining", values, unit="mAh",
               step=50, min=0),
        number(f"{prefix}_CRT_MAH", "Critical capacity remaining", values,
               unit="mAh", step=50, min=0),
        enum(f"{prefix}_FS_LOW_ACT", "Low battery action", values, actions),
        enum(f"{prefix}_FS_CRT_ACT", "Critical battery action", values, actions),
        enum(f"{prefix}_FS_VOLTSRC", "Voltage source", values, FS_SOURCE_OPTIONS,
             hint="Compensated voltage subtracts the sag a high current draw "
                  "causes, so a hard climb does not trigger a low-battery "
                  "failsafe."),
        number(f"{prefix}_LOW_TIMER", "Low voltage must persist for", values,
               unit="s", step=1, min=0),
        number(f"{prefix}_ARM_VOLT", "Minimum voltage to arm", values, unit="V",
               step=0.1, min=0),
        number(f"{prefix}_ARM_MAH", "Minimum capacity to arm", values,
               unit="mAh", step=50, min=0),
    ])


def pack_summary(values: dict[str, float]) -> dict[str, Any]:
    """The numbers the battery diagram is drawn from, normalised.

    Same keys as :func:`corvus.battery_config.pack_summary`. ``cells``,
    ``full_cell`` and ``empty_cell`` are 0 here and always will be: ArduPilot
    has no parameter for any of them, so on this stack the diagram is drawn
    from Corvus's own estimator settings instead.
    """
    thresholds: list[dict[str, Any]] = []
    for param, label in (("BATT_LOW_VOLT", "Low"), ("BATT_CRT_VOLT", "Critical")):
        if param in values and float(values[param]) > 0:
            thresholds.append({
                "id": param.split("_")[1].lower(), "label": label,
                "param": param, "volts": round(float(values[param]), 2),
            })
    for param, label in (("BATT_LOW_MAH", "Low capacity"),
                         ("BATT_CRT_MAH", "Critical capacity")):
        if param in values and float(values[param]) > 0:
            thresholds.append({
                "id": param.split("_")[1].lower() + "_mah", "label": label,
                "param": param, "mah": round(float(values[param]), 0),
            })

    capacity = float(values.get("BATT_CAPACITY", 0) or 0)
    monitor = values.get("BATT_MONITOR")
    return {
        "cells": 0,
        "cells_param": "",
        "capacity_mah": round(capacity, 1) if capacity > 0 else 0.0,
        "capacity_param": "BATT_CAPACITY" if "BATT_CAPACITY" in values else "",
        "full_cell": 0.0,
        "empty_cell": 0.0,
        "load_drop": 0.0,
        "resistance_ohm": 0.0,
        "source": option_label(MONITOR_OPTIONS, float(monitor)) if monitor is not None else "",
        "thresholds": thresholds,
    }


def build(values: dict[str, float], vehicle: str = "copter") -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the Battery & Power page description.

    Same return shape as :func:`corvus.battery_config.build`. *vehicle* picks
    the failsafe-action labels that differ per firmware family; an unrecognised
    class falls back to Copter's, whose unknown numbers still render as
    "Unknown (n)" rather than being silently rewritten.
    """
    sections: list[dict[str, Any] | None] = [
        section("pack", "Pack", _pack_fields("BATT", values),
                hint="What is plugged in, and what reads it. ArduPilot has no "
                     "cell count of its own — Corvus keeps one below, for the "
                     "estimate and the diagram."),
        section("sensing", "Measurement", _sensing_fields("BATT", values),
                hint="Calibrate the multiplier against a meter before trusting "
                     "anything above."),
        section("levels", "Low-battery levels", _level_fields("BATT", values, vehicle),
                hint="Pack volts and remaining capacity, and what the vehicle "
                     "does when one of them is crossed."),
    ]

    second = (_pack_fields("BATT2", values) + _sensing_fields("BATT2", values)
              + _level_fields("BATT2", values, vehicle))
    if second:
        sections.append(section(
            "pack2", "Second pack", second,
            hint="The second monitor. Only fill this in on an aircraft that "
                 "actually carries two packs the autopilot measures separately.",
        ))

    live = [s for s in sections if s is not None]
    return {
        "stack": "ardupilot",
        "vehicle": vehicle,
        "sections": live,
        "pack": pack_summary(values),
        "received": len(values),
    }
