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
        elif path == "/api/ssh/connect":
            self._api_ssh_connect(payload)
        elif path == "/api/ssh/send":
            self._api_ssh_send(payload)
        elif path == "/api/ssh/disconnect":
            self._api_ssh_disconnect(payload)
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
