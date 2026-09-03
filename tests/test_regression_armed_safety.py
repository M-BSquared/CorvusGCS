"""Armed-safety regression tests.

Pins the defense-in-depth contract: mutating/destructive preflight actions
(set_param, calibrate, autotune) are REFUSED client-side while the vehicle is
armed (no command/param write leaves the GCS), while the in-flight commands
(takeoff, land, rtl, arm, set_mode) do NOT carry an armed-refusal — they
proceed while armed (an armed vehicle can be disarmed, landed, switched to
RTL, etc.). Mirrors the patterns in ``tests/test_mavlink_params.py`` and
``tests/test_integration_review.py``.
"""
from __future__ import annotations

import math
import sys
from typing import Any

import pytest
from pymavlink import mavutil

_TESTS_DIR = __import__("pathlib").Path(__file__).resolve().parent
if str(_TESTS_DIR) not in sys.path:
    sys.path.insert(0, str(_TESTS_DIR))

# Reuse the shared recording fakes from the params suite.
from test_mavlink_params import (  # noqa: E402
    ack,
    ready_bridge,
)


def _armed_bridge_with_autoack() -> Any:
    """A connected bridge whose conn records every send and auto-ACKs any
    COMMAND_LONG so the in-flight paths finish promptly without a real
    autopilot. Armed is set True on the store (defense-in-depth input)."""
    bridge = ready_bridge()
    bridge._store.update(armed=True)
    assert bridge._store.get_snapshot()["armed"] is True

    def on_send(args: tuple) -> None:
        # args = (sys, comp, cmd, confirmation, p1..p7); args[2] is the cmd id.
        command = int(args[2])
        bridge._dispatch(ack(command, mavutil.mavlink.MAV_RESULT_ACCEPTED))

    bridge._conn.mav.on_send = on_send
    return bridge


# ---------------------------------------------------------------------------
# 1. set_param / calibrate / autotune are REFUSED while armed
# ---------------------------------------------------------------------------

def test_set_param_refused_while_armed_no_write_sent() -> None:
    bridge = _armed_bridge_with_autoack()
    ok = bridge.set_param("MC_ROLL_P", 8.0)
    assert ok is False
    assert "armed" in bridge.get_last_command_error()
    # Defense-in-depth: nothing left the client (no param write, no read).
    assert bridge._conn.mav.param_sets == []
    assert bridge._conn.mav.param_reads == []
    assert bridge._conn.mav.commands == []


def test_calibrate_refused_while_armed_no_command_sent() -> None:
    bridge = _armed_bridge_with_autoack()
    ok = bridge.calibrate("gyro")
    assert ok is False
    assert "armed" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []


def test_autotune_refused_while_armed_no_command_sent() -> None:
    bridge = _armed_bridge_with_autoack()
    ok = bridge.autotune("roll")
    assert ok is False
    assert "armed" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []


@pytest.mark.parametrize("sensor", ["gyro", "compass", "baro", "accel", "airspeed", "motor"])
def test_every_calibration_sensor_refused_while_armed(sensor: str) -> None:
    bridge = _armed_bridge_with_autoack()
    assert bridge.calibrate(sensor) is False
    assert "armed" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []


@pytest.mark.parametrize("axis", ["roll", "pitch", "yaw", "all"])
def test_every_autotune_axis_refused_while_armed(axis: str) -> None:
    bridge = _armed_bridge_with_autoack()
    assert bridge.autotune(axis) is False
    assert "armed" in bridge.get_last_command_error()
    assert bridge._conn.mav.commands == []


# ---------------------------------------------------------------------------
# 2. In-flight commands do NOT refuse while armed
#    (they may fail on other grounds, but never with an "armed" error)
# ---------------------------------------------------------------------------

def test_arm_path_does_not_refuse_while_armed(monkeypatch: pytest.MonkeyPatch) -> None:
    """Disarming an armed vehicle must proceed (the arm path has no
    armed-refusal). _wait_for_armed_state is stubbed so the test does not
    block on the 3 s state-change poll."""
    bridge = _armed_bridge_with_autoack()
    monkeypatch.setattr(
        "corvus.mavlink_bridge.MavlinkBridge._wait_for_armed_state",
        lambda self, armed, timeout=3.0: True,
    )
    ok = bridge.arm(False)  # disarm while armed
    assert ok is True
    assert "armed" not in bridge.get_last_command_error()
    # A DISARM command actually left the client (the path did not refuse).
    assert bridge._conn.mav.commands
    assert int(bridge._conn.mav.commands[-1][2]) == mavutil.mavlink.MAV_CMD_COMPONENT_ARM_DISARM
    # param1 == 0.0 means disarm (1.0 would mean arm).
    assert bridge._conn.mav.commands[-1][4] == 0.0


