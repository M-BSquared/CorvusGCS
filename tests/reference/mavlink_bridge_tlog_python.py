"""The bridge's tlog session handling, as Corvus had it in Python: the file
name, starting a file per connection and per flight, stopping it, and handing
each frame to it and to the forwarder.

Kept unchanged as the reference tests/test_tlog_parity.py holds the Rust
engine to; not imported by the app.
"""
from __future__ import annotations

import datetime
import logging
import os
from typing import Any

from tests.reference.tlog_python import TlogWriter

logger = logging.getLogger("corvus.mavlink")


def default_log_dir() -> str:
    from corvus import mavlink_bridge

    return mavlink_bridge.default_log_dir()


class TlogSessionMixin:
    """The bridge's tlog session handling, as the bridge had it."""

    @staticmethod
    def _next_tlog_path(log_dir: str) -> str:
        """A tlog path in *log_dir* that is not already taken.

        The name is a timestamp because that sorts, and it carried microseconds
        because two reconnects can land in the same second. That was assumed to
        make it collision-free; it does not. ``datetime.now()`` resolves to the
        platform clock, and Windows' is ~1 ms at best — two reconnects inside
        one tick produced one name, so the second session opened the first
        session's file and overwrote a flight's recorded telemetry.

        The clock is therefore no longer trusted to be unique on its own: if
        the path is taken, a counter is appended. The separator is "_" and not
        "-" so the names still sort into session order — "-" (0x2D) sorts
        BEFORE "." (0x2E), which would have put the second session's file ahead
        of the first's in the Analysis listing.
        """
        stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S-%f")
        path = os.path.join(log_dir, stamp + ".tlog")
        n = 1
        while os.path.exists(path):
            # Zero-padded so _02 sorts after _01 rather than lexicographically
            # scattering once a tick collects ten sessions.
            path = os.path.join(log_dir, f"{stamp}_{n:02d}.tlog")
            n += 1
        return path

    def _start_tlog(self) -> None:
        """Open a fresh tlog for this flight session.

        No-op when the bridge is not running (e.g. direct-_connect unit tests)
        so they stay hermetic. A log-dir permission error must never break the
        link: on failure the tlog is simply disabled for the session.
        """
        if not self._running.is_set():
            return
        # Close any leftover writer from a prior cycle (defensive: _run tears
        # down between reconnects, but guard against a double connect).
        self._stop_tlog()
        try:
            log_dir = default_log_dir()
            os.makedirs(log_dir, exist_ok=True)
            path = self._next_tlog_path(log_dir)
            self._tlog = TlogWriter(path)
            self._tlog.set_conn(self._conn_str)
        except Exception as exc:
            logger.warning("tlog disabled: %s", exc)
            self._tlog = None

    def _roll_tlog_on_disarm(self, armed: bool) -> None:
        """Start the next tlog when the vehicle disarms after a flight.

        Two sorties on one battery swap used to share a file, so a log could
        not be picked by flight. The disarm heartbeat itself is already in the
        closing file (frames are logged before they are dispatched), so that
        file holds the whole flight and the new one opens on the ground. The
        firmware version is asked for again so the new file carries its own
        AUTOPILOT_VERSION for the flight review.
        """
        was_armed = self._tlog_armed
        self._tlog_armed = armed
        if not was_armed or armed or self._tlog is None:
            return
        self._start_tlog()
        if self._tlog is not None:
            self._request_version()

    def _stop_tlog(self) -> None:
        """Stop and close the current tlog if active; idempotent."""
        tlog = self._tlog
        if tlog is None:
            return
        self._tlog = None
        try:
            tlog.stop()
        except Exception as exc:
            logger.debug("tlog stop failed: %s", exc)

    def _write_tlog(self, msg: Any) -> None:
        """Log a raw MAVLink frame if a tlog is active; never breaks the link.

        Also hands the same frame to the forwarder when one is attached: both
        consumers want the bytes exactly as they came off the wire, and both
        are non-blocking appends, so taking the msgbuf once serves both.
        """
        tlog = self._tlog
        sink = self._frame_sink
        if tlog is None and sink is None:
            return
        try:
            get_buf = getattr(msg, "get_msgbuf", None)
            if get_buf is None:
                return
            raw = get_buf()
        except Exception as exc:
            logger.debug("frame capture failed: %s", exc)
            return
        if tlog is not None:
            try:
                tlog.write_frame(raw)
            except Exception as exc:
                logger.debug("tlog write failed: %s", exc)
        if sink is not None:
            try:
                sink(raw)
            except Exception as exc:  # noqa: BLE001 - a sink never breaks the link
                logger.debug("frame forward failed: %s", exc)
