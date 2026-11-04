"""The geofence area: kept on this station, put on the vehicle on request.

CorvusHandler inherits GeofenceRoutes, and the @route registry wires them.
The parameters that decide what the fence does are not here: they ride
``GET /api/safety`` in the Geofence section of the connected stack's schema.
"""
from __future__ import annotations

import logging
import os
from typing import Any

from .. import geofence
from ..http_routes import route

logger = logging.getLogger("corvus.server")


class GeofenceRoutes:
    # Where the area is kept. None is the default: next to the config file
    # when there is one, else ~/.corvus. Tests point it at a temp file.
    geofence_path: str | None = None

    def _geofence_file(self) -> str:
        if self.geofence_path:
            return self.geofence_path
        config_path = getattr(self, "config_path", None)
        if config_path:
            return os.path.join(os.path.dirname(os.path.abspath(config_path)),
                                geofence.FILE_NAME)
        return geofence.default_path()

    def _geofence_payload(self, stored: dict[str, Any]) -> dict[str, Any]:
        """The stored area, plus whether it is the one on the vehicle.

        ``on_vehicle`` is None when this link has not transferred a fence, so
        the page says "not known" rather than guessing.
        """
        bridge = getattr(self, "mavlink", None)
        reader = getattr(bridge, "vehicle_fence", None)
        on_vehicle = reader() if callable(reader) else None
        return {
            "ok": True,
            "polygon": stored["polygon"],
            "show_on_map": stored["show_on_map"],
            "on_vehicle": None if on_vehicle is None else on_vehicle == stored["polygon"],
            "vehicle_has_fence": None if on_vehicle is None else bool(on_vehicle),
            "max_vertices": geofence.MAX_VERTICES,
        }

    @route("GET", "/api/geofence")
    def _api_geofence(self) -> None:
        """The area drawn on this station and whether the map shows it."""
        self._send_json(self._geofence_payload(geofence.load(self._geofence_file())))

    @route("POST", "/api/geofence/save")
    def _api_geofence_save(self, payload: dict) -> None:
        """Keep the area and the map toggle. Either may be left out."""
        stored = geofence.load(self._geofence_file())
        if "polygon" in payload:
            polygon, error = geofence.validate_polygon(payload.get("polygon"))
            if polygon is None:
                self._send_json({"ok": False, "error": error}, 400)
                return
            stored["polygon"] = polygon
        if "show_on_map" in payload:
            show = payload.get("show_on_map")
            if not isinstance(show, bool):
                self._send_json({"ok": False, "error": "show_on_map must be true or false"}, 400)
                return
            stored["show_on_map"] = show
        try:
            geofence.save(stored, self._geofence_file())
        except OSError as exc:
            logger.exception("could not save the geofence")
            self._send_json({"ok": False, "error": f"could not save: {exc}"}, 500)
            return
        self._send_json(self._geofence_payload(stored))

    @route("POST", "/api/geofence/upload")
    def _api_geofence_upload(self, payload: dict) -> None:
        """Put the stored area on the vehicle, replacing the fence it holds.

        The area is the one saved here, not one carried in the request, so
        what the Home map shows is what was uploaded.
        """
        del payload
        stored = geofence.load(self._geofence_file())
        if not stored["polygon"]:
            self._send_json({"ok": False, "error": "no area has been drawn"}, 400)
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if not self.mavlink.upload_fence(stored["polygon"]):
            self._send_mission_failure("geofence upload failed")
            return
        self._send_json(self._geofence_payload(stored))

    @route("POST", "/api/geofence/clear")
    def _api_geofence_clear(self, payload: dict) -> None:
        """Remove the fence from the vehicle. The area drawn here is kept."""
        del payload
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if not self.mavlink.clear_fence():
            self._send_mission_failure("geofence clear failed")
            return
        self._send_json(self._geofence_payload(geofence.load(self._geofence_file())))
