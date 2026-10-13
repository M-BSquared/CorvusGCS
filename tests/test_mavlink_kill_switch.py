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
from corvus.mavlink_bridge import KILL_RECHECK_S, KILL_SETTLE_S, PREARM_REPORT_GAP_S, MavlinkBridge
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


# --- a switch the station never saw move ------------------------------------

def sys_status(ready: bool):
    """SYS_STATUS with the prearm check published, passing or failing."""
    bit = mv.mavlink.MAV_SYS_STATUS_PREARM_CHECK
    return mv.mavlink.MAVLink_sys_status_message(
        bit, bit, bit if ready else 0, 0, 12600, 350, 87, 0, 0, 0, 0, 0, 0)


def test_the_check_an_engaged_switch_fails_is_recognised_per_stack() -> None:
    killed = PX4.prearm_event("Kill switch engaged")
    assert killed is not None and killed == PX4.vehicle_event("Kill engaged"), (
        "the same line as the switch's own announcement, so the board merges them")
    stopped = ARDUPILOT.prearm_event("Motors Emergency Stopped")
    assert stopped is not None and stopped == ARDUPILOT.vehicle_event("RC7: MotorEStop HIGH")
    assert PX4.prearm_event("Accel 0 uncalibrated") is None
    assert PX4.prearm_event("Motors Emergency Stopped") is None, "ArduPilot's words are not PX4's"
    assert ARDUPILOT.prearm_event("Kill switch engaged") is None
    assert GENERIC.prearm_event("Kill switch engaged") is not None
    assert GENERIC.prearm_event("Motors Emergency Stopped") is not None


def test_a_switch_engaged_before_the_link_came_up_reads_kill_switch() -> None:
    """The announcement went out before this station was listening. The
    heartbeat alone said TERMINATED; the preflight check names the switch."""
    b = bridge()
    b._dispatch(heartbeat(False, mv.mavlink.MAV_STATE_FLIGHT_TERMINATION))
    assert b._store.get_snapshot()["kill_switch"] is False

    b._publish_statustext("Preflight Fail: Kill switch engaged", 2)
    snap = b._store.get_snapshot()
    assert snap["kill_switch"] is True
    assert snap["prearm_reasons"] == ["Kill switch engaged"], "still counted as a failing check"
    assert warnings(b) == [("critical", "Kill switch engaged. Motors stopped", "kill")]

    # PX4 repeats the report whenever it is asked. The board says it once.
    b._publish_statustext("Preflight Fail: Kill switch engaged", 2)
    assert len(warnings(b)) == 1


def test_a_switch_seen_moving_is_not_said_twice_by_its_check() -> None:
    b = bridge()
    b._publish_statustext("Kill engaged", 6)
    b._publish_statustext("Preflight Fail: Kill switch engaged", 2)
    assert warnings(b) == [("critical", "Kill switch engaged. Motors stopped", "kill")]


def test_ardupilot_names_an_emergency_stop_by_its_check_as_well() -> None:
    b = bridge(autopilot.STACK_ARDUPILOT)
    b._publish_statustext("PreArm: Motors Emergency Stopped", 4)
    assert b._store.get_snapshot()["kill_switch"] is True
    assert warnings(b) == [("critical", "Emergency stop engaged. Motors stopped", "kill")]


def test_a_release_clears_the_termination_it_explains_at_once() -> None:
    """The heartbeat comes once a second. Waiting for it after a release had
    the bar flash TERMINATED between KILL SWITCH and READY."""
    b = bridge()
    b._publish_statustext("Kill engaged", 6)
    b._dispatch(heartbeat(False, mv.mavlink.MAV_STATE_FLIGHT_TERMINATION))
    b._publish_statustext("Kill disengaged", 6)
    snap = b._store.get_snapshot()
    assert snap["kill_switch"] is False and snap["flight_termination"] is False


def test_a_release_does_not_clear_a_termination_it_does_not_explain() -> None:
    b = bridge()
    b._dispatch(heartbeat(True, mv.mavlink.MAV_STATE_FLIGHT_TERMINATION))
    b._publish_statustext("Kill disengaged", 6)
    assert b._store.get_snapshot()["flight_termination"] is True


# --- resolved lines ---------------------------------------------------------

def resolved(b: MavlinkBridge) -> dict[str, bool]:
    return {w["msg"]: bool(w.get("resolved")) for w in b._store.get_snapshot()["warnings"]}


def test_a_release_resolves_the_kill_line_and_keeps_it_on_the_board() -> None:
    b = bridge()
    b._publish_statustext("Kill engaged", 6)
    b._publish_statustext("Kill disengaged", 6)
    assert resolved(b) == {
        "Kill switch engaged. Motors stopped": True,
        "Kill switch released": False,
    }


