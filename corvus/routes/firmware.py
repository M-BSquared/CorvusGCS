"""Firmware flashing.

Route handlers moved out of corvus/server.py. CorvusHandler inherits
FirmwareRoutes, and the @route registry wires them exactly as before.
"""
from __future__ import annotations

import logging
from urllib.parse import parse_qs, urlparse
from ..http_routes import route

logger = logging.getLogger("corvus.server")


class FirmwareRoutes:
    # ---- Firmware flash ----
    @route("GET", "/api/firmware/status")
    def _api_firmware_status(self) -> None:
        """Live flash status + the USB gate. Always 200."""
        if self.flash is None:
            transport = "unknown"
            device = ""
            armed = False
            if self.mavlink is not None:
                transport = self.mavlink.transport()
                device = self.mavlink.serial_device()
            if self.store is not None:
                armed = bool(self.store.get_snapshot().get("armed"))
            self._send_json({
                "state": "idle",
                "transport": transport,
                "device": device,
                "can_flash": False,
                "armed": armed,
                "progress": 0,
                "message": "",
            })
            return
        self._send_json(self.flash.status())

    @route("GET", "/api/firmware/catalog")
    def _api_firmware_catalog(self) -> None:
        """PX4 and ArduPilot releases and their flashable boards, plus the cache.

        Served from the on-disk cache unless ``?refresh=1``, so opening the page
        in the field is instant and needs no network. A failed refresh returns
        200 with the cached list and a non-empty ``error`` — going blank because
        there is no internet is exactly the wrong answer for this app.

        ``detected`` is the board on the USB port per stack, and ``suggested``
        the stack and ArduPilot vehicle of the connected aircraft: what the page
        opens on, never what it flashes without being told.
        """
        if self.flash is None or getattr(self.flash, "catalog", None) is None:
            self._send_json({"releases": [], "cached": [], "error":
                             "firmware catalogue unavailable", "dir": ""})
            return
        params = parse_qs(urlparse(self.path).query)
        refresh = (params.get("refresh", ["0"])[0] or "0").lower() in ("1", "true", "yes")
        try:
            data = self.flash.catalog.catalog(refresh=refresh)
            data["detected"] = self._detect_connected_board(data.get("releases") or [])
            data["suggested"] = self._suggested_firmware()
            self._send_json(data)
        except Exception:  # noqa: BLE001 - a catalogue failure must not 500
            logger.exception("firmware catalogue failed")
            self._send_json({"releases": [], "cached": [],
                             "error": "firmware catalogue failed", "dir": "",
                             "detected": {"px4": None, "ardupilot": None},
                             "suggested": {"stack": "", "vehicle": ""}})

    def _detect_connected_board(self, releases: list) -> dict:
        """Which board is plugged in, per stack, matched against the catalogue.

        A suggestion only — it preselects, it does not decide.
        """
        from ..firmware_catalog import detect_boards
        nothing: dict = {"px4": None, "ardupilot": None}
        if self.mavlink is None or not releases:
            return nothing
        try:
            return detect_boards(self.mavlink.board_identity(), releases)
        except Exception:  # noqa: BLE001 - detection is a convenience, never a gate
            logger.debug("board detection failed", exc_info=True)
            return nothing

    def _suggested_firmware(self) -> dict:
        """The connected aircraft's stack and ArduPilot vehicle, or blanks.

        The vehicle is offered whichever stack is flying now: a quad moving from
        PX4 to ArduPilot wants ArduCopter just as much as one already on it.
        """
        from ..ardupilot_firmware import vehicle_for_mav_type
        out = {"stack": "", "vehicle": ""}
        if self.mavlink is None:
            return out
        try:
            stack = str(getattr(self.mavlink, "stack", "") or "")
            out["stack"] = stack if stack in ("px4", "ardupilot") else ""
            out["vehicle"] = vehicle_for_mav_type(getattr(self.mavlink, "vehicle_type_id", 0))
        except Exception:  # noqa: BLE001 - a default is a convenience, never a gate
            logger.debug("firmware suggestion failed", exc_info=True)
        return out

    @route("POST", "/api/firmware/flash")
    def _api_firmware_flash(self, payload: dict) -> None:
        """Download one catalogue image and flash it.

        The body names a release and a board; the download URL is resolved
        server-side, so the browser never chooses what gets fetched.
        """
        release = payload.get("release", "")
        board = payload.get("board", "")
        if not isinstance(release, str) or not isinstance(board, str):
            self._send_json({"ok": False, "error": "release and board must be strings"}, 400)
            return
        if not release.strip() or not board.strip():
            self._send_json({"ok": False, "error": "release and board are required"}, 400)
            return
        if self.flash is None:
            self._send_json({"ok": False, "error": "flash service unavailable"}, 503)
            return
        if self.flash.start_release(release.strip(), board.strip()):
            self._send_json({"ok": True, "state": "downloading"})
            return
        err = getattr(self.flash, "last_error", "") or "flash refused"
        self._send_json({"ok": False, "error": err}, 409)

    @route("POST", "/api/firmware/cancel")
    def _api_firmware_cancel(self, payload: dict) -> None:
        if self.flash is None:
            self._send_json({"ok": False, "error": "flash service unavailable"}, 503)
            return
        if self.flash.cancel():
            self._send_json({"ok": True})
        else:
            self._send_json({"ok": False, "error": "no flash in progress"})

    @route("GET", "/api/firmware/progress")
    def _sse_firmware(self) -> None:
        """Flash progress as its own stream — every step, none coalesced.

        Erase/program/verify each matter to whoever is watching a bootloader
        write, so this topic buffers FIFO rather than latest-wins. See
        :meth:`_sse_console`: the frontend uses
        ``/api/events?topics=firmware`` over the same binding.
        """
        self._serve_sse_topics({"firmware": "progress"})
