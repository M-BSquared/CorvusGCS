"""SYS_STATUS prearm-check dispatch: the "ready to fly" signal in the top bar.

PX4 mirrors its preflight checks into the MAV_SYS_STATUS_PREARM_CHECK bit of
SYS_STATUS. The bar turns that into READY / NOT READY, so the mapping has to
be exact in one specific way: firmware that never publishes the bit must yield
None (unknown), never a green light the vehicle did not give.
"""
from __future__ import annotations

from types import SimpleNamespace

from pymavlink import mavutil

from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore

PREARM = mavutil.mavlink.MAV_SYS_STATUS_PREARM_CHECK
# A second, unrelated health bit, so the tests prove we mask rather than
# compare the whole word.
GYRO = mavutil.mavlink.MAV_SYS_STATUS_SENSOR_3D_GYRO


class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return getattr(self, "source_system", 1)


class FakeMav:
    def __init__(self) -> None:
        self.commands: list[tuple] = []

    def command_long_send(self, *args: float) -> None:
        self.commands.append(args)


class FakeConnection:
    def __init__(self) -> None:
        self.mav = FakeMav()
        self.source_system = 255
        self.source_component = 190


def ready_bridge() -> MavlinkBridge:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = FakeConnection()
    return bridge


def sys_status(present: int = 0, enabled: int = 0, health: int = 0) -> FakeMessage:
    return FakeMessage(
        message_type="SYS_STATUS",
        onboard_control_sensors_present=present,
        onboard_control_sensors_enabled=enabled,
        onboard_control_sensors_health=health,
        voltage_battery=12600,
        current_battery=350,
        battery_remaining=87,
    )


def prearm_of(msg: FakeMessage) -> bool | None:
    bridge = ready_bridge()
    bridge._dispatch(msg)
    return bridge._store.get_snapshot()["prearm_ok"]


# ---------------------------------------------------------------------------
# The three verdicts
# ---------------------------------------------------------------------------

def test_prearm_healthy_reports_ready() -> None:
    assert prearm_of(sys_status(enabled=PREARM | GYRO, health=PREARM | GYRO)) is True


def test_prearm_unhealthy_reports_not_ready() -> None:
    """The check is published but failing — the operator must see NOT READY."""
    assert prearm_of(sys_status(enabled=PREARM | GYRO, health=GYRO)) is False


def test_prearm_absent_reports_unknown() -> None:
    """Firmware that never advertises the check: unknown, not a clearance."""
    assert prearm_of(sys_status(enabled=GYRO, health=GYRO)) is None


def test_prearm_present_but_not_enabled_is_still_read() -> None:
    """PX4 advertises the check in `present`; a firmware that lists it there
    but not in `enabled` still reports a real verdict in `health`."""
    assert prearm_of(sys_status(present=PREARM, health=PREARM)) is True
    assert prearm_of(sys_status(present=PREARM, health=0)) is False


def test_prearm_missing_fields_do_not_raise() -> None:
    """Minimal SYS_STATUS from a partial implementation must not kill the
    receive loop; the battery fields in the same branch still land."""
    msg = FakeMessage(
        message_type="SYS_STATUS",
        voltage_battery=12000,
        current_battery=100,
        battery_remaining=50,
    )
    bridge = ready_bridge()
    bridge._dispatch(msg)
    snap = bridge._store.get_snapshot()
    assert snap["prearm_ok"] is None
    assert snap["battery_percent"] == 50


def test_battery_still_updates_alongside_prearm() -> None:
    bridge = ready_bridge()
    bridge._dispatch(sys_status(enabled=PREARM, health=PREARM))
    snap = bridge._store.get_snapshot()
    assert snap["prearm_ok"] is True
    assert snap["battery_voltage"] == 12.6
    assert snap["battery_current"] == 3.5
    assert snap["battery_percent"] == 87


# ---------------------------------------------------------------------------
# Lifecycle
# ---------------------------------------------------------------------------

def test_store_starts_unknown() -> None:
    assert VehicleStateStore().get_snapshot()["prearm_ok"] is None


def test_disconnect_clears_readiness() -> None:
    """A READY that outlives its link would be a stale clearance on the bar."""
    store = VehicleStateStore()
    store.update(prearm_ok=True)
    assert store.get_snapshot()["prearm_ok"] is True
    store.set_disconnected()
    assert store.get_snapshot()["prearm_ok"] is None



