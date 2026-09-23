"""Vehicle setup commands: sensor calibration, motor test, autotune, bootloader.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`: the methods run on
the bridge and use its link, locks and state (``_conn``, ``_send_lock``,
``_operation_lock``, ``_store``), which the bridge's ``__init__`` sets up.
Nothing here imports the bridge, so the bridge can import this.
"""
from __future__ import annotations

from typing import Any

from pymavlink import mavutil

from . import autopilot as autopilot_dialect
from .mavlink_common import MAV_RESULT_TEXT


class VehicleSetupMixin:
    """The commands the Setup pages send that are not parameter writes."""

    def _init_setup_state(self) -> None:
        """Set up the calibration and autotune state; called from the bridge's __init__."""
        # The calibration currently running, because ArduPilot cancels a
        # magnetometer fit with a different command than everything else.
        self._active_calibration = ""
        # The mode to go back to when an ArduPilot autotune is stopped, since
        # there the tune is a flight mode rather than a command.
        self._autotune_return_mode = ""

    # ------------------------------------------------------------------
    # Sensor calibration
    # ------------------------------------------------------------------

    # The PX4 parameter slots, kept here because they were here first and the
    # simulated vehicle and its tests read them by this name. The live command
    # comes from the dialect — ArduPilot agrees on gyro/baro/accel and on
    # nothing else, and runs its magnetometer fit from a different command
    # entirely. See corvus.autopilot.
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
        that flows through ``_handle_statustext``.

        The command itself is the dialect's. PX4 runs every calibration through
        ``MAV_CMD_PREFLIGHT_CALIBRATION`` and detects each accelerometer
        position by itself; ArduPilot agrees about gyro, baro and the three
        accelerometer forms, runs the compass from ``DO_START_MAG_CAL``, waits
        to be *told* each accelerometer position (see
        :meth:`accel_calibration_position`), and has no ESC calibration on the
        MAVLink side at all.
        """
        with self._operation_lock:
            self._set_command_error("")
            plan = self._dialect.calibration(sensor)
            if plan is None:
                self._set_command_error(
                    f"{self._dialect.label} has no {sensor} calibration"
                    if sensor in autopilot_dialect.PX4Dialect._CALIBRATION
                    else f"unknown calibration: {sensor}"
                )
                return False
            if not plan.ok:
                self._set_command_error(plan.unsupported)
                return False
            # Defense-in-depth: refuse while armed (both stacks also reject).
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot calibrate while armed")
                return False
            if not self._connection_ready():
                return self._command_failure(f"Calibrate {sensor}", -2)
            result = self._send_command_and_wait(
                plan.command, plan.param_list(), timeout=5.0, retries=0,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure(f"Calibrate {sensor}", result)
            self._active_calibration = sensor
            self._console_publish("CALIBRATE", f"{sensor} calibration started", "success")
            return True

    # How long a cancel waits for the calibration routine's own ACK.
    CALIBRATION_CANCEL_ACK_S = 5.0

    def cancel_calibration(self) -> bool:
        """Abort whatever calibration is running.

        PX4's Commander reads an all-zero ``PREFLIGHT_CALIBRATION`` as "cancel
        the calibration in progress" — verified against v1.16, v1.17 and v1.18.
        ArduPilot needs its own ``DO_CANCEL_MAG_CAL`` for a magnetometer fit,
        which is why the running calibration is remembered at all. Without
        either, an operator who starts the wrong calibration, or one that
        stalls waiting for a side it will never see, has no way out except
        power-cycling the autopilot.

        Deliberately not gated on the armed state: this only ever *stops* work
        the vehicle is doing, so refusing it would be a safety regression rather
        than defense in depth.
        """
        with self._operation_lock.priority():
            self._set_command_error("")
            if not self._connection_ready():
                return self._command_failure("Cancel calibration", -2)
            plan = self._dialect.cancel_calibration(self._active_calibration)
            if not plan.ok:
                self._set_command_error(plan.unsupported)
                return False
            # PX4 answers a cancel twice while a calibration runs: Commander
            # says TEMPORARILY_REJECTED at once (its worker is busy), and the
            # calibration routine says ACCEPTED when it next checks for a
            # cancel. The first answer is not the verdict.
            result = self._send_command_and_wait(
                plan.command, plan.param_list(),
                timeout=self.CALIBRATION_CANCEL_ACK_S, retries=0,
                interim=frozenset({mavutil.mavlink.MAV_RESULT_TEMPORARILY_REJECTED}),
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Cancel calibration", result)
            self._active_calibration = ""
            self._console_publish("CALIBRATE", "calibration cancelled", "warning")
            return True

    def accel_calibration_position(self, position: str) -> bool:
        """Tell the autopilot the aircraft is now in the position it asked for.

        This is the step that has no PX4 equivalent. PX4 recognises each of the
        six accelerometer orientations from the accelerometer itself and moves
        on when it is satisfied; ArduPilot asks for a side over STATUSTEXT and
        then waits for ``MAV_CMD_ACCELCAL_VEHICLE_POS`` before measuring, for
        as long as it takes. A ground station that never sends it turns an
        ArduPilot accelerometer calibration into a screen that says "place
        vehicle level" and never changes.
        """
        with self._operation_lock:
            self._set_command_error("")
            plan = self._dialect.accel_position(position)
            if not plan.ok:
                self._set_command_error(plan.unsupported)
                return False
            if not self._connection_ready():
                return self._command_failure("Calibration position", -2)
            result = self._send_command_and_wait(
                plan.command, plan.param_list(), timeout=5.0, retries=0,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Calibration position", result)
            self._console_publish(
                "CALIBRATE", f"position confirmed: {position}", "info")
            return True

    # ------------------------------------------------------------------
    # Motor test
    # ------------------------------------------------------------------

    # MAV_CMD_DO_MOTOR_TEST (209) — verified against PX4 v1.16, v1.17 and v1.18
    # (mavlink_receiver.cpp -> actuator_test). param1 is the 1-based motor,
    # param2 the throttle type (0 = percent), param3 the throttle value,
    # param4 the timeout in seconds, param5 the motor count (0 = this motor
    # only) and param6 the test order (0 = default).
    MOTOR_TEST_THROTTLE_PERCENT = 0.0

    # A spinning motor with no timeout is a hazard: if the link drops mid-test
    # nothing stops it. Every test therefore carries a bounded timeout that PX4
    # enforces on the vehicle itself, so the motor stops even if the GCS dies.
    MOTOR_TEST_MAX_DURATION_S = 10.0

    def motor_test(self, motor: int, throttle_pct: float, duration_s: float) -> bool:
        """Spin one motor on the bench so the operator can identify it.

        PROPELLERS MUST BE OFF. This is the identification tool behind Setup ->
        Motors: it answers "which physical motor is Motor 3?" without arming.
        The UI gates it behind an explicit propellers-removed acknowledgement;
        this layer enforces what it can — disarmed only, a valid motor number, a
        throttle inside the protocol range, and a bounded duration that the
        *vehicle* counts down, so a dropped link cannot leave a motor running.
        """
        with self._operation_lock:
            self._set_command_error("")
            # bool is a subclass of int — reject it so True never becomes motor 1.
            if isinstance(motor, bool) or not isinstance(motor, int) or not 1 <= motor <= 16:
                self._set_command_error("motor must be between 1 and 16")
                return False
            if not 0.0 <= float(throttle_pct) <= 100.0:
                self._set_command_error("throttle must be between 0 and 100 percent")
                return False
            duration = max(0.0, min(self.MOTOR_TEST_MAX_DURATION_S, float(duration_s)))
            # Defense-in-depth: refuse while armed (PX4 also rejects an actuator
            # test on an armed vehicle).
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot test motors while armed")
                return False
            if not self._connection_ready():
                return self._command_failure(f"Motor test {motor}", -2)
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_DO_MOTOR_TEST,
                [float(motor), self.MOTOR_TEST_THROTTLE_PERCENT, float(throttle_pct),
                 duration, 0.0, 0.0, 0.0],
                timeout=5.0, retries=0,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure(f"Motor test {motor}", result)
            self._console_publish(
                "MOTOR", f"motor {motor} test at {throttle_pct:g}% for {duration:g}s",
                "warning")
            return True

    def stop_motor_test(self) -> bool:
        """Stop a running motor test (throttle 0, timeout 0).

        Deliberately not gated on the armed state, and not gated on a valid
        preceding test: this only ever *stops* a motor, so refusing it would be
        a safety regression rather than defense in depth. Mirrors
        :meth:`cancel_calibration`.
        """
        with self._operation_lock.priority():
            self._set_command_error("")
            if not self._connection_ready():
                return self._command_failure("Stop motor test", -2)
            ok = True
            # Every motor, not just the last one tested: the operator pressing
            # Stop wants silence, and a test that was started by something else
            # (or before a reload) is exactly when that matters most.
            for motor in range(1, 9):
                result = self._send_command_and_wait(
                    mavutil.mavlink.MAV_CMD_DO_MOTOR_TEST,
                    [float(motor), self.MOTOR_TEST_THROTTLE_PERCENT, 0.0,
                     0.0, 0.0, 0.0, 0.0],
                    timeout=2.0, retries=0,
                )
                if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                    ok = False
            if not ok:
                self._set_command_error("motor stop not confirmed for every motor")
            self._console_publish("MOTOR", "motor test stopped", "warning")
            return ok

    # ------------------------------------------------------------------
    # Autotune
    # ------------------------------------------------------------------
    #
    # The autotune runs IN FLIGHT. PX4's mc_autotune_attitude_control injects
    # steps into the rate controller and identifies the airframe from the
    # response, which it can only do on an armed, airborne vehicle; the command
    # handler answers TEMPORARILY_REJECTED while the vehicle is disarmed.
    #
    # Corvus used to refuse to send this command *unless* the vehicle was
    # disarmed, copying the armed-refusal that is correct for calibration and
    # parameter writes. Applied here it inverted the precondition, so the only
    # state in which the command was sent was the only state PX4 rejects it in,
    # and the autotune could never run. The gate below is the real one: the
    # vehicle must be armed, and must not be sitting on the ground.
    #
    # Progress is not STATUSTEXT. PX4 answers this command with a repeated
    # COMMAND_ACK carrying MAV_RESULT_IN_PROGRESS and a 0-100 progress field
    # for as long as the tune runs, then a final ACK with the outcome; that
    # stream is latched into the store by _handle_autotune_ack.

    _AUTOTUNE_AXIS_MAP: dict[str, float] = {
        # PX4's multicopter autotune always tunes all three axes: it ignores
        # param2 on v1.16/v1.17 and wants zero on v1.18. The fixed-wing tune
        # does take an axis selection, but through FW_AT_AXES rather than
        # through this command, so "all" stays the only value on the wire.
        "all": 0.0,
    }

    def autotune(self, axis: str = "all", enable: bool = True) -> bool:
        """Start or stop the autotune, however the connected stack runs one.

        **PX4** runs it as a command: ``MAV_CMD_DO_AUTOTUNE_ENABLE`` (212) with
        param1 = 1 to start and 0 to stop, param2 the axis selection, which
        v1.16-v1.18 accept only as 0 ("all"). A successful start is acknowledged
        with ACCEPTED or IN_PROGRESS, and progress arrives as repeated ACKs.

        **ArduPilot** does not implement that command on Copter at all: its
        autotune is a *flight mode*. So starting one is a mode change to
        AUTOTUNE and stopping one is a mode change back to whatever the vehicle
        was in — which is why the previous mode is remembered here. There is no
        progress stream; ArduPilot narrates the tune over STATUSTEXT, which the
        console already shows.

        Starting is gated on the vehicle being armed and airborne, because both
        stacks require it. Stopping is gated on nothing at all: an operator who
        wants the tune to end is never told to satisfy a precondition first, the
        same reasoning as :meth:`cancel_calibration`.
        """
        if self._dialect.autotune_style == "mode":
            return self._autotune_by_mode(enable)
        with self._operation_lock:
            self._set_command_error("")
            axis_val = self._AUTOTUNE_AXIS_MAP.get(axis)
            if axis_val is None:
                self._set_command_error(f"unknown autotune axis: {axis}")
                return False
            plan = self._dialect.autotune_plan(self._mav_type_id, enable)
            if not plan.ok:
                self._set_command_error(plan.unsupported)
                return False
            if enable:
                problem = self._autotune_precondition()
                if problem:
                    self._set_command_error(problem)
                    return False
            if not self._connection_ready():
                return self._command_failure(f"Autotune {axis}", -2)
            nan = float("nan")
            result = self._send_command_and_wait(
                plan.command,
                [1.0 if enable else 0.0, axis_val, nan, nan, nan, nan, nan],
                timeout=5.0, retries=0,
                accept_in_progress=True,
            )
            if result not in (
                mavutil.mavlink.MAV_RESULT_ACCEPTED,
                mavutil.mavlink.MAV_RESULT_IN_PROGRESS,
            ):
                if enable:
                    self._store.update(autotune_state="failed", autotune_progress=0)
                return self._command_failure(f"Autotune {axis}", result)
            if enable:
                self._store.update(autotune_state="running", autotune_progress=0)
                self._console_publish("AUTOTUNE", "Autotune started", "success")
            else:
                self._store.update(autotune_state="", autotune_progress=0)
                self._console_publish("AUTOTUNE", "Autotune stopped", "warning")
            return True

    def _autotune_by_mode(self, enable: bool) -> bool:
        """ArduPilot's autotune: a flight mode, not a command.

        Copter carries no handler for ``MAV_CMD_DO_AUTOTUNE_ENABLE``, so the
        command Corvus used to send was answered with UNSUPPORTED and the tune
        never ran. The mode that *is* the tune is AUTOTUNE on Copter and on
        Plane; a quadplane's QAUTOTUNE is left to the operator, because which of
        the two a quadplane wants depends on which half of the airframe is
        being tuned.
        """
        mode = self._dialect.autotune_mode(self._mav_type_id)
        if not mode:
            self._set_command_error(
                f"{self._dialect.label} has no autotune mode on this vehicle"
            )
            return False
        if enable:
            problem = self._autotune_precondition()
            if problem:
                self._set_command_error(problem)
                return False
            previous = self._store.get_snapshot().get("mode") or ""
            if not self.set_mode(mode):
                self._store.update(autotune_state="failed", autotune_progress=0)
                return False
            # Only after the switch is accepted, so a refused tune does not
            # leave a return mode nobody is going to use.
            self._autotune_return_mode = previous if previous != mode else ""
            self._store.update(autotune_state="running", autotune_progress=0)
            self._console_publish(
                "AUTOTUNE",
                f"{mode} engaged. ArduPilot narrates the tune over the console",
                "success",
            )
            return True
        # Stopping is leaving the mode. Falling back to LOITER rather than
        # refusing matters: an operator pressing Stop is flying, and "I do not
        # know what mode you were in" is not an answer they can use.
        target = self._autotune_return_mode or ""
        if target not in self._mode_values:
            target = "LOITER" if "LOITER" in self._mode_values else ""
        if not target:
            self._set_command_error(
                "no mode to return to. Select one on the mode selector to "
                "leave the autotune"
            )
            return False
        if not self.set_mode(target):
            return False
        self._autotune_return_mode = ""
        self._store.update(autotune_state="", autotune_progress=0)
        self._console_publish("AUTOTUNE", f"Autotune left, now in {target}", "warning")
        return True

    def _autotune_precondition(self) -> str:
        """Why the autotune cannot start right now, or "" when it can.

        The message is the whole point of this method: "cannot autotune" tells
        an operator standing in a field nothing, whereas naming the missing
        precondition tells them what to do next.

        An unknown landed state (0, the firmware does not publish
        EXTENDED_SYS_STATE, or nothing has arrived yet) is not treated as being
        on the ground. Blocking on the absence of a message would refuse a
        command PX4 would have accepted, and PX4 remains the authority — it
        rejects the tune itself if the vehicle really is grounded.
        """
        snapshot = self._store.get_snapshot()
        if not snapshot.get("armed"):
            return ("the autotune runs in flight: arm the vehicle and take off "
                    "before starting it")
        on_ground = mavutil.mavlink.MAV_LANDED_STATE_ON_GROUND
        if int(snapshot.get("landed_state") or 0) == on_ground:
            return ("the vehicle is still on the ground: hold a stable hover "
                    "before starting the autotune")
        return ""

    def _handle_autotune_ack(self, msg: Any) -> None:
        """Latch the autotune's progress and outcome out of its COMMAND_ACK.

        PX4 re-acknowledges MAV_CMD_DO_AUTOTUNE_ENABLE for the whole run: one
        IN_PROGRESS ACK per step carrying `progress` 0-100, then one final ACK
        whose result is the outcome. This is the only progress a ground station
        receives — the module's uORB status topic never leaves the autopilot —
        so it is stored rather than left to scroll past in the console.

        Called from the receive loop for every autotune ACK, including ones
        answering a command another station sent: a tune running on this
        aircraft is this station's business regardless of who started it.
        """
        try:
            result = int(msg.result)
        except (TypeError, ValueError):
            return
        if result == mavutil.mavlink.MAV_RESULT_IN_PROGRESS:
            try:
                progress = int(getattr(msg, "progress", 0) or 0)
            except (TypeError, ValueError):
                progress = 0
            self._store.update(
                autotune_state="running",
                autotune_progress=max(0, min(100, progress)),
            )
            return
        if result == mavutil.mavlink.MAV_RESULT_ACCEPTED:
            # ACCEPTED closes a run that was already reporting progress; on the
            # very first ACK it only means "command taken", which the start
            # path has already recorded as running.
            if self._store.get_snapshot().get("autotune_state") == "running":
                self._store.update(autotune_state="done", autotune_progress=100)
            return
        self._store.update(autotune_state="failed")
        self._store.merge_warning(
            "Autotune refused: " + MAV_RESULT_TEXT.get(result, f"result={result}"),
            "warning",
        )

    # ------------------------------------------------------------------
    # Reboot
    # ------------------------------------------------------------------

    def reboot_autopilot(self) -> bool:
        """Restart the autopilot so settings that are read at boot take effect.

        ``MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN`` with param1 = 1, the rest 0 (the
        companion and other components are left alone). PX4 v1.16 to v1.18
        (Commander.cpp) and ArduPilot both reboot only when disarmed, so this
        refuses first rather than send a command the vehicle will deny.
        Needed after an accelerometer or compass calibration and after
        ``SYS_AUTOSTART``, ``SENS_EN_*``, ``SER_*`` or ``UAVCAN_ENABLE``.

        The parameter cache is dropped once the vehicle accepts: whatever the
        reboot applies is not what the cache holds. The bridge notices the
        restart itself (:meth:`_note_boot_time`, or the link dropping on USB)
        and asks for the stream rates, version and home again.
        """
        with self._operation_lock:
            self._set_command_error("")
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot reboot while armed")
                return False
            if not self._connection_ready():
                return self._command_failure("Reboot", -2)
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN,
                [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                timeout=3.0, retries=0,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Reboot", result)
            self._reset_parameter_cache()
            self._console_publish(
                "REBOOT", "Reboot accepted. The autopilot restarts now", "success")
            return True

    # ------------------------------------------------------------------
    # Reboot to bootloader (Firmware Flash)
    # ------------------------------------------------------------------

    def reboot_to_bootloader(self) -> bool:
        """Reboot the autopilot into its USB bootloader (for firmware flashing).

        Sends MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN with the param that PX4
        v1.16-v1.18 interpret as 'reboot to bootloader'. Refused while armed
        (defense-in-depth, even though PX4 would reject too). Returns True only
        if PX4 ACCEPTED.

        Verified param1 = 3.0 (REBOOT_TO_BOOTLOADER) against PX4 source
        (src/modules/commander/Commander.cpp — the uORB vehicle_command handler,
        NOT commander_helper.cpp which only deals with LEDs/tunes):
          - v1.16.0  Commander.cpp:1254-1256
            `else if ((param1 == 3) && !isArmed() &&
                       (px4_reboot_request(REBOOT_TO_BOOTLOADER, 400_ms) == 0))`
          - v1.17.0  Commander.cpp:1274-1276  (identical)
          - v1.18    Commander.cpp:1415-1417  (v1.18.0-beta2; v1.18.0 final is
            not yet tagged, beta2 is the latest pre-release — identical)
        All three: param1==3 → px4_reboot_request(REBOOT_TO_BOOTLOADER), ACK =
        VEHICLE_CMD_RESULT_ACCEPTED, then the commander parks in a busy loop
        until the board resets. Armed or boards without CONFIG_BOARDCTL_RESET
        fall through to VEHICLE_CMD_RESULT_DENIED. The three target versions
        agree on param1=3, so we send one shot (retries=0) with a short 3 s
        timeout — the ACK arrives before the FC actually resets. We do NOT stop
        the bridge or close the connection here: the caller (FlashService) owns
        teardown ordering so the bootloader stays reachable on the same device.
        """
        with self._operation_lock:
            self._set_command_error("")
            # Defense-in-depth: PX4 gates every reboot branch on !isArmed()
            # (Commander.cpp), so an armed FC would DENY us regardless.
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot reboot to bootloader while armed")
                return False
            if not self._connection_ready():
                return self._command_failure("Reboot to bootloader", -2)
            nan = float("nan")
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN,
                [3.0, nan, nan, nan, nan, nan, nan],
                timeout=3.0, retries=0,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                return self._command_failure("Reboot to bootloader", result)
            self._console_publish("REBOOT", "Reboot to bootloader accepted", "success")
            return True
