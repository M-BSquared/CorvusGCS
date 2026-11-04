"""The MCP server: an AI client reads and flies through Corvus GCS.

The Model Context Protocol is how a coding or chat agent (OpenCode, Claude
Code, Cursor) is handed tools. Corvus answers it at ``POST /mcp`` on its own
HTTP port, as the protocol's Streamable HTTP transport: one JSON-RPC message
in, one JSON reply out, and an optional ``GET /mcp`` event stream that tells a
client when the tool list changed. Nothing new listens anywhere; the protocol
is a few hundred lines of JSON, so it is written here on the standard library
rather than pulling in the SDK and the web framework it brings.

Three rules decide what an agent may do, and all three are the operator's:

* **Off by default.** The server answers nothing until the operator switches
  it on (Settings, Connections, MCP). Only this computer may connect, and only
  with the token shown there.
* **One switch per kind of act** (:data:`PERMISSIONS`). Reading is on once the
  server is; everything that changes the vehicle is off until it is switched
  on, and the ones that make it move start out asking the operator first. A
  tool whose permission is off is not even listed.
* **Through the station's own endpoints.** A tool never talks to the link. It
  calls the same handler the operator's buttons call (``dispatch``), so an
  agent gets the same validation, the same PX4 and ArduPilot sequences and the
  same refusals, and nothing here needs to know which stack is connected.

Direct control (sticks, velocity) is the one place that is not a single call:
a vehicle flown that way has to be fed setpoints several times a second, and a
language model answers in seconds. So a control call names how long it lasts
(at most :data:`CONTROL_MAX_S`), this module repeats it at
:data:`CONTROL_RATE_HZ`, and when no new one has come in by then the vehicle is
put in hold. An agent that goes quiet stops the aircraft.

Plugins add tools from the browser (``Corvus.plugins``, ``mcpTools``): the
window announces them every few seconds, a call is handed to it over the
``mcp`` event topic, and its answer comes back on ``POST /api/mcp/result``.
Without an open window there is no plugin tool, which is what the operator
agreed to.

A limit worth stating plainly: these permissions govern the MCP tools. An
agent that can also run shell commands on this computer can reach the HTTP API
directly, as any local program can. The permissions are the operator's
instructions to a cooperative agent, enforced on the channel it was given, not
a sandbox.
"""
from __future__ import annotations

import dataclasses
import hmac
import json
import logging
import os
import pathlib
import queue
import re
import secrets
import threading
import time
from typing import Any
from collections.abc import Callable
from urllib.parse import quote

from .version import get_version

logger = logging.getLogger("corvus.mcp")

SERVER_NAME = "corvus-gcs"
SERVER_TITLE = "Corvus GCS"

# Newest first. A client asking for one of these gets it back; anything else
# gets DEFAULT_PROTOCOL, and the client decides whether it can live with that.
PROTOCOL_VERSIONS: tuple[str, ...] = ("2025-11-25", "2025-06-18", "2025-03-26")
DEFAULT_PROTOCOL = "2025-06-18"

# How long a tool waits for the operator to answer, under the 60 s most
# clients give a tool call before they give up on it.
CONFIRM_TIMEOUT_S = 45.0
# How long a plugin has to answer a call handed to its window.
PLUGIN_CALL_TIMEOUT_S = 30.0
# A window that has not announced itself for this long is gone, and its
# plugin tools with it. It announces every 10 s.
WINDOW_TTL_S = 25.0
# A client seen within this window counts as connected for the indicator.
CLIENT_ACTIVE_S = 120.0

CONTROL_RATE_HZ = 5.0
CONTROL_DEFAULT_S = 1.0
CONTROL_MAX_S = 5.0

MAX_SESSIONS = 16
MAX_PLUGIN_TOOLS = 64
MAX_SCHEMA_BYTES = 8192
MAX_DESCRIPTION = 1000
MAX_PARAMS_LISTED = 300
STREAM_QUEUE = 32

JSONRPC_PARSE_ERROR = -32700
JSONRPC_INVALID_REQUEST = -32600
JSONRPC_METHOD_NOT_FOUND = -32601
JSONRPC_INVALID_PARAMS = -32602
JSONRPC_INTERNAL_ERROR = -32603

_TOOL_NAME_RE = re.compile(r"^[A-Za-z0-9_-]{1,48}$")
_PLUGIN_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
_WINDOW_ID_RE = re.compile(r"^[A-Za-z0-9_-]{1,64}$")

WHERE = "Settings, Connections, MCP"


@dataclasses.dataclass(frozen=True)
class Permission:
    """One switch on the MCP card.

    ``writes`` is whether it changes anything; only those have an "ask the
    operator first" switch, and only those are taken away by Stop.
    """

    id: str
    label: str
    hint: str
    writes: bool
    default_on: bool
    default_confirm: bool


PERMISSIONS: tuple[Permission, ...] = (
    Permission("telemetry", "Read telemetry and status",
               "Position, attitude, battery, flight mode, warnings, the vehicle list "
               "and the mission on board.", False, True, False),
    Permission("params_read", "Read parameters",
               "The parameter list the vehicle reported, and a fresh download of it.",
               False, True, False),
    Permission("params_write", "Change parameters",
               "Write one parameter at a time. Only names the connected firmware "
               "reported are accepted.", True, False, True),
    Permission("mission", "Missions",
               "Upload, start and clear a mission. Starting a mission arms the vehicle.",
               True, False, True),
    Permission("arm", "Arm and disarm",
               "Disarming in the air stops the motors.", True, False, True),
    Permission("flight", "Takeoff, land, return, hold and flight mode",
               "Also moves the home position.", True, False, True),
    Permission("goto", "Fly to a point",
               "Fly to one point or through several, at a height above home.",
               True, False, False),
    Permission("control", "Direct control",
               "Stick inputs and velocity setpoints. Each lasts at most "
               f"{CONTROL_MAX_S:g} s; without a new one the vehicle holds.",
               True, False, True),
    Permission("command", "Other MAVLink commands",
               "One command from the list the capabilities name. Arming, takeoff, "
               "mode changes, calibration and flight termination are never on it.",
               True, False, True),
    Permission("plugins", "Plugin tools",
               "Tools the installed plugins add, for the plugins switched on below. "
               "Each plugin has its own switches too.", True, False, False),
)

PERMISSION_IDS: tuple[str, ...] = tuple(p.id for p in PERMISSIONS)
_BY_ID: dict[str, Permission] = {p.id: p for p in PERMISSIONS}


class ToolError(Exception):
    """A tool that ran and failed: told to the agent, not raised as a protocol error."""


# ---------------------------------------------------------------- settings

def coerce_settings(raw: Any) -> dict[str, Any] | None:
    """The ``mcp`` block of the config file, keeping only what is well formed.

    Booleans only, for the reason the forwarding keys are: a config carrying
    ``"true"`` must read as "not set", never as an accidental yes to flying.
    """
    if not isinstance(raw, dict):
        return None
    out: dict[str, Any] = {}
    for key in ("enabled", "fleet"):
        if isinstance(raw.get(key), bool):
            out[key] = raw[key]
    for key in ("permissions", "confirm"):
        block = raw.get(key)
        if isinstance(block, dict):
            kept = {k: v for k, v in block.items() if k in _BY_ID and isinstance(v, bool)}
            if kept:
                out[key] = kept
    plugins = raw.get("plugins")
    if isinstance(plugins, dict):
        kept = {k: v for k, v in plugins.items()
                if isinstance(k, str) and _PLUGIN_ID_RE.match(k) and isinstance(v, bool)}
        if kept:
            out["plugins"] = kept
    return out or None


