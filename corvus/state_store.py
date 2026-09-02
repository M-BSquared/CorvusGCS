"""Thread-safe Vehicle State Store.

The MAVLink bridge writes telemetry here; the HTTP/SSE layer reads from it.
Ring buffers (``deque(maxlen=N)``) bound memory for historical data.

Listener notifications are coalesced to ``NOTIFY_MIN_INTERVAL`` (~30 Hz): a
50 Hz telemetry stream (ATTITUDE, GLOBAL_POSITION_INT, ...) cannot saturate
the render thread, and each listener always receives the latest snapshot.
Fields in ``IMMEDIATE_KEYS`` (arming/mode/link transitions) bypass the
limiter and fire on the call so critical changes are never delayed.
``get_snapshot`` always returns the live current state regardless of
coalescing; only the listener push is rate-limited, mirroring the SSE
latest-wins buffer.

Snapshot caching: every mutation bumps a monotonic ``_data_version`` and
invalidates the cached snapshot. ``get_snapshot`` rebuilds the cache only
when the version changed, so a 50 Hz telemetry stream that mutates state but
whose listener push was coalesced still serves a fresh snapshot on the next
read while consecutive reads with no intervening mutation return the same
object (callers treat it as read-only). The SSE layer keys JSON-serialization
reuse on ``version()`` so N browser tabs re-serialize one snapshot once per
version instead of N times.
"""
from __future__ import annotations

import collections
import json
import math
import threading
import time
from typing import Any, Callable