def test_a_lifted_lockdown_resolves_the_kill_line_too() -> None:
    b = bridge()
    b._publish_statustext("Kill engaged", 6)
    b._dispatch(heartbeat(False, mv.mavlink.MAV_STATE_FLIGHT_TERMINATION))
    b._dispatch(heartbeat(False, mv.mavlink.MAV_STATE_STANDBY))
    assert resolved(b) == {"Kill switch engaged. Motors stopped": True}


def test_ready_to_fly_resolves_every_failed_check_and_the_switch() -> None:
    b = bridge()
    b._publish_statustext("Preflight Fail: Accel 0 uncalibrated", 2)
    b._publish_statustext("Preflight Fail: Kill switch engaged", 2)
    b._publish_statustext("Battery low, land soon", 4)
    b._dispatch(sys_status(ready=False))
    assert not any(resolved(b).values())

    b._kill_evidence_at -= KILL_SETTLE_S  # a "ready" well after the switch's own line
    b._dispatch(sys_status(ready=True))
    assert resolved(b) == {
        "Preflight Fail: Accel 0 uncalibrated": True,
        "Kill switch engaged. Motors stopped": True,
        "Battery low, land soon": False,
    }, "a warning ready does not answer stays live"
    assert b._store.get_snapshot()["kill_switch"] is False


def test_ready_while_armed_says_nothing_about_the_switch() -> None:
    """PX4 skips the kill switch check while armed, so a pass then is no
    evidence the switch is off."""
    b = bridge()
    b._dispatch(heartbeat(True, mv.mavlink.MAV_STATE_ACTIVE))
    b._publish_statustext("Kill engaged", 2)
    b._dispatch(sys_status(ready=True))
    assert b._store.get_snapshot()["kill_switch"] is True
    assert resolved(b) == {"Kill switch engaged. Motors stopped": False}


def test_arming_resolves_the_failed_checks() -> None:
    b = bridge()
    b._publish_statustext("Preflight Fail: Compass not calibrated", 2)
    b._dispatch(heartbeat(True, mv.mavlink.MAV_STATE_ACTIVE))
    assert resolved(b) == {"Preflight Fail: Compass not calibrated": True}


def test_a_check_that_fails_again_is_live_again() -> None:
    b = bridge()
    b._publish_statustext("Preflight Fail: Compass not calibrated", 2)
    b._dispatch(sys_status(ready=True))
    b._publish_statustext("Preflight Fail: Compass not calibrated", 2)
    assert resolved(b) == {"Preflight Fail: Compass not calibrated": False}


# --- a lossy radio ------------------------------------------------------------
#
# The switch's own line is sent once. When it is lost, or when a SYS_STATUS
# composed before the switch moved arrives after it, the station must still
# end up with the switch's real position, without the operator moving it again.

class Recorder:
    """A connection that records the preflight reports the bridge asks for."""

    def __init__(self) -> None:
        self.requests = 0
        recorder = self

        class Mav:
            def command_long_send(self, _sys, _comp, command, *_rest):
                if command == mv.mavlink.MAV_CMD_RUN_PREARM_CHECKS:
                    recorder.requests += 1

        self.mav = Mav()


def wired(stack: str = autopilot.STACK_PX4) -> tuple[MavlinkBridge, Recorder]:
    b = bridge(stack)
    conn = Recorder()
    b._conn = conn
    return b, conn


def ap_heartbeat(armed: bool = False):
    return heartbeat(armed, mv.mavlink.MAV_STATE_STANDBY, mv.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)


def test_a_stale_ready_does_not_undo_a_switch_that_just_engaged() -> None:
    """The bug behind "I have to flip it again": the arming checks run slower
    than the switch's line is sent, so a "ready" composed before the switch
    moved arrived after it and cleared it."""
    for stack, line in ((autopilot.STACK_PX4, "Kill engaged"),
                        (autopilot.STACK_ARDUPILOT, "RC7: MotorEStop HIGH")):
        b, _conn = wired(stack)
        b._dispatch(sys_status(ready=True))
        b._publish_statustext(line, 6)
        b._dispatch(sys_status(ready=True))
        assert b._store.get_snapshot()["kill_switch"] is True, stack


def test_a_ready_well_after_the_switch_still_releases_it() -> None:
    b, _conn = wired(autopilot.STACK_ARDUPILOT)
    b._publish_statustext("RC7: MotorEStop HIGH", 6)
    b._kill_evidence_at -= KILL_SETTLE_S + 0.1
    b._dispatch(sys_status(ready=True))
    assert b._store.get_snapshot()["kill_switch"] is False


def test_a_flip_to_not_ready_asks_for_the_reason_at_once() -> None:
    """A switch whose line was lost turns "ready" into "not ready". The report
    is asked for then, even with older reasons on the list."""
    b, conn = wired()
    b._publish_statustext("Preflight Fail: Compass not calibrated", 2)
    b._dispatch(sys_status(ready=True))
    b._prearm_requested_at -= 60
    b._publish_statustext("Preflight Fail: Accel 0 uncalibrated", 2)
    b._dispatch(sys_status(ready=False))
    assert conn.requests == 1
    b._dispatch(sys_status(ready=False))
    assert conn.requests == 1, "once per flip, not on every SYS_STATUS"
    b._publish_statustext("Preflight Fail: Kill switch engaged", 2)
    assert b._store.get_snapshot()["kill_switch"] is True


