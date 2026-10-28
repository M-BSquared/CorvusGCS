"""ExpressLRS over MAVLink: the serial options, the held DTR/RTS lines, Betaflight.

An ExpressLRS transmitter module on USB is reset by a port that opens with DTR
and RTS asserted, so the lines are part of the connection string and are set
before the port opens. Betaflight sends MAVLink telemetry with a generic
heartbeat, so the link says which flight controller it ends in, and the Setup
tiles that need parameters are reported unavailable for it.
"""
from __future__ import annotations

import re
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from pymavlink import mavutil

from corvus import autopilot, serial_open
from corvus.mavlink_bridge import MavlinkBridge
from corvus.serial_open import HeldLineMavSerial, held_line_port_class, open_serial_link
from corvus.serial_ports import (
    SerialOptions,
    parse_serial_options,
    serial_query,
    split_serial_connection,
    validate_serial_options,
)
from corvus.state_store import VehicleStateStore

ROOT = Path(__file__).resolve().parent.parent
ELRS = "serial:/dev/ttyUSB0:460800?link=elrs&dtr=off&rts=off&fc=betaflight"


# --- the connection string ------------------------------------------------

def test_options_do_not_disturb_the_device_and_the_baud():
    assert split_serial_connection(ELRS) == ("/dev/ttyUSB0", 460800)
    assert split_serial_connection("serial:COM7:460800?dtr=off") == ("COM7", 460800)
    assert split_serial_connection("serial:/dev/ttyUSB0:57600") == ("/dev/ttyUSB0", 57600)


def test_options_are_read_back():
    options = parse_serial_options(ELRS)
    assert options == SerialOptions(dtr=False, rts=False, firmware="betaflight", link="elrs")
    assert options.sets_lines
    assert parse_serial_options("serial:/dev/ttyUSB0:57600") == SerialOptions()
    assert not SerialOptions().sets_lines
    assert parse_serial_options("serial:COM7:9600?dtr=on").dtr is True


def test_a_value_that_is_not_understood_is_dropped_and_then_refused():
    odd = "serial:COM7:9600?dtr=maybe&fc=px4&link=wifi"
    assert parse_serial_options(odd) == SerialOptions()
    with pytest.raises(ValueError, match="dtr"):
        validate_serial_options(odd)
    with pytest.raises(ValueError, match="fc"):
        validate_serial_options("serial:COM7:9600?fc=px4")
    validate_serial_options(ELRS)
    validate_serial_options("serial:COM7:9600")


def test_the_query_round_trips():
    options = parse_serial_options(ELRS)
    assert serial_query(options) == "?link=elrs&dtr=off&rts=off&fc=betaflight"
    assert serial_query(SerialOptions()) == ""
    assert parse_serial_options("serial:x:1" + serial_query(options)) == options


def test_the_bridge_accepts_a_good_string_and_refuses_a_bad_one():
    bridge = MavlinkBridge(VehicleStateStore(), "serial:/dev/ttyUSB0:57600")
    bridge.validate_connection(ELRS)
    with pytest.raises(ValueError):
        bridge.validate_connection("serial:/dev/ttyUSB0:460800?rts=sometimes")


# --- opening the port -----------------------------------------------------

class _FakePort:
    opened: list[_FakePort] = []

    def __init__(self, device: str, baud: int, **kwargs: Any) -> None:
        self.device, self.baud, self.kwargs = device, baud, kwargs
        self.baudrate = baud
        _FakePort.opened.append(self)

    def fileno(self) -> int:
        raise OSError("no descriptor")

    def setBaudrate(self, baud: int) -> None:
        self.baudrate = baud

    def close(self) -> None:
        pass


def test_the_lines_are_handed_to_the_port_before_it_opens():
    _FakePort.opened.clear()
    link = HeldLineMavSerial(
        "/dev/ttyUSB0", 460800, SerialOptions(dtr=False, rts=False), 254, 190,
        port_class=_FakePort,
    )
    port = _FakePort.opened[0]
    assert port.kwargs["dtr"] is False and port.kwargs["rts"] is False
    assert port.kwargs["dsrdtr"] is False and port.kwargs["rtscts"] is False
    assert port.baud == 1200 and port.baudrate == 460800
    assert link.port is port


def test_pyserial_opens_with_the_held_state_not_the_default(monkeypatch: pytest.MonkeyPatch):
    import serial

    seen: list[tuple[Any, Any]] = []
    monkeypatch.setattr(
        serial.Serial, "open",
        lambda self: seen.append((self._dtr_state, self._rts_state)),
    )
    held_line_port_class()("/dev/ttyUSB0", 1200, dtr=False, rts=False)
    held_line_port_class()("/dev/ttyUSB0", 1200)
    held_line_port_class()("/dev/ttyUSB0", 1200, dtr=True, rts=False)
    assert seen == [(False, False), (True, True), (True, False)]


