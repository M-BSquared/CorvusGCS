"""HTTP server with SSE telemetry, MAVLink API, SSH API, and static files.

A ``ThreadingHTTPServer`` subclass that serves the web UI from ``src/``
and exposes JSON/SSE API endpoints under ``/api/``.
"""
from __future__ import annotations

import collections
import http.server
import json
import logging
import math
import mimetypes
import os
import queue
import socketserver
import threading
import time
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

from .mavlink_bridge import (
    TAKEOFF_ALTITUDE_MAX_M,
    TAKEOFF_ALTITUDE_MIN_M,
    MavlinkBridge,
)
from .ssh_bridge import SshBridge
from .state_store import VehicleStateStore, _sanitize
from .version import get_version

logger = logging.getLogger("corvus.server")

REPO_ROOT = Path(__file__).resolve().parent.parent
WEB_DIR = REPO_ROOT / "src"
TELEMETRY_SSE_CAPACITY = 1
CONSOLE_SSE_CAPACITY = 100
PARAMS_SSE_CAPACITY = 16
CALIB_SENSORS = frozenset({
    "gyro", "compass", "baro", "accel", "level", "accel_quick", "airspeed",
})
AUTOTUNE_AXES = frozenset({"roll", "pitch", "yaw", "all"})


class _BoundedSseBuffer:
    def __init__(self, capacity: int) -> None:
        self._capacity = capacity
        self._items: collections.deque[Any] = collections.deque()
        self._condition = threading.Condition()

    def put_latest(self, item: Any) -> None:
        with self._condition:
            self._items.clear()
            self._items.append(item)
            self._condition.notify()

    def put_console(self, entry: dict[str, Any]) -> None:
        key = (entry.get("name"), entry.get("text"), entry.get("level"))
        urgent = entry.get("level") in {"error", "critical", "warning"}
        with self._condition:
            self._items = collections.deque(
                item for item in self._items
                if (item.get("name"), item.get("text"), item.get("level")) != key
            )
            if len(self._items) >= self._capacity:
                drop_index = next(
                    (
                        index for index in range(len(self._items) - 1, -1, -1)
                        if self._items[index].get("level") not in {"error", "critical", "warning"}
                    ),
                    None,
                )
                if drop_index is None:
                    if not urgent:
                        return
                    self._items.pop()
                else:
                    del self._items[drop_index]
            if urgent:
                self._items.appendleft(entry)
            else:
                self._items.append(entry)
            self._condition.notify()

    def get(self, timeout: float | None = None) -> Any:
        deadline = None if timeout is None else time.monotonic() + timeout
        with self._condition:
            while not self._items:
                remaining = None if deadline is None else deadline - time.monotonic()
                if remaining is not None and remaining <= 0:
                    raise queue.Empty
                self._condition.wait(remaining)
            return self._items.popleft()

    def qsize(self) -> int:
        with self._condition:
            return len(self._items)


def _parse_takeoff_altitude(value: object) -> float:
    if isinstance(value, bool):
        raise ValueError("takeoff altitude must be a number")
    try:
        altitude = float(value)
    except (TypeError, ValueError) as exc:
        raise ValueError("takeoff altitude must be a number") from exc
    if not math.isfinite(altitude):
        raise ValueError("takeoff altitude must be finite")
    if not TAKEOFF_ALTITUDE_MIN_M <= altitude <= TAKEOFF_ALTITUDE_MAX_M:
        raise ValueError(
            f"takeoff altitude must be between {TAKEOFF_ALTITUDE_MIN_M:.0f} and "
            f"{TAKEOFF_ALTITUDE_MAX_M:.0f} m AGL"
        )
    return altitude


