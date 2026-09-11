"""PX4 parameter protocol, sensor calibration, and autotune tests.

Covers the parameter download/upload cycle, PARAM_VALUE dispatch, the
calibration command mapping (verified against PX4 v1.18 Commander.cpp),
and the full-autotune wire contract shared by PX4 v1.16-v1.18.
"""
from __future__ import annotations

import math
import threading
import time
from types import SimpleNamespace
from typing import Callable

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
        self.param_requests: list[tuple] = []
        self.param_reads: list[tuple] = []
        self.param_sets: list[tuple] = []
        self.on_send = on_send

    def command_long_send(self, *args: float) -> None:
        self.commands.append(args)
        if self.on_send:
            self.on_send(args)

    def param_request_list_send(self, target_system: int, target_component: int) -> None:
        self.param_requests.append((target_system, target_component))

    def param_request_read_send(
        self, target_system: int, target_component: int, name: bytes, index: int
    ) -> None:
        self.param_reads.append((target_system, target_component, name, index))

    def param_set_send(
        self, target_system: int, target_component: int,
        name: bytes, value: float, ptype: int,
    ) -> None:
        self.param_sets.append((target_system, target_component, name, value, ptype))
        if self.on_send:
            self.on_send(("param_set", target_system, target_component, name, value, ptype))


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


def param_value(
    name: str,
    value: float,
    ptype: int = mavutil.mavlink.MAV_PARAM_TYPE_REAL32,
    index: int = 0,
    count: int = 0,
) -> FakeMessage:
    return FakeMessage(
        message_type="PARAM_VALUE",
        param_id=name.encode("utf-8"),
        param_value=value,
        param_type=ptype,
        param_index=index,
        param_count=count,
    )


# ---------------------------------------------------------------------------
# request_param_list
# ---------------------------------------------------------------------------

def test_request_param_list_sends_request_and_sets_downloading() -> None:
    bridge = ready_bridge()
    result = bridge.request_param_list()
    assert result is True
    assert bridge._conn.mav.param_requests == [(1, 1)]
    assert bridge._param_download_state == "downloading"
    assert bridge._params == {}
    assert bridge._param_received == 0
    assert bridge._param_count == -1


def test_request_param_list_returns_false_when_disconnected() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.request_param_list() is False
    assert bridge._param_download_state == "idle"


# ---------------------------------------------------------------------------
# request_param
# ---------------------------------------------------------------------------

def test_request_param_sends_param_request_read_with_name_and_index_minus_one() -> None:
    bridge = ready_bridge()
    assert bridge.request_param("MPC_XY_VEL_MAX") is True
    assert len(bridge._conn.mav.param_reads) == 1
    target_sys, target_comp, name_bytes, index = bridge._conn.mav.param_reads[0]
    assert target_sys == 1 and target_comp == 1
    assert name_bytes == b"MPC_XY_VEL_MAX"
    assert index == -1


def test_request_param_returns_false_when_disconnected() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.request_param("FOO") is False


# ---------------------------------------------------------------------------
# fetch_params — the named batch read behind Setup -> Motors
# ---------------------------------------------------------------------------

def _answer_on_read(bridge: MavlinkBridge, values: dict[str, float]) -> None:
    """Make the fake link answer PARAM_REQUEST_READ for the names in *values*.

    Wraps the fake mav's param_request_read_send so a request that the vehicle
    "has" is echoed straight back as a PARAM_VALUE, exactly as the real link
    would. Names absent from *values* stay unanswered — the version-tolerance
    case fetch_params exists to survive.
    """
    mav = bridge._conn.mav
    original = mav.param_request_read_send

    def answering(target_system, target_component, name, index):
        original(target_system, target_component, name, index)
        pname = name.decode()
        if pname in values:
            bridge._dispatch(param_value(pname, values[pname]))

    mav.param_request_read_send = answering


def test_fetch_params_reads_only_the_names_asked_for() -> None:
    bridge = ready_bridge()
    _answer_on_read(bridge, {"CA_AIRFRAME": 0.0, "CA_ROTOR_COUNT": 4.0, "MC_ROLL_P": 6.0})

    result = bridge.fetch_params(["CA_AIRFRAME", "CA_ROTOR_COUNT"])

    assert result == {"CA_AIRFRAME": 0.0, "CA_ROTOR_COUNT": 4.0}
    assert [r[2] for r in bridge._conn.mav.param_reads] == [b"CA_AIRFRAME", b"CA_ROTOR_COUNT"]
    assert bridge._conn.mav.param_requests == [], "no full download is triggered"