def _sanitize(obj: Any) -> Any:
    """Replace NaN/Infinity with 0.0 so json.dumps produces valid JSON."""
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

    # Cap listener notification bursts at ~30 Hz. High-frequency telemetry
    # (50 Hz ATTITUDE, 10 Hz GLOBAL_POSITION_INT, ...) is coalesced so each
    # listener fires at most ~30 times/sec, carrying the latest snapshot.
    NOTIFY_MIN_INTERVAL: float = 1 / 30

    # State changes that must reach listeners instantly, skipping the
    # coalesce window: arming, flight mode, and link status/quality/connected
    # transitions are user-visible safety events, not smooth telemetry.
    IMMEDIATE_KEYS: frozenset[str] = frozenset(
        {"armed", "mode", "link_status", "link_quality", "connected"}
    )

    def __init__(self, history_len: int = 600) -> None:
        self._lock = threading.Lock()
        self._data: dict[str, Any] = {
            "connected": False,
            "armed": False,
            "vehicle_type": "",
            "autopilot": "",
            "mode": "",
            "px4_version": "",
            "px4_version_detail": "",
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
            "rollspeed": 0.0,
            "pitchspeed": 0.0,
            "yawspeed": 0.0,
            "battery_percent": 0,
            "battery_voltage": 0.0,
            "battery_current": 0.0,
            "gps_fix": "",
            "gps_satellites": 0,
            "gps_hdop": 99.0,
            "vibration_x": 0.0,
            "vibration_y": 0.0,
            "vibration_z": 0.0,
            "clipping_0": 0,
            "clipping_1": 0,
            "clipping_2": 0,
            "uplink": 0,
            "uplink_rssi": 0.0,
            "uplink_rxerrors": 0,
            "uplink_fixed": 0,
            "heartbeat_jitter_ms": 0.0,
            "link_quality": "unknown",
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
        # Monotonic ts of the last listener notification. Starts at 0.0 so the
        # first update after construction always notifies (monotonic time is
        # far larger than NOTIFY_MIN_INTERVAL at process start).
        self._last_notify: float = 0.0
        # Snapshot cache: incremented on every state mutation so a read can
        # rebuild the cache only when state actually changed. ``_snapshot_version``
        # starts at -1 so the first ``_snapshot_locked`` always builds the cache.
        self._data_version: int = 0
        self._snapshot_cache: dict[str, Any] | None = None
        self._snapshot_version: int = -1

    def _snapshot_locked(self) -> dict[str, Any]:
        """Under ``self._lock``: return the cached snapshot, rebuilding only
        when ``_data_version`` advanced since the last build.

        The cached dict aliases ``self._data``'s scalar values (immutable, so
        safe to share) but owns a defensive copy of the warnings list (rebuilt
        from ``self._data["warnings"]``), so a caller mutating the returned
        ``warnings`` list cannot corrupt the store's canonical warnings.
        Callers and listeners receive the same object and MUST treat it as
        read-only; mutating it cannot affect the store (defensive copy) but
        will pollute the shared cache for other readers until the next
        mutation invalidates it.
        """
        if self._snapshot_version != self._data_version:
            snapshot = dict(self._data)
            snapshot["warnings"] = [dict(warning) for warning in self._data["warnings"]]
            self._snapshot_cache = snapshot
            self._snapshot_version = self._data_version
        return self._snapshot_cache  # type: ignore[return-value]

    def _dispatch_locked(
        self, immediate: bool
    ) -> tuple[dict[str, Any] | None, list[Callable[[dict[str, Any]], None]]]:
        """Under ``self._lock``: rate-limit and stage a listener notification.

        Returns ``(snapshot, listeners)`` when listeners should be called now,
        or ``(None, [])`` when the update is coalesced into the next flush.
        ``immediate`` bypasses the rate limit for arming/mode/link transitions
        so they are never delayed. The snapshot and listener copy are built
        here so the caller can release the lock before invoking listeners;
        listeners must never be called back under ``self._lock``. Because the
        coalesced path drops intermediate snapshots, listeners see the latest
        state at each flush -- the same latest-wins semantics as the SSE buffer
        -- while the fast path guarantees transitions arrive instantly.

        Hybrid fast path: with no listeners subscribed there is nobody to
        push to, so skip the clock read and snapshot build entirely. This
        keeps the no-subscriber hot path a plain data write and leaves
        ``time.monotonic`` call counts unchanged for callers that pin the
        clock (e.g. heartbeat-jitter tests), preserving exact-call-count
        expectations. State still mutates, so ``get_snapshot`` stays current.
        """
        if not self._listeners:
            return None, []
        now = time.monotonic()
        if immediate or (now - self._last_notify) >= self.NOTIFY_MIN_INTERVAL:
            self._last_notify = now
            return self._snapshot_locked(), list(self._listeners)
        return None, []

    def update(self, **kwargs: Any) -> None:
        """Update one or more state fields and notify listeners.

        Non-immediate fields are coalesced to ``<= NOTIFY_MIN_INTERVAL``
        (~30 Hz); any key in ``IMMEDIATE_KEYS`` (armed/mode/link_status/
        link_quality/connected) fires immediately. The snapshot always
        reflects every update via ``get_snapshot``; only the listener push is
        rate-limited.
        """
        immediate = any(k in self.IMMEDIATE_KEYS for k in kwargs)
        with self._lock:
            mutated = False
            for k, v in kwargs.items():
                if k in self._data:
                    self._data[k] = v
                    mutated = True
            if "altitude_amsl" in kwargs:
                self._history["altitude"].append(kwargs["altitude_amsl"])
            if "groundspeed" in kwargs:
                self._history["groundspeed"].append(kwargs["groundspeed"])
            if "battery_percent" in kwargs:
                self._history["battery"].append(kwargs["battery_percent"])
            # Bump the snapshot-cache version only when state actually changed
            # (an update of only unknown keys is a no-op), so reads stay cached.
            if mutated:
                self._data_version += 1
            snapshot, listeners = self._dispatch_locked(immediate)
        if snapshot is not None:
            for fn in listeners:
                fn(snapshot)

    def merge_warning(
        self,
        text: str,
        level: str,
        meta: str | None = None,
        max_warnings: int = 20,
    ) -> None:
        """Atomically insert or refresh a warning and notify listeners.

        Warning notifications share the coalesce window with telemetry; the
        warning is always written to state immediately, so ``get_snapshot``
        reflects it even when the listener push is deferred to the next flush.
        """
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
            self._data_version += 1
            snapshot, listeners = self._dispatch_locked(immediate=False)
        if snapshot is not None:
            for fn in listeners:
                fn(snapshot)

    def heartbeat(self) -> None:
        """Record that a heartbeat was received.

        On the False->True connected transition, notify listeners immediately
        so the UI flips to "connected" without waiting for the next telemetry
        update; subsequent heartbeats only refresh the staleness clock and do
        not re-notify.
        """
        with self._lock:
            was_connected = self._data["connected"]
            self._last_heartbeat = time.time()
            self._data["connected"] = True
            if was_connected:
                snapshot, listeners = None, []
            else:
                # False->True transition mutates state; bump the cache version
                # so the next read rebuilds with connected=True. A subsequent
                # heartbeat on an already-connected store changes nothing.
                self._data_version += 1
                snapshot, listeners = self._dispatch_locked(immediate=True)
        if snapshot is not None:
            for fn in listeners:
                fn(snapshot)

    def set_disconnected(self) -> None:
        """Mark vehicle as disconnected; always notifies immediately."""
        with self._lock:
            self._data["connected"] = False
            self._data["armed"] = False
            self._data["mode"] = "DISCONNECTED"
            # Keep link_connection/link_error so the UI can show the last
            # connection + last error after a disconnect; the mavlink agent
            # overrides link_status to "reconnecting" when about to retry.
            self._data["link_status"] = "disconnected"
            self._data_version += 1
            snapshot, listeners = self._dispatch_locked(immediate=True)
        if snapshot is not None:
            for fn in listeners:
                fn(snapshot)

    def is_stale(self, timeout: float = 5.0) -> bool:
        """Return True if no heartbeat within *timeout* seconds."""
        with self._lock:
            if self._last_heartbeat == 0.0:
                return True
            return (time.time() - self._last_heartbeat) > timeout

    def get_snapshot(self) -> dict[str, Any]:
        """Return a read-only snapshot of the current state.

        The snapshot is cached and rebuilt only when a mutation bumps the
        internal version; consecutive calls with no intervening mutation
        return the same dict object. Callers MUST treat it as read-only:
        the ``warnings`` list is a defensive copy so mutating it cannot reach
        the store's canonical warnings, but mutating the returned dict will
        corrupt the shared cache for other readers until the next mutation.
        """
        with self._lock:
            return self._snapshot_locked()

    def get_history(self, key: str) -> list[float]:
        """Return historical values for *key*."""
        with self._lock:
            return list(self._history.get(key, []))

    def version(self) -> int:
        """Return the monotonic mutation counter.

        Incremented on every state mutation (``update`` of a recognized key,
        ``merge_warning``, ``set_disconnected``, and the ``heartbeat`` False->True
        connected transition). The SSE layer keys JSON-serialization reuse on
        this value: the same version yields a byte-identical snapshot, so the
        serialized form is computed once per version and reused for every
        connected tab.
        """
        with self._lock:
            return self._data_version

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

    def shutdown(self) -> None:
        """Idempotent lifecycle hook.

        The rate-limiter design owns no background threads, so there is
        nothing to stop or join. Kept as a no-op so the backend/app shutdown
        path can call it uniformly without the store needing special casing.
        """
        return

    def to_json(self) -> str:
        """Serialize current state to JSON (NaN/Inf replaced with 0)."""
        return json.dumps(_sanitize(self.get_snapshot()))
