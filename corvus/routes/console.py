"""The MAVLink console and shell.

Route handlers moved out of corvus/server.py. CorvusHandler inherits
ConsoleRoutes, and the @route registry wires them exactly as before.
"""
from __future__ import annotations

import logging
import os
import tempfile
import time

from ..http_input import _parse_takeoff_altitude, _safe_filename
from ..http_routes import route

logger = logging.getLogger("corvus.server")

# PX4 NuttShell (NSH) builtins with no MAVLink-command equivalent (e.g.
# `listener <topic>` subscribes to a uORB topic and prints it). Routed to the
# serial-control debug shell, not a MAV_CMD. `help` stays the local help and
# `param` is not a console command, so this set is disjoint from the Corvus
# command set above.
SHELL_COMMANDS = frozenset({
    "listener", "top", "free", "dmesg", "tasks", "perf", "boot_log", "hrt",
})


class ConsoleRoutes:
    @route("POST", "/api/console/command")
    def _api_console_command(self, payload: dict) -> None:
        command_value = payload.get("command", "")
        cmd = command_value.strip() if isinstance(command_value, str) else ""
        if not cmd or not self.mavlink:
            self._send_json({"error": "no command or not connected"}, 400)
            return
        parts = cmd.lower().split()
        ok = True
        if parts[0] == "arm":
            ok = self.mavlink.arm(True)
        elif parts[0] == "disarm":
            ok = self.mavlink.arm(False)
        elif parts[0] == "mode" and len(parts) > 1:
            ok = self.mavlink.set_mode(parts[1].upper())
        elif parts[0] == "takeoff":
            if len(parts) > 2:
                self._send_json({"error": "usage: takeoff [altitude_agl]"}, 400)
                return
            try:
                alt = _parse_takeoff_altitude(parts[1] if len(parts) > 1 else 10.0)
            except ValueError as exc:
                self._send_json({"error": str(exc)}, 400)
                return
            ok = self.mavlink.takeoff(alt)
        elif parts[0] == "land":
            ok = self.mavlink.land()
        elif parts[0] == "rtl":
            ok = self.mavlink.rtl()
        elif parts[0] == "help":
            self._send_json({"ok": True, "help": True})
            return
        elif parts[0] == "shell" or parts[0] == "nsh":
            # Strip the prefix from the ORIGINAL cmd so the rest keeps its case
            # (NSH is case-sensitive, e.g. `listener sensor_combined`).
            shell_text = cmd.split(maxsplit=1)[1] if len(cmd.split(maxsplit=1)) > 1 else ""
            if not shell_text:
                self._send_json({"error": f"usage: {parts[0]} <command>"}, 400)
                return
            ok_shell = self.mavlink.send_shell_command(shell_text)
            if ok_shell:
                self._send_json({"ok": True, "shell": True})
            else:
                error = self.mavlink.get_last_command_error() or f"shell command failed: {cmd}"
                normalized_error = error.lower()
                status = 503 if (
                    "disconnected" in normalized_error or "not connected" in normalized_error
                ) else 409
                self._send_json({"ok": False, "error": error}, status)
            return
        elif parts[0] in SHELL_COMMANDS:
            # Send the full original cmd (case-preserved) to the NSH shell.
            ok_shell = self.mavlink.send_shell_command(cmd)
            if ok_shell:
                self._send_json({"ok": True, "shell": True})
            else:
                error = self.mavlink.get_last_command_error() or f"shell command failed: {cmd}"
                normalized_error = error.lower()
                status = 503 if (
                    "disconnected" in normalized_error or "not connected" in normalized_error
                ) else 409
                self._send_json({"ok": False, "error": error}, status)
            return
        else:
            self._send_json({"error": f"unknown command: {cmd}"}, 400)
            return
        if ok:
            self._send_json({"ok": True})
        else:
            error = self.mavlink.get_last_command_error() or f"command failed: {cmd}"
            status = 503 if "DISCONNECTED" in error else 409
            self._send_json({"ok": False, "error": error}, status)

    @route("POST", "/api/console/save")
    def _api_console_save(self, payload: dict) -> None:
        """Write the console transcript to disk and return the path.

        Server-side for the same reason parameter export is: the desktop build
        runs in QtWebEngine, which drops an ``<a download>`` unless the host
        app implements a handler, so a browser-download route would silently
        produce nothing there. Logs land beside the tlogs, since that is where
        someone reconstructing a flight will already be looking.
        """
        text = payload.get("text")
        if not isinstance(text, str) or not text.strip():
            self._send_json({"ok": False, "error": "text must be a non-empty string"}, 400)
            return

        cfg = self._live_config()
        target_dir = os.path.expanduser(
            (cfg.tlog_dir or "").strip() or "~/.corvus/logs")
        filename = _safe_filename(
            payload.get("filename"),
            time.strftime("corvus-console_%Y-%m-%d_%H-%M.log"),
            suffix=".log",
        )
        try:
            os.makedirs(target_dir, exist_ok=True)
            path = os.path.join(target_dir, filename)
            fd, tmp_name = tempfile.mkstemp(prefix=filename + ".", suffix=".tmp", dir=target_dir)
            try:
                with os.fdopen(fd, "w", encoding="utf-8") as f:
                    f.write(text)
                    if not text.endswith("\n"):
                        f.write("\n")
                os.replace(tmp_name, path)
            except BaseException:
                try:
                    os.unlink(tmp_name)
                except OSError:
                    pass
                raise
        except OSError as exc:
            self._send_json(
                {"ok": False, "error": f"could not write to {target_dir}: {exc.strerror or exc}"},
                400)
            return
        except Exception as exc:  # noqa: BLE001 - a log save must never 500
            logger.exception("console log save failed")
            self._send_json({"ok": False, "error": f"save failed: {exc}"}, 500)
            return

        logger.info("console log written to %s", path)
        self._send_json({"ok": True, "path": path, "dir": target_dir, "filename": filename})

    @route("GET", "/api/console/stream")
    def _sse_console(self) -> None:
        """STATUSTEXT as its own stream, under the event name ``message``.

        The frontend uses ``/api/events?topics=console`` instead, to stay
        inside the browser's six-connection budget. This endpoint remains
        because it is a published HTTP API a plugin or an external tool may
        hold — and because it costs nothing now that it is the same topic
        binding the multiplexed stream uses, under a different event name.
        """
        self._serve_sse_topics({"console": "message"})
