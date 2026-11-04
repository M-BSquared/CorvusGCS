"""The bridge's RTCM injection, as Corvus had it in Python.

Kept unchanged as the reference tests/test_rtk_parity.py holds the Rust
injection to; not imported by the app. Mixed into a bridge in the tests.
"""
from __future__ import annotations

import logging
import time
from typing import Any

from tests.reference import rtk_python as rtk

logger = logging.getLogger("corvus.mavlink")


class RtcmInjectionMixin:
    def inject_rtcm(self, frame: bytes) -> bool:
        """Put one RTCM 3 correction on the link as GPS_RTCM_DATA.

        Called from the RTK service's reader thread at a few hertz for as long
        as a base station is streaming. Three things about it are deliberate:

        **It does not take the operation lock.** Every other send path here
        serialises behind ``_operation_lock`` because it is a *transaction* —
        a command awaiting its ACK, a parameter awaiting its echo. A
        correction is neither: it is fire-and-forget, it arrives whether or
        not anything else is in progress, and queueing it behind a mission
        upload would stall the corrections for the length of the upload. Only
        ``_send_lock`` is held, which is the same guarantee the heartbeat loop
        works under.

        **It never raises.** The caller is a reader thread whose other job is
        the serial port; a throw here would end the RTK session over a link
        that had merely gone away for a moment. A refusal is counted and
        reported through :meth:`rtcm_stats` instead, which is what the page
        shows.

        **It drops rather than truncates.** A correction too long to express
        in four fragments (see :func:`corvus.rtk.fragments`) is discarded
        whole and counted, because a receiver that reassembles three quarters
        of a message does not get three quarters of a fix — it gets a CRC
        failure, having waited for the rest first.

        Returns True when every fragment was written.
        """
        if not frame:
            return False
        if not self._connection_ready():
            with self._rtcm_lock:
                self._rtcm_dropped += 1
            return False
        with self._rtcm_lock:
            sequence = self._rtcm_sequence
            self._rtcm_sequence = (sequence + 1) & 0x1F
        parts = rtk.fragments(frame, sequence)
        if not parts:
            with self._rtcm_lock:
                self._rtcm_dropped += 1
            logger.warning(
                "RTCM message of %d bytes is too long for GPS_RTCM_DATA; dropped",
                len(frame),
            )
            return False
        try:
            with self._send_lock:
                conn = self._conn
                if conn is None:
                    raise RuntimeError("link closed")
                for flags, chunk in parts:
                    # The field is a fixed 180 bytes; the length is carried
                    # separately, so the tail is padding and not data.
                    conn.mav.gps_rtcm_data_send(
                        flags, len(chunk),
                        list(chunk) + [0] * (rtk.FRAGMENT_BYTES - len(chunk)),
                    )
        except Exception as exc:  # noqa: BLE001 - see the docstring
            with self._rtcm_lock:
                self._rtcm_dropped += 1
            logger.debug("RTCM injection failed: %s", exc)
            return False
        with self._rtcm_lock:
            self._rtcm_bytes += len(frame)
            self._rtcm_messages += 1
            self._rtcm_last_send = time.monotonic()
        return True

    def rtcm_stats(self) -> dict[str, Any]:
        """What has actually reached the aircraft, for the RTK page.

        Separate from what the base *produced*, which the service counts: a
        base streaming happily into a link that is down is the failure this
        pair of numbers exists to make visible.
        """
        with self._rtcm_lock:
            last = self._rtcm_last_send
            return {
                "bytes": self._rtcm_bytes,
                "messages": self._rtcm_messages,
                "dropped": self._rtcm_dropped,
                "age": (time.monotonic() - last) if last else None,
            }

    def forget_rtcm_session(self) -> None:
        """Reset the injection counters when the correction source changes."""
        with self._rtcm_lock:
            self._rtcm_bytes = 0
            self._rtcm_messages = 0
            self._rtcm_dropped = 0
            self._rtcm_last_send = 0.0
