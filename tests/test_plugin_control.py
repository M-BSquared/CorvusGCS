"""Flying the vehicle from a plugin: the command list, the new endpoints, and
naming a vehicle of the fleet in a request.

What is pinned here:

* a plugin can send only the MAV_CMDs the dialect lists for the connected
  stack, and none of the ones that have a sequence of their own (arm,
  takeoff, mode) or do lasting harm (reboot, calibration, termination);
* a command that names a place goes out as COMMAND_INT, and on PX4 its
  altitude above home is converted to AMSL, which is what PX4 reads;
* ``"vehicle": "<link>:<system id>"`` reaches that vehicle's bridge and
  nobody else's, and never moves the operator's selection.
"""
from __future__ import annotations

import http.client
import json
import math
import threading
from types import SimpleNamespace
from typing import Any
from collections.abc import Callable

import pytest
from pymavlink import mavutil

import corvus.server as srv
from corvus import autopilot
from corvus.mavlink_bridge import MavlinkBridge
from corvus.server import CorvusHandler
from corvus.state_store import VehicleStateStore

M = mavutil.mavlink


# ---------------------------------------------------------------------------
# The dialect's list
# ---------------------------------------------------------------------------

# Commands that have an endpoint with the stack's own sequence behind it, or
# that do lasting harm from a single call. None may ever be on a plugin list.
NEVER = (
    M.MAV_CMD_COMPONENT_ARM_DISARM, M.MAV_CMD_NAV_TAKEOFF, M.MAV_CMD_DO_SET_MODE,
    M.MAV_CMD_MISSION_START, M.MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN,
    M.MAV_CMD_PREFLIGHT_CALIBRATION, M.MAV_CMD_PREFLIGHT_STORAGE,
    M.MAV_CMD_DO_FLIGHTTERMINATION, M.MAV_CMD_DO_FENCE_ENABLE,
    M.MAV_CMD_DO_MOTOR_TEST, M.MAV_CMD_SET_MESSAGE_INTERVAL,
)


@pytest.mark.parametrize("stack", [autopilot.STACK_PX4, autopilot.STACK_ARDUPILOT,
                                   autopilot.STACK_GENERIC])
def test_no_stack_lets_a_plugin_send_a_command_with_a_sequence_of_its_own(stack: str) -> None:
    table = autopilot.dialect_for_stack(stack).plugin_commands(M.MAV_TYPE_QUADROTOR)
    assert not set(NEVER) & set(table)


def test_each_stack_lists_what_it_handles() -> None:
    px4 = autopilot.dialect_for_stack(autopilot.STACK_PX4).plugin_commands(2)
    ardupilot = autopilot.dialect_for_stack(autopilot.STACK_ARDUPILOT).plugin_commands(2)
    assert M.MAV_CMD_DO_ORBIT in px4 and M.MAV_CMD_DO_ORBIT not in ardupilot
    assert M.MAV_CMD_DO_SET_SERVO in ardupilot and M.MAV_CMD_DO_SET_SERVO not in px4
    for both in (M.MAV_CMD_DO_CHANGE_SPEED, M.MAV_CMD_DO_REPOSITION):
        assert both in px4 and both in ardupilot
    assert autopilot.dialect_for_stack(autopilot.STACK_GENERIC).plugin_commands(2) == {}


@pytest.mark.parametrize("stack, mav_type, hold, velocity", [
    (autopilot.STACK_PX4, M.MAV_TYPE_QUADROTOR, "LOITER", "OFFBOARD"),
    (autopilot.STACK_PX4, M.MAV_TYPE_FIXED_WING, "LOITER", ""),
    (autopilot.STACK_ARDUPILOT, M.MAV_TYPE_QUADROTOR, "LOITER", "GUIDED"),
    (autopilot.STACK_ARDUPILOT, M.MAV_TYPE_FIXED_WING, "LOITER", ""),
    (autopilot.STACK_ARDUPILOT, M.MAV_TYPE_GROUND_ROVER, "HOLD", "GUIDED"),
    (autopilot.STACK_ARDUPILOT, M.MAV_TYPE_SUBMARINE, "POSHOLD", "GUIDED"),
    (autopilot.STACK_GENERIC, M.MAV_TYPE_QUADROTOR, "", ""),
])
def test_capabilities_name_the_hold_and_velocity_modes(
        stack: str, mav_type: int, hold: str, velocity: str) -> None:
    caps = autopilot.dialect_for_stack(stack).capabilities(mav_type)
    assert caps["hold_mode"] == hold
    assert caps["velocity_mode"] == velocity
    assert caps["commands"] == sorted(caps["commands"])


