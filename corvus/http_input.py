"""Parsing and validation of what the HTTP handlers receive: ports, file and
region names, tile bounds and zooms, takeoff altitudes, place hints.

Pure functions, moved out of corvus/server.py, which re-exports them.
"""
from __future__ import annotations

import math
import os
import re
import time
from typing import Any

from . import geocode
from .mavlink_bridge import TAKEOFF_ALTITUDE_MAX_M, TAKEOFF_ALTITUDE_MIN_M

# A region name is operator-typed free text that ends up in the UI and in the
# .mbtiles file. Keep it short and single-line; everything else about it is the
# operator's business.
_MAX_REGION_NAME = 60


def _coerce_port(value: Any, default: int) -> int:
    """A TCP port from *value*, or *default* when it is not one.

    Saved config entries have already been coerced once on load, but a
    hand-edited file can still carry a port the range check would reject, and
    a connection attempt is not the place to discover it.
    """
    if isinstance(value, bool):
        return default
    try:
        port = int(value)
    except (TypeError, ValueError):
        return default
    return port if 0 <= port <= 65535 else default


def _slugify(value: Any) -> str:
    """Lowercase, filename-safe slug of *value* ("" when there is nothing)."""
    text = re.sub(r"[^A-Za-z0-9]+", "-", str(value or "")).strip("-").lower()
    return text


def _default_params_filename(vehicle_tag: str = "", suffix: str = ".json") -> str:
    """Build a readable default name for an exported parameter file.

    ``corvus-params_<vehicle>_<YYYY-MM-DD_HH-MM><suffix>``. Local time and a
    vehicle tag rather than the old epoch-milliseconds name: a folder of
    exports has to be scannable by eye, and "which airframe was this?" is the
    first question asked of an old parameter file.
    """
    stamp = time.strftime("%Y-%m-%d_%H-%M")
    tag = _slugify(vehicle_tag)
    stem = f"corvus-params_{tag}_{stamp}" if tag else f"corvus-params_{stamp}"
    return stem + suffix


def _safe_filename(raw: Any, fallback: str, suffix: str = ".json") -> str:
    """Reduce *raw* to a single safe filename, or return *fallback*.

    Strips any directory component (so ``../../etc/x`` cannot escape the target
    directory), rejects the empty/dot names, and forces *suffix* so the file is
    recognizable (and, for parameter files, re-importable). A non-string takes
    the fallback rather than being coerced — a client bug should produce the
    sensible default name, not a file called ``42.json``.
    """
    if not isinstance(raw, str):
        return fallback
    name = os.path.basename(raw.strip())
    name = re.sub(r'[\x00-\x1f<>:"/\\|?*]', "", name).strip()
    if not name or name in (".", ".."):
        return fallback
    if name.lower().endswith(suffix.lower()):
        name = name[: -len(suffix)]
    # Truncate the STEM, then re-attach the suffix — truncating afterwards
    # would chop the extension off a long name and leave an unrecognizable file.
    name = name[:115].rstrip() or fallback
    return name + suffix


def _clean_region_name(raw: Any) -> str:
    """Normalize an operator-supplied region name, or return "" if unusable.

    Collapses whitespace (a pasted multi-line name would break the list
    layout) and truncates. Non-strings become "", so the caller falls back to
    a generated name rather than rejecting the download.
    """
    if not isinstance(raw, str):
        return ""
    name = " ".join(raw.split())
    return name[:_MAX_REGION_NAME]


def _default_region_name(bounds: tuple[float, float, float, float]) -> str:
    """Generate a name for an unnamed download: its centre coordinates.

    Not a timestamp — in the field the operator recognizes an area by where it
    is, and a list of identical-looking timestamps is no better than no names
    at all. They can rename it afterwards.
    """
    w, s, e, n = bounds
    lat = (s + n) / 2.0
    lon = (w + e) / 2.0
    return f"{abs(lat):.3f}\u00b0{'N' if lat >= 0 else 'S'} {abs(lon):.3f}\u00b0{'E' if lon >= 0 else 'W'}"


