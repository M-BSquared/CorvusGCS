"""PX4 battery schema for the Setup -> Battery & Power page.

Same split as :mod:`corvus.safety_config` and :mod:`corvus.tuning_config`: this
module owns the *knowledge* of which PX4 parameters describe a pack, the bridge
only fetches raw values, the HTTP layer only serialises, and the frontend
renders whatever description it is handed. Nothing on the page hardcodes a
parameter name, so a firmware that lacks one sends one field fewer.

The ownership rule between this page and Safety & Sensors is a prefix: every
``BAT*`` parameter belongs here, everything else belongs there. That is why the
low-battery *levels* moved onto this page while ``COM_LOW_BAT_ACT`` — the
*action* those levels trigger — stayed on the failsafe card where the RC-loss
and datalink-loss actions are. One control per parameter, and no operator has
to guess which of two pages holds the real value.

Three groups, in the order somebody actually sets a vehicle up:

``pack``       what is plugged in — cells, capacity, the voltage a cell holds
               full and empty, the sag allowance and the internal resistance
``sensing``    how the autopilot measures it — the source, the divider and the
               amps-per-volt of the power module, and the ADC channels
``levels``     the fractions at which the failsafe fires

Every field is *candidate* only, exactly as in :mod:`corvus.safety_config`: a
parameter this firmware did not answer for is dropped, and a section with
nothing left disappears. That is what lets one schema serve PX4 v1.16, v1.17
and v1.18 — and the pre-v1.11 un-indexed names (``BAT_N_CELLS`` rather than
``BAT1_N_CELLS``) listed alongside their modern replacements, where they cost
nothing on a firmware that has moved on.

``build`` also returns a ``pack`` summary. That is not a second copy of the
fields — it is the handful of numbers the page's battery diagram is drawn from
(how many cells to draw, where full and empty sit, where the thresholds fall),
normalised so the same drawing code serves this schema and the ArduPilot one
without asking which stack it is looking at.
"""
from __future__ import annotations

from typing import Any

from .param_fields import enum, number, option_label, present, section

# BAT1_SOURCE: where the measurement comes from at all. An operator who has
# wired a power module but left this on ESCs gets no voltage, which reads on
# every page as an aircraft with a flat battery.
SOURCE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Power module"},
    {"value": 1, "label": "External (ADC)"},
    {"value": 2, "label": "ESCs"},
]

# The instances PX4 exposes. Two is what the parameter set actually carries;
# a third pack is a DroneCAN battery and reports itself.
INSTANCES = (1, 2)


def _pack_fields(prefix: str, values: dict[str, float]) -> list[dict[str, Any]]:
    """The pack description for one battery instance."""
    return present([
        number(f"{prefix}_N_CELLS", "Cells in series", values, unit="S",
               step=1, min=0, max=24,
               hint="How many cells the pack has. Everything the autopilot "
                    "decides about the battery is per cell, so this is the one "
                    "number that must be right."),
        number(f"{prefix}_CAPACITY", "Capacity", values, unit="mAh", step=50,
               min=-1,
               hint="The pack's rated capacity. -1 leaves the autopilot with no "
                    "capacity figure, which is why its own remaining estimate "
                    "can only ever be a voltage reading."),
        number(f"{prefix}_V_CHARGED", "Full cell voltage", values, unit="V",
               step=0.01, min=0, max=5,
               hint="Resting voltage of one fully charged cell — 4.05 V is the "
                    "PX4 default for LiPo, 4.2 V is the charger's."),
        number(f"{prefix}_V_EMPTY", "Empty cell voltage", values, unit="V",
               step=0.01, min=0, max=5,
               hint="Resting voltage of one cell at 0% remaining. This is the "
                    "landing decision, not the cell's datasheet minimum."),
        number(f"{prefix}_V_LOAD_DROP", "Load drop allowance", values, unit="V",
               step=0.01, min=0, max=1.5,
               hint="How far one cell is expected to sag at full throttle. Used "
                    "to keep a hard climb from reading as an empty pack."),
        number(f"{prefix}_R_INTERNAL", "Internal resistance", values, unit="Ohm",
               step=0.001, min=-1, max=1,
               hint="Measured pack resistance, used to correct voltage back to "
                    "rest. -1 lets PX4 fall back to the load-drop figure above."),
    ])