def test_every_hold_mode_is_a_mode_the_vehicle_can_be_put_in() -> None:
    for stack in (autopilot.STACK_PX4, autopilot.STACK_ARDUPILOT):
        dialect = autopilot.dialect_for_stack(stack)
        for mav_type in (2, 1, 10, 12):
            mode = dialect.hold_mode(mav_type)
            assert mode in dialect.mode_table(mav_type, (1, 18, 0)), (stack, mav_type)


# ---------------------------------------------------------------------------
# The bridge
# ---------------------------------------------------------------------------

class FakeMav:
    def __init__(self, on_send: Callable[[int], None]) -> None:
        self.commands: list[tuple] = []
        self.command_ints: list[tuple] = []
        self.setpoints: list[tuple] = []
        self.on_send = on_send

    def command_long_send(self, *args: Any) -> None:
        self.commands.append(args)
        self.on_send(int(args[2]))

    def command_int_send(self, *args: Any) -> None:
        self.command_ints.append(args)
        self.on_send(int(args[3]))

    def set_position_target_local_ned_send(self, *args: Any) -> None:
        self.setpoints.append(args)


def bridge_for(stack_id: int, result: int = M.MAV_RESULT_ACCEPTED,
               mav_type: int = M.MAV_TYPE_QUADROTOR) -> MavlinkBridge:
    """A connected bridge on *stack_id* that answers every command with *result*."""
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._latch_dialect(stack_id, mav_type)

    def answer(command: int) -> None:
        bridge._dispatch(SimpleNamespace(
            get_type=lambda: "COMMAND_ACK", get_srcSystem=lambda: 1,
            command=command, result=result, target_system=255, target_component=190,
        ))

    bridge._conn = SimpleNamespace(mav=FakeMav(answer), source_system=255,
                                   source_component=190)
    return bridge


def test_a_listed_command_goes_out_as_command_long() -> None:
    bridge = bridge_for(M.MAV_AUTOPILOT_PX4)
    assert bridge.plugin_command(M.MAV_CMD_DO_CHANGE_SPEED, [1.0, 5.0, -1.0])
    sent = bridge._conn.mav.commands[-1]
    assert sent[2] == M.MAV_CMD_DO_CHANGE_SPEED
    assert sent[4:7] == (1.0, 5.0, -1.0)
    assert all(math.isnan(v) for v in sent[7:]), "unset params travel as NaN"


def test_a_command_off_the_list_is_refused_without_sending() -> None:
    bridge = bridge_for(M.MAV_AUTOPILOT_PX4)
    assert not bridge.plugin_command(M.MAV_CMD_DO_FLIGHTTERMINATION, [1.0])
    assert not bridge._conn.mav.commands
    assert bridge.get_last_command_error() == (
        "Command DO_FLIGHTTERMINATION is not available on this autopilot")
    assert not bridge.plugin_command(M.MAV_CMD_DO_SET_SERVO, [9.0, 1500.0]), (
        "ArduPilot's servo command is not sent to PX4")


def test_a_refusal_by_the_vehicle_is_reported() -> None:
    bridge = bridge_for(M.MAV_AUTOPILOT_ARDUPILOTMEGA, result=M.MAV_RESULT_DENIED)
    assert not bridge.plugin_command(M.MAV_CMD_DO_SET_SERVO, [9.0, 1500.0])
    assert "DENIED" in bridge.get_last_command_error()


def test_a_place_on_px4_is_sent_as_command_int_at_its_amsl_height() -> None:
    bridge = bridge_for(M.MAV_AUTOPILOT_PX4)
    bridge._home_alt_amsl = 500.0
    assert bridge.plugin_command(
        M.MAV_CMD_DO_REPOSITION, [-1.0, 1.0], (48.0812345, 11.6405678, 30.0, "relative"))
    assert not bridge._conn.mav.commands
    sent = bridge._conn.mav.command_ints[-1]
    assert sent[2] == M.MAV_FRAME_GLOBAL
    assert sent[10:] == (480812345, 116405678, 530.0)


