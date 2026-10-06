"""The vehicle link: connection, commands, flight modes and status.

Route handlers moved out of corvus/server.py. CorvusHandler inherits
VehicleRoutes, and the @route registry wires them exactly as before.
"""
from __future__ import annotations

import logging
import math

from .. import autopilot
from ..http_input import _parse_takeoff_altitude
from ..http_routes import route
from ..mavlink_bridge import FLY_TO_MAX_POINTS, TAKEOFF_ALTITUDE_MAX_M, TAKEOFF_ALTITUDE_MIN_M

logger = logging.getLogger("corvus.server")


class VehicleRoutes:
    @route("GET", "/api/mavlink/modes")
    def _api_mavlink_modes(self) -> None:
        if self.mavlink:
            self._send_json({
                "modes": self.mavlink.get_available_modes(),
                "labels": self.mavlink.get_mode_labels(),
            })
        else:
            self._send_json({"modes": [], "labels": {}})

    @route("GET", "/api/mavlink/capabilities")
    def _api_mavlink_capabilities(self) -> None:
        """What the connected flight stack can do.

        The frontend uses this to stop offering controls the vehicle would
        refuse — the MAVLink console on a stack with no shell, ESC calibration
        on a stack that does it through a parameter, an autotune button that
        starts a mode rather than a command. A missing control with a reason is
        a better answer than one that fails on press.
        """
        if self.mavlink:
            self._send_json(self.mavlink.capabilities())
            return
        self._send_json(
            autopilot.dialect_for_stack(autopilot.STACK_GENERIC).capabilities()
            | {"modes": [], "vehicle_type": 0, "mission_unsupported": {}}
        )

    @route("GET", "/api/mavlink/serial-ports")
    def _api_mavlink_serial_ports(self) -> None:
        """Enumerate serial ports for the connection manager UI.

        Always answers 200 so the frontend can safely poll: an empty list
        means "no bridge" or "enumeration failed" (with an ``error`` field).
        """
        if self.mavlink is None:
            self._send_json({"ports": []})
            return
        try:
            ports = self.mavlink.list_serial_ports()
        except Exception as exc:  # noqa: BLE001 - UI polling must never 500
            logger.exception("list_serial_ports failed")
            self._send_json({"ports": [], "error": str(exc)})
            return
        self._send_json({"ports": ports})

    def _note_manual_link(self, conn: str) -> None:
        """Record that the operator chose the link by hand, and clear the offer.

        Never raises: this is bookkeeping on top of a connect that has already
        succeeded, and an auto-connect bug must not turn a working link into a
        500.
        """
        session = self.autoconnect_session
        if session is None:
            return
        try:
            session.note_manual_connect()
            if conn:
                session.note_decision("manual", conn)
            if self.store is not None:
                self.store.update(
                    link_suggestion=None, link_auto=session.as_dict(),
                )
        except Exception:  # noqa: BLE001 - never fail a good connect over this
            logger.exception("autoconnect: could not record the manual connect")

    @route("GET", "/api/mavlink/auto")
    def _api_mavlink_auto(self) -> None:
        """Read-only view of what auto-connect decided and what it is offering.

        The same two values the store pushes over SSE (``link_auto`` and
        ``link_suggestion``), served as a plain GET so the behaviour can be
        asked about directly — from a test, from a support session, from a
        terminal at the field — without holding an event stream open. No side
        effects: this endpoint decides nothing and dials nothing.
        """
        session = self.autoconnect_session
        snapshot = self.store.get_snapshot() if self.store is not None else {}
        auto = session.as_dict() if session is not None else dict(
            snapshot.get("link_auto") or {},
        )
        self._send_json({
            "auto": auto,
            "suggestion": snapshot.get("link_suggestion"),
        })

    @route("POST", "/api/mavlink/connect")
    def _api_mavlink_connect(self, payload: dict) -> None:
        conn = payload.get("connection", "udp:127.0.0.1:14550")
        # Validate the connection string up front: a non-string/empty value
        # would be passed straight to the bridge. set_connection() now raises
        # ValueError on a bad spec — surface that as a 400, not a 500.
        if not isinstance(conn, str) or not conn:
            self._send_json(
                {"ok": False, "error": "connection must be a non-empty string"}, 400,
            )
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "mavlink not ready"}, 503)
            return
        # Validate BEFORE tearing anything down. This used to stop the bridge
        # first, so a typo in the connection field killed a working link and
        # handed back a 400 — the operator lost the aircraft to a spelling
        # mistake, with no way back except retyping the old string from memory.
        try:
            self.mavlink.validate_connection(conn)
        except ValueError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        try:
            self.mavlink.stop()
            self.mavlink.set_connection(conn)
            self.mavlink.start()
        except ValueError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        # The operator picked a link, so auto-connect stops picking for them —
        # for the rest of this process, not for the rest of time (the flag is
        # never persisted, so the next field launch starts from the hardware
        # again). Set only now, after the dial actually went through: a string
        # the bridge refused is not a choice, it is a typo.
        self._note_manual_link(conn)
        self._send_json({"ok": True, "connection": conn})

    @route("POST", "/api/mavlink/disconnect")
    def _api_mavlink_disconnect(self, payload: dict) -> None:
        """Close the MAVLink link and leave it closed.

        Until now the only way out of a connection was to make another one, so
        an operator who wanted the radio free — to hand the aircraft to another
        GCS, to swap a cable, to stop a reconnect loop hammering a port that
        moved — had to quit the app. ``stop()`` already does the full teardown
        (tlog closed, pending commands cancelled, shell released), so this is
        that call plus an honest reply.

        Idempotent: disconnecting an already-closed link is a success, because
        the state the caller asked for is the state they get.

        Takes ``payload`` because every POST handler is dispatched with the
        parsed body, even the ones with nothing to read from it.
        """
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "mavlink not ready"}, 503)
            return
        try:
            self.mavlink.stop()
        except Exception as exc:  # noqa: BLE001 - teardown must never 500
            logger.exception("mavlink disconnect failed")
            self._send_json({"ok": False, "error": f"disconnect failed: {exc}"}, 500)
            return
        # "Leave it closed" has to mean closed. Auto-connect keeps the manual
        # override it was given (and takes one now if it had none), so nothing
        # dials the link back open behind the operator who just freed the
        # radio. The way back is another connect, or a restart.
        self._note_manual_link("")
        self._send_json({"ok": True})

    @route("POST", "/api/mavlink/arm")
    def _api_mavlink_arm(self, payload: dict) -> None:
        arm = payload.get("arm", True)
        if not isinstance(arm, bool):
            self._send_json({"ok": False, "error": "arm must be boolean"}, 400)
            return
        if self.mavlink:
            ok = self.mavlink.arm(arm)
            if ok:
                self._send_json({"ok": True})
            else:
                error = self.mavlink.get_last_command_error() or "arm command failed"
                status = 503 if "DISCONNECTED" in error else 409
                self._send_json({"ok": False, "error": error}, status)
        else:
            self._send_json({"error": "not connected"}, 400)

    @route("POST", "/api/mavlink/mode")
    def _api_mavlink_mode(self, payload: dict) -> None:
        mode = payload.get("mode", "")
        if self.mavlink and isinstance(mode, str) and mode:
            ok = self.mavlink.set_mode(mode.upper())
            if ok:
                self._send_json({"ok": True})
            else:
                error = self.mavlink.get_last_command_error() or "mode change failed"
                status = 503 if "DISCONNECTED" in error else 409
                self._send_json({"ok": False, "error": error}, status)
        else:
            self._send_json({"error": "not connected or no mode"}, 400)

    @route("POST", "/api/mavlink/takeoff")
    def _api_mavlink_takeoff(self, payload: dict) -> None:
        try:
            alt = _parse_takeoff_altitude(payload.get("altitude", 10.0))
        except ValueError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        if self.mavlink:
            ok = self.mavlink.takeoff(alt)
            if ok:
                self._send_json({"ok": True, "altitude_agl": alt})
            else:
                error = self.mavlink.get_last_command_error() or "takeoff command failed"
                status = 503 if "DISCONNECTED" in error else 409
                self._send_json({"ok": False, "error": error}, status)
        else:
            self._send_json({"error": "not connected"}, 400)

    @route("POST", "/api/mavlink/land")
    def _api_mavlink_land(self, payload: dict) -> None:
        if self.mavlink:
            ok = self.mavlink.land()
            if ok:
                self._send_json({"ok": True})
            else:
                error = self.mavlink.get_last_command_error() or "land command failed"
                status = 503 if "DISCONNECTED" in error else 409
                self._send_json({"ok": False, "error": error}, status)
        else:
            self._send_json({"error": "not connected"}, 400)

    @route("POST", "/api/mavlink/rtl")
    def _api_mavlink_rtl(self, payload: dict) -> None:
        if self.mavlink:
            ok = self.mavlink.rtl()
            if ok:
                self._send_json({"ok": True})
            else:
                error = self.mavlink.get_last_command_error() or "RTL command failed"
                status = 503 if "DISCONNECTED" in error else 409
                self._send_json({"ok": False, "error": error}, status)
        else:
            self._send_json({"error": "not connected"}, 400)

    @route("POST", "/api/mavlink/reboot")
    def _api_mavlink_reboot(self, payload: dict) -> None:
        """Restart the autopilot. Disarmed only; the bridge refuses otherwise."""
        del payload
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if self.mavlink.reboot_autopilot():
            self._send_json({"ok": True})
            return
        error = self.mavlink.get_last_command_error() or "reboot failed"
        status = 503 if "not connected" in error or "DISCONNECTED" in error else 409
        self._send_json({"ok": False, "error": error}, status)

    @route("POST", "/api/mavlink/shell/send")
    def _api_mavlink_shell_send(self, payload: dict) -> None:
        """Type into the PX4 NSH shell, for a terminal.

        ``{"data": "ver all\\n"}`` is sent as it is: no newline added, nothing
        trimmed, so a Ctrl-C (``"\\x03"``) reaches a running ``top``. The
        output arrives on the ``shell`` topic of ``/api/events``. 409 on a
        stack with no shell, 503 with nothing connected.
        """
        data = payload.get("data")
        if not isinstance(data, str) or not data:
            self._send_json({"ok": False, "error": "data must be non-empty text"}, 400)
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if self.mavlink.write_shell(data):
            self._send_json({"ok": True})
            return
        error = self.mavlink.get_last_command_error() or "shell write failed"
        status = 503 if "not connected" in error else 409
        self._send_json({"ok": False, "error": error}, status)

    @route("POST", "/api/mavlink/shell/close")
    def _api_mavlink_shell_close(self, payload: dict) -> None:
        """End the terminal's shell: PX4 stops it and frees the link.

        Idempotent, and fine with nothing connected. What was running in the
        shell (a ``top``, a ``listener``) stops with it.
        """
        del payload
        if self.mavlink is not None:
            self.mavlink.stop_shell()
        self._send_json({"ok": True})

    @route("POST", "/api/mavlink/manual")
    def _api_mavlink_manual(self, payload: dict) -> None:
        """Forward one virtual-joystick frame to the vehicle as MANUAL_CONTROL.

        The stream endpoint for ``src/js/joystick.js``: normalized axes in,
        one MAVLink frame out, no ACK. Each frame is independent — the browser
        re-sends at a fixed rate and a dropped one is simply superseded — so
        this validates and dispatches, and never blocks on the vehicle.
        """
        axes: dict[str, float] = {}
        for name, low, high in (("x", -1.0, 1.0), ("y", -1.0, 1.0),
                                ("z", 0.0, 1.0), ("r", -1.0, 1.0)):
            raw = payload.get(name, 0.0 if name != "z" else 0.5)
            # bool is a subclass of int — reject it so True never flies as 1.0.
            if isinstance(raw, bool) or not isinstance(raw, (int, float)) or not math.isfinite(float(raw)):
                self._send_json({"ok": False, "error": f"{name} must be a finite number"}, 400)
                return
            value = float(raw)
            if not low <= value <= high:
                self._send_json(
                    {"ok": False, "error": f"{name} must be between {low:g} and {high:g}"}, 400)
                return
            axes[name] = value

        buttons = payload.get("buttons", 0)
        if isinstance(buttons, bool) or not isinstance(buttons, int) or not 0 <= buttons <= 0xFFFF:
            self._send_json({"ok": False, "error": "buttons must be an integer between 0 and 65535"}, 400)
            return

        if not self.mavlink:
            self._send_json({"error": "not connected"}, 400)
            return
        if self.mavlink.manual_control(buttons=buttons, **axes):
            self._send_json({"ok": True})
            return
        error = self.mavlink.get_last_command_error() or "manual control send failed"
        status = 503 if "DISCONNECTED" in error else 409
        self._send_json({"ok": False, "error": error}, status)

    @route("POST", "/api/mavlink/sethome")
    def _api_mavlink_sethome(self, payload: dict) -> None:
        """Move the home position to a clicked map coordinate.

        Lateral only: the bridge keeps the existing home altitude, because a
        map click carries no terrain height. Allowed while armed — relocating
        home is how an operator redirects RTL mid-flight.
        """
        lat = payload.get("lat")
        lon = payload.get("lon")
        # bool is a subclass of int — reject it so True never flies as 1.0.
        for name, value, limit in (("lat", lat, 90.0), ("lon", lon, 180.0)):
            if isinstance(value, bool) or not isinstance(value, (int, float)) \
                    or not math.isfinite(float(value)):
                self._send_json({"ok": False, "error": f"{name} must be a finite number"}, 400)
                return
            if not -limit <= float(value) <= limit:
                self._send_json(
                    {"ok": False, "error": f"{name} must be between {-limit:g} and {limit:g}"}, 400)
                return

        if self.mavlink is None:
            self._send_json({"error": "not connected"}, 400)
            return
        if self.mavlink.set_home(float(lat), float(lon)):
            self._send_json({"ok": True})
            return
        error = self.mavlink.get_last_command_error() or "set home failed"
        status = 503 if "DISCONNECTED" in error else 409
        self._send_json({"ok": False, "error": error}, status)

    @route("POST", "/api/mavlink/gotopoints")
    def _api_mavlink_gotopoints(self, payload: dict) -> None:
        """Dispatch a multi-waypoint fly-to command to the vehicle."""
        raw_points = payload.get("points")
        if not isinstance(raw_points, list) or not raw_points:
            self._send_json({"ok": False, "error": "points must be a non-empty list"}, 400)
            return
        # Checked before the per-point walk below, so an oversized payload
        # costs one comparison rather than a coordinate-validated copy of
        # itself. The bridge enforces the same bound — this is the early out.
        if len(raw_points) > FLY_TO_MAX_POINTS:
            self._send_json(
                {"ok": False,
                 "error": f"too many points ({len(raw_points)}); "
                          f"the maximum is {FLY_TO_MAX_POINTS}"},
                400,
            )
            return
        cleaned: list[dict[str, float]] = []
        for index, item in enumerate(raw_points):
            if not isinstance(item, dict):
                self._send_json({"ok": False, "error": f"point {index} must be an object"}, 400)
                return
            lat = item.get("lat")
            lon = item.get("lon")
            alt_agl = item.get("alt_agl")
            # bool is a subclass of int — reject it so True is never coerced to 1.0
            if isinstance(lat, bool) or not isinstance(lat, (int, float)) or not math.isfinite(float(lat)):
                self._send_json({"ok": False, "error": f"point {index} lat must be a finite number"}, 400)
                return
            if isinstance(lon, bool) or not isinstance(lon, (int, float)) or not math.isfinite(float(lon)):
                self._send_json({"ok": False, "error": f"point {index} lon must be a finite number"}, 400)
                return
            if isinstance(alt_agl, bool) or not isinstance(alt_agl, (int, float)) or not math.isfinite(float(alt_agl)):
                self._send_json({"ok": False, "error": f"point {index} alt_agl must be a finite number"}, 400)
                return
            lat_f = float(lat)
            lon_f = float(lon)
            alt_f = float(alt_agl)
            if not -90.0 <= lat_f <= 90.0:
                self._send_json({"ok": False, "error": f"point {index} lat must be between -90 and 90"}, 400)
                return
            if not -180.0 <= lon_f <= 180.0:
                self._send_json({"ok": False, "error": f"point {index} lon must be between -180 and 180"}, 400)
                return
            if not TAKEOFF_ALTITUDE_MIN_M <= alt_f <= TAKEOFF_ALTITUDE_MAX_M:
                self._send_json(
                    {"ok": False, "error": f"point {index} alt_agl must be between "
                     f"{TAKEOFF_ALTITUDE_MIN_M:.0f} and {TAKEOFF_ALTITUDE_MAX_M:.0f} m AGL"},
                    400,
                )
                return
            cleaned.append({"lat": lat_f, "lon": lon_f, "alt_agl": alt_f})
        if self.mavlink is None:
            self._send_json({"error": "not connected"}, 400)
            return
        ok = self.mavlink.fly_to_points(cleaned)
        if ok:
            self._send_json({"ok": True})
        else:
            error = self.mavlink.get_last_command_error() or "fly to points failed"
            status = 503 if "DISCONNECTED" in error else 409
            self._send_json({"ok": False, "error": error}, status)
