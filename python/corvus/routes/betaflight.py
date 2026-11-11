"""Betaflight: read a flight controller over MSP.

Route handlers for the Betaflight page. Read only: the page lists the serial
ports and reads the controller on one of them (see :mod:`corvus.msp`), and
nothing it can do changes the controller.
"""
from __future__ import annotations

import logging

from .. import msp
from ..http_routes import route
from ..serial_ports import is_bootloader_port, is_rtk_device, same_port

logger = logging.getLogger("corvus.server")


class BetaflightRoutes:
    def _betaflight_link_device(self) -> str:
        """The serial port the MAVLink link holds, or "" when it is not on a serial port."""
        mavlink = getattr(self, "mavlink", None)
        if mavlink is None:
            return ""
        try:
            return mavlink.serial_device() or ""
        except Exception:  # noqa: BLE001 - a bridge-shaped double may not have it
            return ""

    @route("GET", "/api/betaflight/status")
    def _api_betaflight_status(self) -> None:
        """The ports a controller could be on, and which one the MAVLink link holds.

        Always 200 so the page can poll it. The link's own port is listed and
        flagged, not hidden: a Betaflight USB port is rarely the link's, and
        an operator who picked the wrong cable should be told which it is.
        """
        link_device = self._betaflight_link_device()
        ports: list[dict] = []
        mavlink = getattr(self, "mavlink", None)
        if mavlink is not None:
            try:
                for port in mavlink.list_serial_ports():
                    device = port.get("device", "")
                    if (
                        is_bootloader_port(device, port.get("hwid", ""), port.get("description", ""))
                        or is_rtk_device(device, port.get("hwid", ""), port.get("description", ""))
                    ):
                        continue
                    ports.append({**port, "link": bool(link_device) and same_port(device, link_device)})
            except Exception:  # noqa: BLE001 - UI polling must never 500
                logger.exception("listing ports for the Betaflight page failed")
        self._send_json({
            "ports": ports,
            "link_device": link_device,
            "default_baud": msp.DEFAULT_BAUD,
            "bauds": list(msp.BAUD_CANDIDATES),
        })

    @route("POST", "/api/betaflight/read")
    def _api_betaflight_read(self, payload: dict) -> None:
        """Read the controller on ``device``: firmware, board, sensors, PIDs, rates.

        409 for every condition the operator can do something about (a port
        that is the MAVLink link, one that will not open, one that does not
        answer), 400 for a request that is malformed. A synchronous read: it
        takes a second or two and holds nothing afterwards.
        """
        device = payload.get("device")
        if not isinstance(device, str) or not device.strip():
            self._send_json({"ok": False, "error": "device is required"}, 400)
            return
        device = device.strip()
        baud = payload.get("baud", msp.DEFAULT_BAUD)
        if isinstance(baud, bool) or not isinstance(baud, int) or baud <= 0:
            self._send_json({"ok": False, "error": "baud must be a positive integer"}, 400)
            return
        link_device = self._betaflight_link_device()
        if link_device and same_port(device, link_device):
            self._send_json({
                "ok": False,
                "error": "This port carries the MAVLink link. Read the flight "
                         "controller on its USB port instead.",
            }, 409)
            return
        try:
            result = msp.read_flight_controller(device, baud)
        except msp.MspError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 409)
            return
        except Exception as exc:  # noqa: BLE001 - a serial layer raises broadly
            logger.exception("Betaflight read failed")
            self._send_json({"ok": False, "error": f"read failed: {exc}"}, 500)
            return
        result["ok"] = True
        result["device"] = device
        result["baud"] = baud
        self._send_json(result)
