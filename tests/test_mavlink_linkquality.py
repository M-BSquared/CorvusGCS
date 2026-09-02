"""Link-quality tracking and two-tier degrade-don't-drop tests (A1/A3/A4).

Covers RADIO_STATUS parsing into a 0-100 uplink score plus the raw radio
metrics, heartbeat inter-arrival jitter, the link_quality string thresholds
(good/fair/poor, heartbeat-driven when no RADIO_STATUS arrives), the two-tier
heartbeat state machine (degrade between warn and drop, tear down only past
drop), and the exponential reconnect backoff.
"""
from __future__ import annotations

from types import SimpleNamespace
from typing import Any, Callable

import pytest
from pymavlink import mavutil

from corvus.mavlink_bridge import (
    HEARTBEAT_DROP_TIMEOUT_UDP,
    HEARTBEAT_WARN_TIMEOUT_UDP,
    MavlinkBridge,
)
from corvus.state_store import VehicleStateStore


# ---------------------------------------------------------------------------
# Shared fakes (mirror tests/test_mavlink_vibration.py)
# ---------------------------------------------------------------------------

class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return getattr(self, "source_system", 1)


class FakeMav:
    def __init__(self, on_send: Callable[[tuple], None] | None = None) -> None:
        self.commands: list[tuple] = []
        self.on_send = on_send

    def command_long_send(self, *args: float) -> None:
        self.commands.append(args)
        if self.on_send:
            self.on_send(args)


class FakeConnection:
    def __init__(self, on_send: Callable[[tuple], None] | None = None) -> None:
        self.mav = FakeMav(on_send)
        self.source_system = 255
        self.source_component = 190
        self.recv_match: Callable[..., Any] = lambda blocking, timeout: None
        self.closed = False

    def close(self) -> None:
        self.closed = True


def ack(command: int, result: int, target_system: int = 255) -> FakeMessage:
    return FakeMessage(
        message_type="COMMAND_ACK",
        command=command,
        result=result,
        target_system=target_system,
        target_component=190,
        source_system=1,
    )


def heartbeat_msg() -> FakeMessage:
    return FakeMessage(
        message_type="HEARTBEAT",
        type=mavutil.mavlink.MAV_TYPE_QUADROTOR,
        autopilot=mavutil.mavlink.MAV_AUTOPILOT_PX4,
        base_mode=0,
        custom_mode=(3 << 16),  # POSCTL
    )


def radio_status(
    rssi: int = 0, remrssi: int = 0, txbuf: int = 100,
    noise: int = 0, remnoise: int = 0, rxerrors: int = 0, fixed: int = 0,
) -> FakeMessage:
    return FakeMessage(
        message_type="RADIO_STATUS",
        rssi=rssi, remrssi=remrssi, txbuf=txbuf,
        noise=noise, remnoise=remnoise,
        rxerrors=rxerrors, fixed=fixed,
    )


def seed_link_quality_fields(store: VehicleStateStore) -> None:
    """Inject the link-quality keys the bridge writes (A2 contract).

    Mirrors the fields the backend agent adds to ``VehicleStateStore._data``;
    ``update()`` ignores keys absent from ``_data``, so seeding here makes the
    bridge's publishes observable before that integration lands.
    """
    with store._lock:
        store._data.setdefault("uplink", 0)
        store._data.setdefault("uplink_rssi", 0.0)
        store._data.setdefault("uplink_rxerrors", 0)
        store._data.setdefault("uplink_fixed", 0)
        store._data.setdefault("heartbeat_jitter_ms", 0.0)
        store._data.setdefault("link_quality", "unknown")


def ready_bridge(connection: str = "udp:0.0.0.0:14540") -> MavlinkBridge:
    store = VehicleStateStore()
    seed_link_quality_fields(store)
    store.heartbeat()
    bridge = MavlinkBridge(store, connection)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = FakeConnection()
    return bridge


# ---------------------------------------------------------------------------
# A1: RADIO_STATUS → uplink + radio metrics
# ---------------------------------------------------------------------------