def test_a_place_on_ardupilot_keeps_its_relative_frame() -> None:
    bridge = bridge_for(M.MAV_AUTOPILOT_ARDUPILOTMEGA)
    assert bridge.plugin_command(
        M.MAV_CMD_DO_SET_ROI_LOCATION, [], (48.0, 11.0, 5.0, "relative"))
    sent = bridge._conn.mav.command_ints[-1]
    assert sent[2] == M.MAV_FRAME_GLOBAL_RELATIVE_ALT and sent[12] == 5.0


def test_px4_without_a_home_height_refuses_a_relative_place() -> None:
    bridge = bridge_for(M.MAV_AUTOPILOT_PX4)
    bridge._await_takeoff_reference = lambda _alt: None  # type: ignore[method-assign]
    assert not bridge.plugin_command(
        M.MAV_CMD_DO_REPOSITION, [], (48.0, 11.0, 30.0, "relative"))
    assert not bridge._conn.mav.command_ints


def test_a_place_is_required_for_and_only_for_a_place_command() -> None:
    bridge = bridge_for(M.MAV_AUTOPILOT_PX4)
    assert not bridge.plugin_command(M.MAV_CMD_DO_REPOSITION, [])
    assert not bridge.plugin_command(M.MAV_CMD_DO_CHANGE_SPEED, [1.0], (48.0, 11.0, 0.0, "amsl"))
    assert not bridge._conn.mav.commands and not bridge._conn.mav.command_ints


def test_hold_switches_to_the_stack_hold_mode() -> None:
    bridge = bridge_for(M.MAV_AUTOPILOT_ARDUPILOTMEGA, mav_type=M.MAV_TYPE_GROUND_ROVER)
    asked: list[str] = []
    bridge.set_mode = lambda mode: asked.append(mode) or True  # type: ignore[method-assign]
    assert bridge.hold()
    assert asked == ["HOLD"]


def test_a_velocity_setpoint_flies_the_velocity_and_nothing_else() -> None:
    bridge = bridge_for(M.MAV_AUTOPILOT_PX4)
    assert bridge.velocity_setpoint(2.0, -1.0, 0.5, 30.0, body=True)
    sent = bridge._conn.mav.setpoints[-1]
    assert sent[3] == M.MAV_FRAME_BODY_NED
    mask = sent[4]
    assert mask & 0b111 == 0b111, "position ignored"
    assert mask & (8 | 16 | 32) == 0, "velocity flown"
    assert mask & 1024 and not mask & 2048, "yaw ignored, yaw rate flown"
    assert sent[8:11] == (2.0, -1.0, 0.5)
    assert sent[15] == pytest.approx(math.radians(30.0))

    assert bridge.velocity_setpoint(1.0, 0.0, 0.0)
    assert bridge._conn.mav.setpoints[-1][3] == M.MAV_FRAME_LOCAL_NED
    assert bridge._conn.mav.setpoints[-1][4] & 2048, "no yaw rate given, none flown"


def test_velocity_is_refused_where_no_mode_flies_it() -> None:
    plane = bridge_for(M.MAV_AUTOPILOT_ARDUPILOTMEGA, mav_type=M.MAV_TYPE_FIXED_WING)
    assert not plane.velocity_setpoint(5.0, 0.0, 0.0)
    assert "not available" in plane.get_last_command_error()
    copter = bridge_for(M.MAV_AUTOPILOT_PX4)
    assert not copter.velocity_setpoint(300.0, 0.0, 0.0)
    assert not copter.velocity_setpoint(float("nan"), 0.0, 0.0)
    assert not copter._conn.mav.setpoints


# ---------------------------------------------------------------------------
# The HTTP layer
# ---------------------------------------------------------------------------

class FakeBridge:
    def __init__(self, ok: bool = True, error: str = "") -> None:
        self.ok = ok
        self.error = error
        self.calls: list[tuple] = []

    def plugin_command(self, command: int, params: list[float], position: Any) -> bool:
        self.calls.append(("command", command, params, position))
        return self.ok

    def velocity_setpoint(self, vx: float, vy: float, vz: float,
                          yaw_rate: float | None, body: bool = False) -> bool:
        self.calls.append(("velocity", vx, vy, vz, yaw_rate, body))
        return self.ok

    def hold(self) -> bool:
        self.calls.append(("hold",))
        return self.ok

    def get_last_command_error(self) -> str:
        return self.error