def effective_settings(raw: Any) -> dict[str, Any]:
    """The settings in force: what the file says, the defaults for the rest."""
    saved = coerce_settings(raw) or {}
    perms = saved.get("permissions", {})
    confirm = saved.get("confirm", {})
    return {
        "enabled": saved.get("enabled", False),
        "fleet": saved.get("fleet", False),
        "permissions": {p.id: perms.get(p.id, p.default_on) for p in PERMISSIONS},
        "confirm": {p.id: (confirm.get(p.id, p.default_confirm) if p.writes else False)
                    for p in PERMISSIONS},
        "plugins": dict(saved.get("plugins", {})),
    }


def compact_settings(settings: dict[str, Any]) -> dict[str, Any] | None:
    """*settings* without what equals the defaults, for a lean config file."""
    defaults = effective_settings(None)
    out: dict[str, Any] = {}
    for key in ("enabled", "fleet"):
        if settings.get(key) != defaults[key]:
            out[key] = settings[key]
    for key in ("permissions", "confirm"):
        changed = {k: v for k, v in settings.get(key, {}).items() if defaults[key].get(k) != v}
        if changed:
            out[key] = changed
    if settings.get("plugins"):
        out["plugins"] = dict(settings["plugins"])
    return coerce_settings(out)


def level(settings: dict[str, Any], permission: str) -> str:
    """``"off"``, ``"confirm"`` or ``"allow"`` for one permission."""
    if not settings.get("permissions", {}).get(permission):
        return "off"
    if settings.get("confirm", {}).get(permission):
        return "confirm"
    return "allow"


def plugin_allowed(settings: dict[str, Any], plugin_id: str) -> bool:
    """Whether a plugin's tools are offered: the plugin switch, default on."""
    return settings.get("plugins", {}).get(plugin_id, True) is not False


# ------------------------------------------------------------------- tools

@dataclasses.dataclass(frozen=True)
class Tool:
    """One built-in tool.

    ``run(service, args)`` returns the JSON-able result or raises ToolError.
    ``summary(args)`` is the line the console and the confirmation show.
    ``vehicle`` is whether it may be pointed at another vehicle of the fleet.
    """

    name: str
    permission: str
    title: str
    description: str
    properties: dict[str, Any]
    required: tuple[str, ...]
    run: Callable[[McpService, dict[str, Any]], Any]
    summary: Callable[[dict[str, Any]], str]
    vehicle: bool = True
    read_only: bool = False


def _num(args: dict[str, Any], key: str, default: float | None = None) -> float:
    value = args.get(key, default)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ToolError(f"{key} must be a number")
    return float(value)


def _str(args: dict[str, Any], key: str) -> str:
    value = args.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ToolError(f"{key} must be a non-empty string")
    return value.strip()


def _duration(args: dict[str, Any]) -> float:
    seconds = _num(args, "duration_s", CONTROL_DEFAULT_S)
    if not 0.2 <= seconds <= CONTROL_MAX_S:
        raise ToolError(f"duration_s must be between 0.2 and {CONTROL_MAX_S:g}")
    return seconds


def _params_or_hint(svc: McpService) -> list[dict[str, Any]]:
    body = svc.api("GET", "/api/params")
    if not body.get("complete"):
        raise ToolError(
            f"The parameter list is not loaded (state {body.get('state', 'idle')}, "
            f"{body.get('received', 0)} of {body.get('count', 0)}). Call "
            "download_parameters and try again in a few seconds.")
    return list(body.get("params") or [])


def _run_list_parameters(svc: McpService, args: dict[str, Any]) -> Any:
    params = _params_or_hint(svc)
    prefix = args.get("prefix")
    if prefix is not None:
        if not isinstance(prefix, str):
            raise ToolError("prefix must be a string")
        params = [p for p in params if str(p.get("name", "")).upper().startswith(prefix.upper())]
    total = len(params)
    return {"count": total, "truncated": total > MAX_PARAMS_LISTED,
            "params": params[:MAX_PARAMS_LISTED]}


def _run_get_parameters(svc: McpService, args: dict[str, Any]) -> Any:
    names = args.get("names")
    if not isinstance(names, list) or not names or not all(isinstance(n, str) for n in names):
        raise ToolError("names must be a non-empty list of parameter names")
    by_name = {str(p.get("name")): p for p in _params_or_hint(svc)}
    found = [by_name[n] for n in names if n in by_name]
    missing = [n for n in names if n not in by_name]
    return {"params": found, "missing": missing}


def _run_set_parameter(svc: McpService, args: dict[str, Any]) -> Any:
    name = _str(args, "name")
    value = _num(args, "value")
    known = {str(p.get("name")) for p in _params_or_hint(svc)}
    if name not in known:
        raise ToolError(f"{name} is not a parameter the connected firmware reported.")
    svc.api("POST", "/api/params/set", {"name": name, "value": value})
    return {"ok": True, "name": name, "value": value}


def _run_fly_to(svc: McpService, args: dict[str, Any]) -> Any:
    points = args.get("points")
    if not isinstance(points, list) or not points:
        raise ToolError("points must be a non-empty list")
    wire = []
    for index, p in enumerate(points):
        if not isinstance(p, dict):
            raise ToolError(f"point {index} must be an object")
        item = {"lat": p.get("lat"), "lon": p.get("lon")}
        if p.get("alt") is not None:
            item["alt_agl"] = p.get("alt")
        wire.append(item)
    return svc.api("POST", "/api/mavlink/gotopoints", {"points": wire}, vehicle=args.get("vehicle"))


def _run_command(svc: McpService, args: dict[str, Any]) -> Any:
    body: dict[str, Any] = {"command": _str(args, "command")}
    params = args.get("params", [])
    if not isinstance(params, list):
        raise ToolError("params must be a list of numbers")
    body["params"] = params
    for key in ("lat", "lon", "alt", "frame"):
        if args.get(key) is not None:
            body[key] = args[key]
    return svc.api("POST", "/api/mavlink/command", body, vehicle=args.get("vehicle"))


def _post(path: str, body: Callable[[dict[str, Any]], dict[str, Any]] | None = None,
          ) -> Callable[[McpService, dict[str, Any]], Any]:
    def run(svc: McpService, args: dict[str, Any]) -> Any:
        return svc.api("POST", path, body(args) if body else {}, vehicle=args.get("vehicle"))
    return run


def _get(path: str, key: str | None = None) -> Callable[[McpService, dict[str, Any]], Any]:
    def run(svc: McpService, args: dict[str, Any]) -> Any:
        out = svc.api("GET", path, vehicle=args.get("vehicle"))
        return out.get(key) if key else out
    return run