def test_takeoff_path_does_not_refuse_while_armed() -> None:
    bridge = _armed_bridge_with_autoack()
    # Fails on "no home/global altitude reference" (no real telemetry), NOT
    # on armed — proving the takeoff path has no armed-refusal.
    ok = bridge.takeoff(10.0)
    assert ok is False
    err = bridge.get_last_command_error()
    assert "armed" not in err
    # No TAKEOFF command was sent because the altitude reference check
    # fails first; the armed check never blocked it.
    assert all(int(c[2]) != mavutil.mavlink.MAV_CMD_NAV_TAKEOFF
               for c in bridge._conn.mav.commands)


def test_land_path_proceeds_while_armed() -> None:
    bridge = _armed_bridge_with_autoack()
    ok = bridge.land()
    assert ok is True
    assert "armed" not in bridge.get_last_command_error()
    assert bridge._conn.mav.commands
    assert int(bridge._conn.mav.commands[-1][2]) == mavutil.mavlink.MAV_CMD_NAV_LAND


def test_rtl_path_proceeds_while_armed() -> None:
    bridge = _armed_bridge_with_autoack()
    ok = bridge.rtl()
    assert ok is True
    assert "armed" not in bridge.get_last_command_error()
    assert bridge._conn.mav.commands
    assert int(bridge._conn.mav.commands[-1][2]) == mavutil.mavlink.MAV_CMD_NAV_RETURN_TO_LAUNCH


def test_set_mode_path_does_not_refuse_while_armed() -> None:
    bridge = _armed_bridge_with_autoack()
    # ready_bridge has no mode mapping -> set_mode fails on "unknown mode",
    # NOT on armed. The point is the armed check is absent.
    ok = bridge.set_mode("MANUAL")
    assert ok is False
    err = bridge.get_last_command_error()
    assert "armed" not in err
    # set_mode sends no COMMAND_LONG when the mode is unknown to the GCS.
    assert all(int(c[2]) != mavutil.mavlink.MAV_CMD_DO_SET_MODE
               for c in bridge._conn.mav.commands)


def test_disarmed_vehicle_allows_set_param_calibrate_autotune() -> None:
    """Negative control: with armed=False the refused actions are NOT
    refused on armed grounds (they proceed to their normal failure path).
    Proves the refusal is armed-specific, not unconditional."""
    bridge = ready_bridge()
    assert bridge._store.get_snapshot()["armed"] is False
    # set_param: not refused on armed -> proceeds to "not found" (no param
    # seeded) which is a different, non-armed error.
    ok = bridge.set_param("NEVER_SEEN", 1.0)
    assert ok is False
    assert "armed" not in bridge.get_last_command_error()
    # calibrate: not refused on armed -> sends the command (auto-ack via conn).
    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))
    bridge._conn.mav.on_send = on_send
    assert bridge.calibrate("gyro") is True
    assert "armed" not in bridge.get_last_command_error()
    assert bridge.autotune("all") is True
    assert "armed" not in bridge.get_last_command_error()


def test_calibrate_motor_disarmed_sends_preflight_with_param7_one() -> None:
    """Motor/ESC calibration (disarmed) must send exactly one
    MAV_CMD_PREFLIGHT_CALIBRATION with param7=1.0 and params 1-6 NaN — the
    PX4 Commander.cpp motor/ESC path verified across v1.16/1.17/1.18."""
    bridge = ready_bridge()
    assert bridge._store.get_snapshot()["armed"] is False

    def on_send(args: tuple) -> None:
        bridge._dispatch(ack(int(args[2]), mavutil.mavlink.MAV_RESULT_ACCEPTED))
    bridge._conn.mav.on_send = on_send

    assert bridge.calibrate("motor") is True
    assert "armed" not in bridge.get_last_command_error()

    cmds = bridge._conn.mav.commands
    assert len(cmds) == 1, "exactly one COMMAND_LONG sent (retries=0)"
    cmd = cmds[0]
    # command_long_send args: (sys, comp, cmd, confirm, p1..p7) -> p7 is index 10.
    assert int(cmd[2]) == mavutil.mavlink.MAV_CMD_PREFLIGHT_CALIBRATION
    assert cmd[10] == 1.0, "param7=1.0 selects motor/ESC calibration"
    assert all(math.isnan(cmd[i]) for i in range(4, 10)), "params 1-6 are NaN"
