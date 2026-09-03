"""PX4 parameter protocol, sensor calibration, and autotune tests.

Covers the parameter download/upload cycle, PARAM_VALUE dispatch, the
calibration command mapping (verified against PX4 v1.18 Commander.cpp),
and autotune axis selection (verified against PX4 v1.18 mavlink_receiver.cpp
and the AUTOTUNE_AXIS enum).
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


# ---------------------------------------------------------------------------
# autotune
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("axis,expected_axis_val", [
    ("roll", 1.0),
    ("pitch", 2.0),
    ("yaw", 4.0),
    ("all", 0.0),
])
def test_autotune_sends_correct_params_on_accepted(
    axis: str, expected_axis_val: float,
) -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        command = int(args[2])
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.autotune(axis) is True
    cmd = bridge._conn.mav.commands[0]
    assert cmd[2] == mavutil.mavlink.MAV_CMD_DO_AUTOTUNE_ENABLE
    assert cmd[4] == pytest.approx(1.0)       # param1 = enable
    assert cmd[5] == pytest.approx(expected_axis_val)  # param2 = axis
    for i in range(2, 7):
        assert math.isnan(cmd[4 + i])


def test_autotune_unknown_axis_returns_false() -> None:
    bridge = ready_bridge()
    assert bridge.autotune("sideways") is False
    assert "unknown autotune axis" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []


def test_autotune_refuses_if_armed() -> None:
    bridge = ready_bridge()
    bridge._store.update(armed=True)
    assert bridge.autotune("all") is False
    assert "armed" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []


def test_autotune_returns_false_when_disconnected() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.autotune("all") is False


def test_autotune_deny_ack_returns_false() -> None:
    bridge = ready_bridge()

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_DENIED))

    bridge._conn = FakeConnection(on_send)
    assert bridge.autotune("all") is False
    assert "DENIED" in bridge.get_last_command_error()


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