_LATLON = {
    "lat": {"type": "number", "minimum": -90, "maximum": 90, "description": "Latitude, degrees"},
    "lon": {"type": "number", "minimum": -180, "maximum": 180, "description": "Longitude, degrees"},
}

_PLAN_SCHEMA = {
    "type": "object",
    "description": (
        "A Corvus mission plan. items is the route in order. Each item has a type: "
        "takeoff, waypoint, loiter_turns, loiter_time, land or rtl. Every type but rtl "
        "has lat, lon and alt (metres above home; land ignores alt). Optional fields: "
        "waypoint hold (s) and accept_radius (m); loiter_turns turns, radius (m) and "
        "direction (1 clockwise, -1 counterclockwise); loiter_time seconds, radius and "
        "direction; takeoff pitch (degrees). Plan level: name, speed (m/s)."),
    "properties": {
        "name": {"type": "string"},
        "speed": {"type": "number"},
        "items": {"type": "array", "items": {"type": "object"}},
    },
    "required": ["items"],
}


def builtin_tools() -> tuple[Tool, ...]:
    """Every tool Corvus itself offers, in the order a client lists them."""
    def said(text: str) -> Callable[[dict[str, Any]], str]:
        return lambda _a: text

    return (
        Tool("get_vehicle_state", "telemetry", "Vehicle state",
             "The live state of the vehicle: position, altitudes (amsl, agl), attitude, "
             "speeds, battery, GPS, flight mode, armed, landed state, home, mission "
             "progress, warnings and pre-arm reasons.",
             {}, (), _get("/api/state"), said("read the vehicle state"), read_only=True),
        Tool("list_vehicles", "telemetry", "Vehicles",
             "Every vehicle Corvus hears, its links, and which one the operator flies. "
             "The ids are what the vehicle argument of other tools takes.",
             {}, (), _get("/api/fleet"), said("list the vehicles"), vehicle=False, read_only=True),
        Tool("get_capabilities", "telemetry", "Capabilities",
             "What the connected autopilot can do through Corvus: the stack (PX4 or "
             "ArduPilot), hold and velocity modes, and the commands send_command accepts.",
             {}, (), _get("/api/mavlink/capabilities"), said("read the capabilities"),
             read_only=True),
        Tool("list_flight_modes", "telemetry", "Flight modes",
             "The flight mode names the connected firmware accepts, for set_flight_mode.",
             {}, (), _get("/api/mavlink/modes", "modes"), said("list the flight modes"),
             read_only=True),
        Tool("download_mission", "telemetry", "Mission on board",
             "Read the mission the vehicle holds, as a Corvus plan.",
             {}, (), _post("/api/mission/download"), said("read the mission on board"),
             vehicle=False, read_only=True),
        Tool("list_parameters", "params_read", "List parameters",
             f"The parameters the vehicle reported, name, value and type, at most "
             f"{MAX_PARAMS_LISTED} per call. Narrow it with a name prefix such as MPC_.",
             {"prefix": {"type": "string", "description": "Only names starting with this"}},
             (), _run_list_parameters, said("list parameters"), vehicle=False, read_only=True),
        Tool("get_parameters", "params_read", "Read parameters",
             "The current values of the named parameters.",
             {"names": {"type": "array", "items": {"type": "string"}}}, ("names",),
             _run_get_parameters,
             lambda a: f"read {', '.join(map(str, a.get('names') or []))[:120]}",
             vehicle=False, read_only=True),
        Tool("download_parameters", "params_read", "Reload parameters",
             "Ask the vehicle for its full parameter list again. Takes a few seconds; "
             "list_parameters answers once it is complete.",
             {}, (), _post("/api/params/download"), said("download the parameters"),
             vehicle=False, read_only=True),
        Tool("set_parameter", "params_write", "Change a parameter",
             "Write one parameter. The name must be one the vehicle reported.",
             {"name": {"type": "string"}, "value": {"type": "number"}}, ("name", "value"),
             _run_set_parameter, lambda a: f"set {a.get('name')} to {a.get('value')}",
             vehicle=False),
        Tool("upload_mission", "mission", "Upload a mission",
             "Upload a mission plan to the vehicle. With start true it is started too, "
             "which arms the vehicle.",
             {"plan": _PLAN_SCHEMA, "start": {"type": "boolean", "default": False}},
             ("plan",),
             _post("/api/mission/upload",
                   lambda a: {"plan": a.get("plan"), "start": a.get("start") is True}),
             lambda a: ("upload and start a mission of "
                        if a.get("start") is True else "upload a mission of ")
             + f"{len((a.get('plan') or {}).get('items') or [])} items"),
        Tool("start_mission", "mission", "Start the mission",
             "Run the mission already on the vehicle. items is how many items it has. "
             "This arms the vehicle.",
             {"items": {"type": "integer", "minimum": 1}}, ("items",),
             _post("/api/mission/start", lambda a: {"items": a.get("items")}),
             said("start the mission")),
        Tool("clear_mission", "mission", "Clear the mission",
             "Delete the mission stored on the vehicle.",
             {}, (), _post("/api/mission/clear"), said("clear the mission")),
        Tool("arm", "arm", "Arm", "Arm the motors.",
             {}, (), _post("/api/mavlink/arm", lambda _a: {"arm": True}), said("arm")),
        Tool("disarm", "arm", "Disarm",
             "Disarm the motors. In the air this stops them.",
             {}, (), _post("/api/mavlink/arm", lambda _a: {"arm": False}), said("disarm")),
        Tool("takeoff", "flight", "Take off",
             "Take off to an altitude above home, in metres. Corvus arms the vehicle in "
             "the order the autopilot needs.",
             {"altitude": {"type": "number", "description": "Metres above home"}},
             ("altitude",),
             _post("/api/mavlink/takeoff", lambda a: {"altitude": a.get("altitude")}),
             lambda a: f"take off to {a.get('altitude')} m"),
        Tool("land", "flight", "Land", "Land where the vehicle is.",
             {}, (), _post("/api/mavlink/land"), said("land")),
        Tool("return_to_launch", "flight", "Return",
             "Fly back to home and land there.",
             {}, (), _post("/api/mavlink/rtl"), said("return to launch")),
        Tool("hold", "flight", "Hold",
             "Stop where the vehicle is and keep it there.",
             {}, (), _post("/api/mavlink/hold"), said("hold")),
        Tool("set_flight_mode", "flight", "Flight mode",
             "Switch to a flight mode named by list_flight_modes.",
             {"mode": {"type": "string"}}, ("mode",),
             _post("/api/mavlink/mode", lambda a: {"mode": a.get("mode")}),
             lambda a: f"mode {a.get('mode')}"),
        Tool("set_home", "flight", "Set home",
             "Move the home position, where return flies to.",
             dict(_LATLON), ("lat", "lon"),
             _post("/api/mavlink/sethome", lambda a: {"lat": a.get("lat"), "lon": a.get("lon")}),
             lambda a: f"set home to {a.get('lat')}, {a.get('lon')}"),
        Tool("fly_to", "goto", "Fly to",
             "Fly to one point, or through several in order. alt is metres above home; "
             "a rover or boat ignores it.",
             {"points": {"type": "array", "minItems": 1, "items": {
                 "type": "object",
                 "properties": dict(_LATLON, alt={"type": "number",
                                                  "description": "Metres above home"}),
                 "required": ["lat", "lon"]}}},
             ("points",), _run_fly_to,
             lambda a: (f"fly to {a['points'][0].get('lat')}, {a['points'][0].get('lon')}"
                        if isinstance(a.get("points"), list) and len(a["points"]) == 1
                        and isinstance(a["points"][0], dict)
                        else f"fly through {len(a.get('points') or [])} points")),
        Tool("manual_control", "control", "Stick input",
             "Hold stick positions for duration_s seconds (MANUAL_CONTROL). x pitch "
             "forward, y roll right, r yaw right, each -1 to 1; z throttle 0 to 1, 0.5 "
             "holds height in position modes. Without a new call the vehicle holds.",
             {"x": {"type": "number", "minimum": -1, "maximum": 1},
              "y": {"type": "number", "minimum": -1, "maximum": 1},
              "z": {"type": "number", "minimum": 0, "maximum": 1},
              "r": {"type": "number", "minimum": -1, "maximum": 1},
              "duration_s": {"type": "number", "minimum": 0.2, "maximum": CONTROL_MAX_S,
                             "default": CONTROL_DEFAULT_S}},
             (), lambda svc, a: svc.control("manual", a),
             lambda a: "stick input"),
        Tool("set_velocity", "control", "Velocity",
             "Fly at a velocity for duration_s seconds. vx, vy, vz in m/s: north, east, "
             "down, or with frame body forward, right, down. yaw_rate in degrees per "
             "second. Without a new call the vehicle holds. Flies in the capabilities' "
             "velocity_mode, never on a plane.",
             {"vx": {"type": "number"}, "vy": {"type": "number"}, "vz": {"type": "number"},
              "yaw_rate": {"type": "number"},
              "frame": {"type": "string", "enum": ["local", "body"], "default": "local"},
              "duration_s": {"type": "number", "minimum": 0.2, "maximum": CONTROL_MAX_S,
                             "default": CONTROL_DEFAULT_S}},
             (), lambda svc, a: svc.control("velocity", a),
             lambda a: f"velocity {a.get('vx', 0)}, {a.get('vy', 0)}, {a.get('vz', 0)} m/s"),
        Tool("stop_control", "control", "Stop control",
             "End direct control now and put the vehicle in hold.",
             {}, (), lambda svc, a: svc.stop_control(a.get("vehicle")), said("stop and hold")),
        Tool("send_command", "command", "MAVLink command",
             "Send one MAV_CMD from the capabilities' command list, with up to seven "
             "params. A command that names a place takes lat, lon and alt.",
             {"command": {"type": "string", "description": "e.g. DO_CHANGE_SPEED"},
              "params": {"type": "array", "items": {"type": "number"}, "maxItems": 7},
              "lat": {"type": "number"}, "lon": {"type": "number"},
              "alt": {"type": "number"}, "frame": {"type": "string"}},
             ("command",), _run_command, lambda a: f"command {a.get('command')}"),
    )


