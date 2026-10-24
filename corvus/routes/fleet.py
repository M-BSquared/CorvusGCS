"""Several vehicles: the list, names and colours, which one is flown, extra links.

See corvus/fleet.py. With no fleet wired (the unit-test fixtures) the list is
built from the one bridge the handler has, so the page still answers.
"""
from __future__ import annotations

import functools
import logging
from collections.abc import Callable
from typing import Any
from urllib.parse import parse_qs, urlparse

from ..fleet import MAIN_LINK, parse_vehicle_key
from ..http_routes import route

logger = logging.getLogger("corvus.server")

# Endpoints whose service is bound to the main link: the on-board log list,
# flashing over USB and the SiK radio on that link. Run while a vehicle on
# another link is selected, they would act on an aircraft the operator is not
# looking at, so they say so instead.
MAIN_LINK_ONLY: frozenset[str] = frozenset({
    "/api/logs/refresh",
    "/api/logs/download",
    "/api/logs/erase",
    "/api/firmware/flash",
    "/api/sik/load",
    "/api/sik/save",
    "/api/sik/reset",
})

MAIN_LINK_ONLY_ERROR = (
    "This works with the vehicle on the main link. "
    "Select that vehicle in the vehicle list first."
)


def targets_vehicle(handler: Callable[..., None]) -> Callable[..., None]:
    """Let a vehicle endpoint act on any vehicle of the fleet, not only the flown one.

    A request naming ``vehicle`` ("<link>:<system id>", the id ``GET
    /api/fleet`` lists), in the JSON body of a POST or the query of a GET, runs
    against that vehicle's bridge and store, so a plugin can fly several
    aircraft without moving the operator's selection. Without it nothing
    changes: the handler talks to the selected vehicle as it always has.

    The swap is an instance attribute over the class-level proxies, set for
    this request only; a handler instance serves one request at a time.
    """
    @functools.wraps(handler)
    def wrapper(self: Any, *args: Any) -> None:
        if args and isinstance(args[0], dict):
            key = args[0].get("vehicle")
        else:
            query = parse_qs(urlparse(getattr(self, "path", "") or "").query)
            key = (query.get("vehicle") or [None])[0]
        if key is None or key == "":
            handler(self, *args)
            return
        target = self._vehicle_target(key)
        if target is None:
            return
        own = {name: self.__dict__[name] for name in ("mavlink", "store")
               if name in self.__dict__}
        self.mavlink, self.store = target
        try:
            handler(self, *args)
        finally:
            for name in ("mavlink", "store"):
                if name in own:
                    setattr(self, name, own[name])
                else:
                    self.__dict__.pop(name, None)
    return wrapper


