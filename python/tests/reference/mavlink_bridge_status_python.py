"""What the vehicle says in words, as Corvus had it in Python: STATUSTEXT
reassembly and publishing, the vehicle events (the kill switch), and the
preflight report requests.

Kept unchanged as the reference tests/test_bridge_status_parity.py holds the
Rust engine to; not imported by the app.
"""
from __future__ import annotations

import logging
import time
from typing import Any

from corvus_link import mavutil

from corvus.autopilot import VehicleEvent
from corvus.mavlink_bridge import (
    _LEVEL_RANK,
    KILL_RECHECK_S,
    KILL_SETTLE_S,
    PREARM_CHANGE_REQUEST_MIN_S,
    PREARM_REASONS_MAX,
    PREARM_REPORT_GAP_S,
    PREARM_REPORT_REQUEST_EVERY_S,
    STATUSTEXT_CHUNK_BYTES,
    STATUSTEXT_CHUNK_TIMEOUT_S,
    STATUSTEXT_MAX_PARTIALS,
)

logger = logging.getLogger("corvus.mavlink")


class StatusMixin:
    """STATUSTEXT, vehicle events and the preflight report, as the bridge had them."""

    def _handle_statustext(self, msg: Any) -> None:
        """Reassemble MAVLink2 STATUSTEXT chunks and publish complete messages."""
        now = time.monotonic()
        self._cleanup_statustext_chunks(now)
        raw_value = msg.text
        if isinstance(raw_value, bytes):
            raw_bytes = raw_value
            text = raw_bytes.split(b"\x00", 1)[0].decode("utf-8", errors="replace")
        else:
            raw_text = str(raw_value)
            raw_bytes = raw_text.encode("utf-8", errors="replace")
            text = raw_text.split("\x00", 1)[0]
        severity = int(msg.severity)
        chunk_id = int(getattr(msg, "id", 0))
        chunk_seq = int(getattr(msg, "chunk_seq", 0))
        has_terminator = b"\x00" in raw_bytes
        is_final = has_terminator or len(raw_bytes) < STATUSTEXT_CHUNK_BYTES

        if chunk_id == 0:
            self._publish_statustext(text, severity)
            return

        source_system = getattr(msg, "get_srcSystem", lambda: 0)()
        source_component = getattr(msg, "get_srcComponent", lambda: 0)()
        key = (int(source_system), int(source_component), chunk_id)
        if (
            key not in self._statustext_chunks
            and len(self._statustext_chunks) >= STATUSTEXT_MAX_PARTIALS
        ):
            # Evict the least recently updated rather than refusing the new
            # one: the oldest partial is the one least likely to still be
            # completed, and dropping the newest would stall the live message.
            oldest = min(
                self._statustext_chunks,
                key=lambda k: float(self._statustext_chunks[k]["updated"]),
            )
            self._statustext_chunks.pop(oldest, None)
        partial = self._statustext_chunks.setdefault(
            key,
            {"parts": {}, "final_seq": None, "severity": severity, "updated": now},
        )
        partial["parts"][chunk_seq] = text
        partial["severity"] = min(int(partial["severity"]), severity)
        partial["updated"] = now
        if is_final:
            partial["final_seq"] = chunk_seq
        final_seq = partial["final_seq"]
        if final_seq is None or not all(seq in partial["parts"] for seq in range(final_seq + 1)):
            return
        complete = "".join(partial["parts"][seq] for seq in range(final_seq + 1))
        complete_severity = int(partial["severity"])
        self._statustext_chunks.pop(key, None)
        self._publish_statustext(complete, complete_severity)

    def _cleanup_statustext_chunks(self, now: float | None = None) -> None:
        current = time.monotonic() if now is None else now
        stale = [
            key for key, partial in self._statustext_chunks.items()
            if current - float(partial["updated"]) > STATUSTEXT_CHUNK_TIMEOUT_S
        ]
        for key in stale:
            self._statustext_chunks.pop(key, None)

    def _publish_statustext(self, text: str, severity: int) -> None:
        # PX4 ends a STATUSTEXT that doubles one of its events with a tab (a
        # marker for stations that show the event instead). Corvus shows the
        # text, and without the strip the same message is two warnings.
        text = text.rstrip()
        level = "critical" if severity <= 3 else ("warning" if severity <= 5 else "info")
        self._console_publish("STATUSTEXT", text, level)
        event = self._dialect.vehicle_event(text)
        if event is not None:
            self._note_vehicle_event(event, level)
            return
        reason = self._dialect.prearm_failure(text)
        if reason:
            self._note_prearm_failure(reason)
            kill = self._dialect.prearm_event(reason)
            if kill is not None:
                self._prearm_report_kill = True
                self._kill_evidence_at = time.monotonic()
                # The switch, named by the check it fails because its own line
                # was never seen. Said once, in that line's words, so the board
                # and the bar call it the same thing however it was learned.
                if not self._store.get_snapshot().get("kill_switch"):
                    self._note_vehicle_event(kill, level)
                return
        if level != "info" or not self._dialect.is_routine_statustext(text):
            self._store_update_warning(text, level, event="prearm" if reason else None)

    def _note_vehicle_event(self, event: VehicleEvent, level: str) -> None:
        """Latch the kill switch a line reports and put the line on the board."""
        if event.kind == "kill":
            self._kill_evidence_at = time.monotonic()
            self._store.update(kill_switch=True)
        worst = max(level, event.level, key=_LEVEL_RANK.__getitem__)
        self._store_update_warning(event.message, worst, event=event.kind)
        if event.kind == "unkill":
            self._release_kill_switch()

    def _release_kill_switch(self) -> None:
        """The switch is off: clear it, and the lockdown blamed on it.

        PX4 reports the kill switch as flight termination in a heartbeat that
        comes once a second, so waiting for the next one had the bar read
        TERMINATED for up to a second after every release. A termination that
        really is something else is back with that heartbeat.
        """
        changes: dict[str, Any] = {"kill_switch": False}
        if self._store.get_snapshot().get("kill_switch"):
            changes["flight_termination"] = False
        self._store.update(**changes)
        self._store.resolve_warnings({"kill"})

    def _note_prearm_failure(self, reason: str) -> None:
        now = time.monotonic()
        if now - self._prearm_reason_at > PREARM_REPORT_GAP_S:
            self._prearm_reasons = []
            self._prearm_report_started_at = now
            self._prearm_report_kill = False
        self._prearm_reason_at = now
        if reason in self._prearm_reasons or len(self._prearm_reasons) >= PREARM_REASONS_MAX:
            return
        self._prearm_reasons.append(reason)
        self._store.update(prearm_reasons=list(self._prearm_reasons))

    def _clear_prearm_reasons(self) -> None:
        self._prearm_reasons = []
        self._prearm_reason_at = 0.0
        self._store.update(prearm_reasons=[])

    def _note_prearm_state(self, prearm: bool | None) -> None:
        """React to SYS_STATUS's verdict: clear the reasons, or go and get them."""
        now = time.monotonic()
        previous, self._prearm_last = self._prearm_last, prearm
        snapshot = self._store.get_snapshot()
        if prearm is True:
            if self._prearm_reasons:
                self._clear_prearm_reasons()
            self._store.resolve_warnings({"prearm"})
            # Disarmed and ready means the switch is off: an engaged one fails
            # a check of its own on both stacks. Armed, PX4 skips that check,
            # so a pass says nothing about the switch. Nor does a pass that
            # may predate the switch's own line (KILL_SETTLE_S).
            if not snapshot.get("armed"):
                if snapshot.get("kill_switch"):
                    if now - self._kill_evidence_at >= KILL_SETTLE_S:
                        self._release_kill_switch()
                else:
                    self._store.resolve_warnings({"kill"})
            return
        if prearm is not False:
            return
        self._check_kill_released_by_report(now, snapshot)
        if (
            previous is True and not snapshot.get("armed")
            and now - self._prearm_requested_at >= PREARM_CHANGE_REQUEST_MIN_S
        ):
            # Ready a moment ago, not now. If that was a kill switch whose
            # line was lost, the report names it within a second.
            self._prearm_requested_at = now
            self._request_prearm_report()
            return
        if (
            snapshot.get("kill_switch") and not snapshot.get("armed")
            and self._dialect.prearm_report_complete
            and now - max(self._prearm_requested_at, self._kill_evidence_at) >= KILL_RECHECK_S
        ):
            self._prearm_requested_at = now
            self._request_prearm_report()
            return
        if self._prearm_reasons:
            return
        if now - self._prearm_requested_at < PREARM_REPORT_REQUEST_EVERY_S:
            return
        self._prearm_requested_at = now
        self._request_prearm_report()

    def _check_kill_released_by_report(self, now: float, snapshot: dict[str, Any]) -> None:
        """Release the switch when a whole report asked for since says nothing of it.

        Only on a stack whose report names every failing check, only while
        disarmed (the checks do not run armed), and only for a report that
        began after the last request, which was itself made after the switch
        was last reported engaged. A report still arriving is not judged.
        """
        if not snapshot.get("kill_switch") or snapshot.get("armed"):
            return
        if not self._dialect.prearm_report_complete:
            return
        if self._prearm_report_kill or not self._prearm_report_started_at:
            return
        if self._prearm_requested_at <= self._kill_evidence_at:
            return
        if self._prearm_report_started_at < self._prearm_requested_at:
            return
        if now - self._prearm_reason_at <= PREARM_REPORT_GAP_S:
            return
        self._release_kill_switch()

    def _request_prearm_report(self) -> None:
        """Ask for the preflight report now. Fire and forget, like _request_home.

        Runs on the receive thread, so it cannot wait for its own ACK; whether
        it worked is judged by the reasons arriving.
        """
        conn = self._conn
        if conn is None:
            return
        nan = float("nan")
        try:
            with self._send_lock:
                if conn is not self._conn:
                    return
                conn.mav.command_long_send(
                    self._target_system, self._target_component,
                    mavutil.mavlink.MAV_CMD_RUN_PREARM_CHECKS, 0,
                    nan, nan, nan, nan, nan, nan, nan,
                )
        except Exception as exc:  # noqa: BLE001 - the next SYS_STATUS asks again
            logger.debug("prearm report request failed: %s", exc)
