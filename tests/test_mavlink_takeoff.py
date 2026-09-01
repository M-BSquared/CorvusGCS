from __future__ import annotations

import math
import threading
import time
from types import SimpleNamespace
from typing import Callable

import pytest
from pymavlink import mavutil

from corvus.mavlink_bridge import MavlinkBridge, STATUSTEXT_CHUNK_TIMEOUT_S
from corvus.state_store import VehicleStateStore


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
    return bridge


def test_px4_auto_submodes_use_bits_24_to_31() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    expected = {
        1: "AUTO.READY",
        2: "AUTO.TAKEOFF",
        3: "AUTO.LOITER",
        4: "AUTO.MISSION",
        5: "AUTO.RTL",
        6: "AUTO.LAND",
        7: "AUTO.RTGS",
        9: "AUTO.PRECLAND",
        11: "AUTO.EXTERNAL1",
    }
    for submode, name in expected.items():
        msg = SimpleNamespace(custom_mode=(submode << 24) | (4 << 16))
        assert bridge._decode_mode(msg) == name


def test_takeoff_sends_amsl_target_before_arming() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        command = int(args[2])
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))
        if command == mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM:
            bridge._dispatch(heartbeat(True))

    bridge._conn = FakeConnection(on_send)
    bridge._home_alt_amsl = 123.5

    assert bridge.takeoff(10.0)
    commands = bridge._conn.mav.commands
    assert [item[2] for item in commands] == [
        mavutil.mavlink.MAV_CMD_NAV_TAKEOFF,
        mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM,
    ]
    takeoff_params = commands[0][4:]
    assert all(math.isnan(value) for value in takeoff_params[:6])
    assert takeoff_params[6] == pytest.approx(133.5)


def test_takeoff_uses_global_position_reference_when_home_is_missing() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        command = int(args[2])
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))
        if command == mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM:
            bridge._dispatch(heartbeat(True))

    bridge._conn = FakeConnection(on_send)
    bridge._dispatch(FakeMessage(
        message_type="GLOBAL_POSITION_INT",
        lat=52_0000000,
        lon=13_0000000,
        alt=128_400,
        relative_alt=3_400,
        hdg=0,
    ))

    assert bridge.takeoff(10.0)
    assert bridge._conn.mav.commands[0][10] == pytest.approx(135.0)


def test_takeoff_rejection_does_not_arm() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_DENIED))

    bridge._conn = FakeConnection(on_send)
    bridge._home_alt_amsl = 100.0

    assert not bridge.takeoff(10.0)
    assert [item[2] for item in bridge._conn.mav.commands] == [
        mavutil.mavlink.MAV_CMD_NAV_TAKEOFF,
    ]
    assert bridge.get_last_command_error() == "Takeoff failed: DENIED"


@pytest.mark.parametrize("altitude", [True, None, "bad", float("nan"), 0, 51])
def test_takeoff_rejects_invalid_altitudes(altitude: object) -> None:
    bridge = ready_bridge()
    bridge._conn = FakeConnection()
    bridge._home_alt_amsl = 100.0

    assert not bridge.takeoff(altitude)  # type: ignore[arg-type]
    assert bridge._conn.mav.commands == []
    assert bridge.get_last_command_error()


def test_takeoff_requires_a_fresh_connection_and_altitude_reference() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    bridge._conn = FakeConnection()
    bridge._home_alt_amsl = 100.0
    assert not bridge.takeoff(10.0)
    assert bridge.get_last_command_error() == "Takeoff failed: DISCONNECTED"

    bridge = ready_bridge()
    bridge._conn = FakeConnection()
    assert not bridge.takeoff(10.0)
    assert bridge.get_last_command_error() == "Takeoff failed: no home or global altitude reference"


def test_ack_in_progress_waits_for_final_result() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        command = int(args[2])
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_IN_PROGRESS))
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    result = bridge._send_command_and_wait(22, [float("nan")] * 7, timeout=0.01, retries=0)
    assert result == mavutil.mavlink.MAV_RESULT_ACCEPTED


def test_ack_for_another_gcs_is_ignored_and_command_is_retried() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        command = int(args[2])
        if int(args[3]) == 0:
            bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED, target_system=42))
        else:
            bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    result = bridge._send_command_and_wait(22, [float("nan")] * 7, timeout=0.01, retries=1)

    assert result == mavutil.mavlink.MAV_RESULT_ACCEPTED
    assert [item[3] for item in bridge._conn.mav.commands] == [0, 1]


