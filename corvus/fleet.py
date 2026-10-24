"""Several vehicles at once: who is out there, what they are called, which one is flown.

One vehicle is flown at a time. Every page, every command, the HUD and the
telemetry stream belong to that one, exactly as they did before this module
existed; the others are *seen*: their position, heading, mode and battery
reach the map and the vehicle list.

**One link is one vehicle.** A link (a radio, a USB cable, a UDP port) flies
the first autopilot it latches and nothing else; another system id heard on
the same link is ignored and told to the console once. A second vehicle needs
a radio and a link of its own, which is how it works in the field anyway:
every PX4 leaves the factory as system 1, so two of those cannot share a link.
An extra link is a complete :class:`~corvus.mavlink_bridge.MavlinkBridge` with
its own store, so the vehicle on it is as flyable as the one on the main link.

Selecting a vehicle swaps the bridge and store the HTTP layer talks to
(:class:`ActiveBridge`, :class:`ActiveStore`). Removing a vehicle closes its
link (the main link is disconnected and left closed), and it leaves the list.

The guarantee this module is built around: with one vehicle and no extra
link, nothing changes. The proxies point at the main bridge and store and never
move, the tracker only reads frames the bridge already received, and nothing
is ever sent to a vehicle on the list's behalf.
"""
from __future__ import annotations

import json
import logging
import math
import re
import threading
import time
from typing import Any
from collections.abc import Callable

from . import autopilot as autopilot_dialect

logger = logging.getLogger("corvus.fleet")

MAIN_LINK = "main"

# Colours a vehicle is given when the operator has not chosen one. Picked to
# stay apart from each other and from the theme's own red, which the first
# vehicle keeps so a single aircraft looks exactly as it always has.
PALETTE: tuple[str, ...] = (
    "#2F7DF6",  # blue
    "#1FA971",  # green
    "#F5A623",  # amber
    "#9B59D0",  # violet
    "#00A3B4",  # teal
    "#E8508C",  # pink
    "#7DB928",  # lime
    "#C9712E",  # copper
)

# A vehicle whose heartbeat has not been heard for this long is drawn as lost.
STALE_S = 5.0
# ...and after this long it is dropped from the list altogether. The operator
# can remove it at once instead.
FORGET_S = 600.0
# How often the fleet snapshot is pushed to the browser.
PUBLISH_INTERVAL_S = 0.25
# How long colours wait for the link to latch the vehicle it flies, which is
# the one that keeps the theme's colour. Past this they are handed out anyway.
COLOR_GRACE_S = 15.0
# Messages from a vehicle that is not being flown, as the notification board
# shows them: STATUSTEXT at WARNING (4) or worse. The vehicle being flown has
# its own, fuller path through the state store.
EVENT_MAX_SEVERITY = 4
# The same line from the same vehicle is told once in this long. PX4 repeats a
# standing warning, and a board that filled with it would bury the next one.
EVENT_REPEAT_S = 30.0
MAX_EVENTS = 40

MAX_EXTRA_LINKS = 8
MAX_NAME_CHARS = 40
# Vehicles remembered in the config (names and colours). Oldest unnamed first
# when the table is full, so an operator's own names are the last to go.
MAX_REMEMBERED = 64

_COLOR_RE = re.compile(r"^#[0-9A-Fa-f]{6}$")
_LINK_ID_RE = re.compile(r"^link-[0-9]{1,3}$")
_KEY_RE = re.compile(r"^(main|link-[0-9]{1,3}):([0-9]{1,3})$")


def vehicle_key(link_id: str, system_id: int) -> str:
    """The id a vehicle is known by: its link and its MAVLink system id."""
    return f"{link_id}:{int(system_id)}"


def parse_vehicle_key(key: Any) -> tuple[str, int] | None:
    """``"link-2:3"`` -> ``("link-2", 3)``, or None for anything else."""
    if not isinstance(key, str):
        return None
    m = _KEY_RE.match(key)
    if m is None:
        return None
    system_id = int(m.group(2))
    if not 1 <= system_id <= 255:
        return None
    return m.group(1), system_id


def clean_name(value: Any) -> str:
    """A vehicle name as typed, trimmed and bounded; "" for none."""
    if not isinstance(value, str):
        return ""
    text = " ".join(value.split())
    return text[:MAX_NAME_CHARS]


def clean_color(value: Any) -> str:
    """``#RRGGBB`` in upper case, or "" for "the theme's own colour"."""
    if isinstance(value, str) and _COLOR_RE.match(value.strip()):
        return value.strip().upper()
    return ""


