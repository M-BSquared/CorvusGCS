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
import re
import socketserver
import threading
import time
import urllib.request
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlparse

from . import tile_sources
from .mavlink_bridge import (
    TAKEOFF_ALTITUDE_MAX_M,
    TAKEOFF_ALTITUDE_MIN_M,
    MavlinkBridge,
)
from .ssh_bridge import SshBridge
from .state_store import VehicleStateStore, _sanitize
from .tile_cache import TileCache, default_cache_dir
from .version import get_version

logger = logging.getLogger("corvus.server")

REPO_ROOT = Path(__file__).resolve().parent.parent
WEB_DIR = REPO_ROOT / "src"
TELEMETRY_SSE_CAPACITY = 1
CONSOLE_SSE_CAPACITY = 100
PARAMS_SSE_CAPACITY = 16
TILES_PROGRESS_SSE_CAPACITY = 16
TILE_UPSTREAM_TIMEOUT_S = 10
# Path-parameter tile route: /api/tiles/<source>/<z>/<x>/<y>.png
# Checked in _handle_api_get only when the path ends in ".png", so it can
# never shadow the exact /api/tiles/{sources,jobs,progress,download,cancel}
# routes (none of which end in ".png" with four numeric segments).
_TILE_PATH_RE = re.compile(r"^/api/tiles/([^/]+)/(\d+)/(\d+)/(\d+)\.png$")
CALIB_SENSORS = frozenset({
    "gyro", "compass", "baro", "accel", "level", "accel_quick", "airspeed",
})
AUTOTUNE_AXES = frozenset({"roll", "pitch", "yaw", "all"})

# Route registry: the @route decorator tags handler methods while the
# CorvusHandler class body executes; the pending entries are wired onto the
# class-level _GET_ROUTES/_POST_ROUTES tables after the class is defined. The
# dispatch methods then resolve path -> method name by lookup instead of an
# if/elif chain, so adding an endpoint is just "decorate a method".
_pending_routes: list[tuple[str, str, str]] = []


def route(http_method: str, path: str):
    """Register the decorated handler as the route for *path*."""
    def decorator(func):
        _pending_routes.append((http_method, path, func.__name__))
        return func
    return decorator


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


