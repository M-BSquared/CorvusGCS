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
import errno
import os
import queue
import re
import socketserver
import tempfile
import threading
import time
import urllib.request
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlparse

from . import motor_config, safety_config, tile_sources
from .config import (
    CorvusConfig,
    default_config_path,
    load_config,
    save_config,
    to_public_dict,
)
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
FIRMWARE_SSE_CAPACITY = 16
# Interactive cache-fill timeout. Short on purpose: this is the browser waiting
# for a map tile, and a tile that takes longer than this has already missed the
# frame it was wanted for. The offline DOWNLOADER uses its own, longer timeout
# (corvus/tile_downloader._FETCH_TIMEOUT) because there nobody is watching.
TILE_UPSTREAM_TIMEOUT_S = 4

# Offline circuit breaker for the interactive fill path.
#
# In the field there is no internet, so every tile the operator pans onto that
# was not pre-downloaded is a cache miss, and every miss used to spend the full
# timeout failing to reach the upstream. A viewport is dozens of tiles, so the
# map became treacle exactly when it needed to be quick. After this many
# consecutive failures the fill path stops trying and misses 404 instantly,
# which is what the map wants anyway; one success re-arms it.
TILE_UPSTREAM_FAIL_THRESHOLD = 3
# How long to stay tripped before probing the network again. Long enough that a
# genuinely offline session is not re-testing constantly, short enough that
# walking back into coverage recovers on its own without a restart.
TILE_UPSTREAM_COOLDOWN_S = 30.0
# Body-size caps defend against a malformed/huge Content-Length: an unguarded
# int() crashes the handler on a non-numeric header, and an unbounded read
# can exhaust memory. General JSON API vs raw firmware binary (a few MB).
MAX_JSON_BODY_BYTES = 8 * 1024 * 1024
MAX_FIRMWARE_BODY_BYTES = 64 * 1024 * 1024
# Operator-supplied company logo: a brand mark, not an image library, so a few
# MB is already generous and keeps a mis-picked photo out of the config dir.
MAX_LOGO_BODY_BYTES = 4 * 1024 * 1024
# A ULog opened from anywhere on the operator's machine. Generous — a long
# multirotor flight at full logging rate is tens of MB — but far below the
# parser's own ceiling, because this one is held in memory as a request body
# before anything has looked at it.
MAX_ULOG_BODY_BYTES = 128 * 1024 * 1024
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"
# Path-parameter tile route: /api/tiles/<source>/<z>/<x>/<y>.png
# Checked in _handle_api_get only when the path ends in ".png", so it can
# never shadow the exact /api/tiles/{sources,jobs,progress,download,cancel}
# routes (none of which end in ".png" with four numeric segments).
_TILE_PATH_RE = re.compile(r"^/api/tiles/([^/]+)/(\d+)/(\d+)/(\d+)\.png$")
# "motor" = ESC/motor calibration (MAV_CMD_PREFLIGHT_CALIBRATION param7=1.0); motors spin — props must be removed
CALIB_SENSORS = frozenset({
    "gyro", "compass", "baro", "accel", "level", "accel_quick", "airspeed", "motor",
})
AUTOTUNE_AXES = frozenset({"roll", "pitch", "yaw", "all"})
# PX4 NuttShell (NSH) builtins with no MAVLink-command equivalent (e.g.
# `listener <topic>` subscribes to a uORB topic and prints it). Routed to the
# serial-control debug shell, not a MAV_CMD. `help` stays the local help and
# `param` is not a console command, so this set is disjoint from the Corvus
# command set above.
SHELL_COMMANDS = frozenset({
    "listener", "top", "free", "dmesg", "tasks", "perf", "boot_log", "hrt",
})

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


class _UpstreamBreaker:
    """Circuit breaker around the interactive tile cache-fill.

    Field use is offline, so a pan onto un-downloaded ground produces a whole
    viewport of cache misses at once. Each miss that tries the network costs
    the full timeout, and the map stops responding precisely when the operator
    is looking for something. After ``threshold`` consecutive failures the
    breaker trips and misses fail instantly for ``cooldown`` seconds; the first
    request after that is allowed through as a probe, and any success closes
    the breaker again — so walking back into coverage recovers by itself.

    Thread-safe: tiles are served from many handler threads at once, which is
    the whole reason the failures arrive in a burst.
    """

    def __init__(self, threshold: int = TILE_UPSTREAM_FAIL_THRESHOLD,
                 cooldown: float = TILE_UPSTREAM_COOLDOWN_S) -> None:
        self._threshold = max(1, int(threshold))
        self._cooldown = float(cooldown)
        self._lock = threading.Lock()
        self._failures = 0
        self._open_until = 0.0

    def allow(self) -> bool:
        """True if a network attempt should be made now.

        When the cooldown has elapsed this returns True exactly once per
        cooldown window (a probe) unless the attempt succeeds — that way a
        still-offline session does not go back to paying the timeout on every
        tile the moment the window expires.
        """
        with self._lock:
            if self._failures < self._threshold:
                return True
            if time.monotonic() >= self._open_until:
                # Probe: re-arm the window now so only this one request gets
                # through until it reports back.
                self._open_until = time.monotonic() + self._cooldown
                return True
            return False

    def record_success(self) -> None:
        with self._lock:
            self._failures = 0
            self._open_until = 0.0

    def record_failure(self) -> None:
        with self._lock:
            self._failures += 1
            if self._failures >= self._threshold:
                self._open_until = time.monotonic() + self._cooldown

    def is_open(self) -> bool:
        """True while the breaker is suppressing network attempts."""
        with self._lock:
            return self._failures >= self._threshold and time.monotonic() < self._open_until

    def reset(self) -> None:
        with self._lock:
            self._failures = 0
            self._open_until = 0.0


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


def _build_tile_resources(
    cache_dir: str,
) -> tuple[dict[str, TileCache], "_TileProgressBus", Any, "_UpstreamBreaker"]:
    """Build the tile caches, progress bus, downloader pool, and fill breaker.

    Shared by ``create_server`` (browser mode) and ``corvus.app.start_backend``
    (desktop mode) so the two entry points never diverge on how tile resources
    are wired — the desktop app's offline maps depend on the exact same
    per-source caches + downloader the browser mode uses, and on the same
    breaker, without which the packaged app is the one that feels broken in the
    field. The caller picks the cache dir (operator-config override or the
    default); this helper only constructs the objects rooted at that dir.
    """
    tile_caches: dict[str, TileCache] = {
        sid: TileCache(os.path.join(cache_dir, f"{sid}.mbtiles"))
        for sid in tile_sources.TILE_SOURCES
    }
    tile_progress_bus = _TileProgressBus()
    tile_downloader = _build_tile_downloader(tile_caches)
    tile_breaker = _UpstreamBreaker()
    return tile_caches, tile_progress_bus, tile_downloader, tile_breaker


def _params_export_dir(cfg: Any) -> str:
    """Directory exported parameter files are written to.

    The operator's ``params_dir`` when set, otherwise ``~/.corvus/params`` —
    the same "empty string means the built-in default" convention the tile
    cache and tlog directories use.
    """
    configured = getattr(cfg, "params_dir", "") or ""
    if configured.strip():
        return os.path.expanduser(configured.strip())
    return os.path.expanduser("~/.corvus/params")


def _firmware_dir(cfg: Any) -> str:
    """Directory downloaded PX4 images and the cached catalogue live in.

    Same "empty string means the built-in default" convention as the tile
    cache, tlog and params directories.
    """
    configured = getattr(cfg, "firmware_dir", "") or ""
    if configured.strip():
        return os.path.expanduser(configured.strip())
    from .firmware_catalog import default_firmware_dir
    return default_firmware_dir()


def _log_download_dir(cfg: Any) -> str:
    """Where downloaded ULogs and exported tlogs are written.

    Same "empty string means the built-in default" convention as every other
    directory. Deliberately NOT ``tlog_dir``: that one is Corvus's own
    recording folder, and mixing what the app writes with what the operator
    pulled off the aircraft makes both harder to reason about.
    """
    configured = getattr(cfg, "log_download_dir", "") or ""
    if configured.strip():
        return os.path.expanduser(configured.strip())
    return os.path.expanduser("~/.corvus/flightlogs")


def _tlog_dir(cfg: Any) -> str:
    """Where Corvus records its own tlogs."""
    configured = getattr(cfg, "tlog_dir", "") or ""
    if configured.strip():
        return os.path.expanduser(configured.strip())
    return os.path.expanduser("~/.corvus/logs")


def _slugify(value: Any) -> str:
    """Lowercase, filename-safe slug of *value* ("" when there is nothing)."""
    text = re.sub(r"[^A-Za-z0-9]+", "-", str(value or "")).strip("-").lower()
    return text


def _default_params_filename(vehicle_tag: str = "") -> str:
    """Build a readable default name for an exported parameter file.

    ``corvus-params_<vehicle>_<YYYY-MM-DD_HH-MM>.json``. Local time and a
    vehicle tag rather than the old epoch-milliseconds name: a folder of
    exports has to be scannable by eye, and "which airframe was this?" is the
    first question asked of an old parameter file.
    """
    stamp = time.strftime("%Y-%m-%d_%H-%M")
    tag = _slugify(vehicle_tag)
    return f"corvus-params_{tag}_{stamp}.json" if tag else f"corvus-params_{stamp}.json"