def coerce_config(raw: Any) -> dict[str, Any] | None:
    """The ``fleet`` block of the config, bounded and typed. Never raises.

    ``{"links": [{"id": "link-2", "connection": "serial:/dev/ttyUSB1:57600"}],
    "vehicles": {"link-2:1": {"name": "Scout", "color": "#2F7DF6"}},
    "theme_vehicle": "main:1"}``. ``theme_vehicle`` is the one vehicle that
    keeps the theme's own colour while it has none chosen. Anything else (the
    ``shared_links`` switch of earlier versions) is dropped.
    """
    if not isinstance(raw, dict):
        return None
    out: dict[str, Any] = {}
    links: list[dict[str, str]] = []
    seen_ids: set[str] = set()
    seen_conns: set[str] = set()
    for entry in raw.get("links") or []:
        if not isinstance(entry, dict):
            continue
        link_id = entry.get("id")
        conn = entry.get("connection")
        if not isinstance(link_id, str) or not _LINK_ID_RE.match(link_id):
            continue
        if not isinstance(conn, str) or not conn.strip():
            continue
        conn = conn.strip()
        if link_id in seen_ids or conn in seen_conns:
            continue
        seen_ids.add(link_id)
        seen_conns.add(conn)
        links.append({"id": link_id, "connection": conn})
        if len(links) >= MAX_EXTRA_LINKS:
            break
    if links:
        out["links"] = links
    vehicles: dict[str, dict[str, str]] = {}
    raw_vehicles = raw.get("vehicles")
    if isinstance(raw_vehicles, dict):
        for key, meta in raw_vehicles.items():
            if parse_vehicle_key(key) is None or not isinstance(meta, dict):
                continue
            entry: dict[str, str] = {}
            name = clean_name(meta.get("name"))
            color = clean_color(meta.get("color"))
            if name:
                entry["name"] = name
            if color:
                entry["color"] = color
            if entry:
                vehicles[key] = entry
            if len(vehicles) >= MAX_REMEMBERED:
                break
    if vehicles:
        out["vehicles"] = vehicles
    if parse_vehicle_key(raw.get("theme_vehicle")) is not None:
        out["theme_vehicle"] = raw["theme_vehicle"]
    return out or None


def _finite(value: Any) -> float | None:
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    return number if math.isfinite(number) else None


class FleetTracker:
    """What the one autopilot a link flies last said about itself. Passive.

    Fed every frame of that vehicle the bridge receives. It keeps a few fields,
    enough to draw the aircraft on the map and list it, and decides nothing
    about the link. Until :meth:`bind` names the system the link latched it
    takes nothing, so a stray frame on a noisy radio never becomes a vehicle.
    Thread-safe: written by the receive thread, read by the fleet publisher.
    """

    def __init__(self, type_names: dict[int, str] | None = None) -> None:
        self._lock = threading.Lock()
        self._system: int | None = None
        self._component = 0
        self._entry: dict[str, Any] | None = None
        self._type_names = dict(type_names or {})
        # STATUSTEXT at warning level or worse, as (seq, system, severity,
        # text). Bounded; the fleet reads past its own cursor.
        self._events: list[tuple[int, int, int, str]] = []
        self._event_seq = 0

    def bind(self, system: int, component: int = 0) -> None:
        """Track *system* from now on, and only it. Another system forgets the last."""
        try:
            system = int(system)
            component = int(component or 0)
        except (TypeError, ValueError):
            return
        if not 1 <= system <= 255:
            return
        with self._lock:
            if system != self._system:
                self._entry = None
            self._system = system
            self._component = component
            if self._entry is not None and component:
                self._entry["component"] = component

    def bound_system(self) -> int | None:
        with self._lock:
            return self._system

    def observe(self, msg: Any, vehicle_heartbeat: bool = False, now: float | None = None) -> None:
        """Take one received frame. *vehicle_heartbeat* is the bridge's verdict
        that a HEARTBEAT is an autopilot's and not a gimbal's or a station's."""
        try:
            name = msg.get_type()
            system = int(msg.get_srcSystem() or 0)
            component = int(msg.get_srcComponent() or 0)
        except Exception:  # noqa: BLE001 - a malformed frame is not the tracker's problem
            return
        stamp = time.monotonic() if now is None else now
        with self._lock:
            if self._system is None or system != self._system:
                return
            entry = self._entry
            if name == "HEARTBEAT":
                if not vehicle_heartbeat:
                    return
                if self._component and component and component != self._component:
                    # A companion running MAVSDK heartbeats as an autopilot on
                    # the same system id; it does not speak for the airframe.
                    return
                if entry is None:
                    entry = self._new_entry(system, component or self._component, stamp)
                    self._entry = entry
                self._take_heartbeat(entry, msg, stamp)
                return
            if entry is None or (entry["component"] and component != entry["component"]):
                return
            if name == "GLOBAL_POSITION_INT":
                lat = _finite(getattr(msg, "lat", None))
                lon = _finite(getattr(msg, "lon", None))
                if lat is None or lon is None or (lat == 0 and lon == 0):
                    return
                if not (-90e7 <= lat <= 90e7 and -180e7 <= lon <= 180e7):
                    return
                entry["position"] = [round(lon / 1e7, 7), round(lat / 1e7, 7)]
                rel = _finite(getattr(msg, "relative_alt", None))
                amsl = _finite(getattr(msg, "alt", None))
                entry["altitude_rel"] = round(rel / 1000.0, 1) if rel is not None else None
                entry["altitude_amsl"] = round(amsl / 1000.0, 1) if amsl is not None else None
                hdg = getattr(msg, "hdg", 65535)
                if isinstance(hdg, (int, float)) and hdg != 65535:
                    entry["heading"] = round(float(hdg) / 100.0, 1)
                vx = _finite(getattr(msg, "vx", 0)) or 0.0
                vy = _finite(getattr(msg, "vy", 0)) or 0.0
                entry["groundspeed"] = round(math.hypot(vx, vy) / 100.0, 1)
            elif name == "VFR_HUD":
                heading = _finite(getattr(msg, "heading", None))
                if heading is not None:
                    entry["heading"] = heading
                speed = _finite(getattr(msg, "groundspeed", None))
                if speed is not None:
                    entry["groundspeed"] = round(speed, 1)
            elif name == "ATTITUDE" and entry.get("heading") is None:
                yaw = _finite(getattr(msg, "yaw", None))
                if yaw is not None:
                    entry["heading"] = round(math.degrees(yaw) % 360.0, 1)
            elif name == "STATUSTEXT":
                self._take_statustext(entry, msg)
            elif name == "SYS_STATUS":
                remaining = getattr(msg, "battery_remaining", -1)
                if isinstance(remaining, int) and 0 <= remaining <= 100:
                    entry["battery"] = remaining
                voltage = getattr(msg, "voltage_battery", 65535)
                if isinstance(voltage, int) and 0 < voltage != 65535:
                    entry["voltage"] = round(voltage / 1000.0, 1)

    def _take_statustext(self, entry: dict[str, Any], msg: Any) -> None:
        try:
            severity = int(getattr(msg, "severity", 6))
        except (TypeError, ValueError):
            return
        if severity > EVENT_MAX_SEVERITY:
            return
        raw = getattr(msg, "text", "")
        if isinstance(raw, (bytes, bytearray)):
            raw = bytes(raw).split(b"\x00", 1)[0].decode("utf-8", errors="replace")
        text = " ".join(str(raw).split("\x00", 1)[0].split())
        if not text:
            return
        self._event_seq += 1
        self._events.append((self._event_seq, entry["sysid"], severity, text))
        if len(self._events) > MAX_EVENTS:
            del self._events[:-MAX_EVENTS]

    def events_since(self, seq: int) -> tuple[int, list[tuple[int, int, str]]]:
        """``(last seq, [(system, severity, text), ...])`` after *seq*."""
        with self._lock:
            fresh = [(s, sev, t) for n, s, sev, t in self._events if n > seq]
            return self._event_seq, fresh

    def _new_entry(self, system: int, component: int, stamp: float) -> dict[str, Any]:
        return {
            "sysid": system, "component": component,
            "first_heartbeat": stamp, "last_heartbeat": stamp,
            "type": "", "type_id": 0, "autopilot": 0, "stack": "",
            "mode": "", "mode_label": "", "armed": False, "system_status": 0,
            "position": None, "altitude_rel": None, "altitude_amsl": None,
            "heading": None, "groundspeed": None, "battery": None, "voltage": None,
        }

    def _take_heartbeat(self, entry: dict[str, Any], msg: Any, stamp: float) -> None:
        entry["last_heartbeat"] = stamp
        try:
            type_id = int(getattr(msg, "type", 0) or 0)
        except (TypeError, ValueError):
            type_id = 0
        entry["type_id"] = type_id
        entry["type"] = self._type_names.get(type_id, f"TYPE_{type_id}")
        autopilot_id = getattr(msg, "autopilot", 0)
        entry["autopilot"] = autopilot_id
        base_mode = int(getattr(msg, "base_mode", 0) or 0)
        entry["armed"] = bool(base_mode & 128)
        entry["system_status"] = int(getattr(msg, "system_status", 0) or 0)
        try:
            dialect = autopilot_dialect.dialect_for(autopilot_id)
            mode = dialect.decode_mode(getattr(msg, "custom_mode", 0), base_mode, type_id)
            entry["stack"] = dialect.stack
            entry["mode"] = mode
            entry["mode_label"] = dialect.mode_label(mode) if mode else ""
        except Exception:  # noqa: BLE001 - an undecodable mode is shown as none
            entry["mode"] = entry["mode_label"] = ""

    def systems(self, now: float | None = None) -> list[dict[str, Any]]:
        """The vehicle, as a list of none or one plain dict."""
        stamp = time.monotonic() if now is None else now
        with self._lock:
            entry = self._entry
            if entry is None:
                return []
            age = stamp - entry["last_heartbeat"]
            if age > FORGET_S:
                self._entry = None
                return []
            item = {
                k: v for k, v in entry.items()
                if k not in ("first_heartbeat", "last_heartbeat", "component", "autopilot")
            }
        item["stale"] = age > STALE_S
        return [item]

    def forget(self) -> None:
        """Drop the vehicle and the binding: the link flies nothing now."""
        with self._lock:
            self._entry = None
            self._system = None
            self._component = 0


