"""Kill switch, flight termination and disarm reasons.

PX4 keeps reporting armed while its kill switch holds the motors, so the armed
flag alone had the status bar saying the propellers were live over stopped
motors. The heartbeat's system_status is what says otherwise, and the
vehicle's own STATUSTEXT names the cause. Both are asserted here, for PX4 and
ArduPilot, together with the disarm lines the operator is told about.
"""

from __future__ import annotations

from pymavlink import mavutil as mv

from corvus import autopilot
from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore

PX4 = autopilot.dialect_for_stack(autopilot.STACK_PX4)
ARDUPILOT = autopilot.dialect_for_stack(autopilot.STACK_ARDUPILOT)
GENERIC = autopilot.dialect_for_stack(autopilot.STACK_GENERIC)


def bridge(stack: str = autopilot.STACK_PX4) -> MavlinkBridge:
    b = MavlinkBridge(VehicleStateStore())
    b._target_system, b._target_component = 1, 1
    b._dialect = autopilot.dialect_for_stack(stack)
    return b


def heartbeat(armed: bool, system_status: int, ap: int = mv.mavlink.MAV_AUTOPILOT_PX4):
    msg = mv.mavlink.MAVLink_heartbeat_message(
        type=mv.mavlink.MAV_TYPE_QUADROTOR,
        autopilot=ap,
        base_mode=(mv.mavlink.MAV_MODE_FLAG_SAFETY_ARMED if armed else 0)
        | mv.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED,
        custom_mode=(4 << 16) | (3 << 24),
        system_status=system_status, mavlink_version=3,
    )
    msg.pack(mv.mavlink.MAVLink(None, srcSystem=1, srcComponent=1))
    return msg


def warnings(b: MavlinkBridge) -> list[tuple[str, str, str | None]]:
    return [(w["level"], w["msg"], w.get("event")) for w in b._store.get_snapshot()["warnings"]]


# --- the dialect -----------------------------------------------------------

def test_px4_kill_lines_are_recognised_in_both_wordings() -> None:
    for text in ("Kill engaged", "Kill-switch engaged"):
        event = PX4.vehicle_event(text)
        assert event is not None and event.kind == "kill" and event.level == "critical"
    for text in ("Kill disengaged", "Kill-switch disengaged"):
        event = PX4.vehicle_event(text)
        assert event is not None and event.kind == "unkill"


def test_px4_disarm_names_the_reason_and_rates_the_unexpected_ones() -> None:
    kill = PX4.vehicle_event("Disarmed by kill-switch")
    assert kill == autopilot.VehicleEvent("disarm", "warning", "Disarmed by the kill switch")
    landing = PX4.vehicle_event("Disarmed by landing")
    assert landing == autopilot.VehicleEvent("disarm", "info", "Disarmed by auto disarm after landing")
    unknown = PX4.vehicle_event("Disarmed by RC switch")
    assert unknown == autopilot.VehicleEvent("disarm", "info", "Disarmed by RC switch")


def test_other_lines_are_not_events() -> None:
    for text in ("Armed by external command", "Preflight Fail: compass",
                 "Takeoff detected", "Kill switch mapped to channel 7"):
        assert PX4.vehicle_event(text) is None


def test_ardupilot_reads_its_motor_emergency_stop_switch() -> None:
    engaged = ARDUPILOT.vehicle_event("RC7: MotorEStop HIGH")
    assert engaged is not None and engaged.kind == "kill" and engaged.level == "critical"
    released = ARDUPILOT.vehicle_event("RC7: MotorEStop LOW")
    assert released is not None and released.kind == "unkill"
    assert ARDUPILOT.vehicle_event("Kill engaged") is None, "PX4's words are not ArduPilot's"
    assert PX4.vehicle_event("RC7: MotorEStop HIGH") is None


def test_a_stack_neither_dialect_knows_reads_both_wordings() -> None:
    assert GENERIC.vehicle_event("Kill engaged") is not None
    assert GENERIC.vehicle_event("RC8: MotorEStop HIGH") is not None


# --- the bridge ------------------------------------------------------------

def test_a_kill_on_the_ground_is_raised_to_critical_and_sets_the_flag() -> None:
    b = bridge()
    lines: list[dict] = []
    b.add_console_sub(lines.append)
    # PX4 sends a kill on the ground as INFO (severity 6).
    b._publish_statustext("Kill engaged\t", 6)
    assert warnings(b) == [("critical", "Kill switch engaged. Motors stopped", "kill")]
    assert b._store.get_snapshot()["kill_switch"] is True
    assert lines[0]["text"] == "Kill engaged", "the console keeps the vehicle's own words"

    b._publish_statustext("Kill disengaged\t", 6)
    assert b._store.get_snapshot()["kill_switch"] is False
    assert warnings(b)[-1] == ("info", "Kill switch released", "unkill")


def test_a_disarm_line_reaches_the_board_tagged_as_an_event() -> None:
    b = bridge()
    b._publish_statustext("Disarmed by kill-switch\t", 6)
    assert warnings(b) == [("warning", "Disarmed by the kill switch", "disarm")]


def test_the_vehicles_own_severity_is_never_lowered() -> None:
    b = bridge()
    b._publish_statustext("Disarmed by landing", 2)
    assert warnings(b)[0][0] == "critical"


def test_the_heartbeat_reports_flight_termination() -> None:
    b = bridge()
    b._dispatch(heartbeat(True, mv.mavlink.MAV_STATE_ACTIVE))
    snap = b._store.get_snapshot()
    assert snap["armed"] is True and snap["flight_termination"] is False

    b._dispatch(heartbeat(True, mv.mavlink.MAV_STATE_FLIGHT_TERMINATION))
    snap = b._store.get_snapshot()
    assert snap["armed"] is True, "PX4 still reports armed under the kill switch"
    assert snap["flight_termination"] is True


def test_a_lost_release_line_is_repaired_by_the_heartbeat() -> None:
    b = bridge()
    b._publish_statustext("Kill engaged", 2)
    b._dispatch(heartbeat(True, mv.mavlink.MAV_STATE_FLIGHT_TERMINATION))
    assert b._store.get_snapshot()["kill_switch"] is True
    # "Kill disengaged" never arrives; the heartbeat clears the lockdown.
    b._dispatch(heartbeat(True, mv.mavlink.MAV_STATE_ACTIVE))
    snap = b._store.get_snapshot()
    assert snap["flight_termination"] is False and snap["kill_switch"] is False


def test_an_ardupilot_emergency_stop_survives_its_unchanged_heartbeat() -> None:
    """ArduPilot's heartbeat says nothing about an emergency stop, so an
    ordinary heartbeat must not clear the flag the text set."""
    b = bridge(autopilot.STACK_ARDUPILOT)
    b._publish_statustext("RC7: MotorEStop HIGH", 6)
    b._dispatch(heartbeat(True, mv.mavlink.MAV_STATE_ACTIVE, mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA))
    assert b._store.get_snapshot()["kill_switch"] is True


def test_a_disconnect_forgets_the_lockdown() -> None:
    store = VehicleStateStore()
    store.update(kill_switch=True, flight_termination=True)
    store.set_disconnected()
    snap = store.get_snapshot()
    assert snap["kill_switch"] is False and snap["flight_termination"] is False
