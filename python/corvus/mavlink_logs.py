"""On-board log transfer: listing, fetching and erasing the vehicle's logs.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`. The LOG_* request side
is ``corvus_link.logs.LogEngine``, in Rust; the replies are routed by the
bridge's dispatcher to the sink installed with ``set_log_sink``.
Nothing here imports the bridge, so the bridge can import this.
"""
from __future__ import annotations

import logging
from collections.abc import Callable
from typing import Any

from corvus_link import logs as _engine

logger = logging.getLogger("corvus.mavlink")


class LogTransferMixin:
    """On-board log transfer: listing, fetching and erasing the vehicle's logs."""

    _log_engine = _engine.LogEngine()

    def set_log_sink(self, sink: Callable[[Any], None] | None) -> None:
        """Route LOG_ENTRY / LOG_DATA to *sink* (None detaches)."""
        self._log_sink = sink

    def request_log_list(self, start: int = 0, end: int = 0xFFFF) -> bool:
        """Ask the vehicle to enumerate its on-board logs.

        Answered with a LOG_ENTRY per log, which the receive loop hands to the
        registered sink. Fire-and-forget: the protocol has no ACK, so
        completeness is judged from the entries themselves.
        """
        with self._operation_lock:
            return self._log_engine.request_list(self, start, end)

    def request_log_data(self, log_id: int, offset: int, count: int) -> bool:
        """Ask for one chunk of a log. Answered with LOG_DATA messages."""
        with self._operation_lock:
            return self._log_engine.request_data(self, log_id, offset, count)

    def erase_logs(self) -> bool:
        """Erase EVERY on-board log via MAVLink LOG_ERASE (121).

        There is no per-log delete in the MAVLink log protocol: LOG_ERASE
        clears the whole log directory. Refused while armed.
        """
        with self._operation_lock:
            return self._log_engine.erase(self)

    def log_request_end(self) -> bool:
        """Tell the vehicle the GCS is done reading logs.

        PX4 keeps the log session open until it hears this, which blocks
        logging of the next flight, so it is sent on every exit path.
        """
        with self._operation_lock:
            return self._log_engine.request_end(self)