# ---------------------------------------------------------------------------
# Why it is not ready
# ---------------------------------------------------------------------------

NOT_READY = dict(enabled=PREARM | GYRO, health=GYRO)
READY = dict(enabled=PREARM | GYRO, health=PREARM | GYRO)
RUN_PREARM_CHECKS = mavutil.mavlink.MAV_CMD_RUN_PREARM_CHECKS


def statustext(text: str, severity: int = 2) -> FakeMessage:
    return FakeMessage(message_type="STATUSTEXT", text=text, severity=severity, id=0,
                       chunk_seq=0)


def reasons(bridge: MavlinkBridge) -> list[str]:
    return bridge._store.get_snapshot()["prearm_reasons"]


def asked(bridge: MavlinkBridge) -> int:
    return sum(1 for c in bridge._conn.mav.commands if int(c[2]) == RUN_PREARM_CHECKS)


def test_px4s_preflight_fail_lines_are_the_reasons() -> None:
    bridge = ready_bridge()
    bridge._dispatch(statustext("Preflight Fail: Accel 0 uncalibrated\t"))
    bridge._dispatch(statustext("Preflight Fail: No valid data from Compass 0"))
    bridge._dispatch(statustext("Preflight Fail: Accel 0 uncalibrated"))
    bridge._dispatch(statustext("Takeoff detected"))
    assert reasons(bridge) == ["Accel 0 uncalibrated", "No valid data from Compass 0"]


def test_a_new_report_replaces_the_last_one(monkeypatch) -> None:
    import corvus.mavlink_bridge as bridge_module

    clock = [100.0]
    monkeypatch.setattr(bridge_module.time, "monotonic", lambda: clock[0])
    bridge = ready_bridge()
    bridge._dispatch(statustext("Preflight Fail: Accel 0 uncalibrated"))
    clock[0] += 10.0
    bridge._dispatch(statustext("Preflight Fail: GPS fix too low"))
    assert reasons(bridge) == ["GPS fix too low"]


def test_ardupilots_prearm_lines_are_read_on_an_ardupilot_link() -> None:
    bridge = ready_bridge()
    bridge._latch_dialect(mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA,
                          mavutil.mavlink.MAV_TYPE_QUADROTOR)
    bridge._dispatch(statustext("PreArm: Compass not calibrated"))
    bridge._dispatch(statustext("Preflight Fail: not ArduPilot wording"))
    assert reasons(bridge) == ["Compass not calibrated"]


def test_a_vehicle_that_says_not_ready_without_a_reason_is_asked_once() -> None:
    bridge = ready_bridge()
    bridge._dispatch(sys_status(**NOT_READY))
    bridge._dispatch(sys_status(**NOT_READY))
    assert asked(bridge) == 1, "throttled, not once per SYS_STATUS"


def test_a_vehicle_that_already_said_why_is_not_asked() -> None:
    bridge = ready_bridge()
    bridge._dispatch(statustext("Preflight Fail: Accel 0 uncalibrated"))
    bridge._dispatch(sys_status(**NOT_READY))
    assert asked(bridge) == 0


def test_a_ready_or_armed_vehicle_has_no_reasons_left() -> None:
    bridge = ready_bridge()
    bridge._dispatch(statustext("Preflight Fail: Accel 0 uncalibrated"))
    bridge._dispatch(sys_status(**READY))
    assert reasons(bridge) == []
    assert asked(bridge) == 0, "a ready vehicle is never asked"

    bridge._dispatch(statustext("Preflight Fail: Accel 0 uncalibrated"))
    bridge._dispatch(FakeMessage(
        message_type="HEARTBEAT", type=mavutil.mavlink.MAV_TYPE_QUADROTOR,
        autopilot=mavutil.mavlink.MAV_AUTOPILOT_PX4,
        base_mode=mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED, custom_mode=0,
        system_status=4))
    assert reasons(bridge) == []


def test_a_dead_link_takes_its_reasons_with_it() -> None:
    bridge = ready_bridge()
    bridge._dispatch(statustext("Preflight Fail: Accel 0 uncalibrated"))
    bridge._store.set_disconnected()
    assert reasons(bridge) == []