def test_ardupilot_notices_a_release_whose_line_was_lost() -> None:
    """Indoors, without GPS, the vehicle never turns ready, so "ready" could
    not clear the switch: it stayed on the bar until moved again."""
    b, conn = wired(autopilot.STACK_ARDUPILOT)
    b._dispatch(ap_heartbeat())
    b._publish_statustext("PreArm: Motors Emergency Stopped", 2)
    b._publish_statustext("PreArm: GPS 1: Bad fix", 2)
    b._dispatch(sys_status(ready=False))
    assert conn.requests == 0, "the switch was just named: nothing to ask yet"

    b._kill_evidence_at -= KILL_RECHECK_S + 1
    b._prearm_requested_at -= KILL_RECHECK_S + 1
    b._dispatch(sys_status(ready=False))
    assert conn.requests == 1, "asked again while it is held"

    # "RC7: MotorEStop LOW" is lost. The report that answers no longer names it.
    b._prearm_reason_at -= PREARM_REPORT_GAP_S + 1
    b._publish_statustext("PreArm: GPS 1: Bad fix", 2)
    b._dispatch(sys_status(ready=False))
    assert b._store.get_snapshot()["kill_switch"] is True, "a report still arriving is not judged"
    b._prearm_reason_at -= PREARM_REPORT_GAP_S + 1
    b._dispatch(sys_status(ready=False))
    snap = b._store.get_snapshot()
    assert snap["kill_switch"] is False
    assert resolved(b)["Emergency stop engaged. Motors stopped"] is True


def test_ardupilot_keeps_a_switch_the_new_report_still_names() -> None:
    b, _conn = wired(autopilot.STACK_ARDUPILOT)
    b._dispatch(ap_heartbeat())
    b._publish_statustext("RC7: MotorEStop HIGH", 6)
    b._kill_evidence_at -= KILL_RECHECK_S + 1
    b._prearm_requested_at -= KILL_RECHECK_S + 1
    b._dispatch(sys_status(ready=False))
    b._prearm_reason_at -= PREARM_REPORT_GAP_S + 1
    b._publish_statustext("PreArm: Motors Emergency Stopped", 2)
    b._prearm_reason_at -= PREARM_REPORT_GAP_S + 1
    b._dispatch(sys_status(ready=False))
    assert b._store.get_snapshot()["kill_switch"] is True


def test_a_report_from_before_the_request_proves_nothing() -> None:
    b, _conn = wired(autopilot.STACK_ARDUPILOT)
    b._dispatch(ap_heartbeat())
    b._publish_statustext("RC7: MotorEStop HIGH", 6)
    b._publish_statustext("PreArm: GPS 1: Bad fix", 2)   # unprompted, same moment
    b._prearm_reason_at -= PREARM_REPORT_GAP_S + 1
    b._dispatch(sys_status(ready=False))
    assert b._store.get_snapshot()["kill_switch"] is True


def test_px4_is_not_released_by_a_report_that_omits_the_switch() -> None:
    """PX4's text report is not relied on to name every check; its heartbeat
    says whether the motors are locked down."""
    b, _conn = wired()
    b._publish_statustext("Kill engaged", 6)
    b._kill_evidence_at -= KILL_RECHECK_S + 1
    b._prearm_requested_at = b._kill_evidence_at + 0.5
    b._publish_statustext("Preflight Fail: Accel 0 uncalibrated", 2)
    b._prearm_reason_at -= PREARM_REPORT_GAP_S + 1
    b._dispatch(sys_status(ready=False))
    assert b._store.get_snapshot()["kill_switch"] is True


def test_armed_ardupilot_is_not_judged_by_a_report() -> None:
    b, _conn = wired(autopilot.STACK_ARDUPILOT)
    b._dispatch(ap_heartbeat(armed=True))
    b._publish_statustext("RC7: MotorEStop HIGH", 6)
    b._kill_evidence_at -= KILL_RECHECK_S + 1
    b._prearm_requested_at = b._kill_evidence_at + 0.5
    b._publish_statustext("PreArm: GPS 1: Bad fix", 2)
    b._prearm_reason_at -= PREARM_REPORT_GAP_S + 1
    b._dispatch(sys_status(ready=False))
    assert b._store.get_snapshot()["kill_switch"] is True


def test_arming_is_not_a_flip_worth_asking_about() -> None:
    b, conn = wired()
    b._dispatch(sys_status(ready=True))
    b._dispatch(heartbeat(True, mv.mavlink.MAV_STATE_ACTIVE))
    b._prearm_reasons = ["already known"]
    b._dispatch(sys_status(ready=False))
    assert conn.requests == 0
