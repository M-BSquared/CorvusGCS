"""The mission plan: what it is, what makes one valid, and where it is kept.

The Mission page lets an operator draw a flight before it is flown — a start
point, waypoints, orbits, a landing — and this module is the one description of
what that drawing means. It sits between the frontend, which edits a plan, and
``mavlink_bridge``, which turns one into MISSION_ITEM_INT messages.

Three jobs, deliberately separate:

* :data:`ITEM_SPECS` states every item type once — its MAVLink command number,
  whether it carries a position, and which of the four command parameters its
  named fields land in. The frontend mirrors the *names*; this mirrors nothing.
  A per-item ``speed`` is deliberately not among them: it is a command of its
  own on the wire, not a parameter of the item it is set on. Nor is a per-item
  ``name``, which never reaches the wire at all: MISSION_ITEM_INT has no field
  for one, so it lives in the plan and in the saved file only.
* :func:`validate_plan` is the only gate. It is reached from an HTTP endpoint,
  so it treats every value as hostile: wrong types, NaN, infinities, out-of-
  range coordinates and oversized lists all come back as a message rather than
  an exception.
* :func:`plan_to_items` lowers a validated plan to a flat, wire-shaped list.
  It does not import pymavlink — the command numbers below are the frozen
  MAV_CMD values, and ``mavlink_bridge`` asserts them against ``mavutil`` so a
  drift would fail loudly there rather than fly quietly here.

The file store (:func:`list_plans` and friends) keeps saved plans as plain JSON
under ``~/.corvus/missions``. A plan is a document the operator may want to
copy onto another laptop, so it is readable, and the filename is derived from a
sanitized title rather than from anything a caller supplies verbatim.

stdlib only.
"""
from __future__ import annotations

import json
import logging
import math
import os
import pathlib
import re
import tempfile
from typing import Any

from .paths import corvus_path

logger = logging.getLogger("corvus.mission")

# Same ceiling the fly-to-points upload already enforces: PX4 will take more,
# but a plan that long is a sign of a runaway generator rather than a flight.
MISSION_MAX_ITEMS = 255

# Plan altitudes are metres above the home position, the frame every item below
# is uploaded in (MAV_FRAME_GLOBAL_RELATIVE_ALT). The bounds are wider than the
# 1-120 m the TAKEOFF command accepts on purpose: that limit guards a single
# button an operator presses while looking at the aircraft, whereas a plan is
# drawn, reviewed and uploaded before anything flies. Negative altitudes are
# allowed because home is not always the lowest ground on the route — a valley
# leg below the launch point is a real flight, not a typo.
MISSION_ALT_MIN_M = -100.0
MISSION_ALT_MAX_M = 1000.0

# Orbit geometry. A radius under a few metres is not a circle the aircraft can
# fly, and PX4 clamps it to NAV_LOITER_RAD anyway; the upper bound is the point
# past which "orbit" stops describing the path.
MISSION_RADIUS_MIN_M = 1.0
MISSION_RADIUS_MAX_M = 10000.0

# Cruise speed, when the plan pins one — and the same bounds for a speed
# pinned on a single item, which is the speed the leg INTO that item is flown
# at. 0 is not "unset" here — it is a real DO_CHANGE_SPEED value meaning "no
# change" — so an absent speed is None rather than zero, at both levels.
MISSION_SPEED_MIN_MS = 0.5
MISSION_SPEED_MAX_MS = 100.0

MISSION_HOLD_MAX_S = 3600.0
MISSION_TURNS_MAX = 100.0
MISSION_NAME_MAX = 64
# A point's own name. Short, because it is drawn next to the point on the map
# and in a list row that shares its width with the point's values.
MISSION_POINT_NAME_MAX = 40

