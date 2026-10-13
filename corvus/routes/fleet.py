"""Several vehicles: the list, names and colours, which one is flown, extra links.

See corvus/fleet.py. With no fleet wired (the unit-test fixtures) the list is
built from the one bridge the handler has, so the page still answers.
"""
from __future__ import annotations

import logging
from typing import Any

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


class FleetRoutes:
    fleet: Any = None

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

    @route("POST", "/api/fleet/settings")
    def _api_fleet_settings(self, payload: dict) -> None:
        """``shared_links``: list every vehicle a link carries, or only its own."""
        if self.fleet is None:
            self._send_json({"ok": False, "error": "only one vehicle is supported here"}, 503)
            return
        shared = payload.get("shared_links")
        if not isinstance(shared, bool):
            self._send_json({"ok": False, "error": "shared_links must be true or false"}, 400)
            return
        self.fleet.set_shared_links(shared)
        self._send_json({"ok": True, "fleet": self.fleet.snapshot()})
