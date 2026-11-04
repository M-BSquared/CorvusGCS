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
from corvus.instance_lock import (
    ALLOW_MULTI_ENV,
    InstanceLock,
    allow_multi,
    describe_peer,
)
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

    The sequence lives in :func:`corvus.server.stop_backend` — it was
    duplicated here and in corvus/app.py, character for character, and the
    order in it is load-bearing. Kept as a named function because it is what
    the signal handlers and the shutdown tests reach for.
    """
    from corvus.server import stop_backend
    stop_backend(server, logger)

def main() -> int:
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

    # CLI args override the config file when present. A mistyped port is an
    # operator error, so it gets an operator's answer — the usage line and a
    # non-zero exit — rather than an int() traceback out of module scope.
    port = cfg.http_port
    if len(sys.argv) > 1:
        try:
            port = int(sys.argv[1])
        except ValueError:
            logger.error(
                "invalid port %r\nusage: python3 serve.py [PORT] [MAVLINK_CONNECTION]",
                sys.argv[1],
            )
            return 2
        if not 1 <= port <= 65535:
            logger.error("port %d is out of range (1-65535)", port)
            return 2
    # None, not the config value, when no argument was given: the startup
    # resolver treats an explicit argument as the operator's own decision and
    # lets it outrank a flight controller on a cable, so handing it the config
    # file's string would make auto-connect impossible to reach. The file is
    # passed separately, at its own priority.
    mavlink_conn = sys.argv[2] if len(sys.argv) > 2 else None

    # One ground station per machine — the serial link, the forwarder's UDP
    # port and ~/.corvus/config.json cannot be shared between two of them, and
    # nothing about a split serial stream looks like a failure while it is
    # happening. See corvus/instance_lock.py. CORVUS_ALLOW_MULTI=1 opts out.
    lock = InstanceLock()
    if not lock.acquire(port=None) and not allow_multi():
        logger.error("CORVUS GCS is %s.", describe_peer(lock.peer()))
        logger.error("Open that one, or set %s=1 to run a second instance.",
                     ALLOW_MULTI_ENV)
        return 1
    atexit.register(lock.release)

    try:
        server = create_server(port=port, mavlink_conn=mavlink_conn)
    except OSError as exc:
        # Every port in the search span is busy. A named error beats a
        # traceback out of the constructor.
        logger.error("could not start the HTTP server: %s", exc)
        lock.release()
        return 1
    # create_server may have landed on a different port than the one asked
    # for; everything the operator is told from here on uses the real one.
    port = server.server_address[1]
    lock.write_port(port)

    shutting_down = threading.Event()
    # _torn_down tracks whether _stop_all has actually run, so the atexit
    # teardown (which fires if the main thread exits abnormally before the
    # normal wait->_stop_all path) never double-tears-down with the normal
    # path. Without a real atexit teardown the daemon threads were killed by
    # the OS with no flush/close — tlogs could truncate, violating the clean
    # shutdown contract.
    _torn_down = threading.Event()

    # Test-and-set under a lock. The normal path and the atexit hook can
    # reach _teardown() from different threads, and the window between
    # is_set() and set() was wide enough for both to pass — two concurrent
    # _stop_all() runs closing the same sockets and joining the same threads.
    _teardown_lock = threading.Lock()

    def _teardown() -> None:
        """Run _stop_all exactly once; safe from the normal path and atexit."""
        with _teardown_lock:
            if _torn_down.is_set():
                return
            _torn_down.set()
        _stop_all(server)

    def shutdown(*_args) -> None:
        # Event.set() is idempotent and atomic; no guard needed, and none
        # wanted — this runs in a signal handler.
        shutting_down.set()

    signal.signal(signal.SIGINT, shutdown)
    signal.signal(signal.SIGTERM, shutdown)
    # Real teardown at interpreter exit: if the normal wait->_stop_all path
    # already ran, _torn_down is set and this is a no-op; if the main thread
    # died early (uncaught exception), this is the only chance to flush tlogs
    # and join threads. The signal handlers still only set the event, so the
    # normal wait->_stop_all path works exactly as before.
    atexit.register(_teardown)

    logger.info("CORVUS GCS v%s — http://localhost:%d/ (MAVLink: %s)",
                get_version(), port, server.mavlink.connection_string())
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
    _teardown()
    # Start the watchdog AFTER teardown returns so it can never pre-empt
    # _stop_all: mavlink.stop() joins up to ~8s, and a 2s watchdog firing
    # mid-teardown would cut the tlog flush short. The watchdog now only
    # guards the final server_thread.join against a hang.
    watchdog.start()
    try:
        server_thread.join(timeout=2)
    except Exception:
        logger.exception("http server thread join failed")
    logger.info("All connections stopped. Exiting.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