def _sensing_fields(prefix: str, values: dict[str, float]) -> list[dict[str, Any]]:
    """How the autopilot measures one battery instance."""
    return present([
        enum(f"{prefix}_SOURCE", "Measured by", values, SOURCE_OPTIONS,
             hint="Where voltage and current are read from."),
        number(f"{prefix}_V_DIV", "Voltage divider", values, step=0.01, min=-1,
               hint="The board's voltage divider. Calibrate it against a meter "
                    "on the pack: a divider that is 3% out is 3% of every "
                    "reading on this page."),
        number(f"{prefix}_A_PER_V", "Amps per volt", values, unit="A/V",
               step=0.01, min=-1,
               hint="The power module's current scaling, from its datasheet or "
                    "measured against a clamp meter."),
        number(f"{prefix}_V_CHANNEL", "Voltage ADC channel", values, step=1,
               min=-1, hint="-1 uses the board default."),
        number(f"{prefix}_I_CHANNEL", "Current ADC channel", values, step=1,
               min=-1, hint="-1 uses the board default."),
    ])


def _legacy_pack_fields(values: dict[str, float]) -> list[dict[str, Any]]:
    """The un-indexed names PX4 used before multi-battery support (< v1.11).

    Listed beside the modern ones rather than behind a version switch: a
    firmware that has them does not have ``BAT1_*``, and one that has ``BAT1_*``
    never answers for these, so exactly one set survives the candidate filter.
    """
    return present([
        number("BAT_N_CELLS", "Cells in series", values, unit="S", step=1,
               min=0, max=24),
        number("BAT_CAPACITY", "Capacity", values, unit="mAh", step=50, min=-1),
        number("BAT_V_CHARGED", "Full cell voltage", values, unit="V",
               step=0.01, min=0, max=5),
        number("BAT_V_EMPTY", "Empty cell voltage", values, unit="V",
               step=0.01, min=0, max=5),
        number("BAT_V_LOAD_DROP", "Load drop allowance", values, unit="V",
               step=0.01, min=0, max=1.5),
        number("BAT_R_INTERNAL", "Internal resistance", values, unit="Ohm",
               step=0.001, min=-1, max=1),
    ])


def _levels_section(values: dict[str, float]) -> dict[str, Any] | None:
    fields = present([
        number("BAT_LOW_THR", "Low level", values, step=0.01, min=0, max=1,
               hint="Remaining capacity, 0-1, at which the low-battery action "
                    "fires. 0.15 is 15%."),
        number("BAT_CRIT_THR", "Critical level", values, step=0.01, min=0, max=1),
        number("BAT_EMERGEN_THR", "Emergency level", values, step=0.01, min=0,
               max=1),
        number("BAT_AVRG_CURRENT", "Average current", values, unit="A",
               step=0.1, min=0,
               hint="The current PX4 assumes when it has nothing better, which "
                    "is what its remaining-time figure is built on."),
    ])
    return section(
        "levels", "Low-battery levels", fields,
        hint="A fraction of the pack, not a voltage. What happens when one of "
             "these is crossed is the low-battery action on Safety & Sensors.",
    )


def param_names() -> list[str]:
    """Every parameter the Battery & Power page may need, in one flat list.

    Handed to the bridge as a single batched read: the page asks for the whole
    superset once and renders what came back, rather than probing name by name.
    """
    names: list[str] = ["BAT_LOW_THR", "BAT_CRIT_THR", "BAT_EMERGEN_THR",
                        "BAT_AVRG_CURRENT"]
    for suffix in ("N_CELLS", "CAPACITY", "V_CHARGED", "V_EMPTY", "V_LOAD_DROP",
                   "R_INTERNAL"):
        names.append(f"BAT_{suffix}")
    for instance in INSTANCES:
        prefix = f"BAT{instance}"
        for suffix in ("N_CELLS", "CAPACITY", "V_CHARGED", "V_EMPTY",
                       "V_LOAD_DROP", "R_INTERNAL", "SOURCE", "V_DIV",
                       "A_PER_V", "V_CHANNEL", "I_CHANNEL"):
            names.append(f"{prefix}_{suffix}")
    return names


