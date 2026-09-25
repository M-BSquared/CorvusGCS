"""Battery chemistry, cell arithmetic, and the remaining-charge estimate.

Pure functions and tables. No MAVLink, no HTTP, no config file — the bridge
calls this on every ``SYS_STATUS``, the HTTP layer calls it to describe a pack,
and the tests call it with numbers. That split is deliberate: this is the one
place that decides what "47% remaining" means, and a second copy of the curve
somewhere else is a second answer to that question.

Why estimate at all
-------------------
``SYS_STATUS.battery_remaining`` is whatever the autopilot chose to publish,
and on most airframes that is a coulomb count seeded by a guess. PX4 reports
100% until it has seen enough current to believe otherwise, ArduPilot reports
-1 (no estimate at all) unless ``BATT_CAPACITY`` is set and the monitor
measures current, and both are wrong in the one direction that costs an
aircraft: optimistic. A pack that was flown, charged to storage and flown again
reads full on the second take-off.

Resting cell voltage is the other half of the picture. It is not better in
every case — under a hard climb it reads far lower than the pack's real state
of charge — but it is *independent*, it needs no capacity figure, and it cannot
inherit a stale count. So Corvus computes it alongside the autopilot's own
number and lets the operator choose which one the interface shows, rather than
picking for them.

The curve
---------
State of charge against resting cell voltage is not a straight line. A LiPo
sits between 3.7 V and 3.9 V for most of its usable charge and then falls off a
cliff, so the linear reading a 3.3–4.2 V interpolation gives is pessimistic in
the middle and dangerously optimistic at the end — exactly backwards. Each
chemistry therefore carries a measured table and this module interpolates
between its points.

Sag
---
Voltage under load is not resting voltage. A 6S pack pulling 60 A through
5 mΩ per cell reads 1.8 V low, which is 30 percentage points of state of charge
at the wrong end of the curve. When the operator (or the autopilot's own
``BAT1_R_INTERNAL``) supplies an internal resistance, the measured voltage is
corrected back to rest before the curve is consulted. With no resistance
configured the raw voltage is used and the estimate is simply conservative —
never corrected by a number nobody measured.
"""
from __future__ import annotations

import math
from typing import Any

# The cell counts a real airframe uses. The auto-detect below walks this range,
# so its only job is to stay wide enough for the biggest pack anyone flies
# (24S exists on heavy lift) without inventing counts for noise.
MIN_CELLS = 1
MAX_CELLS = 24

# Per-cell resistance the sag correction will accept, in milliohms. A healthy
# pack is single-digit; anything past 100 mΩ per cell is a typo or a pack that
# should not be flown, and correcting by it would invent charge that is not
# there.
MAX_RESISTANCE_MOHM = 100.0

# State of charge against RESTING cell voltage, richest to flattest. Each table
# is ordered high to low and interpolated linearly between its points; the
# points themselves are what makes the answer non-linear.
#
# LiPo is the widely published discharge table for a cobalt-oxide cell at low
# load. Li-ion is an 18650-class cell, which holds a lower plateau and is
# usable further down. LiFePO4 is the flattest of the three — 3.20 V and
# 3.28 V are 30% and 70% — which is precisely why a linear reading of one is
# useless and a table is not.
CHEMISTRIES: dict[str, dict[str, Any]] = {
    "lipo": {
        "label": "LiPo",
        "full": 4.2,
        "empty": 3.3,
        "nominal": 3.7,
        "curve": [
            (4.20, 100.0), (4.15, 95.0), (4.11, 90.0), (4.08, 85.0),
            (4.02, 80.0), (3.98, 75.0), (3.95, 70.0), (3.91, 65.0),
            (3.87, 60.0), (3.85, 55.0), (3.84, 50.0), (3.82, 45.0),
            (3.80, 40.0), (3.79, 35.0), (3.77, 30.0), (3.75, 25.0),
            (3.73, 20.0), (3.71, 15.0), (3.69, 10.0), (3.61, 5.0),
            (3.27, 0.0),
        ],
    },
    "liion": {
        "label": "Li-ion",
        "full": 4.2,
        "empty": 3.0,
        "nominal": 3.6,
        "curve": [
            (4.20, 100.0), (4.10, 90.0), (4.00, 80.0), (3.92, 70.0),
            (3.85, 60.0), (3.78, 50.0), (3.70, 40.0), (3.62, 30.0),
            (3.53, 20.0), (3.40, 10.0), (3.20, 5.0), (3.00, 0.0),
        ],
    },
    "lifepo4": {
        "label": "LiFePO4",
        "full": 3.65,
        "empty": 2.8,
        "nominal": 3.2,
        "curve": [
            (3.65, 100.0), (3.35, 99.0), (3.32, 90.0), (3.30, 80.0),
            (3.28, 70.0), (3.26, 60.0), (3.25, 50.0), (3.22, 40.0),
            (3.20, 30.0), (3.17, 20.0), (3.13, 10.0), (3.00, 5.0),
            (2.80, 0.0),
        ],
    },
}