class _TileProgressBus:
    """Pub-sub fan-out so many SSE clients can watch one download job.

    The downloader accepts a single ``on_progress`` callback per job (set at
    ``start()`` time); SSE clients come and go. The bus is the one callback the
    downloader calls; it reads ``job_id`` from the progress dict and fans the
    dict out to every registered subscriber for that job.

    Contract handed to the downloader: it invokes ``on_progress(progress)``
    with a dict containing at least ``{job_id, state, done, total, failed}``
    — the same shape its own ``status()`` returns. Without ``job_id`` the bus
    cannot route, so the call is dropped.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._subs: dict[str, list[Any]] = collections.defaultdict(list)

    def subscribe(self, job_id: str, fn: Any) -> None:
        with self._lock:
            self._subs[job_id].append(fn)

    def unsubscribe(self, job_id: str, fn: Any) -> None:
        with self._lock:
            try:
                self._subs[job_id].remove(fn)
            except ValueError:
                pass
            if not self._subs[job_id]:
                self._subs.pop(job_id, None)

    def make_on_progress(self) -> Any:
        """Return the single ``on_progress`` callback to hand to the downloader."""
        def _on_progress(progress: Any) -> None:
            if not isinstance(progress, dict):
                return
            job_id = progress.get("job_id")
            if not job_id:
                return
            with self._lock:
                subs = list(self._subs.get(job_id, ()))
            for fn in subs:
                try:
                    fn(progress)
                except Exception:  # noqa: BLE001 - a bad subscriber must not kill the download
                    logger.exception("tile progress subscriber raised")
        return _on_progress


class _TileDownloaderPool:
    """Unified facade over one TileDownloader per source.

    ``TileDownloader`` binds a single ``TileCache`` at construction (see its
    contract); we keep one cache per source so ``/api/tiles/<source>/...``
    serves the correct raster, which requires one downloader per source. The
    pool exposes the single ``start``/``cancel``/``status``/``list_jobs``/
    ``shutdown`` surface the HTTP layer wants and routes by source (``start``)
    or ``job_id`` (``cancel``/``status``).
    """

    def __init__(
        self,
        caches: dict[str, TileCache],
        downloader_cls: Any,
        max_workers: int = 3,
    ) -> None:
        self._downloaders: dict[str, Any] = {
            sid: downloader_cls(cache, max_workers=max_workers)
            for sid, cache in caches.items()
        }

    def start(
        self,
        source: str,
        upstream: str,
        bounds: tuple,
        minzoom: int,
        maxzoom: int,
        on_progress: Any = None,
    ) -> str:
        dl = self._downloaders.get(source)
        if dl is None:
            raise ValueError(f"no downloader for source {source!r}")
        return dl.start(source, upstream, bounds, minzoom, maxzoom, on_progress=on_progress)

    def cancel(self, job_id: str) -> bool:
        for dl in self._downloaders.values():
            if dl.cancel(job_id):
                return True
        return False

    def status(self, job_id: str) -> dict | None:
        for dl in self._downloaders.values():
            st = dl.status(job_id)
            if st is not None:
                return st
        return None

    def list_jobs(self) -> list[dict]:
        jobs: list[dict] = []
        for dl in self._downloaders.values():
            jobs.extend(dl.list_jobs())
        return jobs

    def shutdown(self) -> None:
        for dl in self._downloaders.values():
            try:
                dl.shutdown()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("tile downloader shutdown failed")
        self._downloaders.clear()


def _build_tile_downloader(
    caches: dict[str, TileCache],
) -> _TileDownloaderPool | None:
    """Construct the per-source downloader pool, or None if the (parallel-built)
    ``tile_downloader`` module is not importable yet — keeps the server usable
    while that module is mid-edit."""
    try:
        from .tile_downloader import TileDownloader  # lazy: module may be mid-edit
    except ImportError:
        logger.warning("tile_downloader module not available; download endpoints disabled")
        return None
    return _TileDownloaderPool(caches, TileDownloader)


def _validate_tile_bounds(bounds: Any) -> tuple[str | None, tuple[float, float, float, float] | None]:
    """Validate a ``{w,s,e,n}`` bounds object.

    Returns ``(error, None)`` on failure or ``(None, (w,s,e,n))`` on success.
    """
    if not isinstance(bounds, dict):
        return ("bounds must be an object {w,s,e,n}", None)
    for key in ("w", "s", "e", "n"):
        value = bounds.get(key)
        # bool is a subclass of int — reject it so True is never coerced to 1.0
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(float(value)):
            return (f"bounds.{key} must be a finite number", None)
    w, s, e, n = float(bounds["w"]), float(bounds["s"]), float(bounds["e"]), float(bounds["n"])
    if not -180.0 <= w <= 180.0 or not -180.0 <= e <= 180.0:
        return ("bounds w/e must be in [-180, 180]", None)
    if not -90.0 <= s <= 90.0 or not -90.0 <= n <= 90.0:
        return ("bounds s/n must be in [-90, 90]", None)
    if w > e or s > n:
        return ("bounds must satisfy w<=e and s<=n", None)
    return (None, (w, s, e, n))


def _validate_tile_zooms(minzoom: Any, maxzoom: Any, src_maxzoom: int) -> str | None:
    """Validate ``minzoom``/``maxzoom`` (ints, 0..22, min<=max, within source cap)."""
    if isinstance(minzoom, bool) or not isinstance(minzoom, (int, float)):
        return "minzoom must be a number"
    if isinstance(maxzoom, bool) or not isinstance(maxzoom, (int, float)):
        return "maxzoom must be a number"
    if float(minzoom) != int(minzoom) or float(maxzoom) != int(maxzoom):
        return "zooms must be integers"
    mz, Mz = int(minzoom), int(maxzoom)
    if not 0 <= mz <= 22 or not 0 <= Mz <= 22:
        return "zooms must be in [0, 22]"
    if mz > Mz:
        return "minzoom must be <= maxzoom"
    if Mz > src_maxzoom:
        return f"maxzoom exceeds source max ({src_maxzoom})"
    return None


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


# 1-slot memoization of the serialized telemetry snapshot, keyed by the
# store's mutation version: N browser tabs sharing one backend re-serialize
# the SAME snapshot exactly once per version instead of N times. The store
# reference is held (``is``) so a GC-reused id can never match a stale entry,
# and a stale snapshot from the SSE queue is never cached for a newer
# version (see the ``is store.get_snapshot()`` guard).
_serialize_lock = threading.Lock()
_last_serialized_store: VehicleStateStore | None = None
_last_serialized_version: int | None = None
_last_serialized_bytes: bytes = b""


def _serialize_telemetry_snapshot(
    store: VehicleStateStore, snap: dict[str, Any]
) -> bytes:
    """Serialize *snap* once per store version, reusing cached bytes.

    With N connected SSE clients the same snapshot version is serialized at
    most once: the first client to see a new version pays the ``json.dumps`` +
    ``_sanitize`` cost and stores the bytes; every other client (and every
    later read at the same version) reuses them. ``_sanitize`` is applied
    here, once, so NaN/Inf never reach the wire.

    A snapshot pulled from the SSE queue may be stale (the store advanced
    between the listener push and this call): it is serialized and sent
    as-is (matching the pre-optimization behaviour) but is cached only when
    it is still the store's current snapshot, so a stale snapshot can never
    poison the cache for the fresh version.
    """
    global _last_serialized_store, _last_serialized_version, _last_serialized_bytes
    version = store.version()
    with _serialize_lock:
        if _last_serialized_store is store and version == _last_serialized_version:
            return _last_serialized_bytes
        data = json.dumps(_sanitize(snap)).encode("utf-8")
        # Cache only when snap is still the store's current (cached) snapshot
        # for this version; a stale snap from the queue is sent but not cached.
        if snap is store.get_snapshot():
            _last_serialized_store = store
            _last_serialized_version = version
            _last_serialized_bytes = data
        return data


class CorvusHandler(http.server.BaseHTTPRequestHandler):
    """Request handler routing between static files and API endpoints."""

    mavlink: MavlinkBridge | None = None
    store: VehicleStateStore | None = None
    ssh: SshBridge | None = None
    # Tile resources: per-source caches + a single downloader facade + the
    # progress pub-sub bus. None when tiles are not configured (e.g. the
    # parallel-built tile_downloader module is mid-edit).
    tile_caches: dict[str, TileCache] | None = None
    tile_downloader: Any = None
    tile_progress_bus: _TileProgressBus | None = None

    _GET_ROUTES: dict[str, str] = {}
    _POST_ROUTES: dict[str, str] = {}

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

    def _send_sse_bytes(self, event: str, data: bytes) -> None:
        # data is pre-encoded utf-8 JSON; write the framing directly to avoid
        # the double encode that ``_send_sse`` would incur for a str payload.
        # Wire output is byte-identical to ``_send_sse(event, data.decode())``.
        self.wfile.write(b"event: " + event.encode("utf-8") + b"\ndata: " + data + b"\n\n")
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
        if path.startswith("/api/"):
            # Path-parameter tile route: /api/tiles/<source>/<z>/<x>/<y>.png
            # Only matched when the path ends in ".png", so the exact
            # /api/tiles/{sources,jobs,progress} routes are never shadowed.
            if path.startswith("/api/tiles/") and path.endswith(".png"):
                m = _TILE_PATH_RE.match(path)
                if m is not None:
                    self._api_tiles_serve(
                        m.group(1),
                        int(m.group(2)),
                        int(m.group(3)),
                        int(m.group(4)),
                    )
                    return
            method_name = self._GET_ROUTES.get(path)
            if method_name is not None:
                getattr(self, method_name)()
            else:
                self._send_json({"error": "not found"}, 404)
        else:
            self._serve_static(path)

    @route("GET", "/api/version")
    def _api_version(self) -> None:
        px4_profile = "undetected"
        if self.store:
            px4_profile = self.store.get_snapshot().get("px4_version") or px4_profile
        self._send_json({
            "product": "Corvus GCS",
            "version": get_version(),
            "px4_profile": px4_profile,
        })

    @route("GET", "/api/state")
    def _api_state(self) -> None:
        if self.store:
            self._send_json(self.store.get_snapshot())
        else:
            self._send_json({"error": "no store"}, 500)

    @route("GET", "/api/ssh/sessions")
    def _api_ssh_sessions(self) -> None:
        if self.ssh:
            self._send_json({"sessions": self.ssh.list_sessions()})
        else:
            self._send_json({"sessions": []})

    @route("GET", "/api/mavlink/modes")
    def _api_mavlink_modes(self) -> None:
        if self.mavlink:
            self._send_json({"modes": self.mavlink.get_available_modes()})
        else:
            self._send_json({"modes": []})

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

    @route("GET", "/api/params")
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

        if path.startswith("/api/"):
            method_name = self._POST_ROUTES.get(path)
            if method_name is not None:
                getattr(self, method_name)(payload)
            else:
                self._send_json({"error": "not found"}, 404)
        else:
            self._send_json({"error": "not found"}, 404)

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
        else:
            self._send_json({"error": f"unknown command: {cmd}"}, 400)
            return
        if ok:
            self._send_json({"ok": True})
        else:
            error = self.mavlink.get_last_command_error() or f"command failed: {cmd}"
            status = 503 if "DISCONNECTED" in error else 409
            self._send_json({"ok": False, "error": error}, status)

    @route("POST", "/api/mavlink/connect")
    def _api_mavlink_connect(self, payload: dict) -> None:
        conn = payload.get("connection", "udp:127.0.0.1:14540")
        if self.mavlink:
            self.mavlink.stop()
            self.mavlink.set_connection(conn)
            self.mavlink.start()
        self._send_json({"ok": True, "connection": conn})

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

    @route("POST", "/api/mavlink/gotopoints")
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

    @route("POST", "/api/ssh/connect")
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

    @route("POST", "/api/ssh/send")
    def _api_ssh_send(self, payload: dict) -> None:
        name = payload.get("name", "")
        data = payload.get("data", "")
        if self.ssh and name:
            ok = self.ssh.send(name, data)
            self._send_json({"ok": ok})
        else:
            self._send_json({"error": "no session"}, 400)

    @route("POST", "/api/ssh/disconnect")
    def _api_ssh_disconnect(self, payload: dict) -> None:
        name = payload.get("name", "")
        if self.ssh and name:
            ok = self.ssh.disconnect(name)
            self._send_json({"ok": ok})
        else:
            self._send_json({"error": "no session"}, 400)

    @route("POST", "/api/params/download")
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

    @route("POST", "/api/params/set")
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

    @route("POST", "/api/calibrate")
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

    @route("POST", "/api/autotune")
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

    @route("POST", "/api/vibration/stream")
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
    @route("GET", "/api/telemetry")
    def _sse_telemetry(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        q = _BoundedSseBuffer(TELEMETRY_SSE_CAPACITY)
        listener = q.put_latest
        try:
            if self.store:
                self.store.add_listener(listener)
                # Serialize the initial snapshot through the version-keyed cache so
                # a second tab connecting at the same version reuses these bytes.
                self._send_sse_bytes(
                    "state",
                    _serialize_telemetry_snapshot(self.store, self.store.get_snapshot()),
                )
            while True:
                try:
                    snap = q.get(timeout=15)
                    self._send_sse_bytes(
                        "state", _serialize_telemetry_snapshot(self.store, snap)
                    )
                except queue.Empty:
                    self._send_sse("ping", "{}")
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            if self.store:
                self.store.remove_listener(listener)

    @route("GET", "/api/console/stream")
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

    @route("GET", "/api/params/progress")
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
        try:
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

    @route("GET", "/api/ssh/stream")
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

    # ---- Tile cache / download / serve ----
    @route("GET", "/api/tiles/sources")
    def _api_tiles_sources(self) -> None:
        """List every source with its live cache stats; always 200."""
        caches = self.tile_caches or {}
        sources = []
        for entry in tile_sources.list_sources():
            sid = entry["id"]
            cache = caches.get(sid)
            stats = cache.stats() if cache is not None else {"count": 0, "minzoom": None, "maxzoom": None}
            sources.append({
                "id": sid,
                "label": entry["label"],
                "maxzoom": entry["maxzoom"],
                "cached_count": stats["count"],
                "minzoom": stats["minzoom"],
                "maxzoom": stats["maxzoom"],
            })
        self._send_json({"sources": sources})

    @route("GET", "/api/tiles/jobs")
    def _api_tiles_jobs(self) -> None:
        """List all download jobs across every source."""
        if self.tile_downloader is None:
            self._send_json({"jobs": []})
            return
        self._send_json({"jobs": self.tile_downloader.list_jobs()})

    @route("POST", "/api/tiles/download")
    def _api_tiles_download(self, payload: dict) -> None:
        """Start a tile download job for a source within bounds/zooms."""
        if self.tile_downloader is None:
            self._send_json({"ok": False, "error": "tile service unavailable"}, 503)
            return
        source = payload.get("source")
        if not isinstance(source, str) or tile_sources.get(source) is None:
            self._send_json({"ok": False, "error": "unknown source"}, 400)
            return
        src = tile_sources.get(source)
        err, bounds = _validate_tile_bounds(payload.get("bounds"))
        if err is not None:
            self._send_json({"ok": False, "error": err}, 400)
            return
        zoom_err = _validate_tile_zooms(payload.get("minzoom"), payload.get("maxzoom"), src["maxzoom"])
        if zoom_err is not None:
            self._send_json({"ok": False, "error": zoom_err}, 400)
            return
        minzoom = int(payload["minzoom"])
        maxzoom = int(payload["maxzoom"])
        on_progress = None
        if self.tile_progress_bus is not None:
            on_progress = self.tile_progress_bus.make_on_progress()
        try:
            job_id = self.tile_downloader.start(
                source, src["upstream"], bounds, minzoom, maxzoom, on_progress=on_progress
            )
        except Exception as exc:  # noqa: BLE001 - a download submit must never 500
            logger.exception("tile download start failed")
            self._send_json({"ok": False, "error": f"start failed: {exc}"}, 500)
            return
        status = self.tile_downloader.status(job_id) if job_id else None
        if status and status.get("state") == "failed":
            self._send_json(
                {"ok": False, "error": status.get("error", "download failed")}, 409
            )
            return
        self._send_json({"job_id": job_id})

    @route("POST", "/api/tiles/cancel")
    def _api_tiles_cancel(self, payload: dict) -> None:
        """Cancel a running download job by id."""
        if self.tile_downloader is None:
            self._send_json({"ok": False, "error": "tile service unavailable"}, 503)
            return
        job_id = payload.get("id")
        if not isinstance(job_id, str) or not job_id:
            self._send_json({"ok": False, "error": "id required"}, 400)
            return
        if self.tile_downloader.cancel(job_id):
            self._send_json({"ok": True})
        else:
            self._send_json({"ok": False, "error": "unknown job"}, 404)

    @route("GET", "/api/tiles/progress")
    def _sse_tiles_progress(self) -> None:
        """SSE stream of one download job's progress (latest-wins, coalesced)."""
        query = urlparse(self.path).query
        params = parse_qs(query)
        job_id = params.get("id", [None])[0]
        if not job_id:
            self._send_json({"error": "missing id"}, 400)
            return
        if self.tile_downloader is None:
            self._send_json({"error": "tile service unavailable"}, 503)
            return
        status = self.tile_downloader.status(job_id)
        if status is None:
            self._send_json({"error": "unknown job"}, 404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        q = _BoundedSseBuffer(TILES_PROGRESS_SSE_CAPACITY)
        listener = q.put_latest
        try:
            if self.tile_progress_bus is not None:
                self.tile_progress_bus.subscribe(job_id, listener)
            self._send_sse("progress", json.dumps(_sanitize(status)))
            while True:
                try:
                    entry = q.get(timeout=15)
                    self._send_sse("progress", json.dumps(_sanitize(entry)))
                    state = entry.get("state") if isinstance(entry, dict) else None
                    if state in ("done", "cancelled", "failed"):
                        break
                except queue.Empty:
                    self._send_sse("ping", "{}")
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            if self.tile_progress_bus is not None:
                self.tile_progress_bus.unsubscribe(job_id, listener)

    def _api_tiles_serve(self, source: str, z: int, x: int, y: int) -> None:
        """Serve a cached tile, transparently fetching+ caching from upstream.

        Online: a cache miss is filled from the upstream template (short
        timeout) then stored, so the map works and the cache grows in the
        field. Offline (or fetch failure): a miss is a 404.
        """
        if tile_sources.get(source) is None:
            self._send_json({"error": "not found"}, 404)
            return
        if z < 0 or z > 22 or x < 0 or y < 0 or x >= (1 << z) or y >= (1 << z):
            self._send_json({"error": "tile out of range"}, 404)
            return
        caches = self.tile_caches or {}
        cache = caches.get(source)
        blob = cache.get_tile(z, x, y) if cache is not None else None
        if blob is None:
            blob = self._fetch_upstream_tile(source, z, x, y)
            if blob is None:
                self._send_json({"error": "tile not available"}, 404)
                return
            if cache is not None:
                try:
                    cache.put_tile(z, x, y, blob)
                except Exception:  # noqa: BLE001 - caching is best-effort
                    logger.exception("tile cache write failed")
        self.send_response(200)
        self.send_header("Content-Type", "image/png")
        self.send_header("Content-Length", str(len(blob)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Cache-Control", "max-age=86400")
        self.end_headers()
        self.wfile.write(blob)

    def _fetch_upstream_tile(self, source: str, z: int, x: int, y: int) -> bytes | None:
        """Best-effort online cache-fill for a single tile; None on any failure."""
        src = tile_sources.get(source)
        if src is None:
            return None
        url = (src["upstream"]
               .replace("{z}", str(z))
               .replace("{y}", str(y))
               .replace("{x}", str(x)))
        try:
            req = urllib.request.Request(
                url, headers={"User-Agent": f"CorvusGCS/{get_version()}"}
            )
            with urllib.request.urlopen(req, timeout=TILE_UPSTREAM_TIMEOUT_S) as resp:
                data = resp.read()
        except Exception:  # noqa: BLE001 - offline field use must never 500
            logger.debug("upstream tile fetch failed: %s", url, exc_info=True)
            return None
        return data or None

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


# Wire @route declarations collected during the CorvusHandler class body onto
# the class-level route tables that _handle_api_get/_handle_api_post dispatch
# through.
for _http_method, _path, _name in _pending_routes:
    table = CorvusHandler._GET_ROUTES if _http_method == "GET" else CorvusHandler._POST_ROUTES
    table[_path] = _name
_pending_routes.clear()


class CorvusServer(socketserver.ThreadingTCPServer):
    """Threaded TCP server with clean shutdown."""
    allow_reuse_address = True
    daemon_threads = True

    def shutdown(self) -> None:
        """Stop the HTTP loop and release every tile resource.

        Download workers are stopped first so they are not mid-write to the
        caches; the HTTP serve loop is then stopped; caches are closed last so
        no in-flight handler touches a closed SQLite handle during teardown.
        Any failure is logged, never raised — shutdown must always complete.
        """
        downloader = getattr(self, "tile_downloader", None)
        if downloader is not None:
            try:
                downloader.shutdown()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("tile downloader shutdown failed")
        try:
            super().shutdown()
        except Exception:  # noqa: BLE001 - shutdown must not raise
            logger.exception("http server shutdown failed")
        caches = getattr(self, "tile_caches", None) or {}
        for cache in caches.values():
            try:
                cache.close()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("tile cache close failed")
        self.tile_downloader = None
        self.tile_caches = {}


def create_server(
    port: int = 8000,
    mavlink_conn: str = "udp:127.0.0.1:14540",
) -> CorvusServer:
    """Create and wire up the full Corvus backend server."""
    store = VehicleStateStore()
    mavlink = MavlinkBridge(store, mavlink_conn)
    ssh = SshBridge()

    # Tile resources: one MBTiles cache per source under ~/.corvus/tiles/.
    # TileDownloader binds a single cache at construction, so per-source caches
    # imply per-source downloaders; _TileDownloaderPool presents them as one.
    cache_dir = default_cache_dir()
    tile_caches: dict[str, TileCache] = {
        sid: TileCache(os.path.join(cache_dir, f"{sid}.mbtiles"))
        for sid in tile_sources.TILE_SOURCES
    }
    tile_progress_bus = _TileProgressBus()
    tile_downloader = _build_tile_downloader(tile_caches)

    CorvusHandler.store = store
    CorvusHandler.mavlink = mavlink
    CorvusHandler.ssh = ssh
    CorvusHandler.tile_caches = tile_caches
    CorvusHandler.tile_downloader = tile_downloader
    CorvusHandler.tile_progress_bus = tile_progress_bus

    server = CorvusServer(("", port), CorvusHandler)
    server.mavlink = mavlink
    server.ssh = ssh
    server.store = store
    server.tile_caches = tile_caches
    server.tile_downloader = tile_downloader
    mavlink.start()
    return server