# MAV_CMD values, frozen. Stated as literals so this module stays importable
# without pymavlink (the HTTP layer validates plans on machines where the
# MAVLink link may never open); mavlink_bridge checks each one against
# ``mavutil.mavlink`` before it sends anything.
MAV_CMD_NAV_WAYPOINT = 16
MAV_CMD_NAV_LOITER_TURNS = 18
MAV_CMD_NAV_LOITER_TIME = 19
MAV_CMD_NAV_RETURN_TO_LAUNCH = 20
MAV_CMD_NAV_LAND = 21
MAV_CMD_NAV_TAKEOFF = 22
MAV_CMD_DO_CHANGE_SPEED = 178

# One row per item type the planner can draw.
#
# ``params`` maps a plan field onto the MAVLink command parameter that carries
# it, plus the bounds and the default used when the field is absent. ``position``
# says whether the item owns a lat/lon/alt; ``rtl`` is the only one that does
# not, because "come home" names no place on the map. ``yaw_slot`` is the
# parameter PX4 reads as a heading, which is sent as NaN (no heading asked for).
#
# The field NAMES are the contract with src/js/mission.js. The parameter
# SLOTS are the contract with PX4 v1.16-v1.18, where all six commands below
# are accepted in a MISSION_ITEM_INT stream.
ITEM_SPECS: dict[str, dict[str, Any]] = {
    "takeoff": {
        "command": MAV_CMD_NAV_TAKEOFF,
        "position": True,
        "yaw_slot": 4,
        "label": "Takeoff",
        "params": {
            # Fixed-wing climb-out pitch. Zero means "use the airframe's own",
            # which is what a multirotor wants and what PX4 defaults to.
            "pitch": {"slot": 1, "min": 0.0, "max": 45.0, "default": 0.0},
        },
    },
    "waypoint": {
        "command": MAV_CMD_NAV_WAYPOINT,
        "position": True,
        "yaw_slot": 4,
        "label": "Waypoint",
        "params": {
            "hold": {"slot": 1, "min": 0.0, "max": MISSION_HOLD_MAX_S, "default": 0.0},
            # 0 = the airframe's NAV_ACC_RAD. Anything else is the operator
            # saying this particular turn has to be flown tighter or looser.
            "accept_radius": {"slot": 2, "min": 0.0, "max": 1000.0, "default": 0.0},
        },
    },
    "loiter_turns": {
        "command": MAV_CMD_NAV_LOITER_TURNS,
        "position": True,
        "label": "Circle",
        "params": {
            "turns": {"slot": 1, "min": 0.0, "max": MISSION_TURNS_MAX, "default": 1.0},
            "radius": {"slot": 3, "min": MISSION_RADIUS_MIN_M,
                       "max": MISSION_RADIUS_MAX_M, "default": 50.0},
            # Which way round. ``slot: None`` because it has no parameter of
            # its own — PX4 reads the SIGN of the radius, and plan_to_items
            # applies it there. The plan keeps a radius a length, because a
            # negative distance is not something an operator should have to
            # type to turn the other way.
            "direction": {"slot": None, "min": -1.0, "max": 1.0, "default": 1.0},
        },
    },
    "loiter_time": {
        "command": MAV_CMD_NAV_LOITER_TIME,
        "position": True,
        "label": "Hold",
        "params": {
            "seconds": {"slot": 1, "min": 0.0, "max": MISSION_HOLD_MAX_S, "default": 30.0},
            "radius": {"slot": 3, "min": MISSION_RADIUS_MIN_M,
                       "max": MISSION_RADIUS_MAX_M, "default": 50.0},
            "direction": {"slot": None, "min": -1.0, "max": 1.0, "default": 1.0},
        },
    },
    "land": {
        "command": MAV_CMD_NAV_LAND,
        "position": True,
        "yaw_slot": 4,
        "label": "Land",
        "params": {},
    },
    "rtl": {
        "command": MAV_CMD_NAV_RETURN_TO_LAUNCH,
        "position": False,
        "label": "Return",
        "params": {},
    },
}

ITEM_TYPES: tuple[str, ...] = tuple(ITEM_SPECS)

# The item types that orbit, and therefore have a direction to orbit in.
ORBIT_TYPES: tuple[str, ...] = ("loiter_turns", "loiter_time")

