"""Live per-message statistics for Live Messages on the Analysis page.

The bridge hands every decoded frame to :meth:`MessageScope.record`. Only a
counter, two timestamps and a reference to the newest message are kept per
(system, component, message) key, so the receive loop pays a dictionary lookup
and no field conversion. Nothing is counted at all unless the page is polling:
every snapshot keeps the counter awake for a few seconds, and it goes back to
sleep (and forgets its numbers) when the page closes or pauses. Fields are turned into JSON when somebody asks for one
message, never for the whole stream.
"""
from __future__ import annotations

import math
import threading
import time
from typing import Any

MAX_KEYS = 512
RATE_WINDOW_S = 1.0
SILENT_AFTER_S = 2.5
ACTIVE_FOR_S = 3.0
MAX_STRING = 256
MAX_LIST = 64


class _Entry:
    __slots__ = ("msg_id", "count", "first", "last", "rate", "win_start",
                 "win_count", "size", "msg")

    def __init__(self, msg_id: int, now: float) -> None:
        self.msg_id = msg_id
        self.count = 0
        self.first = now
        self.last = now
        self.rate = 0.0
        self.win_start = now
        self.win_count = 0
        self.size = 0
        self.msg: Any = None


def _json_safe(value: Any, depth: int = 0) -> Any:
    """A MAVLink field value as something ``json.dumps`` accepts."""
    if isinstance(value, bool) or value is None:
        return value
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, (bytes, bytearray)):
        text = bytes(value).split(b"\x00", 1)[0]
        return text.decode("utf-8", "replace")[:MAX_STRING]
    if isinstance(value, str):
        return value[:MAX_STRING]
    if isinstance(value, (list, tuple)) and depth < 2:
        return [_json_safe(v, depth + 1) for v in list(value)[:MAX_LIST]]
    return str(value)[:MAX_STRING]


def key_for(system: int, component: int, name: str) -> str:
    """The identifier the page uses to select one row."""
    return f"{system}:{component}:{name}"


class MessageScope:
    """Thread-safe table of every MAVLink message type seen on the link."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._entries: dict[tuple[int, int, str], _Entry] = {}
        self._since = time.time()
        self._dropped = 0
        self._active_until = 0.0

    def record(self, msg: Any, now: float | None = None) -> None:
        """Count one decoded message. Never raises into the receive loop.

        Returns at once while nobody is watching, so the receive loop pays
        one clock read and one comparison.
        """
        if time.monotonic() > self._active_until:
            return
        try:
            name = msg.get_type()
            if name == "BAD_DATA":
                return
            system = int(msg.get_srcSystem())
            component = int(msg.get_srcComponent())
            msg_id = int(msg.get_msgId())
        except Exception:  # noqa: BLE001 - an odd frame is skipped, not fatal
            return
        t = time.monotonic() if now is None else now
        key = (system, component, name)
        with self._lock:
            entry = self._entries.get(key)
            if entry is None:
                if len(self._entries) >= MAX_KEYS:
                    self._dropped += 1
                    return
                entry = self._entries[key] = _Entry(msg_id, t)
            entry.count += 1
            entry.win_count += 1
            entry.last = t
            entry.msg = msg
            elapsed = t - entry.win_start
            if elapsed >= RATE_WINDOW_S:
                entry.rate = entry.win_count / elapsed
                entry.win_start = t
                entry.win_count = 0
            try:
                entry.size = len(msg.get_msgbuf())
            except Exception:  # noqa: BLE001
                pass

    def clear(self) -> None:
        """Forget everything, e.g. when the operator presses Clear."""
        with self._lock:
            self._entries.clear()
            self._dropped = 0
            self._since = time.time()

    def snapshot(self, select: str = "", now: float | None = None) -> dict[str, Any]:
        """Rows for every message type, plus the fields of the *select* row.

        Asking also wakes the counter; the first ask after a quiet spell starts
        a fresh count.
        """
        real = time.monotonic()
        t = real if now is None else now
        rows: list[dict[str, Any]] = []
        chosen: Any = None
        chosen_key = ""
        with self._lock:
            if real > self._active_until:
                self._entries.clear()
                self._dropped = 0
                self._since = time.time()
            self._active_until = real + ACTIVE_FOR_S
            for (system, component, name), e in self._entries.items():
                age = max(0.0, t - e.last)
                silent = age > SILENT_AFTER_S
                key = key_for(system, component, name)
                rows.append({
                    "key": key,
                    "system": system,
                    "component": component,
                    "name": name,
                    "id": e.msg_id,
                    "count": e.count,
                    "rate": 0.0 if silent else round(e.rate, 2),
                    "age": round(age, 2),
                    "size": e.size,
                })
                if select and key == select:
                    chosen = e.msg
                    chosen_key = key
            total = sum(e.count for e in self._entries.values())
            dropped = self._dropped
            since = self._since
        rows.sort(key=lambda r: (r["system"], r["component"], r["name"]))
        fields: list[dict[str, Any]] | None = None
        if chosen is not None:
            fields = self._fields(chosen)
        return {
            "rows": rows,
            "total": total,
            "dropped": dropped,
            "since": since,
            "selected": chosen_key if chosen is not None else "",
            "fields": fields,
        }

    @staticmethod
    def _fields(msg: Any) -> list[dict[str, Any]]:
        """Name, value and unit of every field of one message, in wire order."""
        try:
            names = list(msg.get_fieldnames())
        except Exception:  # noqa: BLE001
            return []
        units = getattr(msg, "fieldunits_by_name", None) or {}
        out: list[dict[str, Any]] = []
        for field in names:
            try:
                value = getattr(msg, field)
            except Exception:  # noqa: BLE001
                continue
            out.append({
                "name": field,
                "value": _json_safe(value),
                "unit": units.get(field, "") if isinstance(units, dict) else "",
            })
        return out