def test_radio_status_strong_signal_scores_near_100() -> None:
    bridge = ready_bridge()
    # remrssi=150 (strong), txbuf=100 → remrssi_pct=100, txbuf_pct=100 → 100.
    bridge._dispatch(radio_status(rssi=200, remrssi=150, txbuf=100,
                                  noise=10, remnoise=20, rxerrors=3, fixed=1))
    snap = bridge._store.get_snapshot()
    assert snap["uplink"] == 100
    assert snap["uplink_rssi"] == 150.0
    assert snap["uplink_rxerrors"] == 3
    assert snap["uplink_fixed"] == 1


def test_radio_status_weak_signal_scores_low() -> None:
    bridge = ready_bridge()
    # remrssi=40 (weak floor), txbuf=100 → remrssi_pct=0, txbuf_pct=100 → 30.
    bridge._dispatch(radio_status(remrssi=40, txbuf=100))
    snap = bridge._store.get_snapshot()
    assert snap["uplink"] == 30


def test_radio_status_clamps_to_0_100() -> None:
    bridge = ready_bridge()
    # remrssi way above 150 still clamps to 100.
    bridge._dispatch(radio_status(remrssi=250, txbuf=100))
    assert bridge._store.get_snapshot()["uplink"] == 100
    # remrssi below 40 floor and txbuf 0 clamps to 0.
    bridge._dispatch(radio_status(remrssi=10, txbuf=0))
    assert bridge._store.get_snapshot()["uplink"] == 0


def test_radio_status_unknown_remrssi_keeps_last_score() -> None:
    """A momentary 0/255 remrssi must not flap the uplink to 0 (stability)."""
    bridge = ready_bridge()
    bridge._dispatch(radio_status(remrssi=150, txbuf=100))
    assert bridge._store.get_snapshot()["uplink"] == 100
    # remrssi=0 → unknown reading: keep the last score, publish raw rssi=0.
    bridge._dispatch(radio_status(remrssi=0, txbuf=100, rxerrors=7, fixed=2))
    snap = bridge._store.get_snapshot()
    assert snap["uplink"] == 100  # unchanged
    assert snap["uplink_rssi"] == 0.0
    assert snap["uplink_rxerrors"] == 7
    assert snap["uplink_fixed"] == 2


def test_radio_status_cumulative_counters_stored_as_is() -> None:
    bridge = ready_bridge()
    bridge._dispatch(radio_status(remrssi=150, txbuf=100, rxerrors=1234, fixed=56))
    snap = bridge._store.get_snapshot()
    assert snap["uplink_rxerrors"] == 1234
    assert snap["uplink_fixed"] == 56


def test_radio_status_txbuf_penalty_lowers_score() -> None:
    bridge = ready_bridge()
    # remrssi=150 (full 100) but txbuf=0 (buffer full/congested) → 70.
    bridge._dispatch(radio_status(remrssi=150, txbuf=0))
    assert bridge._store.get_snapshot()["uplink"] == 70


# ---------------------------------------------------------------------------
# A1: heartbeat jitter
# ---------------------------------------------------------------------------

def test_heartbeat_jitter_zero_before_two_heartbeats(monkeypatch: pytest.MonkeyPatch) -> None:
    bridge = ready_bridge()
    clock = iter([10.0])
    monkeypatch.setattr("corvus.mavlink_bridge.time.monotonic", lambda: next(clock))
    bridge._dispatch(heartbeat_msg())
    assert bridge._store.get_snapshot()["heartbeat_jitter_ms"] == 0.0


def test_heartbeat_jitter_steady_heartbeat_is_zero(monkeypatch: pytest.MonkeyPatch) -> None:
    bridge = ready_bridge()
    times = iter([0.0, 1.0, 2.0, 3.0])
    monkeypatch.setattr("corvus.mavlink_bridge.time.monotonic", lambda: next(times))
    for _ in range(4):
        bridge._dispatch(heartbeat_msg())
    # intervals all 1.0 s → stddev 0.
    assert bridge._store.get_snapshot()["heartbeat_jitter_ms"] == 0.0


