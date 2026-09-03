"""MAVLink transport classification + reboot-to-bootloader (Firmware Flash).

Covers the Firmware-Flash surface added to MavlinkBridge:
``transport()`` / ``is_direct_usb()`` / ``serial_device()`` classify the
configured connection so the flash gate can refuse non-USB links, and
``reboot_to_bootloader()`` sends ``MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN`` with
param1=3.0 (verified for PX4 v1.16/v1.17/v1.18 — see the method docstring).

Hermetic — no real autopilot, no real serial device. Reuses the FakeConn /
command-capture scaffolding pattern from ``test_mavlink_takeoff.py`` and
``test_mavlink_serial.py``.
"""
from __future__ import annotations

import math
from types import SimpleNamespace
from typing import Callable

import pytest
from pymavlink import mavutil

from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore


# ---------------------------------------------------------------------------
# Scaffolding (mirrors test_mavlink_takeoff.py)
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


def ready_bridge() -> MavlinkBridge:
    """A bridge over a fresh, connected, disarmed store (heartbeat == connected)."""
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    return bridge


# ---------------------------------------------------------------------------
# transport()
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("conn_str, expected", [
    # Direct USB (CDC ACM) — flashable.
    ("serial:/dev/ttyACM0:115200", "usb"),
    ("serial:/dev/ttyACM0", "usb"),
    ("serial:/dev/ttyACM3", "usb"),
    # Direct USB via /dev/serial/by-id pointing at a known Pixhawk-class FC.
    ("serial:/dev/serial/by-id/usb-3D_Robotics_PX4_FMU-v5_x7_0-if00", "usb"),
    ("serial:/dev/serial/by-id/usb-Hex_ProfiCNC_Pixhawk4_x4-if00", "usb"),
    ("serial:/dev/serial/by-id/usb-mRo_Pixhawk_v2-if00", "usb"),
    # SiK radio / USB-to-serial adapter — NOT flashable.
    ("serial:/dev/ttyUSB0:57600", "sik"),
    ("serial:/dev/ttyUSB1", "sik"),
    # UDP family.
    ("udp:0.0.0.0:14540", "udp"),
    ("udpin:0.0.0.0:14540", "udp"),
    ("udpbcast:10.0.0.255:14550", "udp"),
    # TCP.
    ("tcp:1.2.3.4:5760", "tcp"),
    # Unrecognised serial / empty / garbage → unknown.
    ("serial:/dev/ttyAMA0", "unknown"),
    ("serial:/dev/ttyTHS0", "unknown"),
    ("", "unknown"),
    ("garbage", "unknown"),
])
def test_transport_classifies_connection(conn_str: str, expected: str) -> None:
    bridge = MavlinkBridge(VehicleStateStore(), conn_str)
    assert bridge.transport() == expected


# ---------------------------------------------------------------------------
# is_direct_usb()
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("conn_str, expected", [
    ("serial:/dev/ttyACM0:115200", True),
    ("serial:/dev/ttyACM0", True),
    ("serial:/dev/serial/by-id/usb-3D_Robotics_PX4_FMU-v5_x7_0-if00", True),
    ("serial:/dev/ttyUSB0:57600", False),
    ("udp:0.0.0.0:14540", False),
    ("udpin:0.0.0.0:14540", False),
    ("udpbcast:10.0.0.0:255:14550", False),
    ("tcp:1.2.3.4:5760", False),
    ("serial:/dev/ttyAMA0", False),
    ("", False),
])
def test_is_direct_usb_mirrors_transport(conn_str: str, expected: bool) -> None:
    bridge = MavlinkBridge(VehicleStateStore(), conn_str)
    assert bridge.is_direct_usb() is expected
    # The invariant: is_direct_usb() is exactly transport() == "usb".
    assert bridge.is_direct_usb() == (bridge.transport() == "usb")


