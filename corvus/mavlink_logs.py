"""On-board log transfer: listing, fetching and erasing the vehicle's logs.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`: the methods run on
the bridge and use its link, locks and state, which the bridge's ``__init__``
sets up. The LOG_* request side; the replies are routed by the bridge's dispatcher
to the sink installed with ``set_log_sink``.
Nothing here imports the bridge, so the bridge can import this.
"""
from __future__ import annotations

import logging

from collections.abc import Callable
from typing import Any

logger = logging.getLogger("corvus.mavlink")

class LogTransferMixin:
    """On-board log transfer: listing, fetching and erasing the vehicle's logs."""

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
            if not self._connection_ready():
                self._set_command_error("not connected")
                return False
            try:
                with self._send_lock:
                    self._conn.mav.log_request_list_send(
                        self._target_system, self._target_component,
                        int(start), int(end),
                    )
            except Exception as exc:  # noqa: BLE001
                self._set_command_error(f"log list request failed: {exc}")
                return False
            return True

    def request_log_data(self, log_id: int, offset: int, count: int) -> bool:
        """Ask for one chunk of a log. Answered with LOG_DATA messages."""
        with self._operation_lock:
            if not self._connection_ready():
                self._set_command_error("not connected")
                return False
            try:
                with self._send_lock:
                    self._conn.mav.log_request_data_send(
                        self._target_system, self._target_component,
                        int(log_id), int(offset), int(count),
                    )
            except Exception as exc:  # noqa: BLE001
                self._set_command_error(f"log data request failed: {exc}")
                return False
            return True

    def erase_logs(self) -> bool:
        """Erase EVERY on-board log via MAVLink LOG_ERASE (121).

        There is no per-log delete in the MAVLink log protocol — LOG_ERASE
        clears the whole log directory — so this method is named for what it
        actually does rather than for what a caller might wish it did.

        Refused while armed: it is destructive, irreversible, and has no
        business happening with a vehicle that is live.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not self._connection_ready():
                self._set_command_error("not connected")
                return False
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot erase logs while armed")
                return False
            try:
                with self._send_lock:
                    self._conn.mav.log_erase_send(
                        self._target_system, self._target_component,
                    )
            except Exception as exc:  # noqa: BLE001
                self._set_command_error(f"log erase failed: {exc}")
                return False
            self._console_publish("LOGS", "erasing all on-board logs", "warning")
            return True

    def log_request_end(self) -> bool:
        """Tell the vehicle the GCS is done reading logs.

        PX4 keeps the log session open until it hears this, which blocks
        logging of the next flight — so it is sent on every exit path, success
        or not.
        """
        with self._operation_lock:
            if not self._connection_ready():
                return False
            try:
                with self._send_lock:
                    self._conn.mav.log_request_end_send(
                        self._target_system, self._target_component,
                    )
            except Exception:  # noqa: BLE001 - best-effort teardown
                return False
            return True