def _int_or(raw: Any, fallback: int) -> int:
    """A query parameter as an int, or the fallback. Never raises."""
    try:
        return int(str(raw).strip())
    except (TypeError, ValueError):
        return fallback


def _parse_near(raw: Any) -> geocode.Near | None:
    """``"<lon>,<lat>"`` from a query string, or None.

    Returns a :class:`geocode.Near`, which names the order the wire format
    already uses, so the lon/lat pair cannot be read the wrong way round
    downstream.

    Only ever a ranking hint, so anything unparseable is dropped silently
    rather than failing the search it was meant to improve.
    """
    parts = str(raw or "").split(",")
    if len(parts) != 2:
        return None
    try:
        lon, lat = float(parts[0]), float(parts[1])
    except ValueError:
        return None
    if not math.isfinite(lon) or not math.isfinite(lat):
        return None
    if not -180.0 <= lon <= 180.0 or not -90.0 <= lat <= 90.0:
        return None
    return geocode.Near(lon, lat)


def _validate_tile_bounds(bounds: Any) -> tuple[str | None, tuple[float, float, float, float] | None]:
    """Validate a ``{w,s,e,n}`` bounds object.

    Returns ``(error, None)`` on failure or ``(None, (w,s,e,n))`` on success.
    """
    if not isinstance(bounds, dict):
        return ("bounds must be an object {w,s,e,n}", None)
    for key in ("w", "s", "e", "n"):
        value = bounds.get(key)
        # bool is a subclass of int — reject it so True is never coerced to 1.0
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(float(value)):
            return (f"bounds.{key} must be a finite number", None)
    w, s, e, n = float(bounds["w"]), float(bounds["s"]), float(bounds["e"]), float(bounds["n"])
    if not -180.0 <= w <= 180.0 or not -180.0 <= e <= 180.0:
        return ("bounds w/e must be in [-180, 180]", None)
    if not -90.0 <= s <= 90.0 or not -90.0 <= n <= 90.0:
        return ("bounds s/n must be in [-90, 90]", None)
    if w > e or s > n:
        return ("bounds must satisfy w<=e and s<=n", None)
    return (None, (w, s, e, n))


def _validate_tile_zooms(minzoom: Any, maxzoom: Any, src_maxzoom: int) -> str | None:
    """Validate ``minzoom``/``maxzoom`` (ints, 0..22, min<=max, within source cap)."""
    if isinstance(minzoom, bool) or not isinstance(minzoom, (int, float)):
        return "minzoom must be a number"
    if isinstance(maxzoom, bool) or not isinstance(maxzoom, (int, float)):
        return "maxzoom must be a number"
    # Range first: JSON reads 1e400 as inf, and int(inf) raises rather than
    # answering, which dropped the connection instead of a 400.
    if not -1 < minzoom < 23 or not -1 < maxzoom < 23:
        return "zooms must be in [0, 22]"
    if float(minzoom) != int(minzoom) or float(maxzoom) != int(maxzoom):
        return "zooms must be integers"
    mz, Mz = int(minzoom), int(maxzoom)
    if not 0 <= mz <= 22 or not 0 <= Mz <= 22:
        return "zooms must be in [0, 22]"
    if mz > Mz:
        return "minzoom must be <= maxzoom"
    if Mz > src_maxzoom:
        return f"maxzoom exceeds source max ({src_maxzoom})"
    return None


def _parse_takeoff_altitude(value: object) -> float:
    if isinstance(value, bool):
        raise ValueError("takeoff altitude must be a number")
    try:
        altitude = float(value)
    except (TypeError, ValueError) as exc:
        raise ValueError("takeoff altitude must be a number") from exc
    if not math.isfinite(altitude):
        raise ValueError("takeoff altitude must be finite")
    if not TAKEOFF_ALTITUDE_MIN_M <= altitude <= TAKEOFF_ALTITUDE_MAX_M:
        raise ValueError(
            f"takeoff altitude must be between {TAKEOFF_ALTITUDE_MIN_M:.0f} and "
            f"{TAKEOFF_ALTITUDE_MAX_M:.0f} m AGL"
        )
    return altitude


def _reject_json_constant(value: str) -> None:
    raise ValueError(f"non-finite JSON number: {value}")
