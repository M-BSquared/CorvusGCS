"""Parameter files in the formats other ground stations read.

* ``qgc``: QGroundControl's ``.params``, the file PX4 users exchange. Tab
  separated ``system  component  name  value  type`` rows under ``#`` comments,
  where type is the MAV_PARAM_TYPE the vehicle reported. QGroundControl checks
  that type on import, so it is written exactly as received.
* ``mission-planner``: Mission Planner's ``.param``, ``NAME,VALUE`` per line,
  which is what ArduPilot users and ArduPilot's own default files use.
* ``json``: Corvus's own file, with the export metadata beside the values.

The browser reads all three back on import (js/setup-parameters.js), because
that is where the file the operator picked is.
"""
from __future__ import annotations

import json
import math
import struct
from typing import Any

FORMAT_QGC = "qgc"
FORMAT_MISSION_PLANNER = "mission-planner"
FORMAT_JSON = "json"

PARAM_FILE_SUFFIX = {
    FORMAT_QGC: ".params",
    FORMAT_MISSION_PLANNER: ".param",
    FORMAT_JSON: ".json",
}

_REAL32 = 9


def format_param_value(value: Any, param_type: Any) -> str:
    """The shortest text that reads back as the value the vehicle holds.

    A float parameter travels as a float32, so 0.1 arrives as
    0.10000000149011612. Writing that is correct but unreadable, and writing it
    rounded to a fixed number of digits can change the value; the shortest
    decimal that rounds to the same float32 is both exact and what a person
    typed in the first place.
    """
    number = float(value)
    if not math.isfinite(number):
        return repr(number)
    try:
        kind = int(param_type)
    except (TypeError, ValueError):
        kind = _REAL32
    if kind != _REAL32 and number == int(number):
        return str(int(number))
    try:
        target = struct.unpack("<f", struct.pack("<f", number))[0]
    except OverflowError:
        return repr(number)
    for digits in range(1, 10):
        text = f"{number:.{digits}g}"
        if struct.unpack("<f", struct.pack("<f", float(text)))[0] == target:
            return text
    return repr(number)


def write_qgc(
    params: list[dict[str, Any]], *, system_id: int, component_id: int,
    stack: str = "", vehicle: str = "", version: str = "", git_hash: str = "",
) -> str:
    """Render *params* as a QGroundControl ``.params`` file."""
    lines = [
        f"# Onboard parameters for Vehicle {system_id}",
        "#",
        f"# Stack: {stack}",
        f"# Vehicle: {vehicle}",
        f"# Version: {version}",
        f"# Git Revision: {git_hash}",
        "#",
        "# Vehicle-Id Component-Id Name Value Type",
    ]
    for p in sorted(params, key=lambda p: p["name"]):
        ptype = p.get("type") or _REAL32
        lines.append("\t".join((
            str(system_id), str(component_id), p["name"],
            format_param_value(p["value"], ptype), str(int(ptype)),
        )))
    return "\n".join(lines) + "\n"


def write_mission_planner(params: list[dict[str, Any]], *, header: str = "") -> str:
    """Render *params* as a Mission Planner ``.param`` file."""
    lines = [f"# {line}" for line in header.splitlines() if line.strip()]
    for p in sorted(params, key=lambda p: p["name"]):
        lines.append(f"{p['name']},{format_param_value(p['value'], p.get('type'))}")
    return "\n".join(lines) + "\n"


def write_json(doc: dict[str, Any]) -> str:
    """Render the Corvus JSON parameter file."""
    return json.dumps(doc, indent=2, ensure_ascii=False) + "\n"