def test_disconnect_wakes_pending_command_without_waiting_for_timeout() -> None:
    bridge = ready_bridge()
    bridge._conn = FakeConnection()
    result: list[int] = []
    worker = threading.Thread(
        target=lambda: result.append(
            bridge._send_command_and_wait(22, [float("nan")] * 7, timeout=10.0, retries=0)
        )
    )
    worker.start()
    deadline = time.monotonic() + 1.0
    while not bridge._conn.mav.commands and time.monotonic() < deadline:
        time.sleep(0.001)

    bridge._cancel_pending_commands()
    worker.join(timeout=1.0)

    assert not worker.is_alive()
    assert result == [-2]


def test_arm_ack_must_be_confirmed_by_heartbeat_state() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    bridge._wait_for_armed_state = lambda armed, timeout=3.0: False  # type: ignore[method-assign]

    assert not bridge.arm(True)
    assert bridge.get_last_command_error() == "Arm accepted but vehicle state did not change"


def test_set_mode_waits_for_accepted_ack() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    bridge._mode_mapping = {"TAKEOFF"}
    bridge._mode_values = {"TAKEOFF": (29, 4, 2)}

    assert bridge.set_mode("TAKEOFF")
    command = bridge._conn.mav.commands[0]
    assert command[2] == mavutil.mavlink.MAV_CMD_DO_SET_MODE
    assert command[4:7] == (29.0, 4.0, 2.0)


def test_set_mode_returns_false_on_denied_ack() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_DENIED))

    bridge._conn = FakeConnection(on_send)
    bridge._mode_mapping = {"TAKEOFF"}
    bridge._mode_values = {"TAKEOFF": (29, 4, 2)}

    assert not bridge.set_mode("TAKEOFF")
    assert bridge.get_last_command_error() == "Mode change failed: DENIED"


def statustext(text: str, message_id: int = 0, chunk_seq: int = 0, severity: int = 4) -> FakeMessage:
    return FakeMessage(
        message_type="STATUSTEXT",
        text=text,
        id=message_id,
        chunk_seq=chunk_seq,
        severity=severity,
        source_system=1,
    )


def test_statustext_reassembles_standard_mavlink2_chunks_without_chunk_count() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    bridge._handle_statustext(statustext("A" * 50, message_id=7, chunk_seq=0))
    assert bridge._store.get_snapshot()["warnings"] == []

    bridge._handle_statustext(statustext("final", message_id=7, chunk_seq=1))

    warnings = bridge._store.get_snapshot()["warnings"]
    assert len(warnings) == 1
    assert warnings[0]["level"] == "warning"
    assert warnings[0]["msg"] == "A" * 50 + "final"
    assert warnings[0]["meta"]


def test_statustext_waits_for_missing_chunk_and_detects_nul_final_chunk() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    bridge._handle_statustext(statustext("tail\x00padding", message_id=8, chunk_seq=1, severity=3))
    assert bridge._store.get_snapshot()["warnings"] == []

    bridge._handle_statustext(statustext("B" * 50, message_id=8, chunk_seq=0, severity=5))

    warning = bridge._store.get_snapshot()["warnings"][0]
    assert warning["msg"] == "B" * 50 + "tail"
    assert warning["level"] == "critical"


def test_statustext_exact_50_byte_chunk_requires_a_following_final_chunk() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    bridge._handle_statustext(statustext("D" * 50, message_id=10, chunk_seq=0))
    bridge._handle_statustext(statustext("E" * 50, message_id=10, chunk_seq=1))
    assert bridge._store.get_snapshot()["warnings"] == []

    bridge._handle_statustext(statustext("", message_id=10, chunk_seq=2))

    assert bridge._store.get_snapshot()["warnings"][0]["msg"] == "D" * 50 + "E" * 50


def test_statustext_discards_stale_partial_messages() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    bridge._handle_statustext(statustext("C" * 50, message_id=9, chunk_seq=0))
    key = next(iter(bridge._statustext_chunks))
    bridge._statustext_chunks[key]["updated"] = time.monotonic() - STATUSTEXT_CHUNK_TIMEOUT_S - 1

    bridge._cleanup_statustext_chunks()

    assert bridge._statustext_chunks == {}


def test_receive_loop_checks_heartbeat_after_non_heartbeat_telemetry() -> None:
    bridge = ready_bridge()
    bridge._running.set()
    telemetry = FakeMessage(message_type="ATTITUDE", roll=0.0, pitch=0.0, yaw=0.0)
    bridge._conn = FakeConnection()
    bridge._conn.recv_match = lambda blocking, timeout: telemetry  # type: ignore[attr-defined]
    checks = iter([False, True])
    bridge._store.is_stale = lambda timeout: next(checks)  # type: ignore[method-assign]

    with pytest.raises(ConnectionError, match="heartbeat timeout"):
        bridge._receive_loop()
