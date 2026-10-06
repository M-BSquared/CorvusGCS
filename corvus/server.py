"""HTTP server with SSE telemetry, MAVLink API, SSH API, and static files.

A ``ThreadingHTTPServer`` subclass that serves the web UI from ``src/``
and exposes JSON/SSE API endpoints under ``/api/``.
"""
from __future__ import annotations

import dataclasses
import http.server
import json
import logging
import math
import mimetypes
import errno
import os
import queue
import re
import shlex
import socket
import socketserver
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any
from collections.abc import Callable
from email.utils import formatdate, parsedate_to_datetime
from urllib.parse import parse_qs, unquote, urlparse

from . import (
    ardupilot_battery, ardupilot_motors, ardupilot_mounting, ardupilot_rc,
    ardupilot_remote_id, ardupilot_safety, ardupilot_tuning, autopilot, battery,
    battery_config, dem_tiles, geocode, motor_config, mounting_config, param_files,
    param_metadata, rc_config, remote_id,
    remote_id_config,
    local_shell, net_probe, rtk, rtk_service,
    safety_config, sik_config, sik_service, tile_sessions, tile_sources, tuning_config,
    video, websocket,
)
from .config import (
    CorvusConfig,
    default_config_path,
    load_config,
    save_config,
    to_public_dict,
)
from .mavlink_forwarder import (
    DEFAULT_HOST as _FORWARD_DEFAULT_HOST,
    DEFAULT_LISTEN_HOST as _FORWARD_DEFAULT_LISTEN_HOST,
    DEFAULT_LISTEN_PORT as _FORWARD_DEFAULT_LISTEN_PORT,
    DEFAULT_PORT as _FORWARD_DEFAULT_PORT,
)
from .mavlink_bridge import (
    MavlinkBridge,
)
from .file_manager import open_folder, open_url
from . import plugin_config
from .plugin_registry import (
    bundled_plugins_dir,
    ensure_user_plugins_dir,
    find_dir as find_plugin_dir,
    is_valid_id as is_valid_plugin_id,
    public_list as public_plugin_list,
    resolve_asset as resolve_plugin_asset,
    user_plugins_dir,
)
from .sse_buffers import _BoundedSseBuffer, _MultiplexSseBuffer  # noqa: F401 - re-exported
from .http_input import (  # noqa: F401 - re-exported, tests import them from here
    _clean_region_name,
    _coerce_port,
    _default_params_filename,
    _default_region_name,
    _int_or,
    _parse_near,
    _parse_takeoff_altitude,
    _reject_json_constant,
    _safe_filename,
    _slugify,
    _validate_tile_bounds,
    _validate_tile_zooms,
)
from .http_paths import (  # noqa: F401 - re-exported, tests import them from here
    _default_settings_filename,
    _missions_dir,
    _params_export_dir,
    _settings_export_dir,
)
from .http_routes import CLIENT_GONE_ERRORS, _config_write_lock, _pending_routes, route
from .routes.console import ConsoleRoutes
from .routes.control import ControlRoutes
from .routes.firmware import FirmwareRoutes
from .routes.geofence import GeofenceRoutes
from .routes.logs import LogRoutes
from .routes.mission import MissionRoutes
from .routes.rtk import RtkRoutes
from .routes.settings import SettingsRoutes
from .routes.tiles import TileRoutes
from .routes.vehicle import VehicleRoutes
from .routes.video import VideoRoutes
from .ssh_bridge import SshBridge, run_command as ssh_run_command
from .tile_http import (  # noqa: F401 - re-exported, tests import them from here
    TILE_UPSTREAM_COOLDOWN_S,
    TILE_UPSTREAM_FAIL_THRESHOLD,
    _AbsentTiles,
    _TileDownloaderPool,
    _delete_region_tiles,
    _TileProgressBus,
    _UpstreamBreaker,
)
from .state_store import VehicleStateStore, _sanitize
from .paths import corvus_path
from .tile_cache import TileCache, default_cache_dir
from .version import get_version

logger = logging.getLogger("corvus.server")

REPO_ROOT = Path(__file__).resolve().parent.parent
WEB_DIR = REPO_ROOT / "src"
TELEMETRY_SSE_CAPACITY = 1
CONSOLE_SSE_CAPACITY = 100
PARAMS_SSE_CAPACITY = 16
SSH_SSE_CAPACITY = 256
# NSH output for a terminal, one SERIAL_CONTROL chunk (at most 70 bytes) per
# entry: about 35 KB, several screens of `top`.
SHELL_SSE_CAPACITY = 512
TILES_PROGRESS_SSE_CAPACITY = 16
FIRMWARE_SSE_CAPACITY = 16
# Interactive cache-fill timeout. Short on purpose: this is the browser waiting
# for a map tile, and a tile that takes longer than this has already missed the
# frame it was wanted for. The offline DOWNLOADER uses its own, longer timeout
# (corvus/tile_downloader._FETCH_TIMEOUT) because there nobody is watching.
TILE_UPSTREAM_TIMEOUT_S = 4
# Largest tile body accepted from an upstream. A 256px tile is tens of
# kilobytes; anything past this is a captive portal's login page, a proxy
# error page, or an upstream that has stopped being a tile server. Read one
# byte past the cap so a body that exceeds it can be recognised and dropped
# rather than cached and later served as a map tile.
TILE_UPSTREAM_MAX_BYTES = 4 * 1024 * 1024


# Body-size caps defend against a malformed/huge Content-Length: an unguarded
# int() crashes the handler on a non-numeric header, and an unbounded read
# can exhaust memory. General JSON API vs raw firmware binary (a few MB).
MAX_JSON_BODY_BYTES = 8 * 1024 * 1024
# A settings file can carry the company logo (4 MB) and the operator's plugins
# (4 MB each, see corvus/plugin_files.py), both as base64 inside the JSON.
MAX_SETTINGS_BODY_BYTES = 32 * 1024 * 1024
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

# Response hardening headers.
#
# The UI is a local web page with full control of an aircraft, and the only
# thing between an injected script and that aircraft is the browser's own
# policy engine — which was being told nothing at all. One injected <script>,
# out of a plugin asset or any value the page renders, could read
# GET /api/config, POST /api/mavlink/arm, and ship the result anywhere.
#
# ``connect-src 'self'`` is the line that matters most: nothing the page runs
# may talk to a host other than this server, so even a successful injection
# cannot get the telemetry, the SSH credentials or the map service keys off
# the machine. ``frame-ancestors 'none'`` stops another page embedding the UI
# and clickjacking the arm button, and ``base-uri``/``object-src``/
# ``form-action`` close the classic script-redirection routes.
#
# ``'unsafe-inline'`` is in script-src because index.html carries the pre-paint
# theme script and a plugin page may carry inline handlers. That is a real
# weakening, and it is why this policy is written in terms of *where data may
# go* rather than trusting "no inline script" to hold. ``blob:`` is in
# worker-src and img-src because MapLibre builds its tile worker from a blob
# URL and draws through canvas blobs — without it there is no map.
#
# Everything the app loads is served by this process (src/index.html names no
# CDN and no remote font; offline operation is a contract), so 'self' is the
# whole allow-list.
CONTENT_SECURITY_POLICY = "; ".join((
    "default-src 'self'",
    "script-src 'self' 'unsafe-inline'",
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data: blob:",
    "font-src 'self'",
    "connect-src 'self'",
    "worker-src 'self' blob:",
    "child-src 'self' blob:",
    "media-src 'self' blob:",
    "object-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
    "frame-ancestors 'none'",
))

# Sent with every response, whatever produced it, from the one ``end_headers``
# override below — so a route added later cannot forget them. ``nosniff`` is
# the one that is not merely defence in depth: a plugin asset or an uploaded
# file served under a guessed content type must not be re-interpreted as
# script by the browser.
SECURITY_HEADERS: tuple[tuple[str, str], ...] = (
    ("Content-Security-Policy", CONTENT_SECURITY_POLICY),
    ("X-Content-Type-Options", "nosniff"),
    ("X-Frame-Options", "DENY"),
    ("Referrer-Policy", "no-referrer"),
)
# The content type of every file this server hands out, fixed rather than
# asked of the host. ``mimetypes.guess_type`` reads the Windows registry, where
# an installed editor or IDE can have registered ``.js`` as ``text/plain``; with
# ``nosniff`` above, Chromium then refuses to run a single script and the app
# is a blank window, on that one machine only. The table covers what src/ and
# a plugin folder may serve (plugin_registry.ASSET_SUFFIXES); anything else
# falls back to Python's own built-in table, which the registry cannot reach.
_CONTENT_TYPES: dict[str, str] = {
    ".html": "text/html",
    ".js": "text/javascript",
    ".mjs": "text/javascript",
    ".css": "text/css",
    ".json": "application/json",
    ".map": "application/json",
    ".svg": "image/svg+xml",
    ".png": "image/png",
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".gif": "image/gif",
    ".webp": "image/webp",
    ".ico": "image/vnd.microsoft.icon",
    ".woff": "font/woff",
    ".woff2": "font/woff2",
    ".ttf": "font/ttf",
    ".otf": "font/otf",
    ".wasm": "application/wasm",
    ".txt": "text/plain",
    ".md": "text/markdown",
    ".csv": "text/csv",
}
# An instance, not the module functions: it starts from the built-in table and
# never takes in the registry or /etc/mime.types.
_BUILTIN_TYPES = mimetypes.MimeTypes()


def content_type(path: str | os.PathLike[str]) -> str:
    """The Content-Type for a file served from disk, the same on every host."""
    name = os.fspath(path)
    fixed = _CONTENT_TYPES.get(os.path.splitext(name)[1].lower())
    if fixed:
        return fixed
    return _BUILTIN_TYPES.guess_type(name)[0] or "application/octet-stream"


# The two constant building-cell bodies. `pending` tells the frontend the
# backend has queued the fetch and is worth asking again; its absence means
# this cell is simply empty and asking again would be a loop.
_EMPTY_BUILDINGS = b'{"type":"FeatureCollection","features":[]}'
_PENDING_BUILDINGS = b'{"type":"FeatureCollection","features":[],"pending":true}'
_UNAVAILABLE_BUILDINGS = b'{"type":"FeatureCollection","features":[],"unavailable":true}'
# Path-parameter tile route: /api/tiles/<source>/<z>/<x>/<y>.png
# Checked in _handle_api_get only when the path ends in ".png", so it can
# never shadow the exact /api/tiles/{sources,jobs,progress,download,cancel}
# routes (none of which end in ".png" with four numeric segments).
_TILE_PATH_RE = re.compile(r"^/api/tiles/([^/]+)/(\d+)/(\d+)/(\d+)\.png$")
# Path-parameter building route: /api/buildings/<z>/<x>/<y>.json — the OSM
# footprints 3D mode extrudes, addressed on the same slippy grid as the tiles
# so a pan over ground already visited is a cache hit. Same shadowing argument
# as the tile route: ".json" with three numeric segments cannot collide with
# any exact /api/... path.
_BUILDINGS_PATH_RE = re.compile(r"^/api/buildings/(\d+)/(\d+)/(\d+)\.json$")
# Path-parameter route for a plugin's own files:
# /api/plugins/asset/<plugin id>/<path inside the plugin folder>. The id is
# matched narrowly (the same shape plugin_registry accepts) so only the
# trailing group can carry separators, and that group is resolved against the
# plugin folder by resolve_plugin_asset rather than trusted here.
_PLUGIN_ASSET_RE = re.compile(r"^/api/plugins/asset/([A-Za-z0-9][A-Za-z0-9_.\-]{0,63})/(.+)$")
# "motor" = ESC/motor calibration (MAV_CMD_PREFLIGHT_CALIBRATION param7=1.0); motors spin — props must be removed
CALIB_SENSORS = frozenset({
    "gyro", "compass", "baro", "accel", "level", "accel_quick", "airspeed", "motor",
})
# PX4's multicopter autotune always tunes all three axes; the fixed-wing one
# takes its axis selection from FW_AT_AXES rather than from the command. So
# "all" is the only value that ever goes on the wire.
AUTOTUNE_AXES = frozenset({"all"})

# Cross-origin reads of the API.
#
# Every JSON body and every SSE stream used to go out with
# ``Access-Control-Allow-Origin: *``, which is a standing invitation: any page
# the operator happens to open in a browser could fetch
# http://localhost:PORT/api/state, subscribe to the telemetry stream, and read
# the SSH host, user and key path back out of GET /api/config. The UI never
# needed the header — it is served by this same server, so its own requests are
# same-origin and carry no Origin at all.
#
# What the header is still for is the case that is not the web at large:
# another tool on this machine — a plugin's dev server, or the UI opened at
# "localhost" while something else talks to "127.0.0.1", which the spec counts
# as two origins. Those are echoed back by name. Everything else gets no
# header unless the operator explicitly names one exact remote origin through
# CORVUS_ALLOWED_ORIGIN, and the browser refuses to hand the body to any other
# page that asked.
_CORS_LOOPBACK_HOSTS = frozenset({"localhost", "127.0.0.1", "::1"})
REMOTE_ORIGIN_ENV = "CORVUS_ALLOWED_ORIGIN"


def _cors_allowed_origin(origin: Any) -> str:
    """Return *origin* when it is a configured or loopback web origin.

    Never returns ``*``. This header is the only thing between a visited web
    page and a vehicle's live telemetry, so the answer is a name or nothing.
    """
    if not isinstance(origin, str) or not origin or len(origin) > 256:
        return ""
    try:
        parsed = urlparse(origin)
        host = parsed.hostname or ""
        # Accessing it IS the validation: urlsplit defers parsing the port
        # until it is read, and a bad one raises here rather than handing back
        # a clean hostname.
        parsed.port  # noqa: B018 - evaluated for the ValueError, not the value
    except ValueError:
        return ""
    if parsed.scheme not in ("http", "https"):
        return ""
    if parsed.username is not None or parsed.password is not None:
        return ""
    if parsed.path not in ("", "/") or parsed.params or parsed.query or parsed.fragment:
        return ""
    configured = (os.environ.get(REMOTE_ORIGIN_ENV) or "").strip()
    return origin if host in _CORS_LOOPBACK_HOSTS or origin == configured else ""


def _host_header_allowed(host_header: Any) -> bool:
    """Is the ``Host`` on this request one this server actually answers to?

    This is the defence the CSRF guard cannot provide. A page on
    ``http://evil.example`` whose DNS is re-pointed at 127.0.0.1 after it
    loads makes requests the *browser* considers same-origin: no ``Origin``
    header, ``Sec-Fetch-Site: same-origin``. Every check in
    ``_mutating_request_allowed`` passes, and the request reaches
    ``/api/mavlink/arm``. The one thing the attacker cannot forge is the
    ``Host``, because the browser fills it in from the name in the URL bar —
    and that name is never ``localhost``.

    Applied to reads as well as writes: ``GET /api/config`` hands back SSH
    hostnames, usernames and key paths.

    What is allowed:

    * the loopback names, which is what the UI is opened as;
    * the host of an explicitly configured ``CORVUS_ALLOWED_ORIGIN``;
    * anything at all when the operator has opted this server onto a wider
      interface with ``CORVUS_BIND``. That mode is already documented as
      having no authentication, the legitimate client then reaches it under
      some LAN name this process cannot enumerate, and refusing those would
      break a supported configuration to harden a case the operator has
      already accepted.

    A request with no ``Host`` at all is allowed: HTTP/1.0 clients and some
    tooling omit it, and a rebinding attack never can — it is a browser, and
    browsers always send one.
    """
    if (os.environ.get(BIND_HOST_ENV) or "").strip() not in ("", DEFAULT_BIND_HOST):
        return True
    if not isinstance(host_header, str) or not host_header.strip():
        return True
    if len(host_header) > 256:
        return False
    try:
        # Parsed as an authority so "[::1]:8000" and "127.0.0.1:8000" both
        # give up their hostname rather than being split by hand on ":".
        parsed = urlparse(f"//{host_header.strip()}")
        hostname = (parsed.hostname or "").strip()
        # Touched for its side effect, exactly as _cors_allowed_origin does:
        # the port is only validated when it is read, and "localhost:junk"
        # would otherwise hand back a clean "localhost".
        parsed.port  # noqa: B018 - evaluated for the ValueError, not the value
    except ValueError:
        return False
    if not hostname:
        return False
    if hostname in _CORS_LOOPBACK_HOSTS:
        return True
    configured = (os.environ.get(REMOTE_ORIGIN_ENV) or "").strip()
    if configured:
        try:
            if hostname == (urlparse(configured).hostname or "").strip():
                return True
        except ValueError:
            return False
    return False






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


def _build_building_service(cache_dir: str) -> Any:
    """Construct the OSM building-footprint service, or None.

    Deliberately NOT part of ``_build_tile_resources``' return tuple: that
    tuple is a contract two entry points and several tests unpack, and the
    buildings cache is an independent, optional overlay. None everywhere it
    cannot be built means 3D mode loses its buildings and keeps its terrain,
    which is the right failure for a rendering aid.
    """
    try:
        from .buildings import BuildingService, default_cache_path
    except ImportError:
        logger.warning("buildings module not available; 3D buildings disabled")
        return None
    try:
        return BuildingService(
            default_cache_path(cache_dir),
            user_agent=f"CorvusGCS/{get_version()}",
        )
    except Exception:  # noqa: BLE001 - an unwritable cache dir must not stop launch
        logger.exception("building service init failed")
        return None


def _build_geocoder() -> Any:
    """Construct the place-search service, or None.

    Shared by both entry points for the same reason ``_build_tile_resources``
    is: the desktop app and browser mode must offer the same map. Holds no
    thread, socket or file handle at rest, so nothing here has to be torn down
    on the shutdown path.
    """
    try:
        return geocode.Geocoder(user_agent=f"CorvusGCS/{get_version()}")
    except Exception:  # noqa: BLE001 - never block launch over a search box
        logger.exception("place search unavailable; /api/geocode disabled")
        return None


def _build_tile_resources(
    cache_dir: str,
) -> tuple[dict[str, TileCache], _TileProgressBus, Any, _UpstreamBreaker]:
    """Build the tile caches, progress bus, downloader pool, and fill breaker.

    Shared by ``create_server`` (browser mode) and ``corvus.app.start_backend``
    (desktop mode) so the two entry points never diverge on how tile resources
    are wired — the desktop app's offline maps depend on the exact same
    per-source caches + downloader the browser mode uses, and on the same
    breaker, without which the packaged app is the one that feels broken in the
    field. The caller picks the cache dir (operator-config override or the
    default); this helper only constructs the objects rooted at that dir.
    """
    # all_sources(), not TILE_SOURCES: the elevation tiles 3D mode reads
    # heights from are served and cached through exactly the same route, and a
    # DEM without a cache is a 3D map that flattens the moment the link drops.
    tile_caches: dict[str, TileCache] = {
        sid: TileCache(os.path.join(cache_dir, f"{sid}.mbtiles"))
        for sid in tile_sources.all_sources()
    }
    for sid, cache in tile_caches.items():
        try:
            _settle_interrupted_regions(cache)
        except Exception:  # noqa: BLE001 - bookkeeping must never stop launch
            logger.exception("could not settle interrupted regions for %s", sid)
    tile_progress_bus = _TileProgressBus()
    tile_downloader = _build_tile_downloader(tile_caches)
    tile_breaker = _UpstreamBreaker()
    return tile_caches, tile_progress_bus, tile_downloader, tile_breaker


def _settle_interrupted_regions(cache: TileCache) -> int:
    """Close out regions a previous run left in state ``running``.

    At startup no download job exists, so a ``running`` region is one whose
    process was killed or lost power mid-download before the final progress
    update could land. Left alone it shows "downloading" forever, with no job
    behind it to finish or cancel. It becomes ``cancelled`` with the tile count
    the cache actually holds for its area. Returns the number settled.
    """
    from corvus.tile_downloader import tile_ranges

    settled = 0
    for region in cache.list_regions():
        if region.get("state") != "running":
            continue
        b = region["bounds"]
        ranges = tile_ranges((b["w"], b["s"], b["e"], b["n"]),
                             int(region["minzoom"]), int(region["maxzoom"]))
        cache.update_region(region["id"], state="cancelled",
                            tile_count=cache.count_tiles(ranges))
        settled += 1
    return settled


def _tile_cache_dir(cfg: Any) -> str:
    """Directory the per-source ``.mbtiles`` caches live in.

    Same "empty string means the built-in default" convention as the params,
    tlog and firmware directories — and, like them, ``expanduser``'d: a
    ``~/tiles`` typed into the Settings page used to be taken literally, which
    created a directory actually named ``~`` next to wherever the app happened
    to be launched from, and left the operator looking for tiles that were
    downloaded somewhere they would never think to look.

    Shared by both entry points so browser mode and the desktop app resolve
    the operator's override identically; two modes reading different
    ``.mbtiles`` files means an area downloaded in one is missing in the other.
    """
    configured = getattr(cfg, "tile_cache_dir", "") or ""
    if configured.strip():
        return os.path.normpath(os.path.expanduser(configured.strip()))
    return default_cache_dir()










def _build_log_service(mavlink: Any, config: Any) -> Any:
    """Build the flight-log service, or None if it cannot be constructed.

    Shared by ``create_server`` (browser mode) and ``corvus.app.start_backend``
    (desktop mode). It lives here for the same reason ``_build_tile_resources``
    does, and it is not a hypothetical concern: the desktop app never built one
    at all, so every packaged build shipped an Analysis page whose ULog half
    reported "log service unavailable" and listed nothing — the feature was
    only ever reachable in browser mode.
    """
    try:
        from .log_service import LogService
        return LogService(
            mavlink,
            log_dir=lambda: _log_download_dir(config),
            tlog_dir=lambda: _tlog_dir(config),
        )
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("log service unavailable; log endpoints disabled")
        return None