def _safe_filename(raw: Any, fallback: str, suffix: str = ".json") -> str:
    """Reduce *raw* to a single safe filename, or return *fallback*.

    Strips any directory component (so ``../../etc/x`` cannot escape the target
    directory), rejects the empty/dot names, and forces *suffix* so the file is
    recognizable (and, for parameter files, re-importable). A non-string takes
    the fallback rather than being coerced — a client bug should produce the
    sensible default name, not a file called ``42.json``.
    """
    if not isinstance(raw, str):
        return fallback
    name = os.path.basename(raw.strip())
    name = re.sub(r'[\x00-\x1f<>:"/\\|?*]', "", name).strip()
    if not name or name in (".", ".."):
        return fallback
    if name.lower().endswith(suffix.lower()):
        name = name[: -len(suffix)]
    # Truncate the STEM, then re-attach the suffix — truncating afterwards
    # would chop the extension off a long name and leave an unrecognizable file.
    name = name[:115].rstrip() or fallback
    return name + suffix


# A region name is operator-typed free text that ends up in the UI and in the
# .mbtiles file. Keep it short and single-line; everything else about it is the
# operator's business.
_MAX_REGION_NAME = 60


def _clean_region_name(raw: Any) -> str:
    """Normalize an operator-supplied region name, or return "" if unusable.

    Collapses whitespace (a pasted multi-line name would break the list
    layout) and truncates. Non-strings become "", so the caller falls back to
    a generated name rather than rejecting the download.
    """
    if not isinstance(raw, str):
        return ""
    name = " ".join(raw.split())
    return name[:_MAX_REGION_NAME]


def _default_region_name(bounds: tuple[float, float, float, float]) -> str:
    """Generate a name for an unnamed download: its centre coordinates.

    Not a timestamp — in the field the operator recognizes an area by where it
    is, and a list of identical-looking timestamps is no better than no names
    at all. They can rename it afterwards.
    """
    w, s, e, n = bounds
    lat = (s + n) / 2.0
    lon = (w + e) / 2.0
    return f"{abs(lat):.3f}\u00b0{'N' if lat >= 0 else 'S'} {abs(lon):.3f}\u00b0{'E' if lon >= 0 else 'W'}"