def test_heartbeat_jitter_computes_rolling_stddev_ms(monkeypatch: pytest.MonkeyPatch) -> None:
    bridge = ready_bridge()
    # Intervals: 1.0 s, 2.0 s → mean 1.5, stddev 0.5 s = 500 ms.
    times = iter([0.0, 1.0, 3.0])
    monkeypatch.setattr("corvus.mavlink_bridge.time.monotonic", lambda: next(times))
    for _ in range(3):
        bridge._dispatch(heartbeat_msg())
    assert bridge._store.get_snapshot()["heartbeat_jitter_ms"] == pytest.approx(500.0)


def test_heartbeat_jitter_uses_rolling_window(monkeypatch: pytest.MonkeyPatch) -> None:
    bridge = ready_bridge()
    # 32 timestamps (one over the maxlen=30 window): only the last 30 matter.
    times_iter = iter([float(i) for i in range(32)])
    monkeypatch.setattr("corvus.mavlink_bridge.time.monotonic", lambda: next(times_iter))
    for _ in range(32):
        bridge._dispatch(heartbeat_msg())
    assert len(bridge._hb_times) == 30


# ---------------------------------------------------------------------------
# A1: link_quality string thresholds
# ---------------------------------------------------------------------------

def test_link_quality_good_with_radio_and_low_jitter(monkeypatch: pytest.MonkeyPatch) -> None:
    bridge = ready_bridge()
    # Steady 1 Hz heartbeat → jitter 0; strong radio → uplink 100 → good.
    times = iter([0.0, 1.0, 2.0])
    monkeypatch.setattr("corvus.mavlink_bridge.time.monotonic", lambda: next(times))
    for _ in range(3):
        bridge._dispatch(heartbeat_msg())
    bridge._dispatch(radio_status(remrssi=150, txbuf=100))
    assert bridge._store.get_snapshot()["link_quality"] == "good"


def test_link_quality_fair_when_uplink_between_40_and_70(monkeypatch: pytest.MonkeyPatch) -> None:
    bridge = ready_bridge()
    times = iter([0.0, 1.0, 2.0])
    monkeypatch.setattr("corvus.mavlink_bridge.time.monotonic", lambda: next(times))
    for _ in range(3):
        bridge._dispatch(heartbeat_msg())
    # remrssi=80 → remrssi_pct≈36.36; txbuf=100 → score≈55 → fair.
    bridge._dispatch(radio_status(remrssi=80, txbuf=100))
    snap = bridge._store.get_snapshot()
    assert 40 <= snap["uplink"] < 70
    assert snap["link_quality"] == "fair"


def test_link_quality_poor_when_uplink_below_40(monkeypatch: pytest.MonkeyPatch) -> None:
    bridge = ready_bridge()
    times = iter([0.0, 1.0, 2.0])
    monkeypatch.setattr("corvus.mavlink_bridge.time.monotonic", lambda: next(times))
    for _ in range(3):
        bridge._dispatch(heartbeat_msg())
    # remrssi=40 → remrssi_pct=0; txbuf=100 → score 30 → poor.
    bridge._dispatch(radio_status(remrssi=40, txbuf=100))
    snap = bridge._store.get_snapshot()
    assert snap["uplink"] < 40
    assert snap["link_quality"] == "poor"


