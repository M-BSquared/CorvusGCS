"""Coalescing / fast-path behaviour for ``VehicleStateStore`` notifications.

The store rate-limits listener pushes to ``NOTIFY_MIN_INTERVAL`` (~30 Hz) so
a 50 Hz telemetry stream cannot saturate the render thread, while
``IMMEDIATE_KEYS`` (arming/mode/link transitions) bypass the limiter. These
tests pin the monotonic clock so coalesce timing is deterministic instead of
wall-clock dependent; total runtime is well under one second.
"""
from __future__ import annotations

import time
from typing import Any

import pytest

from corvus.state_store import VehicleStateStore


def _frozen_clock(
    monkeypatch: pytest.MonkeyPatch, start: float = 1000.0
) -> dict[str, float]:
    """Replace ``time.monotonic`` with a controllable clock.

    Only the store's rate limiter reads ``time.monotonic``; freezing it makes
    coalesce-window assertions deterministic regardless of machine speed.
    Returns a mutable holder so a test can advance the clock past the window.
    """
    clock = {"t": start}
    monkeypatch.setattr(time, "monotonic", lambda: clock["t"])
    return clock


def test_rapid_non_immediate_updates_are_coalesced(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A burst of 100 high-frequency roll updates at one instant fires the
    listener once; the other 99 are coalesced into the next flush."""
    _frozen_clock(monkeypatch)
    store = VehicleStateStore()
    received: list[dict[str, Any]] = []
    store.add_listener(received.append)

    for i in range(100):
        store.update(roll=float(i))

    # First call notifies (start - 0 >= interval); the next 99 are coalesced.
    assert len(received) == 1
    assert received[0]["roll"] == 0.0
    # The latest value is in state even though its push was coalesced.
    assert store.get_snapshot()["roll"] == 99.0


def test_coalesced_update_flushes_after_interval_elapses(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A non-immediate update that was coalesced fires once the clock advances
    past NOTIFY_MIN_INTERVAL (rate limiter, latest snapshot wins)."""
    clock = _frozen_clock(monkeypatch)
    store = VehicleStateStore()
    received: list[dict[str, Any]] = []
    store.add_listener(received.append)

    store.update(roll=1.0)  # first call -> notify
    store.update(roll=2.0)  # same instant -> coalesced
    assert len(received) == 1

    clock["t"] += VehicleStateStore.NOTIFY_MIN_INTERVAL + 0.01
    store.update(roll=3.0)  # past window -> notify latest
    assert len(received) == 2
    assert received[-1]["roll"] == 3.0


def test_immediate_key_bypasses_coalesce_window(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """``armed`` (an IMMEDIATE_KEY) notifies on the call even inside the
    coalesce window where a non-immediate update would be dropped."""
    _frozen_clock(monkeypatch)
    store = VehicleStateStore()
    received: list[dict[str, Any]] = []
    store.add_listener(received.append)

    store.update(roll=0.1)  # first call -> notify
    store.update(roll=0.2)  # same instant -> coalesced
    assert len(received) == 1

    store.update(armed=True)  # immediate -> notifies now
    assert len(received) == 2
    assert received[-1]["armed"] is True


def test_heartbeat_first_call_notifies_connected_transition(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The False->True connected transition from ``heartbeat`` fires
    immediately; subsequent heartbeats do not re-notify."""
    _frozen_clock(monkeypatch)
    store = VehicleStateStore()
    received: list[dict[str, Any]] = []
    store.add_listener(received.append)

    assert store.get_snapshot()["connected"] is False
    store.heartbeat()
    assert len(received) == 1
    assert received[-1]["connected"] is True

    store.heartbeat()  # already connected -> no new push
    assert len(received) == 1


def test_set_disconnected_notifies_immediately(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """``set_disconnected`` bypasses the coalesce window (safety transition),
    even when a non-immediate update immediately before it was coalesced."""
    _frozen_clock(monkeypatch)
    store = VehicleStateStore()
    received: list[dict[str, Any]] = []
    store.add_listener(received.append)

    store.update(roll=1.0)  # first call -> notify
    store.update(roll=2.0)  # same instant -> coalesced
    assert len(received) == 1

    store.set_disconnected()
    assert len(received) == 2
    assert received[-1]["connected"] is False
    assert received[-1]["armed"] is False
    assert received[-1]["mode"] == "DISCONNECTED"
    assert received[-1]["link_status"] == "disconnected"


def test_get_snapshot_reflects_coalesced_updates(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """``get_snapshot`` returns the live state even when its push was
    coalesced (the listener only sees the flushed snapshot)."""
    _frozen_clock(monkeypatch)
    store = VehicleStateStore()
    received: list[dict[str, Any]] = []
    store.add_listener(received.append)

    store.update(roll=1.0)  # notify
    store.update(roll=2.0)  # coalesced
    store.update(roll=3.0)  # coalesced

    assert len(received) == 1
    assert received[-1]["roll"] == 1.0  # flushed snapshot is stale-ish
    assert store.get_snapshot()["roll"] == 3.0  # live state is latest


def test_shutdown_is_idempotent_noop() -> None:
    """``shutdown`` owns no threads in the rate-limiter design; calling it
    twice must not raise and leaves the store usable."""
    store = VehicleStateStore()
    store.shutdown()
    store.shutdown()

    # Store still functional after shutdown (no-op, no state torn down).
    received: list[dict[str, Any]] = []
    store.add_listener(received.append)
    store.update(armed=True)
    assert received[-1]["armed"] is True
