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

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from corvus.server import create_server
from corvus.version import get_version

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    datefmt="%H:%M:%S",
)
logger = logging.getLogger("corvus")


def main() -> None:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    mavlink_conn = sys.argv[2] if len(sys.argv) > 2 else "udp:0.0.0.0:14540"

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

    try:
        # Event.wait() is race-free; signal.pause() can miss a signal delivered between the check and the call.
        shutting_down.wait()
    except (KeyboardInterrupt, SystemExit):
        pass

    logger.info("Shutting down — stopping all connections …")
    try: server.mavlink.stop()
    except: pass
    try: server.ssh.shutdown()
    except: pass
    try: server.shutdown()
    except: pass
    logger.info("All connections stopped. Exiting.")
    os._exit(0)


if __name__ == "__main__":
    main()