# ---------------------------------------------------------------------------
# serial_device()
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("conn_str, expected", [
    # serial: links strip the prefix and the trailing :baud.
    ("serial:/dev/ttyACM0:115200", "/dev/ttyACM0"),
    ("serial:/dev/ttyACM0", "/dev/ttyACM0"),
    ("serial:/dev/ttyUSB0:57600", "/dev/ttyUSB0"),
    ("serial:/dev/ttyUSB0", "/dev/ttyUSB0"),
    ("serial:COM3:115200", "COM3"),
    # by-id paths (no colon inside) are returned whole.
    ("serial:/dev/serial/by-id/usb-3D_Robotics_PX4_FMU-v5_x7_0-if00",
     "/dev/serial/by-id/usb-3D_Robotics_PX4_FMU-v5_x7_0-if00"),
    # Non-serial links return "".
    ("udp:0.0.0.0:14540", ""),
    ("udpin:0.0.0.0:14540", ""),
    ("udpbcast:10.0.0.255:14550", ""),
    ("tcp:1.2.3.4:5760", ""),
    ("", ""),
])
def test_serial_device_strips_prefix_and_baud(conn_str: str, expected: str) -> None:
    bridge = MavlinkBridge(VehicleStateStore(), conn_str)
    assert bridge.serial_device() == expected


def test_serial_device_returns_empty_for_non_serial() -> None:
    assert MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540").serial_device() == ""
    assert MavlinkBridge(VehicleStateStore(), "tcp:1.2.3.4:5760").serial_device() == ""


# ---------------------------------------------------------------------------
# reboot_to_bootloader()
# ---------------------------------------------------------------------------

def test_reboot_refused_while_armed_does_not_send() -> None:
    bridge = ready_bridge()
    bridge._store.update(armed=True)
    bridge._conn = FakeConnection()  # a send on this would prove the gate failed

    assert bridge.reboot_to_bootloader() is False
    # No command left the wire — the armed guard returns before _send_command_and_wait.
    assert bridge._conn.mav.commands == []
    assert "armed" in bridge.get_last_command_error()


def test_reboot_sends_preflight_reboot_shutdown_with_param1_3() -> None:
    """param1=3.0 is the PX4 'reboot to bootloader' value (v1.16/v1.17/v1.18)."""
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        command = int(args[2])
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)

    assert bridge.reboot_to_bootloader() is True
    assert len(bridge._conn.mav.commands) == 1
    sent = bridge._conn.mav.commands[0]
    # command_long_send(target_system, target_component, command, confirmation, p0..p6)
    assert int(sent[2]) == mavutil.mavlink.MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN
    # param1 (p0) must be 3.0 — the PX4 REBOOT_TO_BOOTLOADER selector.
    assert sent[4] == pytest.approx(3.0)
    # params 2..6 are NaN (unused), param1 is the only meaningful selector.
    for value in sent[5:11]:
        assert math.isnan(value)


def test_reboot_returns_true_on_accepted() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.reboot_to_bootloader() is True


def test_reboot_returns_false_on_denied() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_DENIED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.reboot_to_bootloader() is False
    assert "DENIED" in bridge.get_last_command_error()


def test_reboot_publishes_console_success_entry() -> None:
    bridge = ready_bridge()
    console: list[dict] = []
    bridge.add_console_sub(lambda entry: console.append(entry))

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.reboot_to_bootloader() is True

    reboot_entries = [e for e in console if e.get("name") == "REBOOT"]
    assert len(reboot_entries) == 1
    assert reboot_entries[0]["level"] == "success"
    assert "bootloader" in reboot_entries[0]["text"].lower()


def test_reboot_refused_when_disconnected() -> None:
    """A bridge with no fresh heartbeat must not send a reboot command."""
    bridge = MavlinkBridge(VehicleStateStore())  # no heartbeat → not connected
    bridge._conn = FakeConnection()
    assert bridge.reboot_to_bootloader() is False
    assert bridge._conn.mav.commands == []
