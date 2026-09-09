#!/usr/bin/env python3
"""Corvus GCS standalone desktop app wrapper.

Launches the backend HTTP/SSE server in a background thread and opens the
UI in a PyQt6 QtWebEngine window — no browser needed. Handles clean
shutdown of both the server and the web engine.

Usage:
    python3 corvus/app.py [port] [mavlink_connection]

Chromium rendering flags are set to handle environments where GBM/EGL
is not available (falls back to Vulkan or software rendering).
"""
from __future__ import annotations

import atexit
import logging
import os
import signal
import sys
import threading
import time

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, REPO_ROOT)
WEB_DIR = os.path.join(REPO_ROOT, "src")
os.environ.setdefault("QTWEBENGINE_CHROMIUM_FLAGS", " ".join([
    "--enable-features=Vulkan",
    "--use-vulkan",
    "--ignore-gpu-blocklist",
    "--enable-unsafe-swiftshader",
    "--disable-gpu-sandbox",
    "--disable-software-rasterizer",
    "--enable-webgl",
    "--force-fieldtrials=GPUHardwareRendering/Default",
]))

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    datefmt="%H:%M:%S",
)
logger = logging.getLogger("corvus.app")

from corvus.config import default_config_path, load_config
from corvus.mavlink_bridge import MavlinkBridge
from corvus.ssh_bridge import SshBridge
from corvus.state_store import VehicleStateStore
from corvus.version import get_version


def find_free_port(preferred: int = 8000) -> int:
    """Return *preferred* if free, otherwise the next free port."""
    import socket
    for port in range(preferred, preferred + 20):
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
                s.bind(("", port))
                return port
        except OSError:
            continue
    return preferred


def start_backend(port: int, mavlink_conn: str) -> tuple:
    """Start the HTTP/SSE server and MAVLink bridge in-process. Return (server, mavlink, ssh)."""
    from corvus.server import (
        CorvusHandler, CorvusServer, _build_forwarder, _build_log_service,
        _build_tile_resources,
    )
    from corvus.tile_cache import default_cache_dir

    store = VehicleStateStore()
    mavlink = MavlinkBridge(store, mavlink_conn)
    ssh = SshBridge()

    # Operator config: the desktop app reads the same config file browser mode
    # does, so /api/config and the tile cache dir honor the operator override.
    # Set these on the class BEFORE constructing the server so every handler
    # shares the one live config object (per-request instance attrs would not
    # persist, leaving the next request reading a stale/default value).
    cfg = load_config()
    cfg_path = default_config_path()
    CorvusHandler.config = cfg
    CorvusHandler.config_path = cfg_path

    CorvusHandler.store = store
    CorvusHandler.mavlink = mavlink
    CorvusHandler.ssh = ssh

    # Tile resources: built through the SAME helper create_server() uses so
    # offline maps work in the desktop app too (POST /api/tiles/download,
    # /api/tiles/sources cached_count, local MBTiles caching). Without this
    # the desktop app's tile endpoints 503 and tiles never cache locally.
    # Honor the operator config override for the cache dir.
    cache_dir = cfg.tile_cache_dir or default_cache_dir()
    tile_caches, tile_progress_bus, tile_downloader, tile_breaker = \
        _build_tile_resources(cache_dir)
    CorvusHandler.tile_caches = tile_caches
    CorvusHandler.tile_downloader = tile_downloader
    CorvusHandler.tile_progress_bus = tile_progress_bus
    CorvusHandler.tile_breaker = tile_breaker

    # Firmware-flash service (direct USB only). Wired on the independently-
    # built server so the desktop app supports flashing too.
    flash = None
    try:
        from corvus.flash_service import FlashService
        flash = FlashService(mavlink, store)
    except Exception:
        logger.exception("flash service unavailable")
    CorvusHandler.flash = flash

    # Flight-log service: on-board ULog download over MAVLink plus the local
    # tlog listing. Built through the same helper create_server() uses — the
    # desktop app used to skip it entirely, which left every packaged build
    # with an Analysis page that could not list or download a single ULog.
    logs = _build_log_service(mavlink, cfg)
    CorvusHandler.logs = logs

    # Second-station MAVLink forwarding, so QGroundControl can share the one
    # physical link. Off unless the operator turned it on in the config.
    forwarder = _build_forwarder(mavlink, cfg)
    CorvusHandler.forwarder = forwarder

    # Update check against the GitHub releases, wired on the independently-
    # built desktop server so the app prompts there too. Constructed only; the
    # first network call happens when the frontend asks.
    updates = None
    try:
        from corvus.update_check import UpdateChecker
        updates = UpdateChecker()
    except Exception:
        logger.exception("update checker unavailable")
    CorvusHandler.updates = updates

    server = CorvusServer(("", port), CorvusHandler)
    server.mavlink = mavlink
    server.ssh = ssh
    server.store = store
    server.config = cfg
    server.config_path = cfg_path
    server.flash = flash
    server.logs = logs
    server.forwarder = forwarder
    server.updates = updates
    # Mirror tile resources onto the server instance so CorvusServer.shutdown()
    # closes the caches and stops the downloader (no leaked SQLite handles).
    server.tile_caches = tile_caches
    server.tile_downloader = tile_downloader
    mavlink.start()

    backend_thread = threading.Thread(
        target=server.serve_forever, name="corvus-backend", daemon=True,
    )
    backend_thread.start()
    return server, mavlink, ssh