DEFAULT_CHEMISTRY = "lipo"

# What the estimator does when nobody has configured it. `estimate` is off on
# purpose: turning it on changes the number on the top bar, and a ground
# station must not do that to an operator who never asked.
DEFAULTS: dict[str, Any] = {
    "estimate": False,
    "chemistry": DEFAULT_CHEMISTRY,
    "cells": 0,          # 0 = work it out from the pack voltage
    "full_cell": 0.0,    # 0 = the chemistry's own figure
    "empty_cell": 0.0,   # 0 = the chemistry's own figure
    "resistance": 0.0,   # mOhm per cell, 0 = do not correct for sag
    "capacity_mah": 0.0,  # 0 = unknown
}


def chemistry(name: Any) -> dict[str, Any]:
    """The chemistry table for *name*, falling back to LiPo.

    A name this build has never heard of gets the default rather than an
    error: the setting travels in a config file that an older or newer Corvus
    may have written, and an unknown string must cost an estimate, not a
    launch.
    """
    key = str(name or "").strip().lower()
    return CHEMISTRIES.get(key, CHEMISTRIES[DEFAULT_CHEMISTRY])


def chemistry_options() -> list[dict[str, Any]]:
    """The chemistries as the page's picker wants them.

    The curve itself does not travel: the frontend never interpolates, it only
    names a chemistry and shows what one cell of it holds full and empty so the
    operator can tell LiPo from LiFePO4 without knowing the table behind it.
    """
    return [
        {
            "value": key,
            "label": entry["label"],
            "full": entry["full"],
            "empty": entry["empty"],
            "nominal": entry["nominal"],
        }
        for key, entry in CHEMISTRIES.items()
    ]


def _clamp(value: float, low: float, high: float) -> float:
    return low if value < low else (high if value > high else value)


def _number(raw: Any, default: float = 0.0) -> float:
    """A float, or *default*. Booleans are not numbers here."""
    if isinstance(raw, bool):
        return default
    try:
        value = float(raw)
    except (TypeError, ValueError):
        return default
    if value != value or value in (float("inf"), float("-inf")):
        return default
    return value


def coerce_settings(raw: Any) -> dict[str, Any] | None:
    """Keep the estimator keys out of a parsed config file; drop the rest.

    Bounded here rather than at the point of use, because these numbers reach
    an arithmetic path that runs on every telemetry frame: a negative cell
    count or a 4000 mOhm resistance typed into a config file must not be able
    to produce a percentage at all.
    """
    if not isinstance(raw, dict):
        return None
    out: dict[str, Any] = {}
    if isinstance(raw.get("estimate"), bool):
        out["estimate"] = raw["estimate"]
    name = raw.get("chemistry")
    if isinstance(name, str) and name.strip().lower() in CHEMISTRIES:
        out["chemistry"] = name.strip().lower()
    if "cells" in raw:
        out["cells"] = int(_clamp(_number(raw.get("cells")), 0, MAX_CELLS))
    for key, high in (("full_cell", 5.0), ("empty_cell", 5.0)):
        if key in raw:
            out[key] = round(_clamp(_number(raw.get(key)), 0.0, high), 3)
    if "resistance" in raw:
        out["resistance"] = round(
            _clamp(_number(raw.get("resistance")), 0.0, MAX_RESISTANCE_MOHM), 3)
    if "capacity_mah" in raw:
        out["capacity_mah"] = round(_clamp(_number(raw.get("capacity_mah")), 0.0, 1e6), 1)
    return out or None