def handler_with(bridge: Any) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


@pytest.mark.parametrize("name", ["DO_CHANGE_SPEED", "MAV_CMD_DO_CHANGE_SPEED",
                                  "do_change_speed", M.MAV_CMD_DO_CHANGE_SPEED])
def test_a_command_is_named_by_name_or_number(name: Any) -> None:
    bridge = FakeBridge()
    handler, responses = handler_with(bridge)
    handler._api_mavlink_command({"command": name, "params": [1, 5, None]})
    assert responses[-1] == ({"ok": True, "command": M.MAV_CMD_DO_CHANGE_SPEED}, 200)
    _, command, params, position = bridge.calls[-1]
    assert command == M.MAV_CMD_DO_CHANGE_SPEED and position is None
    assert params[:2] == [1.0, 5.0] and math.isnan(params[2])


@pytest.mark.parametrize("payload", [
    {"command": "NOT_A_COMMAND"},
    {"command": True},
    {"command": "DO_CHANGE_SPEED", "params": [1] * 8},
    {"command": "DO_CHANGE_SPEED", "params": ["1"]},
    {"command": "DO_CHANGE_SPEED", "params": [float("inf")]},
    {"command": "DO_CHANGE_SPEED", "lat": 48.0, "lon": 11.0},
    {"command": "DO_REPOSITION"},
    {"command": "DO_REPOSITION", "lat": 91.0, "lon": 11.0},
    {"command": "DO_REPOSITION", "lat": 48.0, "lon": 11.0, "frame": "terrain"},
    {"command": "DO_REPOSITION", "params": [1] * 5, "lat": 48.0, "lon": 11.0},
])
def test_a_malformed_command_is_refused_before_the_bridge(payload: dict) -> None:
    bridge = FakeBridge()
    handler, responses = handler_with(bridge)
    handler._api_mavlink_command(payload)
    assert responses[-1][1] == 400
    assert not bridge.calls


def test_a_place_command_carries_its_place() -> None:
    bridge = FakeBridge()
    handler, responses = handler_with(bridge)
    handler._api_mavlink_command({"command": "DO_REPOSITION", "params": [-1, 1],
                                  "lat": 48.1, "lon": 11.6, "alt": 25})
    assert responses[-1][1] == 200
    assert bridge.calls[-1][3] == (48.1, 11.6, 25.0, "relative")


def test_a_refused_command_says_why() -> None:
    handler, responses = handler_with(FakeBridge(False, "Command DO_GRIPPER failed: DENIED"))
    handler._api_mavlink_command({"command": "DO_GRIPPER", "params": [1, 0]})
    assert responses[-1] == ({"ok": False, "error": "Command DO_GRIPPER failed: DENIED"}, 409)
    handler, responses = handler_with(FakeBridge(False, "Command X failed: DISCONNECTED"))
    handler._api_mavlink_command({"command": "DO_GRIPPER"})
    assert responses[-1][1] == 503


def test_velocity_is_validated_and_forwarded() -> None:
    bridge = FakeBridge()
    handler, responses = handler_with(bridge)
    handler._api_mavlink_velocity({"vx": 1, "vy": -2, "vz": 0, "yaw_rate": 15, "frame": "body"})
    assert responses[-1] == ({"ok": True}, 200)
    assert bridge.calls[-1] == ("velocity", 1.0, -2.0, 0.0, 15.0, True)
    for bad in ({"vx": 31}, {"vy": "1"}, {"vz": True}, {"yaw_rate": 181},
                {"frame": "earth"}):
        handler._api_mavlink_velocity(bad)
        assert responses[-1][1] == 400, bad
    assert len(bridge.calls) == 1


def test_hold_endpoint() -> None:
    bridge = FakeBridge()
    handler, responses = handler_with(bridge)
    handler._api_mavlink_hold({})
    assert responses[-1] == ({"ok": True}, 200) and bridge.calls == [("hold",)]


# ---------------------------------------------------------------------------
# Naming a vehicle of the fleet
# ---------------------------------------------------------------------------