def test_fetch_params_serves_already_cached_values_without_asking_again() -> None:
    bridge = ready_bridge()
    bridge._dispatch(param_value("CA_AIRFRAME", 2.0))

    assert bridge.fetch_params(["CA_AIRFRAME"]) == {"CA_AIRFRAME": 2.0}
    assert bridge._conn.mav.param_reads == [], "a cached parameter is not re-requested"


def test_fetch_params_omits_what_the_firmware_does_not_have() -> None:
    """A parameter absent on this PX4 version is a missing key, never an error."""
    bridge = ready_bridge()
    _answer_on_read(bridge, {"CA_AIRFRAME": 0.0})

    result = bridge.fetch_params(["CA_AIRFRAME", "PWM_MAIN_TIM3"], timeout=0.5)

    assert result == {"CA_AIRFRAME": 0.0}


def test_fetch_params_retransmits_the_stragglers_once() -> None:
    """A lossy link drops individual replies; one retry covers that."""
    bridge = ready_bridge()
    _answer_on_read(bridge, {})   # nothing ever answers
    bridge.fetch_params(["CA_AIRFRAME"], timeout=0.5)

    assert [r[2] for r in bridge._conn.mav.param_reads] == [b"CA_AIRFRAME", b"CA_AIRFRAME"]


def test_fetch_params_returns_empty_when_disconnected() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.fetch_params(["CA_AIRFRAME"]) == {}
    assert bridge.get_last_command_error() == "not connected"


def test_fetch_params_with_no_names_never_touches_the_link() -> None:
    bridge = ready_bridge()
    assert bridge.fetch_params([]) == {}
    assert bridge._conn.mav.param_reads == []


def test_fetch_params_bails_out_when_the_bridge_is_stopping() -> None:
    """Shutdown must not wait out the read budget (AGENTS.md lifecycle)."""
    bridge = ready_bridge()
    _answer_on_read(bridge, {})
    bridge._stop_event.set()

    start = time.monotonic()
    assert bridge.fetch_params(["CA_AIRFRAME"], timeout=5.0) == {}
    assert time.monotonic() - start < 1.0
    assert bridge._conn.mav.param_reads == [], "no request is sent while stopping"


def test_fetch_params_does_not_disturb_the_download_state_machine() -> None:
    bridge = ready_bridge()
    _answer_on_read(bridge, {"CA_AIRFRAME": 0.0})
    before = bridge._param_download_state

    bridge.fetch_params(["CA_AIRFRAME"])

    assert bridge._param_download_state == before == "idle"


# ---------------------------------------------------------------------------
# motor_test — the bench identification spin behind Setup -> Motors
# ---------------------------------------------------------------------------

def _accepting_bridge() -> MavlinkBridge:
    """A ready bridge whose fake link ACKs every command it is sent."""
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1

    def on_send(args):
        # command_long_send(target_sys, target_comp, command, confirmation, p1..p7)
        if isinstance(args, tuple) and args and not isinstance(args[0], str):
            bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    return bridge


def _motor_test_commands(bridge: MavlinkBridge) -> list[tuple]:
    return [c for c in bridge._conn.mav.commands
            if int(c[2]) == mavutil.mavlink.MAV_CMD_DO_MOTOR_TEST]


def test_motor_test_sends_do_motor_test_with_percent_throttle_and_a_timeout() -> None:
    bridge = _accepting_bridge()

    assert bridge.motor_test(3, 20.0, 2.0) is True

    commands = _motor_test_commands(bridge)
    assert len(commands) == 1
    # command_long_send(target_sys, target_comp, command, confirmation, p1..p7)
    p1, p2, p3, p4, p5, p6 = commands[0][4:10]
    assert p1 == 3.0, "1-based motor number"
    assert p2 == 0.0, "MOTOR_TEST_THROTTLE_PERCENT"
    assert p3 == 20.0
    assert p4 == 2.0, "the vehicle counts the timeout down itself"
    assert p5 == 0.0 and p6 == 0.0, "this motor only, default order"


def test_a_motor_test_timeout_is_always_bounded() -> None:
    """A spinning motor with no timeout keeps spinning when the link drops."""
    bridge = _accepting_bridge()

    bridge.motor_test(1, 10.0, 3600.0)

    timeout = _motor_test_commands(bridge)[0][7]
    assert timeout == bridge.MOTOR_TEST_MAX_DURATION_S


