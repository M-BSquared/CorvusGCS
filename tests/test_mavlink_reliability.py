"""Regression tests for the link and command reliability fixes.

Each test pins one behaviour that was wrong before: a link that stayed
"degraded" after its heartbeat came back, a restart that left two threads on
one port, a command resent while the vehicle was already carrying it out, a
Disarm that sent nothing, abort commands that queued behind a takeoff or a
mission start, a MISSION_COUNT lost on the radio, a parameter write failed by
an echo that was already on its way, and a motor test above 8 that Stop missed.
"""
from __future__ import annotations

import threading
import time
from types import SimpleNamespace

import pytest
from pymavlink import mavutil

from corvus import autopilot
from corvus import mavlink_bridge as mb
from corvus import mavlink_missions
from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore

M = mavutil.mavlink
REAL32 = M.MAV_PARAM_TYPE_REAL32


def _from_vehicle(msg):
    msg.pack(M.MAVLink(None, srcSystem=1, srcComponent=1))
    return msg


def heartbeat(armed: bool = False, system_status: int = M.MAV_STATE_STANDBY):
    return _from_vehicle(M.MAVLink_heartbeat_message(
        type=M.MAV_TYPE_QUADROTOR, autopilot=M.MAV_AUTOPILOT_PX4,
        base_mode=(M.MAV_MODE_FLAG_SAFETY_ARMED if armed else 0)
        | M.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED,
        custom_mode=(4 << 16) | (3 << 24), system_status=system_status, mavlink_version=3,
    ))


class Vehicle:
    """A fake link: records every send and answers commands with scripted ACKs.

    ``answers[command]`` is the list of results acknowledged for each send of
    that command, or a callable returning one. Anything unscripted is
    ACCEPTED.
    """

    def __init__(self, bridge: MavlinkBridge) -> None:
        self.bridge = bridge
        self.sent: list[tuple] = []
        self.answers: dict[int, object] = {}
        self.on_param_set = None

    def _answer(self, command: int) -> None:
        script = self.answers.get(command, [M.MAV_RESULT_ACCEPTED])
        results = script() if callable(script) else script
        for result in results:
            self.ack(command, result)

    def ack(self, command: int, result: int) -> None:
        self.bridge._dispatch(_from_vehicle(M.MAVLink_command_ack_message(command, result)))

    def command_long_send(self, _ts, _tc, command, confirmation, *params):
        self.sent.append(("long", command, confirmation, params))
        self._answer(command)

    def command_int_send(self, _ts, _tc, _frame, command, *rest):
        self.sent.append(("int", command, rest))
        self._answer(command)

    def heartbeat_send(self, *args):
        pass

    def mission_count_send(self, _ts, _tc, count, *typed):
        self.sent.append(("count", count, typed))

    def mission_ack_send(self, _ts, _tc, result, mission_type):
        self.sent.append(("mission_ack", result, mission_type))

    def param_set_send(self, _ts, _tc, name, value, ptype):
        self.sent.append(("param_set", name, value))
        if self.on_param_set is not None:
            self.on_param_set(value)

    def param(self, name: str, value: float, ptype: int = REAL32) -> None:
        self.bridge._dispatch(_from_vehicle(M.MAVLink_param_value_message(
            name.encode(), value, ptype, 1, 0)))

    def commands(self, command: int) -> list[tuple]:
        return [entry for entry in self.sent if entry[0] in ("long", "int") and entry[1] == command]


def connected(stack: str = autopilot.STACK_PX4) -> tuple[MavlinkBridge, Vehicle]:
    store = VehicleStateStore()
    store.update(connected=True)
    store.heartbeat()
    b = MavlinkBridge(store)
    b._target_system, b._target_component = 1, 1
    b._dialect = autopilot.dialect_for_stack(stack)
    vehicle = Vehicle(b)
    b._conn = SimpleNamespace(mav=vehicle, source_system=254, source_component=190,
                              close=lambda: None)
    return b, vehicle


def hold_abort(b: MavlinkBridge) -> tuple[threading.Thread, threading.Event]:
    """Another thread presses an abort command and holds it until released."""
    release = threading.Event()

    def press() -> None:
        with b._operation_lock.priority():
            release.wait(5)

    thread = threading.Thread(target=press, daemon=True)
    thread.start()
    deadline = time.monotonic() + 2
    while not b._operation_lock.abort_waiting() and time.monotonic() < deadline:
        time.sleep(0.005)
    return thread, release


# --- the link ----------------------------------------------------------------

def test_a_degraded_link_recovers_when_the_heartbeat_returns() -> None:
    b, _vehicle = connected()
    b._mark_degraded_once()
    assert b._store.get_snapshot()["link_status"] == "degraded"
    b._dispatch(heartbeat())
    assert b._store.get_snapshot()["link_status"] == "connected"
    # A second dropout in the same session is reported again.
    b._mark_degraded_once()
    assert b._store.get_snapshot()["link_status"] == "degraded"