def app_icon_inverted(cfg) -> bool:
    """Whether the operator asked for the inverted cut of the mark.

    Reads the live config object the HTTP handlers mutate in place, so the
    Settings switch reaches the Dock / taskbar without a restart. Anything
    other than a genuine ``True`` means the normal (white artwork) mark.
    """
    ui = getattr(cfg, "ui", None)
    return isinstance(ui, dict) and ui.get("inverted_app_icon") is True


def app_icon_path(inverted: bool) -> str:
    """Absolute path of the PNG the Dock / taskbar icon is built from."""
    name = "CorvusGCS_logo_inverted.png" if inverted else "CorvusGCS_logo.png"
    return os.path.join(REPO_ROOT, "assets", name)


def _stop_all(server) -> None:
    """Ordered, exception-safe backend teardown. Never raises.

    Mirrors serve.py: mavlink (stop telemetry, join threads, flush tlog),
    ssh (join reader threads), state store (forward-compat lifecycle hook),
    then the HTTP server + tile caches (stop accepting requests, close
    SQLite handles last). Each step is independently guarded so a failure
    in one cannot skip the rest.
    """
    # Flash uses the MAVLink bridge (it stops/starts it), so cancel/join the
    # uploader BEFORE tearing the bridge down.
    flash = getattr(server, "flash", None)
    if flash is not None:
        try:
            logger.info("stopping flash …")
            flash.shutdown()
            logger.info("flash stopped")
        except Exception:
            logger.exception("flash shutdown failed")
    # Log downloads hold a sink on the MAVLink bridge and a worker thread, so
    # they are stopped alongside flash — before the bridge itself goes away.
    logs = getattr(server, "logs", None)
    if logs is not None:
        try:
            logger.info("stopping log service …")
            logs.shutdown()
            logger.info("log service stopped")
        except Exception:
            logger.exception("log service shutdown failed")
    # The forwarder holds a UDP socket, two daemon threads, and a sink on the
    # bridge's receive path, so it is released before the bridge goes away.
    forwarder = getattr(server, "forwarder", None)
    if forwarder is not None:
        try:
            logger.info("stopping mavlink forwarding …")
            mav = getattr(server, "mavlink", None)
            if mav is not None:
                mav.set_frame_sink(None)
            forwarder.stop()
            logger.info("mavlink forwarding stopped")
        except Exception:
            logger.exception("mavlink forwarder shutdown failed")
    mavlink = getattr(server, "mavlink", None)
    if mavlink is not None:
        try:
            logger.info("stopping mavlink …")
            mavlink.stop()
            logger.info("mavlink stopped")
        except Exception:
            logger.exception("mavlink stop failed")
    ssh = getattr(server, "ssh", None)
    if ssh is not None:
        try:
            logger.info("stopping ssh …")
            ssh.shutdown()
            logger.info("ssh stopped")
        except Exception:
            logger.exception("ssh shutdown failed")
    store = getattr(server, "store", None)
    if store is not None:
        try:
            logger.info("stopping state store …")
            store.shutdown()
            logger.info("state store stopped")
        except Exception:
            logger.exception("state store shutdown failed")
    try:
        logger.info("stopping http server + tiles …")
        server.shutdown()
        logger.info("http server + tiles stopped")
    except Exception:
        logger.exception("http server shutdown failed")
    # Defense-in-depth: shutdown() stops the serve loop but does not close
    # the listening TCP socket. os._exit reclaims it in the live path, but an
    # explicit close keeps the fd table clean on a graceful exit and lets the
    # hermetic shutdown tests assert fileno==-1 without calling server_close
    # themselves.
    try:
        logger.info("closing http socket …")
        server.server_close()
        logger.info("http socket closed")
    except Exception:
        logger.exception("http socket close failed")