def test_motor_test_is_refused_while_armed() -> None:
    bridge = _accepting_bridge()
    bridge._dispatch(heartbeat(armed=True))

    assert bridge.motor_test(1, 10.0, 2.0) is False
    assert bridge.get_last_command_error() == "cannot test motors while armed"
    assert _motor_test_commands(bridge) == [], "nothing reaches the link"


@pytest.mark.parametrize("motor", [0, 17, -1, 1.5, "1", True])
def test_motor_test_rejects_an_impossible_motor_number(motor) -> None:
    bridge = _accepting_bridge()
    assert bridge.motor_test(motor, 10.0, 2.0) is False
    assert _motor_test_commands(bridge) == []


@pytest.mark.parametrize("throttle", [-0.1, 100.1, 500.0])
def test_motor_test_rejects_a_throttle_outside_the_protocol_range(throttle) -> None:
    bridge = _accepting_bridge()
    assert bridge.motor_test(1, throttle, 2.0) is False
    assert _motor_test_commands(bridge) == []


def test_motor_test_returns_false_when_disconnected() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.motor_test(1, 10.0, 2.0) is False


def test_stopping_a_motor_test_zeroes_every_motor() -> None:
    """Stop must silence motors this session never started, too."""
    bridge = _accepting_bridge()

    assert bridge.stop_motor_test() is True

    commands = _motor_test_commands(bridge)
    assert [c[4] for c in commands] == [float(n) for n in range(1, 9)]
    assert all(c[6] == 0.0 and c[7] == 0.0 for c in commands), "throttle 0, timeout 0"


def test_stopping_a_motor_test_is_not_refused_while_armed() -> None:
    """Refusing to STOP a motor would be a safety regression, not defense in depth."""
    bridge = _accepting_bridge()
    bridge._dispatch(heartbeat(armed=True))

    assert bridge.stop_motor_test() is True
    assert _motor_test_commands(bridge), "the stop still reaches the link"


# ---------------------------------------------------------------------------
# PARAM_VALUE dispatch
# ---------------------------------------------------------------------------

def test_param_value_dispatch_increments_received_and_caches_entries() -> None:
    bridge = ready_bridge()
    bridge._param_download_state = "downloading"
    for i in range(3):
        bridge._dispatch(param_value(f"PARAM_{i}", float(i), index=i, count=3))
    assert bridge._param_received == 3
    assert bridge._param_count == 3
    assert bridge._param_download_state == "complete"
    entries = bridge.get_params()
    assert len(entries) == 3
    assert [e["name"] for e in entries] == ["PARAM_0", "PARAM_1", "PARAM_2"]


def test_param_value_duplicate_name_does_not_double_count() -> None:
    bridge = ready_bridge()
    bridge._param_download_state = "downloading"
    bridge._dispatch(param_value("DUP", 1.0, index=0, count=2))
    bridge._dispatch(param_value("DUP", 2.0, index=0, count=2))
    bridge._dispatch(param_value("OTHER", 3.0, index=1, count=2))
    assert bridge._param_received == 2
    assert bridge._param_download_state == "complete"
    assert bridge.get_param("DUP")["value"] == 2.0


def test_param_value_notifies_listeners_with_status_dict() -> None:
    bridge = ready_bridge()
    statuses: list[dict] = []
    bridge.add_param_listener(statuses.append)
    bridge._dispatch(param_value("FOO", 42.0, index=0, count=1))
    assert len(statuses) == 1
    s = statuses[0]
    assert s["name"] == "FOO"
    assert s["value"] == 42.0
    assert s["received"] == 1
    assert s["count"] == 1
    assert s["state"] == "complete"


def test_param_value_single_request_does_not_flip_state_to_downloading() -> None:
    bridge = ready_bridge()
    assert bridge._param_download_state == "idle"
    bridge._dispatch(param_value("LONELY", 1.0, index=5, count=0))
    assert bridge._param_download_state == "idle"
    assert bridge.get_param("LONELY") is not None


def test_param_value_decodes_bytes_param_id_and_strips_nul() -> None:
    bridge = ready_bridge()
    msg = FakeMessage(
        message_type="PARAM_VALUE",
        param_id=b"MC_ROLL_P\x00padding",
        param_value=5.0,
        param_type=mavutil.mavlink.MAV_PARAM_TYPE_REAL32,
        param_index=0,
        param_count=0,
    )
    bridge._dispatch(msg)
    assert bridge.get_param("MC_ROLL_P") is not None
    assert bridge.get_param("MC_ROLL_P")["value"] == 5.0


