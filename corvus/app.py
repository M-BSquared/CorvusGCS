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

import functools
import http.server
import socketserver

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


def main() -> int:
    from PyQt6.QtCore import QUrl, Qt
    from PyQt6.QtGui import QGuiApplication
    from PyQt6.QtWebEngineWidgets import QWebEngineView
    from PyQt6.QtWidgets import QApplication, QMainWindow, QVBoxLayout, QWidget

    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    mavlink_conn = sys.argv[2] if len(sys.argv) > 2 else "udp:0.0.0.0:14540"

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
        logger.info("Shutting down — stopping all connections …")
        try:
            mavlink.stop()
        except Exception:
            pass
        try:
            ssh.shutdown()
        except Exception:
            pass
        try:
            server.shutdown()
        except Exception:
            pass
        try:
            web.page().deleteLater()
        except Exception:
            pass
        logger.info("All connections stopped. Exiting.")
        os._exit(0)

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
    shutdown()
    return ret


if __name__ == "__main__":
    sys.exit(main())