class FleetRoutes:
    fleet: Any = None

    def _vehicle_target(self, key: Any) -> tuple[Any, Any] | None:
        """The bridge and store that fly vehicle *key*, or None once refused.

        Refused with 400 for an id that is not one, 409 for a vehicle whose
        link is closed or flies another system (a command meant for one
        aircraft must never reach a different one), and 503 while its link
        has lost it.
        """
        parsed = parse_vehicle_key(key)
        if parsed is None:
            self._send_json({"ok": False, "error": "unknown vehicle"}, 400)
            return None
        link_id, system_id = parsed
        fleet = self.fleet
        if fleet is None:
            if link_id != MAIN_LINK:
                self._send_json({"ok": False, "error": "that vehicle's link is not open"}, 409)
                return None
            bridge, store = self.mavlink, self.store
        else:
            link = fleet.link(link_id)
            if link is None:
                self._send_json({"ok": False, "error": "that vehicle's link is not open"}, 409)
                return None
            bridge, store = link.bridge, link.store
        controlled = None
        if bridge is not None:
            try:
                controlled = bridge.controlled_system()
            except Exception:  # noqa: BLE001 - a bridge without it controls nothing
                controlled = None
        if controlled is not None and controlled != system_id:
            self._send_json(
                {"ok": False, "error": f"this link flies system {controlled}"}, 409)
            return None
        if controlled is None and fleet is not None:
            self._send_json({"ok": False, "error": "that vehicle is not connected"}, 503)
            return None
        return bridge, store

    def _fleet_refuses(self, path: str) -> bool:
        """Answer 409 for a main-link-only endpoint while another link is flown."""
        fleet = self.fleet
        if fleet is None or path not in MAIN_LINK_ONLY or fleet.active_is_main():
            return False
        self._send_json({"ok": False, "error": MAIN_LINK_ONLY_ERROR}, 409)
        return True

    def _single_vehicle_snapshot(self) -> dict[str, Any]:
        snap = self.store.get_snapshot() if self.store is not None else {}
        return {"active": snap.get("vehicle_id", ""), "active_link": "main",
                "links": [], "vehicles": [], "palette": []}

    @route("GET", "/api/fleet")
    def _api_fleet(self) -> None:
        """Every vehicle heard, every link, and which vehicle is flown."""
        if self.fleet is None:
            self._send_json(self._single_vehicle_snapshot())
            return
        self._send_json(self.fleet.snapshot())

    @route("POST", "/api/fleet/select")
    def _api_fleet_select(self, payload: dict) -> None:
        """Fly the vehicle ``id`` ("<link>:<system id>") from now on."""
        if self.fleet is None:
            self._send_json({"ok": False, "error": "only one vehicle is supported here"}, 503)
            return
        error = self.fleet.select(payload.get("id"))
        if error:
            self._send_json({"ok": False, "error": error}, 400)
            return
        self._send_json({"ok": True, "fleet": self.fleet.snapshot()})

    @route("POST", "/api/fleet/vehicle")
    def _api_fleet_vehicle(self, payload: dict) -> None:
        """Name a vehicle or give it a colour. An empty value clears it."""
        if self.fleet is None:
            self._send_json({"ok": False, "error": "only one vehicle is supported here"}, 503)
            return
        name = payload.get("name")
        color = payload.get("color")
        if name is not None and not isinstance(name, str):
            self._send_json({"ok": False, "error": "name must be text"}, 400)
            return
        if color is not None and not isinstance(color, str):
            self._send_json({"ok": False, "error": "colour must be #RRGGBB"}, 400)
            return
        error = self.fleet.set_meta(payload.get("id"), name=name, color=color)
        if error:
            self._send_json({"ok": False, "error": error}, 400)
            return
        self._send_json({"ok": True, "fleet": self.fleet.snapshot()})

    @route("POST", "/api/fleet/links/add")
    def _api_fleet_link_add(self, payload: dict) -> None:
        """Open another MAVLink link, for a vehicle on a radio of its own."""
        if self.fleet is None:
            self._send_json({"ok": False, "error": "only one link is supported here"}, 503)
            return
        link_id, error = self.fleet.add_link(payload.get("connection"))
        if link_id is None:
            self._send_json({"ok": False, "error": error}, 400)
            return
        self._send_json({"ok": True, "id": link_id, "fleet": self.fleet.snapshot()})

    @route("POST", "/api/fleet/links/remove")
    def _api_fleet_link_remove(self, payload: dict) -> None:
        """Close an extra link. The main link is closed from the LINK tab."""
        if self.fleet is None:
            self._send_json({"ok": False, "error": "only one link is supported here"}, 503)
            return
        error = self.fleet.remove_link(payload.get("id"))
        if error:
            self._send_json({"ok": False, "error": error}, 400)
            return
        self._send_json({"ok": True, "fleet": self.fleet.snapshot()})

    @route("POST", "/api/fleet/vehicle/remove")
    def _api_fleet_vehicle_remove(self, payload: dict) -> None:
        """Take a vehicle off the list by closing its link.

        An armed vehicle that is still heard needs ``confirm_armed: true``. On
        the main link the link is disconnected and auto-connect is told to
        leave it closed, exactly as the LINK tab's Disconnect does.
        """
        if self.fleet is None:
            self._send_json({"ok": False, "error": "only one vehicle is supported here"}, 503)
            return
        key = payload.get("id")
        confirm = payload.get("confirm_armed", False)
        if not isinstance(confirm, bool):
            self._send_json({"ok": False, "error": "confirm_armed must be true or false"}, 400)
            return
        error, status = self.fleet.remove_vehicle(key, confirm_armed=confirm)
        if error:
            self._send_json({"ok": False, "error": error}, status)
            return
        if isinstance(key, str) and key.startswith("main:"):
            self._note_manual_link("")
        self._send_json({"ok": True, "fleet": self.fleet.snapshot()})