def _build_flash_service(mavlink: Any, store: Any, config: Any) -> Any:
    """Build the firmware-flash service, or None if it cannot be constructed.

    Shared by ``create_server`` (browser mode) and ``corvus.app.start_backend``
    (desktop mode) for the same reason ``_build_log_service`` is, and after the
    same bug: the desktop app built a ``FlashService`` with no catalogue, so
    every packaged build shipped a Firmware page that listed no PX4 release and
    no board, and answered "firmware catalogue unavailable" to any flash — the
    release flow was only ever reachable in browser mode.

    Imported lazily so the server still builds while ``flash_service`` or
    ``firmware_catalog`` is mid-edit.
    """
    try:
        from .firmware_catalog import FirmwareCatalog
        from .flash_service import FlashService
        return FlashService(mavlink, store, catalog=FirmwareCatalog(_firmware_dir(config)))
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("flash service unavailable; firmware endpoints disabled")
        return None


def _build_sik_service(mavlink: Any, store: Any) -> Any:
    """Build the SiK telemetry-radio service, or None.

    Borrows the bridge's serial port for the length of an AT session, so it
    holds the same two handles the flash service does and is torn down on the
    same path — which is also why both launchers must build it the same way.
    """
    try:
        return sik_service.SikService(mavlink, store)
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("SiK radio service unavailable; radio endpoints disabled")
        return None


def _build_rtk_service(mavlink: Any, store: Any, config: Any) -> Any:
    """Build the RTK base-station service, or None.

    Built and *started* here, unlike the other services, because RTK is the
    one whose default is to act: a base plugged in before the ground station
    is opened has to be found without anybody asking, which means the search
    has to be running before anybody could have asked. Everything that keeps
    that safe is in :mod:`corvus.rtk_service` — it opens only a port whose USB
    descriptor says GNSS receiver, and never the one the MAVLink bridge is on.

    Disabled in the config file means constructed and not started, so the page
    can still be opened and the switch turned back on without a restart.
    """
    try:
        settings = rtk.settings(getattr(config, "rtk", None))
        service = rtk_service.RtkService(mavlink, store, settings)
        if settings.get("enabled"):
            service.start()
        return service
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("RTK service unavailable; RTK endpoints disabled")
        return None


def _build_video_service(config: Any) -> Any:
    """Build the camera video service, or None.

    Constructed only: no ffmpeg runs until a camera window asks for a frame,
    and none keeps running once no window does (see :mod:`corvus.video`).
    """
    try:
        return video.VideoService(getattr(config, "video", None))
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("video service unavailable; camera endpoints disabled")
        return None


def _build_local_runner() -> Any:
    """Build the runner for programs started on this computer, or None.

    Constructed only: nothing runs until a launcher button asks, and whatever
    it starts is stopped at shutdown (see :mod:`corvus.local_shell`).
    """
    try:
        return local_shell.LocalRunner()
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("local runner unavailable; local launcher buttons disabled")
        return None


def _build_pinger() -> Any:
    """Build the one-shot pinger for the companion-computer indicator, or None."""
    try:
        return net_probe.Pinger()
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("pinger unavailable; the companion indicator cannot probe")
        return None


def _build_update_checker() -> Any:
    """Build the release-update checker, or None.

    Constructed only — it touches the network the first time the frontend
    asks, never at startup. Shared so the packaged app prompts for updates on
    exactly the terms browser mode does.
    """
    try:
        from .update_check import UpdateChecker
        return UpdateChecker()
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("update checker unavailable; update endpoints disabled")
        return None


def _build_forwarder(mavlink: Any, config: Any) -> Any:
    """Build and start the MAVLink forwarder when the operator enabled it.

    Returns None when it is off (the default). A forwarder that could not bind
    is returned anyway, stopped, so the Settings page can say why instead of
    showing a switch that silently does nothing — but either way the app comes
    up with telemetry working, because a busy UDP port must never cost the
    operator the link to the aircraft.
    """
    cfg = getattr(config, "forwarding", None)
    if not isinstance(cfg, dict) or not cfg.get("enabled"):
        return None
    try:
        from .mavlink_forwarder import (
            DEFAULT_HOST, DEFAULT_LISTEN_HOST, DEFAULT_LISTEN_PORT, DEFAULT_PORT,
            MavlinkForwarder,
        )
        listen_port = cfg.get("listen_port")
        forwarder = MavlinkForwarder(
            mavlink.inject_raw,
            host=str(cfg.get("host") or DEFAULT_HOST),
            port=int(cfg.get("port") or DEFAULT_PORT),
            listen_host=str(cfg.get("listen_host") or DEFAULT_LISTEN_HOST),
            # 0 means "any free port" and must survive: `or DEFAULT` would
            # turn the operator's explicit ephemeral choice back into 14551.
            listen_port=(
                DEFAULT_LISTEN_PORT if not isinstance(listen_port, int)
                or isinstance(listen_port, bool) else listen_port
            ),
            endpoints=cfg.get("endpoints") or (),
            allow_commands=bool(cfg.get("allow_commands")),
            link_address=getattr(mavlink, "local_udp_address", None),
        )
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("MAVLink forwarder unavailable")
        return None
    if forwarder.start():
        mavlink.set_frame_sink(forwarder.feed)
    return forwarder


def build_autoconnect_session(config: Any) -> Any:
    """The per-process auto-connect state, seeded from the operator config.

    Public because both launchers build one and neither may end up with
    different toggles than the other. Never raises: auto-connect failing to
    read its own settings must degrade to "off", not to "no ground station".
    """
    from .autoconnect import SessionState
    session = SessionState()
    try:
        from .config import autoconnect_settings
        session.apply_config(autoconnect_settings(config))
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("autoconnect: settings unreadable; running with defaults")
    return session


def start_autoconnect_watcher(mavlink: Any, store: Any, session: Any) -> Any:
    """Start the auto-connect watcher, or return None when it is off.

    Returns None rather than a stopped object when disabled: unlike the
    forwarder there is no settings page that has to explain a switch which did
    nothing, and a None is one fewer thing for the teardown path to reason
    about.
    """
    if session is None or not session.enabled:
        return None
    try:
        from .autoconnect import AutoConnectWatcher
        watcher = AutoConnectWatcher(bridge=mavlink, store=store, session=session)
        watcher.start()
    except Exception:  # noqa: BLE001 - never block server creation
        logger.exception("autoconnect: watcher unavailable")
        return None
    return watcher


def stop_autoconnect_watcher(server: Any) -> None:
    """Join the auto-connect watcher. Never raises; safe on a server without one.

    Called from both launchers' teardown BEFORE ``mavlink.stop()``: the watcher
    calls ``stop/set_connection/start`` on the bridge, so a tick that lands
    after the bridge went down would restart the link the shutdown just closed.
    """
    watcher = getattr(server, "autoconnect", None)
    if watcher is None:
        return
    try:
        logger.info("stopping autoconnect watcher …")
        watcher.stop()
        logger.info("autoconnect watcher stopped")
    except Exception:  # noqa: BLE001 - teardown never raises
        logger.exception("autoconnect watcher stop failed")


def stop_backend(server: Any, log: Any = None) -> None:
    """Ordered, exception-safe backend teardown. Never raises.

    THE shutdown sequence, for every launcher. It used to be a 98-line
    function duplicated character-for-character in ``serve.py`` and
    ``corvus/app.py`` — same nine steps, same comments, differing only in the
    docstring, with app.py's saying "Mirrors serve.py". Nothing enforced the
    mirror, and the order is load-bearing in ways that are invisible from a
    diff: the auto-connect watcher calls stop/set_connection/start on the
    bridge, so a tick that lands after ``mavlink.stop()`` starts the link the
    shutdown just closed.

    Order: the services that borrow the MAVLink bridge (flash, SiK, RTK, logs,
    forwarder) first, so none of them is mid-operation when the bridge goes;
    then the auto-connect watcher, for the reason above; then the bridge
    itself (joins its threads, flushes the tlog); then SSH (joins reader
    threads); then the camera decoders (ffmpeg processes); then the state store; then the HTTP server and the tile caches,
    whose SQLite handles close last so no in-flight handler touches a closed
    database. Every step is guarded on its own, so a failure in one cannot
    skip the rest.

    *log* is the caller's logger, so a teardown still reports itself under
    the launcher the operator actually started — ``corvus.serve`` or
    ``corvus.app`` — rather than under this module.
    """
    if log is None:
        log = logger
    # Flash uses the MAVLink bridge (it stops/starts it), so cancel/join the
    # uploader BEFORE tearing the bridge down.
    flash = getattr(server, "flash", None)
    if flash is not None:
        try:
            log.info("stopping flash …")
            flash.shutdown()
            log.info("flash stopped")
        except Exception:
            log.exception("flash shutdown failed")
    # The radio service may be holding the bridge's serial port and will
    # restart the bridge when its session ends, so it is told to stop doing that
    # in the same breath as flash and for the same reason.
    sik = getattr(server, "sik", None)
    if sik is not None:
        try:
            sik.shutdown()
        except Exception:
            log.exception("SiK radio shutdown failed")
    # The RTK service holds a serial port (or a caster socket) and a thread,
    # and its teardown *writes* to that port to take the receiver back out of
    # base mode. So it goes before the bridge, like the rest of them, and it
    # goes while its port is still open — see RtkService.stop().
    rtk_svc = getattr(server, "rtk", None)
    if rtk_svc is not None:
        try:
            log.info("stopping RTK …")
            rtk_svc.shutdown()
            log.info("RTK stopped")
        except Exception:
            log.exception("RTK shutdown failed")
    # Log downloads hold a sink on the MAVLink bridge and a worker thread, so
    # they are stopped alongside flash — before the bridge itself goes away.
    logs = getattr(server, "logs", None)
    if logs is not None:
        try:
            log.info("stopping log service …")
            logs.shutdown()
            log.info("log service stopped")
        except Exception:
            log.exception("log service shutdown failed")
    # The forwarder holds a UDP socket, two daemon threads, and a sink on the
    # bridge's receive path, so it is released before the bridge goes away.
    forwarder = getattr(server, "forwarder", None)
    if forwarder is not None:
        try:
            log.info("stopping mavlink forwarding …")
            mav = getattr(server, "mavlink", None)
            if mav is not None:
                mav.set_frame_sink(None)
            forwarder.stop()
            log.info("mavlink forwarding stopped")
        except Exception:
            log.exception("mavlink forwarder shutdown failed")
    # The auto-connect watcher calls stop/set_connection/start on the bridge,
    # so it is joined before the bridge goes away: a tick landing after
    # mavlink.stop() would start the link the shutdown just closed.
    stop_autoconnect_watcher(server)
    mavlink = getattr(server, "mavlink", None)
    if mavlink is not None:
        try:
            log.info("stopping mavlink …")
            mavlink.stop()
            log.info("mavlink stopped")
        except Exception:
            log.exception("mavlink stop failed")
    ssh = getattr(server, "ssh", None)
    if ssh is not None:
        try:
            log.info("stopping ssh …")
            ssh.shutdown()
            log.info("ssh stopped")
        except Exception:
            log.exception("ssh shutdown failed")
    # Programs a launcher button started on this computer: Corvus' own
    # children, stopped with it.
    local_runner = getattr(server, "local", None)
    if local_runner is not None:
        try:
            log.info("stopping local programs …")
            local_runner.shutdown()
            log.info("local programs stopped")
        except Exception:
            log.exception("local programs shutdown failed")
    pinger = getattr(server, "pinger", None)
    if pinger is not None:
        try:
            pinger.shutdown()
        except Exception:
            log.exception("pinger shutdown failed")
    # One ffmpeg per camera being watched, each with two reader threads.
    video_svc = getattr(server, "video", None)
    if video_svc is not None:
        try:
            log.info("stopping video …")
            video_svc.shutdown()
            log.info("video stopped")
        except Exception:
            log.exception("video shutdown failed")
    store = getattr(server, "store", None)
    if store is not None:
        try:
            log.info("stopping state store …")
            store.shutdown()
            log.info("state store stopped")
        except Exception:
            log.exception("state store shutdown failed")
    try:
        log.info("stopping http server + tiles …")
        server.shutdown()
        log.info("http server + tiles stopped")
    except Exception:
        log.exception("http server shutdown failed")
    # Defense-in-depth: shutdown() stops the serve loop but does not close
    # the listening TCP socket. os._exit reclaims it in the live path, but an
    # explicit close keeps the fd table clean on a graceful exit and lets the
    # hermetic shutdown tests assert fileno==-1 without calling server_close
    # themselves.
    try:
        log.info("closing http socket …")
        server.server_close()
        log.info("http socket closed")
    except Exception:
        log.exception("http socket close failed")


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
    return corvus_path("flightlogs")


def _tlog_dir(cfg: Any) -> str:
    """Where Corvus records its own tlogs."""
    configured = getattr(cfg, "tlog_dir", "") or ""
    if configured.strip():
        return os.path.expanduser(configured.strip())
    return corvus_path("logs")


def _compose_remote_command(directory: str, command: str, detach: bool) -> str:
    """Build the shell line a one-button launcher sends to the remote host.

    ``cd -- '<dir>' && <command>``, with the directory quoted for the remote
    shell (``shlex.quote`` produces POSIX quoting, which is what the ``sh`` on
    the far end reads — the local platform does not enter into it). ``--`` so a
    folder whose name begins with a dash is a folder, not an option.

    *command* is passed through as written. It is a command line the operator
    typed for their own machine, so quoting it would break every pipeline and
    argument in it; the SSH account's own permissions are the boundary here,
    exactly as they are in the terminal on the SSH tab.

    With *detach*, the program is started under ``nohup`` in the background and
    its pid echoed, so it outlives both the SSH connection and this request.
    Its output goes nowhere unless the command redirects it itself — a launcher
    starts a long-running program, and holding a request open to collect its
    log is not what the button is for.
    """
    if detach:
        body = f"{{ nohup {command} >/dev/null 2>&1 & }}; echo $!"
    else:
        body = command
    if directory:
        return f"cd -- {shlex.quote(directory)} && {body}"
    return body


def _param_metadata_cache_dir() -> str:
    """Where the opt-in copies of PX4's parameter metadata are kept."""
    return corvus_path("param-metadata")




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


