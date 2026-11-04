"""Vehicle setup commands: sensor calibration, motor test, autotune, bootloader.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`: the methods run on
the bridge and use its link, locks and state (``_conn``, ``_send_lock``,
``_operation_lock``, ``_store``), which the bridge's ``__init__`` sets up.
Nothing here imports the bridge, so the bridge can import this.

The commands themselves are ``corvus_link.setup.SetupEngine``, in Rust. This
mixin holds the operation lock around each one and keeps the tunables, which
the engine reads from the bridge.
"""
from __future__ import annotations

import sys
from typing import Any

from corvus_link import setup as _engine

from . import autopilot as autopilot_dialect
from .mavlink_common import MAV_RESULT_TEXT  # noqa: F401 - the engine reads it


def _engine_attribute(name: str) -> property:
    return property(
        lambda self: getattr(self._setup_engine, name),
        lambda self, value: setattr(self._setup_engine, name, value))


class VehicleSetupMixin:
    """The commands the Setup pages send that are not parameter writes."""

    # Kept addressable by the names the bridge used. The calibration running
    # is remembered because ArduPilot cancels a magnetometer fit with its own
    # command; the return mode because its autotune is a flight mode.
    _active_calibration = _engine_attribute("active_calibration")
    _autotune_return_mode = _engine_attribute("autotune_return_mode")
    _motors_tested = _engine_attribute("motors_tested")

    def _init_setup_state(self) -> None:
        """Set up the calibration and autotune state; called from the bridge's __init__."""
        self._setup_engine = _engine.SetupEngine(sys.modules[__name__])

    # ------------------------------------------------------------------
    # Sensor calibration
    # ------------------------------------------------------------------

    # The PX4 parameter slots, kept here because they were here first and the
    # simulated vehicle and its tests read them by this name. The live command
    # comes from the dialect. See corvus.autopilot.
    _CALIBRATION_MAP: dict[str, list[float]] = {
        name: plan.param_list()
        for name, plan in autopilot_dialect.PX4Dialect._CALIBRATION.items()
        if plan.ok
    }

    def available_calibrations(self) -> list[str]:
        """The calibrations the connected stack can actually run."""
        return sorted(
            name for name, plan in self._dialect._CALIBRATION.items() if plan.ok
        )

    def calibrate(self, sensor: str) -> bool:
        """Start a sensor calibration on the connected autopilot.

        Calibration is interactive: the ACCEPTED ack arrives quickly (the
        firmware starts a worker), and the operator then follows the guidance
        that flows through ``_handle_statustext``. The command is the
        dialect's; ArduPilot also waits to be told each accelerometer position
        (see :meth:`accel_calibration_position`). Refused while armed.
        """
        with self._operation_lock:
            return self._setup_engine.calibrate(self, sensor)

    # How long a cancel waits for the calibration routine's own ACK.
    CALIBRATION_CANCEL_ACK_S = 5.0

    def cancel_calibration(self) -> bool:
        """Abort whatever calibration is running.

        PX4 cancels on an all-zero ``PREFLIGHT_CALIBRATION`` (v1.16 to v1.18)
        and answers TEMPORARILY_REJECTED before the ACCEPTED that counts;
        ArduPilot needs ``DO_CANCEL_MAG_CAL`` for a magnetometer fit.
        Deliberately not gated on the armed state: it only ever stops work.
        """
        with self._operation_lock.priority():
            return self._setup_engine.cancel_calibration(self)

    def accel_calibration_position(self, position: str) -> bool:
        """Tell the autopilot the aircraft is now in the position it asked for.

        ArduPilot only: it asks for a side over STATUSTEXT and waits for
        ``MAV_CMD_ACCELCAL_VEHICLE_POS`` before measuring. PX4 detects each
        orientation itself.
        """
        with self._operation_lock:
            return self._setup_engine.accel_position(self, position)

    # ------------------------------------------------------------------
    # Motor test
    # ------------------------------------------------------------------

    # A spinning motor with no timeout is a hazard: if the link drops mid-test
    # nothing stops it. Every test therefore carries a bounded timeout that the
    # vehicle enforces itself, so the motor stops even if the GCS dies.
    MOTOR_TEST_MAX_DURATION_S = 10.0

    def motor_test(self, motor: int, throttle_pct: float, duration_s: float) -> bool:
        """Spin one motor on the bench so the operator can identify it.

        PROPELLERS MUST BE OFF. Disarmed only, motor 1 to 16 (a bool is not a
        motor), throttle 0 to 100 percent, and a duration capped at
        :attr:`MOTOR_TEST_MAX_DURATION_S` that the vehicle counts down. The
        command is the dialect's: ``ACTUATOR_TEST`` on PX4, ``DO_MOTOR_TEST``
        on ArduPilot.
        """
        with self._operation_lock:
            return self._setup_engine.motor_test(self, motor, throttle_pct, duration_s)

    def stop_motor_test(self) -> bool:
        """Stop every motor: the ones this session spun first, newest first,
        then the rest of 1 to 8.

        Deliberately not gated on the armed state or on a preceding test: it
        only ever stops a motor. Mirrors :meth:`cancel_calibration`.
        """
        with self._operation_lock.priority():
            return self._setup_engine.stop_motor_test(self)

    # ------------------------------------------------------------------
    # Autotune
    # ------------------------------------------------------------------
    #
    # The autotune runs IN FLIGHT: starting it needs an armed vehicle that is
    # not on the ground (the opposite of the calibration gate). PX4 reports
    # progress as repeated COMMAND_ACKs with IN_PROGRESS, latched into the
    # store by _handle_autotune_ack.

    _AUTOTUNE_AXIS_MAP: dict[str, float] = {
        # PX4's multicopter autotune always tunes all three axes: it ignores
        # param2 on v1.16/v1.17 and wants zero on v1.18.
        "all": 0.0,
    }

    def autotune(self, axis: str = "all", enable: bool = True) -> bool:
        """Start or stop the autotune, however the connected stack runs one.

        **PX4** runs it as ``MAV_CMD_DO_AUTOTUNE_ENABLE`` (param1 1 to start,
        0 to stop, param2 the axis, only 0 on v1.16 to v1.18), acknowledged
        with ACCEPTED or IN_PROGRESS. **ArduPilot** runs it as the AUTOTUNE
        flight mode; stopping returns to the mode the vehicle was in.

        Starting needs the vehicle armed and airborne; stopping needs nothing.
        """
        if self._dialect.autotune_style == "mode":
            return self._autotune_by_mode(enable)
        with self._operation_lock:
            return self._setup_engine.autotune(self, axis, enable)

    def _autotune_by_mode(self, enable: bool) -> bool:
        """ArduPilot's autotune: a mode change, back to the previous mode (or
        LOITER) when stopped."""
        return self._setup_engine.autotune_by_mode(self, enable)

    def _autotune_precondition(self) -> str:
        """Why the autotune cannot start right now, or "" when it can.

        An unknown landed state is not treated as on the ground: PX4 remains
        the authority and rejects the tune itself if it really is.
        """
        return self._setup_engine.autotune_precondition(self)

    def _handle_autotune_ack(self, msg: Any) -> None:
        """Latch the autotune's progress and outcome out of its COMMAND_ACK,
        including ones answering a command another station sent."""
        self._setup_engine.handle_autotune_ack(self, msg)

    # ------------------------------------------------------------------
    # Reboot
    # ------------------------------------------------------------------

    def reboot_autopilot(self) -> bool:
        """Restart the autopilot so settings that are read at boot take effect.

        ``MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN`` param1 = 1; refused while armed,
        as both stacks do. The parameter cache is dropped once the vehicle
        accepts.
        """
        with self._operation_lock:
            return self._setup_engine.reboot(self)

    def reboot_to_bootloader(self) -> bool:
        """Reboot the autopilot into its USB bootloader (for firmware flashing).

        ``MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN`` param1 = 3, verified against PX4
        Commander.cpp in v1.16, v1.17 and v1.18; one shot, 3 s. Refused while
        armed. The caller (FlashService) owns the teardown order, so the link
        stays open here.
        """
        with self._operation_lock:
            return self._setup_engine.reboot_to_bootloader(self)
