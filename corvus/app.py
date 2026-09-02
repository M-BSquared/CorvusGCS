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
    from corvus.server import CorvusHandler, CorvusServer

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

    backend_thread = threading.Thread(
        target=server.serve_forever, name="corvus-backend", daemon=True,
    )
    backend_thread.start()
    return server, mavlink, ssh


def _stop_all(server) -> None:
    """Ordered, exception-safe backend teardown. Never raises.

    Mirrors serve.py: mavlink (stop telemetry, join threads, flush tlog),
    ssh (join reader threads), state store (forward-compat lifecycle hook),
    then the HTTP server + tile caches (stop accepting requests, close
    SQLite handles last). Each step is independently guarded so a failure
    in one cannot skip the rest.
    """
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
    from PyQt6.QtGui import QGuiApplication
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

    import threading as _t
    shutting_down = _t.Event()

    def shutdown() -> None:
        if shutting_down.is_set():
            return
        shutting_down.set()

        # Bounded exit watchdog. The Qt event loop normally returns cleanly
        # and main() then sys.exit(ret); but QtWebEngine child processes
        # and stuck daemon threads can block interpreter teardown. The
        # daemon sleeps 2s and forces os._exit(0) if the main thread is
        # still alive then — the app never hangs on quit. It dies with the
        # process on a clean exit, so the normal path is just sys.exit.
        def _watchdog() -> None:
            time.sleep(2.0)
            if threading.main_thread().is_alive():
                logger.warning("clean exit timed out after 2s; forcing os._exit(0)")
                os._exit(0)
        _t.Thread(target=_watchdog, name="exit-watchdog", daemon=True).start()

        logger.info("Shutting down — stopping all connections …")
        _stop_all(server)
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

    window.show()
    logger.info("CORVUS GCS v%s — standalone window ready", get_version())
    ret = app.exec()
    shutdown()  # belt-and-suspenders if aboutToQuit did not fire
    return ret


if __name__ == "__main__":
    sys.exit(main())
