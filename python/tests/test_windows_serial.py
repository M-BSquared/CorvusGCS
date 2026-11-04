"""Serial links on Windows, where the port name tells you nothing.

A POSIX device path is self-describing: ``/dev/ttyACM0`` is a flight
controller's own USB interface, ``/dev/ttyUSB0`` is a bridge chip with
something behind it, and the classification that gates firmware flashing reads
straight off the name. Windows hands out ``COM7`` for all of it, so the same
question has to be answered from the port's USB descriptor instead.

These run on any host — the shapes are matched by regex, not by ``os.name`` —
so the Windows behaviour is pinned from the Mac and Linux the project is
developed on.
"""
from __future__ import annotations

import pytest

from corvus.firmware_uploader import FirmwareUploader
from corvus.mavlink_bridge import MavlinkBridge, is_windows_com_port
from corvus.state_store import VehicleStateStore


# Descriptors as pyserial reports them on Windows.
_PIXHAWK_HWID = "USB VID:PID=26AC:0032 SER=0 LOCATION=1-1.4"
_STM32_HWID = "USB VID:PID=0483:5740 SER=3567355C3235 LOCATION=1-2"
_FTDI_HWID = "USB VID:PID=0403:6001 SER=A1046ZQ8 LOCATION=1-3"
_CP210X_HWID = "USB VID:PID=10C4:EA60 SER=0001 LOCATION=1-5"
_BLUETOOTH_HWID = "BTHENUM\\{00001101-0000-1000-8000-00805F9B34FB}_LOCALMFG&0000"


@pytest.fixture()
def bridge():
    return MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")


def _with_ports(monkeypatch, ports):
    monkeypatch.setattr(
        MavlinkBridge, "list_serial_ports", staticmethod(lambda: ports))


# ---------------------------------------------------------------------------
# Recognising the shape
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("device, expected", [
    ("COM1", True),
    ("COM7", True),
    ("COM12", True),
    ("com3", True),                    # the drive letter case nobody agrees on
    (r"\\.\COM12", True),              # the form needed past COM9
    ("/dev/ttyACM0", False),
    ("COM", False),
    ("COMX", False),
    ("", False),
    (None, False),
])
def test_a_com_port_is_recognised_by_shape(device, expected) -> None:
    assert is_windows_com_port(device) is expected


# ---------------------------------------------------------------------------
# Classifying it
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("hwid, expected", [
    (_PIXHAWK_HWID, "usb"),            # flashable: the FMU's own interface
    (_STM32_HWID, "usb"),
    (_FTDI_HWID, "sik"),               # a bridge — a radio, not the FMU
    (_CP210X_HWID, "sik"),
    (_BLUETOOTH_HWID, "unknown"),
])
def test_a_com_port_is_classified_by_its_usb_descriptor(
        bridge, monkeypatch, hwid, expected) -> None:
    _with_ports(monkeypatch, [{"device": "COM7", "description": "", "hwid": hwid}])
    bridge.set_connection("serial:COM7:57600")
    assert bridge.transport() == expected


def test_an_unlisted_com_port_is_not_assumed_flashable(bridge, monkeypatch) -> None:
    """Unknown wins ties. Guessing 'usb' here would offer to flash firmware
    down a telemetry radio, and the operator finds out when the autopilot
    stops answering."""
    _with_ports(monkeypatch, [])
    bridge.set_connection("serial:COM7:57600")
    assert bridge.transport() == "unknown"
    assert bridge.is_direct_usb() is False


def test_a_bridge_descriptor_outranks_a_vendor_name_in_the_product_string(
        bridge, monkeypatch) -> None:
    """An FTDI cable sold with a flight-controller brand on it is still a
    cable. The bridge check runs first for exactly this case."""
    _with_ports(monkeypatch, [{
        "device": "COM4",
        "description": "Pixhawk telemetry cable",
        "hwid": _FTDI_HWID,
    }])
    bridge.set_connection("serial:COM4:57600")
    assert bridge.transport() == "sik"


def test_the_right_port_is_looked_up_when_several_are_present(
        bridge, monkeypatch) -> None:
    _with_ports(monkeypatch, [
        {"device": "COM3", "description": "", "hwid": _FTDI_HWID},
        {"device": "COM7", "description": "", "hwid": _PIXHAWK_HWID},
        {"device": "COM9", "description": "", "hwid": _BLUETOOTH_HWID},
    ])
    bridge.set_connection("serial:COM7:57600")
    assert bridge.transport() == "usb"
    bridge.set_connection("serial:COM3:57600")
    assert bridge.transport() == "sik"


def test_a_com_connection_string_survives_the_baud_split(bridge) -> None:
    """``serial:COM7:57600`` splits on the LAST colon, so the device keeps its
    name and the baud is still read."""
    bridge.set_connection("serial:COM7:115200")
    assert bridge.serial_device() == "COM7"
    assert bridge._parse_serial("serial:COM7:115200") == ("COM7", 115200)
    # And the escaped form for ports past COM9.
    assert bridge._parse_serial(r"serial:\\.\COM12:57600") == (r"\\.\COM12", 57600)


def test_posix_classification_is_untouched(bridge) -> None:
    """The Windows path is additional, not a rewrite: the device-path rules
    that gate flashing on Linux and macOS answer exactly as before."""
    for conn, expected in (
        ("serial:/dev/ttyACM0:57600", "usb"),
        ("serial:/dev/ttyUSB0:57600", "sik"),
        ("serial:/dev/ttyS0:57600", "unknown"),
        ("udp:0.0.0.0:14540", "udp"),
        ("tcp:127.0.0.1:5760", "tcp"),
    ):
        bridge.set_connection(conn)
        assert bridge.transport() == expected, conn


# ---------------------------------------------------------------------------
# Flashing: the bootloader device is not a file
# ---------------------------------------------------------------------------

def test_a_com_port_is_present_when_the_port_list_says_so(monkeypatch) -> None:
    """``os.path.exists("COM7")`` is False for a port that is present and
    openable, which made every Windows flash wait out the connect timeout and
    report "bootloader device not available"."""
    import os
    assert os.path.exists("COM7") is False, "the premise of this test"

    _with_ports(monkeypatch, [{"device": "COM7", "description": "", "hwid": _PIXHAWK_HWID}])
    assert FirmwareUploader._device_present("COM7") is True

    _with_ports(monkeypatch, [{"device": "COM3", "description": "", "hwid": _PIXHAWK_HWID}])
    assert FirmwareUploader._device_present("COM7") is False


def test_a_posix_bootloader_device_is_still_a_filesystem_check(tmp_path) -> None:
    node = tmp_path / "ttyACM0"
    assert FirmwareUploader._device_present(str(node)) is False
    node.write_text("")
    assert FirmwareUploader._device_present(str(node)) is True


def test_enumeration_failing_does_not_refuse_the_flash(monkeypatch) -> None:
    """If the port list cannot be read, let pyserial be the judge: refusing on
    our own failed guess would strand a board that is sitting in its
    bootloader waiting to be written."""
    def boom():
        raise OSError("no port enumeration on this host")
    monkeypatch.setattr(MavlinkBridge, "list_serial_ports", staticmethod(boom))
    assert FirmwareUploader._device_present("COM7") is True