def test_a_plain_serial_link_still_goes_through_mavlink_connection(
    monkeypatch: pytest.MonkeyPatch,
):
    calls: list[tuple[str, dict]] = []
    monkeypatch.setattr(
        mavutil, "mavlink_connection",
        lambda device, **kw: calls.append((device, kw)) or "plain",
    )
    assert open_serial_link("/dev/ttyUSB0", 57600, SerialOptions(), 254, 190) == "plain"
    assert calls == [("/dev/ttyUSB0", {
        "baud": 57600, "source_system": 254, "source_component": 190,
    })]


def test_held_lines_use_the_held_opener(monkeypatch: pytest.MonkeyPatch):
    made: list[Any] = []
    monkeypatch.setattr(
        serial_open, "HeldLineMavSerial",
        lambda *a, **k: made.append(a) or "held",
    )
    assert open_serial_link("COM7", 460800, SerialOptions(dtr=False), 254, 190) == "held"
    assert made and made[0][0] == "COM7"


# --- the dialect ----------------------------------------------------------

def _heartbeat(autopilot_id: int) -> SimpleNamespace:
    return SimpleNamespace(
        type=mavutil.mavlink.MAV_TYPE_QUADROTOR, autopilot=autopilot_id,
        base_mode=0, custom_mode=0,
    )


def test_a_generic_heartbeat_is_betaflight_only_when_the_link_says_so():
    generic = mavutil.mavlink.MAV_AUTOPILOT_GENERIC
    assert autopilot.dialect_for(generic).stack == autopilot.STACK_GENERIC
    assert autopilot.dialect_for(generic, "betaflight").stack == autopilot.STACK_BETAFLIGHT
    assert autopilot.dialect_for(generic, "something").stack == autopilot.STACK_GENERIC


def test_an_autopilot_that_names_itself_beats_the_hint():
    assert autopilot.dialect_for(autopilot.MAV_AUTOPILOT_PX4, "betaflight").stack == "px4"
    assert autopilot.dialect_for(autopilot.MAV_AUTOPILOT_ARDUPILOTMEGA, "betaflight").stack == "ardupilot"


def _connected(monkeypatch: pytest.MonkeyPatch, conn: str, autopilot_id: int) -> dict:
    from tests.test_mavlink_serial import FakeConnectConnection, seed_link_fields

    store = VehicleStateStore()
    seed_link_fields(store)
    bridge = MavlinkBridge(store, conn)
    fake = FakeConnectConnection()
    fake._heartbeat = _heartbeat(autopilot_id)
    monkeypatch.setattr("corvus.mavlink_bridge.open_serial_link", lambda *a, **k: fake)
    monkeypatch.setattr(bridge, "_start_gcs_heartbeat", lambda: None)
    bridge._connect()
    return {"snapshot": store.get_snapshot(), "caps": bridge.capabilities()}


def test_betaflight_over_expresslrs_connects_as_betaflight(monkeypatch: pytest.MonkeyPatch):
    out = _connected(monkeypatch, ELRS, mavutil.mavlink.MAV_AUTOPILOT_GENERIC)
    assert out["snapshot"]["autopilot_stack"] == "betaflight"
    assert out["caps"]["stack"] == "betaflight"
    assert out["caps"]["telemetry_only"] is True
    assert "calibration" in out["caps"]["setup_unavailable"]


def test_px4_over_expresslrs_is_still_px4(monkeypatch: pytest.MonkeyPatch):
    out = _connected(monkeypatch, ELRS, autopilot.MAV_AUTOPILOT_PX4)
    assert out["snapshot"]["autopilot_stack"] == "px4"
    assert out["caps"]["setup_unavailable"] == {}


def test_a_generic_heartbeat_without_the_hint_stays_generic(monkeypatch: pytest.MonkeyPatch):
    out = _connected(
        monkeypatch, "serial:/dev/ttyUSB0:57600", mavutil.mavlink.MAV_AUTOPILOT_GENERIC,
    )
    assert out["snapshot"]["autopilot_stack"] == "generic"
    assert out["caps"]["setup_unavailable"] == {}


def test_only_betaflight_reports_unavailable_setup_tiles():
    for stack in (autopilot.STACK_PX4, autopilot.STACK_ARDUPILOT, autopilot.STACK_GENERIC):
        caps = autopilot.dialect_for_stack(stack).capabilities()
        assert caps["setup_unavailable"] == {} and caps["telemetry_only"] is False


def test_betaflight_tiles_name_real_setup_tiles_and_keep_the_independent_ones():
    tiles = set(re.findall(r'makeTile\("(\w+)"', (ROOT / "src/js/setup.js").read_text()))
    blocked = set(autopilot.dialect_for_stack("betaflight").setup_unavailable)
    assert blocked <= tiles, f"unknown tile ids: {blocked - tiles}"
    assert tiles - blocked == {"sik", "video", "betaflight"}
    for reason in autopilot.dialect_for_stack("betaflight").setup_unavailable.values():
        assert "Betaflight" in reason
        assert not re.search("—|–| - ", reason)


def test_betaflight_takes_no_commands_it_cannot_answer():
    dialect = autopilot.dialect_for_stack("betaflight")
    assert not dialect.autotune_plan(2, True).ok
    assert dialect.available_modes(2) == []
    assert not dialect.supports_shell
