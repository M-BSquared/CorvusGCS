"""Regressions from a review of the MAVLink link against PX4 and ArduPilot.

Each test here pins a defect that shipped and failed quietly on a real
vehicle rather than raising anything:

* MAVLink 2 signing was installed on a pymavlink object that pymavlink itself
  replaces when the first MAVLink 2 frame arrives, so the link said "signing
  enabled" and then sent everything unsigned;
* a companion computer's SYSTEM_TIME (MAVROS sends one at 1 Hz under the
  vehicle's system id) read as the vehicle rebooting every second;
* an analog power module's pack voltage, which both stacks put in cell slot 0,
  was shown as a single 16.8 V cell;
* the connect-time stream batch asked PX4 for a HEARTBEAT interval, which PX4
  answers with FAILED, and reported that as a critical warning on every
  connect;
* the stream fallback carried no EXTENDED_STATUS, and nothing asked ArduPilot
  for MISSION_CURRENT or SYSTEM_TIME at all;
* the mode table was built once at connect and never again, so a vehicle
  swapped behind a router kept the previous airframe's mode numbers;
* the dialects' mission start: PX4 is told both ends, ArduPilot 4.6 refuses
  anything but (0, 0).
"""
from __future__ import annotations

import os
from types import SimpleNamespace
from typing import Any

import pytest
from pymavlink import mavutil

from corvus import autopilot
from corvus.mavlink_bridge import MavlinkBridge, ParamEntry
from corvus.state_store import VehicleStateStore


class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return getattr(self, "source_system", 1)

    def get_srcComponent(self) -> int:
        return getattr(self, "source_component", 1)


def _bridge(connection: str = "udp:0.0.0.0:14550") -> MavlinkBridge:
    store = VehicleStateStore()
    store.heartbeat()
    store.update(connected=True)
    bridge = MavlinkBridge(store, connection)
    bridge._target_system = 1
    bridge._target_component = 1
    return bridge


# ---------------------------------------------------------------------------
# Signing
# ---------------------------------------------------------------------------