# Item types whose command number the MAVLink layer must agree with. Exported
# so mavlink_bridge can assert the pairing rather than restate it.
COMMAND_OF: dict[str, int] = {name: spec["command"] for name, spec in ITEM_SPECS.items()}

_SAFE_NAME_RE = re.compile(r"[^A-Za-z0-9 ._-]+")
# Control characters (C0, DEL, C1) and the line and paragraph separators. A
# point name is one line of text on a map; none of these can be part of one.
_POINT_NAME_JUNK_RE = re.compile(r"[\x00-\x1f\x7f-\x9f\u2028\u2029]+")

# WGS-84 mean radius. Plan distances are used for the profile's x-axis and the
# duration estimate, neither of which is navigation, so a spherical earth is
# more than accurate enough and costs no dependency.
EARTH_RADIUS_M = 6371008.8


def _finite(value: Any) -> float | None:
    """*value* as a float, or None when it is not a finite number.

    ``bool`` is rejected explicitly: it is a subclass of ``int``, so a JSON
    ``true`` would otherwise be accepted as an altitude of 1 m.
    """
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    number = float(value)
    if not math.isfinite(number):
        return None
    return number


def _bounded(value: Any, low: float, high: float) -> float | None:
    number = _finite(value)
    if number is None or not low <= number <= high:
        return None
    return number


def safe_plan_name(raw: Any) -> str:
    """A filename-safe mission name, or "" when *raw* cannot yield one.

    Everything outside the allowed set collapses to a single space, and the
    result is trimmed of leading dots so a name can never address a hidden
    file, a parent directory, or a device node. An empty answer is the caller's
    signal to refuse, never a reason to invent a name.
    """
    if not isinstance(raw, str):
        return ""
    cleaned = _SAFE_NAME_RE.sub(" ", raw).strip(" .")
    cleaned = re.sub(r"\s+", " ", cleaned)
    return cleaned[:MISSION_NAME_MAX].strip()


def clean_point_name(raw: Any) -> str:
    """A point's display name, or "" when *raw* carries none.

    Unlike a plan name this is never a filename, so it keeps any printable
    character, umlauts and all. Control characters become spaces, runs of
    whitespace collapse to one, and the result is capped at
    :data:`MISSION_POINT_NAME_MAX`. Anything that is not a string is no name
    rather than an error: the name is a label, and a plan must not be refused
    over one.
    """
    if not isinstance(raw, str):
        return ""
    cleaned = re.sub(r"\s+", " ", _POINT_NAME_JUNK_RE.sub(" ", raw)).strip()
    return cleaned[:MISSION_POINT_NAME_MAX].strip()


def missions_dir(configured: str = "") -> str:
    """Where saved plans live: *configured* when set, else ``~/.corvus/missions``."""
    return configured.strip() or corvus_path("missions")


