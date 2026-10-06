"""RTK corrections.

Route handlers moved out of corvus/server.py. CorvusHandler inherits
RtkRoutes, and the @route registry wires them exactly as before.
"""
from __future__ import annotations

import logging
from .. import rtk, rtk_service
from ..config import default_config_path, save_config
from ..http_routes import route

logger = logging.getLogger("corvus.server")


class RtkRoutes:
    @route("GET", "/api/rtk/status")
    def _api_rtk_status(self) -> None:
        """What the base is doing, in one read.

        Always 200 so the page can poll it: a service that could not be built
        is reported as one that is off, with the reason on it, rather than as
        an error the page has to render a banner for.
        """
        if self.rtk is None:
            self._send_json({
                "enabled": False,
                "state": rtk_service.STATE_OFF,
                "message": "RTK corrections are unavailable in this build",
                "error": "", "warning": "",
                "source": "usb", "mode": "survey", "device": "", "baud": 0,
                "receiver": None, "receiver_label": "",
                "survey": None, "survey_progress": 0,
                "survey_target": {
                    "accuracy": rtk.DEFAULT_SURVEY_ACCURACY_M,
                    "duration": rtk.DEFAULT_SURVEY_DURATION_S,
                },
                "frames": 0, "frame_bytes": 0, "crc_errors": 0,
                "source_age": None, "uptime": None, "messages": {},
                "injected": {"bytes": 0, "messages": 0, "dropped": 0, "age": None},
                "link_ready": False,
                "vehicle": {"fix": "", "satellites": 0, "hdop": 0},
                "ports": [],
                "settings": rtk.defaults(),
                "defaults": rtk.defaults(),
            })
            return
        try:
            self._send_json(self.rtk.status())
        except Exception as exc:  # noqa: BLE001 - a status read must never 500
            logger.exception("RTK status failed")
            self._send_json({"ok": False, "error": f"RTK status failed: {exc}"}, 500)

    @route("POST", "/api/rtk/settings")
    def _api_rtk_settings(self, payload: dict) -> None:
        """Replace the RTK settings, persist them, and restart the session.

        The whole block is replaced rather than merged, because the page sends
        the whole form and a merge cannot express "clear the NTRIP username".
        The one exception is the NTRIP password: an empty one means "keep the
        stored password", since the status endpoint never sent the real one to
        the browser in the first place and a form round-trip would otherwise
        blank it on every save.
        """
        if not isinstance(payload, dict):
            self._send_json({"ok": False, "error": "settings must be an object"}, 400)
            return
        stored = rtk.settings(getattr(self.config, "rtk", None))
        incoming = dict(payload)
        ntrip_in = incoming.get("ntrip")
        if isinstance(ntrip_in, dict) and not str(ntrip_in.get("password") or "").strip():
            ntrip_in = dict(ntrip_in)
            ntrip_in["password"] = stored.get("ntrip", {}).get("password", "")
            incoming["ntrip"] = ntrip_in
        resolved = rtk.settings(incoming)

        # Refused before anything is stored, so a form that cannot work is not
        # saved and then reported as broken on every restart.
        if resolved["source"] == "ntrip":
            problem = rtk.ntrip_problem(resolved["ntrip"])
            if problem:
                self._send_json({"ok": False, "error": problem}, 400)
                return
        if resolved["source"] == "usb" and resolved["mode"] == "fixed":
            problem = rtk.fixed_position_problem(resolved["fixed"])
            if problem:
                self._send_json({"ok": False, "error": problem}, 400)
                return

        self.config.rtk = resolved
        warning = ""
        try:
            save_config(self.config, self.config_path or default_config_path())
        except Exception:  # noqa: BLE001 - it still runs this session
            logger.exception("could not persist RTK config")
            warning = "set for this session but not saved"

        if self.rtk is None:
            self._send_json({"ok": True, "warning": warning or
                             "RTK corrections are unavailable in this build"})
            return
        try:
            self.rtk.apply_settings(resolved)
        except Exception as exc:  # noqa: BLE001
            logger.exception("applying RTK settings failed")
            self._send_json({"ok": False, "error": f"could not apply: {exc}"}, 500)
            return
        result = {"ok": True, "status": self.rtk.status()}
        if warning:
            result["warning"] = warning
        self._send_json(result)

    @route("POST", "/api/rtk/restart")
    def _api_rtk_restart(self, payload: dict) -> None:
        """Survey again from zero.

        The answer to a base that converged somewhere it should not have — run
        before the tripod was level, or with the antenna still under a roof.
        POST because it throws away a completed survey, which on a fresh
        session costs another three minutes.
        """
        del payload
        if self.rtk is None:
            self._send_json({"ok": False, "error": "RTK corrections are unavailable"}, 503)
            return
        try:
            self.rtk.restart_survey()
        except Exception as exc:  # noqa: BLE001
            logger.exception("RTK restart failed")
            self._send_json({"ok": False, "error": f"could not restart: {exc}"}, 500)
            return
        self._send_json({"ok": True, "status": self.rtk.status()})