# The route handlers of each area live in corvus/routes/; their @route
# entries land in the same registry, wired onto this class below.
class CorvusHandler(
    ConsoleRoutes,
    ControlRoutes,
    FirmwareRoutes,
    GeofenceRoutes,
    LogRoutes,
    MissionRoutes,
    RtkRoutes,
    SettingsRoutes,
    TileRoutes,
    VehicleRoutes,
    VideoRoutes,
    http.server.BaseHTTPRequestHandler,
):
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
    # RTK base station (corvus/rtk_service.py). None when the service could
    # not be built; the endpoints then report that rather than 500.
    rtk: Any = None
    # Camera video (corvus/video.py). None when it could not be built; the
    # endpoints then say video is unavailable rather than 500.
    video: Any = None
    # Programs a launcher button starts on this computer without a window
    # (corvus/local_shell.py). The ones in a terminal are SSH-bridge sessions.
    local: Any = None
    # One-shot pings from this computer (corvus/net_probe.py), for the
    # companion-computer indicator.
    pinger: Any = None

    # SiK telemetry-radio configuration (corvus/sik_service.py). None when not
    # wired, same as `flash`; the endpoints then report the service as absent
    # rather than 500.
    sik: Any = None
    # Flight-log service (on-board ULog download + local tlog listing). None
    # when not wired, same as `flash`.
    logs: Any = None
    # Second-station MAVLink forwarding (QGroundControl alongside Corvus).
    # None when off, which is the default.
    forwarder: Any = None
    # GitHub release update check. None when not wired; the endpoints then
    # report "no check available" rather than 500.
    updates: Any = None
    # Auto-connect session state (corvus/autoconnect.py). None in the unit-test
    # fixtures, where a connect must still work and simply records no override.
    autoconnect_session: Any = None
    # Tile resources: per-source caches + a single downloader facade + the
    # progress pub-sub bus. None when tiles are not configured (e.g. the
    # parallel-built tile_downloader module is mid-edit).
    tile_caches: dict[str, TileCache] | None = None
    # Shared across handler threads: one breaker per process, not per request.
    tile_breaker: _UpstreamBreaker | None = None
    # Elevation tiles derived from a coarser cached one when the real tile is
    # neither on disk nor reachable (corvus/dem_tiles.py). Memory only, bounded,
    # nothing to tear down.
    dem_fallback: dem_tiles.DemFallback | None = dem_tiles.DemFallback()
    # Regional elevation tiles known not to exist. Memory only, bounded.
    tile_absent: _AbsentTiles | None = _AbsentTiles()
    tile_downloader: Any = None
    tile_progress_bus: _TileProgressBus | None = None
    # OSM building footprints for 3D mode. None when the module could not be
    # built; the route then answers an empty FeatureCollection.
    buildings: Any = None
    # Place search over Nominatim (corvus/geocode.py). None when it could not
    # be built; /api/geocode then says so and the map's search box falls back
    # to the coordinate parsing it does itself.
    geocoder: Any = None
    # Plugin roots. None means "the real ones" (~/.corvus/plugins and the
    # bundled <repo>/plugins); the tests point them at a tmp_path so a scan
    # never reads the developer's own plugin folder.
    plugin_user_dir: str | None = None
    plugin_bundled_dir: str | None = None
    # Whether the first start setup is still to be done, as {"pending": bool}.
    # A dict so a route can clear it for every handler; create_server sets it
    # from whether a config file existed when the process started. None (the
    # tests, and anything that never went through create_server) means done.
    first_start: dict[str, bool] | None = None

    _GET_ROUTES: dict[str, str] = {}
    _POST_ROUTES: dict[str, str] = {}

    # Socket timeout for this connection, in seconds.
    #
    # Without one, every blocking read and write on the client socket waits
    # forever. A browser tab killed between its headers and its body leaves
    # ``rfile.read(Content-Length)`` parked on a thread that never comes back,
    # and the connection, the thread and everything the handler is holding
    # leak for the life of the process — a reloaded Firmware or Analysis page
    # is enough to do it, because both POST bodies are megabytes.
    #
    # Generous on purpose. It has to clear the SSE keep-alive interval (15 s)
    # by a wide margin, because it also bounds the *writes* those streams
    # make; a client that has stopped reading for a minute while telemetry
    # piles up in its socket buffer is wedged, and dropping it is right.
    # TimeoutError is already in CLIENT_GONE_ERRORS, so the SSE loops treat
    # that exactly like a closed tab.
    timeout = 120

    def setup(self) -> None:
        # Whether any bytes of a response have gone out yet. The error handler
        # below can only send a clean 500 while this is False — once a status
        # line is on the wire (every SSE stream, any partially written body),
        # appending another response would corrupt the stream, so the only
        # honest move left is to close the connection.
        self._response_started = False
        super().setup()

    # The subset of CLIENT_GONE_ERRORS that really only ever means "the peer
    # left". CLIENT_GONE_ERRORS itself ends in the OSError those four inherit
    # from, which is right for an SSE write loop — where every OSError is the
    # socket — and wrong for a whole request, where an OSError is just as
    # likely to be a disk the endpoint could not read. Swallowing that one
    # quietly would hide the very failure worth reporting.
    _PEER_GONE_ERRORS = (
        BrokenPipeError,
        ConnectionResetError,
        ConnectionAbortedError,
        TimeoutError,
    )

    def send_response(self, *args: Any, **kwargs: Any) -> None:
        """Start a response: mark it started and reset the header bookkeeping.

        ``_response_started`` is what stops ``_guarded`` writing a 500 on top
        of a body already going out. The other two are the per-response state
        the ``end_headers`` override below reads, and they have to be cleared
        HERE rather than after the write: a keep-alive connection serves many
        responses through one handler instance, so state left over from the
        previous one would make the next one skip its hardening headers.
        """
        self._response_started = True
        self._security_headers_sent = False
        self._headers_buffer_names = []
        super().send_response(*args, **kwargs)

    def log_message(self, fmt: str, *args) -> None:
        logger.debug("%s - %s", self.address_string(), fmt % args)

    def _guarded(self, dispatch: Callable[[], None]) -> None:
        """Run one request, turning any escape into a response, never a drop.

        ``http.server`` does not catch exceptions out of ``do_GET``/``do_POST``:
        they unwind into ``socketserver``, which prints a bare traceback and
        closes the socket without writing anything. The browser sees a network
        error rather than a status code, so the UI cannot tell "the backend has
        a bug" from "the backend is gone" — and during a flight the two call
        for very different reactions from the operator. Any endpoint that
        raises now answers 500 instead, with the traceback in the log where it
        belongs.

        A client that simply left is not an error and is logged as such; there
        is nobody to answer.
        """
        self._response_started = False
        try:
            if not self._host_header_allowed():
                return
            dispatch()
        except self._PEER_GONE_ERRORS as exc:
            logger.debug("client gone during %s: %s", self.path, exc)
            self.close_connection = True
        except Exception:  # noqa: BLE001 - an endpoint bug must not drop the socket
            logger.exception("unhandled error serving %s", self.path)
            if self._response_started:
                self.close_connection = True
                return
            try:
                self._send_json({"error": "internal server error"}, 500)
            except CLIENT_GONE_ERRORS:
                self.close_connection = True

    def _host_header_allowed(self) -> bool:
        """Reject a request addressed to a name this server does not answer to.

        Sits in ``_guarded`` so it covers reads as well as writes — see
        :func:`_host_header_allowed` for what the check is for. Answers 403
        and closes the connection, the same way the cross-site guard does.
        """
        headers = getattr(self, "headers", None)
        host = headers.get("Host", "") if headers is not None else ""
        if _host_header_allowed(host):
            return True
        logger.warning("rejected request for Host %r on %s", host, self.path)
        self.close_connection = True
        self._send_json({"ok": False, "error": "host not allowed"}, 403)
        return False

    def end_headers(self) -> None:
        """Emit the hardening headers, then close the header block.

        Overridden rather than called from each route because there are close
        to a hundred of them plus five raw-byte writers, and a header set that
        depends on every one of them remembering is a header set that is
        missing somewhere. ``BaseHTTPRequestHandler`` funnels every response —
        JSON, static file, tile, SSE, 304, and its own ``send_error`` — through
        this one call.

        Guarded by a flag rather than by trust: ``send_error`` and a handful of
        stdlib paths can reach here twice on one response, and a duplicated
        ``Content-Security-Policy`` is not additive — the browser intersects
        them, which would silently tighten the policy in ways nobody wrote
        down. A header the route set itself always wins.
        """
        if not getattr(self, "_security_headers_sent", False):
            self._security_headers_sent = True
            existing = {
                name.lower()
                for name, _ in getattr(self, "_headers_buffer_names", ())
            }
            for name, value in SECURITY_HEADERS:
                if name.lower() not in existing:
                    self.send_header(name, value)
        super().end_headers()

    def send_header(self, keyword: str, value: str) -> None:
        """Record what has been set so ``end_headers`` does not duplicate it."""
        names = getattr(self, "_headers_buffer_names", None)
        if names is None:
            names = []
            self._headers_buffer_names = names
        names.append((keyword, value))
        super().send_header(keyword, value)

    def _send_cors(self) -> None:
        """Emit this response's CORS headers: a loopback origin, or nothing.

        ``Vary: Origin`` rides along unconditionally so a cache cannot serve
        one origin's allowed response to another. Defensive ``getattr``: the
        unit-test handlers are built with ``object.__new__`` and have no
        ``headers``.
        """
        headers = getattr(self, "headers", None)
        origin = _cors_allowed_origin(headers.get("Origin", "") if headers else "")
        if origin:
            self.send_header("Access-Control-Allow-Origin", origin)
        self.send_header("Vary", "Origin")

    def _send_json(self, data: dict, status: int = 200) -> None:
        body = json.dumps(_sanitize(data)).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self._send_cors()
        self.end_headers()
        self.wfile.write(body)

    def _sse_stopping(self) -> bool:
        """True once the server this handler belongs to is shutting down.

        Every SSE loop checks this where it would otherwise send a keep-alive
        ping, so a shutdown is noticed within one wait instead of leaving a
        thread writing into a torn-down backend. Defensive ``getattr``: the
        unit-test handlers are built without a ``server``.
        """
        server = getattr(self, "server", None)
        stopping = getattr(server, "stopping", None)
        return stopping is not None and stopping.is_set()

    def _send_sse(self, event: str, data: str) -> None:
        self.wfile.write(f"event: {event}\ndata: {data}\n\n".encode())
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
        """Persist the live config to its path; never raises into the handler.

        Taken under ``_config_write_lock`` so the serialization walk cannot
        run between two field assignments of a concurrent update and write a
        half-applied config to disk. Re-entrant: callers that already hold the
        lock for a whole read-modify-write pass through.
        """
        with _config_write_lock:
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
        except Exception:  # noqa: BLE001 - GET /api/config must never 500
            # logger.exception already carries the traceback; the bound name
            # was never read.
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
            "missions_dir": cfg.missions_dir,
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
        # Carried through the merge but absent from ``known`` below: a write of
        # any other setting must not drop the operator's map keys, and no
        # client may set one here. POST /api/tiles/token owns them.
        if cfg.map_tokens is not None:
            merged["map_tokens"] = cfg.map_tokens
        if cfg.branding is not None:
            merged["branding"] = cfg.branding
        if cfg.controls is not None:
            merged["controls"] = cfg.controls
        if cfg.ui is not None:
            merged["ui"] = cfg.ui
        if cfg.autoconnect is not None:
            merged["autoconnect"] = cfg.autoconnect
        if cfg.updates is not None:
            merged["updates"] = cfg.updates
        if cfg.battery is not None:
            merged["battery"] = cfg.battery
        if cfg.remote_id is not None:
            merged["remote_id"] = cfg.remote_id
        if cfg.plugins is not None:
            merged["plugins"] = cfg.plugins
        if cfg.parameters is not None:
            merged["parameters"] = cfg.parameters
        if cfg.review is not None:
            merged["review"] = cfg.review
        if cfg.checklists is not None:
            merged["checklists"] = cfg.checklists

        known = {
            "mavlink_connection", "http_port", "tile_cache_dir", "tlog_dir",
            "params_dir", "firmware_dir", "log_download_dir", "missions_dir",
            "tile_sources", "stream_rates", "ssh_connections",
            "theme", "map", "branding", "controls", "ui", "updates",
            "autoconnect", "battery", "remote_id", "parameters", "review",
            "checklists",
        }
        # Real config keys that belong to their own endpoint. A client that
        # POSTs back a whole GET /api/config body carries them along, and
        # calling those "unknown" sends whoever reads the log hunting for a typo
        # that is not there. They are still dropped from this merge: forwarding
        # is applied by POST /api/forwarding, which also starts and stops the
        # forwarder, so a generic merge would change the file and not the
        # running state. The live value is untouched either way — it is not in
        # ``merged``, and _save_live_config serializes the live config object.
        # Plugin settings are not in the main config at all any more; each
        # plugin keeps a file of its own (corvus/plugin_config.py), written by
        # POST /api/plugins/settings.
        owned_elsewhere = {"forwarding", "map_tokens", "plugins"}

        for key, value in partial.items():
            if key not in known:
                if key in owned_elsewhere:
                    logger.debug("ignoring config key %r in POST /api/config; "
                                 "it is owned by its own endpoint", key)
                else:
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
                # Merged per key, like controls: the base layer and the 3D
                # mode are independent settings written by two different
                # controls on the map's own rail, one at a time. Replacing
                # wholesale meant choosing a map service erased which 3D the
                # operator was in, and choosing a 3D mode erased their map
                # service — each silently, and only noticed at the next launch.
                base = merged.get("map")
                merged["map"] = {**base, **value} if isinstance(base, dict) else dict(value)
            elif key == "branding":
                if not isinstance(value, dict):
                    return None, "branding must be an object"
                merged["branding"] = value
            elif key == "controls":
                if not isinstance(value, dict):
                    return None, "controls must be an object"
                # Merged per key, unlike theme which replaces wholesale.
                # Each control is an independent switch and the UI toggles one
                # at a time, so a POST naming only "arrow_keys" must not turn
                # the joystick off as a side effect.
                base = merged.get("controls")
                merged["controls"] = {**base, **value} if isinstance(base, dict) else dict(value)
            elif key == "ui":
                if not isinstance(value, dict):
                    return None, "ui must be an object"
                # Merged per key, like controls: the interface size and the
                # app-icon switch are independent settings the UI writes one
                # at a time, so a POST naming only one must not clear the other.
                base = merged.get("ui")
                merged["ui"] = {**base, **value} if isinstance(base, dict) else dict(value)
            elif key == "updates":
                if not isinstance(value, dict):
                    return None, "updates must be an object"
                # Merged per key, like controls: dismissing a version must not
                # also rewrite the operator's "check for updates" switch.
                base = merged.get("updates")
                merged["updates"] = {**base, **value} if isinstance(base, dict) else dict(value)
            elif key == "autoconnect":
                if not isinstance(value, dict):
                    return None, "autoconnect must be an object"
                # Merged per key, like controls: the four toggles are
                # independent, and a POST naming only "enabled" must not put
                # the transport switches back to their defaults. Persisted
                # only — this endpoint never dials and never tears a link down,
                # the way POST /api/config has never dialled for
                # mavlink_connection either. The new toggles take effect on the
                # live watcher below, and fully on the next launch.
                base = merged.get("autoconnect")
                merged["autoconnect"] = (
                    {**base, **value} if isinstance(base, dict) else dict(value)
                )
            elif key == "battery":
                if not isinstance(value, dict):
                    return None, "battery must be an object"
                # Merged per key, like controls: the page saves one field at a
                # time, and a POST naming only "cells" must not put the
                # chemistry and the estimator switch back to their defaults.
                base = merged.get("battery")
                merged["battery"] = {**base, **value} if isinstance(base, dict) else dict(value)
            elif key == "remote_id":
                if not isinstance(value, dict):
                    return None, "remote_id must be an object"
                # Merged one level deep, because the page saves one card at a
                # time: a POST carrying only "basic_id" must not clear the
                # operator registration the operator typed into the card above
                # it. Deeper than that is _coerce_remote_id's job — every leaf
                # is re-coerced from the merge below.
                base = merged.get("remote_id")
                if isinstance(base, dict):
                    nested = dict(base)
                    for sub_key, sub_value in value.items():
                        if (isinstance(sub_value, dict)
                                and isinstance(nested.get(sub_key), dict)):
                            nested[sub_key] = {**nested[sub_key], **sub_value}
                        else:
                            nested[sub_key] = sub_value
                    merged["remote_id"] = nested
                else:
                    merged["remote_id"] = dict(value)
            elif key == "parameters":
                if not isinstance(value, dict):
                    return None, "parameters must be an object"
                # Merged per key, like controls: one switch per POST.
                base = merged.get("parameters")
                merged["parameters"] = (
                    {**base, **value} if isinstance(base, dict) else dict(value)
                )
            elif key == "review":
                if not isinstance(value, dict):
                    return None, "review must be an object"
                # Merged per key, like parameters: one option per POST.
                base = merged.get("review")
                merged["review"] = {**base, **value} if isinstance(base, dict) else dict(value)
            elif key == "checklists":
                if not isinstance(value, dict):
                    return None, "checklists must be an object"
                # Merged per key, like controls: the Settings switch, the Home
                # window's list picker and the Setup editor each write their
                # own key. "lists" is one value and replaces the stored lists
                # whole, because the editor always sends every list.
                base = merged.get("checklists")
                merged["checklists"] = (
                    {**base, **value} if isinstance(base, dict) else dict(value)
                )
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
            elif key == "missions_dir":
                if not isinstance(value, str):
                    return None, "missions_dir must be a string"
                merged["missions_dir"] = value
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
        #
        # In place means every assignment below is its own visible state.
        # Under the lock they are one: no concurrent save can
        # serialize the object between two of them (which wrote a file that was
        # part old config and part new), and no second updater can interleave
        # its own assignments with these.
        #
        # Exactly the fields that went through the merge are copied. A
        # hand-kept list of assignments once missed missions_dir, so a POST
        # of it answered 200 and was dropped. Copying every field would be
        # the opposite bug: rtk, video and forwarding are never in ``merged``
        # (their own endpoints write them), and _build_config would hand
        # back None for each.
        with _config_write_lock:
            for field in dataclasses.fields(new_cfg):
                if field.name in merged:
                    setattr(cfg, field.name, getattr(new_cfg, field.name))
            self._save_live_config()
            self._refresh_autoconnect_session(cfg)
            self._refresh_battery_settings(cfg)
            self._refresh_remote_id(cfg)
            return to_public_dict(cfg), None

    def _refresh_battery_settings(self, cfg: Any) -> None:
        """Hand the bridge the battery estimator settings just persisted.

        The estimate runs on the telemetry path, so a setting that only took
        effect on the next launch would leave the operator watching a
        percentage they had already corrected. Never raises: a settings save
        must not 500 over the feature it is configuring.
        """
        bridge = self.mavlink
        if bridge is None or not hasattr(bridge, "set_battery_settings"):
            return
        try:
            bridge.set_battery_settings(getattr(cfg, "battery", None))
        except Exception:  # noqa: BLE001 - never fail a config save over this
            logger.exception("battery: could not apply the new estimator settings")

    def _refresh_remote_id(self, cfg: Any) -> None:
        """Hand the bridge the Remote ID identity just persisted.

        It has to take effect now rather than on the next launch, and for a
        sharper reason than the battery estimator's: an operator who has just
        corrected a mistyped serial number is standing next to an aircraft that
        is still broadcasting the old one to everyone in range. Never raises: a
        settings save must not 500 over the feature it is configuring.
        """
        bridge = self.mavlink
        if bridge is None or not hasattr(bridge, "set_remote_id"):
            return
        try:
            bridge.set_remote_id(getattr(cfg, "remote_id", None))
        except Exception:  # noqa: BLE001 - never fail a config save over this
            logger.exception("remote id: could not apply the new identity")

    def _refresh_autoconnect_session(self, cfg: Any) -> None:
        """Point the live auto-connect session at the toggles just persisted.

        A switch the operator turned off in Settings has to stop the watcher
        that is already running, not only the one the next launch would start.
        Never raises: a settings save must not 500 over the feature it is
        configuring.
        """
        session = self.autoconnect_session
        if session is None:
            return
        try:
            from .config import autoconnect_settings
            session.apply_config(autoconnect_settings(cfg))
            if self.store is not None:
                self.store.update(link_auto=session.as_dict())
        except Exception:  # noqa: BLE001 - never fail a config save over this
            logger.exception("autoconnect: could not apply the new settings")

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
            # Whether one is stored, never what it is: a connection that came
            # from another machine without its password is shown as needing one.
            clean["has_password"] = bool(clean.pop("password", None))
            clean["connected"] = live.get(clean.get("name", ""), False)
            out.append(clean)
        return out

    def do_GET(self) -> None:
        self._guarded(self._do_get)

    def _do_get(self) -> None:
        path = urlparse(self.path).path
        if path.startswith("/api/"):
            self._handle_api_get(path)
        else:
            self._serve_static(path)

    def do_POST(self) -> None:
        self._guarded(self._do_post)

    def _do_post(self) -> None:
        if not self._mutating_request_allowed():
            return
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

    def _mutating_request_allowed(self) -> bool:
        """Reject browser cross-site writes before any endpoint sees them."""
        headers = getattr(self, "headers", None)
        origin = headers.get("Origin", "") if headers is not None else ""
        fetch_site = headers.get("Sec-Fetch-Site", "") if headers is not None else ""
        if origin:
            allowed = bool(_cors_allowed_origin(origin))
        else:
            allowed = not (
                isinstance(fetch_site, str)
                and fetch_site.strip().lower() == "cross-site"
            )
        if allowed:
            return True
        self.close_connection = True
        self._send_json({"ok": False, "error": "cross-site request forbidden"}, 403)
        return False

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
            # Path-parameter building route: /api/buildings/<z>/<x>/<y>.json
            if path.startswith("/api/buildings/") and path.endswith(".json"):
                m = _BUILDINGS_PATH_RE.match(path)
                if m is not None:
                    self._api_buildings_serve(
                        int(m.group(1)), int(m.group(2)), int(m.group(3)),
                    )
                    return
            # Path-parameter plugin asset route, matched before the exact
            # table so /api/plugins itself is never shadowed by it.
            if path.startswith("/api/plugins/asset/"):
                m = _PLUGIN_ASSET_RE.match(path)
                if m is not None:
                    self._api_plugin_asset(m.group(1), m.group(2))
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
            # Kept under its PX4 name because the shape of this response is
            # pinned; the value is whatever firmware answered, PX4 or
            # ArduPilot. Which stack that is rides on /api/state and
            # /api/mavlink/capabilities.
            "px4_profile": px4_profile,
        })

    @route("GET", "/api/update")
    def _api_update(self) -> None:
        """Whether a newer Corvus GCS release exists on GitHub.

        Answered from the on-disk cache unless it has gone stale or
        ``?refresh=1`` is set, so a launch in the field costs no network and no
        wait. A failed check returns 200 with the last known release and a
        non-empty ``error`` — the frontend fetches this in the background and
        an unreachable GitHub must be silent, not an alert.

        ``enabled`` is false when the operator turned the check off; the
        network is then never touched and ``update_available`` is always false.
        ``skipped`` echoes the release the operator dismissed so the frontend
        knows not to raise the dialog for it again.
        """
        cfg = self._live_config()
        updates_cfg = cfg.updates if isinstance(cfg.updates, dict) else {}
        enabled = updates_cfg.get("check", True) is not False
        skipped = str(updates_cfg.get("skipped") or "")
        if self.updates is None:
            self._send_json({
                "enabled": enabled, "skipped": skipped,
                "current": get_version(), "latest": "", "update_available": False,
                "name": "", "url": "", "published": "", "notes": "", "assets": [],
                "checked_at": 0, "error": "update check unavailable",
            })
            return
        params = parse_qs(urlparse(self.path).query)
        refresh = (params.get("refresh", ["0"])[0] or "0").lower() in ("1", "true", "yes")
        try:
            if enabled:
                data = self.updates.check(force=refresh)
            else:
                # Off means off: report what is already on disk, reach for
                # nothing. update_available stays false so no dialog can fire.
                data = self.updates.cached_status()
                data["update_available"] = False
        except Exception:  # noqa: BLE001 - an update check must never 500
            logger.exception("update check failed")
            data = {
                "current": get_version(), "latest": "", "update_available": False,
                "name": "", "url": "", "published": "", "notes": "", "assets": [],
                "checked_at": 0, "error": "update check failed",
            }
        data["enabled"] = enabled
        data["skipped"] = skipped
        self._send_json(data)

    @route("POST", "/api/update/skip")
    def _api_update_skip(self, payload: dict) -> None:
        """Remember a release the operator dismissed, so it stops prompting.

        An empty/absent ``version`` clears the dismissal, which is what the
        Settings page's "check again" does.
        """
        raw = payload.get("version")
        version = raw.strip() if isinstance(raw, str) else ""
        public, error = self._apply_config_partial({"updates": {"skipped": version}})
        if error is not None:
            self._send_json({"ok": False, "error": error}, 400)
            return
        stored = ""
        cfg_updates = self._live_config().updates
        if isinstance(cfg_updates, dict):
            stored = str(cfg_updates.get("skipped") or "")
        self._send_json({"ok": True, "skipped": stored, "config": public})

    @route("POST", "/api/update/open")
    def _api_update_open(self, payload: dict) -> None:
        """Open the latest release's page in the operator's system browser.

        The URL is re-derived from the cached check result and never read from
        the request. It has to be a server-side action at all because the
        desktop build runs the UI inside QtWebEngine, where an external link
        simply goes nowhere (the same reason the credits dialog prints URLs
        instead of linking them); taking the URL from the caller would turn
        that convenience into an open redirect out of the app.
        """
        if self.updates is None:
            self._send_json({"ok": False, "error": "update check unavailable"}, 503)
            return
        try:
            url = self.updates.cached_status().get("url") or ""
        except Exception:  # noqa: BLE001 - never 500 on a convenience action
            logger.exception("update page lookup failed")
            url = ""
        if not url:
            self._send_json({"ok": False, "error": "no release page known"}, 404)
            return
        # Belt and braces over update_check's own derivation: whatever reaches
        # this line is about to be handed to the platform's URL opener, and on
        # every desktop OS that is a program launcher for schemes far beyond
        # http. The scheme is the part that decides which program runs, so it
        # is checked here rather than assumed from upstream.
        if urlparse(url).scheme not in ("http", "https"):
            logger.warning("refusing to open a non-web release URL: %r", url)
            self._send_json({"ok": False, "error": "no release page known"}, 404)
            return
        opened = open_url(url)
        if not opened:
            self._send_json(
                {"ok": False, "error": "no browser available", "url": url}, 200)
            return
        self._send_json({"ok": True, "url": url})

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

    # ---- Plugins ----
    def _plugin_roots(self) -> tuple[str | None, str | None]:
        """The (user, bundled) plugin roots this handler scans."""
        return self.plugin_user_dir, self.plugin_bundled_dir

    @route("GET", "/api/plugins")
    def _api_plugins(self) -> None:
        """List the installed plugins and where they are dropped in.

        The frontend loads each plugin's scripts from the returned paths, and
        the Settings page shows the two folders. ``settings`` carries the saved
        per-plugin state so a plugin can restore its form without a second
        request. Always answers 200: no plugins is an empty list, not an error.
        """
        user_dir, bundled_dir = self._plugin_roots()
        try:
            plugins = public_plugin_list(user_dir, bundled_dir)
        except Exception:  # noqa: BLE001 - a broken folder must not 500 the UI
            logger.exception("plugin discovery failed; reporting none")
            plugins = []
        self._send_json({
            "plugins": plugins,
            "user_dir": user_dir or user_plugins_dir(),
            "bundled_dir": bundled_dir or bundled_plugins_dir(),
            "settings": self._plugin_settings_all(),
        })

    def _api_plugin_asset(self, plugin_id: str, rel: str) -> None:
        """Serve one file out of a plugin's folder.

        Not a static-file route with a different root: the folder is looked up
        through the registry (so only a folder that parsed as a plugin is
        reachable at all) and the path is resolved by the same containment
        check the manifest parser uses. A miss on either is a 404 — an
        operator's plugin folder is not a place to probe for files.
        """
        user_dir, bundled_dir = self._plugin_roots()
        try:
            plugin_dir = find_plugin_dir(plugin_id, user_dir, bundled_dir)
        except Exception:  # noqa: BLE001
            logger.exception("plugin lookup failed for %r", plugin_id)
            plugin_dir = None
        if plugin_dir is None:
            self._send_json({"error": "not found"}, 404)
            return
        target = resolve_plugin_asset(plugin_dir, rel)
        if target is None:
            self._send_json({"error": "not found"}, 404)
            return
        try:
            body = target.read_bytes()
        except OSError as exc:
            logger.warning("plugin asset %s/%s unreadable: %s", plugin_id, rel, exc)
            self._send_json({"error": "not found"}, 404)
            return
        ctype = content_type(target)
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        # Same as the app's own static files: a plugin edited on disk must show
        # up on the next start, not after a cache expiry the operator cannot see.
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        self.wfile.write(body)

    def _plugin_settings_all(self) -> dict[str, Any]:
        """Every plugin's saved settings, keyed by plugin id.

        Read from each plugin's own config file. Anything still under the main
        config's old ``plugins`` key fills in for a plugin that has no file
        yet, so a migration that could not write loses nothing.
        """
        legacy = self._live_config().plugins
        out: dict[str, Any] = dict(legacy) if isinstance(legacy, dict) else {}
        out.update(plugin_config.load_all(self.plugin_user_dir))
        return out




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

    # Which parameter schema the setup pages are built from. The pages —
    # motors, safety, tuning, radio, battery, Remote ID — are each a list of
    # *parameter names*, and
    # not one PX4 name exists on an ArduPilot vehicle. Choosing the wrong module
    # does not error; the vehicle answers for nothing and the page comes up
    # empty, which is how an ArduPilot operator used to get a blank Safety page
    # rather than their own failsafes.
    _SCHEMAS: dict[str, dict[str, Any]] = {
        autopilot.STACK_ARDUPILOT: {
            "motors": ardupilot_motors, "safety": ardupilot_safety,
            "tuning": ardupilot_tuning, "rc": ardupilot_rc,
            "battery": ardupilot_battery, "remote_id": ardupilot_remote_id,
            "mounting": ardupilot_mounting,
        },
    }
    _DEFAULT_SCHEMAS: dict[str, Any] = {
        "motors": motor_config, "safety": safety_config,
        "tuning": tuning_config, "rc": rc_config,
        "battery": battery_config, "remote_id": remote_id_config,
        "mounting": mounting_config,
    }

    def _schema(self, page: str) -> Any:
        """The schema module for *page* on the stack currently connected.

        PX4's modules are the default, and a stack Corvus does not recognise
        gets them too: they are the primary target, and a page built from a
        superset the vehicle answers for none of is the same empty page an
        unknown stack would get from any other choice.
        """
        stack = ""
        if self.mavlink is not None:
            # getattr, not attribute access: a plugin or a test may hand the
            # server a bridge-shaped object that predates the dialect layer, and
            # the setup pages must still render for it rather than 500.
            stack = getattr(self.mavlink, "stack", "")
        elif self.store is not None:
            stack = self.store.get_snapshot().get("autopilot_stack") or ""
        return self._SCHEMAS.get(stack, self._DEFAULT_SCHEMAS)[page]

    def _vehicle_type_id(self) -> int:
        """The MAV_TYPE the schemas key their per-airframe tables off."""
        return int(getattr(self.mavlink, "vehicle_type_id", 0) or 0)

    def _fetch_page_params(self, names: list[str]) -> dict[str, float]:
        """One setup page's batched read; ``?fresh=1`` asks the vehicle, not the cache.

        A page asks fresh right after "Check values", so it is redrawn from
        what the vehicle holds now, including what another station changed.
        ``fresh`` is passed only when asked: bridge-shaped test doubles
        predate it.
        """
        query = parse_qs(urlparse(getattr(self, "path", "") or "").query)
        fresh = (query.get("fresh") or [""])[0] in ("1", "true")
        return self.mavlink.fetch_params(names, **({"fresh": True} if fresh else {}))


    @route("GET", "/api/mounting")
    def _api_mounting(self) -> None:
        """Return how the flight controller is mounted, for the Calibration page.

        Same contract as ``/api/motors``: one batched read of the parameters
        the stack's mounting schema (:mod:`corvus.mounting_config` or
        :mod:`corvus.ardupilot_mounting`) knows about, only what the vehicle
        answered for, and always 200 with ``connected`` saying which it is.
        ``orientation`` is the board rotation every calibration is measured
        through; ``positions`` are the lever arms, which the Motors page shows
        and edits.
        """
        schema = self._schema("mounting")
        if self.mavlink is None:
            payload = schema.build({})
            payload["connected"] = False
            self._send_json(payload)
            return
        try:
            values = self._fetch_page_params(schema.param_names())
        except Exception as exc:  # noqa: BLE001 - a read must never 500 the page
            logger.exception("mounting parameter fetch failed")
            payload = schema.build({})
            payload["connected"] = False
            payload["error"] = str(exc)
            self._send_json(payload)
            return
        payload = schema.build(values)
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

        ``?extra=A,B,C`` adds parameters the operator named on the page itself,
        for the settings no preset can know about. They ride the same batch read
        and come back as ``extra``. The names are browser input, so
        :func:`safety_config.normalise_extra` bounds them before they reach the
        bridge — an unbounded list here would be an unbounded burst of
        ``PARAM_REQUEST_READ`` at the aircraft.

        Always 200 so the page can render a "not connected" state instead of an
        error banner; ``connected`` says which it is.
        """
        schema = self._schema("safety")
        query = parse_qs(urlparse(self.path).query)
        extra = schema.normalise_extra(
            [n for value in query.get("extra", []) for n in value.split(",")])
        if self.mavlink is None:
            self._send_json({
                "connected": False, "sections": [], "extra": [], "received": 0,
            })
            return
        try:
            values = self._fetch_page_params(schema.param_names() + extra)
        except Exception as exc:  # noqa: BLE001 - a read must never 500 the page
            logger.exception("safety parameter fetch failed")
            self._send_json({
                "connected": False, "sections": [], "extra": [], "received": 0,
                "error": str(exc),
            })
            return
        payload = schema.build(
            values, extra,
            **({"vehicle": autopilot.vehicle_class(self._vehicle_type_id())}
               if schema is ardupilot_safety else {}),
        )
        payload["connected"] = bool(values)
        if not values:
            payload["error"] = self.mavlink.get_last_command_error() or "no parameters received"
        self._send_json(payload)


    @route("GET", "/api/tuning")
    def _api_tuning(self) -> None:
        """Return the vehicle's PID tuning configuration as a description.

        Same shape and same contract as ``/api/safety`` and ``/api/motors``:
        one batched read of the parameters :mod:`corvus.tuning_config` knows
        about, handed to that module to turn into groups — one per loop of the
        control cascade, innermost first, plus the autotune.

        A parameter the connected firmware does not answer for is one field
        fewer, an empty section is dropped, and an empty group disappears, so a
        multicopter and a fixed wing each get their own controllers out of the
        one schema without a version switch (AGENTS.md: graceful fallback
        across v1.16/v1.17/v1.18).

        Always 200 so the page can render a "not connected" state instead of an
        error banner; ``connected`` says which it is.
        """
        schema = self._schema("tuning")
        if self.mavlink is None:
            self._send_json({"connected": False, "groups": [], "received": 0})
            return
        try:
            values = self._fetch_page_params(schema.param_names())
        except Exception as exc:  # noqa: BLE001 - a read must never 500 the page
            logger.exception("tuning parameter fetch failed")
            self._send_json({
                "connected": False, "groups": [], "received": 0, "error": str(exc),
            })
            return
        payload = schema.build(
            values,
            **({"vehicle": autopilot.vehicle_class(self._vehicle_type_id())}
               if schema is ardupilot_tuning else {}),
        )
        payload["connected"] = bool(values)
        if not values:
            payload["error"] = self.mavlink.get_last_command_error() or "no parameters received"
        self._send_json(payload)

    @route("GET", "/api/battery")
    def _api_battery(self) -> None:
        """Return the vehicle's battery configuration as a description.

        Same shape and same contract as ``/api/safety``, ``/api/motors`` and
        ``/api/tuning``: one batched read of the parameters
        :mod:`corvus.battery_config` knows about, handed to that module to turn
        into sections — the pack, how the autopilot measures it, and the levels
        the low-battery failsafe reacts to.

        Two things ride along that the other pages have no equivalent of:

        ``pack`` is the normalised summary the page's battery diagram is drawn
        from (cell count, the volts a cell holds full and empty, where the
        thresholds fall), so the same drawing code serves a PX4 and an
        ArduPilot aircraft.

        ``settings`` is Corvus's *own* estimator configuration — which
        remaining figure the interface shows and the pack it is read against.
        It lives in the config file rather than on the vehicle because it is
        not a vehicle setting: ArduPilot has no cell-count parameter at all,
        and on either stack the choice of whose estimate to believe is the
        operator's, not the autopilot's. It is written back through
        ``POST /api/config`` under the same key.

        Always 200 so the page can render a "not connected" state instead of an
        error banner; ``connected`` says which it is.
        """
        schema = self._schema("battery")
        query = parse_qs(urlparse(self.path).query)
        fresh = (query.get("fresh") or [""])[0] in ("1", "true")
        stored = getattr(self.config, "battery", None)
        settings = battery.settings(stored)
        # Both, because they are different questions. ``settings`` is what the
        # estimator actually runs on; ``configured`` is what the operator
        # typed, where a 0 means "work it out" and has to keep meaning that
        # when the form is drawn again.
        configured = dict(stored) if isinstance(stored, dict) else {}
        empty = {"connected": False, "sections": [], "received": 0,
                 "pack": schema.pack_summary({}), "settings": settings,
                 "configured": configured,
                 "chemistries": battery.chemistry_options()}
        if self.mavlink is None:
            self._send_json(empty)
            return
        try:
            # ``fresh`` only when asked: bridge-shaped test doubles predate it.
            values = self.mavlink.fetch_params(
                schema.param_names(), **({"fresh": True} if fresh else {}))
        except Exception as exc:  # noqa: BLE001 - a read must never 500 the page
            logger.exception("battery parameter fetch failed")
            self._send_json(dict(empty, error=str(exc)))
            return
        payload = schema.build(
            values,
            **({"vehicle": autopilot.vehicle_class(self._vehicle_type_id())}
               if schema is ardupilot_battery else {}),
        )
        payload["settings"] = settings
        payload["configured"] = configured
        payload["chemistries"] = battery.chemistry_options()
        payload["connected"] = bool(values)
        if not values:
            payload["error"] = self.mavlink.get_last_command_error() or "no parameters received"
        self._send_json(payload)

    @route("GET", "/api/remoteid")
    def _api_remote_id(self) -> None:
        """Return the Remote ID page: the identity, the rules, and the vehicle.

        This is the one setup page whose subject is not the aircraft. The
        serial number, the operator registration, the flight description and
        the EU classification are the *operator's* and are not stored on the
        vehicle at all — they are broadcast to it over the link once a second
        by whichever station is flying it (see :mod:`corvus.remote_id`). So the
        response carries three separate things rather than one set of sections:

        ``identity`` / ``configured``
            What Corvus broadcasts, resolved and as-typed. Written back through
            ``POST /api/config`` under the same key, like the battery
            estimator's settings.

        ``findings`` / ``schema``
            What each region's broadcast format asks for that this identity
            does not have, and the option tables the form is drawn from. The
            findings are advisory by design: an aircraft flown under a national
            exemption is not misconfigured, and a page that refused to save
            would be wrong about it.

        ``sections`` / ``status``
            The vehicle's own Remote ID parameters, read in one batch like
            every other setup page, and what the live broadcast is doing —
            whether the link can carry the messages at all, whether they are
            going out, and the arm verdict the aircraft sent back.

        Always 200 so the page can render a "not connected" state instead of an
        error banner; ``connected`` says which it is. The identity half renders
        either way — it is configured on the ground, often before the aircraft
        is powered.
        """
        schema = self._schema("remote_id")
        stored = getattr(self.config, "remote_id", None)
        identity = remote_id.settings(stored)
        configured = dict(stored) if isinstance(stored, dict) else {}
        base: dict[str, Any] = {
            "connected": False, "sections": [], "received": 0,
            "identity": identity,
            "configured": configured,
            "schema": remote_id.schema(),
            "findings": remote_id.findings(identity),
            "status": {"enabled": bool(identity["enabled"]), "supported": False,
                       "broadcasting": False, "last_sent_age": None,
                       "error": "", "arm_status": None},
            "suggested_ua_type": 0,
        }
        if self.mavlink is None:
            self._send_json(base)
            return
        if hasattr(self.mavlink, "remote_id_status"):
            try:
                base["status"] = self.mavlink.remote_id_status()
            except Exception:  # noqa: BLE001 - status must never 500 the page
                logger.exception("remote id status failed")
        # What the airframe looks like, offered as a suggestion for the UA type.
        # Never written on the operator's behalf: what an aircraft *is* and what
        # it was *registered as* are two different facts, and only the second
        # one is the one a regulator reads.
        base["suggested_ua_type"] = remote_id.ua_type_for_vehicle(self._vehicle_type_id())
        try:
            values = self._fetch_page_params(schema.param_names())
        except Exception as exc:  # noqa: BLE001 - a read must never 500 the page
            logger.exception("remote id parameter fetch failed")
            self._send_json(dict(base, error=str(exc)))
            return
        payload = dict(base, **schema.build(values))
        payload["connected"] = bool(values)
        if not values:
            payload["error"] = self.mavlink.get_last_command_error() or "no parameters received"
        self._send_json(payload)

    @route("GET", "/api/remoteid/status")
    def _api_remote_id_status(self) -> None:
        """Return only what the Remote ID broadcast is doing right now.

        Split out of ``/api/remoteid`` because the page polls it. That endpoint
        reads the vehicle's parameters, and a parameter read on a link that is
        reconnecting can sit for tens of seconds behind the bridge's operation
        lock — polled every few seconds, those pile up against the browser's
        six-connection-per-origin limit and starve the map and every other
        fetch on the page. This one touches no MAVLink at all: it reads three
        fields the bridge already holds in memory and returns.

        The findings ride along because they are derived from the stored
        identity alone, so the page can keep its checklist in step with a save
        without asking for the schema and the parameters a second time.
        """
        stored = getattr(self.config, "remote_id", None)
        identity = remote_id.settings(stored)
        payload: dict[str, Any] = {
            "identity": identity,
            "findings": remote_id.findings(identity),
            "status": {"enabled": bool(identity["enabled"]), "supported": False,
                       "broadcasting": False, "last_sent_age": None,
                       "error": "", "arm_status": None},
        }
        if self.mavlink is not None and hasattr(self.mavlink, "remote_id_status"):
            try:
                payload["status"] = self.mavlink.remote_id_status()
            except Exception:  # noqa: BLE001 - a poll must never 500 the page
                logger.exception("remote id status failed")
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
        limit = MAX_SETTINGS_BODY_BYTES if path == "/api/settings/import" else MAX_JSON_BODY_BYTES
        if length > limit:
            self._send_json({"error": "request too large"}, 413)
            return
        raw = self.rfile.read(length) if length else b"{}"
        if length and len(raw) != length:
            self.close_connection = True
            self._send_json({"error": "request body was cut short"}, 400)
            return
        try:
            payload = json.loads(raw, parse_constant=_reject_json_constant)
        except (json.JSONDecodeError, ValueError):
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


















    # ---- Mission planner -------------------------------------------------
    #
    # A plan is validated in exactly one place (``corvus.mission``), and every
    # route below goes through it — the one that uploads to the aircraft, the
    # one that writes a file, and the one that reads a file back. That is the
    # whole reason the model is its own module: the frontend is not a trusted
    # source of mission items, and neither is a JSON file an operator copied
    # from another machine.










    @route("POST", "/api/ssh/connect")
    def _api_ssh_connect(self, payload: dict) -> None:
        """Open a named interactive SSH session.

        ``name`` is the session key the terminal, ``/api/ssh/send`` and
        ``/api/ssh/stream`` all address. Credentials come from ``host`` /
        ``username`` / ``password`` / ``key_path`` when given, and otherwise
        from the saved connection named by ``from`` — or by ``name`` itself,
        which is the SSH tab's one-session-per-saved-host case.
        """
        if not self.ssh:
            self._send_json({"error": "ssh not ready"}, 500)
            return
        name = payload.get("name", "device")
        host = payload.get("host", "")
        # ``from`` names the SAVED connection to borrow credentials from, while
        # ``name`` names the session. They are the same thing for the SSH tab
        # (one session per saved host), but a plugin that wants several live
        # sessions on one machine — a launcher with a terminal per button —
        # needs its own session names without duplicating the host entry.
        borrow = payload.get("from", "")
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
        if not isinstance(borrow, str):
            self._send_json({"error": "from must be a string"}, 400)
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
        # The saved connection this attempt was resolved from, if any. A
        # failure then carries ``needs`` + ``connection`` so the UI can ask
        # for exactly what is missing and save it under that name.
        lookup = ""
        if not host:
            lookup = borrow or name
            saved = None
            for entry in self._live_config().ssh_connections:
                if isinstance(entry, dict) and entry.get("name") == lookup:
                    saved = entry
                    break
            if saved is None:
                self._send_json({
                    "error": f"no saved connection named {lookup!r}" if borrow else "no host",
                    "needs": "connection", "connection": lookup,
                }, 400)
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
                self._send_json({"error": "no host", "needs": "connection",
                                 "connection": lookup}, 400)
                return
        # An empty username reaches paramiko as an empty auth user and fails
        # with an opaque authentication error; say what is actually missing.
        if not isinstance(username, str) or not username:
            reply_err: dict[str, Any] = {"error": "username is required"}
            if lookup:
                reply_err.update(needs="credentials", connection=lookup)
            self._send_json(reply_err, 400)
            return
        ok = self.ssh.connect(name, host, port, username, password, key_path)
        # Echo the identity the connection actually used. The UI renders the
        # terminal header from this rather than assuming a default account,
        # so an operator connecting as someone other than "corvus" sees their
        # own user.
        reply: dict[str, Any] = {
            "ok": ok, "connected": ok,
            "name": name, "host": host, "port": port, "username": username,
        }
        if not ok:
            # Every caller already shows `error`; without it a refused port, a
            # host that is off and a wrong password all read the same.
            reply["error"] = self.ssh.connect_error(name) or "Connection failed."
            # getattr: a bridge double written before the flag has no answer,
            # which is "not known to be the login", not a crash.
            auth_failed = getattr(self.ssh, "connect_auth_failed", None)
            if lookup and callable(auth_failed) and auth_failed(name):
                reply.update(needs="credentials", connection=lookup)
        self._send_json(reply)

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






    # ---- First start setup (src/js/welcome.js) ----
    def _finish_first_start(self) -> None:
        """Mark the first start setup done for every handler of this process."""
        state = self.first_start
        if state is not None:
            state["pending"] = False

    @route("GET", "/api/welcome")
    def _api_welcome(self) -> None:
        """Whether the first start setup should open: ``{"pending": bool}``.

        Pending on a station that had no config file when Corvus started,
        until the operator finishes it, skips it, or imports a settings file.
        """
        state = self.first_start
        self._send_json({"pending": bool(state and state.get("pending"))})

    @route("POST", "/api/welcome")
    def _api_welcome_done(self, payload: dict) -> None:
        """Mark the first start setup finished or skipped.

        The config is written even when nothing in it changed, because a
        config file on disk is what tells the next start that this station
        has been set up. ``saved`` is false when that write failed: the setup
        is then done for this session and offered again on the next start.
        """
        self._finish_first_start()
        saved = True
        with _config_write_lock:
            try:
                save_config(self._live_config(), self.config_path or default_config_path())
            except OSError as exc:
                logger.error("first start setup: config not saved: %s", exc)
                saved = False
        self._send_json({"ok": True, "saved": saved})

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
        if len(raw) != length:
            self.close_connection = True
            self._send_json({"ok": False, "error": "logo upload was cut short"}, 400)
            return
        if not raw.startswith(PNG_MAGIC):
            self._send_json({"ok": False, "error": "logo must be a PNG image"}, 400)
            return

        query = parse_qs(urlparse(self.path).query)
        # Only the basename is kept: this string is display metadata, never a
        # path the server opens, and stripping directories (both separators,
        # so a Windows client's path is handled too) keeps it that way.
        raw_name = (query.get("name") or [""])[0].replace("\\", "/")
        name = os.path.basename(raw_name).strip()[:120] or "logo.png"

        try:
            self._store_logo(raw)
        except OSError as exc:
            logger.error("company logo write to %s failed: %s", self._logo_path(), exc)
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
        ``port`` (int, default 22), ``username`` (str). ``key_path`` is
        optional and may be ``""``. Replaces any existing entry with the same
        name; appends otherwise. Persists atomically. Returns the redacted
        connection list (no password).

        ``password`` is optional too, but treated differently: GET never
        echoes it back (see ``_ssh_connections_public``), so an editor working
        from that list cannot round-trip it. Omitting the key entirely keeps
        whatever password the entry being replaced already had; sending it
        (including ``""``) sets it, same as before.

        ``original_name`` renames an existing entry in place instead of
        appending a second one: the entry matched for both the replace target
        and the password fallback above is the one named ``original_name``
        (falling back to ``name`` when omitted, i.e. the non-renaming case).
        Renaming onto a name already used by a *different* saved entry is
        rejected — silently replacing it would merge two connections into one.
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
        has_password = "password" in payload
        password = payload.get("password", "")
        if not isinstance(password, str):
            password = ""
        original_name = payload.get("original_name")
        if not isinstance(original_name, str) or not original_name:
            original_name = name

        # Scan-then-append is a read-modify-write on a list every handler
        # thread shares. Two saves racing here lost a connection outright: the
        # remove endpoint rebuilds the list from a snapshot taken before this
        # append, so the entry that was just added vanished on the next save.
        with _config_write_lock:
            cfg = self._live_config()
            existing_index = None
            existing_entry = None
            for i, existing in enumerate(cfg.ssh_connections):
                if isinstance(existing, dict) and existing.get("name") == original_name:
                    existing_index = i
                    existing_entry = existing
                    break

            if name != original_name and any(
                isinstance(e, dict) and e.get("name") == name for e in cfg.ssh_connections
            ):
                self._send_json(
                    {"error": f'"{name}" is already saved. Pick another name.'}, 400)
                return

            if not has_password:
                password = existing_entry.get("password", "") if existing_entry else ""

            entry = {
                "name": name,
                "host": host,
                "port": port,
                "username": username,
                "key_path": key_path,
                "password": password,
            }
            if existing_index is not None:
                cfg.ssh_connections[existing_index] = entry
            else:
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
        with _config_write_lock:
            cfg = self._live_config()
            cfg.ssh_connections = [
                entry for entry in cfg.ssh_connections
                if isinstance(entry, dict) and entry.get("name") != name
            ]
            self._save_live_config()
        self._send_json({"ok": True, "connections": self._ssh_connections_public()})

    @route("POST", "/api/ssh/run")
    def _api_ssh_run(self, payload: dict) -> None:
        """Run one command on a host over its own short-lived SSH connection.

        The building block behind a one-button launcher: optionally ``cd`` into
        *directory*, then run *command*. With ``detach`` the command is started
        with ``nohup`` in the background and the call returns as soon as the
        shell has forked it, so a program that runs for hours does not hold the
        request open — and does not die when the connection closes.

        Credentials come from the saved connection named by ``name`` (the same
        list the SSH page manages), so neither the browser nor a plugin ever
        holds a password. ``host``/``username``/``port`` may be given directly
        instead for an ad-hoc target.
        """
        command = payload.get("command")
        if not isinstance(command, str) or not command.strip():
            self._send_json({"ok": False, "error": "command must be a non-empty string"}, 400)
            return
        command = command.strip()
        directory = payload.get("directory", "")
        if not isinstance(directory, str):
            self._send_json({"ok": False, "error": "directory must be a string"}, 400)
            return
        directory = directory.strip()
        detach = payload.get("detach", False)
        if not isinstance(detach, bool):
            self._send_json({"ok": False, "error": "detach must be a boolean"}, 400)
            return

        name = payload.get("name", "")
        if not isinstance(name, str):
            self._send_json({"ok": False, "error": "name must be a string"}, 400)
            return
        host = payload.get("host", "")
        if not isinstance(host, str):
            self._send_json({"ok": False, "error": "host must be a string"}, 400)
            return
        username = payload.get("username", "")
        if not isinstance(username, str):
            username = ""
        raw_port = payload.get("port", 22)
        if isinstance(raw_port, bool):
            self._send_json({"ok": False, "error": "port must be an integer"}, 400)
            return
        try:
            port = int(raw_port)
        except (TypeError, ValueError):
            self._send_json({"ok": False, "error": "port must be an integer"}, 400)
            return
        if not 0 <= port <= 65535:
            self._send_json({"ok": False, "error": "port must be between 0 and 65535"}, 400)
            return

        password: str | None = None
        key_path: str | None = None
        saved: dict[str, Any] | None = None
        if name:
            for entry in self._live_config().ssh_connections:
                if isinstance(entry, dict) and entry.get("name") == name:
                    saved = entry
                    break
            if saved is None and not host:
                self._send_json({"ok": False, "error": f"no saved connection named {name!r}",
                                 "needs": "connection", "connection": name}, 400)
                return
            if saved is not None:
                host = host or saved.get("host", "")
                if "port" not in payload:
                    port = _coerce_port(saved.get("port", 22), 22)
                username = username or saved.get("username", "")
                password = saved.get("password") or None
                key_path = saved.get("key_path") or None
        from_saved = bool(name) and saved is not None
        if not host:
            body: dict[str, Any] = {"ok": False, "error": "no host"}
            if from_saved:
                body.update(needs="connection", connection=name)
            self._send_json(body, 400)
            return
        if not username:
            body = {"ok": False, "error": "username is required"}
            if from_saved:
                body.update(needs="credentials", connection=name)
            self._send_json(body, 400)
            return

        line = _compose_remote_command(directory, command, detach)
        result = ssh_run_command(
            host=host, command=line, port=port, username=username,
            password=password, key_path=key_path,
        )
        # Echo what actually ran: a launcher that failed is nearly always a
        # wrong folder or a wrong binary, and the operator can only see that
        # if the composed line comes back with the error.
        result["command"] = line
        result["host"] = host
        result["username"] = username
        result["port"] = port
        if from_saved and result.pop("auth_failed", False):
            result.update(needs="credentials", connection=name)
        result.pop("auth_failed", None)
        # 200 even for a command that failed: the request itself succeeded, and
        # the caller needs the ``stderr`` in the body to say *why* it failed —
        # an error status would leave the UI with nothing but a status line.
        # ``ok`` is the field to branch on.
        self._send_json(result)

    @route("POST", "/api/plugins/settings")
    def _api_plugins_settings(self, payload: dict) -> None:
        """Save a plugin's settings and persist them.

        Scoped by plugin id, and merged key-by-key by default so a plugin
        writing one field does not clear the rest. ``replace: true`` stores the
        object as given instead — for a plugin that owns its whole settings
        object and needs a key it no longer uses to actually go away, which a
        merge can never do.

        Stored in the plugin's own file, ``<plugin folder>/<id>/config.json``
        (see corvus/plugin_config.py), never in the main config, so it can be
        copied to another machine on its own. This is ordinary UI state (which
        saved SSH connection a launcher points at, which folder it opens) and
        the file is not a secret store: a plugin references a saved SSH
        connection by name instead of keeping a password.
        """
        plugin_id = payload.get("id")
        if not isinstance(plugin_id, str) or not plugin_id:
            self._send_json({"error": "id must be a non-empty string"}, 400)
            return
        if not is_valid_plugin_id(plugin_id):
            self._send_json({"error": "id is not a valid plugin id"}, 400)
            return
        settings = payload.get("settings")
        if not isinstance(settings, dict):
            self._send_json({"error": "settings must be an object"}, 400)
            return
        replace = payload.get("replace", False)
        if not isinstance(replace, bool):
            self._send_json({"error": "replace must be a boolean"}, 400)
            return
        user_dir = self.plugin_user_dir
        with _config_write_lock:
            cfg = self._live_config()
            legacy = cfg.plugins if isinstance(cfg.plugins, dict) else {}
            # A merge onto a plugin whose settings were never moved out of the
            # main config starts from those, not from nothing.
            if (not replace and plugin_id in legacy
                    and plugin_config.load(plugin_id, user_dir) is None):
                base = legacy[plugin_id]
                settings = {**base, **settings} if isinstance(base, dict) else settings
            try:
                saved = plugin_config.update(plugin_id, settings, replace=replace,
                                             user_dir=user_dir)
            except (OSError, ValueError) as exc:
                logger.error("plugin %s: saving settings failed: %s", plugin_id, exc)
                self._send_json({"error": f"could not save settings: {exc}"}, 500)
                return
            if plugin_id in legacy:
                rest = {k: v for k, v in legacy.items() if k != plugin_id}
                cfg.plugins = rest or None
                self._save_live_config()
        self._send_json({"ok": True, "settings": saved})

    @route("POST", "/api/plugins/folder")
    def _api_plugins_folder(self, payload: dict) -> None:
        """Create the operator's plugin folder if needed and show it to them.

        The folder is created here rather than at startup so an operator who
        never opens this never gets a directory they did not ask for; the
        README that explains the plugin layout is written with it.
        """
        path = self.plugin_user_dir or ensure_user_plugins_dir()
        ok, error = open_folder(path)
        # The path is worth returning either way: on a headless machine the
        # operator still needs to know where to copy the folder to.
        self._send_json({"ok": ok, "path": path, "error": error}, 200 if ok else 500)

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

    @route("POST", "/api/ssh/resize")
    def _api_ssh_resize(self, payload: dict) -> None:
        """Set a session's remote pty size to the operator's terminal size."""
        name = payload.get("name", "")
        if not isinstance(name, str) or not name:
            self._send_json({"ok": False, "error": "name must be a non-empty string"}, 400)
            return
        dims = {}
        for key in ("cols", "rows"):
            raw = payload.get(key)
            if isinstance(raw, bool):
                self._send_json({"ok": False, "error": f"{key} must be an integer"}, 400)
                return
            try:
                dims[key] = int(raw)
            except (TypeError, ValueError):
                self._send_json({"ok": False, "error": f"{key} must be an integer"}, 400)
                return
            if not 1 <= dims[key] <= 1000:
                self._send_json({"ok": False, "error": f"{key} out of range"}, 400)
                return
        if not self.ssh:
            self._send_json({"ok": False, "error": "no session"}, 400)
            return
        self._send_json({"ok": self.ssh.resize(name, dims["cols"], dims["rows"])})

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
        they then have to hunt for. ``format`` is the file the connected
        stack's users exchange: QGroundControl's ``.params`` for PX4, Mission
        Planner's ``.param`` for ArduPilot. Always 200.
        """
        fmt = self._param_file_info()["format"]
        self._send_json({
            "dir": _params_export_dir(self._live_config()),
            "filename": _default_params_filename(
                self._vehicle_tag(), param_files.PARAM_FILE_SUFFIX[fmt]),
            "format": fmt,
            "formats": [
                {"id": key, "suffix": suffix}
                for key, suffix in param_files.PARAM_FILE_SUFFIX.items()
            ],
        })

    def _param_file_info(self) -> dict[str, Any]:
        """The connected vehicle's parameter file details, or PX4's without one."""
        info = None
        if self.mavlink is not None and hasattr(self.mavlink, "param_file_info"):
            try:
                info = self.mavlink.param_file_info()
            except Exception:  # noqa: BLE001 - an export must never 500 on a header
                logger.debug("param_file_info failed", exc_info=True)
        info = dict(info or {})
        if info.get("format") not in param_files.PARAM_FILE_SUFFIX:
            info["format"] = autopilot.dialect_for_stack(
                autopilot.STACK_PX4).param_file_format
        return info

    def _vehicle_tag(self) -> str:
        """Short identifier for the connected vehicle, or "" when unknown.

        Used only to make an exported filename recognizable ("...px4-quad...").
        Never fails: no telemetry, no tag.
        """
        try:
            state = self.store.get_snapshot() if self.store is not None else {}
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
        ``format`` is ``qgc``, ``mission-planner`` or ``json`` (the default,
        for a caller that predates the choice); the filename's suffix follows
        it. See :mod:`corvus.param_files`.
        """
        params = payload.get("params")
        if not isinstance(params, list) or not params:
            self._send_json({"ok": False, "error": "params must be a non-empty list"}, 400)
            return
        fmt = payload.get("format", param_files.FORMAT_JSON)
        if fmt not in param_files.PARAM_FILE_SUFFIX:
            self._send_json({"ok": False, "error": f"unknown format {fmt!r}"}, 400)
            return
        for index, entry in enumerate(params):
            value = entry.get("value") if isinstance(entry, dict) else None
            name = entry.get("name") if isinstance(entry, dict) else None
            if (not isinstance(name, str) or not name or isinstance(value, bool)
                    or not isinstance(value, (int, float))):
                self._send_json(
                    {"ok": False, "error": f"invalid parameter at index {index}"}, 400)
                return

        raw_dir = payload.get("dir")
        target_dir = raw_dir if isinstance(raw_dir, str) and raw_dir.strip() else \
            _params_export_dir(self._live_config())
        target_dir = os.path.expanduser(target_dir.strip())

        suffix = param_files.PARAM_FILE_SUFFIX[fmt]
        filename = _safe_filename(
            payload.get("filename"),
            _default_params_filename(self._vehicle_tag(), suffix), suffix)

        exported_at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        if fmt == param_files.FORMAT_QGC:
            info = self._param_file_info()
            text = param_files.write_qgc(
                params,
                system_id=int(info.get("system_id") or 1),
                component_id=int(info.get("component_id") or 1),
                stack=str(info.get("stack") or ""),
                vehicle=str(info.get("vehicle") or ""),
                version=str(info.get("version") or ""),
                git_hash=str(info.get("git_hash") or ""),
            )
        elif fmt == param_files.FORMAT_MISSION_PLANNER:
            vehicle = self._vehicle_tag()
            text = param_files.write_mission_planner(params, header="\n".join((
                f"Corvus GCS {get_version()} parameter export",
                f"Exported {exported_at}" + (f", vehicle {vehicle}" if vehicle else ""),
            )))
        else:
            text = param_files.write_json({
                "product": "Corvus GCS",
                "version": get_version(),
                "exported_at": exported_at,
                "vehicle": self._vehicle_tag(),
                "param_count": len(params),
                "params": params,
            })
        try:
            os.makedirs(target_dir, exist_ok=True)
            path = os.path.join(target_dir, filename)
            # Atomic: a half-written parameter file is worse than none, because
            # it looks importable. Same temp-then-replace shape as save_config.
            fd, tmp_name = tempfile.mkstemp(prefix=filename + ".", suffix=".tmp", dir=target_dir)
            try:
                with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as f:
                    f.write(text)
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
            "filename": filename, "param_count": len(params), "format": fmt,
        })

    @route("GET", "/api/params/metadata")
    def _api_params_metadata(self) -> None:
        """The vehicle's parameter metadata: defaults, and on PX4 descriptions.

        ``{"state","error","received","size","count"}``; ``state`` is idle,
        loading, ready or unavailable, and ``params`` (name -> metadata) comes
        with ready. Read from the vehicle over MAVLink FTP once
        ``POST /api/params/metadata`` asked for it; see
        :mod:`corvus.param_metadata`. Always 200.
        """
        status = getattr(self.mavlink, "param_metadata_status", None)
        if status is None:
            self._send_json({"state": "idle", "error": "not connected",
                             "received": 0, "size": 0, "count": 0})
            return
        self._send_json(status(include_params=True))

    @route("POST", "/api/params/metadata")
    def _api_params_metadata_fetch(self, payload: dict) -> None:
        """Start reading the parameter metadata from the vehicle in the background.

        Idempotent: a fetch already running or finished for this link is left
        alone, a failed one is tried again. The answer is the status, without
        the metadata itself; poll ``GET /api/params/metadata`` for that.
        """
        start = getattr(self.mavlink, "start_param_metadata", None)
        if start is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        cache = self._param_metadata_cache() if self._cache_param_defaults() else None
        self._send_json(dict(start(cache=cache), ok=True))

    def _cache_param_defaults(self) -> bool:
        """Has the operator switched on the copy of the parameter metadata?"""
        return bool((self._live_config().parameters or {}).get("cache_defaults"))

    def _param_metadata_cache(self) -> param_metadata.ParamMetadataCache:
        return param_metadata.ParamMetadataCache(_param_metadata_cache_dir())

    @route("GET", "/api/params/metadata/cache")
    def _api_params_metadata_cache(self) -> None:
        """``{"enabled","dir","files","bytes"}``: the copies the switch keeps."""
        info = self._param_metadata_cache().info()
        self._send_json(dict(info, enabled=self._cache_param_defaults()))

    @route("POST", "/api/params/metadata/cache/clear")
    def _api_params_metadata_cache_clear(self, payload: dict) -> None:
        """Delete every kept copy; the next editor reads the file from the vehicle."""
        cache = self._param_metadata_cache()
        removed = cache.clear()
        self._send_json(dict(cache.info(), ok=True, removed=removed,
                             enabled=self._cache_param_defaults()))

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
            normalized_error = error.lower()
            status = 503 if (
                "disconnected" in normalized_error or "not connected" in normalized_error
            ) else 409
            self._send_json({"ok": False, "error": error}, status)

    @route("POST", "/api/params/verify")
    def _api_params_verify(self, payload: dict) -> None:
        """Read parameters back from the vehicle and write again what did not stick.

        Body: ``{"params": [{"name", "value"}, ...], "names": [...]}``.
        ``params`` are the values the operator set and wants on the vehicle
        (may be empty); ``names`` are further parameters to read back so a
        page can redraw itself from what the vehicle really holds. Every name
        is read fresh, never from the cache. See
        :meth:`corvus.mavlink_bridge.MavlinkBridge.verify_params` for the
        answer, which comes back with ``ok: true`` whenever the check ran,
        whatever it found: ``all_confirmed`` is the verdict.
        """
        params = payload.get("params", [])
        names = payload.get("names", [])
        if not isinstance(params, list) or not isinstance(names, list):
            self._send_json(
                {"ok": False, "error": "params and names must be lists"}, 400)
            return
        if any(not isinstance(n, str) or not n for n in names):
            self._send_json(
                {"ok": False, "error": "names must be non-empty strings"}, 400)
            return
        clean: list[dict[str, Any]] = []
        for index, entry in enumerate(params):
            name = entry.get("name") if isinstance(entry, dict) else None
            value = entry.get("value") if isinstance(entry, dict) else None
            if not isinstance(name, str) or not name:
                self._send_json(
                    {"ok": False, "error": f"invalid parameter at index {index}: name must be a non-empty string"},
                    400,
                )
                return
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                self._send_json(
                    {"ok": False, "error": f"invalid parameter at index {index}: value must be a number"},
                    400,
                )
                return
            clean.append({"name": name, "value": float(value)})
        verify = getattr(self.mavlink, "verify_params", None)
        if self.mavlink is None or verify is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        result = verify(clean, names)
        if result is None:
            error = self.mavlink.get_last_command_error() or "check failed"
            status = 503 if "not connected" in error else 400
            self._send_json({"ok": False, "error": error}, status)
            return
        self._send_json(dict(result, ok=True))

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

    @route("POST", "/api/calibrate/position")
    def _api_calibrate_position(self, payload: dict) -> None:
        """Confirm the aircraft is in the position the autopilot asked for.

        PX4 recognises each of the six accelerometer orientations by itself, so
        on a PX4 link this is refused with an explanation rather than sent.
        ArduPilot waits to be told, indefinitely, which is why the endpoint
        exists at all.
        """
        raw = payload.get("position", "")
        position = raw.strip().lower() if isinstance(raw, str) else ""
        if position not in autopilot.ACCELCAL_POSITIONS:
            self._send_json({"ok": False, "error": "unknown position"}, 400)
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if self.mavlink.accel_calibration_position(position):
            self._send_json({"ok": True})
            return
        error = self.mavlink.get_last_command_error() or "position failed"
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
        """Start or stop the autotune, however the connected stack runs one.

        ``{"axis": "all"}`` starts it; ``{"axis": "all", "enabled": false}``
        stops one that is running. Starting is refused unless the vehicle is
        armed and airborne — the tune injects steps into the rate controller
        and neither stack will run it on the ground. PX4 runs it as a command;
        ArduPilot runs it as a flight mode, which the bridge handles.
        """
        raw = payload.get("axis", "")
        axis = raw.strip().lower() if isinstance(raw, str) else ""
        if axis not in AUTOTUNE_AXES:
            self._send_json({
                "ok": False,
                "error": "full autotune only; use axis 'all'",
            }, 400)
            return
        enabled = payload.get("enabled", True)
        if not isinstance(enabled, bool):
            self._send_json({"ok": False, "error": "enabled must be boolean"}, 400)
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        ok = self.mavlink.autotune(axis, enable=enabled)
        if ok:
            self._send_json({"ok": True})
        else:
            error = self.mavlink.get_last_command_error() or "autotune failed"
            normalized_error = error.lower()
            status = 503 if (
                "disconnected" in normalized_error or "not connected" in normalized_error
            ) else 409
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

    @route("POST", "/api/tuning/stream")
    def _api_tuning_stream(self, payload: dict) -> None:
        """Enable/disable the high-rate controller-setpoint stream on demand.

        Read-only stream-rate control, and safe while armed for the same reason
        the vibration one is — no command and no parameter write leaves the
        GCS. Unlike that one it is *used* while armed: the PID tuning page it
        feeds is opened in flight, which is the only time a setpoint trace has
        anything to show.

        The frontend toggles this when the tuning page opens and closes, so the
        rest of the time PX4 streams these messages at its own default rate.
        """
        enabled = payload.get("enabled")
        if not isinstance(enabled, bool):
            self._send_json({"ok": False, "error": "enabled must be boolean"}, 400)
            return
        raw_rate = payload.get("rate_hz", 20)
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
        ok = self.mavlink.set_tuning_stream(enabled, rate_hz)
        if ok:
            self._send_json({"ok": True, "enabled": enabled, "rate_hz": rate_hz})
        else:
            error = self.mavlink.get_last_command_error() or "tuning stream request failed"
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
        if len(raw) != length:
            self.close_connection = True
            self._send_json({"ok": False, "error": "firmware upload was cut short"}, 400)
            return
        ok = self.flash.start(raw)
        if ok:
            self._send_json({"ok": True, "state": "flashing"})
        else:
            err = getattr(self.flash, "last_error", "") or "flash refused"
            self._send_json({"ok": False, "error": err}, 409)


    # ---- SiK telemetry radio ----
    #
    # These five are the only endpoints in the server that configure something
    # other than the autopilot. A SiK radio holds its own EEPROM and is reached
    # over AT commands on a serial port rather than over MAVLink (see
    # corvus/sik_config.py for what the settings mean and corvus/sik_service.py
    # for how the port is borrowed and given back).
    #
    # All four of the acting endpoints are POST, including the read. That is not
    # an oversight: loading settings takes the serial port away from the MAVLink
    # bridge for a second or two and puts a radio into command mode, which is a
    # side effect on the aircraft's telemetry link and has no business behind a
    # method a browser may retry, prefetch or cache.

    @route("GET", "/api/sik/status")
    def _api_sik_status(self) -> None:
        """Ports, the live link's own port, and whether a session may run.

        Always 200 so the page can poll it: an absent service is reported as one
        that cannot configure, with the reason in ``blocked_reason``.
        """
        if self.sik is None:
            self._send_json({
                "ports": [], "link_device": "", "link_baud": sik_config.DEFAULT_BAUD,
                "transport": "unknown", "armed": False, "busy": False,
                "can_configure": False,
                "blocked_reason": "radio configuration is unavailable",
                "default_baud": sik_config.DEFAULT_BAUD,
                "bauds": list(sik_config.BAUD_CANDIDATES),
                "schema": sik_config.schema(),
            })
            return
        status = self.sik.status()
        # The register table travels with the status so the page can render its
        # form, option lists and help text before a radio has been read — and so
        # the labels live in exactly one place rather than being restated in JS.
        status["schema"] = sik_config.schema()
        self._send_json(status)

    def _sik_target(self, payload: dict) -> tuple[str, int | None] | None:
        """Pull ``device`` and optional ``baud`` out of a request body.

        Returns ``None`` after sending the error response, so every caller is a
        two-line guard. A baud that is not a positive integer is refused rather
        than defaulted: silently substituting 57600 for a typo would open the
        port, fail the escape, and report the failure against a number the
        operator never chose.
        """
        device = payload.get("device")
        if not isinstance(device, str) or not device.strip():
            self._send_json({"ok": False, "error": "device is required"}, 400)
            return None
        raw = payload.get("baud")
        if raw is None:
            return device.strip(), None
        if isinstance(raw, bool) or not isinstance(raw, int) or raw <= 0:
            self._send_json({"ok": False, "error": "baud must be a positive integer"}, 400)
            return None
        return device.strip(), raw

    def _sik_run(self, call: Any) -> None:
        """Run one session and map its outcome onto a status code.

        409 rather than 500 for a :class:`SikError`: every one of them is a
        condition the operator can do something about — a radio that is not
        powered, a port something else has open, a vehicle that is armed — and
        none of them is the server failing.
        """
        if self.sik is None:
            self._send_json({"ok": False, "error": "radio configuration unavailable"}, 503)
            return
        try:
            result = call()
        except sik_config.SikConfigError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        except sik_service.SikError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 409)
            return
        except Exception as exc:  # noqa: BLE001 - a serial layer raises broadly
            logger.exception("SiK radio session failed")
            self._send_json({"ok": False, "error": f"radio session failed: {exc}"}, 500)
            return
        result["ok"] = True
        self._send_json(result)

    @route("POST", "/api/sik/load")
    def _api_sik_load(self, payload: dict) -> None:
        """Read both radios and report where the pair disagrees.

        ``remote`` may be sent false to skip the far radio, which is what a
        bench session with only one radio plugged in wants: the remote read is
        the slow part of a load, and waiting six seconds for a partner that is
        known to be switched off is time spent learning nothing.
        """
        target = self._sik_target(payload)
        if target is None:
            return
        device, baud = target
        include_remote = payload.get("remote", True)
        if not isinstance(include_remote, bool):
            self._send_json({"ok": False, "error": "remote must be boolean"}, 400)
            return
        self._sik_run(lambda: self.sik.load(device, baud, include_remote=include_remote))

    @route("POST", "/api/sik/save")
    def _api_sik_save(self, payload: dict) -> None:
        """Write settings to either radio, far end first.

        ``local`` and ``remote`` are each ``{REGISTER_NAME: value}`` and both are
        optional; what is not sent is not written. Validation, the write order
        and the commit-and-reboot sequence all live in the service and the
        schema module — see :meth:`corvus.sik_service.SikService.save` for why
        the remote radio is written before the one on the cable.
        """
        target = self._sik_target(payload)
        if target is None:
            return
        device, baud = target
        local = payload.get("local")
        remote = payload.get("remote")
        for name, value in (("local", local), ("remote", remote)):
            if value is not None and not isinstance(value, dict):
                self._send_json({"ok": False, "error": f"{name} must be an object"}, 400)
                return
        if not local and not remote:
            self._send_json({"ok": False, "error": "nothing to write"}, 400)
            return
        self._sik_run(lambda: self.sik.save(device, baud, local=local, remote=remote))

    @route("POST", "/api/sik/reset")
    def _api_sik_reset(self, payload: dict) -> None:
        """Factory-default one end of the link (``AT&F`` / ``RT&F``)."""
        target = self._sik_target(payload)
        if target is None:
            return
        device, baud = target
        which = payload.get("target", "local")
        if which not in {"local", "remote"}:
            self._send_json({"ok": False, "error": "target must be 'local' or 'remote'"}, 400)
            return
        self._sik_run(lambda: self.sik.reset(device, baud, target=which))

    # ---- RTK base station ----
    #
    # Three endpoints, and only one of them acts on hardware. The split is the
    # same one the rest of the app uses: a GET that a page may poll freely, and
    # POSTs for the two things that restart a session — which costs a running
    # survey, and is therefore never something a browser should be able to do
    # by prefetching a link.
    #
    # The settings live in the config file rather than on the vehicle because
    # none of them is a vehicle setting: where the corrections come from is a
    # property of the ground station, and the aircraft is told nothing about it
    # beyond the corrections themselves.




    # ---- Programs on this computer (launcher buttons) ----
    def _local_only(self) -> bool:
        """Refuse, and say why, unless the request came from this computer.

        Running a program here is nothing the operator cannot already do on
        their own laptop, and a process on it already runs as them. It is not
        something another machine may do: CORVUS_BIND can open the rest of the
        API to the flight-line network (whoever reaches it can fly the
        aircraft), but never this. Cross-site pages are already refused by
        _mutating_request_allowed, and a rebound DNS name by the Host check.
        """
        client = getattr(self, "client_address", None)
        host = str(client[0]) if client else ""
        if host in ("127.0.0.1", "::1", "::ffff:127.0.0.1") or host.startswith("127."):
            return True
        self._send_json({"ok": False, "error":
                         "Programs on this computer can only be started from this computer."}, 403)
        return False

    @route("GET", "/api/local/status")
    def _api_local_status(self) -> None:
        """Whether a terminal on this computer is possible here, and why not."""
        caps = local_shell.capabilities()
        caps["background"] = self.local is not None
        caps["running"] = self.local.count() if self.local is not None else 0
        self._send_json(caps)

    @route("POST", "/api/local/connect")
    def _api_local_connect(self, payload: dict) -> None:
        """Open (or replace) a shell on this computer under ``name``.

        ``{name, directory?}``. It becomes a session like an SSH one, so
        ``/api/ssh/send``, ``/resize``, ``/stream``, ``/disconnect`` and the
        terminal windows all work on it by that name.
        """
        if not self._local_only():
            return
        name = payload.get("name")
        directory = payload.get("directory", "")
        if not isinstance(name, str) or not name.strip() or len(name) > 128:
            self._send_json({"ok": False, "error": "name must be a non-empty string"}, 400)
            return
        if not isinstance(directory, str):
            directory = ""
        if self.ssh is None:
            self._send_json({"ok": False, "connected": False, "error": "terminals are unavailable"}, 503)
            return
        connected = self.ssh.connect_local(name, directory)
        answer: dict[str, Any] = {"ok": connected, "connected": connected, "name": name}
        if not connected:
            answer["error"] = self.ssh.connect_error(name) or "The shell could not be started."
        self._send_json(answer)

    @route("POST", "/api/local/run")
    def _api_local_run(self, payload: dict) -> None:
        """Start a program on this computer without a window.

        ``{command, directory?}``. Answers at once when it is still running
        (``{ok, pid}``), or with its exit code and last output when it ended
        within a second (``{ok, exited, code, output}``). It is stopped when
        Corvus closes.
        """
        if not self._local_only():
            return
        command = payload.get("command")
        directory = payload.get("directory", "")
        if not isinstance(command, str) or not isinstance(directory, str):
            self._send_json({"ok": False, "error": "command and directory must be strings"}, 400)
            return
        if self.local is None:
            self._send_json({"ok": False, "error": "Programs on this computer are unavailable."}, 503)
            return
        result = self.local.run(command, directory)
        result["command"] = command
        self._send_json(result)

    @route("POST", "/api/local/ping")
    def _api_local_ping(self, payload: dict) -> None:
        """Send one ICMP echo from this computer and say whether it came back.

        ``{host, wait_ms?}`` -> ``{ok, host, reachable, rtt_ms}``. One probe per
        request; the caller decides how often to ask and when silence means
        offline. POST, and only from this computer: it starts a process, and
        another machine has no business mapping the flight-line network
        through this one.
        """
        if not self._local_only():
            return
        host = payload.get("host")
        wait_ms = payload.get("wait_ms", 2000)
        if not isinstance(host, str) or isinstance(wait_ms, bool) \
                or not isinstance(wait_ms, (int, float)) or not math.isfinite(wait_ms):
            self._send_json({"ok": False, "error": "host must be a string and wait_ms a number"}, 400)
            return
        _, problem = net_probe.clean_host(host)
        if problem:
            self._send_json({"ok": False, "error": problem}, 400)
            return
        if self.pinger is None:
            self._send_json({"ok": False, "error": "Pinging is unavailable in this build."}, 503)
            return
        self._send_json(self.pinger.ping(host, wait_ms / 1000.0))





    def _apply_video(self, stored: dict[str, Any]) -> None:
        if self.video is not None:
            self.video.apply_settings(stored)






    # ---- Second-station MAVLink forwarding ----
    @route("GET", "/api/forwarding")
    def _api_forwarding_status(self) -> None:
        """Where to point QGroundControl, and whether anything is listening."""
        forwarder = getattr(self, "forwarder", None)
        if forwarder is None:
            cfg = getattr(self.config, "forwarding", None) or {}
            listen_port = cfg.get("listen_port")
            if not isinstance(listen_port, int) or isinstance(listen_port, bool):
                listen_port = _FORWARD_DEFAULT_LISTEN_PORT
            self._send_json({
                "running": False,
                "host": cfg.get("host") or _FORWARD_DEFAULT_HOST,
                "port": cfg.get("port") or _FORWARD_DEFAULT_PORT,
                "listen_host": cfg.get("listen_host") or _FORWARD_DEFAULT_LISTEN_HOST,
                "listen_port": listen_port,
                "listen_port_requested": listen_port,
                "allow_commands": bool(cfg.get("allow_commands")),
                "endpoints": list(cfg.get("endpoints") or []),
                "peers": [], "targets": [], "frames_sent": 0,
                "datagrams_received": 0,
                "frames_injected": 0, "dropped": 0, "sysid_conflict": False,
                "error": "",
            })
            return
        self._send_json(forwarder.status())

    def _restart_forwarder(self) -> Any:
        """Stop the running forwarder and start the one the live config says.

        Called with ``_config_write_lock`` held, for the reason given in
        ``_api_forwarding_set``. Returns the new forwarder, or None when
        forwarding is off.
        """
        old = getattr(self, "forwarder", None)
        if old is not None:
            try:
                if self.mavlink is not None:
                    self.mavlink.set_frame_sink(None)
                old.stop()
            except Exception:  # noqa: BLE001 - a stuck old forwarder never blocks the new one
                logger.exception("stopping the previous forwarder failed")
        forwarder = _build_forwarder(self.mavlink, self.config) if self.mavlink else None
        type(self).forwarder = forwarder
        server = getattr(self, "server", None)
        if server is not None:
            server.forwarder = forwarder
        return forwarder

    @route("POST", "/api/forwarding")
    def _api_forwarding_set(self, payload: dict) -> None:
        """Turn forwarding on/off and persist the choice.

        ``allow_commands`` decides whether another ground station may command
        this aircraft, so it is accepted only as a real boolean and is stored
        exactly as sent — never inferred from ``enabled``. Restarting the
        forwarder rather than mutating it in place keeps one rule: what is
        running is what the saved config says.
        """
        for key in ("enabled", "allow_commands"):
            if key in payload and not isinstance(payload[key], bool):
                self._send_json({"ok": False, "error": f"{key} must be true or false"}, 400)
                return
        port = payload.get("port", None)
        if port is not None:
            if isinstance(port, bool) or not isinstance(port, int) or not (0 < port < 65536):
                self._send_json({"ok": False, "error": "port must be 1-65535"}, 400)
                return
        # 0 is a real answer here — "bind any free port" — so the range starts
        # one lower than the target port's.
        listen_port = payload.get("listen_port", None)
        if listen_port is not None:
            if (isinstance(listen_port, bool) or not isinstance(listen_port, int)
                    or not (0 <= listen_port < 65536)):
                self._send_json(
                    {"ok": False, "error": "listen_port must be 0-65535"}, 400)
                return
        endpoints = payload.get("endpoints", None)
        if endpoints is not None:
            if not isinstance(endpoints, list) or any(
                    not isinstance(e, str) for e in endpoints):
                self._send_json({"ok": False, "error": "endpoints must be strings"}, 400)
                return
        if self.config is None:
            self._send_json({"ok": False, "error": "no config available"}, 503)
            return

        # Stop-old-then-start-new, under the config lock. Without it two
        # overlapping requests (a double-clicked toggle, or two Settings tabs)
        # both read the same live forwarder, both stop it, and both then try to
        # bind the same UDP port. One bind wins and one fails — but the last
        # writer to reach ``type(self).forwarder`` decides which object the app
        # keeps a reference to. Lose that coin flip and the *running* forwarder
        # becomes unreachable while its socket and threads stay alive: the UI
        # reports forwarding as stopped, and the port it needs to restart on is
        # held by an object nothing can stop short of quitting Corvus.
        with _config_write_lock:
            current = dict(getattr(self.config, "forwarding", None) or {})
            for key in ("enabled", "allow_commands"):
                if key in payload:
                    current[key] = payload[key]
            if port is not None:
                current["port"] = port
            if listen_port is not None:
                current["listen_port"] = listen_port
            for key in ("host", "listen_host"):
                if isinstance(payload.get(key), str) and payload[key].strip():
                    current[key] = payload[key].strip()
            if endpoints is not None:
                current["endpoints"] = [e.strip() for e in endpoints if e.strip()]
            self.config.forwarding = current
            forwarder = self._restart_forwarder()

            try:
                save_config(self.config, self.config_path or default_config_path())
            except Exception:  # noqa: BLE001 - it still runs this session
                logger.exception("could not persist forwarding config")
                status = forwarder.status() if forwarder is not None else {"running": False}
                self._send_json({"ok": True, "status": status,
                                 "warning": "set for this session but not saved"})
                return
        if forwarder is not None and not forwarder.status()["running"]:
            self._send_json({"ok": False, "error": forwarder.error or "could not start",
                             "status": forwarder.status()}, 409)
            return
        self._send_json({
            "ok": True,
            "status": forwarder.status() if forwarder is not None else {
                "running": False, "allow_commands": bool(current.get("allow_commands")),
            },
        })







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
            data = review_bytes(raw, name, self._review_sensitivity())
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




    # ---- SSE endpoints ----
    # ---- SSE topics -------------------------------------------------------
    # One definition per topic: what it subscribes to, what its first payload
    # is, and how it buffers. Both the single-topic endpoints below and the
    # multiplexed /api/events stream drive from these, so "what subscribing to
    # params means" is written down once. Splitting that across a per-endpoint
    # copy and a multiplexed copy is the divergence ARCH-1 is about.
    #
    # Each binder takes the buffer to fill and returns (unbind, initial) where
    # *initial* is the payload to send immediately, or None for a topic that
    # has nothing to say until something happens.

    def _bind_console(self, buf: _BoundedSseBuffer, job_id: str | None = None):
        if not self.mavlink:
            return (lambda: None), None
        listener = buf.put_console
        self.mavlink.add_console_sub(listener)
        return (lambda: self.mavlink.remove_console_sub(listener)), None

    def _bind_shell(self, buf: _BoundedSseBuffer, job_id: str | None = None):
        # The initial event is everything the shell printed so far, marked as
        # a replay: a terminal clears itself and draws it, so a stream that
        # reconnects redraws the session instead of printing it twice.
        if not self.mavlink:
            return (lambda: None), {"text": "", "replay": True}
        listener = buf.put_fifo
        backlog = self.mavlink.add_shell_sub(listener)
        return (
            lambda: self.mavlink.remove_shell_sub(listener),
            {"text": backlog, "replay": True},
        )

    def _bind_params(self, buf: _BoundedSseBuffer, job_id: str | None = None):
        if not self.mavlink:
            return (lambda: None), {"state": "idle", "count": 0, "received": 0}
        listener = buf.put_latest
        self.mavlink.add_param_listener(listener)
        status = self.mavlink.param_status()
        initial = _sanitize({
            "state": status["state"],
            "count": status["count"],
            "received": status["received"],
        })
        return (lambda: self.mavlink.remove_param_listener(listener)), initial

    def _bind_firmware(self, buf: _BoundedSseBuffer, job_id: str | None = None):
        if self.flash is None:
            return (lambda: None), None
        listener = buf.put_fifo
        self.flash.add_listener(listener)
        st = self.flash.status()
        initial = _sanitize({
            "state": st["state"], "percent": st["progress"], "message": st["message"],
        })
        return (lambda: self.flash.remove_listener(listener)), initial

    def _bind_tiles(self, buf: _BoundedSseBuffer, job_id: str | None = None):
        # The only topic that is per-object rather than per-server: it follows
        # one download job, and without an id there is nothing to follow.
        if not job_id or self.tile_downloader is None or self.tile_progress_bus is None:
            return (lambda: None), None
        # Subscribe BEFORE reading the status. The other order lost a job that
        # finished in between: the initial event said "running" and the
        # terminal one had already gone to nobody, so the dialog waited forever.
        listener = buf.put_latest
        self.tile_progress_bus.subscribe(job_id, listener)
        status = self.tile_downloader.status(job_id)
        if status is None:
            self.tile_progress_bus.unsubscribe(job_id, listener)
            return (lambda: None), None
        return (
            lambda: self.tile_progress_bus.unsubscribe(job_id, listener),
            _sanitize(status),
        )

    @property
    def _sse_topics(self) -> dict:
        """topic -> (binder, buffer capacity, payload shaper)."""
        return {
            "console": (self._bind_console, CONSOLE_SSE_CAPACITY, lambda e: e),
            "params": (self._bind_params, PARAMS_SSE_CAPACITY, lambda e: {
                "state": e["state"], "count": e["count"], "received": e["received"],
            }),
            "firmware": (self._bind_firmware, FIRMWARE_SSE_CAPACITY, lambda e: _sanitize({
                "state": e.get("state"),
                "percent": e.get("percent"),
                "message": e.get("message"),
            })),
            "tiles": (self._bind_tiles, TILES_PROGRESS_SSE_CAPACITY, _sanitize),
            "shell": (self._bind_shell, SHELL_SSE_CAPACITY, lambda e: e),
        }

    _SSE_TERMINAL_STATES = ("done", "cancelled", "failed")

    def _serve_sse_topics(
        self,
        wanted: dict[str, str],
        job_id: str | None = None,
        close_on: tuple[str, ...] = (),
    ) -> None:
        """Stream *wanted* (topic -> SSE event name) down one connection.

        The whole point of the multiplexed form: a browser caps concurrent
        HTTP/1.1 requests per origin at six, and this app could hold five
        event streams open while the map still wanted that budget for tiles.
        A viewport is dozens of tiles, and they queue one at a time behind the
        streams at exactly the moment — pre-flight setup with a region
        downloading — the operator is busiest.

        Every topic keeps its own buffer and drop policy; only the socket is
        shared. A topic whose service is absent binds to nothing and simply
        never fires, which is why an unavailable service degrades to a quiet
        stream rather than a failed request.

        *close_on* names topics that finish: once one of them reports a
        terminal state the whole stream ends. That is right for a connection
        opened to watch a single job and wrong for a multiplexed one, where a
        finished download must not take the console down with it.
        """
        table = self._sse_topics
        mux = _MultiplexSseBuffer()
        unbinds: list = []
        initials: list[tuple[str, Any]] = []
        for topic in wanted:
            binder, capacity, _shape = table[topic]
            buf = mux.topic(topic, capacity)
            unbind, initial = binder(buf, job_id)
            unbinds.append(unbind)
            if initial is not None:
                initials.append((topic, initial))
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self._send_cors()
        self.end_headers()
        try:
            for topic, initial in initials:
                self._send_sse(wanted[topic], json.dumps(initial))
            while True:
                try:
                    finished = False
                    for topic, entry in mux.drain(timeout=15):
                        _binder, _capacity, shape = table[topic]
                        self._send_sse(wanted[topic], json.dumps(shape(entry)))
                        if topic in close_on and isinstance(entry, dict):
                            finished = entry.get("state") in self._SSE_TERMINAL_STATES
                    if finished:
                        break
                except queue.Empty:
                    if self._sse_stopping():
                        break
                    self._send_sse("ping", "{}")
        except CLIENT_GONE_ERRORS:
            pass
        finally:
            for unbind in unbinds:
                try:
                    unbind()
                except Exception:  # noqa: BLE001 - teardown must not raise
                    logger.exception("SSE topic unbind failed")

    @route("GET", "/api/events")
    def _sse_events(self) -> None:
        """One stream carrying any of console, params, firmware, tiles and shell.

        ``?topics=console,params`` selects them; ``?job=<id>`` is the download
        job the ``tiles`` topic follows. Unknown names are ignored rather than
        rejected, so a newer frontend asking an older backend for a topic it
        has never heard of gets the rest of what it asked for instead of a
        400 and no stream at all.

        Each topic arrives under its own event name, which is what lets one
        connection replace four: the single-topic endpoints all call theirs
        ``progress``, and three ``progress`` streams down one socket would be
        indistinguishable.

        Telemetry is deliberately NOT here. It is the 50 Hz path with its own
        version-keyed snapshot cache, it is open for the whole session, and it
        is the one stream whose stutter an operator would read as the aircraft
        misbehaving. SSH keeps its own for the same reason in reverse: it is
        per-session, high-volume, and ends when its shell does.
        """
        params = parse_qs(urlparse(self.path).query)
        asked = [t.strip() for t in (params.get("topics", [""])[0] or "").split(",")]
        table = self._sse_topics
        wanted = {t: t for t in asked if t in table}
        if not wanted:
            self._send_json({"error": "no known topics requested"}, 400)
            return
        self._serve_sse_topics(wanted, job_id=params.get("job", [None])[0])

    @route("GET", "/api/telemetry")
    def _sse_telemetry(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self._send_cors()
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
                    if self._sse_stopping():
                        break
                    self._send_sse("ping", "{}")
        except CLIENT_GONE_ERRORS:
            pass
        finally:
            if self.store:
                self.store.remove_listener(listener)


    @route("GET", "/api/params/progress")
    def _sse_params(self) -> None:
        """Parameter-download progress as its own stream (latest-wins).

        See :meth:`_sse_console`: the frontend uses
        ``/api/events?topics=params``; this stays as the published
        single-topic form, over the same binding.
        """
        self._serve_sse_topics({"params": "progress"})

    @route("GET", "/api/ssh/stream")
    def _sse_ssh_stream(self) -> None:
        """Stream one session's shell output verbatim.

        ``?name=`` picks the session; without it the first connected one is
        used (the single-session case, and what older clients sent). Naming it
        matters once two connections are open at once: the stream would
        otherwise attach to whichever session the bridge happened to list
        first, and the terminal would show another machine's output.
        """
        params = parse_qs(urlparse(self.path).query)
        wanted = (params.get("name") or [""])[0]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self._send_cors()
        self.end_headers()
        q = _BoundedSseBuffer(SSH_SSE_CAPACITY)
        listener = q.put_fifo
        attached_name: str | None = None
        if self.ssh:
            sessions = self.ssh.list_sessions()
            for s in sessions:
                if not s["connected"]:
                    continue
                if wanted and s["name"] != wanted:
                    continue
                attached_name = s["name"]
                session = self.ssh.get_session(s["name"])
                if session:
                    session.add_sub(listener)
                break
        # Poll once a second rather than blocking for the full ping interval:
        # that is what lets the stream notice the shell has ended, tell the
        # client, and let this thread go. Blocking on a 15s get left one
        # handler thread per closed session alive for the life of the page.
        if attached_name is None:
            # Nothing to attach to: say so and let the socket go rather than
            # holding a thread open pinging an empty stream.
            try:
                self._send_sse("closed", json.dumps({"name": wanted or None}))
            except CLIENT_GONE_ERRORS:
                pass
            return
        idle = 0.0
        try:
            while True:
                try:
                    text = q.get(timeout=1.0)
                    idle = 0.0
                    self._send_sse("output", json.dumps({"text": text, "name": attached_name}))
                    continue
                except queue.Empty:
                    pass
                if self._sse_stopping():
                    break
                if attached_name and self.ssh is not None:
                    live = self.ssh.get_session(attached_name)
                    if live is None or not live.connected:
                        self._send_sse("closed", json.dumps({"name": attached_name}))
                        break
                idle += 1.0
                if idle >= 15.0:
                    idle = 0.0
                    self._send_sse("ping", "{}")
        except CLIENT_GONE_ERRORS:
            pass
        finally:
            if self.ssh and attached_name:
                session = self.ssh.get_session(attached_name)
                if session:
                    session.remove_sub(listener)

    @route("GET", "/api/ssh/ws")
    def _ws_ssh(self) -> None:
        """One terminal over a WebSocket: output out, keystrokes and size in.

        The same as ``/api/ssh/stream`` with ``/api/ssh/send`` and
        ``/api/ssh/resize``, on one connection that is not one of the six a
        browser allows per host (see :mod:`corvus.websocket` for why that
        matters). JSON messages: ``{"type": "output", "text"}`` and
        ``{"type": "closed", "name"}`` to the browser, ``{"type": "input",
        "data"}`` and ``{"type": "resize", "cols", "rows"}`` from it.

        Input writes to a shell, so the opening request is held to what the
        POSTs are held to: a page on another site is refused before the
        upgrade. ``?name=`` is required; a session that is not there, or not
        connected, is answered with ``closed`` right after the upgrade, so
        the terminal can tell it apart from a server that has no WebSocket.
        """
        if not self._mutating_request_allowed():
            return
        problem = websocket.handshake_problem(self.headers)
        if problem:
            self._send_json({"ok": False, "error": problem}, 400)
            return
        name = (parse_qs(urlparse(self.path).query).get("name") or [""])[0]
        self.close_connection = True
        self._response_started = True
        self.wfile.write(websocket.handshake_response(self.headers.get("Sec-WebSocket-Key", "")))
        self.wfile.flush()
        ws = websocket.WebSocket(self.connection)

        session = self.ssh.get_session(name) if (self.ssh is not None and name) else None
        if session is None or not session.connected:
            try:
                ws.send_text(json.dumps({"type": "closed", "name": name or None}))
            except websocket.Closed:
                pass
            ws.close()
            return

        q = _BoundedSseBuffer(SSH_SSE_CAPACITY)
        listener = q.put_fifo
        done = threading.Event()
        session.add_sub(listener)
        writer = threading.Thread(
            target=self._ws_ssh_writer, args=(ws, q, name, done),
            name=f"ssh-ws-{name}", daemon=True)
        writer.start()
        try:
            while not done.is_set() and not self._sse_stopping():
                try:
                    text = ws.receive(1.0)
                except websocket.Closed:
                    break
                if text is not None:
                    self._ws_ssh_input(name, text)
        finally:
            done.set()
            session.remove_sub(listener)
            writer.join(2.0)
            ws.close(websocket.CLOSE_GOING_AWAY if self._sse_stopping() else websocket.CLOSE_NORMAL)

    def _ws_ssh_writer(self, ws: websocket.WebSocket, q: _BoundedSseBuffer,
                       name: str, done: threading.Event) -> None:
        """Send a session's output as it comes; say so when the session ends."""
        idle = 0.0
        try:
            while not done.is_set():
                try:
                    text = q.get(timeout=1.0)
                    idle = 0.0
                    ws.send_text(json.dumps({"type": "output", "text": text}))
                    continue
                except queue.Empty:
                    pass
                if self._sse_stopping():
                    break
                live = self.ssh.get_session(name) if self.ssh is not None else None
                if live is None or not live.connected:
                    # What the shell printed on its way out comes before the end.
                    for text in q.drain():
                        ws.send_text(json.dumps({"type": "output", "text": text}))
                    ws.send_text(json.dumps({"type": "closed", "name": name}))
                    break
                idle += 1.0
                if idle >= 15.0:
                    idle = 0.0
                    ws.ping()
        except websocket.Closed:
            pass
        finally:
            done.set()

    def _ws_ssh_input(self, name: str, text: str) -> None:
        """Apply one message from a terminal. Anything malformed is ignored."""
        try:
            msg = json.loads(text)
        except ValueError:
            return
        if not isinstance(msg, dict) or self.ssh is None:
            return
        kind = msg.get("type")
        if kind == "input":
            data = msg.get("data")
            if isinstance(data, str) and data:
                self.ssh.send(name, data)
        elif kind == "resize":
            dims = [msg.get("cols"), msg.get("rows")]
            if all(isinstance(v, int) and not isinstance(v, bool) and 1 <= v <= 1000 for v in dims):
                self.ssh.resize(name, dims[0], dims[1])


    # ---- Place search ----
    @route("GET", "/api/geocode")
    def _api_geocode(self) -> None:
        """Places matching ``?q=``, for the map's search box.

        Always 200 with ``{ok, results, error}``. The distinction that matters
        to the operator is not HTTP's: "found nothing" and "could not look"
        are both ordinary outcomes in the field, and the box says something
        different for each. A status code would collapse them into one red
        line — and the frontend answers a coordinate pair itself, without ever
        asking here, so this endpoint being unreachable is not the search
        being unavailable.

        ``?near=<lon>,<lat>`` biases the ranking toward the map's centre. It
        is a preference, never a filter: a search for somewhere far away still
        finds it.
        """
        params = parse_qs(urlparse(self.path).query)
        query = (params.get("q", [""])[0] or "").strip()
        if not query:
            self._send_json({"ok": False, "results": [], "error": "q is required"}, 400)
            return
        if self.geocoder is None:
            self._send_json({
                "ok": False, "results": [],
                "error": "Place search is not available in this build.",
            })
            return
        limit = _int_or(params.get("limit", [None])[0], geocode.DEFAULT_RESULTS)
        near = _parse_near(params.get("near", [""])[0])
        try:
            results = self.geocoder.search(query, limit=limit, near=near)
        except geocode.GeocodeError as exc:
            self._send_json({"ok": False, "results": [], "error": str(exc)})
            return
        except Exception:  # noqa: BLE001 - a search must never 500 the app
            logger.exception("place search failed for %r", query)
            self._send_json({
                "ok": False, "results": [], "error": "The place search failed.",
            })
            return
        self._send_json({"ok": True, "results": results, "error": ""})













    def _api_tiles_serve(self, source: str, z: int, x: int, y: int) -> None:
        """Serve a cached tile, transparently fetching+ caching from upstream.

        Online: a cache miss is filled from the upstream template (short
        timeout) then stored, so the map works and the cache grows in the
        field. Offline (or fetch failure): a miss is a 404, except for an
        elevation tile with a coarser ancestor on disk, which is derived from
        it (see corvus/dem_tiles.py) so 3D terrain has no holes.
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
        derived = False
        if blob is None:
            blob = self._fetch_upstream_tile(source, z, x, y)
            if blob is None:
                blob = self._derive_terrain_tile(source, z, x, y, cache)
                derived = blob is not None
            if blob is None:
                self._send_json({"error": "tile not available"}, 404)
                return
            if cache is not None and not derived:
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
        elif blob[:4] == b"RIFF" and blob[8:12] == b"WEBP":
            ctype = "image/webp"
        else:
            ctype = "image/png"
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(blob)))
        self._send_cors()
        # A derived tile is a stand-in: the browser must ask again soon, so the
        # real one replaces it once the link is back.
        self.send_header("Cache-Control", "max-age=300" if derived else "max-age=86400")
        self.end_headers()
        self.wfile.write(blob)

    def _derive_terrain_tile(self, source: str, z: int, x: int, y: int,
                             cache: Any) -> bytes | None:
        """An elevation tile resampled from the nearest cached ancestor, or None.

        Only for elevation sources: a blurred photograph is worse than the
        coarser one MapLibre already shows, but a missing height is a hole in
        the terrain. Never stored, see corvus/dem_tiles.py.
        """
        fallback = self.dem_fallback
        if fallback is None or cache is None or not tile_sources.is_terrain(source):
            return None
        sparse = tile_sources.sparse_above(source)
        if sparse is not None and z > sparse:
            # Above the level such a source covers everywhere, a missing tile
            # means "use the level above", and MapLibre does exactly that with
            # a 404. Anything stood in here would be coarser data drawn at a
            # zoom where the real parent is sharper.
            return None
        try:
            blob = fallback.tile(source, z, x, y, cache.get_tile)
            if blob is None and source != tile_sources.DEFAULT_TERRAIN:
                blob = self._terrain_from_default(source, z, x, y)
            return blob
        except Exception:  # noqa: BLE001 - a fallback must never 500 the map
            logger.debug("terrain tile derivation failed: %s/%d/%d/%d",
                         source, z, x, y, exc_info=True)
            return None

    def _terrain_from_default(self, source: str, z: int, x: int, y: int) -> bytes | None:
        """A keyed service's elevation tile, stood in for by the free one.

        The free elevation is what an offline download always carries, so this
        is what keeps a better model from ever meaning worse terrain: over
        ground downloaded before the key was set, or with the key refused, the
        operator still gets the free heights rather than a hole. Cached,
        fetched or derived, in that order, then re-packed for *source*.
        """
        base_id = tile_sources.DEFAULT_TERRAIN
        base_cache = (self.tile_caches or {}).get(base_id)
        spec = tile_sources.get(source)
        base_spec = tile_sources.get(base_id)
        if base_cache is None or spec is None or base_spec is None:
            return None
        try:
            base = base_cache.get_tile(z, x, y)
        except Exception:  # noqa: BLE001 - an unreadable cache is a miss
            base = None
        if base is None:
            base = self._fetch_upstream_tile(base_id, z, x, y)
            if base is not None:
                try:
                    base_cache.put_tile(z, x, y, base)
                except Exception:  # noqa: BLE001 - caching is best-effort
                    logger.exception("tile cache write failed")
        if base is None and self.dem_fallback is not None:
            base = self.dem_fallback.tile(base_id, z, x, y, base_cache.get_tile)
        if base is None:
            return None
        return dem_tiles.transcode(base, base_spec["encoding"], spec["encoding"],
                                   int(spec.get("tile_size", 256)))

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
        sparse = tile_sources.sparse_above(source)
        absent = self.tile_absent
        if (sparse is not None and z > sparse and absent is not None
                and absent.covers(source, z, x, y, sparse)):
            return None
        token = self._map_token(str(src.get("provider") or ""))
        if tile_sources.needs_token(source):
            if not token:
                # No key, so there is nothing to ask for. Returned BEFORE the
                # breaker is consulted and without recording a failure: three
                # of these would otherwise trip the shared breaker and stop the
                # cache-fill for every OTHER source for the cooldown, so a
                # keyless service the operator merely clicked past would take
                # the working map down with it.
                logger.debug("no API key stored for %s; not fetching %s/%d/%d/%d",
                             src.get("provider"), source, z, x, y)
                return None
        breaker = self.tile_breaker
        if breaker is not None and not breaker.allow():
            logger.debug("upstream breaker open; skipping fetch for %s/%d/%d/%d",
                         source, z, x, y)
            return None
        template = src["upstream"]
        session_based = bool(token) and tile_sources.licensed(source) is not None
        if session_based:
            try:
                template = tile_sessions.resolve(source, token)
            except tile_sessions.TileSessionError as exc:
                # Only "could not reach it" is the network's fault. A refused
                # key says nothing about the link, and must not open the
                # breaker for every other source.
                logger.debug("licensed tile API unavailable for %s: %s", source, exc)
                if exc.offline and breaker is not None:
                    breaker.record_failure()
                return None
        url = tile_sources.build_tile_url(template, z, x, y, token)
        try:
            req = urllib.request.Request(
                url, headers={"User-Agent": f"CorvusGCS/{get_version()}"}
            )
            with urllib.request.urlopen(req, timeout=TILE_UPSTREAM_TIMEOUT_S) as resp:
                data = resp.read(TILE_UPSTREAM_MAX_BYTES + 1)
        except urllib.error.HTTPError as exc:
            if session_based and exc.code in (401, 403):
                # The session was withdrawn early; the next tile opens a new one.
                tile_sessions.invalidate(source, token)
            if exc.code == 404 and sparse is not None and z > sparse and absent is not None:
                absent.add(source, z, x, y)
            # The upstream ANSWERED, so the link is up: a status code is about
            # that one service. It must not open the breaker, which every
            # source shares, or an imagery service throttling us with 429s
            # would stop the elevation tiles too and leave holes in the 3D
            # terrain for the whole cooldown.
            logger.debug("upstream tile refused (%s): %s", exc.code,
                         tile_sources.redact_url(url))
            return None
        except Exception:  # noqa: BLE001 - offline field use must never 500
            # Redacted: this line is the one an operator copies into a bug
            # report when tiles will not load, and for a keyed service the URL
            # carries their credential.
            logger.debug("upstream tile fetch failed: %s",
                         tile_sources.redact_url(url), exc_info=True)
            if breaker is not None:
                breaker.record_failure()
            return None
        if (not data or len(data) > TILE_UPSTREAM_MAX_BYTES
                or not tile_sources.looks_like_image(data)):
            # A 200 with an empty body is as useless as a failure, and so is an
            # oversized one; counting both keeps a broken-but-reachable
            # upstream from holding the breaker closed forever. A body that is
            # not an image is a captive portal's login page or a proxy's error
            # page: caching it would serve that page as a map tile, and for
            # an elevation tile decode it as terrain, forever.
            if len(data) > TILE_UPSTREAM_MAX_BYTES:
                logger.debug("upstream tile too large, discarding: %s",
                             tile_sources.redact_url(url))
            elif data:
                logger.debug("upstream answered something that is not an image: %s",
                             tile_sources.redact_url(url))
            if breaker is not None:
                breaker.record_failure()
            return None
        if breaker is not None:
            breaker.record_success()
        return data

    def _api_buildings_serve(self, z: int, x: int, y: int) -> None:
        """Serve the OSM building footprints for one grid cell, as GeoJSON.

        Always 200 with a FeatureCollection, even when there is nothing to
        give. This is on the map's render path: a 404 or a 503 would make the
        frontend distinguish "no buildings here" from "the service is down",
        and both answers draw the same thing — nothing. A cell is fetched from
        Overpass at most once and then answered from disk, offline included.
        """
        from .buildings import CELL_ZOOM

        service = self.buildings
        if (service is None
                or z != CELL_ZOOM
                or x < 0 or y < 0 or x >= (1 << z) or y >= (1 << z)):
            # An off-grid zoom is a frontend bug, not an operator-visible one:
            # answer empty rather than teaching the map to handle an error it
            # can do nothing about. `pending` is false: nothing is coming.
            self._send_buildings(_EMPTY_BUILDINGS, gzipped=False)
            return
        try:
            lookup = getattr(service, "lookup", None)
            payload, coming = (lookup(z, x, y) if callable(lookup)
                               else (service.cell(z, x, y), True))
        except Exception:  # noqa: BLE001 - an overlay must never 500 the map
            logger.exception("building cell lookup failed: %d/%d/%d", z, x, y)
            self._send_buildings(_EMPTY_BUILDINGS, gzipped=False)
            return
        if payload is None:
            # Not cached. `pending` when the service has queued it and will
            # have it shortly: the frontend's cue to ask again. `unavailable`
            # when nothing is on its way (offline, or Overpass asked us to
            # back off): the frontend stops asking until the view changes,
            # rather than polling for a cell no worker will fetch.
            self._send_buildings(_PENDING_BUILDINGS if coming else _UNAVAILABLE_BUILDINGS,
                                 gzipped=False, cache=False)
            return
        self._send_buildings(payload, gzipped=True)

    def _send_buildings(self, body: bytes, *, gzipped: bool, cache: bool = True) -> None:
        """Write a building-cell response.

        The store holds the GeoJSON already gzipped, so the compressed bytes
        go straight onto the wire instead of being inflated here and deflated
        again by the HTTP layer.

        A real cell is cacheable for a day — buildings do not move, and the
        frontend keeps its own in-memory set anyway. A "pending" answer must
        NOT be, or the browser would keep replaying it from its own cache
        after the worker has long since filled ours.
        """
        self.send_response(200)
        self.send_header("Content-Type", "application/geo+json")
        if gzipped:
            self.send_header("Content-Encoding", "gzip")
        self.send_header("Content-Length", str(len(body)))
        self._send_cors()
        self.send_header("Cache-Control", "max-age=86400" if cache else "no-store")
        self.end_headers()
        self.wfile.write(body)

    # ---- Static files ----
    def _serve_static(self, path: str) -> None:
        if path == "/":
            path = "/index.html"
        # Percent-decoded first: a URL path is an encoded form, so without
        # this "/js/my%20file.js" looks for a file literally named
        # "my%20file.js" and 404s. No asset that ships today needs it — this
        # is for whoever adds the first plugin asset with a space in its name.
        #
        # Decoding is what makes "%2e%2e%2f" mean "../", so it is only safe
        # because of the containment check below: resolve() collapses the
        # traversal and relative_to() is what refuses anything that climbed
        # out of WEB_DIR. Decode, then join, then resolve, then check — in
        # that order.
        try:
            file_path = (WEB_DIR / unquote(path).lstrip("/")).resolve()
            file_path.relative_to(WEB_DIR)
        except ValueError:
            # relative_to on an escapee, or a decoded NUL byte that pathlib
            # refuses outright. Neither is a path under WEB_DIR.
            self._send_json({"error": "forbidden"}, 403)
            return
        if not file_path.is_file():
            self._send_json({"error": "not found"}, 404)
            return
        ctype = content_type(file_path)
        # Validators, so the browser can ask "still the same?" instead of
        # being handed the whole file again. src/ is ~4.8 MB and index.html
        # pulls 38 blocking scripts, including maplibre (1.0 MB), plotly
        # (1.0 MB), lucide and xterm — all of it was read off disk, written
        # to the socket and re-parsed on every launch and every reload,
        # because "no-cache" without a validator means "revalidate against
        # nothing", which can only ever be a full re-send.
        #
        # Cache-Control stays "no-cache": still revalidated every time, which
        # is what makes a plugin asset edited on disk show up on reload. Only
        # the body stops crossing the wire when nothing changed.
        try:
            stat = file_path.stat()
        except OSError:
            self._send_json({"error": "not found"}, 404)
            return
        etag = f'"{stat.st_mtime_ns:x}-{stat.st_size:x}"'
        last_modified = formatdate(stat.st_mtime, usegmt=True)
        if self._static_is_unchanged(etag, stat.st_mtime):
            self.send_response(304)
            self.send_header("ETag", etag)
            self.send_header("Last-Modified", last_modified)
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            return
        body = file_path.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-cache")
        self.send_header("ETag", etag)
        self.send_header("Last-Modified", last_modified)
        self.end_headers()
        self.wfile.write(body)

    def _static_is_unchanged(self, etag: str, mtime: float) -> bool:
        """Does the client already hold this exact version of the file?

        ``If-None-Match`` wins outright when present, per RFC 9110: it is the
        strong validator, and a client that sent both is asking to be judged
        on the ETag. Only when there is no ETag does the date get a say —
        and then at one-second resolution, which is why the ETag carries
        nanoseconds and the size: a file rewritten twice within the same
        second is a different file and has a different tag.
        """
        headers = getattr(self, "headers", None)
        if headers is None:
            return False
        if_none_match = (headers.get("If-None-Match") or "").strip()
        if if_none_match:
            # A comma-separated list, and each entry may carry the weak
            # prefix. Compared weakly, which is what a GET is allowed to do.
            candidates = [
                part.strip()[2:] if part.strip().startswith("W/") else part.strip()
                for part in if_none_match.split(",")
            ]
            return "*" in candidates or etag in candidates
        if_modified_since = (headers.get("If-Modified-Since") or "").strip()
        if if_modified_since:
            try:
                since = parsedate_to_datetime(if_modified_since).timestamp()
            except (TypeError, ValueError, OverflowError):
                return False
            # int(): HTTP dates have no sub-second part, so a file written
            # 0.4 s after the timestamp the client holds must still count as
            # newer once truncated.
            return int(mtime) <= int(since)
        return False


# Wire @route declarations collected during the CorvusHandler class body onto
# the class-level route tables that _handle_api_get/_handle_api_post dispatch
# through.
for _http_method, _path, _name in _pending_routes:
    table = CorvusHandler._GET_ROUTES if _http_method == "GET" else CorvusHandler._POST_ROUTES
    table[_path] = _name
_pending_routes.clear()


class CorvusServer(socketserver.ThreadingTCPServer):
    """Threaded TCP server with clean shutdown.

    ``allow_reuse_address`` is deliberately platform-conditional. On POSIX
    SO_REUSEADDR only lets a restart rebind a port still in TIME_WAIT, which
    is what we want. On Windows the same flag means something else entirely:
    it lets a second process bind a port another process is *actively
    listening on*, and the kernel then delivers each incoming connection to
    one of them arbitrarily. A second Corvus launched there would silently
    steal half the first one's HTTP and SSE traffic — telemetry streams
    landing in the wrong window, config writes hitting the wrong backend.
    So Windows gets exclusive binding instead (see :meth:`server_bind`).
    """
    allow_reuse_address = os.name != "nt"
    daemon_threads = True

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        # Set before the base constructor: binding can raise (a taken port),
        # and shutdown() must find a usable event on a half-built server.
        self.stopping = threading.Event()
        super().__init__(*args, **kwargs)

    def server_bind(self) -> None:
        """Bind, asking Windows for an exclusive claim on the port.

        SO_EXCLUSIVEADDRUSE is the Windows counterpart to *not* setting
        SO_REUSEADDR: it makes a second bind to a live port fail loudly with
        EADDRINUSE, which is exactly the answer :func:`bind_server` is
        looking for. Guarded because the constant only exists on Windows and
        the option is refused on some layered service providers.
        """
        if os.name == "nt":
            exclusive = getattr(socket, "SO_EXCLUSIVEADDRUSE", None)
            if exclusive is not None:
                try:
                    self.socket.setsockopt(socket.SOL_SOCKET, exclusive, 1)
                except OSError:  # noqa: BLE001 - fall back to a plain bind
                    logger.debug("SO_EXCLUSIVEADDRUSE refused; binding plainly")
        super().server_bind()

    def handle_error(self, request: Any, client_address: Any) -> None:
        """Report a request that died outside the handler's own guard.

        ``socketserver``'s default prints a bare traceback to stderr, which is
        how a closed browser tab used to produce forty lines of noise: the
        reset lands on ``handle_one_request``'s read of the request line,
        before ``do_GET``/``do_POST`` exist to catch anything. That noise is
        what buries a real fault in the log during a flight — the same reason
        CLIENT_GONE_ERRORS exists for the SSE writers, applied to the half of
        the connection they never see.

        A peer that left is logged at debug. Anything else keeps its full
        traceback, because anything else is a bug.
        """
        exc = sys.exc_info()[1]
        if isinstance(exc, CLIENT_GONE_ERRORS):
            logger.debug("client %s disconnected: %s", client_address, exc)
            return
        logger.exception("error serving %s", client_address)

    def shutdown(self) -> None:
        """Stop the HTTP loop, the MAVLink-borrowing services, and the caches.

        Download workers are stopped first so they are not mid-write to the
        caches; the HTTP serve loop is then stopped; caches are closed last so
        no in-flight handler touches a closed SQLite handle during teardown.
        Any failure is logged, never raised — shutdown must always complete.

        ``stopping`` is set first of all. ``super().shutdown()`` only stops the
        *accept* loop; every SSE handler already connected is parked in its own
        thread on a 15-second wait and knows nothing about any of this, so it
        would wake up afterwards and write to a server whose caches are closed
        and whose downloader has been set to None. The event is what those
        loops check on each wake, so they leave on their own within one wait
        rather than being left to the process exit to clean up.
        """
        stopping = getattr(self, "stopping", None)
        if stopping is not None:
            stopping.set()
        # The watcher re-dials the bridge on its own, so a caller that only has
        # the server must not be left with it running. Joining it twice (the
        # launchers' stop_backend did already) is harmless.
        stop_autoconnect_watcher(self)
        downloader = getattr(self, "tile_downloader", None)
        if downloader is not None:
            try:
                downloader.shutdown()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("tile downloader shutdown failed")
        # The forwarder holds a UDP socket and two daemon threads. serve.py and
        # the desktop app both stop it before the bridge, but a caller that
        # only has the server (the shutdown-path tests, an embedder) must not
        # be left with a bound port after shutdown() returned.
        forwarder = getattr(self, "forwarder", None)
        if forwarder is not None:
            try:
                mav = getattr(self, "mavlink", None)
                if mav is not None:
                    mav.set_frame_sink(None)
                forwarder.stop()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("mavlink forwarder shutdown failed")
        # Stop the flash service before the HTTP loop so a running upload is
        # cancelled/joined before its (shared) MAVLink bridge is torn down.
        flash = getattr(self, "flash", None)
        if flash is not None:
            try:
                flash.shutdown()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("flash service shutdown failed")
        # The radio service can be holding the bridge's serial port for an AT
        # session and restarts the bridge when that session ends. Same reason
        # the forwarder is here: serve.py and the desktop app both stop it,
        # but a caller that only has the server must not be left with a
        # service that can re-open the serial link after shutdown() returned.
        sik = getattr(self, "sik", None)
        if sik is not None:
            try:
                sik.shutdown()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("SiK radio shutdown failed")
        # The RTK service owns a port, a socket and a thread, and undoes its
        # own configuration on the way out. Same reason as the radio service:
        # a caller that only has the server must not be left with a thread
        # that can reopen a serial port after shutdown() returned.
        rtk_svc = getattr(self, "rtk", None)
        if rtk_svc is not None:
            try:
                rtk_svc.shutdown()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("RTK shutdown failed")
        # Log downloads hold a sink on the MAVLink bridge and a worker thread,
        # so they go with flash, before the bridge does.
        logs = getattr(self, "logs", None)
        if logs is not None:
            try:
                logs.shutdown()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("log service shutdown failed")
        # Programs started on this computer. Same reason as the services above.
        local_runner = getattr(self, "local", None)
        if local_runner is not None:
            try:
                local_runner.shutdown()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("local programs shutdown failed")
        pinger = getattr(self, "pinger", None)
        if pinger is not None:
            try:
                pinger.shutdown()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("pinger shutdown failed")
        # ffmpeg processes. Same reason as the services above: a caller that
        # only has the server must not be left with a decoder running.
        video_svc = getattr(self, "video", None)
        if video_svc is not None:
            try:
                video_svc.shutdown()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("video shutdown failed")
        try:
            super().shutdown()
        except Exception:  # noqa: BLE001 - shutdown must not raise
            logger.exception("http server shutdown failed")
        try:
            from .flight_review import clear_cache
            clear_cache()
        except Exception:  # noqa: BLE001 - shutdown must not raise
            logger.exception("flight review cache clear failed")
        try:
            from .tlog_review import clear_cache as clear_tlog_cache
            clear_tlog_cache()
        except Exception:  # noqa: BLE001 - shutdown must not raise
            logger.exception("telemetry review cache clear failed")
        caches = getattr(self, "tile_caches", None) or {}
        for cache in caches.values():
            try:
                cache.close()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("tile cache close failed")
        buildings = getattr(self, "buildings", None)
        if buildings is not None:
            try:
                buildings.close()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("building cache close failed")
        self.tile_downloader = None
        self.tile_caches = {}
        self.buildings = None


# How many consecutive ports a launch will try before giving up. Twenty is
# enough for every plausible number of Corvus windows plus whatever else on
# the machine happens to have taken 8000.
PORT_SEARCH_SPAN = 20

# Which interface the HTTP server listens on.
#
# This defaulted to "" — every interface on the machine — and the API behind it
# has no authentication of any kind: arm, takeoff, a parameter write and a
# firmware upload are each one unauthenticated POST. On a flight-line hotspot
# "every interface" means every laptop and phone on that network, which is not
# what "a local UI served to the operator's own browser" was ever meant to be.
# Loopback is the default, so the API is reachable by this machine only.
#
# CORVUS_BIND is the deliberate way out for the setup that genuinely wants a
# second screen: give it an interface address (or 0.0.0.0 for all of them) and
# accept that whoever can reach the port can fly the aircraft. It is logged
# loudly, once, at bind time — an exposed ground station should never be a
# thing you find out about later.
BIND_HOST_ENV = "CORVUS_BIND"
DEFAULT_BIND_HOST = "127.0.0.1"


def bind_host() -> str:
    """The interface to listen on: loopback unless ``CORVUS_BIND`` says otherwise."""
    host = (os.environ.get(BIND_HOST_ENV) or "").strip()
    if not host or host == DEFAULT_BIND_HOST:
        return DEFAULT_BIND_HOST
    logger.warning(
        "%s=%s: the HTTP API will accept requests from the network, and it has "
        "no authentication. Any non-browser client that can reach this port can "
        "arm the vehicle. Browser writes from a remote UI remain blocked unless "
        "%s is set to that UI's exact origin",
        BIND_HOST_ENV, host, REMOTE_ORIGIN_ENV,
    )
    allowed_origin = (os.environ.get(REMOTE_ORIGIN_ENV) or "").strip()
    if allowed_origin:
        logger.warning("remote browser origin allowed to control the vehicle: %s", allowed_origin)
    return host


def ipv6_loopback_taken(port: int) -> bool:
    """Whether another program holds *port* on ``[::1]``.

    The UI is opened as ``http://localhost:<port>``, and macOS and Windows
    resolve ``localhost`` to ``::1`` first. This server listens on
    ``127.0.0.1`` only, so a program listening on ``[::1]`` (or on ``[::]``)
    with the same port gets the window's requests instead: the app shows
    another program's page, or a blank one, and says nothing. A port taken
    there is treated as taken. A host without IPv6 has nothing to take.
    """
    if not socket.has_ipv6:
        return False
    try:
        with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as probe:
            probe.bind(("::1", int(port)))
    except OSError as exc:
        return exc.errno == errno.EADDRINUSE
    return False


def bind_server(
    port: int,
    handler: type | None = None,
    host: str | None = None,
    span: int = PORT_SEARCH_SPAN,
    server_cls: type | None = None,
) -> CorvusServer:
    """Return a server actually listening on *port*, or the next free one.

    This replaces the "probe a port with a throwaway socket, close it, then
    bind it for real" pattern, which has a window between the probe and the
    bind. Two Corvus windows launched together both probed 8000, both saw it
    free, and then one of them hit ``OSError: [Errno 48] Address already in
    use`` out of the server constructor — an unhandled exception during
    startup, i.e. the launch died before the operator saw a window.

    Here the bind *is* the test: each candidate port is claimed for real, and
    EADDRINUSE simply moves on to the next. Whatever comes back is a socket
    this process owns. Errors that are not "port taken" (a permission denial,
    an unusable interface) are raised immediately rather than retried across
    twenty ports that will all fail the same way.

    *host* defaults to :func:`bind_host` — loopback, unless ``CORVUS_BIND``
    deliberately opts into a wider interface. Passing it explicitly (the tests
    do) bypasses the environment.

    Raises OSError when the whole span is occupied, naming the range tried so
    the message is actionable.
    """
    handler = handler or CorvusHandler
    server_cls = server_cls or CorvusServer
    host = bind_host() if host is None else host
    first = int(port)
    last_exc: OSError | None = None
    for candidate in range(first, first + max(1, int(span))):
        if host == DEFAULT_BIND_HOST and ipv6_loopback_taken(candidate):
            logger.info("port %d is taken on [::1]; looking for a free one", candidate)
            continue
        try:
            return server_cls((host, candidate), handler)
        except OSError as exc:
            if exc.errno != errno.EADDRINUSE:
                raise
            last_exc = exc
            if candidate != first:
                continue
            logger.info("port %d is taken; looking for a free one", candidate)
    raise OSError(
        errno.EADDRINUSE,
        f"no free port in {first}-{first + max(1, int(span)) - 1}",
    ) from last_exc






DEFAULT_MAVLINK_CONNECTION = "udp:127.0.0.1:14550"


def apply_startup_connection(
    bridge: MavlinkBridge,
    conn: Any,
    *,
    configured: str | None = None,
    session: Any = None,
    store: Any = None,
) -> str:
    """Point *bridge* at the link to start on, and say in the log which and why.

    The string reaches here from a command-line argument or from
    ``~/.corvus/config.json``, and neither was checked against what
    ``mavlink_connection`` actually accepts: only ``POST /api/mavlink/connect``
    validated, because that is the path with somewhere to put a 400. A typo in
    the file or on the command line therefore started the app into a reconnect
    loop that could never succeed, reporting whatever pymavlink made of the
    string — for a bare word, an attempt to open it as a serial device.

    Falling back rather than refusing to start: a ground station that comes up
    on the default port is recoverable from inside the app, and one that exits
    on a config typo is recoverable only by editing JSON in the field.

    With a *session* (:class:`corvus.autoconnect.SessionState`) this also runs
    the auto-connect resolver, so a flight controller on a USB cable beats the
    string a config file was left holding last week. *conn* is then the
    command-line argument and *configured* the file's value; called with
    neither, or with auto-connect disabled, the behaviour is exactly what it
    was — set the string, fall back on a bad one.

    Returns the connection string the bridge ended up with.
    """
    from . import autoconnect as ac

    decision: Any = None
    if session is not None and session.enabled:
        try:
            decision = ac.resolve_startup_connection(
                conn if isinstance(conn, str) else None,
                configured,
                ac.classify_ports(bridge.list_serial_ports()),
                usb_enabled=session.usb,
                sik_enabled=session.sik,
                udp_fallback_enabled=session.udp_fallback,
                default_connection=DEFAULT_MAVLINK_CONNECTION,
                validator=bridge.validate_connection,
            )
        except Exception:  # noqa: BLE001 - resolution must never block a launch
            logger.exception("autoconnect: startup resolution failed")
            decision = None

    target = decision.connection_string if decision is not None else conn
    try:
        bridge.set_connection(target)
    except ValueError as exc:
        logger.error(
            "MAVLink connection %r is not usable (%s), starting on %s instead",
            target, exc, DEFAULT_MAVLINK_CONNECTION,
        )
        bridge.set_connection(DEFAULT_MAVLINK_CONNECTION)
        decision = ac.StartupDecision(
            DEFAULT_MAVLINK_CONNECTION, ac.REASON_DEFAULT_FALLBACK,
        )

    if decision is not None:
        logger.info(
            "autoconnect: startup winner=%s reason=%s%s",
            bridge.connection_string(), decision.reason,
            f" detail={decision.detail}" if decision.detail else "",
        )
        if session is not None:
            session.note_decision(decision.reason, bridge.connection_string())
            if store is not None:
                try:
                    store.update(link_auto=session.as_dict())
                except Exception:  # noqa: BLE001 - publishing is never fatal
                    logger.exception("autoconnect: could not publish link_auto")
    return bridge.connection_string()


def _migrate_plugin_settings(config: CorvusConfig, cfg_path: str) -> None:
    """Move plugin settings out of the main config into per-plugin files, once.

    The key is only dropped from the main config when every plugin's file was
    written; otherwise it stays, and GET /api/plugins keeps reading it, so a
    read-only plugin folder costs the migration and not the settings.
    """
    if not config.plugins:
        return
    if not plugin_config.migrate(config.plugins):
        return
    config.plugins = None
    try:
        save_config(config, cfg_path)
    except OSError as exc:
        logger.warning("could not drop migrated plugin settings from %s: %s", cfg_path, exc)


def create_server(
    port: int = 8000,
    mavlink_conn: str | None = None,
    config_path: str | None = None,
) -> CorvusServer:
    """Create and wire up the full Corvus backend server.

    *mavlink_conn* is the command-line connection string, or None when none was
    given. None does not mean the default — it means "nobody has said", which
    is what lets auto-connect look at the hardware before it reaches for the
    config file (see :func:`apply_startup_connection`).
    """
    store = VehicleStateStore()
    mavlink = MavlinkBridge(store, DEFAULT_MAVLINK_CONNECTION)
    ssh = SshBridge()

    # Operator config: loaded once at startup; the /api/config endpoints
    # mutate the live object and persist it back through save_config. The
    # CLI args (port/mavlink_conn) still beat the file for the bind/conn.
    # Loaded BEFORE the bridge is pointed anywhere, because the auto-connect
    # toggles live in it and the startup resolver needs them.
    cfg_path = config_path or default_config_path()
    # Before anything can write the file: no config yet is a first start.
    first_start = {"pending": not os.path.exists(cfg_path)}
    config = load_config(cfg_path)
    _migrate_plugin_settings(config, cfg_path)

    # The battery estimator runs on the telemetry path, so the bridge needs the
    # operator's settings before the first frame rather than at the first save.
    mavlink.set_battery_settings(config.battery)
    # And the Remote ID identity before the first heartbeat, for a stricter
    # reason: an aircraft that connects and starts broadcasting a *default*
    # identity for the seconds until somebody opens the settings page has
    # broadcast a false one, which is worse than having broadcast nothing.
    mavlink.set_remote_id(config.remote_id)

    autoconnect_session = build_autoconnect_session(config)
    apply_startup_connection(
        mavlink, mavlink_conn,
        configured=config.mavlink_connection,
        session=autoconnect_session,
        store=store,
    )

    # Tile resources: one MBTiles cache per source under ~/.corvus/tiles/.
    # TileDownloader binds a single cache at construction, so per-source caches
    # imply per-source downloaders; _TileDownloaderPool presents them as one.
    # Built through the shared _build_tile_resources helper so the desktop app
    # (app.start_backend) and browser mode (create_server) never diverge.
    # Honor the operator's tile_cache_dir exactly as the desktop app does: an
    # override that only takes in one mode points the two modes at different
    # .mbtiles files, so an area downloaded in one is missing in the other.
    cache_dir = _tile_cache_dir(config)
    tile_caches, tile_progress_bus, tile_downloader, tile_breaker = \
        _build_tile_resources(cache_dir)
    buildings = _build_building_service(cache_dir)
    geocoder = _build_geocoder()

    CorvusHandler.store = store
    CorvusHandler.mavlink = mavlink
    CorvusHandler.ssh = ssh
    CorvusHandler.config = config
    CorvusHandler.config_path = cfg_path
    CorvusHandler.first_start = first_start
    CorvusHandler.autoconnect_session = autoconnect_session
    CorvusHandler.tile_caches = tile_caches
    CorvusHandler.tile_downloader = tile_downloader
    CorvusHandler.tile_progress_bus = tile_progress_bus
    CorvusHandler.tile_breaker = tile_breaker
    CorvusHandler.buildings = buildings
    CorvusHandler.geocoder = geocoder

    # Create ~/.corvus/plugins (with its README) at startup rather than only
    # when Settings opens it, so an operator who was told "drop it in the
    # plugins folder" finds one already there. Never raises.
    ensure_user_plugins_dir()

    # Firmware-flash service (direct USB only), built through the shared
    # helper the desktop app uses so neither mode ends up without a catalogue.
    flash = _build_flash_service(mavlink, store, config)
    CorvusHandler.flash = flash

    # SiK telemetry-radio configuration, through the shared builder so the
    # desktop app gets the same radio page this one does.
    sik = _build_sik_service(mavlink, store)
    CorvusHandler.sik = sik

    # RTK corrections. Built through the shared builder like everything else,
    # and started by it: the default is that a base plugged in is found.
    rtk_svc = _build_rtk_service(mavlink, store, config)
    CorvusHandler.rtk = rtk_svc

    # Flight-log service: on-board ULog download over MAVLink plus the local
    # tlog listing. Lazily imported for the same reason as the flash service.
    logs = _build_log_service(mavlink, config)
    CorvusHandler.logs = logs

    # Second-station forwarding: off unless the operator turned it on, so the
    # default behaviour is exactly what it was before this existed.
    forwarder = _build_forwarder(mavlink, config)
    CorvusHandler.forwarder = forwarder

    # Update check against the GitHub releases, through the shared builder.
    updates = _build_update_checker()
    CorvusHandler.updates = updates

    # Camera video. Nothing runs until a camera window asks for a frame.
    video_svc = _build_video_service(config)
    CorvusHandler.video = video_svc

    # Launcher buttons that run on this computer. Nothing runs until one does.
    local_runner = _build_local_runner()
    CorvusHandler.local = local_runner
    pinger = _build_pinger()
    CorvusHandler.pinger = pinger

    # bind_server, not a bare constructor: a port that is already taken moves
    # to the next one instead of raising out of create_server and killing the
    # launch. Callers that care which port they got read server_address[1].
    server = bind_server(port, CorvusHandler)
    server.mavlink = mavlink
    server.ssh = ssh
    server.store = store
    server.config = config
    server.config_path = cfg_path
    server.flash = flash
    server.sik = sik
    server.rtk = rtk_svc
    server.logs = logs
    server.forwarder = forwarder
    server.updates = updates
    server.video = video_svc
    server.local = local_runner
    server.pinger = pinger
    server.tile_caches = tile_caches
    server.tile_downloader = tile_downloader
    server.buildings = buildings
    server.geocoder = geocoder
    server.autoconnect_session = autoconnect_session
    mavlink.start()
    # After mavlink.start(), never before: the resolver above already decided
    # the first link synchronously, and the watcher exists only for what is
    # plugged in afterwards. Starting it earlier would let it dial a bridge
    # that has not been started yet.
    server.autoconnect = start_autoconnect_watcher(mavlink, store, autoconnect_session)
    return server