def _validate_item(index: int, raw: Any) -> tuple[dict[str, Any] | None, str]:
    if not isinstance(raw, dict):
        return None, f"item {index} must be an object"
    kind = raw.get("type")
    if not isinstance(kind, str) or kind not in ITEM_SPECS:
        return None, f"item {index} has an unknown type {kind!r}"
    spec = ITEM_SPECS[kind]
    item: dict[str, Any] = {"type": kind}

    if spec["position"]:
        lat = _bounded(raw.get("lat"), -90.0, 90.0)
        if lat is None:
            return None, f"item {index} lat must be a number between -90 and 90"
        lon = _bounded(raw.get("lon"), -180.0, 180.0)
        if lon is None:
            return None, f"item {index} lon must be a number between -180 and 180"
        # A landing ends on the ground by definition, so its altitude is not
        # the operator's to set — accepting one would let a plan claim a
        # touchdown 40 m in the air.
        if kind == "land":
            alt = 0.0
        else:
            alt = _bounded(raw.get("alt"), MISSION_ALT_MIN_M, MISSION_ALT_MAX_M)
            if alt is None:
                return None, (
                    f"item {index} alt must be between {MISSION_ALT_MIN_M:.0f} and "
                    f"{MISSION_ALT_MAX_M:.0f} m above home"
                )
        item["lat"], item["lon"], item["alt"] = lat, lon, alt

    for field, rule in spec["params"].items():
        if raw.get(field) is None:
            item[field] = float(rule["default"])
            continue
        value = _bounded(raw.get(field), rule["min"], rule["max"])
        if value is None:
            return None, (
                f"item {index} {field} must be between "
                f"{rule['min']:g} and {rule['max']:g}"
            )
        item[field] = value

    # Direction is one of two things, not a range. It is bounds-checked above
    # like every other field and then snapped here, so a 0 (or a 0.5 from a
    # hand-edited file) resolves to a real direction rather than to a radius
    # of zero once the sign is applied.
    if kind in ORBIT_TYPES:
        item["direction"] = 1.0 if item["direction"] >= 0 else -1.0

    # A speed pinned on ONE item: how fast the leg INTO it is flown. Not in
    # ``params`` because it is not a parameter of this command at all — it
    # becomes a DO_CHANGE_SPEED of its own, immediately ahead of the item, and
    # :func:`plan_to_items` is where that happens. Absent means "carry on at
    # whatever speed is already set", which is not a number and therefore
    # cannot be given a default: the key simply stays off the item.
    speed = raw.get("speed")
    if speed is not None:
        value = _bounded(speed, MISSION_SPEED_MIN_MS, MISSION_SPEED_MAX_MS)
        if value is None:
            return None, (
                f"item {index} speed must be between {MISSION_SPEED_MIN_MS:g} "
                f"and {MISSION_SPEED_MAX_MS:g} m/s"
            )
        item["speed"] = value

    # Absent and empty are the same thing: the point is shown by its number
    # and its kind. The key stays off the item rather than carrying "".
    name = clean_point_name(raw.get("name"))
    if name:
        item["name"] = name

    return item, ""


def validate_plan(raw: Any) -> tuple[dict[str, Any] | None, str]:
    """Return a cleaned plan, or ``(None, reason)``.

    The cleaned plan carries only known keys with coerced values, so callers
    downstream — the uploader, the file store — never have to re-check
    anything. Ordering is preserved: a mission is a sequence, and item 3 coming
    after item 2 is the whole point.
    """
    if not isinstance(raw, dict):
        return None, "plan must be an object"

    items_raw = raw.get("items")
    if not isinstance(items_raw, list):
        return None, "plan items must be a list"
    if len(items_raw) > MISSION_MAX_ITEMS:
        return None, (
            f"too many items ({len(items_raw)}); the maximum is {MISSION_MAX_ITEMS}"
        )

    items: list[dict[str, Any]] = []
    for index, entry in enumerate(items_raw):
        item, error = _validate_item(index, entry)
        if item is None:
            return None, error
        items.append(item)

    plan: dict[str, Any] = {"version": 1, "items": items}

    name = safe_plan_name(raw.get("name"))
    plan["name"] = name or "Mission"

    # The planned launch point. Optional: a plan drawn before the aircraft has
    # a home is still a plan, and PX4 uses the vehicle's own home when the
    # mission does not name one.
    home_raw = raw.get("home")
    if isinstance(home_raw, dict):
        lat = _bounded(home_raw.get("lat"), -90.0, 90.0)
        lon = _bounded(home_raw.get("lon"), -180.0, 180.0)
        if lat is None or lon is None:
            return None, "plan home must carry a finite lat and lon"
        home: dict[str, Any] = {"lat": lat, "lon": lon}
        elevation = _finite(home_raw.get("elevation"))
        if elevation is not None:
            home["elevation"] = elevation
        plan["home"] = home
    elif home_raw is not None:
        return None, "plan home must be an object"

    speed = raw.get("speed")
    if speed is not None:
        value = _bounded(speed, MISSION_SPEED_MIN_MS, MISSION_SPEED_MAX_MS)
        if value is None:
            return None, (
                f"plan speed must be between {MISSION_SPEED_MIN_MS:g} and "
                f"{MISSION_SPEED_MAX_MS:g} m/s"
            )
        plan["speed"] = value

    # The ceiling is on what is UPLOADED, not on what was drawn. Every pinned
    # speed becomes a command of its own, so a plan can sit under the limit on
    # the page and go over it on the wire; counting the drawn items alone
    # would let that through and fail at the aircraft instead.
    wire = len(plan_to_items(plan))
    if wire > MISSION_MAX_ITEMS:
        return None, (
            f"too many items ({wire} once the speed changes are counted); "
            f"the maximum is {MISSION_MAX_ITEMS}"
        )

    return plan, ""