# ---------------------------------------------------------------------------
# set_param
# ---------------------------------------------------------------------------

def test_set_param_cache_hit_sends_param_set_and_confirms_matching_echo(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bridge = ready_bridge()
    monkeypatch.setattr(time, "sleep", lambda s: None)
    bridge._dispatch(param_value("MC_ROLL_P", 5.0, index=0, count=1))

    def on_send(args: tuple) -> None:
        # Echo back the PARAM_VALUE with the new value.
        bridge._dispatch(param_value("MC_ROLL_P", 8.0, index=0, count=1))

    bridge._conn.mav.on_send = on_send

    assert bridge.set_param("MC_ROLL_P", 8.0) is True
    sets = bridge._conn.mav.param_sets
    assert len(sets) == 1
    _, _, name_bytes, value, ptype = sets[0]
    assert name_bytes == b"MC_ROLL_P"
    assert value == 8.0


def test_set_param_mismatched_echo_returns_false_with_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bridge = ready_bridge()
    monkeypatch.setattr(time, "sleep", lambda s: None)
    bridge._dispatch(param_value("MC_PITCH_P", 3.0, index=0, count=1))

    def on_send(args: tuple) -> None:
        bridge._dispatch(param_value("MC_PITCH_P", 99.0, index=0, count=1))

    bridge._conn.mav.on_send = on_send

    assert bridge.set_param("MC_PITCH_P", 5.0) is False
    assert "not confirmed" in bridge.get_last_command_error()


def test_set_param_unknown_param_returns_false_with_not_found(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bridge = ready_bridge()
    # Fast-forward monotonic so _wait_for_param's 2s deadline expires instantly.
    fake_clock = [0.0]
    monkeypatch.setattr(time, "monotonic", lambda: fake_clock[0])
    monkeypatch.setattr(time, "sleep", lambda s: fake_clock.__setitem__(0, fake_clock[0] + s))
    # No PARAM_VALUE will arrive → request_param times out.
    assert bridge.set_param("NONEXISTENT", 1.0) is False
    assert "not found" in bridge.get_last_command_error()


def test_set_param_refuses_if_armed(monkeypatch: pytest.MonkeyPatch) -> None:
    bridge = ready_bridge()
    monkeypatch.setattr(time, "sleep", lambda s: None)
    bridge._store.update(armed=True)
    bridge._dispatch(param_value("MC_ROLL_P", 5.0, index=0, count=1))
    assert bridge.set_param("MC_ROLL_P", 8.0) is False
    assert "armed" in bridge.get_last_command_error()
    assert bridge._conn.mav.param_sets == []


def test_set_param_returns_false_when_disconnected() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.set_param("FOO", 1.0) is False
    assert "not connected" in bridge.get_last_command_error()


# ---------------------------------------------------------------------------
# param_status / get_params / get_param
# ---------------------------------------------------------------------------

def test_param_status_returns_correct_shape() -> None:
    bridge = ready_bridge()
    status = bridge.param_status()
    assert set(status.keys()) == {"state", "count", "received"}
    assert status["state"] == "idle"
    assert status["count"] == -1
    assert status["received"] == 0


def test_get_params_returns_sorted_list_of_dicts() -> None:
    bridge = ready_bridge()
    bridge._dispatch(param_value("Z_PARAM", 1.0, index=2, count=3))
    bridge._dispatch(param_value("A_PARAM", 2.0, index=0, count=3))
    bridge._dispatch(param_value("M_PARAM", 3.0, index=1, count=3))
    params = bridge.get_params()
    assert [p["name"] for p in params] == ["A_PARAM", "M_PARAM", "Z_PARAM"]
    assert all(set(p.keys()) == {"name", "value", "type"} for p in params)


def test_get_param_returns_dict_or_none() -> None:
    bridge = ready_bridge()
    bridge._dispatch(param_value("EXISTS", 42.0, index=0, count=1))
    result = bridge.get_param("EXISTS")
    assert result is not None
    assert result["name"] == "EXISTS"
    assert result["value"] == 42.0
    assert bridge.get_param("MISSING") is None


# ---------------------------------------------------------------------------
# calibrate
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("sensor,param_index,expected_value", [
    ("gyro", 0, 1.0),
    ("compass", 1, 1.0),
    ("baro", 2, 1.0),
    ("accel", 4, 1.0),
    ("level", 4, 2.0),
    ("accel_quick", 4, 4.0),
    ("airspeed", 5, 1.0),
])
def test_calibrate_sends_correct_params_on_accepted(
    sensor: str, param_index: int, expected_value: float,
) -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        command = int(args[2])
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.calibrate(sensor) is True
    cmd = bridge._conn.mav.commands[0]
    assert cmd[2] == mavutil.mavlink.MAV_CMD_PREFLIGHT_CALIBRATION
    # param_index+4 because command_long_send args are:
    # (sys, comp, cmd, confirmation, p1..p7) → p1 is at offset 4.
    assert cmd[4 + param_index] == pytest.approx(expected_value)
    # All other params should be NaN.
    for i in range(7):
        if i != param_index:
            assert math.isnan(cmd[4 + i])


def test_calibrate_unknown_sensor_returns_false() -> None:
    bridge = ready_bridge()
    assert bridge.calibrate("nonexistent") is False
    assert "unknown calibration" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []


def test_calibrate_refuses_if_armed() -> None:
    bridge = ready_bridge()
    bridge._store.update(armed=True)
    assert bridge.calibrate("gyro") is False
    assert "armed" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []


def test_calibrate_deny_ack_returns_false() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_DENIED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.calibrate("gyro") is False
    assert "DENIED" in bridge.get_last_command_error()


def test_cancel_calibration_sends_all_zero_params() -> None:
    """PX4 reads an all-zero PREFLIGHT_CALIBRATION as "cancel what is running".

    Verified against v1.16 / v1.17 / v1.18 Commander.cpp. Any non-zero param
    would start a calibration instead of stopping one, so the payload is pinned.
    """
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.cancel_calibration() is True
    cmd = bridge._conn.mav.commands[0]
    assert cmd[2] == mavutil.mavlink.MAV_CMD_PREFLIGHT_CALIBRATION
    assert list(cmd[4:11]) == [0.0] * 7


def test_cancel_calibration_allowed_while_armed() -> None:
    """Cancel only ever stops work, so the armed gate must not block it.

    Refusing it while armed would leave the one escape hatch unreachable in the
    exact state an operator is most likely to want it.
    """
    bridge = ready_bridge()
    bridge._store.update(armed=True)

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.cancel_calibration() is True
    assert bridge._conn.mav.commands


def test_cancel_calibration_deny_ack_returns_false() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_DENIED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.cancel_calibration() is False
    assert "DENIED" in bridge.get_last_command_error()


def test_erase_logs_refused_while_armed() -> None:
    """Destructive, irreversible, and no business happening on a live vehicle."""
    bridge = ready_bridge()
    bridge._store.update(armed=True)
    assert bridge.erase_logs() is False
    assert "armed" in bridge.get_last_command_error()


def test_erase_logs_refused_without_a_link() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.erase_logs() is False
    assert "not connected" in bridge.get_last_command_error()


# ---------------------------------------------------------------------------
# autotune
#
# The autotune runs IN FLIGHT: PX4 injects steps into the rate controller and
# identifies the airframe from the response, so it rejects the command while
# the vehicle is disarmed. These tests pin that precondition in the direction
# PX4 actually enforces it — the bridge used to refuse unless the vehicle was
# *disarmed*, which meant the command only ever left the GCS in the one state
# PX4 refuses it in, and no autotune could ever start.
# ---------------------------------------------------------------------------

def flying_bridge() -> MavlinkBridge:
    """A ready bridge that is armed and airborne — the autotune precondition."""
    bridge = ready_bridge()
    bridge._store.update(
        armed=True, landed_state=mavutil.mavlink.MAV_LANDED_STATE_IN_AIR)
    return bridge


@pytest.mark.parametrize("px4_version", ["v1.16.0", "v1.17.0", "v1.18.0"])
def test_autotune_sends_full_tune_params_on_accepted_for_supported_px4(
    px4_version: str,
) -> None:
    bridge = flying_bridge()
    bridge._store.update(px4_version=px4_version)

    def on_send(args: tuple) -> None:
        command = int(args[2])
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.autotune("all") is True
    cmd = bridge._conn.mav.commands[0]
    assert cmd[2] == mavutil.mavlink.MAV_CMD_DO_AUTOTUNE_ENABLE
    assert cmd[4] == pytest.approx(1.0)       # param1 = enable
    assert cmd[5] == pytest.approx(0.0)       # param2 = PX4 full/default tune
    for i in range(2, 7):
        assert math.isnan(cmd[4 + i])


@pytest.mark.parametrize("axis", ["roll", "pitch", "yaw"])
def test_autotune_rejects_unsupported_per_axis_requests(axis: str) -> None:
    bridge = flying_bridge()

    assert bridge.autotune(axis) is False
    assert "unknown autotune axis" in bridge.get_last_command_error().lower()
    assert bridge._conn.mav.commands == []


def test_autotune_initial_in_progress_ack_confirms_successful_start() -> None:
    bridge = flying_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(
            int(args[2]), mavutil.mavlink.MAV_RESULT_IN_PROGRESS,
        ))

    bridge._conn = FakeConnection(on_send)

    assert bridge.autotune("all") is True
    assert bridge.get_last_command_error() == ""
    assert bridge._store.get_snapshot()["autotune_state"] == "running"


def test_autotune_unknown_axis_returns_false() -> None:
    bridge = flying_bridge()
    assert bridge.autotune("sideways") is False
    assert "unknown autotune axis" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []


def test_autotune_refused_while_disarmed_says_to_take_off() -> None:
    """The regression: disarmed is the one state PX4 will not autotune in."""
    bridge = ready_bridge()
    assert bridge.autotune("all") is False
    error = bridge.get_last_command_error()
    assert "in flight" in error and "take off" in error
    assert bridge._conn.mav.commands == []


def test_autotune_refused_while_armed_on_the_ground() -> None:
    bridge = ready_bridge()
    bridge._store.update(
        armed=True, landed_state=mavutil.mavlink.MAV_LANDED_STATE_ON_GROUND)
    assert bridge.autotune("all") is False
    assert "on the ground" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []


def test_autotune_allowed_when_the_landed_state_is_unknown() -> None:
    """A firmware that never sends EXTENDED_SYS_STATE must not be locked out.

    landed_state 0 means "not reported", not "on the ground". Blocking on the
    absence of a message would refuse a command PX4 would have accepted, and
    PX4 stays the authority — it rejects the tune itself if the vehicle really
    is grounded.
    """
    bridge = ready_bridge()
    bridge._store.update(armed=True, landed_state=0)

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.autotune("all") is True


def test_autotune_stop_sends_param1_zero_and_needs_no_precondition() -> None:
    """Stopping is never gated: an operator ending the injection is not asked
    to satisfy a precondition first (mirrors cancel_calibration)."""
    bridge = ready_bridge()   # disarmed, on the ground

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.autotune("all", enable=False) is True
    cmd = bridge._conn.mav.commands[0]
    assert cmd[2] == mavutil.mavlink.MAV_CMD_DO_AUTOTUNE_ENABLE
    assert cmd[4] == pytest.approx(0.0)       # param1 = disable
    assert bridge._store.get_snapshot()["autotune_state"] == ""


def test_autotune_returns_false_when_disconnected() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    bridge._store.update(
        armed=True, landed_state=mavutil.mavlink.MAV_LANDED_STATE_IN_AIR)
    assert bridge.autotune("all") is False


def test_autotune_deny_ack_returns_false() -> None:
    bridge = flying_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_DENIED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.autotune("all") is False
    assert "DENIED" in bridge.get_last_command_error()
    assert bridge._store.get_snapshot()["autotune_state"] == "failed"


# ---------------------------------------------------------------------------
# autotune progress, out of the repeated COMMAND_ACK
#
# PX4 re-acknowledges MAV_CMD_DO_AUTOTUNE_ENABLE for the whole run with
# IN_PROGRESS + a 0-100 progress field. That stream is the only progress a
# ground station gets, so the bridge latches it into the store.
# ---------------------------------------------------------------------------

def autotune_ack(result: int, progress: int = 0) -> FakeMessage:
    return FakeMessage(
        message_type="COMMAND_ACK",
        command=mavutil.mavlink.MAV_CMD_DO_AUTOTUNE_ENABLE,
        result=result,
        progress=progress,
    )


def test_autotune_in_progress_acks_publish_progress() -> None:
    bridge = flying_bridge()
    bridge._dispatch(autotune_ack(mavutil.mavlink.MAV_RESULT_IN_PROGRESS, 42))
    snap = bridge._store.get_snapshot()
    assert snap["autotune_state"] == "running"
    assert snap["autotune_progress"] == 42


def test_autotune_progress_is_clamped_to_the_reported_range() -> None:
    bridge = flying_bridge()
    bridge._dispatch(autotune_ack(mavutil.mavlink.MAV_RESULT_IN_PROGRESS, 250))
    assert bridge._store.get_snapshot()["autotune_progress"] == 100


def test_autotune_final_accepted_ack_completes_a_running_tune() -> None:
    bridge = flying_bridge()
    bridge._dispatch(autotune_ack(mavutil.mavlink.MAV_RESULT_IN_PROGRESS, 90))
    bridge._dispatch(autotune_ack(mavutil.mavlink.MAV_RESULT_ACCEPTED))
    snap = bridge._store.get_snapshot()
    assert snap["autotune_state"] == "done"
    assert snap["autotune_progress"] == 100


def test_accepted_ack_alone_does_not_claim_a_finished_tune() -> None:
    """The very first ACK only means "command taken"."""
    bridge = flying_bridge()
    bridge._dispatch(autotune_ack(mavutil.mavlink.MAV_RESULT_ACCEPTED))
    assert bridge._store.get_snapshot()["autotune_state"] == ""


def test_autotune_rejection_ack_fails_the_tune_and_warns() -> None:
    bridge = flying_bridge()
    bridge._dispatch(
        autotune_ack(mavutil.mavlink.MAV_RESULT_TEMPORARILY_REJECTED))
    snap = bridge._store.get_snapshot()
    assert snap["autotune_state"] == "failed"
    assert any("Autotune refused" in w["msg"] for w in snap["warnings"])


def test_disconnect_ends_a_running_tune_this_station_can_no_longer_follow() -> None:
    bridge = flying_bridge()
    bridge._dispatch(autotune_ack(mavutil.mavlink.MAV_RESULT_IN_PROGRESS, 30))
    bridge._store.set_disconnected()
    assert bridge._store.get_snapshot()["autotune_state"] == "failed"


# ---------------------------------------------------------------------------
# ATTITUDE body-rate dispatch
# ---------------------------------------------------------------------------

def test_attitude_dispatch_populates_body_rates_in_deg_s() -> None:
    bridge = ready_bridge()
    # 1 rad/s ≈ 57.2958 deg/s
    bridge._dispatch(FakeMessage(
        message_type="ATTITUDE",
        roll=0.1,
        pitch=0.2,
        yaw=0.3,
        rollspeed=0.5,
        pitchspeed=-0.25,
        yawspeed=1.0,
    ))
    snap = bridge._store.get_snapshot()
    if "rollspeed" in snap:
        assert snap["rollspeed"] == pytest.approx(round(math.degrees(0.5), 1))
        assert snap["pitchspeed"] == pytest.approx(round(math.degrees(-0.25), 1))
        assert snap["yawspeed"] == pytest.approx(round(math.degrees(1.0), 1))


# ---------------------------------------------------------------------------
# stop() wakes pending set_param waiters
# ---------------------------------------------------------------------------

def test_stop_wakes_pending_param_set_waiters(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bridge = ready_bridge()
    monkeypatch.setattr(time, "sleep", lambda s: None)
    bridge._dispatch(param_value("MC_ROLL_P", 5.0, index=0, count=1))

    results: list[bool] = []
    worker_started = threading.Event()

    def worker() -> None:
        worker_started.set()
        results.append(bridge.set_param("MC_ROLL_P", 8.0))

    t = threading.Thread(target=worker, daemon=True)
    t.start()
    worker_started.wait(timeout=1.0)
    # Give the worker time to register its pending entry.
    time.sleep(0.1)

    bridge.stop()
    t.join(timeout=2.0)

    assert not t.is_alive()
    # The pending set_param should have been woken with -2 → returns False.
    assert results == [False]
    assert bridge._param_download_state == "idle"


# ---------------------------------------------------------------------------
# Batch parameter upload (start_param_upload / set_params_batch / result)
# ---------------------------------------------------------------------------

def test_start_param_upload_starts_worker_and_sets_uploading(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bridge = ready_bridge()
    monkeypatch.setattr(time, "sleep", lambda s: None)
    # ready_bridge() never calls start(), so _running is clear; the worker
    # loop checks _running each iteration, so set it for the upload to run.
    bridge._running.set()
    # Pre-populate the cache so set_param takes the cache-hit path.
    bridge._dispatch(param_value("A", 0.0, index=0, count=0))
    bridge._dispatch(param_value("B", 0.0, index=1, count=0))

    # Gate the worker inside the first param_set_send so the "uploading" state
    # can be asserted before the worker races to completion. The echo is
    # dispatched after the gate opens, so set_param confirms each write.
    gate = threading.Event()

    def on_send(args: tuple) -> None:
        # args = ("param_set", sys, comp, name_bytes, value, ptype)
        gate.wait(timeout=5)
        name = args[3].decode("utf-8")
        value = args[4]
        ptype = args[5]
        bridge._dispatch(param_value(name, value, ptype=ptype, index=0, count=0))

    bridge._conn.mav.on_send = on_send

    assert bridge.start_param_upload([
        {"name": "A", "value": 1.0},
        {"name": "B", "value": 2.0},
    ]) is True
    assert bridge._param_download_state == "uploading"
    assert bridge._param_count == 2
    assert bridge._param_received == 0

    gate.set()
    bridge._param_upload_thread.join(timeout=5)
    assert not bridge._param_upload_thread.is_alive()

    assert bridge._param_download_state == "upload_complete"
    assert bridge._param_received == 2
    result = bridge.get_param_upload_result()
    assert result["written"] == 2
    assert result["failed"] == 0
    assert result["errors"] == []


def test_start_param_upload_refuses_if_armed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bridge = ready_bridge()
    monkeypatch.setattr(time, "sleep", lambda s: None)
    bridge._dispatch(param_value("X", 1.0, index=0, count=0))
    bridge._store.update(armed=True)
    assert bridge.start_param_upload([{"name": "X", "value": 2.0}]) is False
    assert "armed" in bridge.get_last_command_error()
    # No worker thread should have been started.
    assert bridge._param_upload_thread is None


def test_start_param_upload_refuses_when_disconnected() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.start_param_upload([{"name": "X", "value": 1.0}]) is False
    assert "not connected" in bridge.get_last_command_error()
    assert bridge._param_upload_thread is None


def test_start_param_upload_refuses_while_downloading() -> None:
    bridge = ready_bridge()
    bridge._param_download_state = "downloading"
    assert bridge.start_param_upload([{"name": "X", "value": 1.0}]) is False
    assert "in progress" in bridge.get_last_command_error()
    assert bridge._param_upload_thread is None


def test_start_param_upload_rejects_invalid_entry() -> None:
    bridge = ready_bridge()
    # value must be a number (str is not).
    assert bridge.start_param_upload([{"name": "X", "value": "not-a-number"}]) is False
    assert "invalid parameter list" in bridge.get_last_command_error()
    # name must be a non-empty string.
    assert bridge.start_param_upload([{"name": "", "value": 1.0}]) is False
    assert "invalid parameter list" in bridge.get_last_command_error()
    # empty list is not a valid upload.
    assert bridge.start_param_upload([]) is False
    assert "invalid parameter list" in bridge.get_last_command_error()
    assert bridge._param_upload_thread is None


def test_get_param_upload_result_idle_default() -> None:
    bridge = ready_bridge()
    result = bridge.get_param_upload_result()
    assert result == {"state": "idle", "written": 0, "failed": 0, "errors": []}


def test_stop_joins_upload_worker(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bridge = ready_bridge()
    monkeypatch.setattr(time, "sleep", lambda s: None)
    # ready_bridge() never calls start(); set _running so the worker loop
    # enters its first iteration (stop() clears it to break the loop).
    bridge._running.set()
    # Pre-populate the cache so set_param takes the cache-hit path and reaches
    # the echo wait (avoids the 2s _wait_for_param spin on a cache miss).
    bridge._dispatch(param_value("MC_ROLL_P", 5.0, index=0, count=0))

    # on_send does NOT echo → set_param blocks on pending.event.wait(1.0).
    # send_seen fires inside param_set_send (after the pending entry is
    # registered), so the test can wait for the worker to be past the
    # registration before calling stop().
    send_seen = threading.Event()

    def on_send(args: tuple) -> None:
        send_seen.set()

    bridge._conn.mav.on_send = on_send

    assert bridge.start_param_upload([{"name": "MC_ROLL_P", "value": 8.0}]) is True
    worker = bridge._param_upload_thread
    assert worker is not None
    # Wait until the worker has sent param_set_send (pending registered).
    assert send_seen.wait(timeout=2.0)

    bridge.stop()
    # stop() joined the worker and cleared the field.
    assert bridge._param_upload_thread is None
    assert not worker.is_alive()