@pytest.fixture
def backend(tmp_path, monkeypatch):
    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    monkeypatch.setattr(srv, "default_cache_dir", lambda: str(cache_dir))
    monkeypatch.setattr(MavlinkBridge, "start", lambda self: None)
    config_path = tmp_path / "config.json"
    config_path.write_text("{}", encoding="utf-8")
    server = srv.create_server(port=0, mavlink_conn="udp:127.0.0.1:9999",
                               config_path=str(config_path))
    thread = threading.Thread(target=server.serve_forever, daemon=True,
                              kwargs={"poll_interval": 0.05})
    thread.start()
    yield server
    srv.stop_backend(server)
    thread.join(timeout=5)


def _request(server, method: str, path: str, payload: dict | None = None):
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    try:
        body = json.dumps(payload or {}) if method == "POST" else None
        headers = {"Content-Type": "application/json"} if body is not None else {}
        conn.request(method, path, body=body, headers=headers)
        response = conn.getresponse()
        return response.status, json.loads(response.read() or b"{}")
    finally:
        conn.close()


def test_a_named_vehicle_is_flown_without_being_selected(backend, monkeypatch) -> None:
    server = backend
    status, body = _request(server, "POST", "/api/fleet/links/add",
                            {"connection": "udp:127.0.0.1:9998"})
    assert status == 200
    extra = server.fleet.link("link-2")
    armed: list[tuple[str, bool]] = []
    monkeypatch.setattr(extra.bridge, "controlled_system", lambda: 3)
    monkeypatch.setattr(extra.bridge, "arm", lambda on: armed.append(("extra", on)) or True)
    monkeypatch.setattr(server.mavlink, "arm", lambda on: armed.append(("main", on)) or True)

    status, body = _request(server, "POST", "/api/mavlink/arm",
                            {"arm": True, "vehicle": "link-2:3"})
    assert (status, body) == (200, {"ok": True})
    assert armed == [("extra", True)]
    assert server.fleet.active_is_main(), "the operator's selection did not move"

    status, _ = _request(server, "POST", "/api/mavlink/arm", {"arm": False})
    assert status == 200 and armed[-1] == ("main", False), "without a name, the selected one"

    assert srv.CorvusHandler.mavlink.target is server.mavlink


def test_a_get_names_its_vehicle_in_the_query(backend, monkeypatch) -> None:
    server = backend
    _request(server, "POST", "/api/fleet/links/add", {"connection": "udp:127.0.0.1:9998"})
    extra = server.fleet.link("link-2")
    monkeypatch.setattr(extra.bridge, "controlled_system", lambda: 1)
    extra.store.update(mode="GUIDED")
    status, body = _request(server, "GET", "/api/state?vehicle=link-2:1")
    assert status == 200 and body["mode"] == "GUIDED"
    status, body = _request(server, "GET", "/api/mavlink/capabilities?vehicle=link-2%3A1")
    assert status == 200 and "commands" in body


@pytest.mark.parametrize("vehicle, status", [
    ("nonsense", 400),
    ("link-7:1", 409),      # no such link
    ("link-2:4", 409),      # the link flies system 1
])
def test_a_command_never_reaches_a_vehicle_it_was_not_meant_for(
        backend, monkeypatch, vehicle: str, status: int) -> None:
    server = backend
    _request(server, "POST", "/api/fleet/links/add", {"connection": "udp:127.0.0.1:9998"})
    extra = server.fleet.link("link-2")
    called: list[str] = []
    monkeypatch.setattr(extra.bridge, "controlled_system", lambda: 1)
    for bridge, name in ((extra.bridge, "extra"), (server.mavlink, "main")):
        monkeypatch.setattr(bridge, "land", lambda name=name: called.append(name) or True)
    got, body = _request(server, "POST", "/api/mavlink/land", {"vehicle": vehicle})
    assert got == status and body["ok"] is False
    assert not called


def test_a_vehicle_its_link_has_lost_is_unavailable(backend) -> None:
    server = backend
    _request(server, "POST", "/api/fleet/links/add", {"connection": "udp:127.0.0.1:9998"})
    status, body = _request(server, "POST", "/api/mavlink/hold", {"vehicle": "link-2:1"})
    assert status == 503 and "not connected" in body["error"]