def test_a_cycle_a_restart_replaced_closes_what_it_opened(monkeypatch) -> None:
    """stop() could not join a thread blocked opening the port, start() began
    another, and the old one used to carry on with the same port."""
    b = MavlinkBridge(VehicleStateStore(), "udp:127.0.0.1:0")
    opened: list[SimpleNamespace] = []

    def slow_open(*_a, **_k):
        b._run_generation += 1          # the restart, while this was opening
        conn = SimpleNamespace(closed=False)
        conn.close = lambda: setattr(conn, "closed", True)
        opened.append(conn)
        return conn

    monkeypatch.setattr(mb.mavutil, "mavlink_connection", slow_open)
    b._cycle.generation = b._run_generation
    try:
        with pytest.raises(ConnectionError):
            b._connect()
    finally:
        b._cycle.generation = None
    assert opened[0].closed is True
    assert b._conn is None, "the new thread's link is not replaced by the old one's"


def test_an_old_cycle_never_starts_again(monkeypatch) -> None:
    b = MavlinkBridge(VehicleStateStore(), "udp:127.0.0.1:0")
    monkeypatch.setattr(b, "_connect", lambda: pytest.fail("an old cycle connected"))
    b._running.set()
    b._run_generation = 5
    b._run(4)
    b._running.clear()


# --- the command transaction -------------------------------------------------

def test_a_command_in_progress_is_never_resent() -> None:
    b, vehicle = connected()
    command = M.MAV_CMD_PREFLIGHT_CALIBRATION
    vehicle.answers[command] = [M.MAV_RESULT_IN_PROGRESS]
    result = b._send_command_and_wait(command, [], timeout=0.1, retries=2)
    assert result == M.MAV_RESULT_IN_PROGRESS
    assert len(vehicle.commands(command)) == 1


def test_a_command_in_progress_returns_its_final_answer() -> None:
    b, vehicle = connected()
    command = M.MAV_CMD_PREFLIGHT_CALIBRATION

    def in_progress_then_done():
        threading.Timer(0.05, vehicle.ack, (command, M.MAV_RESULT_ACCEPTED)).start()
        return [M.MAV_RESULT_IN_PROGRESS]

    vehicle.answers[command] = in_progress_then_done
    assert b._send_command_and_wait(command, [], timeout=0.5, retries=2) == M.MAV_RESULT_ACCEPTED
    assert len(vehicle.commands(command)) == 1


# --- arming --------------------------------------------------------------------

def test_disarm_is_sent_even_when_telemetry_already_reads_disarmed() -> None:
    """Right after an accepted Arm the store can still read disarmed. A Disarm
    pressed then used to return success without sending anything."""
    b, vehicle = connected()
    assert b.arm(False) is True
    sent = vehicle.commands(M.MAV_CMD_COMPONENT_ARM_DISARM)
    assert len(sent) == 1 and sent[0][3][0] == 0.0


def test_ardupilot_refusing_a_state_it_is_already_in_is_not_a_failure() -> None:
    b, vehicle = connected(autopilot.STACK_ARDUPILOT)
    vehicle.answers[M.MAV_CMD_COMPONENT_ARM_DISARM] = [M.MAV_RESULT_FAILED]
    assert b.arm(False) is True, "disarming a disarmed ArduPilot answers FAILED"
    assert b.arm(True) is False, "but arming one that stays disarmed is a failure"


# --- abort commands are never held up by a sequence --------------------------

def test_aborts_from_the_sequence_itself_do_not_interrupt_it() -> None:
    b, _vehicle = connected()
    mark = b._operation_lock.abort_mark()
    with b._operation_lock.priority():
        pass
    assert b._operation_lock.aborted_since(mark) is False
    thread, release = hold_abort(b)
    release.set()
    thread.join(2)
    assert b._operation_lock.aborted_since(mark) is True


def test_disarm_does_not_wait_for_a_takeoff_to_see_its_arm_confirmed(monkeypatch) -> None:
    """Takeoff held the command lock across the arm confirmation, up to six
    seconds of reading telemetry, and a Disarm pressed then waited for it."""
    b, _vehicle = connected()
    b._home_alt_amsl = 500.0
    disarm_done: list[bool] = []

    def wait_for_armed(armed: bool, timeout: float = 0.0) -> bool:
        if armed:
            thread = threading.Thread(target=lambda: disarm_done.append(b.arm(False)))
            thread.start()
            thread.join(2.0)
        return True

    monkeypatch.setattr(b, "_wait_for_armed_state", wait_for_armed)
    b.takeoff(10.0)
    assert disarm_done == [True]


