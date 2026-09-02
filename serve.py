#!/usr/bin/env python3
"""Corvus GCS entry point (browser mode).

Starts the HTTP/SSE server, the MAVLink bridge, and the SSH bridge in a
single process. Clean shutdown on SIGINT / SIGTERM / atexit — stops all
connections, threads, and background tasks.
"""
from __future__ import annotations

import atexit
import logging
import os
import signal
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from corvus.config import default_config_path, load_config
from corvus.server import create_server
from corvus.version import get_version

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    datefmt="%H:%M:%S",
)
logger = logging.getLogger("corvus")


def _stop_all(server) -> None:
    """Ordered, exception-safe backend teardown. Never raises.

    Order matters: mavlink first (stop producing telemetry, join its
    threads, flush the tlog), then ssh (join reader threads), then the
    state store (forward-compat lifecycle hook), then the HTTP server +
    tile caches (stop accepting requests, close SQLite handles last so no
    in-flight handler touches a closed DB). Each step is independently
    guarded so a failure in one cannot skip the rest.
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


def main() -> None:
    cfg = load_config()
    cfg_path = default_config_path()
    # Forward-compat env hooks: the map/mavlink agents' default_cache_dir()/
    # default_log_dir() do not read these env vars yet, but setting them now
    # is harmless and lets those agents honour an operator override later
    # without touching serve.py.
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

    server = create_server(port=port, mavlink_conn=mavlink_conn)
    shutting_down = threading.Event()

    def shutdown(*_args) -> None:
        if shutting_down.is_set():
            return
        shutting_down.set()

    signal.signal(signal.SIGINT, shutdown)
    signal.signal(signal.SIGTERM, shutdown)
    atexit.register(lambda: shutdown())

    logger.info("CORVUS GCS v%s — http://localhost:%d/ (MAVLink: %s)",
                get_version(), port, mavlink_conn)
    logger.info("Press Ctrl+C to stop.")

    server_thread = threading.Thread(target=server.serve_forever, name="http-server", daemon=True)
    server_thread.start()

    # Bounded exit. After the stop sequence we prefer a clean sys.exit(0),
    # but stuck daemon threads (or a QtWebEngine child in app mode) can
    # block interpreter teardown. A daemon watchdog sleeps 2s and forces
    # os._exit(0) if the main thread is still alive then — so the field
    # laptop never hangs on shutdown. It dies with the process on a clean
    # exit, so the normal path is just sys.exit(0).
    def _watchdog() -> None:
        time.sleep(2.0)
        if threading.main_thread().is_alive():
            logger.warning("clean exit timed out after 2s; forcing os._exit(0)")
            os._exit(0)

    watchdog = threading.Thread(target=_watchdog, name="exit-watchdog", daemon=True)

    try:
        # Event.wait() is race-free; signal.pause() can miss a signal
        # delivered between the check and the call.
        shutting_down.wait()
    except (KeyboardInterrupt, SystemExit):
        # A synchronous KeyboardInterrupt/SystemExit must still reach the
        # stop sequence below — `pass` suppresses the exception and lets
        # execution fall through to _stop_all; it never bypasses cleanup.
        pass

    logger.info("Shutting down — stopping all connections …")
    watchdog.start()
    _stop_all(server)
    try:
        server_thread.join(timeout=2)
    except Exception:
        logger.exception("http server thread join failed")
    logger.info("All connections stopped. Exiting.")
    sys.exit(0)


if __name__ == "__main__":
    main()