class ActiveStore:
    """The state store the HTTP layer reads: the active vehicle's.

    Everything is passed through to the store of the link being flown. What is
    not passed through is a listener: the SSE stream registers here once, and
    when the operator selects a vehicle on another link the listener is moved
    to that link's store and handed its snapshot at once.
    """

    _OWN = frozenset({"_target", "_lock", "_listeners", "_generation"})

    def __init__(self, store: Any) -> None:
        object.__setattr__(self, "_target", store)
        object.__setattr__(self, "_lock", threading.RLock())
        object.__setattr__(self, "_listeners", [])
        object.__setattr__(self, "_generation", 0)

    def __getattr__(self, name: str) -> Any:
        return getattr(object.__getattribute__(self, "_target"), name)

    def __setattr__(self, name: str, value: Any) -> None:
        if name in self._OWN:
            object.__setattr__(self, name, value)
        else:
            setattr(self._target, name, value)

    @property
    def target(self) -> Any:
        return self._target

    def add_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._lock:
            if fn not in self._listeners:
                self._listeners.append(fn)
            self._target.add_listener(fn)

    def remove_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._lock:
            try:
                self._listeners.remove(fn)
            except ValueError:
                pass
            self._target.remove_listener(fn)

    def version(self) -> int:
        # Two stores count their versions independently, and the SSE layer
        # reuses serialised bytes keyed on this number. Without the
        # generation a switch could hand out the other vehicle's bytes.
        return self._generation * 1_000_000_000_000 + int(self._target.version())

    def retarget(self, store: Any) -> None:
        """Follow *store* from now on, moving every listener with it."""
        with self._lock:
            old = self._target
            if store is old:
                return
            for fn in self._listeners:
                old.remove_listener(fn)
                store.add_listener(fn)
            object.__setattr__(self, "_target", store)
            object.__setattr__(self, "_generation", self._generation + 1)
            listeners = list(self._listeners)
        snapshot = store.get_snapshot()
        for fn in listeners:
            try:
                fn(snapshot)
            except Exception:  # noqa: BLE001 - one dead stream must not stop the rest
                logger.debug("fleet: listener failed on switch", exc_info=True)