def test_a_takeoff_stops_once_the_operator_takes_over() -> None:
    b, vehicle = connected()
    b._home_alt_amsl = 500.0
    pressed: list = []

    def accept_then_abort():
        pressed.append(hold_abort(b))
        return [M.MAV_RESULT_ACCEPTED]

    vehicle.answers[M.MAV_CMD_NAV_TAKEOFF] = accept_then_abort
    try:
        assert b.takeoff(10.0) is False
    finally:
        for thread, release in pressed:
            release.set()
            thread.join(2)
    assert vehicle.commands(M.MAV_CMD_COMPONENT_ARM_DISARM) == [], "not armed on top of it"
    assert "took over" in b.get_last_command_error()


def test_a_mission_start_stops_once_the_operator_takes_over() -> None:
    b, vehicle = connected()
    mark = b._operation_lock.abort_mark()
    thread, release = hold_abort(b)
    release.set()
    thread.join(2)
    assert b._start_uploaded_mission("Mission start", 3, mark) is False
    assert vehicle.commands(M.MAV_CMD_MISSION_START) == []


# --- missions --------------------------------------------------------------------

def _item() -> dict:
    return {"frame": 3, "command": 16, "current": 0, "autocontinue": 1,
            "param1": 0.0, "param2": 0.0, "param3": 0.0, "param4": 0.0,
            "x_int": 0, "y_int": 0, "x_f": 0.0, "y_f": 0.0, "z": 10.0}


def test_a_lost_mission_count_is_sent_again(monkeypatch) -> None:
    monkeypatch.setattr(mavlink_missions, "MISSION_COUNT_RETRY_S", 0.05)
    monkeypatch.setattr(mavlink_missions, "MISSION_UPLOAD_QUIET_S", 0.5)
    b, vehicle = connected()
    assert b._upload_mission([_item()]) == -1
    counts = [entry for entry in vehicle.sent if entry[0] == "count"]
    assert len(counts) == mavlink_missions.MISSION_COUNT_TRIES


def test_a_mission_count_answered_is_not_sent_again(monkeypatch) -> None:
    monkeypatch.setattr(mavlink_missions, "MISSION_COUNT_RETRY_S", 0.05)
    b, vehicle = connected()

    def request_then_accept():
        b._mission_last_request = time.monotonic() + 1
        threading.Timer(0.3, lambda: b._handle_mission_ack(_from_vehicle(
            M.MAVLink_mission_ack_message(254, 190, M.MAV_MISSION_ACCEPTED)))).start()

    original = vehicle.mission_count_send

    def count_send(*args):
        original(*args)
        request_then_accept()

    vehicle.mission_count_send = count_send
    assert b._upload_mission([_item()]) == M.MAV_MISSION_ACCEPTED
    assert len([entry for entry in vehicle.sent if entry[0] == "count"]) == 1


def test_a_mission_upload_gives_way_to_an_abort() -> None:
    """A long upload over a radio used to hold RTL until its last item."""
    b, vehicle = connected()
    thread, release = hold_abort(b)
    try:
        assert b._upload_mission([_item()], mission_type=M.MAV_MISSION_TYPE_FENCE) == -3
    finally:
        release.set()
        thread.join(2)
    assert ("mission_ack", mavlink_missions.MAV_MISSION_OPERATION_CANCELLED,
            M.MAV_MISSION_TYPE_FENCE) in vehicle.sent


# --- parameters --------------------------------------------------------------------

def test_an_echo_already_on_its_way_does_not_fail_a_write() -> None:
    b, vehicle = connected()
    vehicle.param("MPC_XY_VEL_MAX", 12.0)

    def answer(value: float) -> None:
        vehicle.param("MPC_XY_VEL_MAX", 12.0)    # a download's frame, still in flight
        threading.Timer(0.1, vehicle.param, ("MPC_XY_VEL_MAX", value)).start()

    vehicle.on_param_set = answer
    assert b.set_param("MPC_XY_VEL_MAX", 10.0) is True, b.get_last_command_error()
    assert len([entry for entry in vehicle.sent if entry[0] == "param_set"]) == 1


# --- motor test ----------------------------------------------------------------------

def test_stop_reaches_a_motor_above_eight() -> None:
    b, vehicle = connected()
    assert b.motor_test(12, 10.0, 1.0) is True
    vehicle.sent.clear()
    assert b.stop_motor_test() is True
    stops = vehicle.commands(autopilot.MAV_CMD_ACTUATOR_TEST)
    assert stops and stops[0][3][4] == 12.0, "the motor that was spun is stopped first"
    assert {entry[3][4] for entry in stops} >= {12.0, 1.0, 8.0}
