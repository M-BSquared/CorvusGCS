"""The geofence area: the polygon the vehicle may fly in.

The operator draws it on the Safety & Sensors page. It is kept on this station
(``~/.corvus/geofence.json``) so it survives a restart and can be drawn on the
Home map without a vehicle, and it is put on the vehicle as an inclusion
polygon over the MAVLink mission protocol with ``mission_type`` FENCE.

That transfer is the same on PX4 and ArduPilot: one
``MAV_CMD_NAV_FENCE_POLYGON_VERTEX_INCLUSION`` item per corner, each carrying
the corner count in param1. What differs is what makes the fence act, which is
a parameter question and so belongs to the safety schema of each stack
(:func:`corvus.safety_config.geofence_enable_writes` and its ArduPilot twin).

Nothing here imports pymavlink, so the HTTP layer and the tests can use it
without a link.
"""
from __future__ import annotations

import json
import logging
import math
import os
import tempfile
from typing import Any

from .paths import corvus_path

logger = logging.getLogger("corvus.geofence")

# MAV_CMD_NAV_FENCE_POLYGON_VERTEX_INCLUSION and MAV_MISSION_TYPE_FENCE, stated
# here so this module stays importable without pymavlink. The bridge checks
# both against pymavlink at import.
MAV_CMD_NAV_FENCE_POLYGON_VERTEX_INCLUSION = 5001
MAV_MISSION_TYPE_FENCE = 1

# Corners one area may have. ArduPilot sizes its fence storage from the board
# and a small one holds a few dozen points; an area clicked onto a map is a
# handful. 64 is well past anything drawn by hand and inside what both stacks
# store.
MAX_VERTICES = 64
MIN_VERTICES = 3

FILE_NAME = "geofence.json"


def default_path() -> str:
    """Where the area is kept: ``~/.corvus/geofence.json``."""
    return corvus_path(FILE_NAME)


def _number(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    number = float(value)
    return number if math.isfinite(number) else None


def _segments_cross(a: tuple[float, float], b: tuple[float, float],
                    c: tuple[float, float], d: tuple[float, float]) -> bool:
    """Whether segment ab properly crosses segment cd (shared ends do not count)."""
    def orient(p: tuple[float, float], q: tuple[float, float], r: tuple[float, float]) -> float:
        return (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])

    d1, d2 = orient(c, d, a), orient(c, d, b)
    d3, d4 = orient(a, b, c), orient(a, b, d)
    return ((d1 > 0) != (d2 > 0) and d1 != 0 and d2 != 0
            and (d3 > 0) != (d4 > 0) and d3 != 0 and d4 != 0)


def self_intersects(polygon: list[list[float]]) -> bool:
    """Whether any two non-adjacent edges of the closed ring cross. Pure."""
    n = len(polygon)
    points = [(float(p[0]), float(p[1])) for p in polygon]
    for i in range(n):
        a, b = points[i], points[(i + 1) % n]
        for j in range(i + 1, n):
            if j == i or (j + 1) % n == i or j == (i + 1) % n:
                continue
            c, d = points[j], points[(j + 1) % n]
            if _segments_cross(a, b, c, d):
                return True
    return False


def validate_polygon(raw: Any) -> tuple[list[list[float]] | None, str]:
    """Clean an area from the browser: ``[[lon, lat], ...]``.

    Returns ``(polygon, "")`` or ``(None, reason)``. An empty list is a valid
    answer and means "no area". A closing corner that repeats the first is
    dropped, since the ring closes itself.
    """
    if raw is None:
        return [], ""
    if not isinstance(raw, list):
        return None, "the area must be a list of [lon, lat] corners"
    polygon: list[list[float]] = []
    for corner in raw:
        if not isinstance(corner, (list, tuple)) or len(corner) < 2:
            return None, "every corner must be [lon, lat]"
        lon, lat = _number(corner[0]), _number(corner[1])
        if lon is None or lat is None or abs(lon) > 180 or abs(lat) > 90:
            return None, "a corner is not a position on the globe"
        polygon.append([lon, lat])
    if len(polygon) > 1 and polygon[0] == polygon[-1]:
        polygon.pop()
    if not polygon:
        return [], ""
    if len(polygon) < MIN_VERTICES:
        return None, f"an area needs at least {MIN_VERTICES} corners"
    if len(polygon) > MAX_VERTICES:
        return None, f"an area may have at most {MAX_VERTICES} corners"
    if self_intersects(polygon):
        return None, "the edges of the area cross each other"
    return polygon, ""


def fence_items(polygon: list[list[float]]) -> list[dict[str, Any]]:
    """The fence transfer for *polygon*: one inclusion vertex per corner.

    ``{"command", "lat", "lon", "params"}`` like the mission items, with the
    corner count in param1 as the protocol asks of every vertex.
    """
    count = float(len(polygon))
    return [
        {"command": MAV_CMD_NAV_FENCE_POLYGON_VERTEX_INCLUSION,
         "lat": float(lat), "lon": float(lon),
         "params": [count, 0.0, 0.0, 0.0]}
        for lon, lat in polygon
    ]


def _empty() -> dict[str, Any]:
    return {"polygon": [], "show_on_map": True}


def load(path: str | None = None) -> dict[str, Any]:
    """The stored area and its map toggle. A missing or broken file is "none"."""
    target = path or default_path()
    try:
        with open(target, encoding="utf-8") as handle:
            data = json.load(handle)
    except FileNotFoundError:
        return _empty()
    except (OSError, ValueError) as exc:
        logger.warning("could not read the geofence from %s: %s", target, exc)
        return _empty()
    if not isinstance(data, dict):
        return _empty()
    polygon, _error = validate_polygon(data.get("polygon"))
    show = data.get("show_on_map")
    return {"polygon": polygon or [],
            "show_on_map": show if isinstance(show, bool) else True}


def save(state: dict[str, Any], path: str | None = None) -> None:
    """Write the area atomically: the field laptop loses power mid-write."""
    target = path or default_path()
    directory = os.path.dirname(target) or "."
    os.makedirs(directory, exist_ok=True)
    data = {"polygon": state.get("polygon") or [],
            "show_on_map": bool(state.get("show_on_map", True))}
    handle_fd, temp_path = tempfile.mkstemp(dir=directory, prefix=".geofence-", suffix=".json")
    try:
        with os.fdopen(handle_fd, "w", encoding="utf-8") as handle:
            json.dump(data, handle, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temp_path, target)
    except BaseException:
        try:
            os.unlink(temp_path)
        except OSError:
            pass
        raise