def test_signing_survives_the_switch_to_mavlink_2(
    tmp_path: Any, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """pymavlink starts a connection as MAVLink 1 unless MAVLINK20 is set, and
    swaps ``conn.mav`` for a fresh object on the first MAVLink 2 byte it
    reads. The key lived on the old object."""
    key = bytes(range(1, 33))
    path = tmp_path / "signing.key"
    path.write_bytes(key)
    os.chmod(path, 0o600)
    monkeypatch.setenv("CORVUS_MAVLINK_SIGNING_KEY_FILE", str(path))

    old_env = os.environ.get("MAVLINK20")
    conn = mavutil.mavlink_connection(
        "udpout:127.0.0.1:14999", source_system=254, source_component=190)
    try:
        # As in a fresh process: nothing has upgraded the protocol yet.
        conn.WIRE_PROTOCOL_VERSION = "1.0"
        bridge = _bridge()
        bridge._conn = conn

        bridge._apply_signing()
        # The vehicle's first MAVLink 2 frame.
        conn.auto_mavlink_version(b"\xfd")

        assert conn.mav.signing.secret_key == key
        assert conn.mav.signing.sign_outgoing is True
        assert conn.WIRE_PROTOCOL_VERSION == "2.0"
    finally:
        conn.close()
        if old_env is None:
            os.environ.pop("MAVLINK20", None)
        else:
            os.environ["MAVLINK20"] = old_env
        mavutil.set_dialect(mavutil.current_dialect)


# ---------------------------------------------------------------------------
# Whose clock
# ---------------------------------------------------------------------------

def test_a_companions_system_time_is_not_a_vehicle_reboot() -> None:
    bridge = _bridge()
    bridge._params["MPC_XY_VEL_MAX"] = ParamEntry("MPC_XY_VEL_MAX", 12.0, 9, 0, 1, 1)

    for component, uptime in ((1, 600_000), (191, 12_000), (1, 601_000), (191, 13_000)):
        bridge._dispatch(FakeMessage(
            message_type="SYSTEM_TIME", time_unix_usec=0, time_boot_ms=uptime,
            source_system=1, source_component=component,
        ))

    assert "MPC_XY_VEL_MAX" in bridge._params, "the cache was dropped for nothing"
    assert bridge._store.get_snapshot()["boot_ms"] == 601_000


def test_the_autopilots_own_reboot_is_still_noticed() -> None:
    bridge = _bridge()
    bridge._params["MPC_XY_VEL_MAX"] = ParamEntry("MPC_XY_VEL_MAX", 12.0, 9, 0, 1, 1)
    bridge._request_version = lambda: None          # type: ignore[method-assign]
    bridge._request_home = lambda: None             # type: ignore[method-assign]
    bridge._schedule_message_intervals = lambda: None  # type: ignore[method-assign]

    for uptime in (600_000, 2_000):
        bridge._dispatch(FakeMessage(
            message_type="SYSTEM_TIME", time_unix_usec=0, time_boot_ms=uptime,
            source_system=1, source_component=1,
        ))

    assert bridge._params == {}


# ---------------------------------------------------------------------------
# Battery
# ---------------------------------------------------------------------------

def _battery(voltages: list[int], ext: list[int] | None = None) -> SimpleNamespace:
    return SimpleNamespace(
        id=0, voltages=voltages, voltages_ext=ext or [0, 0, 0, 0],
        current_consumed=-1, temperature=32767, time_remaining=0,
    )


def test_a_pack_voltage_in_slot_zero_is_not_a_cell() -> None:
    """An analog power module cannot see its cells; both stacks then report
    the whole pack in slot 0 and mark the rest unused."""
    bridge = _bridge()
    bridge._handle_battery_status(_battery([16800] + [65535] * 9))
    assert bridge._store.get_snapshot()["battery_cell_voltages"] == []


def test_a_pack_above_65_volts_carried_into_slot_one_is_not_two_cells() -> None:
    bridge = _bridge()
    bridge._handle_battery_status(_battery([65534, 1000] + [65535] * 8))
    assert bridge._store.get_snapshot()["battery_cell_voltages"] == []


def test_real_cell_readings_are_still_published() -> None:
    bridge = _bridge()
    bridge._handle_battery_status(_battery([4100, 4090] + [65535] * 8))
    assert bridge._store.get_snapshot()["battery_cell_voltages"] == [4.1, 4.09]


# ---------------------------------------------------------------------------
# The connect-time stream batch
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("connection", ["udp:0.0.0.0:14550", "serial:/dev/ttyUSB0:57600"])
def test_the_batch_never_asks_for_a_heartbeat_interval(connection: str) -> None:
    """PX4 v1.16 to v1.18 answer that with FAILED, on every connect."""
    intervals = _bridge(connection)._message_intervals()
    assert mavutil.mavlink.MAVLINK_MSG_ID_HEARTBEAT not in intervals


@pytest.mark.parametrize("connection", ["udp:0.0.0.0:14550", "serial:/dev/ttyUSB0:57600"])
def test_the_batch_asks_for_what_ardupilot_would_not_send(connection: str) -> None:
    """ArduPilot's SRx rates default to 0, and on a link where the batch is
    accepted REQUEST_DATA_STREAM is never sent: no mission progress, and no
    uptime to tell a reboot from a dropped link."""
    intervals = _bridge(connection)._message_intervals()
    assert mavutil.mavlink.MAVLINK_MSG_ID_MISSION_CURRENT in intervals
    assert mavutil.mavlink.MAVLINK_MSG_ID_SYSTEM_TIME in intervals


@pytest.mark.parametrize("connection", ["udp:0.0.0.0:14550", "serial:/dev/ttyUSB0:57600"])
def test_the_stream_fallback_carries_the_status_group(connection: str) -> None:
    """SYS_STATUS, GPS_RAW_INT and MISSION_CURRENT are EXTENDED_STATUS."""
    rates = _bridge(connection)._stream_rates()
    assert rates[mavutil.mavlink.MAV_DATA_STREAM_EXTENDED_STATUS] > 0


def test_a_refused_interval_at_connect_puts_nothing_on_the_status_bar() -> None:
    bridge = _bridge()
    bridge._conn = SimpleNamespace()
    published: list[tuple[str, str]] = []
    bridge._console_publish = (  # type: ignore[method-assign]
        lambda name, text, level: published.append((text, level)))
    refused = mavutil.mavlink.MAVLINK_MSG_ID_MISSION_CURRENT

    def answer(command: int, params: list[float], **_: Any) -> int:
        if int(params[0]) == refused:
            return mavutil.mavlink.MAV_RESULT_FAILED
        return mavutil.mavlink.MAV_RESULT_ACCEPTED

    bridge._send_command_and_wait = answer  # type: ignore[method-assign]
    bridge._request_message_intervals()

    assert bridge._store.get_snapshot()["warnings"] == []
    assert not any(level == "error" for _text, level in published)


def test_an_operators_own_interval_request_still_reports_a_refusal() -> None:
    bridge = _bridge()
    bridge._conn = SimpleNamespace()
    bridge._console_publish = lambda *a: None  # type: ignore[method-assign]
    bridge._send_command_and_wait = (  # type: ignore[method-assign]
        lambda command, params, **_: mavutil.mavlink.MAV_RESULT_DENIED)

    assert bridge.set_message_interval(mavutil.mavlink.MAVLINK_MSG_ID_VIBRATION, 0) is False
    assert "DENIED" in bridge.get_last_command_error()
    assert bridge._store.get_snapshot()["warnings"]


# ---------------------------------------------------------------------------
# The mode table follows the vehicle
# ---------------------------------------------------------------------------

def test_a_different_airframe_behind_the_router_gets_its_own_modes() -> None:
    """Mode 4 is GUIDED on a copter and ACRO on a plane."""
    bridge = _bridge()
    bridge._conn = SimpleNamespace(mode_mapping=lambda: None)
    bridge._latch_dialect(mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA,
                          mavutil.mavlink.MAV_TYPE_QUADROTOR)
    bridge._build_mode_mapping()
    assert bridge._mode_values["GUIDED"].custom_mode == 4

    bridge._dispatch(FakeMessage(
        message_type="HEARTBEAT", type=mavutil.mavlink.MAV_TYPE_FIXED_WING,
        autopilot=mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA,
        base_mode=mavutil.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED, custom_mode=5,
    ))

    assert bridge._mode_values["GUIDED"].custom_mode == 15
    assert bridge._mode_values["ACRO"].custom_mode == 4
    assert bridge._store.get_snapshot()["mode"] == "FBWA"


def test_connect_pins_the_sysid_mode_mapping_reads(monkeypatch) -> None:
    """mavutil.mode_mapping() reads sysid_state[conn.sysid], the first vehicle
    it latched onto, and not target_system."""
    from corvus import mavlink_bridge as mb

    hb = mavutil.mavlink.MAVLink_heartbeat_message(
        type=mavutil.mavlink.MAV_TYPE_QUADROTOR,
        autopilot=mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA,
        base_mode=mavutil.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED,
        custom_mode=0, system_status=3, mavlink_version=3)
    hb.pack(mavutil.mavlink.MAVLink(None, srcSystem=3, srcComponent=1))
    read_for: list[int] = []

    class FakeConn:
        def __init__(self) -> None:
            self.target_system = 7
            self.target_component = 1
            self.sysid = 7
            self.sysid_state = {7: object(), 3: object()}
            self.mav = SimpleNamespace(
                request_data_stream_send=lambda *a, **k: None,
                command_long_send=lambda *a, **k: None,
                autopilot_version_request_send=lambda *a, **k: None,
            )

        def wait_heartbeat(self, blocking: bool = True, timeout: float | None = None):
            return hb

        def mode_mapping(self):
            read_for.append(self.sysid)
            return None

    conn = FakeConn()
    monkeypatch.setattr(mb.mavutil, "mavlink_connection", lambda *a, **k: conn)
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14550")
    monkeypatch.setattr(bridge, "_start_gcs_heartbeat", lambda: None)
    monkeypatch.setattr(bridge, "_request_version", lambda: None)
    bridge._connect()

    assert conn.sysid == 3
    assert read_for == [3]


# ---------------------------------------------------------------------------
# Mission start, per dialect
# ---------------------------------------------------------------------------

def test_px4_is_told_both_ends_of_the_mission() -> None:
    px4 = autopilot.dialect_for_stack(autopilot.STACK_PX4)
    assert px4.mission_start_params(5) == (0.0, 4.0)
    assert px4.mission_arm_mode(mavutil.mavlink.MAV_TYPE_QUADROTOR) == ""


@pytest.mark.parametrize(("mav_type", "arm_mode"), [
    (mavutil.mavlink.MAV_TYPE_QUADROTOR, "GUIDED"),
    (mavutil.mavlink.MAV_TYPE_HELICOPTER, "GUIDED"),
    (mavutil.mavlink.MAV_TYPE_GROUND_ROVER, "GUIDED"),
    (mavutil.mavlink.MAV_TYPE_FIXED_WING, "AUTO"),
    (mavutil.mavlink.MAV_TYPE_VTOL_TILTROTOR, "AUTO"),
])
def test_ardupilot_starts_every_mission_with_zeroes(mav_type: int, arm_mode: str) -> None:
    ardupilot = autopilot.dialect_for_stack(autopilot.STACK_ARDUPILOT)
    assert ardupilot.mission_start_params(5) == (0.0, 0.0)
    assert ardupilot.mission_arm_mode(mav_type) == arm_mode