def _delete_region_tiles(cache: Any, region: dict) -> int:
    """Delete the tiles of *region* that no other region still covers.

    Regions overlap by design (an operator pre-downloads a wide area at low
    zoom and a landing site at high zoom inside it). Deleting one region's
    full tile set would punch holes in the other, so every candidate tile is
    checked against the remaining regions first and kept if any still needs
    it. Returns the number of tiles actually removed.
    """
    from corvus.tile_downloader import enumerate_tiles

    def _tiles_of(r: dict) -> set:
        b = r.get("bounds") or {}
        return set(enumerate_tiles(
            (b.get("w", 0.0), b.get("s", 0.0), b.get("e", 0.0), b.get("n", 0.0)),
            int(r.get("minzoom", 0)), int(r.get("maxzoom", 0)),
        ))

    doomed = _tiles_of(region)
    for other in cache.list_regions():
        if other["id"] == region["id"]:
            continue
        doomed -= _tiles_of(other)
        if not doomed:
            break
    return cache.delete_tiles(doomed) if doomed else 0


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
    # Live operator config (loaded once at startup, mutated by the config
    # endpoints). The HTTP layer is the only writer; the MAVLink/SSH bridges
    # never touch it. None when the server is constructed without one (the
    # unit-test fixtures), in which case the config endpoints fall back to
    # fresh defaults on every read so they still answer.
    config: CorvusConfig | None = None
    config_path: str | None = None
    # Firmware-flash service (direct USB only). None when not wired (e.g. the
    # parallel-built flash_service module is mid-edit or the handler is used in
    # a unit test that only sets mavlink/store).
    flash: Any = None
    # Flight-log service (on-board ULog download + local tlog listing). None
    # when not wired, same as `flash`.
    logs: Any = None
    # Tile resources: per-source caches + a single downloader facade + the
    # progress pub-sub bus. None when tiles are not configured (e.g. the
    # parallel-built tile_downloader module is mid-edit).
    tile_caches: dict[str, TileCache] | None = None
    # Shared across handler threads: one breaker per process, not per request.
    tile_breaker: "_UpstreamBreaker | None" = None
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

    # ---- Live config helpers ----
    def _live_config(self) -> CorvusConfig:
        """Return the live CorvusConfig, loading from disk on first access.

        Handlers reach config through here so the unit-test fixtures that do
        not set ``self.config`` still get a fresh-defaults instance per call
        (no shared mutable state between tests).
        """
        if self.config is not None:
            return self.config
        path = self.config_path or default_config_path()
        cfg = load_config(path)
        self.config = cfg
        self.config_path = path
        return cfg

    def _save_live_config(self) -> None:
        """Persist the live config to its path; never raises into the handler."""
        cfg = self._live_config()
        try:
            save_config(cfg, self.config_path)
        except OSError as exc:
            # A failed write must not crash the HTTP handler; the operator's
            # in-memory state is still correct for this session.
            logger.error("config save to %s failed: %s", self.config_path, exc)

    def _public_config(self) -> dict[str, Any]:
        """Return the live config as a redacted dict (passwords stripped)."""
        try:
            return to_public_dict(self._live_config())
        except Exception as exc:  # noqa: BLE001 - GET /api/config must never 500
            logger.exception("public config build failed; returning defaults")
            return {}

    def _apply_config_partial(self, partial: dict[str, Any]) -> tuple[dict[str, Any] | None, str | None]:
        """Validate + merge a partial config update; return (public, error).

        Unknown keys are dropped with a warning log (never crash). Known keys
        are type-coerced via the same defensive coercion the load path uses,
        by round-tripping the merged dict back through ``_build_config``. The
        merged config is persisted atomically. Returns the new redacted
        public dict, or ``(None, error_message)`` on a validation failure.
        """
        cfg = self._live_config()
        merged: dict[str, Any] = {
            "mavlink_connection": cfg.mavlink_connection,
            "http_port": cfg.http_port,
            "tile_cache_dir": cfg.tile_cache_dir,
            "tlog_dir": cfg.tlog_dir,
            "params_dir": cfg.params_dir,
            "firmware_dir": cfg.firmware_dir,
            "log_download_dir": cfg.log_download_dir,
            "ssh_connections": [dict(e) for e in cfg.ssh_connections],
        }
        if cfg.tile_sources is not None:
            merged["tile_sources"] = cfg.tile_sources
        if cfg.stream_rates is not None:
            merged["stream_rates"] = cfg.stream_rates
        if cfg.theme is not None:
            merged["theme"] = cfg.theme
        if cfg.map is not None:
            merged["map"] = cfg.map
        if cfg.branding is not None:
            merged["branding"] = cfg.branding
        if cfg.controls is not None:
            merged["controls"] = cfg.controls
        if cfg.ui is not None:
            merged["ui"] = cfg.ui

        known = {
            "mavlink_connection", "http_port", "tile_cache_dir", "tlog_dir",
            "params_dir", "firmware_dir", "log_download_dir",
            "tile_sources", "stream_rates", "ssh_connections",
            "theme", "map", "branding", "controls", "ui",
        }
        for key, value in partial.items():
            if key not in known:
                logger.warning("dropping unknown config key %r in POST /api/config", key)
                continue
            if key == "ssh_connections":
                # Coerce each entry defensively before storing; a non-list or
                # bad entry is dropped, never crashes.
                if not isinstance(value, list):
                    return None, "ssh_connections must be a list"
                merged["ssh_connections"] = value
            elif key == "theme":
                if not isinstance(value, dict):
                    return None, "theme must be an object"
                merged["theme"] = value
            elif key == "map":
                if not isinstance(value, dict):
                    return None, "map must be an object"
                merged["map"] = value
            elif key == "branding":
                if not isinstance(value, dict):
                    return None, "branding must be an object"
                merged["branding"] = value
            elif key == "controls":
                if not isinstance(value, dict):
                    return None, "controls must be an object"
                # Merged per key, unlike theme/map which replace wholesale.
                # Each control is an independent switch and the UI toggles one
                # at a time, so a POST naming only "arrow_keys" must not turn
                # the joystick off as a side effect.
                base = merged.get("controls")
                merged["controls"] = {**base, **value} if isinstance(base, dict) else dict(value)
            elif key == "ui":
                if not isinstance(value, dict):
                    return None, "ui must be an object"
                merged["ui"] = value
            elif key == "mavlink_connection":
                if not isinstance(value, str) or not value:
                    return None, "mavlink_connection must be a non-empty string"
                merged["mavlink_connection"] = value
            elif key == "http_port":
                if isinstance(value, bool):
                    return None, "http_port must be an integer"
                try:
                    port = int(value)
                except (TypeError, ValueError):
                    return None, "http_port must be an integer"
                if not 0 <= port <= 65535:
                    return None, "http_port must be between 0 and 65535"
                merged["http_port"] = port
            elif key == "tile_cache_dir":
                if not isinstance(value, str):
                    return None, "tile_cache_dir must be a string"
                merged["tile_cache_dir"] = value
            elif key == "tlog_dir":
                if not isinstance(value, str):
                    return None, "tlog_dir must be a string"
                merged["tlog_dir"] = value
            elif key == "params_dir":
                if not isinstance(value, str):
                    return None, "params_dir must be a string"
                merged["params_dir"] = value
            elif key == "firmware_dir":
                if not isinstance(value, str):
                    return None, "firmware_dir must be a string"
                merged["firmware_dir"] = value
            elif key == "log_download_dir":
                if not isinstance(value, str):
                    return None, "log_download_dir must be a string"
                merged["log_download_dir"] = value
            elif key == "tile_sources":
                if not isinstance(value, dict):
                    return None, "tile_sources must be an object"
                merged["tile_sources"] = value
            elif key == "stream_rates":
                if not isinstance(value, dict):
                    return None, "stream_rates must be an object"
                merged["stream_rates"] = value

        # Re-build through the same coercion the load path uses, so a partial
        # update gets the exact same defensive treatment as a full file.
        from .config import _build_config
        new_cfg = _build_config(merged)
        # Mutate the live config object IN PLACE (not replace). The class
        # attribute (and every concurrent handler) shares this one object;
        # replacing it would set a per-request instance attribute that dies
        # with the handler, leaving the next request reading a stale value.
        cfg.mavlink_connection = new_cfg.mavlink_connection
        cfg.http_port = new_cfg.http_port
        cfg.tile_cache_dir = new_cfg.tile_cache_dir
        cfg.tlog_dir = new_cfg.tlog_dir
        cfg.params_dir = new_cfg.params_dir
        cfg.firmware_dir = new_cfg.firmware_dir
        cfg.log_download_dir = new_cfg.log_download_dir
        cfg.tile_sources = new_cfg.tile_sources
        cfg.stream_rates = new_cfg.stream_rates
        cfg.ssh_connections = new_cfg.ssh_connections
        cfg.theme = new_cfg.theme
        cfg.map = new_cfg.map
        cfg.branding = new_cfg.branding
        cfg.controls = new_cfg.controls
        cfg.ui = new_cfg.ui
        self._save_live_config()
        return to_public_dict(cfg), None

    def _ssh_connections_public(self) -> list[dict[str, Any]]:
        """Return the persisted ssh_connections with live ``connected`` merged.

        ``connected`` is computed from the live SshBridge sessions (matched
        by name) so the UI reflects what is actually open right now. The
        ``password`` key is stripped from every entry.
        """
        cfg = self._live_config()
        live: dict[str, bool] = {}
        if self.ssh is not None:
            for s in self.ssh.list_sessions():
                live[s.get("name", "")] = bool(s.get("connected", False))
        out: list[dict[str, Any]] = []
        for entry in cfg.ssh_connections:
            if not isinstance(entry, dict):
                continue
            clean = dict(entry)
            clean.pop("password", None)
            clean["connected"] = live.get(clean.get("name", ""), False)
            out.append(clean)
        return out

    def do_GET(self) -> None:
        path = urlparse(self.path).path
        if path.startswith("/api/"):
            self._handle_api_get(path)
        else:
            self._serve_static(path)

    def do_POST(self) -> None:
        path = urlparse(self.path).path
        # Firmware upload carries a raw octet-stream body, not JSON. Handle it
        # before the JSON dispatcher so the binary payload is never json-parsed.
        if path == "/api/firmware/upload":
            self._api_firmware_upload_raw()
            return
        # Same reason: the company logo arrives as raw PNG bytes.
        if path == "/api/branding/logo":
            self._api_branding_logo_upload_raw()
            return
        # Same reason: a ULog opened from outside the download folder arrives
        # as its own bytes.
        if path == "/api/logs/review/upload":
            self._api_logs_review_upload_raw()
            return
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

    @route("GET", "/api/config")
    def _api_config(self) -> None:
        """Return the live operator config with secrets redacted.

        Every SSH ``password`` is stripped from ``ssh_connections``; all other
        fields (including ``key_path``) are returned. Never crashes; on any
        error returns ``{"config": {}}`` so the UI can still render defaults.
        """
        self._send_json({"config": self._public_config()})

    @route("GET", "/api/ssh/connections")
    def _api_ssh_connections(self) -> None:
        """Return the persisted SSH connections with live ``connected`` status.

        Builds the list from ``config.ssh_connections`` and merges the live
        ``connected`` flag by matching names against ``ssh.list_sessions()``.
        No password is ever echoed back.
        """
        self._send_json({"connections": self._ssh_connections_public()})

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

    @route("GET", "/api/motors")
    def _api_motors(self) -> None:
        """Return the vehicle's motor configuration as a renderable description.

        Reads only the ~100 parameters the Motors page needs (a named batch
        read, not the full ~1300-parameter download the editor uses) and hands
        them to :mod:`corvus.motor_config`, which owns the PX4 schema. The
        response lists only the fields the connected firmware actually answered
        for, so a parameter absent on v1.16 is one field fewer rather than an
        error (AGENTS.md: graceful fallback across v1.16/v1.17/v1.18).

        Always 200 so the page can render a "not connected" state instead of an
        error banner; ``connected`` says which it is.
        """
        if self.mavlink is None:
            payload = motor_config.build({})
            payload["connected"] = False
            self._send_json(payload)
            return
        try:
            values = self.mavlink.fetch_params(motor_config.param_names())
        except Exception as exc:  # noqa: BLE001 - a read must never 500 the page
            logger.exception("motor parameter fetch failed")
            payload = motor_config.build({})
            payload["connected"] = False
            payload["error"] = str(exc)
            self._send_json(payload)
            return
        payload = motor_config.build(values)
        payload["connected"] = bool(values)
        if not values:
            payload["error"] = self.mavlink.get_last_command_error() or "no parameters received"
        self._send_json(payload)

    @route("GET", "/api/safety")
    def _api_safety(self) -> None:
        """Return the vehicle's safety and sensor configuration as a description.

        Same shape and same contract as ``/api/motors``: one batched read of the
        parameters :mod:`corvus.safety_config` knows about, handed to that module
        to turn into sections. Only what the connected firmware actually answered
        for comes back, so a parameter absent on v1.16 is one field fewer rather
        than an error (AGENTS.md: graceful fallback across v1.16/v1.17/v1.18).

        Always 200 so the page can render a "not connected" state instead of an
        error banner; ``connected`` says which it is.
        """
        if self.mavlink is None:
            self._send_json({"connected": False, "sections": [], "received": 0})
            return
        try:
            values = self.mavlink.fetch_params(safety_config.param_names())
        except Exception as exc:  # noqa: BLE001 - a read must never 500 the page
            logger.exception("safety parameter fetch failed")
            self._send_json({
                "connected": False, "sections": [], "received": 0, "error": str(exc),
            })
            return
        payload = safety_config.build(values)
        payload["connected"] = bool(values)
        if not values:
            payload["error"] = self.mavlink.get_last_command_error() or "no parameters received"
        self._send_json(payload)

    # ---- POST API ----
    def _handle_api_post(self, path: str) -> None:
        # Guard the Content-Length parse: a non-numeric header raises ValueError
        # uncaught (dropping the connection) if read here without the try/except.
        # Cap the body so a huge Content-Length cannot exhaust memory (DoS)
        # before json.loads ever runs.
        try:
            length = int(self.headers.get("Content-Length", 0))
        except (TypeError, ValueError):
            self._send_json({"error": "invalid content-length"}, 400)
            return
        if length < 0:
            self._send_json({"error": "invalid content-length"}, 400)
            return
        if length > MAX_JSON_BODY_BYTES:
            self._send_json({"error": "request too large"}, 413)
            return
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
        elif parts[0] == "shell" or parts[0] == "nsh":
            # Strip the prefix from the ORIGINAL cmd so the rest keeps its case
            # (NSH is case-sensitive, e.g. `listener sensor_combined`).
            shell_text = cmd.split(maxsplit=1)[1] if len(cmd.split(maxsplit=1)) > 1 else ""
            ok_shell = self.mavlink.send_shell_command(shell_text)
            if ok_shell:
                self._send_json({"ok": True, "shell": True})
            else:
                error = self.mavlink.get_last_command_error() or f"shell command failed: {cmd}"
                status = 503 if "DISCONNECTED" in error else 409
                self._send_json({"ok": False, "error": error}, status)
            return
        elif parts[0] in SHELL_COMMANDS:
            # Send the full original cmd (case-preserved) to the NSH shell.
            ok_shell = self.mavlink.send_shell_command(cmd)
            if ok_shell:
                self._send_json({"ok": True, "shell": True})
            else:
                error = self.mavlink.get_last_command_error() or f"shell command failed: {cmd}"
                status = 503 if "DISCONNECTED" in error else 409
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

    @route("POST", "/api/mavlink/connect")
    def _api_mavlink_connect(self, payload: dict) -> None:
        conn = payload.get("connection", "udp:127.0.0.1:14540")
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
        # Validate name/host types up front: a non-string (int/list/dict) would
        # crash the saved-entry lookup or the bridge call. Empty host is allowed
        # here — the connect-by-name path below fills it from a saved entry (or
        # 400s with "no host" when none matches), preserving the UI's
        # "connect by name" button.
        if not isinstance(name, str) or not name:
            self._send_json({"error": "name must be a non-empty string"}, 400)
            return
        if not isinstance(host, str):
            self._send_json({"error": "host must be a string"}, 400)
            return
        # Port coercion mirrors _api_ssh_connections_upsert: reject bool, then
        # try/except, then range-check. The unguarded int(payload...) crashed on
        # "abc"/list/dict/null.
        raw_port = payload.get("port", 22)
        if isinstance(raw_port, bool):
            self._send_json({"error": "port must be an integer"}, 400)
            return
        try:
            port = int(raw_port)
        except (TypeError, ValueError):
            self._send_json({"error": "port must be an integer"}, 400)
            return
        if not 0 <= port <= 65535:
            self._send_json({"error": "port must be between 0 and 65535"}, 400)
            return
        username = payload.get("username", "corvus")
        password = payload.get("password")
        key_path = payload.get("key_path")
        # Connect-by-name: when host is absent/empty BUT name matches a saved
        # connection, load the saved creds (host, port, username, key_path,
        # password) and connect with those. This lets the UI's card CONNECT
        # button connect by name without the UI holding the password.
        if not host:
            saved = None
            for entry in self._live_config().ssh_connections:
                if isinstance(entry, dict) and entry.get("name") == name:
                    saved = entry
                    break
            if saved is None:
                self._send_json({"error": "no host"}, 400)
                return
            host = saved.get("host", "")
            port = int(saved.get("port", 22))
            username = saved.get("username", username)
            # The UI never holds the password; only the saved entry does.
            if password is None:
                password = saved.get("password") or None
            if not key_path:
                key_path = saved.get("key_path") or None
            if not host:
                self._send_json({"error": "no host"}, 400)
                return
        ok = self.ssh.connect(name, host, port, username, password, key_path)
        self._send_json({"ok": ok, "connected": ok})

    @route("POST", "/api/config")
    def _api_config_update(self, payload: dict) -> None:
        """Apply a partial config update, persist atomically, return redacted.

        Accepts any subset of the known config keys. Unknown keys are dropped
        with a warning log (never crash). On a validation failure returns
        ``400 {"error": "..."}``. Never crashes on a bad payload — the
        ``do_POST`` dispatcher already rejected non-dict bodies.
        """
        public, error = self._apply_config_partial(payload)
        if error is not None:
            self._send_json({"error": error}, 400)
            return
        self._send_json({"ok": True, "config": public})

    # ---- Company logo (optional operator branding) ----
    def _branding_dir(self) -> Path:
        """Directory holding operator branding assets, beside the config file.

        Derived from ``config_path`` rather than hardcoded to ``~/.corvus`` so
        a test (or a second instance started with ``--config``) keeps its
        assets next to the config it is actually using.
        """
        base = Path(self.config_path or default_config_path()).parent
        return base / "branding"

    def _logo_path(self) -> Path:
        """Path of the stored company logo (always a PNG, one per install)."""
        return self._branding_dir() / "logo.png"

    @route("GET", "/api/branding/logo")
    def _api_branding_logo(self) -> None:
        """Serve the operator's company logo PNG, or 404 when none is set.

        ``no-store`` because the operator replaces this file in place: a
        cached copy would leave the old mark in the top bar until reload.
        """
        path = self._logo_path()
        try:
            blob = path.read_bytes()
        except OSError:
            self._send_json({"error": "no company logo"}, 404)
            return
        if not blob:
            self._send_json({"error": "no company logo"}, 404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "image/png")
        self.send_header("Content-Length", str(len(blob)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(blob)

    def _api_branding_logo_upload_raw(self) -> None:
        """Store a raw PNG body as the company logo and record its name.

        Bypasses the JSON dispatcher (the body is ``application/octet-stream``,
        same shape as the firmware upload). The body must actually be a PNG —
        the magic bytes are checked, so a renamed JPEG is refused here rather
        than rendering as a broken image in the top bar. The display name
        comes from the ``?name=`` query parameter and is stored in the config;
        the bytes are written atomically to ``branding/logo.png``.
        """
        try:
            length = int(self.headers.get("Content-Length", 0))
        except (TypeError, ValueError):
            self._send_json({"ok": False, "error": "invalid content-length"}, 400)
            return
        if length <= 0:
            self._send_json({"ok": False, "error": "empty logo body"}, 400)
            return
        if length > MAX_LOGO_BODY_BYTES:
            self._send_json({"ok": False, "error": "logo too large (max 4 MB)"}, 413)
            return
        raw = self.rfile.read(length)
        if not raw.startswith(PNG_MAGIC):
            self._send_json({"ok": False, "error": "logo must be a PNG image"}, 400)
            return

        query = parse_qs(urlparse(self.path).query)
        # Only the basename is kept: this string is display metadata, never a
        # path the server opens, and stripping directories (both separators,
        # so a Windows client's path is handled too) keeps it that way.
        raw_name = (query.get("name") or [""])[0].replace("\\", "/")
        name = os.path.basename(raw_name).strip()[:120] or "logo.png"

        path = self._logo_path()
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            fd, tmp_name = tempfile.mkstemp(prefix=path.name + ".", suffix=".tmp",
                                            dir=str(path.parent))
            try:
                with os.fdopen(fd, "wb") as f:
                    f.write(raw)
                os.replace(tmp_name, path)
            except OSError:
                try:
                    os.unlink(tmp_name)
                except OSError:
                    pass
                raise
        except OSError as exc:
            logger.error("company logo write to %s failed: %s", path, exc)
            self._send_json({"ok": False, "error": "could not store logo"}, 500)
            return

        public, error = self._apply_config_partial({"branding": {"logo": name}})
        if error is not None:
            self._send_json({"ok": False, "error": error}, 400)
            return
        self._send_json({"ok": True, "logo": name, "config": public})

    @route("POST", "/api/branding/logo/remove")
    def _api_branding_logo_remove(self, payload: dict) -> None:
        """Drop the company logo: delete the file and clear the config key.

        Idempotent — removing when none is set is a success, so a stale UI
        never reports an error for a state the operator already wanted.
        """
        try:
            self._logo_path().unlink()
        except FileNotFoundError:
            pass
        except OSError as exc:
            logger.error("company logo delete failed: %s", exc)
            self._send_json({"ok": False, "error": "could not remove logo"}, 500)
            return
        public, error = self._apply_config_partial({"branding": {}})
        if error is not None:
            self._send_json({"ok": False, "error": error}, 400)
            return
        self._send_json({"ok": True, "config": public})

    @route("POST", "/api/ssh/connections")
    def _api_ssh_connections_upsert(self, payload: dict) -> None:
        """Upsert a saved SSH connection by name; does NOT connect.

        Validates ``name`` (non-empty str), ``host`` (non-empty str),
        ``port`` (int, default 22), ``username`` (str). ``key_path`` and
        ``password`` are optional and may be ``""``. Replaces any existing
        entry with the same name; appends otherwise. Persists atomically.
        Returns the redacted connection list (no password).
        """
        name = payload.get("name")
        host = payload.get("host")
        if not isinstance(name, str) or not name:
            self._send_json({"error": "name must be a non-empty string"}, 400)
            return
        if not isinstance(host, str) or not host:
            self._send_json({"error": "host must be a non-empty string"}, 400)
            return
        raw_port = payload.get("port", 22)
        if isinstance(raw_port, bool):
            self._send_json({"error": "port must be an integer"}, 400)
            return
        try:
            port = int(raw_port)
        except (TypeError, ValueError):
            self._send_json({"error": "port must be an integer"}, 400)
            return
        if not 0 <= port <= 65535:
            self._send_json({"error": "port must be between 0 and 65535"}, 400)
            return
        username = payload.get("username", "")
        if not isinstance(username, str):
            self._send_json({"error": "username must be a string"}, 400)
            return
        key_path = payload.get("key_path", "")
        if not isinstance(key_path, str):
            key_path = ""
        password = payload.get("password", "")
        if not isinstance(password, str):
            password = ""

        entry = {
            "name": name,
            "host": host,
            "port": port,
            "username": username,
            "key_path": key_path,
            "password": password,
        }
        cfg = self._live_config()
        replaced = False
        for i, existing in enumerate(cfg.ssh_connections):
            if isinstance(existing, dict) and existing.get("name") == name:
                cfg.ssh_connections[i] = entry
                replaced = True
                break
        if not replaced:
            cfg.ssh_connections.append(entry)
        self._save_live_config()
        self._send_json({"ok": True, "connections": self._ssh_connections_public()})

    @route("POST", "/api/ssh/connections/remove")
    def _api_ssh_connections_remove(self, payload: dict) -> None:
        """Disconnect (if live) and remove a saved SSH connection by name.

        Order matters: stop the live subprocess FIRST (so its reader thread
        is joined before the entry vanishes), then drop the entry from the
        persisted list, then save atomically. Idempotent: a name that is
        neither live nor saved still returns ``{"ok": true}``.
        """
        name = payload.get("name")
        if not isinstance(name, str) or not name:
            self._send_json({"error": "name must be a non-empty string"}, 400)
            return
        if self.ssh is not None:
            try:
                self.ssh.disconnect(name)
            except Exception as exc:  # noqa: BLE001 - a bad live session must not block removal
                logger.warning("ssh disconnect for %r during remove failed: %s", name, exc)
        cfg = self._live_config()
        cfg.ssh_connections = [
            entry for entry in cfg.ssh_connections
            if isinstance(entry, dict) and entry.get("name") != name
        ]
        self._save_live_config()
        self._send_json({"ok": True, "connections": self._ssh_connections_public()})

    @route("POST", "/api/ssh/send")
    def _api_ssh_send(self, payload: dict) -> None:
        name = payload.get("name", "")
        data = payload.get("data", "")
        # data is written verbatim to the remote shell; a non-string (int/list/
        # dict) would crash channel.send or silently mis-send. Validate before
        # touching the bridge; keep reporting ok from the bridge unchanged.
        if not isinstance(data, str):
            self._send_json({"ok": False, "error": "data must be a string"}, 400)
            return
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

    @route("GET", "/api/params/export/target")
    def _api_params_export_target(self) -> None:
        """Report where a parameter export would be written, and a filename.

        The dialog prefills from this so the operator sees the real path before
        committing, rather than a file vanishing into a browser download folder
        they then have to hunt for. Always 200.
        """
        self._send_json({
            "dir": _params_export_dir(self._live_config()),
            "filename": _default_params_filename(self._vehicle_tag()),
        })

    def _vehicle_tag(self) -> str:
        """Short identifier for the connected vehicle, or "" when unknown.

        Used only to make an exported filename recognizable ("...px4-quad...").
        Never fails: no telemetry, no tag.
        """
        try:
            state = self.state_store.snapshot() if self.state_store is not None else {}
        except Exception:  # noqa: BLE001 - a filename hint must never raise
            return ""
        parts = [state.get("autopilot"), state.get("vehicle_type")]
        return "-".join(_slugify(p) for p in parts if p)

    @route("POST", "/api/params/export")
    def _api_params_export(self, payload: dict) -> None:
        """Write an exported parameter file to disk and return its full path.

        Saving server-side rather than as a browser download is deliberate:
        the desktop build runs the UI inside QtWebEngine, where an
        ``<a download>`` is dropped unless the host app implements a download
        handler — so the old export silently produced nothing there. Writing
        the file here works identically in the desktop app and in a browser,
        and can report exactly where it landed.

        ``dir`` and ``filename`` are optional; both default to what
        ``GET /api/params/export/target`` reports. The filename is sanitized to
        a single path component, so a caller cannot write outside *dir*.
        """
        params = payload.get("params")
        if not isinstance(params, list) or not params:
            self._send_json({"ok": False, "error": "params must be a non-empty list"}, 400)
            return

        raw_dir = payload.get("dir")
        target_dir = raw_dir if isinstance(raw_dir, str) and raw_dir.strip() else \
            _params_export_dir(self._live_config())
        target_dir = os.path.expanduser(target_dir.strip())

        filename = _safe_filename(
            payload.get("filename"), _default_params_filename(self._vehicle_tag()))

        doc = {
            "product": "Corvus GCS",
            "version": get_version(),
            "exported_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "vehicle": self._vehicle_tag(),
            "param_count": len(params),
            "params": params,
        }
        try:
            os.makedirs(target_dir, exist_ok=True)
            path = os.path.join(target_dir, filename)
            # Atomic: a half-written parameter file is worse than none, because
            # it looks importable. Same temp-then-replace shape as save_config.
            fd, tmp_name = tempfile.mkstemp(prefix=filename + ".", suffix=".tmp", dir=target_dir)
            try:
                with os.fdopen(fd, "w", encoding="utf-8") as f:
                    json.dump(doc, f, indent=2, ensure_ascii=False)
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
        except Exception as exc:  # noqa: BLE001 - an export must never 500
            logger.exception("parameter export failed")
            self._send_json({"ok": False, "error": f"export failed: {exc}"}, 500)
            return

        logger.info("exported %d parameters to %s", len(params), path)
        self._send_json({
            "ok": True, "path": path, "dir": target_dir,
            "filename": filename, "param_count": len(params),
        })

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

    @route("POST", "/api/params/upload")
    def _api_params_upload(self, payload: dict) -> None:
        """Apply a saved parameter file to the vehicle as a background upload.

        Validates the list up front, hands the clean list to the bridge's
        background uploader, and returns immediately. The writes happen on a
        worker thread; progress flows over ``/api/params/progress`` SSE (state
        ``"uploading"`` -> ``"upload_complete"``) and the final tally is read
        from ``/api/params/upload/result``. Keeping the HTTP thread short means
        N parameter writes never block a request (lean: fire-and-forget start,
        observe over SSE).
        """
        params = payload.get("params")
        if not isinstance(params, list) or not params:
            self._send_json({"ok": False, "error": "params must be a non-empty list"}, 400)
            return
        clean: list[dict[str, Any]] = []
        for index, entry in enumerate(params):
            if not isinstance(entry, dict):
                self._send_json(
                    {"ok": False, "error": f"invalid parameter at index {index}: must be an object"},
                    400,
                )
                return
            name = entry.get("name")
            if not isinstance(name, str) or not name:
                self._send_json(
                    {"ok": False, "error": f"invalid parameter at index {index}: name must be a non-empty string"},
                    400,
                )
                return
            value = entry.get("value")
            # bool is a subclass of int — reject it so True is never coerced to 1.0
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                self._send_json(
                    {"ok": False, "error": f"invalid parameter at index {index}: value must be a number"},
                    400,
                )
                return
            clean.append({"name": name, "value": float(value)})
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        ok = self.mavlink.start_param_upload(clean)
        if ok:
            self._send_json({"ok": True, "state": "uploading", "count": len(clean)})
        else:
            error = self.mavlink.get_last_command_error() or "parameter upload failed"
            status = 503 if "not connected" in error else 409
            self._send_json({"ok": False, "error": error}, status)

    @route("GET", "/api/params/upload/result")
    def _api_params_upload_result(self) -> None:
        """Return the final tally of the last background parameter upload.

        The upload runs on a worker thread; the UI watches
        ``/api/params/progress`` for ``"upload_complete"`` then reads the
        written/failed counts here. Returns the idle default when no bridge is
        attached so the UI can render before a vehicle is connected.
        """
        if self.mavlink is None:
            self._send_json({"state": "idle", "written": 0, "failed": 0, "errors": []})
            return
        result = self.mavlink.get_param_upload_result()
        self._send_json(result)

    @route("POST", "/api/motors/assign")
    def _api_motors_assign(self, payload: dict) -> None:
        """Wire one motor to one output pin (Setup -> Motors, click-to-assign).

        PX4 stores the mapping pin-first (``PWM_MAIN_FUNC3 = 103``), so moving
        *Motor 3* to a different pin is two writes, not one: clear the pin it is
        on, then claim the new one. Doing that here rather than in the browser
        keeps the pair together and lets the outcome be judged as a whole.

        Three cases for a target pin that is already taken:
        free (``Disabled``) is a plain move; another *motor* is a swap, so the
        displaced motor lands on the pin this one vacated; anything else — a
        servo, a gimbal, a parachute — is refused by name, because silently
        moving a servo off its pin is how a control surface stops working.

        Body: ``{motor: 1-16, bank: "MAIN"|"AUX"|"CAN", pin: n}`` to assign, or
        ``{motor: n, output: null}`` to unassign.
        """
        motor = payload.get("motor")
        if isinstance(motor, bool) or not isinstance(motor, int) or not (
                1 <= motor <= motor_config.MAX_MOTOR_FUNCTIONS):
            self._send_json({"ok": False, "error": "motor must be between 1 and 16"}, 400)
            return

        unassign = payload.get("output", False) is None
        target_param = None
        target_label = ""
        if not unassign:
            bank = payload.get("bank")
            pin = payload.get("pin")
            if not isinstance(bank, str) or isinstance(pin, bool) or not isinstance(pin, int):
                self._send_json({"ok": False, "error": "bank and pin are required"}, 400)
                return
            target_param = motor_config.function_param(bank, pin)
            if target_param is None:
                self._send_json({"ok": False, "error": f"unknown output {bank} {pin}"}, 400)
                return
            target_label = f"{bank} {pin}"

        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return

        values = self.mavlink.fetch_params(
            [e for e in motor_config.param_names() if "_FUNC" in e])
        if not values:
            error = self.mavlink.get_last_command_error() or "no output parameters received"
            self._send_json({"ok": False, "error": error}, 503)
            return
        if target_param is not None and target_param not in values:
            self._send_json(
                {"ok": False, "error": f"this board has no output {target_label}"}, 400)
            return

        entries = motor_config.outputs(values)
        source = next((e for e in entries if e["motor"] == motor), None)
        if target_param is not None and source is not None and source["param"] == target_param:
            self._send_json({"ok": True, "writes": [], "note": "already assigned"})
            return

        writes: list[tuple[str, float]] = []
        if target_param is None:
            if source is None:
                self._send_json({"ok": True, "writes": [], "note": "already unassigned"})
                return
            writes.append((source["param"], 0.0))
        else:
            occupant = next(e for e in entries if e["param"] == target_param)
            displaced = occupant["motor"]
            if displaced is None and int(round(occupant["value"])) != 0:
                self._send_json({
                    "ok": False,
                    "error": f"{target_label} drives {occupant['function']} — "
                             "free it in Parameters before assigning a motor to it",
                }, 409)
                return
            if displaced is not None and source is None:
                self._send_json({
                    "ok": False,
                    "error": f"{target_label} already drives Motor {displaced}, and "
                             f"Motor {motor} has no output to swap it onto",
                }, 409)
                return
            # Source first: a motor briefly on no pin is a safer transient than
            # one briefly on two.
            if source is not None:
                vacated = 0.0 if displaced is None else float(
                    motor_config.MOTOR_FUNCTION_BASE + displaced)
                writes.append((source["param"], vacated))
            writes.append((target_param, float(motor_config.MOTOR_FUNCTION_BASE + motor)))

        applied: list[dict[str, Any]] = []
        for name, value in writes:
            if not self.mavlink.set_param(name, value):
                error = self.mavlink.get_last_command_error() or "parameter write failed"
                status = 503 if "not connected" in error else 409
                self._send_json({
                    "ok": False, "error": error, "applied": applied, "failed": name,
                }, status)
                return
            applied.append({"param": name, "value": value})
        logger.info("assigned motor %d -> %s", motor, target_label or "unassigned")
        self._send_json({"ok": True, "writes": applied})

    @route("POST", "/api/motors/test")
    def _api_motors_test(self, payload: dict) -> None:
        """Spin one motor on the bench so the operator can identify it.

        PROPELLERS MUST BE OFF — the UI will not enable this until the operator
        confirms that, and the bridge refuses while armed and bounds the
        duration so the *vehicle* stops the motor even if the link dies.
        """
        motor = payload.get("motor")
        if isinstance(motor, bool) or not isinstance(motor, int):
            self._send_json({"ok": False, "error": "motor must be an integer"}, 400)
            return
        throttle = payload.get("throttle", 15)
        if isinstance(throttle, bool) or not isinstance(throttle, (int, float)):
            self._send_json({"ok": False, "error": "throttle must be a number"}, 400)
            return
        duration = payload.get("duration", 2)
        if isinstance(duration, bool) or not isinstance(duration, (int, float)):
            self._send_json({"ok": False, "error": "duration must be a number"}, 400)
            return
        if not 0 <= float(throttle) <= 100:
            self._send_json(
                {"ok": False, "error": "throttle must be between 0 and 100 percent"}, 400)
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if self.mavlink.motor_test(motor, float(throttle), float(duration)):
            self._send_json({"ok": True, "motor": motor, "throttle": float(throttle)})
            return
        error = self.mavlink.get_last_command_error() or "motor test failed"
        status = 503 if "not connected" in error else 409
        self._send_json({"ok": False, "error": error}, status)

    @route("POST", "/api/motors/test/stop")
    def _api_motors_test_stop(self, payload: dict) -> None:
        """Stop every running motor test.

        Never gated on the armed state: this only ever stops a motor, so
        refusing it would be a safety regression (mirrors /api/calibrate/cancel).
        """
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if self.mavlink.stop_motor_test():
            self._send_json({"ok": True})
            return
        error = self.mavlink.get_last_command_error() or "motor stop failed"
        self._send_json({"ok": False, "error": error}, 503)

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

    @route("POST", "/api/calibrate/cancel")
    def _api_calibrate_cancel(self, payload: dict) -> None:
        """Abort the calibration currently running on the vehicle."""
        del payload
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if self.mavlink.cancel_calibration():
            self._send_json({"ok": True})
            return
        error = self.mavlink.get_last_command_error() or "cancel failed"
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

    @route("POST", "/api/warnings/clear")
    def _api_warnings_clear(self, payload: dict) -> None:
        """Clear all warnings from the vehicle state store.

        Empties the remote warning list so every connected client's notification
        popover reflects an empty set on the next telemetry push. Responds
        ``{"ok": true}`` even when no store is configured so the frontend always
        succeeds (the no-store unit-test fixture case).
        """
        if self.store is not None:
            self.store.clear_warnings()
        self._send_json({"ok": True})

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
        """PX4 releases and their flashable boards, plus what is already cached.

        Served from the on-disk cache unless ``?refresh=1``, so opening the page
        in the field is instant and needs no network. A failed refresh returns
        200 with the cached list and a non-empty ``error`` — going blank because
        there is no internet is exactly the wrong answer for this app.
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
            self._send_json(data)
        except Exception:  # noqa: BLE001 - a catalogue failure must not 500
            logger.exception("firmware catalogue failed")
            self._send_json({"releases": [], "cached": [],
                             "error": "firmware catalogue failed", "dir": "",
                             "detected": None})

    def _detect_connected_board(self, releases: list) -> dict | None:
        """Which board is plugged in, matched against the release build targets.

        Matched against the newest stable release's target list because a build
        target keeps its name across releases, so the answer is valid whichever
        release the operator then picks. A suggestion only — it preselects, it
        does not decide.
        """
        if self.mavlink is None or not releases:
            return None
        try:
            from .firmware_catalog import detect_board
            stable = next((r for r in releases if not r.get("prerelease")), releases[0])
            return detect_board(self.mavlink.board_identity(), stable.get("boards") or [])
        except Exception:  # noqa: BLE001 - detection is a convenience, never a gate
            logger.debug("board detection failed", exc_info=True)
            return None

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

    def _api_firmware_upload_raw(self) -> None:
        """Receive a raw firmware binary and start a flash.

        Bypasses the JSON dispatcher (the body is ``application/octet-stream``).
        Returns ``{"ok": true, "state": "flashing"}`` (200) on accept, or
        ``{"ok": false, "error": ...}`` (400/409/503) on a gate refusal.
        """
        # Guard the Content-Length parse (a non-numeric header must 400, not
        # drop the connection) and cap the body so a huge Content-Length cannot
        # exhaust memory before the flash service ever sees it.
        try:
            length = int(self.headers.get("Content-Length", 0))
        except (TypeError, ValueError):
            self._send_json({"ok": False, "error": "invalid content-length"}, 400)
            return
        if length < 0:
            self._send_json({"ok": False, "error": "invalid content-length"}, 400)
            return
        if length > MAX_FIRMWARE_BODY_BYTES:
            self._send_json({"ok": False, "error": "request too large"}, 413)
            return
        # Refuse BEFORE draining the body: if the flash service or the MAVLink
        # bridge is not wired there is nothing to do with the bytes, so 503
        # immediately rather than reading MBs and then refusing.
        if self.flash is None or self.mavlink is None:
            self._send_json({"ok": False, "error": "flash service unavailable"}, 503)
            return
        if length <= 0:
            self._send_json({"ok": False, "error": "empty firmware body"}, 400)
            return
        raw = self.rfile.read(length)
        ok = self.flash.start(raw)
        if ok:
            self._send_json({"ok": True, "state": "flashing"})
        else:
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

    # ---- Flight logs (on-board ULogs + local tlogs) ----
    @route("GET", "/api/logs/status")
    def _api_logs_status(self) -> None:
        """Everything the Analysis page renders: logs, queue, progress, folder."""
        if self.logs is None:
            self._send_json({
                "state": "idle", "message": "", "percent": 0, "current": None,
                "queued": [], "completed": [], "logs": [], "tlogs": [],
                "dir": "", "connected": False,
            })
            return
        self._send_json(self.logs.status())

    @route("POST", "/api/logs/refresh")
    def _api_logs_refresh(self, payload: dict) -> None:
        """Ask the vehicle to enumerate its on-board logs."""
        del payload
        if self.logs is None:
            self._send_json({"ok": False, "error": "log service unavailable"}, 503)
            return
        if self.logs.refresh():
            self._send_json({"ok": True, "state": "listing"})
            return
        error = getattr(self.logs, "last_error", "") or "could not list logs"
        self._send_json({"ok": False, "error": error},
                        503 if "not connected" in error else 409)

    @route("POST", "/api/logs/download")
    def _api_logs_download(self, payload: dict) -> None:
        """Queue one or more on-board logs for sequential download."""
        raw = payload.get("ids")
        if not isinstance(raw, list) or not raw:
            self._send_json({"ok": False, "error": "ids must be a non-empty list"}, 400)
            return
        ids: list[int] = []
        for item in raw:
            if isinstance(item, bool) or not isinstance(item, (int, float)):
                self._send_json({"ok": False, "error": "ids must be numbers"}, 400)
                return
            ids.append(int(item))
        if self.logs is None:
            self._send_json({"ok": False, "error": "log service unavailable"}, 503)
            return
        if self.logs.start_download(ids):
            self._send_json({"ok": True, "state": "downloading", "queued": len(ids)})
            return
        error = getattr(self.logs, "last_error", "") or "download refused"
        self._send_json({"ok": False, "error": error},
                        503 if "not connected" in error else 409)

    @route("GET", "/api/logs/review")
    def _api_logs_review(self) -> None:
        """Flight Review for one ULog in the download folder.

        The file is named, never pathed: the name is resolved inside the
        download folder and the result checked with realpath, so no query can
        walk out of it.
        """
        params = parse_qs(urlparse(self.path).query)
        name = (params.get("file", [""])[0] or "").strip()
        if not name:
            self._send_json({"ok": False, "error": "file is required"}, 400)
            return
        if self.logs is None:
            self._send_json({"ok": False, "error": "log service unavailable"}, 503)
            return
        directory = os.path.realpath(self.logs.status().get("dir") or "")
        target = os.path.realpath(os.path.join(directory, os.path.basename(name)))
        if not directory or os.path.commonpath([directory, target]) != directory:
            self._send_json({"ok": False, "error": "unknown log file"}, 400)
            return
        if not target.endswith(".ulg") or not os.path.isfile(target):
            self._send_json({"ok": False, "error": "unknown log file"}, 404)
            return
        try:
            from .flight_review import review_file
            from .ulog import UlogError
            data = review_file(target, os.path.basename(target))
        except UlogError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        except (OSError, MemoryError) as exc:
            self._send_json({"ok": False, "error": f"could not read the log ({exc})"}, 400)
            return
        except Exception:  # noqa: BLE001 - a bad log must not 500 the app
            logger.exception("flight review failed for %s", target)
            self._send_json({"ok": False, "error": "could not analyse this log"}, 400)
            return
        # Spread, not mutated: the result may be a cached one that another
        # request is about to send.
        self._send_json({**data, "ok": True})

    def _api_logs_review_upload_raw(self) -> None:
        """Flight Review for a ULog the operator picked from anywhere on disk.

        The bytes arrive as the request body rather than the path arriving as a
        string, and that is the point: the operator chose the file in their own
        file dialog, and the backend never gains the ability to read an
        arbitrary path off this machine on request. The sibling GET route stays
        confined to the download folder for the same reason.

        Bypasses the JSON dispatcher (the body is ``application/octet-stream``).
        """
        try:
            length = int(self.headers.get("Content-Length", 0))
        except (TypeError, ValueError):
            self._send_json({"ok": False, "error": "invalid content-length"}, 400)
            return
        if length < 0:
            self._send_json({"ok": False, "error": "invalid content-length"}, 400)
            return
        if length > MAX_ULOG_BODY_BYTES:
            self._send_json({"ok": False, "error": "that log is too large to review"}, 413)
            return
        if length <= 0:
            self._send_json({"ok": False, "error": "empty log body"}, 400)
            return
        params = parse_qs(urlparse(self.path).query)
        # Label only — it names the review on screen and is never used to open
        # anything, so the basename is taken and the rest discarded.
        name = os.path.basename((params.get("name", [""])[0] or "").strip()) or "log.ulg"
        raw = self.rfile.read(length)
        if len(raw) != length:
            self._send_json({"ok": False, "error": "the upload was cut short"}, 400)
            return
        try:
            from .flight_review import review_bytes
            from .ulog import UlogError
            data = review_bytes(raw, name)
        except UlogError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        except MemoryError:
            self._send_json({"ok": False, "error": "not enough memory for that log"}, 400)
            return
        except Exception:  # noqa: BLE001 - a bad log must not 500 the app
            logger.exception("flight review failed for uploaded %s", name)
            self._send_json({"ok": False, "error": "could not analyse this log"}, 400)
            return
        self._send_json({**data, "ok": True})

    @route("POST", "/api/logs/erase")
    def _api_logs_erase(self, payload: dict) -> None:
        """Erase every on-board log.

        MAVLink has no per-log delete, so this is all-or-nothing by protocol.
        The confirm lives in the UI; the backend refuses while armed and while
        another log job owns the vehicle's single log session.
        """
        del payload
        if self.logs is None:
            self._send_json({"ok": False, "error": "log service unavailable"}, 503)
            return
        if self.logs.erase():
            self._send_json({"ok": True, "state": "erasing"})
            return
        error = getattr(self.logs, "last_error", "") or "erase refused"
        self._send_json({"ok": False, "error": error},
                        503 if "not connected" in error else 409)

    @route("POST", "/api/logs/cancel")
    def _api_logs_cancel(self, payload: dict) -> None:
        del payload
        if self.logs is None:
            self._send_json({"ok": False, "error": "log service unavailable"}, 503)
            return
        if self.logs.cancel():
            self._send_json({"ok": True})
        else:
            self._send_json({"ok": False, "error": "no log operation in progress"})

    @route("POST", "/api/logs/dir")
    def _api_logs_dir(self, payload: dict) -> None:
        """Set the download folder and persist it for future connections.

        Its own endpoint rather than a general config write because this is the
        one setting the Analysis page owns, and it has to be verified as
        writable before the operator queues an hour of downloads into it.
        """
        value = payload.get("dir")
        if not isinstance(value, str) or not value.strip():
            self._send_json({"ok": False, "error": "dir must be a non-empty string"}, 400)
            return
        path = os.path.expanduser(value.strip())
        try:
            os.makedirs(path, exist_ok=True)
            if not os.access(path, os.W_OK):
                raise OSError(errno.EACCES, "not writable")
        except OSError as exc:
            self._send_json({"ok": False,
                             "error": f"cannot use {path} ({exc.strerror or exc})"}, 400)
            return
        if self.config is not None:
            self.config.log_download_dir = path
            try:
                save_config(self.config, self.config_path or default_config_path())
            except Exception:  # noqa: BLE001 - the folder still works this session
                logger.exception("could not persist log_download_dir")
                self._send_json({"ok": True, "dir": path,
                                 "warning": "folder set for this session but not saved"})
                return
        self._send_json({"ok": True, "dir": path})

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

    @route("GET", "/api/firmware/progress")
    def _sse_firmware(self) -> None:
        """SSE stream of flash progress (every event delivered; low-rate).

        Uses an unbounded ``queue.Queue`` (not the coalescing bounded buffer) so
        every erase/program/verify step reaches the client — flashing is
        low-rate and each step matters to the operator.
        """
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        q: queue.Queue = queue.Queue()
        listener = q.put
        try:
            if self.flash is not None:
                self.flash.add_listener(listener)
                st = self.flash.status()
                self._send_sse("progress", json.dumps(_sanitize({
                    "state": st["state"],
                    "percent": st["progress"],
                    "message": st["message"],
                })))
            while True:
                try:
                    entry = q.get(timeout=15)
                    self._send_sse("progress", json.dumps(_sanitize({
                        "state": entry.get("state"),
                        "percent": entry.get("percent"),
                        "message": entry.get("message"),
                    })))
                except queue.Empty:
                    self._send_sse("ping", "{}")
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            if self.flash is not None:
                self.flash.remove_listener(listener)

    # ---- Tile cache / download / serve ----
    @route("GET", "/api/tiles/sources")
    def _api_tiles_sources(self) -> None:
        """List every source with its source cap and live cache stats; always 200.

        ``maxzoom`` is the SOURCE cap (the highest zoom the upstream serves,
        e.g. 19) — a constant capability, never the cache contents. ``minzoom``
        is the source floor (every registered source serves a single z0 world
        tile). The cache stats are reported under distinct ``cached_*`` keys so
        they can never clobber the source cap: the prior duplicate ``maxzoom``
        key returned the cache max (or null when empty), hiding the real 19
        cap from the UI and forcing a hardcoded ``SOURCE_ZOOM_CAP`` workaround.
        """
        caches = self.tile_caches or {}
        sources = []
        for entry in tile_sources.list_sources():
            sid = entry["id"]
            cache = caches.get(sid)
            stats = cache.stats() if cache is not None else {
                "count": 0, "minzoom": None, "maxzoom": None,
            }
            sources.append({
                "id": sid,
                "label": entry["label"],
                # Provider grouping so the UI can offer "which map service"
                # as one choice and then filter the layer/download pickers to
                # that service. Mirrors corvus/tile_sources.PROVIDERS.
                "provider": entry["provider"],
                "style": entry["style"],
                # The legally-required credit string. Served here so the
                # frontend can put it on its MapLibre raster source instead of
                # hand-mirroring the registry — that mirror was the one place
                # a new source could silently ship without attribution.
                "attribution": entry["attribution"],
                "minzoom": 0,
                "maxzoom": entry["maxzoom"],
                "cached_count": stats["count"],
                "cached_minzoom": stats["minzoom"],
                "cached_maxzoom": stats["maxzoom"],
            })
        self._send_json({
            "sources": sources,
            "providers": tile_sources.list_providers(),
            "default_provider": tile_sources.DEFAULT_PROVIDER,
        })

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
        # The operator's name for this area. Optional: an unnamed download is
        # still recorded, under a generated name, so every cached area is
        # accounted for in the region list rather than silently invisible.
        name = _clean_region_name(payload.get("name")) or _default_region_name(bounds)

        cache = (self.tile_caches or {}).get(source)
        on_progress = self._make_tile_progress(cache)
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
        # Record the region as soon as the job exists, keyed by the job id, so
        # the area shows on the map while it is still downloading. The progress
        # callback patches the final tile count and state when the job ends.
        if cache is not None:
            w, s, e, n = bounds
            try:
                cache.add_region({
                    "id": job_id, "name": name, "source": source,
                    "w": w, "s": s, "e": e, "n": n,
                    "minzoom": minzoom, "maxzoom": maxzoom,
                    "tile_count": 0, "state": "running",
                })
            except Exception:  # noqa: BLE001 - bookkeeping must not fail the download
                logger.exception("region record write failed")
        self._send_json({"job_id": job_id, "name": name})

    def _make_tile_progress(self, cache: Any) -> Any:
        """Compose the progress callback handed to the downloader.

        Two jobs on one callback because the downloader accepts exactly one:
        fan the update out to the SSE subscribers (the bus), and keep the
        region record in step with the job. The bus half runs first and is
        never skipped, so a bookkeeping failure cannot cost the operator their
        live progress. Returns None when there is nothing to do.
        """
        bus_fn = (self.tile_progress_bus.make_on_progress()
                  if self.tile_progress_bus is not None else None)
        if cache is None:
            return bus_fn

        # The terminal update must be applied once. The downloader fires
        # progress from several worker threads, so guard the transition with a
        # flag rather than relying on the last call winning.
        finished: dict[str, bool] = {"done": False}

        def _on_progress(progress: Any) -> None:
            if bus_fn is not None:
                bus_fn(progress)
            if not isinstance(progress, dict):
                return
            state = progress.get("state")
            if state not in ("done", "cancelled", "failed") or finished["done"]:
                return
            finished["done"] = True
            try:
                # `done` counts tiles now present in the cache — fetched plus
                # the ones that were already there — so it is exactly how much
                # of the region is available offline.
                cache.update_region(
                    progress.get("job_id", ""),
                    tile_count=progress.get("done", 0),
                    state=state,
                )
            except Exception:  # noqa: BLE001 - never kill the download
                logger.exception("region record update failed")

        return _on_progress

    @route("GET", "/api/tiles/regions")
    def _api_tiles_regions(self) -> None:
        """List every named, pre-downloaded area across all sources.

        Always 200: a source whose cache cannot be read contributes nothing
        rather than failing the whole list, because the map draws these and a
        500 here would blank every region the operator does have.
        """
        regions: list[dict] = []
        for sid, cache in (self.tile_caches or {}).items():
            try:
                for region in cache.list_regions():
                    region["source"] = region.get("source") or sid
                    regions.append(region)
            except Exception:  # noqa: BLE001
                logger.exception("region list failed for source %s", sid)
        regions.sort(key=lambda r: r.get("created_at", ""), reverse=True)
        self._send_json({"regions": regions})

    @route("POST", "/api/tiles/regions/rename")
    def _api_tiles_regions_rename(self, payload: dict) -> None:
        """Rename a stored region.

        Areas downloaded without a name get a coordinate-derived one, which is
        exact but not memorable; this is how "47.350°N 8.550°E" becomes
        "Landing site".
        """
        source = payload.get("source")
        region_id = payload.get("id")
        name = _clean_region_name(payload.get("name"))
        if not isinstance(region_id, str) or not region_id:
            self._send_json({"ok": False, "error": "id required"}, 400)
            return
        if not name:
            self._send_json({"ok": False, "error": "name required"}, 400)
            return
        cache = (self.tile_caches or {}).get(source) if isinstance(source, str) else None
        if cache is None:
            self._send_json({"ok": False, "error": "unknown source"}, 400)
            return
        if not cache.update_region(region_id, name=name):
            self._send_json({"ok": False, "error": "unknown region"}, 404)
            return
        self._send_json({"ok": True, "name": name})

    @route("POST", "/api/tiles/regions/remove")
    def _api_tiles_regions_remove(self, payload: dict) -> None:
        """Forget a named region, optionally deleting its cached tiles.

        ``delete_tiles`` defaults to False: regions overlap, so dropping the
        tiles of one can silently punch holes in another. The caller asks for
        it explicitly, and only the tiles no OTHER region still covers are
        removed.
        """
        source = payload.get("source")
        region_id = payload.get("id")
        if not isinstance(region_id, str) or not region_id:
            self._send_json({"ok": False, "error": "id required"}, 400)
            return
        cache = (self.tile_caches or {}).get(source) if isinstance(source, str) else None
        if cache is None:
            self._send_json({"ok": False, "error": "unknown source"}, 400)
            return
        region = cache.get_region(region_id)
        if region is None:
            self._send_json({"ok": False, "error": "unknown region"}, 404)
            return

        removed_tiles = 0
        if payload.get("delete_tiles") is True:
            try:
                removed_tiles = _delete_region_tiles(cache, region)
            except Exception as exc:  # noqa: BLE001 - deletion must never 500
                logger.exception("region tile deletion failed")
                self._send_json({"ok": False, "error": f"tile deletion failed: {exc}"}, 500)
                return
        cache.remove_region(region_id)
        self._send_json({"ok": True, "removed_tiles": removed_tiles})

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
        # Guard the cache read: a closed/corrupt cache must not 500 the tile
        # request — treat a raised get_tile like a miss and fall through to the
        # upstream fetch (or 404 offline), matching the existing put_tile guard.
        try:
            blob = cache.get_tile(z, x, y) if cache is not None else None
        except Exception:  # noqa: BLE001 - a read failure is a cache miss
            logger.debug(
                "tile cache read failed: %s/%d/%d/%d", source, z, x, y, exc_info=True,
            )
            blob = None
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
        # Sniff the image magic bytes for the Content-Type: satellite tiles are
        # JPEG, streets/topo/osm are PNG. A hardcoded image/png would mislabel
        # JPEG bytes; the correct type keeps caches/proxies and the offline
        # MBTiles round-trip honest.
        if blob[:8] == b"\x89PNG\r\n\x1a\n":
            ctype = "image/png"
        elif blob[:3] == b"\xff\xd8\xff":
            ctype = "image/jpeg"
        else:
            ctype = "image/png"
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(blob)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Cache-Control", "max-age=86400")
        self.end_headers()
        self.wfile.write(blob)

    def _fetch_upstream_tile(self, source: str, z: int, x: int, y: int) -> bytes | None:
        """Best-effort online cache-fill for a single tile; None on any failure.

        Guarded by the upstream breaker: once the network has failed a few
        times in a row this returns None immediately instead of spending the
        timeout, so an offline pan renders its cached tiles at full speed and
        simply leaves the rest blank.
        """
        src = tile_sources.get(source)
        if src is None:
            return None
        breaker = self.tile_breaker
        if breaker is not None and not breaker.allow():
            logger.debug("upstream breaker open; skipping fetch for %s/%d/%d/%d",
                         source, z, x, y)
            return None
        url = tile_sources.build_tile_url(src["upstream"], z, x, y)
        try:
            req = urllib.request.Request(
                url, headers={"User-Agent": f"CorvusGCS/{get_version()}"}
            )
            with urllib.request.urlopen(req, timeout=TILE_UPSTREAM_TIMEOUT_S) as resp:
                data = resp.read()
        except Exception:  # noqa: BLE001 - offline field use must never 500
            logger.debug("upstream tile fetch failed: %s", url, exc_info=True)
            if breaker is not None:
                breaker.record_failure()
            return None
        if not data:
            # A 200 with an empty body is as useless as a failure, and counting
            # it keeps a broken-but-reachable upstream from holding the breaker
            # closed forever.
            if breaker is not None:
                breaker.record_failure()
            return None
        if breaker is not None:
            breaker.record_success()
        return data

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
        # Stop the flash service before the HTTP loop so a running upload is
        # cancelled/joined before its (shared) MAVLink bridge is torn down.
        flash = getattr(self, "flash", None)
        if flash is not None:
            try:
                flash.shutdown()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("flash service shutdown failed")
        try:
            super().shutdown()
        except Exception:  # noqa: BLE001 - shutdown must not raise
            logger.exception("http server shutdown failed")
        try:
            from .flight_review import clear_cache
            clear_cache()
        except Exception:  # noqa: BLE001 - shutdown must not raise
            logger.exception("flight review cache clear failed")
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
    config_path: str | None = None,
) -> CorvusServer:
    """Create and wire up the full Corvus backend server."""
    store = VehicleStateStore()
    mavlink = MavlinkBridge(store, mavlink_conn)
    ssh = SshBridge()

    # Operator config: loaded once at startup; the /api/config endpoints
    # mutate the live object and persist it back through save_config. The
    # CLI args (port/mavlink_conn) still beat the file for the bind/conn.
    cfg_path = config_path or default_config_path()
    config = load_config(cfg_path)

    # Tile resources: one MBTiles cache per source under ~/.corvus/tiles/.
    # TileDownloader binds a single cache at construction, so per-source caches
    # imply per-source downloaders; _TileDownloaderPool presents them as one.
    # Built through the shared _build_tile_resources helper so the desktop app
    # (app.start_backend) and browser mode (create_server) never diverge.
    tile_caches, tile_progress_bus, tile_downloader, tile_breaker = \
        _build_tile_resources(default_cache_dir())

    CorvusHandler.store = store
    CorvusHandler.mavlink = mavlink
    CorvusHandler.ssh = ssh
    CorvusHandler.config = config
    CorvusHandler.config_path = cfg_path
    CorvusHandler.tile_caches = tile_caches
    CorvusHandler.tile_downloader = tile_downloader
    CorvusHandler.tile_progress_bus = tile_progress_bus
    CorvusHandler.tile_breaker = tile_breaker

    # Firmware-flash service (direct USB only). Imported lazily so the server
    # still builds if a parallel edit to flash_service is mid-flight.
    flash: Any = None
    try:
        from .firmware_catalog import FirmwareCatalog
        from .flash_service import FlashService
        flash = FlashService(mavlink, store, catalog=FirmwareCatalog(_firmware_dir(config)))
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("flash service unavailable; firmware endpoints disabled")
    CorvusHandler.flash = flash

    # Flight-log service: on-board ULog download over MAVLink plus the local
    # tlog listing. Lazily imported for the same reason as the flash service.
    logs: Any = None
    try:
        from .log_service import LogService
        logs = LogService(
            mavlink,
            log_dir=lambda: _log_download_dir(config),
            tlog_dir=lambda: _tlog_dir(config),
        )
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("log service unavailable; log endpoints disabled")
    CorvusHandler.logs = logs

    server = CorvusServer(("", port), CorvusHandler)
    server.mavlink = mavlink
    server.ssh = ssh
    server.store = store
    server.config = config
    server.config_path = cfg_path
    server.flash = flash
    server.logs = logs
    server.tile_caches = tile_caches
    server.tile_downloader = tile_downloader
    mavlink.start()
    return server
