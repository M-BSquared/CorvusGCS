"""SYS_STATUS prearm-check dispatch: the "ready to fly" signal in the top bar.

PX4 mirrors its preflight checks into the MAV_SYS_STATUS_PREARM_CHECK bit of
SYS_STATUS. The bar turns that into READY / NOT READY, so the mapping has to
be exact in one specific way: firmware that never publishes the bit must yield
None (unknown), never a green light the vehicle did not give.
"""
from __future__ import annotations

from types import SimpleNamespace
from typing import Callable

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
    def command_long_send(self, *args: float) -> None:
        pass


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
