"""Link-quality tracking and two-tier reconnect regression tests.

Integration-level regression for the mavlink wave: RADIO_STATUS must flow
through ``MavlinkBridge._dispatch`` into the real ``VehicleStateStore``
snapshot (not just a method return), the two-tier heartbeat state machine
must degrade-don't-drop below the DROP timeout and tear down past it, and
``_reconnect_delay`` must be a non-decreasing exponential capped at 8 s.

The fakes and clock helpers are reused from ``tests/test_mavlink_linkquality.py``
so this file stays in lock-step with the unit suite without duplicating the
machinery.
"""
from __future__ import annotations

import sys
from typing import Any

import pytest

from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore

_TESTS_DIR = __import__("pathlib").Path(__file__).resolve().parent
if str(_TESTS_DIR) not in sys.path:
    sys.path.insert(0, str(_TESTS_DIR))

# Reuse the shared fake-conn / clock helpers (same pattern as
# test_integration_review.py importing from test_mavlink_takeoff).
from test_mavlink_linkquality import (  # noqa: E402
    _FakeClock,
    _bridge_for_receive_test,
    radio_status,
    ready_bridge,
)


# ---------------------------------------------------------------------------
# 1. RADIO_STATUS -> store snapshot (integration: store-after-bridge)
# ---------------------------------------------------------------------------

def test_radio_status_populates_all_link_fields_in_store() -> None:
    """A RADIO_STATUS dispatch reaches every link-quality field in the live
    store snapshot (uplink score + raw radio metrics + quality string)."""
    bridge = ready_bridge()
    bridge._dispatch(radio_status(
        rssi=200, remrssi=150, txbuf=100,
        noise=10, remnoise=20, rxerrors=3, fixed=1,
    ))
    snap = bridge._store.get_snapshot()
    assert snap["uplink"] == 100
    assert snap["uplink_rssi"] == 150.0
    assert snap["uplink_rxerrors"] == 3
    assert snap["uplink_fixed"] == 1
    # link_quality is derived and must be a non-empty known state.
    assert snap["link_quality"] in {"good", "fair", "poor"}
    assert snap["link_quality"] == "good"


def test_radio_status_unknown_remrssi_keeps_last_uplink_in_store() -> None:
    """A momentary 0/255 remrssi must not flap the stored uplink to 0."""
    bridge = ready_bridge()
    bridge._dispatch(radio_status(remrssi=150, txbuf=100))
    assert bridge._store.get_snapshot()["uplink"] == 100
    bridge._dispatch(radio_status(remrssi=0, txbuf=100, rxerrors=7, fixed=2))
    snap = bridge._store.get_snapshot()
    assert snap["uplink"] == 100  # unchanged
    assert snap["uplink_rssi"] == 0.0
    assert snap["uplink_rxerrors"] == 7
    assert snap["uplink_fixed"] == 2


def test_store_link_quality_fields_have_defaults_before_any_radio() -> None:
    """A fresh store exposes the link-quality fields with safe defaults so
    the UI never reads an undefined key before the first RADIO_STATUS."""
    store = VehicleStateStore()
    snap = store.get_snapshot()
    assert snap["uplink"] == 0
    assert snap["uplink_rssi"] == 0.0
    assert snap["uplink_rxerrors"] == 0
    assert snap["uplink_fixed"] == 0
    assert snap["link_quality"] == "unknown"


# ---------------------------------------------------------------------------
# 2. Two-tier heartbeat state machine (degrade-don't-drop)
# ---------------------------------------------------------------------------