class ActiveBridge:
    """The MAVLink bridge the HTTP layer commands: the active vehicle's.

    Passes everything through, like :class:`ActiveStore`, and moves the three
    kinds of subscription a long-lived stream holds (console, shell,
    parameter progress) when the active link changes.
    """

    _OWN = frozenset({"_target", "_lock", "_subs"})
    _KINDS = (
        ("console", "add_console_sub", "remove_console_sub"),
        ("shell", "add_shell_sub", "remove_shell_sub"),
        ("params", "add_param_listener", "remove_param_listener"),
    )

    def __init__(self, bridge: Any) -> None:
        object.__setattr__(self, "_target", bridge)
        object.__setattr__(self, "_lock", threading.RLock())
        object.__setattr__(self, "_subs", {kind: [] for kind, _a, _r in self._KINDS})

    def __getattr__(self, name: str) -> Any:
        return getattr(object.__getattribute__(self, "_target"), name)

    def __setattr__(self, name: str, value: Any) -> None:
        if name in self._OWN:
            object.__setattr__(self, name, value)
        else:
            setattr(self._target, name, value)

    @property
    def target(self) -> Any:
        return self._target

    def _add(self, kind: str, method: str, fn: Callable) -> Any:
        with self._lock:
            if fn not in self._subs[kind]:
                self._subs[kind].append(fn)
            return getattr(self._target, method)(fn)

    def _remove(self, kind: str, method: str, fn: Callable) -> None:
        with self._lock:
            try:
                self._subs[kind].remove(fn)
            except ValueError:
                pass
            getattr(self._target, method)(fn)

    def add_console_sub(self, fn: Callable) -> None:
        self._add("console", "add_console_sub", fn)

    def remove_console_sub(self, fn: Callable) -> None:
        self._remove("console", "remove_console_sub", fn)

    def add_shell_sub(self, fn: Callable) -> str:
        return self._add("shell", "add_shell_sub", fn)

    def remove_shell_sub(self, fn: Callable) -> None:
        self._remove("shell", "remove_shell_sub", fn)

    def add_param_listener(self, fn: Callable) -> None:
        self._add("params", "add_param_listener", fn)

    def remove_param_listener(self, fn: Callable) -> None:
        self._remove("params", "remove_param_listener", fn)

    def retarget(self, bridge: Any) -> None:
        with self._lock:
            old = self._target
            if bridge is old:
                return
            for kind, add, remove in self._KINDS:
                for fn in self._subs[kind]:
                    try:
                        getattr(old, remove)(fn)
                    except Exception:  # noqa: BLE001 - best effort on the old link
                        logger.debug("fleet: unsubscribe failed", exc_info=True)
                    getattr(bridge, add)(fn)
            object.__setattr__(self, "_target", bridge)


class FleetLink:
    """One MAVLink link: its bridge, its store, and what the operator calls it."""

    def __init__(self, link_id: str, bridge: Any, store: Any, primary: bool) -> None:
        self.id = link_id
        self.bridge = bridge
        self.store = store
        self.primary = primary


class RtcmFanout:
    """The bridge the RTK service talks to: the main one, plus every extra link.

    RTK corrections are for every aircraft in range, not only the one being
    flown. Everything but :meth:`inject_rtcm` is the main bridge's answer,
    unchanged, so with no extra link this is the main bridge.
    """

    def __init__(self, main: Any, fleet: Fleet) -> None:
        self._main = main
        self._fleet = fleet

    def __getattr__(self, name: str) -> Any:
        return getattr(self._main, name)

    def inject_rtcm(self, frame: bytes) -> bool:
        ok = bool(self._main.inject_rtcm(frame))
        for bridge in self._fleet.extra_bridges():
            try:
                if bridge.is_connected():
                    bridge.inject_rtcm(frame)
            except Exception:  # noqa: BLE001 - one radio must not stop the others
                logger.debug("fleet: RTCM to an extra link failed", exc_info=True)
        return ok

    def is_connected(self) -> bool:
        if self._main.is_connected():
            return True
        return any(b.is_connected() for b in self._fleet.extra_bridges())