def _speed_item(speed: float, item: int) -> dict[str, Any]:
    """One DO_CHANGE_SPEED, positionless, in the shape the uploader wants."""
    return {
        "command": MAV_CMD_DO_CHANGE_SPEED,
        "lat": 0.0, "lon": 0.0, "alt": 0.0,
        # param1 = 1 (ground speed), param2 = the speed, param3 = -1 (no
        # throttle change). PX4 v1.16-v1.18 read exactly these.
        "params": [1.0, float(speed), -1.0, 0.0],
        "item": item,
    }


def plan_to_items(plan: dict[str, Any]) -> list[dict[str, Any]]:
    """Lower a validated plan to flat, wire-shaped mission items.

    Each entry is ``{"command", "lat", "lon", "alt", "params": [p1..p4],
    "item"}``. Positionless items (RTL, the speed change) carry zeros for
    lat/lon/alt, which is what PX4 expects for a command that names no place.
    ``item`` is the index of the plan item the entry belongs to: a speed change
    belongs to the item it is flown into. The uploader does not send it; it is
    how the vehicle's MISSION_CURRENT is read back as a place in the plan.

    SPEED is a command, not a field. PX4 holds whatever DO_CHANGE_SPEED it
    last executed, so a speed is emitted ahead of the first item it applies to
    and then simply persists:

    * The plan's own cruise speed goes FIRST, ahead of every navigation item —
      after the takeoff it would leave the climb-out at the airframe default
      and make the plan's speed a half-truth.
    * An item that pins a speed of its own gets one immediately before it, so
      the change takes effect for the leg INTO that item, which is the leg the
      operator set it on.

    A pinned speed equal to the one already in force emits nothing: the
    aircraft is flying at it, and a second identical command would only spend
    a mission slot to say so.
    """
    items: list[dict[str, Any]] = []
    current: float | None = None

    speed = plan.get("speed")
    if isinstance(speed, (int, float)) and not isinstance(speed, bool):
        current = float(speed)
        items.append(_speed_item(current, 0))

    for index, entry in enumerate(plan.get("items", [])):
        pinned = entry.get("speed")
        if (isinstance(pinned, (int, float)) and not isinstance(pinned, bool)
                and float(pinned) != current):
            current = float(pinned)
            items.append(_speed_item(current, index))
        spec = ITEM_SPECS[entry["type"]]
        params = [0.0, 0.0, 0.0, 0.0]
        # The yaw slot is NaN, "keep the airframe's heading behaviour". 0 is a
        # real yaw, north, and PX4 turns the aircraft to face it.
        if spec.get("yaw_slot"):
            params[spec["yaw_slot"] - 1] = float("nan")
        for field, rule in spec["params"].items():
            if rule["slot"] is None:
                continue
            params[rule["slot"] - 1] = float(entry.get(field, rule["default"]))
        # PX4 v1.16-v1.18 read the sign of MAV_CMD_NAV_LOITER_*'s param3 as
        # the turn direction: positive clockwise, negative counter-clockwise.
        if entry["type"] in ORBIT_TYPES:
            params[2] = math.copysign(params[2], entry.get("direction", 1.0))
        items.append({
            "command": spec["command"],
            "lat": float(entry.get("lat", 0.0)),
            "lon": float(entry.get("lon", 0.0)),
            "alt": float(entry.get("alt", 0.0)),
            "params": params,
            "item": index,
        })

    return items


