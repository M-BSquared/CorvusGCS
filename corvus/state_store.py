"""Thread-safe Vehicle State Store.

The MAVLink bridge writes telemetry here; the HTTP/SSE layer reads from it.
Ring buffers (``deque(maxlen=N)``) bound memory for historical data.
"""
from __future__ import annotations

import collections
import json
import math
import threading
import time
from typing import Any, Callable


def _sanitize(obj: Any) -> Any:
    """Replace NaN/Infinity with None so json.dumps produces valid JSON."""
    if isinstance(obj, float):
        if math.isnan(obj) or math.isinf(obj):
            return 0.0
        return obj
    if isinstance(obj, dict):
        return {k: _sanitize(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [_sanitize(v) for v in obj]
    return obj


class VehicleStateStore:
    """Central thread-safe aggregator for all vehicle telemetry."""

    def __init__(self, history_len: int = 600) -> None:
        self._lock = threading.Lock()
        self._data: dict[str, Any] = {
            "connected": False,
            "armed": False,
            "vehicle_type": "",
            "autopilot": "",
            "mode": "",
            "px4_version": "",
            "position": [0.0, 0.0],
            "altitude_amsl": 0.0,
            "altitude_agl": 0.0,
            "heading": 0.0,
            "groundspeed": 0.0,
            "airspeed": 0.0,
            "vspeed": 0.0,
            "pitch": 0.0,
            "roll": 0.0,
            "yaw": 0.0,
            "battery_percent": 0,
            "battery_voltage": 0.0,
            "battery_current": 0.0,
            "gps_fix": "",
            "gps_satellites": 0,
            "gps_hdop": 99.0,
            "uplink": 0,
            "link_status": "disconnected",
            "link_connection": "",
            "link_error": "",
            "time": "",
            "warnings": [],
            "home": [0.0, 0.0],
            "mission": [],
        }
        self._history: dict[str, collections.deque] = {
            "altitude": collections.deque(maxlen=history_len),
            "groundspeed": collections.deque(maxlen=history_len),
            "battery": collections.deque(maxlen=history_len),
        }
        self._listeners: list[Callable[[dict[str, Any]], None]] = []
        self._last_heartbeat: float = 0.0

    def _snapshot_locked(self) -> dict[str, Any]:
        snapshot = dict(self._data)
        snapshot["warnings"] = [dict(warning) for warning in self._data["warnings"]]
        return snapshot

    def update(self, **kwargs: Any) -> None:
        """Update one or more state fields and notify listeners."""
        with self._lock:
            for k, v in kwargs.items():
                if k in self._data:
                    self._data[k] = v
            if "altitude_amsl" in kwargs:
                self._history["altitude"].append(kwargs["altitude_amsl"])
            if "groundspeed" in kwargs:
                self._history["groundspeed"].append(kwargs["groundspeed"])
            if "battery_percent" in kwargs:
                self._history["battery"].append(kwargs["battery_percent"])
            snapshot = self._snapshot_locked()
            listeners = list(self._listeners)
        for fn in listeners:
            fn(snapshot)

    def merge_warning(
        self,
        text: str,
        level: str,
        meta: str | None = None,
        max_warnings: int = 20,
    ) -> None:
        """Atomically insert or refresh a warning and notify listeners."""
        severity = {"info": 0, "warning": 1, "critical": 2}
        timestamp = meta or time.strftime("%H:%M:%S")
        with self._lock:
            warnings = [dict(warning) for warning in self._data["warnings"]]
            existing = next((warning for warning in warnings if warning.get("msg") == text), None)
            if existing is None:
                warnings.append({"level": level, "msg": text, "meta": timestamp})
            else:
                existing["meta"] = timestamp
                old_level = str(existing.get("level", "info"))
                if severity.get(level, 0) > severity.get(old_level, 0):
                    existing["level"] = level
            self._data["warnings"] = warnings[-max_warnings:]
            snapshot = self._snapshot_locked()
            listeners = list(self._listeners)
        for fn in listeners:
            fn(snapshot)

    def heartbeat(self) -> None:
        """Record that a heartbeat was received."""
        with self._lock:
            self._last_heartbeat = time.time()
            self._data["connected"] = True

    def set_disconnected(self) -> None:
        """Mark vehicle as disconnected."""
        with self._lock:
            self._data["connected"] = False
            self._data["armed"] = False
            self._data["mode"] = "DISCONNECTED"
            # Keep link_connection/link_error so the UI can show the last
            # connection + last error after a disconnect; the mavlink agent
            # overrides link_status to "reconnecting" when about to retry.
            self._data["link_status"] = "disconnected"
            snapshot = self._snapshot_locked()
            listeners = list(self._listeners)
        for fn in listeners:
            fn(snapshot)

    def is_stale(self, timeout: float = 5.0) -> bool:
        """Return True if no heartbeat within *timeout* seconds."""
        with self._lock:
            if self._last_heartbeat == 0.0:
                return True
            return (time.time() - self._last_heartbeat) > timeout

    def get_snapshot(self) -> dict[str, Any]:
        """Return a shallow copy of the current state."""
        with self._lock:
            return self._snapshot_locked()

    def get_history(self, key: str) -> list[float]:
        """Return historical values for *key*."""
        with self._lock:
            return list(self._history.get(key, []))

    def add_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        """Register a listener that receives state snapshots on update."""
        with self._lock:
            self._listeners.append(fn)

    def remove_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        """Unregister a listener."""
        with self._lock:
            try:
                self._listeners.remove(fn)
            except ValueError:
                pass

    def to_json(self) -> str:
        """Serialize current state to JSON (NaN/Inf replaced with 0)."""
        return json.dumps(_sanitize(self.get_snapshot()))