class Fleet:
    """Every link, every vehicle on them, and which one is being flown."""

    def __init__(
        self,
        main_bridge: Any,
        main_store: Any,
        *,
        bridge_factory: Callable[[Any, str], Any] | None = None,
        store_factory: Callable[[], Any] | None = None,
        persist: Callable[[dict[str, Any] | None], None] | None = None,
    ) -> None:
        self._lock = threading.RLock()
        # One add_link at a time. The id and the port check are decided under
        # _lock but the link is opened outside it, so two at once (a double
        # click) took the same id: the second replaced the first in _links and
        # the first bridge kept its port and thread with nothing left to stop it.
        self._add_lock = threading.Lock()
        self._links: dict[str, FleetLink] = {
            MAIN_LINK: FleetLink(MAIN_LINK, main_bridge, main_store, primary=True),
        }
        self._active_link = MAIN_LINK
        self.bridge = ActiveBridge(main_bridge)
        self.store = ActiveStore(main_store)
        self._bridge_factory = bridge_factory
        self._store_factory = store_factory
        self._persist = persist
        self._meta: dict[str, dict[str, str]] = {}
        self._theme_key: str = ""
        self._pending_links: list[dict[str, str]] = []
        # Notifications about vehicles that are not being flown.
        self._events: list[dict[str, Any]] = []
        self._event_seq = 0
        self._event_cursor: dict[str, int] = {}
        self._event_last: dict[tuple[str, str], float] = {}
        self._was_live: dict[str, bool] = {}
        self._listeners: list[Callable[[dict[str, Any]], None]] = []
        self._listeners_lock = threading.Lock()
        self._last_published = ""
        # The publish thread and an HTTP handler's publish_now() both collect
        # events; one at a time, or the bookkeeping dicts change mid-read.
        self._publish_lock = threading.Lock()
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._created = time.monotonic()

    # ---- configuration --------------------------------------------------

    def load(self, block: Any) -> None:
        """Take the ``fleet`` config block: names, colours, and extra links.

        The links are only recorded here; :meth:`start` opens them.
        """
        clean = coerce_config(block) or {}
        with self._lock:
            self._meta = {k: dict(v) for k, v in (clean.get("vehicles") or {}).items()}
            self._theme_key = clean.get("theme_vehicle", "")
            self._pending_links = list(clean.get("links") or [])

    def to_config(self) -> dict[str, Any] | None:
        """The ``fleet`` config block as it stands now, or None for nothing."""
        with self._lock:
            links = [
                {"id": link.id, "connection": link.bridge.connection_string()}
                for link in self._links.values() if not link.primary
            ]
            meta = {k: dict(v) for k, v in self._meta.items() if v}
            theme = self._theme_key
        block: dict[str, Any] = {"links": links, "vehicles": meta, "theme_vehicle": theme}
        return coerce_config(block)

    def _save(self) -> None:
        if self._persist is None:
            return
        try:
            self._persist(self.to_config())
        except Exception:  # noqa: BLE001 - a full disk must not cost the vehicle list
            logger.exception("fleet: could not save the vehicle list")

    # ---- lifecycle ------------------------------------------------------

    def start(self) -> None:
        """Open the configured extra links and start publishing."""
        for entry in self._pending_links:
            try:
                self._open_link(entry["id"], entry["connection"])
            except Exception:  # noqa: BLE001 - one bad link must not stop the app
                logger.exception("fleet: could not open %s", entry.get("connection"))
        self._pending_links = []
        self._stop.clear()
        if self._thread is None or not self._thread.is_alive():
            self._thread = threading.Thread(
                target=self._publish_loop, name="fleet-publish", daemon=True,
            )
            self._thread.start()

    def stop(self) -> None:
        """Stop publishing and close every extra link. The main one is not ours."""
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=2)
            self._thread = None
        with self._lock:
            extras = [link for link in self._links.values() if not link.primary]
        for link in extras:
            self._close(link)

    def _close(self, link: FleetLink) -> None:
        try:
            link.bridge.stop()
        except Exception:  # noqa: BLE001 - teardown keeps going
            logger.exception("fleet: stopping %s failed", link.id)
        try:
            link.store.shutdown()
        except Exception:  # noqa: BLE001 - teardown keeps going
            logger.exception("fleet: store shutdown for %s failed", link.id)

    # ---- links ----------------------------------------------------------

    def extra_bridges(self) -> list[Any]:
        with self._lock:
            return [link.bridge for link in self._links.values() if not link.primary]

    def link(self, link_id: str) -> FleetLink | None:
        with self._lock:
            return self._links.get(link_id)

    def active_link(self) -> FleetLink:
        with self._lock:
            return self._links[self._active_link]

    def active_is_main(self) -> bool:
        with self._lock:
            return self._active_link == MAIN_LINK

    def _next_link_id(self) -> str:
        n = 2
        while f"link-{n}" in self._links:
            n += 1
        return f"link-{n}"

    def _open_link(self, link_id: str, connection: str) -> FleetLink:
        if self._bridge_factory is None or self._store_factory is None:
            raise RuntimeError("extra links are not available")
        store = self._store_factory()
        bridge = self._bridge_factory(store, connection)
        bridge.fleet_link_id = link_id
        link = FleetLink(link_id, bridge, store, primary=False)
        with self._lock:
            self._links[link_id] = link
        bridge.start()
        return link

    def add_link(self, connection: str) -> tuple[str | None, str]:
        """Open another link. Returns ``(link id, "")`` or ``(None, error)``."""
        if not isinstance(connection, str) or not connection.strip():
            return None, "connection must be a non-empty string"
        connection = connection.strip()
        with self._add_lock:
            return self._add_link_locked(connection)

    def _add_link_locked(self, connection: str) -> tuple[str | None, str]:
        with self._lock:
            if len(self._links) - 1 >= MAX_EXTRA_LINKS:
                return None, f"at most {MAX_EXTRA_LINKS} extra links"
            in_use = self.connection_in_use(connection)
            if in_use:
                return None, f"{connection} is already open as {in_use}"
            main = self._links[MAIN_LINK].bridge
            try:
                main.validate_connection(connection)
            except ValueError as exc:
                return None, str(exc)
            link_id = self._next_link_id()
        try:
            self._open_link(link_id, connection)
        except Exception as exc:  # noqa: BLE001 - reported to the operator
            logger.exception("fleet: could not open %s", connection)
            return None, f"could not open the link: {exc}"
        self._withdraw_suggestion(connection)
        self._save()
        self.publish_now()
        return link_id, ""

    def _withdraw_suggestion(self, connection: str) -> None:
        """The device auto-connect offered is now a link of its own: stop offering it."""
        from .serial_ports import same_port, split_serial_connection
        if not connection.startswith("serial:"):
            return
        store = self._links[MAIN_LINK].store
        try:
            offer = store.get_snapshot().get("link_suggestion") or {}
            offered = str(offer.get("connection_string", "") or "")
            if offered.startswith("serial:") and same_port(
                split_serial_connection(offered)[0], split_serial_connection(connection)[0],
            ):
                store.update(link_suggestion=None)
        except Exception:  # noqa: BLE001 - an offer left standing is harmless
            logger.debug("fleet: could not withdraw the offer", exc_info=True)

    def remove_link(self, link_id: str) -> str:
        """Close an extra link, and forget the vehicle on it. Returns "" or why not."""
        with self._lock:
            link = self._links.get(link_id)
            if link is None:
                return "no such link"
            if link.primary:
                return "the main link is closed from the LINK tab"
            del self._links[link_id]
            self._forget_link_meta(link_id)
            if self._active_link == link_id:
                self._switch_link(self._fallback_link())
        self._close(link)
        self._save()
        self.publish_now()
        return ""

    def remove_vehicle(self, key: Any, confirm_armed: bool = False) -> tuple[str, int]:
        """Take a vehicle off the list: close its link, so nothing of it is shown.

        An extra link is closed for good. The main link is disconnected and
        left closed (the caller tells auto-connect); the LINK tab opens it
        again. An armed vehicle that is still heard is only removed with
        *confirm_armed*: it keeps flying, and the station no longer sees it.
        Returns ``("", 200)`` or ``(why not, HTTP status)``.
        """
        parsed = parse_vehicle_key(key)
        if parsed is None:
            return "unknown vehicle", 400
        link_id, system_id = parsed
        link = self.link(link_id)
        if link is None:
            return "that vehicle's link is not open", 404
        entry = self._entry_of(link, system_id)
        if entry is None:
            return "that vehicle is not on the list", 404
        if entry.get("armed") and not entry.get("stale") and not confirm_armed:
            return ("The vehicle is armed. Removing it closes its link: it keeps "
                    "flying and the station no longer sees it."), 409
        if not link.primary:
            error = self.remove_link(link_id)
            return (error, 400) if error else ("", 200)
        try:
            link.bridge.stop()
        except Exception:  # noqa: BLE001 - the list still lets go of it
            logger.exception("fleet: disconnecting the main link failed")
        try:
            link.bridge.fleet_tracker.forget()
        except Exception:  # noqa: BLE001
            logger.debug("fleet: could not clear the main link's list", exc_info=True)
        try:
            link.store.forget_vehicle()
            link.store.update(vehicle_id="")
        except Exception:  # noqa: BLE001
            logger.debug("fleet: could not clear the main link's store", exc_info=True)
        with self._lock:
            self._forget_key(vehicle_key(link_id, system_id))
            if self._active_link == link_id:
                self._switch_link(self._fallback_link(exclude=link_id))
        logger.info("fleet: removed %s", vehicle_key(link_id, system_id))
        self._save()
        self.publish_now()
        return "", 200

    def _entry_of(self, link: FleetLink, system_id: int) -> dict[str, Any] | None:
        try:
            systems = link.bridge.fleet_tracker.systems()
        except Exception:  # noqa: BLE001
            return None
        return next((e for e in systems if e.get("sysid") == system_id), None)

    def _fallback_link(self, exclude: str = "") -> str:
        """The link to fly once the flown one is gone: one with a live vehicle."""
        with self._lock:
            candidates = [l for l in self._links.values() if l.id != exclude]
        for link in candidates:
            if self._controlled(link) is not None:
                return link.id
        return MAIN_LINK

    def _forget_key(self, key: str) -> None:
        self._meta.pop(key, None)
        self._was_live.pop(key, None)
        if self._theme_key == key:
            self._theme_key = ""

    def _forget_link_meta(self, link_id: str) -> None:
        """Names and colours of a closed link's vehicles: the next link of that
        id may carry another aircraft, which must not inherit them."""
        prefix = f"{link_id}:"
        # Copies first: the publish thread writes _was_live without this lock.
        for key in [k for k in list(self._meta) if k.startswith(prefix)]:
            self._forget_key(key)
        for key in [k for k in list(self._was_live) if k.startswith(prefix)]:
            self._was_live.pop(key, None)
        if self._theme_key.startswith(prefix):
            self._theme_key = ""
        self._event_cursor.pop(link_id, None)

    def connection_in_use(self, connection: str, except_link: str = "") -> str:
        """The label of another link already open on *connection*, or "".

        A serial port is the same port at any baud rate: two links on one
        port would each read half the frames.
        """
        from .serial_ports import same_port, split_serial_connection
        device = split_serial_connection(connection)[0] if connection.startswith("serial:") else ""
        with self._lock:
            for link in self._links.values():
                if link.id == except_link:
                    continue
                other = link.bridge.connection_string()
                if other == connection:
                    return self.link_label(link.id)
                if device and other.startswith("serial:"):
                    if same_port(device, split_serial_connection(other)[0]):
                        return self.link_label(link.id)
        return ""

    def link_changed(self) -> None:
        """An extra link was pointed somewhere else from the LINK tab: save it."""
        self._save()
        self.publish_now()

    def extra_serial_devices(self) -> list[str]:
        """The serial ports the extra links hold, for auto-connect to leave alone."""
        devices: list[str] = []
        for bridge in self.extra_bridges():
            try:
                device = bridge.serial_device()
            except Exception:  # noqa: BLE001
                device = ""
            if device:
                devices.append(device)
        return devices

    def link_label(self, link_id: str) -> str:
        if link_id == MAIN_LINK:
            return "Main link"
        return "Link " + link_id.split("-", 1)[1]

    # ---- vehicles -------------------------------------------------------

    def set_meta(self, key: str, name: Any = None, color: Any = None) -> str:
        """Name or colour a vehicle. ``None`` leaves a field as it is."""
        if parse_vehicle_key(key) is None:
            return "unknown vehicle"
        with self._lock:
            entry = dict(self._meta.get(key, {}))
            if name is not None:
                cleaned = clean_name(name)
                if cleaned:
                    entry["name"] = cleaned
                else:
                    entry.pop("name", None)
            if color is not None:
                cleaned = clean_color(color)
                if color and not cleaned:
                    return "colour must be #RRGGBB"
                if cleaned:
                    entry["color"] = cleaned
                else:
                    entry.pop("color", None)
            if entry:
                self._meta[key] = entry
            else:
                self._meta.pop(key, None)
            self._trim_meta()
        self._save()
        self.publish_now()
        return ""

    def _trim_meta(self) -> None:
        while len(self._meta) > MAX_REMEMBERED:
            unnamed = [k for k, v in self._meta.items() if not v.get("name")]
            victim = unnamed[0] if unnamed else next(iter(self._meta))
            del self._meta[victim]

    def select(self, key: Any) -> str:
        """Fly the vehicle *key* from now on. Returns "" or why not."""
        parsed = parse_vehicle_key(key)
        if parsed is None:
            return "unknown vehicle"
        link_id, system_id = parsed
        with self._lock:
            link = self._links.get(link_id)
            if link is None:
                return "that vehicle's link is not open"
            if self._entry_of(link, system_id) is None:
                return "that vehicle is not on the list"
            current = self._controlled(link)
            if current is not None and current != system_id:
                return f"this link flies system {current}"
            self._switch_link(link_id)
        self.publish_now()
        return ""

    def _switch_link(self, link_id: str) -> None:
        with self._lock:
            if link_id == self._active_link:
                return
            link = self._links[link_id]
            self._active_link = link_id
            self.bridge.retarget(link.bridge)
            self.store.retarget(link.store)
        logger.info("fleet: now flying the vehicle on %s", self.link_label(link_id))

    def _controlled(self, link: FleetLink) -> int | None:
        try:
            return link.bridge.controlled_system()
        except Exception:  # noqa: BLE001 - a bridge without it controls nothing
            return None

    def default_name(self, link_id: str, system_id: int) -> str:
        if link_id == MAIN_LINK:
            return f"Vehicle {system_id}"
        return f"Vehicle {system_id} ({self.link_label(link_id)})"

    def snapshot(self) -> dict[str, Any]:
        """The vehicle list and the links, as the browser receives them."""
        with self._lock:
            links = list(self._links.values())
            active_link = self._active_link
        vehicles: list[dict[str, Any]] = []
        link_rows: list[dict[str, Any]] = []
        active_key = ""
        for link in links:
            controlled = self._controlled(link)
            try:
                state = link.store.get_snapshot()
            except Exception:  # noqa: BLE001
                state = {}
            try:
                systems = link.bridge.fleet_tracker.systems()
            except Exception:  # noqa: BLE001
                systems = []
            if link.id == active_link:
                # A vehicle that went quiet is still the one being flown.
                chosen = controlled or (systems[0]["sysid"] if systems else None)
                if chosen:
                    active_key = vehicle_key(link.id, chosen)
            link_rows.append({
                "id": link.id,
                "label": self.link_label(link.id),
                "primary": link.primary,
                "connection": link.bridge.connection_string(),
                "status": state.get("link_status", "disconnected"),
                "error": state.get("link_error", ""),
                "vehicles": len(systems),
            })
            for entry in systems:
                key = vehicle_key(link.id, entry["sysid"])
                row = dict(entry)
                row.update({
                    "id": key,
                    "link": link.id,
                    "link_label": self.link_label(link.id),
                    "controlled": controlled == entry["sysid"],
                })
                vehicles.append(row)
        self._assign_colors(vehicles, active_key)
        with self._lock:
            meta = {k: dict(v) for k, v in self._meta.items()}
        for row in vehicles:
            entry = meta.get(row["id"], {})
            row["name"] = entry.get("name") or self.default_name(row["link"], row["sysid"])
            row["custom_name"] = bool(entry.get("name"))
            row["color"] = entry.get("color", "")
            row["active"] = row["id"] == active_key
        with self._lock:
            events = [dict(e) for e in self._events[-20:]]
        try:
            suggestion = self._links[MAIN_LINK].store.get_snapshot().get("link_suggestion")
        except Exception:  # noqa: BLE001
            suggestion = None
        return {
            "active": active_key,
            "active_link": active_link,
            "links": link_rows,
            "vehicles": vehicles,
            "palette": list(PALETTE),
            "suggestion": suggestion if isinstance(suggestion, dict) else None,
            "events": events,
            "event_seq": self._event_seq,
        }

    def _assign_colors(self, vehicles: list[dict[str, Any]], active_key: str) -> None:
        """Give each vehicle that has none a colour of its own, once.

        Only when there is more than one to tell apart: a single aircraft
        keeps the theme's colour and nothing is written. When a second one
        shows up, the one being flown keeps the theme's colour from then on
        and every other gets a palette colour. Both stick (they are saved),
        so an aircraft is the same colour tomorrow.
        """
        live = [v for v in vehicles if not v.get("stale")]
        if len(live) < 2:
            return
        if not active_key and time.monotonic() - self._created < COLOR_GRACE_S:
            # The station has not latched the vehicle it flies yet, and that
            # is the one the theme's colour is for.
            return
        changed = False
        with self._lock:
            keys = [row["id"] for row in live]
            if not self._theme_key:
                self._theme_key = active_key if active_key in keys else keys[0]
                changed = True
            used = {m.get("color") for m in self._meta.values() if m.get("color")}
            for key in keys:
                if key == self._theme_key or self._meta.get(key, {}).get("color"):
                    continue
                free = [c for c in PALETTE if c not in used]
                color = free[0] if free else PALETTE[len(used) % len(PALETTE)]
                self._meta.setdefault(key, {})["color"] = color
                used.add(color)
                changed = True
            if changed:
                self._trim_meta()
        if changed:
            self._save()

    # ---- publishing -----------------------------------------------------

    def add_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._listeners_lock:
            if fn not in self._listeners:
                self._listeners.append(fn)

    def remove_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._listeners_lock:
            try:
                self._listeners.remove(fn)
            except ValueError:
                pass

    def publish_now(self) -> None:
        self._publish(force=True)

    def _flown(self) -> tuple[str, int | None]:
        with self._lock:
            link = self._links[self._active_link]
        return link.id, self._controlled(link)

    def _add_event(self, key: str, level: str, text: str, now: float) -> None:
        last = self._event_last.get((key, text))
        if last is not None and now - last < EVENT_REPEAT_S:
            return
        self._event_last[(key, text)] = now
        if len(self._event_last) > 4 * MAX_EVENTS:
            cutoff = now - EVENT_REPEAT_S
            self._event_last = {k: t for k, t in self._event_last.items() if t >= cutoff}
        with self._lock:
            self._event_seq += 1
            self._events.append({"seq": self._event_seq, "vehicle": key, "level": level, "text": text})
            if len(self._events) > MAX_EVENTS:
                del self._events[:-MAX_EVENTS]

    def collect_events(self, now: float | None = None) -> None:
        """Turn what the vehicles not being flown said into notifications.

        Two kinds: a STATUSTEXT at warning level or worse (low battery, a
        failsafe, a preflight failure), and a vehicle that stopped
        heartbeating or came back. The vehicle being flown is left out: the
        status bar and the notification board already have all of it.
        """
        stamp = time.monotonic() if now is None else now
        flown_link, flown_system = self._flown()
        with self._lock:
            links = list(self._links.values())
        for link in links:
            tracker = getattr(link.bridge, "fleet_tracker", None)
            if tracker is None:
                continue
            cursor = self._event_cursor.get(link.id)
            last, fresh = tracker.events_since(cursor if cursor is not None else 1 << 62)
            if cursor is None:
                # What was said before the list started is history.
                self._event_cursor[link.id] = last
                fresh = []
            else:
                self._event_cursor[link.id] = last
            for system, severity, text in fresh:
                if link.id == flown_link and system == flown_system:
                    continue
                key = vehicle_key(link.id, system)
                level = "critical" if severity <= 3 else "warning"
                self._add_event(key, level, text, stamp)
            for entry in tracker.systems(now=None):
                key = vehicle_key(link.id, entry["sysid"])
                live = not entry["stale"]
                was = self._was_live.get(key)
                self._was_live[key] = live
                if was is None or was == live:
                    continue
                if link.id == flown_link and entry["sysid"] == flown_system:
                    continue
                if live:
                    self._add_event(key, "info", "heard again", stamp)
                else:
                    self._add_event(key, "critical", "no heartbeat for 5 s, link lost", stamp)

    def _publish(self, force: bool = False) -> None:
        with self._publish_lock:
            self._publish_locked(force)

    def _publish_locked(self, force: bool) -> None:
        try:
            self.collect_events()
        except Exception:  # noqa: BLE001 - notifications never stop the list
            logger.debug("fleet: collecting events failed", exc_info=True)
        with self._listeners_lock:
            listeners = list(self._listeners)
        if not listeners and not force:
            return
        snapshot = self.snapshot()
        text = json.dumps(snapshot, sort_keys=True)
        if not force and text == self._last_published:
            return
        self._last_published = text
        for fn in listeners:
            try:
                fn(snapshot)
            except Exception:  # noqa: BLE001 - one dead stream must not stop the rest
                logger.debug("fleet: listener failed", exc_info=True)

    def _publish_loop(self) -> None:
        while not self._stop.wait(PUBLISH_INTERVAL_S):
            try:
                self._publish()
            except Exception:  # noqa: BLE001 - the loop outlives a bad snapshot
                logger.exception("fleet: publish failed")
