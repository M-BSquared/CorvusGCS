"""First setup endpoints: is the controller fresh, and choosing its airframe.

The wizard itself is the frontend's; these two routes are the parts that need
the vehicle (see :mod:`corvus.first_setup`).
"""
from __future__ import annotations

import logging
from typing import Any

from .. import first_setup
from ..ardupilot_firmware import vehicle_for_mav_type
from ..http_routes import route
from ..serial_ports import usb_identity

logger = logging.getLogger("corvus.server")


class FirstSetupRoutes:
    def _first_setup_context(self) -> tuple[str, str, str]:
        """(stack, vehicle family, transport) of the connected link."""
        mav = self.mavlink
        stack = str(getattr(mav, "stack", "") or "")
        vehicle = vehicle_for_mav_type(getattr(mav, "vehicle_type_id", 0))
        try:
            transport = str(mav.transport() or "unknown")
        except Exception:  # noqa: BLE001 - a bridge-shaped double may lack it
            transport = "unknown"
        return stack, vehicle, transport

    def _first_setup_fingerprint(self, stack: str) -> str:
        """What the wizard remembers a "do not ask again" against.

        The board's USB identity (ids plus serial number where it has one), so a
        second fresh controller is offered the wizard even after the first one
        was declined. Weaker than a hardware uid and good enough to stop a
        prompt repeating for the board the operator already waved away.
        """
        try:
            identity = self.mavlink.board_identity()
        except Exception:  # noqa: BLE001 - identification is best-effort
            identity = {}
        usb = usb_identity(str(identity.get("hwid", "") or "")) or ""
        return f"{stack}|{identity.get('vendor_id', 0)}|{identity.get('product_id', 0)}|{usb}"

    def _first_setup_present(self, name: str) -> dict[str, float]:
        try:
            return self.mavlink.fetch_params([name])
        except Exception:  # noqa: BLE001 - treat an unreadable follow-up as absent
            logger.debug("first setup follow-up read failed", exc_info=True)
            return {}

    @route("GET", "/api/first-setup")
    def _api_first_setup(self) -> None:
        """What the first setup wizard needs to know about the connected controller.

        Always 200. ``connected`` says whether a vehicle answered; nothing else
        in the payload is meaningful without it.
        """
        if self.mavlink is None:
            self._send_json({"connected": False, "needs_setup": False, "prompt": False})
            return
        stack, vehicle, transport = self._first_setup_context()
        try:
            values = self.mavlink.fetch_params(first_setup.param_names(stack))
        except Exception:  # noqa: BLE001 - a read must never 500 the wizard
            logger.exception("first setup parameter fetch failed")
            values = {}
        if not values:
            self._send_json({
                "connected": False, "stack": stack, "vehicle": vehicle,
                "transport": transport, "needs_setup": False, "prompt": False,
                "facts": {}, "reasons": [], "presets": first_setup.presets(stack, vehicle),
                "preset_current": None, "fingerprint": "",
            })
            return
        payload = first_setup.assess(stack, vehicle, transport, values)
        payload["connected"] = True
        payload["fingerprint"] = self._first_setup_fingerprint(stack)
        self._send_json(payload)

    @route("POST", "/api/first-setup/airframe")
    def _api_first_setup_airframe(self, payload: dict) -> None:
        """Choose the aircraft: write the preset's parameters. Disarmed only.

        The body names a preset id from the table the GET returned. The values
        written come from that table, never from the request. The vehicle has to
        be rebooted afterwards; this endpoint does not do it, so the wizard can
        say so first.
        """
        preset = payload.get("preset")
        if isinstance(preset, bool) or not isinstance(preset, int):
            self._send_json({"ok": False, "error": "preset must be an integer"}, 400)
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if self.store is not None and self.store.get_snapshot().get("armed"):
            self._send_json({"ok": False, "error": "disarm before choosing an airframe"}, 409)
            return
        stack, vehicle, _ = self._first_setup_context()
        writes = first_setup.writes_for(stack, vehicle, preset)
        if writes is None:
            self._send_json({"ok": False, "error": "unknown airframe for this vehicle"}, 400)
            return
        written: list[str] = []
        for index, (name, value) in enumerate(writes):
            # Only the first write is the choice itself. A follow-up the
            # firmware has no such parameter for is skipped, not reported.
            if index > 0 and name not in self._first_setup_present(name):
                continue
            if self.mavlink.set_param(name, value):
                written.append(name)
                continue
            # The airframe id is the write that matters. The follow-up
            # (SYS_AUTOCONFIG) is reported but does not undo it.
            error = self.mavlink.get_last_command_error() or f"could not write {name}"
            if index == 0:
                status = 503 if "not connected" in error.lower() else 409
                self._send_json({"ok": False, "error": error}, status)
                return
            result: dict[str, Any] = {"ok": True, "written": written, "warning": error}
            self._send_json(result)
            return
        self._send_json({"ok": True, "written": written, "reboot": True})