# MAV_FRAME values a downloaded item can carry its altitude in. Relative to
# home is the plan's own frame; above sea level converts once home's altitude
# is known; above terrain has no single answer without the ground under it.
_FRAMES_RELATIVE = frozenset({3, 6})         # GLOBAL_RELATIVE_ALT(_INT)
_FRAMES_AMSL = frozenset({0, 5})             # GLOBAL(_INT)
_FRAMES_TERRAIN = frozenset({10, 11})        # GLOBAL_TERRAIN_ALT(_INT)

_TYPE_OF_COMMAND: dict[int, str] = {spec["command"]: name for name, spec in ITEM_SPECS.items()}


def _param(values: list[Any], slot: int) -> float | None:
    """One command parameter as a finite float, or None (absent or NaN)."""
    if not 1 <= slot <= len(values):
        return None
    return _finite(values[slot - 1])


def items_to_plan(
    items: list[dict[str, Any]],
    home: dict[str, float] | None = None,
    home_alt_amsl: float | None = None,
    name: str = "Vehicle mission",
) -> dict[str, Any]:
    """Read a mission downloaded from the vehicle back into a plan.

    *items* are the vehicle's, numbered as the plan numbers them (ArduPilot's
    home slot already removed), each ``{"command", "frame", "lat", "lon",
    "alt", "params"}``. *home* (``{"lat", "lon"}``) stands in for a takeoff or
    landing that names no place, and *home_alt_amsl* converts an item written
    above sea level into the plan's height above home.

    Returns ``{"plan", "plan_index", "skipped", "adjusted", "error"}``:

    ``plan``
        A validated plan, or None when even the representable part is not one
        (``error`` says why).
    ``plan_index``
        One entry per vehicle item: the plan item it became, or None. This is
        how MISSION_CURRENT is shown as a place in the plan.
    ``skipped``
        Items the planner has no way to draw, with the reason: a command it
        does not know, or an altitude above terrain. Not dropped quietly: an
        upload of the plan replaces the whole mission on the vehicle, these
        included, and the operator has to know that before pressing it.
    ``adjusted``
        Values the planner had to change to hold them (a takeoff with no
        position, a hold longer than the planner allows).

    Speed changes are folded back the way :func:`plan_to_items` wrote them:
    one ahead of every navigation item is the plan's speed, one between items
    is the speed of the item after it.
    """
    plan_items: list[dict[str, Any]] = []
    plan_index: list[int | None] = []
    skipped: list[dict[str, Any]] = []
    adjusted: list[dict[str, Any]] = []
    plan_speed: float | None = None
    pending_speed: float | None = None
    # Speed items waiting for the navigation item they belong to.
    pending_slots: list[int] = []
    last_position: tuple[float, float] | None = None

    for seq, raw in enumerate(items or []):
        plan_index.append(None)
        try:
            command = int(raw.get("command"))
        except (TypeError, ValueError):
            skipped.append({"seq": seq, "command": None, "reason": "no command"})
            continue
        values = list(raw.get("params") or [])

        if command == MAV_CMD_DO_CHANGE_SPEED:
            speed = _param(values, 2)
            if speed is None or speed <= 0:
                # -1 and 0 are "no change": nothing for the plan to hold.
                pending_slots.append(seq)
                continue
            speed = min(max(speed, MISSION_SPEED_MIN_MS), MISSION_SPEED_MAX_MS)
            if not plan_items and plan_speed is None and pending_speed is None:
                plan_speed = speed
            else:
                pending_speed = speed
            pending_slots.append(seq)
            continue

        kind = _TYPE_OF_COMMAND.get(command)
        if kind is None:
            skipped.append({"seq": seq, "command": command,
                            "reason": f"command {command} is not one the planner draws"})
            continue
        spec = ITEM_SPECS[kind]
        entry: dict[str, Any] = {"type": kind}

        if spec["position"]:
            frame = int(raw.get("frame", 3) or 0)
            lat = _finite(raw.get("lat"))
            lon = _finite(raw.get("lon"))
            if lat is None or lon is None or (lat == 0.0 and lon == 0.0):
                # "Here": a takeoff or a landing written without a place, the
                # way ArduPilot plans them. The plan needs a point to draw.
                fallback = last_position or (
                    (float(home["lat"]), float(home["lon"])) if home else None)
                if fallback is None:
                    skipped.append({"seq": seq, "command": command,
                                    "reason": "no position, and no home to put it at"})
                    continue
                lat, lon = fallback
                adjusted.append({"seq": seq, "field": "position",
                                 "reason": "the vehicle's item names no place; drawn at "
                                           + ("the previous point" if last_position else "home")})
            alt = _finite(raw.get("alt")) or 0.0
            if frame in _FRAMES_TERRAIN:
                skipped.append({"seq": seq, "command": command,
                                "reason": "its altitude is above terrain, not above home"})
                continue
            if frame in _FRAMES_AMSL:
                if home_alt_amsl is None:
                    skipped.append({"seq": seq, "command": command,
                                    "reason": "its altitude is above sea level and "
                                              "home's is not known yet"})
                    continue
                alt -= float(home_alt_amsl)
            elif frame not in _FRAMES_RELATIVE:
                skipped.append({"seq": seq, "command": command,
                                "reason": f"frame {frame} is not a map position"})
                continue
            if kind != "land":
                bounded = min(max(alt, MISSION_ALT_MIN_M), MISSION_ALT_MAX_M)
                if bounded != alt:
                    adjusted.append({"seq": seq, "field": "alt",
                                     "reason": f"{alt:.0f} m is outside the planner's range"})
                entry["alt"] = bounded
            else:
                entry["alt"] = 0.0
            entry["lat"], entry["lon"] = lat, lon
            last_position = (lat, lon)

        for field, rule in spec["params"].items():
            if rule["slot"] is None:
                continue
            value = _param(values, rule["slot"])
            if value is None:
                continue
            if kind in ORBIT_TYPES and field == "radius":
                entry["direction"] = -1.0 if value < 0 else 1.0
                value = abs(value)
                if value == 0.0:
                    # 0 is "the airframe's own radius" on the vehicle; the
                    # planner has no such value, so its default stands in.
                    continue
            bounded = min(max(value, rule["min"]), rule["max"])
            if bounded != value:
                adjusted.append({"seq": seq, "field": field,
                                 "reason": f"{value:g} is outside the planner's range"})
            entry[field] = bounded

        if pending_speed is not None:
            entry["speed"] = pending_speed
            pending_speed = None
        index = len(plan_items)
        plan_items.append(entry)
        plan_index[seq] = index
        for slot in pending_slots:
            plan_index[slot] = index
        pending_slots = []

    raw_plan: dict[str, Any] = {"name": name, "items": plan_items}
    if home:
        raw_plan["home"] = {"lat": home["lat"], "lon": home["lon"]}
    if plan_speed is not None:
        raw_plan["speed"] = plan_speed
    plan, error = validate_plan(raw_plan)
    return {
        "plan": plan, "plan_index": plan_index,
        "skipped": skipped, "adjusted": adjusted, "error": error,
    }