def settings(raw: Any) -> dict[str, Any]:
    """The full estimator settings: the defaults with *raw* laid over them.

    Accepts a dict, a ``CorvusConfig`` (its ``battery`` attribute is used), or
    None, so every caller can hand in whatever it is holding.
    """
    if raw is not None and not isinstance(raw, dict):
        raw = getattr(raw, "battery", None)
    resolved = dict(DEFAULTS)
    resolved.update(coerce_settings(raw) or {})
    chem = chemistry(resolved["chemistry"])
    # Endpoints resolve here, once, so every function below can treat them as
    # present. 0 has always meant "use the chemistry's own figure"; an operator
    # who pinned 4.35 V full for a high-voltage LiPo keeps it.
    if not resolved["full_cell"]:
        resolved["full_cell"] = chem["full"]
    if not resolved["empty_cell"]:
        resolved["empty_cell"] = chem["empty"]
    if resolved["empty_cell"] >= resolved["full_cell"]:
        resolved["full_cell"] = chem["full"]
        resolved["empty_cell"] = chem["empty"]
    return resolved


def resolve_cells(voltage: float, resolved: dict[str, Any]) -> int:
    """How many cells the measured pack voltage implies.

    A configured count always wins — an operator who says 6S means 6S, and a
    pack flown flat enough to look like 5S must not silently become one.

    With no count configured this is ``ceil(voltage / full_cell)``: the fewest
    cells that could hold this voltage without any of them being over-charged.
    It is exact on a pack that has just been plugged in, which is when a ground
    station gets to look, and that is the case worth being right about. It is
    genuinely ambiguous on a flown pack — 21.0 V is a fresh 5S and a tired 6S,
    and no arithmetic separates them — which is why the caller latches the
    count from the first reading of a connection instead of recomputing it per
    frame, and why the page lets the operator pin it.

    Returns 0 when nothing fits: a pack whose per-cell voltage would be below
    the chemistry's empty is not a cell count, it is a measurement the page
    should decline to interpret.
    """
    configured = int(resolved.get("cells") or 0)
    if configured:
        return configured
    volts = _number(voltage)
    if volts <= 0:
        return 0
    full = float(resolved["full_cell"])
    if full <= 0:
        return 0
    # The epsilon is not decoration: 25.2 / 4.2 is 6.000000000000001 in binary
    # floating point, and a ceil of that turns every fully charged 6S pack into
    # a 7S one reading 3.6 V per cell.
    cells = math.ceil(volts / full - 1e-6)
    if cells < MIN_CELLS or cells > MAX_CELLS:
        return 0
    # A pack under its own empty voltage is still that pack — it is flat, not a
    # different count — so the floor is generous. What it rejects is noise: a
    # 2 V reading from a monitor that is not wired up.
    if volts / cells < float(resolved["empty_cell"]) * 0.75:
        return 0
    return cells


def rest_voltage(voltage: float, current: float, cells: int,
                 resolved: dict[str, Any]) -> float:
    """Pack voltage corrected back to rest, in volts.

    ``V_rest = V_measured + I * R_pack``. With no configured resistance this
    returns the measured voltage unchanged — an uncorrected estimate is
    conservative, and a correction by a number nobody measured is not.
    """
    volts = _number(voltage)
    amps = _number(current)
    ohms_per_cell = _number(resolved.get("resistance")) / 1000.0
    if volts <= 0 or amps <= 0 or cells <= 0 or ohms_per_cell <= 0:
        return volts
    return volts + amps * ohms_per_cell * cells


def state_of_charge(cell_voltage: float, resolved: dict[str, Any]) -> float:
    """Percent remaining for one cell at *cell_voltage*, 0–100.

    The chemistry's curve answers the question; the operator's endpoints
    rescale that answer so their own "empty" reads 0 and their own "full" reads
    100. Rescaling rather than clamping is what keeps a pack configured to land
    at 3.5 V per cell from showing 20% on the ground.
    """
    volts = _number(cell_voltage)
    if volts <= 0:
        return 0.0
    chem = chemistry(resolved.get("chemistry"))
    curve = chem["curve"]
    raw = _interpolate(curve, volts)
    at_empty = _interpolate(curve, float(resolved["empty_cell"]))
    at_full = _interpolate(curve, float(resolved["full_cell"]))
    span = at_full - at_empty
    if span <= 0:
        return _clamp(raw, 0.0, 100.0)
    return _clamp((raw - at_empty) / span * 100.0, 0.0, 100.0)