def _first(values: dict[str, float], *names: str) -> tuple[str, float] | None:
    """The first of *names* this firmware answered for, with its value."""
    for name in names:
        if name in values:
            return name, float(values[name])
    return None


def pack_summary(values: dict[str, float]) -> dict[str, Any]:
    """The numbers the battery diagram is drawn from, normalised.

    Same keys as :func:`corvus.ardupilot_battery.pack_summary`, so the drawing
    code never asks which stack it is looking at. A key the stack has no answer
    for is 0 (or an empty list), which the page renders as "not known" rather
    than as a zero.
    """
    cells = _first(values, "BAT1_N_CELLS", "BAT_N_CELLS")
    capacity = _first(values, "BAT1_CAPACITY", "BAT_CAPACITY")
    full = _first(values, "BAT1_V_CHARGED", "BAT_V_CHARGED")
    empty = _first(values, "BAT1_V_EMPTY", "BAT_V_EMPTY")
    drop = _first(values, "BAT1_V_LOAD_DROP", "BAT_V_LOAD_DROP")
    resistance = _first(values, "BAT1_R_INTERNAL", "BAT_R_INTERNAL")
    source = _first(values, "BAT1_SOURCE")

    thresholds: list[dict[str, Any]] = []
    for param, label in (("BAT_LOW_THR", "Low"),
                         ("BAT_CRIT_THR", "Critical"),
                         ("BAT_EMERGEN_THR", "Emergency")):
        if param in values:
            thresholds.append({
                "id": param.split("_")[1].lower(), "label": label,
                "param": param, "percent": round(float(values[param]) * 100.0, 1),
            })

    return {
        "cells": int(cells[1]) if cells and cells[1] > 0 else 0,
        "cells_param": cells[0] if cells else "",
        "capacity_mah": round(capacity[1], 1) if capacity and capacity[1] > 0 else 0.0,
        "capacity_param": capacity[0] if capacity else "",
        "full_cell": round(full[1], 3) if full and full[1] > 0 else 0.0,
        "empty_cell": round(empty[1], 3) if empty and empty[1] > 0 else 0.0,
        "load_drop": round(drop[1], 3) if drop and drop[1] > 0 else 0.0,
        "resistance_ohm": round(resistance[1], 4) if resistance and resistance[1] > 0 else 0.0,
        "source": option_label(SOURCE_OPTIONS, source[1]) if source else "",
        "thresholds": thresholds,
    }


def build(values: dict[str, float]) -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the Battery & Power page description.

    *values* holds only the parameters the vehicle actually answered for, so
    every section shrinks or disappears on a firmware that lacks them. The
    return shape is what ``GET /api/battery`` serialises:

    ``sections`` — ordered ``{id, title, kind, fields[, hint]}``; ``kind`` is
    always ``"fields"`` here, and each field carries the parameter name it
    writes so the frontend applies edits through ``POST /api/params/set``.

    ``pack`` — the normalised summary the battery diagram is drawn from.
    """
    sections: list[dict[str, Any] | None] = [
        section("pack", "Pack",
                _pack_fields("BAT1", values) or _legacy_pack_fields(values),
                hint="What is plugged in. The cell count is the number to get "
                     "right first: every other figure here is per cell."),
        section("sensing", "Measurement", _sensing_fields("BAT1", values),
                hint="How the autopilot reads the pack. Calibrate the divider "
                     "against a meter before trusting anything above."),
        _levels_section(values),
    ]

    second_pack = _pack_fields("BAT2", values)
    second_sensing = _sensing_fields("BAT2", values)
    if second_pack or second_sensing:
        sections.append(section(
            "pack2", "Second pack", second_pack + second_sensing,
            hint="The second battery instance. Only fill this in on an aircraft "
                 "that actually carries two packs the autopilot measures "
                 "separately.",
        ))

    live = [s for s in sections if s is not None]
    return {
        "sections": live,
        "pack": pack_summary(values),
        "received": len(values),
    }