def leg_distance_m(a: dict[str, Any], b: dict[str, Any]) -> float:
    """Great-circle distance in metres between two ``{"lat","lon"}`` points."""
    lat1 = math.radians(float(a["lat"]))
    lat2 = math.radians(float(b["lat"]))
    dlat = lat2 - lat1
    dlon = math.radians(float(b["lon"]) - float(a["lon"]))
    h = (math.sin(dlat / 2) ** 2
         + math.cos(lat1) * math.cos(lat2) * math.sin(dlon / 2) ** 2)
    return 2.0 * EARTH_RADIUS_M * math.asin(min(1.0, math.sqrt(h)))


def plan_distance_m(plan: dict[str, Any]) -> float:
    """Ground distance flown along a plan, home included when it names one.

    Orbits add their own circumference: a three-turn circle is most of the
    flight time at that point, and a summary that ignored it would under-report
    a plan by minutes.
    """
    points: list[dict[str, Any]] = []
    home = plan.get("home")
    if isinstance(home, dict):
        points.append(home)
    orbit = 0.0
    for entry in plan.get("items", []):
        if not ITEM_SPECS[entry["type"]]["position"]:
            continue
        points.append(entry)
        if entry["type"] == "loiter_turns":
            orbit += 2.0 * math.pi * float(entry.get("radius", 0.0)) \
                * float(entry.get("turns", 0.0))
    total = sum(leg_distance_m(points[i], points[i + 1]) for i in range(len(points) - 1))
    return total + orbit


