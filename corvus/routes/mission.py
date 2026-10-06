"""Mission planning: files, upload and download.

Route handlers moved out of corvus/server.py. CorvusHandler inherits
MissionRoutes, and the @route registry wires them exactly as before.
"""
from __future__ import annotations

import logging
from .. import ardupilot_safety, autopilot, mission
from ..http_paths import _missions_dir
from ..http_routes import route
from ..mavlink_bridge import FLY_TO_MAX_POINTS

logger = logging.getLogger("corvus.server")


class MissionRoutes:
    @route("GET", "/api/mission/return")
    def _api_mission_return(self) -> None:
        """How the connected vehicle flies a Return, for the planner to draw.

        A Return is not flown at a height the plan sets: the vehicle climbs,
        comes home and lands (or circles) at heights of its own. The planner
        used to draw it at the height of the last point, which PX4's default
        return height is often well above. The parameter names belong to the Safety
        page's schema for the connected stack, which turns them into one shape
        for both. Always 200; ``connected`` says whether anything answered.
        """
        if self.mavlink is None:
            self._send_json({"connected": False})
            return
        schema = self._schema("safety")
        ardupilot = schema is ardupilot_safety
        vehicle = autopilot.vehicle_class(self._vehicle_type_id())
        try:
            values = self._fetch_page_params(
                schema.return_param_names(vehicle) if ardupilot else schema.return_param_names())
        except Exception:  # noqa: BLE001 - a read must never 500 the page
            logger.exception("return parameter fetch failed")
            self._send_json({"connected": False})
            return
        payload = (schema.return_profile(values, vehicle) if ardupilot
                   else schema.return_profile(values))
        payload["connected"] = bool(values)
        self._send_json(payload)

    @route("POST", "/api/mission/upload")
    def _api_mission_upload(self, payload: dict) -> None:
        """Upload a planned mission to the vehicle, optionally starting it.

        ``start`` is a separate flag rather than a separate call so the two
        cannot be reordered by a slow link: an operator who asked to fly gets
        the mission on board and started, and one who did not gets a vehicle
        holding a route it has not been told to run.
        """
        plan, error = mission.validate_plan(payload.get("plan"))
        if plan is None:
            self._send_json({"ok": False, "error": error}, 400)
            return
        if not plan["items"]:
            self._send_json({"ok": False, "error": "the mission is empty"}, 400)
            return
        start = payload.get("start")
        if start is not None and not isinstance(start, bool):
            self._send_json({"ok": False, "error": "start must be true or false"}, 400)
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 400)
            return

        items = mission.plan_to_items(plan)
        if not self.mavlink.upload_mission_plan(items):
            self._send_mission_failure("mission upload failed")
            return
        # Which state of the vehicle's mission this plan now is, so the page
        # can follow its progress and stop the moment it no longer is.
        store = getattr(self, "store", None)
        synced = ({"revision": store.get_snapshot().get("mission_revision")}
                  if store is not None else {})
        if not start:
            self._send_json({"ok": True, "items": len(items), "started": False, **synced})
            return
        if not self.mavlink.start_mission(len(items)):
            self._send_mission_failure("mission start failed")
            return
        self._send_json({"ok": True, "items": len(items), "started": True, **synced})

    @route("POST", "/api/mission/start")
    def _api_mission_start(self, payload: dict) -> None:
        """Run the mission already uploaded to the vehicle."""
        count = payload.get("items")
        if isinstance(count, bool) or not isinstance(count, int) or count <= 0:
            self._send_json({"ok": False, "error": "items must be a positive integer"}, 400)
            return
        if count > FLY_TO_MAX_POINTS:
            self._send_json(
                {"ok": False,
                 "error": f"items must be at most {FLY_TO_MAX_POINTS}"}, 400)
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 400)
            return
        if not self.mavlink.start_mission(count):
            self._send_mission_failure("mission start failed")
            return
        self._send_json({"ok": True})

    @route("POST", "/api/mission/clear")
    def _api_mission_clear(self, payload: dict) -> None:
        """Wipe the mission stored on the vehicle."""
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 400)
            return
        if not self.mavlink.clear_mission():
            self._send_mission_failure("mission clear failed")
            return
        self._send_json({"ok": True})

    @route("POST", "/api/mission/download")
    def _api_mission_download(self, payload: dict) -> None:
        """Read the mission the vehicle holds, as a plan the Mission page can draw.

        Answers ``{ok, plan, count, items, plan_index, skipped, adjusted,
        revision, error}``. ``plan`` is None when not even the part the planner
        understands makes a valid plan, and ``error`` says why; ``skipped``
        names what the planner could not draw, which an upload of the plan
        would remove from the vehicle.
        """
        del payload
        download = getattr(self.mavlink, "download_mission", None)
        if self.mavlink is None or download is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        result = download()
        if result is None:
            self._send_mission_failure("mission download failed")
            return
        self._send_json(dict(result, ok=True))

    def _send_mission_failure(self, fallback: str) -> None:
        """Report why the bridge refused, with the status that says whose fault.

        503 for a link that is not there (try again when it is), 409 for a
        vehicle that answered and said no.
        """
        error = (self.mavlink.get_last_command_error() if self.mavlink else "") or fallback
        status = 503 if "DISCONNECTED" in error else 409
        self._send_json({"ok": False, "error": error}, status)

    @route("GET", "/api/mission/plans")
    def _api_mission_plans(self) -> None:
        """List the saved mission plans, newest first."""
        directory = _missions_dir(self._live_config())
        self._send_json({"dir": directory, "plans": mission.list_plans(directory)})

    @route("POST", "/api/mission/plans/save")
    def _api_mission_plans_save(self, payload: dict) -> None:
        """Save a plan under its (sanitized) name, overwriting a namesake."""
        plan, error = mission.validate_plan(payload.get("plan"))
        if plan is None:
            self._send_json({"ok": False, "error": error}, 400)
            return
        name = mission.safe_plan_name(payload.get("name")) or plan["name"]
        if not name:
            self._send_json({"ok": False, "error": "a mission needs a name"}, 400)
            return
        directory = _missions_dir(self._live_config())
        try:
            stored = mission.write_plan(directory, name, plan)
        except OSError as exc:
            logger.exception("could not save mission %r", name)
            self._send_json({"ok": False, "error": f"could not save: {exc}"}, 500)
            return
        self._send_json({"ok": True, "name": stored, "dir": directory})

    @route("POST", "/api/mission/plans/load")
    def _api_mission_plans_load(self, payload: dict) -> None:
        """Read one saved plan back, re-validated on the way out."""
        name = mission.safe_plan_name(payload.get("name"))
        if not name:
            self._send_json({"ok": False, "error": "name must be a mission name"}, 400)
            return
        plan = mission.read_plan(_missions_dir(self._live_config()), name)
        if plan is None:
            self._send_json({"ok": False, "error": f"no saved mission named {name!r}"}, 404)
            return
        self._send_json({"ok": True, "name": name, "plan": plan})

    @route("POST", "/api/mission/plans/remove")
    def _api_mission_plans_remove(self, payload: dict) -> None:
        """Delete one saved plan. Deleting what is gone is a success."""
        name = mission.safe_plan_name(payload.get("name"))
        if not name:
            self._send_json({"ok": False, "error": "name must be a mission name"}, 400)
            return
        if not mission.remove_plan(_missions_dir(self._live_config()), name):
            self._send_json({"ok": False, "error": "could not delete the mission"}, 500)
            return
        self._send_json({"ok": True})
