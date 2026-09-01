"""VIBRATION message parsing and on-demand high-rate streaming tests.

Covers the VIBRATION dispatch (PX4 repurposes vibration_x/y/z as gyro coning,
gyro high-freq, and accel high-freq metrics — we pass the values through),
and the SET_MESSAGE_INTERVAL-based per-message rate control used to request
~10 Hz VIBRATION only while the operator's plugin is open (lean: PX4 defaults
VIBRATION to 0.1 Hz).
"""
from __future__ import annotations

import math
import time
from types import SimpleNamespace
from typing import Any, Callable

import pytest
from pymavlink import mavutil

from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore


# ---------------------------------------------------------------------------
# Shared test fakes (mirrors tests/test_mavlink_takeoff.py patterns)
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


def ack(command: int, result: int, target_system: int = 255) -> FakeMessage:
    return FakeMessage(
        message_type="COMMAND_ACK",
        command=command,
        result=result,
        target_system=target_system,
        target_component=190,
        source_system=1,
    )


def heartbeat(armed: bool) -> FakeMessage:
    base_mode = mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED if armed else 0
    return FakeMessage(
        message_type="HEARTBEAT",
        type=mavutil.mavlink.MAV_TYPE_QUADROTOR,
        autopilot=mavutil.mavlink.MAV_AUTOPILOT_PX4,
        base_mode=base_mode,
        custom_mode=(3 << 16),
    )


def ready_bridge() -> MavlinkBridge:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = FakeConnection()
    return bridge


def vibration_msg(
    vx: float = 0.0, vy: float = 0.0, vz: float = 0.0,
    c0: int = 0, c1: int = 0, c2: int = 0,
) -> FakeMessage:
    return FakeMessage(
        message_type="VIBRATION",
        time_usec=0,
        vibration_x=vx,
        vibration_y=vy,
        vibration_z=vz,
        clipping_0=c0,
        clipping_1=c1,
        clipping_2=c2,
    )


# ---------------------------------------------------------------------------
# VIBRATION dispatch
# ---------------------------------------------------------------------------

def test_vibration_dispatch_populates_store_fields() -> None:
    bridge = ready_bridge()
    bridge._dispatch(vibration_msg(0.12, 0.34, 0.56, 1, 2, 3))
    snap = bridge._store.get_snapshot()
    if "vibration_x" in snap:
        assert snap["vibration_x"] == 0.12
        assert snap["vibration_y"] == 0.34
        assert snap["vibration_z"] == 0.56
        assert snap["clipping_0"] == 1
        assert snap["clipping_1"] == 2
        assert snap["clipping_2"] == 3


def test_vibration_dispatch_rounds_to_4_decimals() -> None:
    bridge = ready_bridge()
    bridge._dispatch(vibration_msg(0.123456, 0.987654, 0.555555, 0, 0, 0))
    snap = bridge._store.get_snapshot()
    if "vibration_x" in snap:
        assert snap["vibration_x"] == pytest.approx(0.1235)
        assert snap["vibration_y"] == pytest.approx(0.9877)
        assert snap["vibration_z"] == pytest.approx(0.5556)


def test_vibration_dispatch_overwrites_previous_values() -> None:
    bridge = ready_bridge()
    bridge._dispatch(vibration_msg(0.1, 0.2, 0.3, 1, 1, 1))
    bridge._dispatch(vibration_msg(0.5, 0.6, 0.7, 5, 6, 7))
    snap = bridge._store.get_snapshot()
    if "vibration_x" in snap:
        assert snap["vibration_x"] == 0.5
        assert snap["clipping_2"] == 7


# ---------------------------------------------------------------------------
# set_message_interval
# ---------------------------------------------------------------------------