BUILTIN_TOOLS: tuple[Tool, ...] = builtin_tools()
_BUILTIN_BY_NAME: dict[str, Tool] = {t.name: t for t in BUILTIN_TOOLS}


def tool_catalog() -> list[dict[str, Any]]:
    """The permissions with the tools each one opens, for the settings card."""
    return [{
        "id": p.id, "label": p.label, "hint": p.hint, "writes": p.writes,
        "tools": [t.name for t in BUILTIN_TOOLS if t.permission == p.id],
    } for p in PERMISSIONS]


def plugin_tool_name(plugin_id: str, name: str) -> str:
    """The name a plugin tool is listed under: the plugin id, then its own."""
    safe = re.sub(r"[^A-Za-z0-9_-]", "_", plugin_id)
    return f"{safe}_{name}"[:64]


def clean_plugin_tools(raw: Any) -> list[dict[str, Any]]:
    """The tools a window announced, keeping only well formed ones."""
    out: list[dict[str, Any]] = []
    if not isinstance(raw, list):
        return out
    seen: set[str] = set()
    for entry in raw:
        if len(out) >= MAX_PLUGIN_TOOLS:
            break
        if not isinstance(entry, dict):
            continue
        plugin = entry.get("plugin")
        name = entry.get("name")
        if not isinstance(plugin, str) or not _PLUGIN_ID_RE.match(plugin):
            continue
        if not isinstance(name, str) or not _TOOL_NAME_RE.match(name):
            continue
        full = plugin_tool_name(plugin, name)
        if full in seen or full in _BUILTIN_BY_NAME:
            continue
        schema = entry.get("input_schema")
        if not isinstance(schema, dict) or schema.get("type") != "object":
            schema = {"type": "object", "properties": {}}
        try:
            if len(json.dumps(schema)) > MAX_SCHEMA_BYTES:
                continue
        except (TypeError, ValueError):
            continue
        description = entry.get("description")
        title = entry.get("title")
        seen.add(full)
        out.append({
            "plugin": plugin, "name": name, "full": full,
            "title": title[:120] if isinstance(title, str) else name,
            "description": description[:MAX_DESCRIPTION] if isinstance(description, str) else "",
            "input_schema": schema,
            "confirm": entry.get("confirm") is True,
            "read_only": entry.get("read_only") is True,
            "plugin_name": entry.get("plugin_name")[:80]
            if isinstance(entry.get("plugin_name"), str) else plugin,
        })
    return out


def _schema(properties: dict[str, Any], required: tuple[str, ...], vehicle: bool) -> dict[str, Any]:
    props = dict(properties)
    if vehicle:
        props["vehicle"] = {"type": "string",
                            "description": "Vehicle id from list_vehicles. Leave out for "
                                           "the vehicle the operator flies."}
    schema: dict[str, Any] = {"type": "object", "properties": props}
    if required:
        schema["required"] = list(required)
    return schema


def _jsonrpc_error(mid: Any, code: int, message: str) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": mid, "error": {"code": code, "message": message}}


def _jsonrpc_result(mid: Any, result: dict[str, Any]) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": mid, "result": result}


def _tool_result(value: Any, is_error: bool = False) -> dict[str, Any]:
    text = value if isinstance(value, str) else json.dumps(value, default=str)
    out: dict[str, Any] = {"content": [{"type": "text", "text": text}], "isError": is_error}
    if isinstance(value, dict) and not is_error:
        out["structuredContent"] = value
    return out


INSTRUCTIONS = (
    "Corvus GCS is a ground control station connected to a real vehicle (PX4 or "
    "ArduPilot). Every tool acts on that vehicle through the station, with the "
    "checks the operator's own buttons have. The operator decides which tools you "
    "have and may have to approve a call in Corvus first; a refused call says why. "
    "Read get_vehicle_state before acting, and check the result of every command. "
    "Heights are metres above home unless a tool says otherwise. Direct control "
    "lasts only for the duration you give; the vehicle holds when you stop sending."
)


# ----------------------------------------------------------------- service