class CorvusHandler(http.server.BaseHTTPRequestHandler):
    """Request handler routing between static files and API endpoints."""

    mavlink: MavlinkBridge | None = None
    store: VehicleStateStore | None = None
    ssh: SshBridge | None = None

    def log_message(self, fmt: str, *args) -> None:
        logger.debug("%s - %s", self.address_string(), fmt % args)

    def _send_json(self, data: dict, status: int = 200) -> None:
        body = json.dumps(_sanitize(data)).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(body)

    def _send_sse(self, event: str, data: str) -> None:
        self.wfile.write(f"event: {event}\ndata: {data}\n\n".encode("utf-8"))
        self.wfile.flush()

    def do_GET(self) -> None:
        path = urlparse(self.path).path
        if path.startswith("/api/"):
            self._handle_api_get(path)
        else:
            self._serve_static(path)

    def do_POST(self) -> None:
        path = urlparse(self.path).path
        if path.startswith("/api/"):
            self._handle_api_post(path)
        else:
            self._send_json({"error": "not found"}, 404)

    # ---- GET API ----
    def _handle_api_get(self, path: str) -> None:
        if path == "/api/version":
            px4_profile = "undetected"
            if self.store:
                px4_profile = self.store.get_snapshot().get("px4_version") or px4_profile
            self._send_json({
                "product": "Corvus GCS",
                "version": get_version(),
                "px4_profile": px4_profile,
            })
        elif path == "/api/state":
            if self.store:
                self._send_json(self.store.get_snapshot())
            else:
                self._send_json({"error": "no store"}, 500)
        elif path == "/api/telemetry":
            self._sse_telemetry()
        elif path == "/api/console/stream":
            self._sse_console()
        elif path == "/api/ssh/sessions":
            if self.ssh:
                self._send_json({"sessions": self.ssh.list_sessions()})
            else:
                self._send_json({"sessions": []})
        elif path == "/api/ssh/stream":
            self._sse_ssh_stream()
        elif path == "/api/mavlink/modes":
            if self.mavlink:
                self._send_json({"modes": self.mavlink.get_available_modes()})
            else:
                self._send_json({"modes": []})
        elif path == "/api/mavlink/serial-ports":
            self._api_mavlink_serial_ports()
        elif path == "/api/params":
            self._api_params()
        elif path == "/api/params/progress":
            self._sse_params()
        else:
            self._send_json({"error": "not found"}, 404)

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

    def _api_params(self) -> None:
        """Return parameter-download status and, when complete, the full list."""
        if self.mavlink is None:
            self._send_json({
                "state": "idle",
                "count": 0,
                "received": 0,
                "complete": False,
                "params": [],
            })
            return
        st = self.mavlink.param_status()
        complete = st["state"] == "complete"
        # lean: partial param lists are not sent — the editor waits for a complete set
        params = self.mavlink.get_params() if complete else []
        self._send_json({
            "state": st["state"],
            "count": st["count"],
            "received": st["received"],
            "complete": complete,
            "params": params,
        })

    # ---- POST API ----
    def _handle_api_post(self, path: str) -> None:
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length) if length else b"{}"
        try:
            payload = json.loads(raw)
        except json.JSONDecodeError:
            self._send_json({"error": "invalid json"}, 400)
            return
        if not isinstance(payload, dict):
            self._send_json({"error": "json payload must be an object"}, 400)
            return

        if path == "/api/console/command":
            self._api_console_command(payload)
        elif path == "/api/mavlink/connect":
            self._api_mavlink_connect(payload)
        elif path == "/api/mavlink/arm":
            self._api_mavlink_arm(payload)
        elif path == "/api/mavlink/mode":
            self._api_mavlink_mode(payload)
        elif path == "/api/mavlink/takeoff":
            self._api_mavlink_takeoff(payload)
        elif path == "/api/mavlink/land":
            self._api_mavlink_land(payload)
        elif path == "/api/mavlink/rtl":
            self._api_mavlink_rtl(payload)
        elif path == "/api/mavlink/gotopoints":
            self._api_mavlink_gotopoints(payload)
        elif path == "/api/ssh/connect":
            self._api_ssh_connect(payload)
        elif path == "/api/ssh/send":
            self._api_ssh_send(payload)
        elif path == "/api/ssh/disconnect":
            self._api_ssh_disconnect(payload)
        elif path == "/api/params/download":
            self._api_params_download(payload)
        elif path == "/api/params/set":
            self._api_params_set(payload)
        elif path == "/api/calibrate":
            self._api_calibrate(payload)
        elif path == "/api/autotune":
            self._api_autotune(payload)
        elif path == "/api/vibration/stream":
            self._api_vibration_stream(payload)
        else:
            self._send_json({"error": "not found"}, 404)

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
        else:
            self._send_json({"error": f"unknown command: {cmd}"}, 400)
            return
        if ok:
            self._send_json({"ok": True})
        else:
            error = self.mavlink.get_last_command_error() or f"command failed: {cmd}"
            status = 503 if "DISCONNECTED" in error else 409
            self._send_json({"ok": False, "error": error}, status)

    def _api_mavlink_connect(self, payload: dict) -> None:
        conn = payload.get("connection", "udp:127.0.0.1:14540")
        if self.mavlink:
            self.mavlink.stop()
            self.mavlink.set_connection(conn)
            self.mavlink.start()
        self._send_json({"ok": True, "connection": conn})

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

    def _api_mavlink_gotopoints(self, payload: dict) -> None:
        """Dispatch a multi-waypoint fly-to command to the vehicle."""
        raw_points = payload.get("points")
        if not isinstance(raw_points, list) or not raw_points:
            self._send_json({"ok": False, "error": "points must be a non-empty list"}, 400)
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

    def _api_ssh_connect(self, payload: dict) -> None:
        if not self.ssh:
            self._send_json({"error": "ssh not ready"}, 500)
            return
        name = payload.get("name", "device")
        host = payload.get("host", "")
        port = int(payload.get("port", 22))
        username = payload.get("username", "corvus")
        password = payload.get("password")
        key_path = payload.get("key_path")
        if not host:
            self._send_json({"error": "no host"}, 400)
            return
        ok = self.ssh.connect(name, host, port, username, password, key_path)
        self._send_json({"ok": ok, "connected": ok})

    def _api_ssh_send(self, payload: dict) -> None:
        name = payload.get("name", "")
        data = payload.get("data", "")
        if self.ssh and name:
            ok = self.ssh.send(name, data)
            self._send_json({"ok": ok})
        else:
            self._send_json({"error": "no session"}, 400)

    def _api_ssh_disconnect(self, payload: dict) -> None:
        name = payload.get("name", "")
        if self.ssh and name:
            ok = self.ssh.disconnect(name)
            self._send_json({"ok": ok})
        else:
            self._send_json({"error": "no session"}, 400)

    def _api_params_download(self, payload: dict) -> None:
        """Trigger a full parameter-list download from the vehicle."""
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        ok = self.mavlink.request_param_list()
        if ok:
            self._send_json({"ok": True, "state": "downloading"})
        else:
            error = self.mavlink.get_last_command_error() or "not connected"
            self._send_json({"ok": False, "error": error}, 503)

    def _api_params_set(self, payload: dict) -> None:
        """Write a single parameter value to the vehicle."""
        name = payload.get("name", "")
        value = payload.get("value")
        if not isinstance(name, str) or not name:
            self._send_json({"ok": False, "error": "name must be a non-empty string"}, 400)
            return
        # bool is a subclass of int — reject it explicitly so we never write 0/1 silently
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            self._send_json({"ok": False, "error": "value must be a number"}, 400)
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        ok = self.mavlink.set_param(name, float(value))
        if ok:
            self._send_json({"ok": True})
        else:
            error = self.mavlink.get_last_command_error() or "parameter write failed"
            status = 503 if "not connected" in error else 409
            self._send_json({"ok": False, "error": error}, status)

    def _api_calibrate(self, payload: dict) -> None:
        """Start a sensor calibration on the vehicle."""
        raw = payload.get("type", "")
        if not isinstance(raw, str) or raw.lower() not in CALIB_SENSORS:
            self._send_json({"ok": False, "error": "unknown calibration type"}, 400)
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        sensor = raw.lower()
        ok = self.mavlink.calibrate(sensor)
        if ok:
            self._send_json({"ok": True})
        else:
            error = self.mavlink.get_last_command_error() or "calibration failed"
            status = 503 if "not connected" in error else 409
            self._send_json({"ok": False, "error": error}, status)

    def _api_autotune(self, payload: dict) -> None:
        """Start an autotune on the given axis."""
        raw = payload.get("axis", "")
        if not isinstance(raw, str) or raw.lower() not in AUTOTUNE_AXES:
            self._send_json({"ok": False, "error": "unknown autotune axis"}, 400)
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        axis = raw.lower()
        ok = self.mavlink.autotune(axis)
        if ok:
            self._send_json({"ok": True})
        else:
            error = self.mavlink.get_last_command_error() or "autotune failed"
            status = 503 if "not connected" in error else 409
            self._send_json({"ok": False, "error": error}, status)

    def _api_vibration_stream(self, payload: dict) -> None:
        """Enable/disable high-rate VIBRATION telemetry on demand.

        Read-only stream-rate control — safe while armed, unlike
        params/calibrate/autotune. The frontend toggles this when the
        vibration plugin opens/closes (lean: high-rate only while open).
        """
        enabled = payload.get("enabled")
        if not isinstance(enabled, bool):
            self._send_json({"ok": False, "error": "enabled must be boolean"}, 400)
            return
        raw_rate = payload.get("rate_hz", 10)
        # bool is a subclass of int — reject it so True is never coerced to 1 Hz
        if isinstance(raw_rate, bool) or not isinstance(raw_rate, (int, float)):
            self._send_json({"ok": False, "error": "rate_hz must be a number"}, 400)
            return
        rate_hz = int(raw_rate)
        if raw_rate != rate_hz or not 1 <= rate_hz <= 50:
            self._send_json(
                {"ok": False, "error": "rate_hz must be between 1 and 50 Hz"}, 400
            )
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        ok = self.mavlink.set_vibration_stream(enabled, rate_hz)
        if ok:
            self._send_json({"ok": True, "enabled": enabled, "rate_hz": rate_hz})
        else:
            error = self.mavlink.get_last_command_error() or "vibration stream request failed"
            status = 503 if "not connected" in error else 409
            self._send_json({"ok": False, "error": error}, status)

    # ---- SSE endpoints ----
    def _sse_telemetry(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        q = _BoundedSseBuffer(TELEMETRY_SSE_CAPACITY)
        listener = q.put_latest
        if self.store:
            self.store.add_listener(listener)
            self._send_sse("state", json.dumps(_sanitize(self.store.get_snapshot())))
        try:
            while True:
                try:
                    snap = q.get(timeout=15)
                    self._send_sse("state", json.dumps(_sanitize(snap)))
                except queue.Empty:
                    self._send_sse("ping", "{}")
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            if self.store:
                self.store.remove_listener(listener)

    def _sse_console(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        q = _BoundedSseBuffer(CONSOLE_SSE_CAPACITY)
        listener = q.put_console
        if self.mavlink:
            self.mavlink.add_console_sub(listener)
        try:
            while True:
                try:
                    entry = q.get(timeout=15)
                    self._send_sse("message", json.dumps(entry))
                except queue.Empty:
                    self._send_sse("ping", "{}")
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            if self.mavlink:
                self.mavlink.remove_console_sub(listener)

    def _sse_params(self) -> None:
        """SSE stream of parameter-download progress (latest-wins, coalesced)."""
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        q = _BoundedSseBuffer(PARAMS_SSE_CAPACITY)
        listener = q.put_latest
        if self.mavlink:
            self.mavlink.add_param_listener(listener)
            status = self.mavlink.param_status()
        else:
            status = {"state": "idle", "count": 0, "received": 0}
        self._send_sse("progress", json.dumps(_sanitize({
            "state": status["state"],
            "count": status["count"],
            "received": status["received"],
        })))
        try:
            while True:
                try:
                    entry = q.get(timeout=15)
                    self._send_sse("progress", json.dumps({
                        "state": entry["state"],
                        "count": entry["count"],
                        "received": entry["received"],
                    }))
                except queue.Empty:
                    self._send_sse("ping", "{}")
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            if self.mavlink:
                self.mavlink.remove_param_listener(listener)

    def _sse_ssh_stream(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        q: queue.Queue = queue.Queue()
        listener = lambda text: q.put(text)
        attached_name: str | None = None
        if self.ssh:
            sessions = self.ssh.list_sessions()
            for s in sessions:
                if s["connected"]:
                    attached_name = s["name"]
                    session = self.ssh.get_session(s["name"])
                    if session:
                        session.add_sub(listener)
                    break
        try:
            while True:
                try:
                    text = q.get(timeout=15)
                    self._send_sse("output", json.dumps({"text": text, "name": attached_name}))
                except queue.Empty:
                    self._send_sse("ping", "{}")
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            if self.ssh and attached_name:
                session = self.ssh.get_session(attached_name)
                if session:
                    session.remove_sub(listener)

    # ---- Static files ----
    def _serve_static(self, path: str) -> None:
        if path == "/":
            path = "/index.html"
        file_path = (WEB_DIR / path.lstrip("/")).resolve()
        try:
            file_path.relative_to(WEB_DIR)
        except ValueError:
            self._send_json({"error": "forbidden"}, 403)
            return
        if not file_path.is_file():
            self._send_json({"error": "not found"}, 404)
            return
        ctype, _ = mimetypes.guess_type(str(file_path))
        if ctype is None:
            ctype = "application/octet-stream"
        body = file_path.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        self.wfile.write(body)


class CorvusServer(socketserver.ThreadingTCPServer):
    """Threaded TCP server with clean shutdown."""
    allow_reuse_address = True
    daemon_threads = True


def create_server(
    port: int = 8000,
    mavlink_conn: str = "udp:127.0.0.1:14540",
) -> CorvusServer:
    """Create and wire up the full Corvus backend server."""
    store = VehicleStateStore()
    mavlink = MavlinkBridge(store, mavlink_conn)
    ssh = SshBridge()

    CorvusHandler.store = store
    CorvusHandler.mavlink = mavlink
    CorvusHandler.ssh = ssh

    server = CorvusServer(("", port), CorvusHandler)
    server.mavlink = mavlink
    server.ssh = ssh
    server.store = store
    mavlink.start()
    return server