def test_set_message_interval_connected_accepted_sends_command_and_returns_true() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        command = int(args[2])
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)

    result = bridge.set_message_interval(241, 100000)

    assert result is True
    cmd = bridge._conn.mav.commands[0]
    assert cmd[2] == mavutil.mavlink.MAV_CMD_SET_MESSAGE_INTERVAL
    assert cmd[4] == pytest.approx(241.0)   # param1 = msg_id
    assert cmd[5] == pytest.approx(100000.0)  # param2 = interval_us
    # params 3-7 must be NaN
    for i in range(2, 7):
        assert math.isnan(cmd[4 + i])


def test_set_message_interval_disconnected_returns_false_sends_nothing() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.set_message_interval(241, 100000) is False


def test_set_message_interval_non_accepted_sets_error() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_DENIED))

    bridge._conn = FakeConnection(on_send)

    assert bridge.set_message_interval(241, 100000) is False
    assert "Set message interval failed: DENIED" == bridge.get_last_command_error()


# ---------------------------------------------------------------------------
# set_vibration_stream
# ---------------------------------------------------------------------------

def test_set_vibration_stream_enabled_10hz_calls_set_message_interval_with_100000us() -> None:
    bridge = ready_bridge()
    sent_intervals: list[int] = []

    def on_send(args: tuple) -> None:
        command = int(args[2])
        # Capture the interval for this specific call
        if command == mavutil.mavlink.MAV_CMD_SET_MESSAGE_INTERVAL:
            sent_intervals.append(int(args[5]))
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)

    assert bridge.set_vibration_stream(True, rate_hz=10) is True
    assert sent_intervals == [100000]
    cmd = bridge._conn.mav.commands[0]
    assert cmd[4] == pytest.approx(float(mavutil.mavlink.MAVLINK_MSG_ID_VIBRATION))


def test_set_vibration_stream_disabled_calls_set_message_interval_with_0() -> None:
    bridge = ready_bridge()
    sent_intervals: list[int] = []

    def on_send(args: tuple) -> None:
        command = int(args[2])
        if command == mavutil.mavlink.MAV_CMD_SET_MESSAGE_INTERVAL:
            sent_intervals.append(int(args[5]))
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)

    assert bridge.set_vibration_stream(False) is True
    assert sent_intervals == [0]


def test_set_vibration_stream_rate_0_returns_false_sends_nothing() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)

    assert bridge.set_vibration_stream(True, rate_hz=0) is False
    assert "vibration rate must be between 1 and 50 Hz" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []


def test_set_vibration_stream_rate_51_returns_false_sends_nothing() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)

    assert bridge.set_vibration_stream(True, rate_hz=51) is False
    assert "vibration rate must be between 1 and 50 Hz" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []


def test_set_vibration_stream_rate_1_uses_1000000us() -> None:
    bridge = ready_bridge()
    sent_intervals: list[int] = []

    def on_send(args: tuple) -> None:
        command = int(args[2])
        if command == mavutil.mavlink.MAV_CMD_SET_MESSAGE_INTERVAL:
            sent_intervals.append(int(args[5]))
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)

    assert bridge.set_vibration_stream(True, rate_hz=1) is True
    assert sent_intervals == [1000000]


def test_set_vibration_stream_succeeds_while_armed() -> None:
    """Vibration streaming is read-only telemetry — safe while armed."""
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    bridge._store.update(armed=True)

    assert bridge.set_vibration_stream(True, rate_hz=10) is True
    assert bridge._conn.mav.commands  # a command was actually sent


def test_set_vibration_stream_disabled_default_rate_restores_lean() -> None:
    bridge = ready_bridge()
    sent_intervals: list[int] = []

    def on_send(args: tuple) -> None:
        command = int(args[2])
        if command == mavutil.mavlink.MAV_CMD_SET_MESSAGE_INTERVAL:
            sent_intervals.append(int(args[5]))
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)

    # Enable high-rate, then disable to restore PX4 default.
    assert bridge.set_vibration_stream(True, rate_hz=10) is True
    assert bridge.set_vibration_stream(False) is True
    assert sent_intervals == [100000, 0]


def test_set_vibration_stream_disconnected_returns_false() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.set_vibration_stream(True, rate_hz=10) is False