# ---------------------------------------------------------------------------
# File store
# ---------------------------------------------------------------------------

def _plan_path(directory: str, name: str) -> str:
    return os.path.join(directory, f"{name}.json")


def list_plans(directory: str) -> list[dict[str, Any]]:
    """Saved plans in *directory*, newest first.

    A directory that does not exist is an empty list, not an error: nothing has
    been saved yet is the normal state of a fresh install. A file that will not
    parse is skipped with a warning rather than failing the whole listing — one
    corrupt plan must not hide the others.
    """
    try:
        entries = sorted(pathlib.Path(directory).glob("*.json"))
    except OSError:
        return []
    plans: list[dict[str, Any]] = []
    for path in entries:
        try:
            stat = path.stat()
            with path.open("r", encoding="utf-8") as handle:
                data = json.load(handle)
        except (OSError, ValueError) as exc:
            logger.warning("skipping unreadable mission %s: %s", path.name, exc)
            continue
        plan, error = validate_plan(data)
        if plan is None:
            logger.warning("skipping invalid mission %s: %s", path.name, error)
            continue
        plans.append({
            "name": path.stem,
            "title": plan["name"],
            "items": len(plan["items"]),
            "distance_m": round(plan_distance_m(plan), 1),
            "modified": stat.st_mtime,
        })
    plans.sort(key=lambda entry: entry["modified"], reverse=True)
    return plans


def read_plan(directory: str, name: str) -> dict[str, Any] | None:
    """Load and validate one saved plan, or None when it cannot be read."""
    safe = safe_plan_name(name)
    if not safe:
        return None
    try:
        with open(_plan_path(directory, safe), encoding="utf-8") as handle:
            data = json.load(handle)
    except (OSError, ValueError) as exc:
        logger.warning("could not read mission %r: %s", safe, exc)
        return None
    plan, error = validate_plan(data)
    if plan is None:
        logger.warning("saved mission %r is invalid: %s", safe, error)
        return None
    return plan


def write_plan(directory: str, name: str, plan: dict[str, Any]) -> str:
    """Save *plan* as *name*, atomically. Returns the stored name.

    Atomic for the same reason the config is: the field laptop loses power, and
    a half-written plan that still parses is worse than no plan at all — it
    would upload a route that stops somewhere in the middle.
    """
    safe = safe_plan_name(name) or safe_plan_name(plan.get("name")) or "Mission"
    os.makedirs(directory, exist_ok=True)
    target = _plan_path(directory, safe)
    handle_fd, temp_path = tempfile.mkstemp(dir=directory, prefix=".mission-", suffix=".json")
    try:
        with os.fdopen(handle_fd, "w", encoding="utf-8") as handle:
            json.dump(plan, handle, indent=2, sort_keys=False)
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
    return safe


def remove_plan(directory: str, name: str) -> bool:
    """Delete one saved plan. Removing what is not there is a success."""
    safe = safe_plan_name(name)
    if not safe:
        return False
    try:
        os.unlink(_plan_path(directory, safe))
    except FileNotFoundError:
        return True
    except OSError as exc:
        logger.warning("could not delete mission %r: %s", safe, exc)
        return False
    return True