def test_receive_loop_warn_gap_degrades_without_dropping(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A heartbeat gap between WARN (3 s) and DROP (8 s) degrades the link
    but keeps the socket open and does NOT raise (A3)."""
    clock = _FakeClock()
    bridge = _bridge_for_receive_test(clock, monkeypatch)
    calls = {"n": 0}

    def fake_recv(blocking: bool, timeout: float) -> Any:
        calls["n"] += 1
        if calls["n"] >= 2:
            bridge._running.clear()
        return None

    bridge._conn.recv_match = fake_recv  # type: ignore[assignment]
    # 5 s stale: past WARN (3 s), below DROP (8 s).
    clock.now = 105.0

    bridge._receive_loop()  # must NOT raise

    snap = bridge._store.get_snapshot()
    assert snap["link_status"] == "degraded"
    assert snap["link_quality"] == "poor"
    # Connection was NOT torn down: socket stays open.
    assert bridge._conn is not None
    assert not bridge._conn.closed  # type: ignore[attr-defined]


def test_receive_loop_drop_gap_raises_connection_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A heartbeat gap past DROP (8 s) raises ConnectionError to trigger the
    reconnect path (A3)."""
    clock = _FakeClock()
    bridge = _bridge_for_receive_test(clock, monkeypatch)
    bridge._conn.recv_match = lambda blocking, timeout: None  # type: ignore[assignment]
    # 10 s stale: past DROP (8 s).
    clock.now = 110.0

    with pytest.raises(ConnectionError, match="heartbeat timeout"):
        bridge._receive_loop()


def test_warn_timeout_below_drop_does_not_close_socket(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Explicit boundary check: a gap just under DROP keeps the socket open;
    a gap just over DROP raises. Pins the WARN/DROP contract for UDP."""
    # Just under DROP: 7.9 s stale (WARN=3, DROP=8).
    clock = _FakeClock()
    bridge = _bridge_for_receive_test(clock, monkeypatch)
    calls = {"n": 0}

    def fake_recv(blocking: bool, timeout: float) -> Any:
        calls["n"] += 1
        if calls["n"] >= 2:
            bridge._running.clear()
        return None

    bridge._conn.recv_match = fake_recv  # type: ignore[assignment]
    clock.now = 107.9
    bridge._receive_loop()
    assert not bridge._conn.closed  # type: ignore[attr-defined]
    assert bridge._store.get_snapshot()["link_status"] == "degraded"


# ---------------------------------------------------------------------------
# 3. Exponential reconnect backoff (non-decreasing, capped at 8 s)
# ---------------------------------------------------------------------------

def test_reconnect_delay_is_non_decreasing_and_capped(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """With jitter pinned to zero the backoff is a clean exponential that
    never decreases and is capped at RECONNECT_CAP_S (8.0). The jitter
    allowance is exercised separately so the cap assertion stays exact."""
    monkeypatch.setattr("corvus.mavlink_bridge.random.uniform", lambda a, b: 0.0)
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")
    delays = [bridge._reconnect_delay(n) for n in range(1, 9)]
    # Non-decreasing across attempts.
    for i in range(len(delays) - 1):
        assert delays[i] <= delays[i + 1], (
            f"backoff decreased: {delays[i]} -> {delays[i + 1]} at attempt {i + 1}"
        )
    # Capped at 8.0 (with jitter off, the capped value is exactly 8.0).
    assert all(d <= 8.0 for d in delays), f"backoff exceeds cap: {delays}"
    assert delays[-1] == 8.0, f"backoff did not reach the 8.0 cap: {delays[-1]}"


def test_reconnect_delay_cap_holds_under_max_jitter(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Even at full +25% jitter the raw base stays bounded by the 8 s cap;
    the jittered value can reach 8.0 * 1.25 = 10 s but never explodes."""
    monkeypatch.setattr("corvus.mavlink_bridge.random.uniform", lambda a, b: b)
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")
    for attempt in range(1, 12):
        d = bridge._reconnect_delay(attempt)
        # Floor always respected.
        assert d >= 0.1
        # raw cap (8.0) * max jitter (1.25) is the absolute ceiling.
        assert d <= 8.0 * 1.25 + 1e-9, f"attempt {attempt}: {d} exceeds 8.0*1.25"


def test_reconnect_delay_floor_respected_under_min_jitter(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """At full -25% jitter every attempt still respects the 0.1 s floor."""
    monkeypatch.setattr("corvus.mavlink_bridge.random.uniform", lambda a, b: a)
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")
    for attempt in range(1, 6):
        assert bridge._reconnect_delay(attempt) >= 0.1