def test_link_quality_good_when_no_radio_status_and_heartbeat_fresh(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """UDP SITL sends no RADIO_STATUS: quality falls back to heartbeat freshness."""
    bridge = ready_bridge()
    times = iter([0.0, 1.0])
    monkeypatch.setattr("corvus.mavlink_bridge.time.monotonic", lambda: next(times))
    bridge._dispatch(heartbeat_msg())
    bridge._dispatch(heartbeat_msg())
    assert not bridge._radio_status_seen
    assert bridge._store.get_snapshot()["link_quality"] == "good"
    # uplink stays 0 without a radio.
    assert bridge._store.get_snapshot()["uplink"] == 0


def test_link_quality_good_requires_low_jitter(monkeypatch: pytest.MonkeyPatch) -> None:
    bridge = ready_bridge()
    # High jitter (>50 ms) with uplink 100 → not "good" (drops to fair).
    times = iter([0.0, 1.0, 3.0])  # intervals 1.0, 2.0 → jitter 500 ms
    monkeypatch.setattr("corvus.mavlink_bridge.time.monotonic", lambda: next(times))
    for _ in range(3):
        bridge._dispatch(heartbeat_msg())
    bridge._dispatch(radio_status(remrssi=150, txbuf=100))
    assert bridge._store.get_snapshot()["uplink"] == 100
    assert bridge._store.get_snapshot()["link_quality"] != "good"


# ---------------------------------------------------------------------------
# A3: two-tier degrade-don't-drop state machine
# ---------------------------------------------------------------------------

class _FakeClock:
    """Controllable wall clock for the store's is_stale() (time.time)."""

    def __init__(self, start: float = 100.0) -> None:
        self.now = start

    def time(self) -> float:
        return self.now


def _bridge_for_receive_test(clock: _FakeClock, monkeypatch: pytest.MonkeyPatch) -> MavlinkBridge:
    store = VehicleStateStore()
    seed_link_quality_fields(store)
    bridge = MavlinkBridge(store, "udp:0.0.0.0:14540")
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = FakeConnection()
    bridge._running.set()
    # Store reads time.time() for is_stale/heartbeat; pin it to our clock.
    monkeypatch.setattr("corvus.state_store.time.time", clock.time)
    # Establish a fresh heartbeat at the starting clock time.
    store.heartbeat()
    return bridge


def test_receive_loop_warn_timeout_degrades_without_dropping(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A heartbeat gap between WARN and DROP must NOT raise (A3)."""
    clock = _FakeClock()
    bridge = _bridge_for_receive_test(clock, monkeypatch)
    # recv_match returns None a couple times then clears _running so the
    # loop exits deterministically (no real blocking).
    calls = {"n": 0}

    def fake_recv(blocking: bool, timeout: float) -> Any:
        calls["n"] += 1
        if calls["n"] >= 2:
            bridge._running.clear()
        return None

    bridge._conn.recv_match = fake_recv  # type: ignore[assignment]
    # Advance the clock past WARN (3 s) but below DROP (8 s): 5 s stale.
    clock.now = 105.0

    # Must NOT raise.
    bridge._receive_loop()

    snap = bridge._store.get_snapshot()
    assert snap["link_status"] == "degraded"
    assert snap["link_quality"] == "poor"
    # Connection was NOT torn down.
    assert bridge._conn is not None
    assert not bridge._conn.closed  # type: ignore[attr-defined]


def test_receive_loop_drop_timeout_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    """A heartbeat gap past DROP must raise to trigger reconnect (A3)."""
    clock = _FakeClock()
    bridge = _bridge_for_receive_test(clock, monkeypatch)
    bridge._conn.recv_match = lambda blocking, timeout: None  # type: ignore[assignment]
    # Advance past DROP (8 s): 10 s stale.
    clock.now = 110.0

    with pytest.raises(ConnectionError, match="heartbeat timeout"):
        bridge._receive_loop()


def test_receive_loop_degrades_only_once(monkeypatch: pytest.MonkeyPatch) -> None:
    """The warn guard must not spam link_status=degraded every iteration."""
    clock = _FakeClock()
    bridge = _bridge_for_receive_test(clock, monkeypatch)
    updates = {"n": 0}
    orig_update = bridge._store.update

    def counting_update(**kwargs: Any) -> None:
        if kwargs.get("link_status") == "degraded":
            updates["n"] += 1
        orig_update(**kwargs)

    bridge._store.update = counting_update  # type: ignore[assignment]

    calls = {"n": 0}

    def fake_recv(blocking: bool, timeout: float) -> Any:
        calls["n"] += 1
        if calls["n"] >= 5:
            bridge._running.clear()
        return None

    bridge._conn.recv_match = fake_recv  # type: ignore[assignment]
    clock.now = 105.0  # between warn and drop

    bridge._receive_loop()

    assert updates["n"] == 1  # degraded published exactly once


def test_warn_and_drop_timeouts_match_contract() -> None:
    serial = MavlinkBridge(VehicleStateStore(), "serial:/dev/ttyUSB0:57600")
    udp = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")
    assert serial._warn_timeout() == 6.0
    assert serial._drop_timeout() == 15.0
    assert udp._warn_timeout() == HEARTBEAT_WARN_TIMEOUT_UDP
    assert udp._drop_timeout() == HEARTBEAT_DROP_TIMEOUT_UDP
    # _heartbeat_timeout() (command-ready gate) is unchanged.
    assert serial._heartbeat_timeout() == 10.0
    assert udp._heartbeat_timeout() == 5.0


# ---------------------------------------------------------------------------
# A4: exponential reconnect backoff
# ---------------------------------------------------------------------------

def test_reconnect_delay_grows_then_caps(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("corvus.mavlink_bridge.random.uniform", lambda a, b: 0.0)
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")
    # base=0.5, cap=8.0: 0.5, 1.0, 2.0, 4.0, 8.0, 8.0 …
    delays = [bridge._reconnect_delay(a) for a in (1, 2, 3, 4, 5, 6)]
    assert delays == [0.5, 1.0, 2.0, 4.0, 8.0, 8.0]


def test_reconnect_delay_jitter_stays_in_bounds(monkeypatch: pytest.MonkeyPatch) -> None:
    # +25% jitter on attempt 1 (base 0.5) → 0.625; on attempt 5 (capped 8.0) → 10.0.
    monkeypatch.setattr("corvus.mavlink_bridge.random.uniform", lambda a, b: b)
    bridge = MavlinkBridge(VehicleStateStore(), "serial:/dev/ttyUSB0:57600")
    assert bridge._reconnect_delay(1) == pytest.approx(0.625)
    assert bridge._reconnect_delay(5) == pytest.approx(10.0)


def test_reconnect_delay_floor(monkeypatch: pytest.MonkeyPatch) -> None:
    # -25% jitter still respects the 0.1 s floor on every attempt.
    monkeypatch.setattr("corvus.mavlink_bridge.random.uniform", lambda a, b: a)
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")
    for attempt in (1, 2, 3):
        assert bridge._reconnect_delay(attempt) >= 0.1


# ---------------------------------------------------------------------------
# A6: send/recv race — _send_command_and_wait bails when conn swaps
# ---------------------------------------------------------------------------

def test_send_command_and_wait_bails_when_conn_swapped_under_send_lock(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        # Simulate a concurrent reconnect swapping self._conn while we held
        # the send lock: the snapshot guard must return -2, not send.
        bridge._conn = None

    bridge._conn = FakeConnection(on_send)
    result = bridge._send_command_and_wait(22, [float("nan")] * 7, timeout=0.05, retries=0)
    assert result == -2


def test_gcs_hb_loop_resets_hb_thread_on_exit() -> None:
    """_gcs_hb_loop must clear _hb_thread on exit so reconnect can restart it."""
    bridge = ready_bridge()
    bridge._running.set()
    # heartbeat_send raises (closed socket) → loop breaks and resets _hb_thread.
    def boom(*args: Any) -> None:
        raise OSError("send on closed socket")

    bridge._conn.mav.heartbeat_send = boom  # type: ignore[assignment]
    # Fast-forward sleep so the loop iterates promptly.
    monkey = pytest.MonkeyPatch()
    monkey.setattr("corvus.mavlink_bridge.time.sleep", lambda _: None)
    try:
        bridge._gcs_hb_loop()
    finally:
        monkey.undo()
    assert bridge._hb_thread is None