class McpService:
    """The MCP server's state: settings, sessions, confirmations, plugin calls.

    ``dispatch(method, path, body)`` runs one Corvus endpoint and returns
    ``(status, body)``. ``load_settings()`` returns the raw ``mcp`` config
    block and ``save_settings(block)`` persists one. ``token_path`` is where
    the access token is kept, apart from the config, so a settings export
    never carries it.
    """

    def __init__(
        self,
        *,
        dispatch: Callable[[str, str, dict[str, Any] | None], tuple[int, dict[str, Any]]],
        load_settings: Callable[[], Any],
        save_settings: Callable[[dict[str, Any] | None], None],
        token_path: str | os.PathLike[str],
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self._dispatch = dispatch
        self._load = load_settings
        self._save = save_settings
        self._token_path = pathlib.Path(token_path)
        self._clock = clock
        self._lock = threading.RLock()
        self._listeners: list[Callable[[dict[str, Any]], None]] = []
        self._sessions: dict[str, dict[str, Any]] = {}
        self._windows: dict[str, dict[str, Any]] = {}
        self._pending: dict[str, dict[str, Any]] = {}
        self._calls: dict[str, dict[str, Any]] = {}
        self._last: dict[str, Any] | None = None
        self._tool_sig: tuple[str, ...] = ()
        self._token: str | None = None
        self._control: dict[str, Any] | None = None
        self._control_cond = threading.Condition(self._lock)
        self._control_thread: threading.Thread | None = None
        self._stopping = False

    # ---- settings ----------------------------------------------------

    def settings(self) -> dict[str, Any]:
        """The settings in force."""
        try:
            raw = self._load()
        except Exception:  # noqa: BLE001 - a broken config reads as defaults
            logger.exception("mcp: settings unreadable")
            raw = None
        return effective_settings(raw)

    def enabled(self) -> bool:
        return bool(self.settings()["enabled"])

    def update_settings(self, patch: dict[str, Any]) -> dict[str, Any]:
        """Merge *patch* into the settings, persist, and tell everyone.

        Raises ValueError for a value that is not a boolean: the caller
        answers 400 with the message.
        """
        current = self.settings()
        for key in ("enabled", "fleet"):
            if key in patch:
                if not isinstance(patch[key], bool):
                    raise ValueError(f"{key} must be true or false")
                current[key] = patch[key]
        for key in ("permissions", "confirm", "plugins"):
            if key not in patch:
                continue
            block = patch[key]
            if not isinstance(block, dict):
                raise ValueError(f"{key} must be an object")
            for name, value in block.items():
                if not isinstance(value, bool):
                    raise ValueError(f"{key}.{name} must be true or false")
                if key != "plugins" and name not in _BY_ID:
                    raise ValueError(f"unknown permission {name}")
                if key == "plugins" and not (isinstance(name, str) and _PLUGIN_ID_RE.match(name)):
                    raise ValueError(f"invalid plugin id {name!r}")
                current[key][name] = value
        self._save(compact_settings(current))
        if not current["enabled"]:
            self._deny_all("The operator switched the MCP server off.")
            self._end_control("MCP switched off")
        if not current["permissions"].get("control"):
            self._end_control("direct control switched off")
        if "enabled" in patch:
            self._log(f"MCP server {'on' if current['enabled'] else 'off'}", "info")
        self._changed()
        return self.settings()

    def settings_reloaded(self) -> None:
        """The config was replaced (an import, a reset): re-read and tell everyone."""
        if not self.enabled():
            self._deny_all("The operator switched the MCP server off.")
            self._end_control("MCP switched off")
        self._changed()

    def stop(self) -> dict[str, Any]:
        """The operator's Stop: every permission that changes anything goes off.

        Pending confirmations are refused and direct control ends in hold.
        Reading stays, so the agent can still see what happened.
        """
        current = self.settings()
        for p in PERMISSIONS:
            if p.writes:
                current["permissions"][p.id] = False
        self._save(compact_settings(current))
        self._deny_all("The operator stopped the AI client.")
        self._end_control("stopped by the operator")
        self._log("MCP: the operator took every permission that changes the vehicle away",
                  "warning", notify=True)
        self._changed()
        return self.settings()

    # ---- token -------------------------------------------------------

    def token(self) -> str:
        """The access token, created on first use and kept in its own file."""
        with self._lock:
            if self._token:
                return self._token
            try:
                text = self._token_path.read_text(encoding="utf-8").strip()
            except OSError:
                text = ""
            if len(text) >= 32:
                self._token = text
                return text
            return self._write_token()

    def regenerate_token(self) -> str:
        """A new token. Clients holding the old one are refused from now on."""
        with self._lock:
            token = self._write_token()
            self._sessions.clear()
        self._log("MCP: new access token, clients have to be given it again", "info")
        self._emit_status()
        return token

    def _write_token(self) -> str:
        token = secrets.token_urlsafe(32)
        try:
            self._token_path.parent.mkdir(parents=True, exist_ok=True)
            tmp = self._token_path.with_name(self._token_path.name + ".tmp")
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                handle.write(token + "\n")
            os.replace(tmp, self._token_path)
        except OSError:
            # Still usable for this session; the next start makes another.
            logger.exception("mcp: token not saved to %s", self._token_path)
        self._token = token
        return token

    def authorized(self, header: str | None) -> bool:
        """Whether an ``Authorization`` header carries the token."""
        if not isinstance(header, str):
            return False
        scheme, _, value = header.strip().partition(" ")
        if scheme.lower() != "bearer" or not value.strip():
            return False
        return hmac.compare_digest(value.strip().encode(), self.token().encode())

    # ---- listeners (the UI's "mcp" event topic) ---------------------

    def add_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._lock:
            self._listeners.append(fn)

    def remove_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._lock:
            try:
                self._listeners.remove(fn)
            except ValueError:
                pass

    def _emit(self, event: dict[str, Any]) -> None:
        with self._lock:
            listeners = list(self._listeners)
        for fn in listeners:
            try:
                fn(event)
            except Exception:  # noqa: BLE001 - one listener never costs the others
                logger.exception("mcp: listener failed")

    def _emit_status(self) -> None:
        self._emit(dict(self.status(), type="status"))

    def _log(self, text: str, level_: str = "cmd", notify: bool = False) -> None:
        logger.info("%s", text)
        self._emit({"type": "log", "text": text, "level": level_, "notify": notify})

    # ---- status ------------------------------------------------------

    def status(self) -> dict[str, Any]:
        """What the settings card and the flight indicator show."""
        now = self._clock()
        settings = self.settings()
        with self._lock:
            # One line per client: a client that reconnects opens a new session
            # and leaves the old one behind until it is evicted.
            clients: list[dict[str, Any]] = []
            seen_names: set[tuple[str, str]] = set()
            for s in sorted(self._sessions.values(), key=lambda s: -s["seen"]):
                key = (s.get("client", ""), s.get("client_version", ""))
                if key in seen_names:
                    continue
                seen_names.add(key)
                clients.append({"name": key[0], "version": key[1],
                                "seen_s": round(now - s["seen"], 1)})
            control = None
            if self._control is not None:
                control = {"kind": self._control["kind"],
                           "vehicle": self._control.get("vehicle") or "",
                           "ends_in_s": round(max(0.0, self._control["until"] - now), 1)}
            pending = [self._pending_event(p) for p in self._pending.values()]
            windows = len(self._live_windows(now))
            last = dict(self._last) if self._last else None
        return {
            "enabled": settings["enabled"],
            "active": any(c["seen_s"] <= CLIENT_ACTIVE_S for c in clients),
            "clients": clients[:8],
            "control": control,
            "writes": any(settings["permissions"][p.id] for p in PERMISSIONS if p.writes),
            "pending": pending,
            "windows": windows,
            "last": last,
        }

    # ---- windows and plugin tools -----------------------------------

    def _live_windows(self, now: float) -> dict[str, dict[str, Any]]:
        dead = [k for k, w in self._windows.items() if now - w["seen"] > WINDOW_TTL_S]
        for key in dead:
            del self._windows[key]
        return self._windows

    def window_heartbeat(self, window_id: Any, tools: Any) -> dict[str, Any]:
        """A Corvus window says it is open, with the plugin tools it can run."""
        if not isinstance(window_id, str) or not _WINDOW_ID_RE.match(window_id):
            raise ValueError("id must be a short identifier")
        cleaned = clean_plugin_tools(tools)
        with self._lock:
            self._windows[window_id] = {"seen": self._clock(), "tools": cleaned}
        self._check_tools_changed()
        return {"tools": len(cleaned)}

    def plugin_tools(self) -> list[dict[str, Any]]:
        """Every plugin tool an open window offers, the first window first."""
        with self._lock:
            windows = sorted(self._live_windows(self._clock()).items(),
                             key=lambda kv: kv[1]["seen"])
            out: dict[str, dict[str, Any]] = {}
            for window_id, window in windows:
                for tool in window["tools"]:
                    out.setdefault(tool["full"], dict(tool, window=window_id))
        return list(out.values())

    def plugin_summary(self) -> list[dict[str, Any]]:
        """The plugins that offer tools, with their tool names, for the card."""
        grouped: dict[str, dict[str, Any]] = {}
        for tool in self.plugin_tools():
            entry = grouped.setdefault(tool["plugin"], {
                "id": tool["plugin"], "name": tool["plugin_name"], "tools": []})
            entry["tools"].append(tool["full"])
        return sorted(grouped.values(), key=lambda e: e["name"].lower())

    # ---- the tool list -----------------------------------------------

    def tools(self) -> list[dict[str, Any]]:
        """The tools this agent may call right now, as ``tools/list`` gives them."""
        settings = self.settings()
        out: list[dict[str, Any]] = []
        for tool in BUILTIN_TOOLS:
            if level(settings, tool.permission) == "off":
                continue
            out.append({
                "name": tool.name, "title": tool.title, "description": tool.description,
                "inputSchema": _schema(tool.properties, tool.required,
                                       tool.vehicle and settings["fleet"]),
                "annotations": {"title": tool.title, "readOnlyHint": tool.read_only,
                                "destructiveHint": not tool.read_only,
                                "openWorldHint": True},
            })
        if level(settings, "plugins") != "off":
            for tool in self.plugin_tools():
                if not plugin_allowed(settings, tool["plugin"]):
                    continue
                out.append({
                    "name": tool["full"], "title": tool["title"],
                    "description": f"{tool['description']} (plugin {tool['plugin_name']})".strip(),
                    "inputSchema": tool["input_schema"],
                    "annotations": {"title": tool["title"], "readOnlyHint": tool["read_only"],
                                    "destructiveHint": not tool["read_only"]},
                })
        return out

    def _check_tools_changed(self) -> None:
        sig = tuple(sorted(t["name"] for t in self.tools())) if self.enabled() else ()
        with self._lock:
            if sig == self._tool_sig:
                return
            self._tool_sig = sig
            streams = [q for s in self._sessions.values() for q in s["streams"]]
        note = {"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}
        for q in streams:
            try:
                q.put_nowait(note)
            except queue.Full:
                pass

    def _changed(self) -> None:
        self._check_tools_changed()
        self._emit_status()

    # ---- JSON-RPC ----------------------------------------------------

    def handle_post(self, message: Any, session_id: str | None,
                    ) -> tuple[int, Any, dict[str, str]]:
        """One POSTed JSON-RPC message (or batch): ``(status, reply, headers)``.

        A reply of None means 202 with no body: the message held only
        notifications or responses.
        """
        headers: dict[str, str] = {}
        batch = isinstance(message, list)
        messages = message if batch else [message]
        if batch and not messages:
            return 400, _jsonrpc_error(None, JSONRPC_INVALID_REQUEST, "empty batch"), headers
        is_init = any(isinstance(m, dict) and m.get("method") == "initialize" for m in messages)
        session = None
        if session_id and not is_init:
            with self._lock:
                session = self._sessions.get(session_id)
            if session is None:
                return 404, _jsonrpc_error(None, JSONRPC_INVALID_REQUEST,
                                           "unknown session, initialize again"), headers
            session["seen"] = self._clock()
        replies = []
        for msg in messages:
            reply, new_session = self._rpc(msg, session)
            if new_session:
                headers["Mcp-Session-Id"] = new_session
            if reply is not None:
                replies.append(reply)
        if not replies:
            return 202, None, headers
        return 200, (replies if batch else replies[0]), headers

    def _rpc(self, msg: Any, session: dict[str, Any] | None) -> tuple[Any, str | None]:
        if not isinstance(msg, dict) or msg.get("jsonrpc") != "2.0":
            return _jsonrpc_error(None, JSONRPC_INVALID_REQUEST, "not a JSON-RPC 2.0 message"), None
        method = msg.get("method")
        if "id" not in msg:
            return None, None
        mid = msg.get("id")
        if not isinstance(method, str):
            # A response to something we never ask: nothing to answer.
            return None, None
        params = msg.get("params") or {}
        if not isinstance(params, dict):
            return _jsonrpc_error(mid, JSONRPC_INVALID_PARAMS, "params must be an object"), None
        try:
            if method == "initialize":
                result, sid = self._initialize(params)
                return _jsonrpc_result(mid, result), sid
            if method == "ping":
                return _jsonrpc_result(mid, {}), None
            if method == "tools/list":
                return _jsonrpc_result(mid, {"tools": self.tools()}), None
            if method == "tools/call":
                name = params.get("name")
                args = params.get("arguments") or {}
                if not isinstance(name, str) or not isinstance(args, dict):
                    return _jsonrpc_error(mid, JSONRPC_INVALID_PARAMS,
                                          "tools/call needs a name and an arguments object"), None
                result = self.call_tool(name, args, session)
                if result is None:
                    return _jsonrpc_error(mid, JSONRPC_INVALID_PARAMS,
                                          f"unknown tool {name}"), None
                return _jsonrpc_result(mid, result), None
            if method in ("resources/list", "prompts/list", "resources/templates/list"):
                key = {"resources/list": "resources", "prompts/list": "prompts",
                       "resources/templates/list": "resourceTemplates"}[method]
                return _jsonrpc_result(mid, {key: []}), None
        except Exception:  # noqa: BLE001 - a bug answers an error, never drops the client
            logger.exception("mcp: %s failed", method)
            return _jsonrpc_error(mid, JSONRPC_INTERNAL_ERROR, "internal error"), None
        return _jsonrpc_error(mid, JSONRPC_METHOD_NOT_FOUND, f"unknown method {method}"), None

    def _initialize(self, params: dict[str, Any]) -> tuple[dict[str, Any], str]:
        asked = params.get("protocolVersion")
        version = asked if asked in PROTOCOL_VERSIONS else DEFAULT_PROTOCOL
        info = params.get("clientInfo") if isinstance(params.get("clientInfo"), dict) else {}
        sid = secrets.token_hex(16)
        now = self._clock()
        with self._lock:
            if len(self._sessions) >= MAX_SESSIONS:
                oldest = min(self._sessions, key=lambda k: self._sessions[k]["seen"])
                self._drop_session(oldest)
            self._sessions[sid] = {
                "client": str(info.get("name") or "MCP client")[:80],
                "client_version": str(info.get("version") or "")[:40],
                "protocol": version, "seen": now, "streams": set(),
            }
            self._tool_sig = tuple(sorted(t["name"] for t in self.tools()))
        self._log(f"MCP: {self._sessions[sid]['client']} connected", "info")
        self._emit_status()
        return {
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": True}},
            "serverInfo": {"name": SERVER_NAME, "title": SERVER_TITLE, "version": get_version()},
            "instructions": INSTRUCTIONS,
        }, sid

    def end_session(self, session_id: str | None) -> bool:
        """A client said goodbye (``DELETE /mcp``)."""
        with self._lock:
            if not session_id or session_id not in self._sessions:
                return False
            self._drop_session(session_id)
        self._emit_status()
        return True

    def _drop_session(self, sid: str) -> None:
        session = self._sessions.pop(sid, None)
        for q in (session or {}).get("streams", ()):
            try:
                q.put_nowait(None)
            except queue.Full:
                pass

    def open_stream(self, session_id: str | None) -> queue.Queue[Any] | None:
        """The queue a ``GET /mcp`` stream drains, or None without a session."""
        with self._lock:
            session = self._sessions.get(session_id or "")
            if session is None:
                return None
            q: queue.Queue[Any] = queue.Queue(STREAM_QUEUE)
            session["streams"].add(q)
            return q

    def close_stream(self, session_id: str | None, q: queue.Queue[Any]) -> None:
        with self._lock:
            session = self._sessions.get(session_id or "")
            if session is not None:
                session["streams"].discard(q)

    # ---- calling a tool ---------------------------------------------

    def call_tool(self, name: str, args: dict[str, Any],
                  session: dict[str, Any] | None = None) -> dict[str, Any] | None:
        """Run one tool for an agent. None for a name that is no tool at all."""
        settings = self.settings()
        client = (session or {}).get("client", "AI client")
        tool = _BUILTIN_BY_NAME.get(name)
        plugin_tool = None
        if tool is None:
            plugin_tool = next((t for t in self.plugin_tools() if t["full"] == name), None)
            if plugin_tool is None:
                return None
        permission = tool.permission if tool else "plugins"
        perm_level = level(settings, permission)
        if perm_level == "off" or (plugin_tool and not plugin_allowed(settings, plugin_tool["plugin"])):
            label = _BY_ID[permission].label
            return self._refused(name, f"The operator has not allowed this in Corvus "
                                       f"({label}, under {WHERE}).")
        vehicle = args.get("vehicle")
        if vehicle is not None:
            if tool is None or not tool.vehicle:
                return self._refused(name, "This tool does not take a vehicle.")
            if not settings["fleet"]:
                return self._refused(name, "The operator allows only the vehicle they fly. "
                                           "Leave vehicle out.")
            if not isinstance(vehicle, str) or not vehicle:
                return self._refused(name, "vehicle must be an id from list_vehicles")
        try:
            summary = tool.summary(args) if tool else (plugin_tool or {}).get("title", name)
        except Exception:  # noqa: BLE001 - a bad argument shape is the run's to report
            summary = name
        where = f" ({vehicle})" if vehicle else ""
        ask = perm_level == "confirm" or bool(plugin_tool and plugin_tool["confirm"])
        if ask and tool is not None and tool.permission == "control" and self._control_live(vehicle):
            ask = False
        if ask and tool is not None and tool.name == "stop_control":
            ask = False
        if ask:
            answer = self.confirm(client, name, summary + where, permission)
            if answer is not True:
                return self._refused(name, answer)
        read_only = tool.read_only if tool else bool(plugin_tool and plugin_tool["read_only"])
        if not read_only:
            self._log(f"MCP {client}: {summary}{where}", "cmd")
        try:
            if tool is not None:
                value = tool.run(self, args)
            else:
                value = self._call_plugin(plugin_tool, args)
        except ToolError as exc:
            self._remember(name, False)
            if not read_only:
                self._log(f"MCP {client}: {summary} failed: {exc}", "error", notify=True)
            return _tool_result(str(exc), is_error=True)
        self._remember(name, True)
        return _tool_result(value if value is not None else {"ok": True})

    def _refused(self, name: str, reason: str) -> dict[str, Any]:
        self._remember(name, False)
        self._log(f"MCP: {name} refused. {reason}", "warning", notify=True)
        return _tool_result(reason, is_error=True)

    def _remember(self, name: str, ok: bool) -> None:
        with self._lock:
            self._last = {"tool": name, "ok": ok, "at": time.time()}
        self._emit_status()

    def api(self, method: str, path: str, body: dict[str, Any] | None = None,
            vehicle: Any = None) -> dict[str, Any]:
        """Run one Corvus endpoint for a tool; a refusal raises ToolError."""
        if vehicle:
            if method == "GET":
                path = f"{path}?vehicle={quote(str(vehicle), safe='')}"
            else:
                body = dict(body or {}, vehicle=vehicle)
        try:
            status, reply = self._dispatch(method, path, body if method != "GET" else None)
        except Exception as exc:  # noqa: BLE001 - an endpoint bug is a failed tool
            logger.exception("mcp: %s %s raised", method, path)
            raise ToolError(f"Corvus could not run this: {exc}") from exc
        reply = reply if isinstance(reply, dict) else {"value": reply}
        if status >= 400 or reply.get("ok") is False or (reply.get("error") and status >= 400):
            raise ToolError(str(reply.get("error") or f"refused ({status})"))
        return reply

    # ---- confirmation ------------------------------------------------

    def _pending_event(self, p: dict[str, Any]) -> dict[str, Any]:
        return {"type": "confirm", "id": p["id"], "client": p["client"], "tool": p["tool"],
                "summary": p["summary"], "permission": p["permission"],
                "permission_label": _BY_ID[p["permission"]].label,
                "expires_in_s": round(max(0.0, p["deadline"] - self._clock()), 1)}

    def confirm(self, client: str, tool: str, summary: str, permission: str) -> bool | str:
        """Ask the operator. True, or the reason it was not allowed."""
        now = self._clock()
        with self._lock:
            if not self._live_windows(now):
                return ("This needs the operator's approval, and no Corvus window is open "
                        "to ask in.")
            pending = {"id": secrets.token_hex(8), "client": client, "tool": tool,
                       "summary": summary, "permission": permission,
                       "deadline": now + CONFIRM_TIMEOUT_S, "event": threading.Event(),
                       "answer": None}
            self._pending[pending["id"]] = pending
        self._emit(self._pending_event(pending))
        self._emit_status()
        pending["event"].wait(CONFIRM_TIMEOUT_S)
        with self._lock:
            self._pending.pop(pending["id"], None)
        answer = pending["answer"]
        self._emit({"type": "confirm_done", "id": pending["id"], "allowed": answer is True})
        self._emit_status()
        if answer is True:
            return True
        if answer is None:
            return f"The operator did not answer within {CONFIRM_TIMEOUT_S:g} s."
        return answer if isinstance(answer, str) else "The operator refused."

    def answer(self, pending_id: Any, allow: Any) -> bool:
        """The operator's answer from the confirmation dialog."""
        with self._lock:
            pending = self._pending.get(pending_id) if isinstance(pending_id, str) else None
            if pending is None:
                return False
            pending["answer"] = True if allow is True else "The operator refused."
            pending["event"].set()
        return True

    def _deny_all(self, reason: str) -> None:
        with self._lock:
            for p in self._pending.values():
                p["answer"] = reason
                p["event"].set()
            for c in self._calls.values():
                c["error"] = reason
                c["event"].set()

    # ---- plugin calls ------------------------------------------------

    def _call_plugin(self, tool: dict[str, Any], args: dict[str, Any]) -> Any:
        call = {"id": secrets.token_hex(8), "event": threading.Event(),
                "ok": False, "result": None, "error": None}
        with self._lock:
            self._calls[call["id"]] = call
        self._emit({"type": "call", "id": call["id"], "window": tool["window"],
                    "plugin": tool["plugin"], "tool": tool["name"], "args": args})
        finished = call["event"].wait(PLUGIN_CALL_TIMEOUT_S)
        with self._lock:
            self._calls.pop(call["id"], None)
        if not finished:
            raise ToolError(f"The plugin did not answer within {PLUGIN_CALL_TIMEOUT_S:g} s.")
        if not call["ok"]:
            raise ToolError(str(call["error"] or "The plugin refused."))
        return call["result"]

    def call_result(self, call_id: Any, ok: Any, result: Any, error: Any) -> bool:
        """A window's answer to a plugin call."""
        with self._lock:
            call = self._calls.get(call_id) if isinstance(call_id, str) else None
            if call is None:
                return False
            call["ok"] = ok is True
            call["result"] = result
            call["error"] = error if isinstance(error, str) else None
            call["event"].set()
        return True

    # ---- direct control ---------------------------------------------

    def _control_live(self, vehicle: Any) -> bool:
        with self._lock:
            c = self._control
            return c is not None and c.get("vehicle") == (vehicle or None) \
                and c["until"] > self._clock()

    def control(self, kind: str, args: dict[str, Any]) -> dict[str, Any]:
        """Start or extend direct control: repeated until it runs out, then hold."""
        seconds = _duration(args)
        vehicle = args.get("vehicle") or None
        if kind == "manual":
            body = {"x": _num(args, "x", 0.0), "y": _num(args, "y", 0.0),
                    "z": _num(args, "z", 0.5), "r": _num(args, "r", 0.0)}
            path = "/api/mavlink/manual"
        else:
            body = {"vx": _num(args, "vx", 0.0), "vy": _num(args, "vy", 0.0),
                    "vz": _num(args, "vz", 0.0)}
            if args.get("yaw_rate") is not None:
                body["yaw_rate"] = _num(args, "yaw_rate")
            frame = args.get("frame", "local")
            if frame not in ("local", "body"):
                raise ToolError('frame must be "local" or "body"')
            body["frame"] = frame
            path = "/api/mavlink/velocity"
        # The first frame goes now, through the endpoint, so a refusal (wrong
        # mode, a plane, no link) is the agent's answer rather than a silent
        # stream of failures.
        self.api("POST", path, body, vehicle=vehicle)
        with self._lock:
            self._control = {"kind": kind, "vehicle": vehicle, "path": path,
                             "body": dict(body, vehicle=vehicle) if vehicle else body,
                             "until": self._clock() + seconds}
            if self._control_thread is None or not self._control_thread.is_alive():
                self._control_thread = threading.Thread(
                    target=self._control_loop, name="corvus-mcp-control", daemon=True)
                self._control_thread.start()
            self._control_cond.notify_all()
        self._emit_status()
        return {"ok": True, "holds_after_s": seconds}

    def stop_control(self, vehicle: Any = None) -> dict[str, Any]:
        """End direct control and hold."""
        with self._lock:
            active = self._control is not None
            target = self._control.get("vehicle") if active else (vehicle or None)
            self._control = None
            self._control_cond.notify_all()
        self.api("POST", "/api/mavlink/hold", {}, vehicle=target)
        self._emit_status()
        return {"ok": True, "was_active": active}

    def _end_control(self, why: str) -> None:
        with self._lock:
            control = self._control
            self._control = None
            self._control_cond.notify_all()
        if control is not None:
            self._hold(control.get("vehicle"), why)

    def _hold(self, vehicle: Any, why: str) -> None:
        try:
            self.api("POST", "/api/mavlink/hold", {}, vehicle=vehicle)
            self._log(f"MCP: direct control ended ({why}), holding", "warning", notify=True)
        except ToolError as exc:
            self._log(f"MCP: direct control ended ({why}) but hold failed: {exc}",
                      "error", notify=True)
        self._emit_status()

    def _control_loop(self) -> None:
        period = 1.0 / CONTROL_RATE_HZ
        while True:
            with self._lock:
                while self._control is None and not self._stopping:
                    self._control_cond.wait(5.0)
                if self._stopping:
                    return
                control = dict(self._control)
                expired = self._clock() >= control["until"]
                if expired:
                    self._control = None
            if expired:
                self._hold(control.get("vehicle"), "no new command")
                continue
            try:
                status, reply = self._dispatch("POST", control["path"], control["body"])
                if status >= 400:
                    logger.warning("mcp: control frame refused: %s", reply.get("error"))
            except Exception:  # noqa: BLE001 - the loop must keep its hold promise
                logger.exception("mcp: control frame failed")
            with self._lock:
                if not self._stopping:
                    self._control_cond.wait(period)

    # ---- teardown ----------------------------------------------------

    def shutdown(self) -> None:
        """Refuse what waits, hold a vehicle under direct control, join the thread."""
        self._deny_all("Corvus GCS is closing.")
        with self._lock:
            control = self._control
            self._control = None
            self._stopping = True
            self._control_cond.notify_all()
            for sid in list(self._sessions):
                self._drop_session(sid)
            thread = self._control_thread
        if control is not None:
            try:
                self._dispatch("POST", "/api/mavlink/hold",
                               {"vehicle": control["vehicle"]} if control.get("vehicle") else {})
            except Exception:  # noqa: BLE001 - closing goes on regardless
                logger.exception("mcp: hold on shutdown failed")
        if thread is not None:
            thread.join(timeout=2.0)