def _interpolate(curve: list[tuple[float, float]], volts: float) -> float:
    """Linear interpolation on a high-to-low (voltage, percent) table."""
    if volts >= curve[0][0]:
        return curve[0][1]
    if volts <= curve[-1][0]:
        return curve[-1][1]
    for index in range(len(curve) - 1):
        high_v, high_p = curve[index]
        low_v, low_p = curve[index + 1]
        if low_v <= volts <= high_v:
            span = high_v - low_v
            if span <= 0:
                return low_p
            return low_p + (volts - low_v) / span * (high_p - low_p)
    return curve[-1][1]


def estimate(voltage: float, current: float, raw_settings: Any = None) -> dict[str, Any]:
    """The whole voltage-side reading of a pack, as one dict.

    ``percent`` is -1 when the pack cannot be read at all (no voltage yet, or a
    voltage no cell count explains). -1 rather than 0 because a ground station
    that shows an empty battery when it simply has not been told anything is
    the same lie as one that shows a full one.
    """
    resolved = settings(raw_settings)
    volts = _number(voltage)
    cells = resolve_cells(volts, resolved)
    if volts <= 0 or cells <= 0:
        return {
            "percent": -1.0, "cells": cells, "cell_voltage": 0.0,
            "rest_cell_voltage": 0.0, "chemistry": resolved["chemistry"],
        }
    rest = rest_voltage(volts, current, cells, resolved)
    return {
        "percent": round(state_of_charge(rest / cells, resolved), 1),
        "cells": cells,
        "cell_voltage": round(volts / cells, 3),
        "rest_cell_voltage": round(rest / cells, 3),
        "chemistry": resolved["chemistry"],
    }


# Corvus's own time-to-empty, for the autopilot that publishes none. It is a
# straight line through the last few minutes of the percentage the operator is
# flying by, so it needs enough of a line to be one: a minute of samples, and
# at least a point of drop across them. Less than that and the slope is noise
# (a voltage reading wobbling with throttle, a coulomb count that has not moved
# yet), and a ground station that prints "3 h 40 min" off noise is worse than
# one that prints nothing.
ENDURANCE_WINDOW_S = 180.0
ENDURANCE_MIN_SPAN_S = 60.0
ENDURANCE_MIN_DROP = 1.0


def drain_endurance(samples: Any) -> float:
    """Seconds until the remaining percentage reaches zero at the recent rate.

    *samples* are ``(seconds, percent)`` pairs, oldest first, taken while the
    aircraft was armed. Only the last :data:`ENDURANCE_WINDOW_S` of them count,
    so a climb at the start of the flight does not set the rate for the cruise.
    Returns -1 when there is no honest answer: too short a window, too little
    drop, or a percentage that is not falling.
    """
    points: list[tuple[float, float]] = []
    for item in samples or ():
        try:
            t, p = float(item[0]), float(item[1])
        except (TypeError, ValueError, IndexError):
            continue
        if math.isfinite(t) and math.isfinite(p) and 0.0 <= p <= 100.0:
            points.append((t, p))
    if len(points) < 3:
        return -1.0
    latest = points[-1][0]
    points = [pt for pt in points if latest - pt[0] <= ENDURANCE_WINDOW_S]
    span = points[-1][0] - points[0][0]
    if len(points) < 3 or span < ENDURANCE_MIN_SPAN_S:
        return -1.0
    n = float(len(points))
    mean_t = sum(t for t, _ in points) / n
    mean_p = sum(p for _, p in points) / n
    sxx = sum((t - mean_t) ** 2 for t, _ in points)
    if sxx <= 0:
        return -1.0
    slope = sum((t - mean_t) * (p - mean_p) for t, p in points) / sxx
    if slope >= 0 or -slope * span < ENDURANCE_MIN_DROP:
        return -1.0
    # The fitted value rather than the last raw sample: the line is what the
    # rate came from, and a single sagging reading should not shorten it.
    now = max(0.0, mean_p + slope * (latest - mean_t))
    return float(round(now / -slope))