def main() -> int:
    from PyQt6.QtCore import QUrl, Qt
    from PyQt6.QtGui import QGuiApplication, QIcon
    from PyQt6.QtWebEngineWidgets import QWebEngineView
    from PyQt6.QtWidgets import QApplication, QMainWindow, QVBoxLayout, QWidget

    cfg = load_config()
    cfg_path = default_config_path()
    # Forward-compat env hooks: the map/mavlink agents' default_cache_dir()/
    # default_log_dir() do not read these env vars yet, but setting them now
    # is harmless and lets those agents honour an operator override later
    # without touching app.py.
    if cfg.tile_cache_dir:
        os.environ["CORVUS_TILE_CACHE_DIR"] = cfg.tile_cache_dir
    if cfg.tlog_dir:
        os.environ["CORVUS_TLOG_DIR"] = cfg.tlog_dir
    logger.info("Config loaded from %s: mavlink=%s http_port=%d "
                "tile_cache_dir=%s tlog_dir=%s",
                cfg_path, cfg.mavlink_connection, cfg.http_port,
                cfg.tile_cache_dir or "(default)", cfg.tlog_dir or "(default)")

    # CLI args override the config file when present.
    port = int(sys.argv[1]) if len(sys.argv) > 1 else cfg.http_port
    mavlink_conn = sys.argv[2] if len(sys.argv) > 2 else cfg.mavlink_connection

    port = find_free_port(port)
    logger.info("Starting backend on port %d (MAVLink: %s)", port, mavlink_conn)
    server, mavlink, ssh = start_backend(port, mavlink_conn)

    app_id = f"corvus.gcs.{get_version()}"
    app = QApplication(sys.argv)
    app.setApplicationName("CORVUS GCS")
    app.setApplicationDisplayName("CORVUS GCS")
    app.setApplicationVersion(get_version())

    window = QMainWindow()
    window.setWindowTitle(f"CORVUS GCS v{get_version()}")
    window.resize(1440, 1024)
    window.setMinimumSize(960, 600)

    central = QWidget()
    layout = QVBoxLayout(central)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(0)

    web = QWebEngineView()
    web.setUrl(QUrl(f"http://localhost:{port}/"))
    layout.addWidget(web)
    window.setCentralWidget(central)

    # Dock / taskbar icon, from the live config. Two cuts of the mark ship in
    # assets/: white artwork for a dark dock, black for a light one. Applied
    # here for the first paint and re-checked on the timer below, so flipping
    # the switch in Settings -> Appearance lands without a restart. Nothing
    # else in the app reads this: it is the icon only, not a theme.
    #
    # Seeded with False, not None, because False is what the platform already
    # shows: both build scripts cut the bundle icon (.icns / .png) from the
    # normal mark. An operator who never touches the switch therefore keeps
    # the packaged icon untouched — Qt is only asked for an icon once the
    # config actually asks for a different one.
    applied_icon: list[bool] = [False]

    def sync_app_icon() -> None:
        want = app_icon_inverted(getattr(server, "config", None))
        if want == applied_icon[0]:
            return
        applied_icon[0] = want          # never retry a missing file every tick
        path = app_icon_path(want)
        if not os.path.exists(path):
            logger.warning("app icon %s missing; keeping the current one", path)
            return
        icon = QIcon(path)
        app.setWindowIcon(icon)         # Dock on macOS, taskbar on Linux
        window.setWindowIcon(icon)

    sync_app_icon()

    import threading as _t
    shutting_down = _t.Event()

    def shutdown() -> None:
        if shutting_down.is_set():
            return
        shutting_down.set()

        logger.info("Shutting down — stopping all connections …")
        _stop_all(server)

        # Bounded exit watchdog. The Qt event loop normally returns cleanly
        # and main() then sys.exit(ret); but QtWebEngine child processes
        # and stuck daemon threads can block interpreter teardown. The
        # daemon sleeps 2s and forces os._exit(0) if the main thread is
        # still alive then — the app never hangs on quit. It dies with the
        # process on a clean exit, so the normal path is just sys.exit.
        #
        # Why start AFTER _stop_all: so the watchdog cannot pre-empt the
        # bounded joins inside mavlink.stop() (which flushes the tlog) and
        # cut that flush short on a lossy link. It only guards the post-
        # teardown phase (Qt page deleteLater + the app.exec() return),
        # matching the same fix already applied in serve.py.
        def _watchdog() -> None:
            time.sleep(2.0)
            if threading.main_thread().is_alive():
                logger.warning("clean exit timed out after 2s; forcing os._exit(0)")
                os._exit(0)
        _t.Thread(target=_watchdog, name="exit-watchdog", daemon=True).start()

        try:
            web.page().deleteLater()
        except Exception:
            logger.exception("web page deleteLater failed")
        logger.info("All connections stopped.")

    app.aboutToQuit.connect(shutdown)
    atexit.register(shutdown)

    def quit_app(*_args) -> None:
        app.quit()

    signal.signal(signal.SIGINT, quit_app)
    signal.signal(signal.SIGTERM, quit_app)

    # Timer to process SIGINT promptly (Qt doesn't handle signals natively)
    from PyQt6.QtCore import QTimer
    timer = QTimer()
    timer.start(500)
    timer.timeout.connect(lambda: None)
    # Same tick picks up an app-icon change written by POST /api/config. A
    # dict lookup and a comparison; the QIcon is only rebuilt when the answer
    # actually changed, which is why polling here beats plumbing an event out
    # of the HTTP thread into the Qt loop.
    timer.timeout.connect(sync_app_icon)

    window.show()
    logger.info("CORVUS GCS v%s — standalone window ready", get_version())
    ret = app.exec()
    shutdown()  # belt-and-suspenders if aboutToQuit did not fire
    return ret


if __name__ == "__main__":
    sys.exit(main())
