//! Vehicle setup commands: sensor calibration, motor test, autotune, reboot
//! and reboot to bootloader.
//!
//! The twin of `corvus_link`'s setup engine (`protocol/setup.rs`). The
//! command itself is the dialect's: PX4 runs every calibration through
//! `PREFLIGHT_CALIBRATION`, ArduPilot agrees about gyro, baro and the
//! accelerometer and runs its compass and its autotune differently. The
//! engine owns what the commands need to remember (the running calibration,
//! the mode an ArduPilot autotune returns to, the motors tested); the bridge
//! is asked for the round trips, the store and the mode change.

use std::sync::Mutex;

use corvus_link::plain::defs::constant_or;

use super::common::{known_result_text, py_g};
use super::flight::FlightSnapshot;
use crate::dialect::Dialect;
use crate::priority_lock::PriorityLock;

/// How long a cancel waits for the calibration routine's own ACK.
pub const CALIBRATION_CANCEL_ACK_S: f64 = 5.0;
/// A spinning motor with no timeout is a hazard: every test carries a bounded
/// timeout the vehicle counts down itself, so a dropped link cannot leave one running.
pub const MOTOR_TEST_MAX_DURATION_S: f64 = 10.0;

/// What the setup commands ask of the bridge they run on.
pub trait SetupHost: Send + Sync {
    fn connection_ready(&self) -> bool;
    fn set_error(&self, text: &str);
    fn console(&self, source: &str, text: &str, level: &str);
    /// Records "`action` failed: `result`" for the operator; always false.
    fn command_failure(&self, action: &str, result: i64) -> bool;
    fn lock(&self) -> &PriorityLock;
    fn snapshot(&self) -> FlightSnapshot;
    /// The autotune state the store holds: "", "running", "done" or "failed".
    fn autotune_state(&self) -> String;
    fn dialect(&self) -> Dialect;
    fn mav_type(&self) -> i64;
    /// COMMAND_LONG and its ACK. Results in `interim` do not end the wait.
    fn send_command(
        &self,
        command: i64,
        params: &[f64],
        timeout: f64,
        retries: i64,
        accept_in_progress: bool,
        interim: &[i64],
    ) -> i64;
    /// Latches the autotune state, and its progress when given.
    fn update_autotune(&self, state: &str, progress: Option<i64>);
    fn merge_warning(&self, text: &str, level: &str);
    /// A flight mode change (the flight engine's `set_mode`).
    fn set_mode(&self, mode: &str) -> bool;
    /// Does the vehicle offer this mode?
    fn has_mode(&self, mode: &str) -> bool;
    fn reset_parameter_cache(&self);
}

fn accepted() -> i64 {
    constant_or("MAV_RESULT_ACCEPTED", 0)
}

#[derive(Default)]
struct State {
    active_calibration: String,
    autotune_return_mode: String,
    motors_tested: Vec<i64>,
}

/// The setup commands of one bridge.
#[derive(Default)]
pub struct SetupEngine {
    state: Mutex<State>,
}

impl SetupEngine {
    pub fn new() -> SetupEngine {
        SetupEngine::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The calibration currently running, because ArduPilot cancels a
    /// magnetometer fit with a different command than everything else.
    pub fn active_calibration(&self) -> String {
        self.state().active_calibration.clone()
    }

    pub fn autotune_return_mode(&self) -> String {
        self.state().autotune_return_mode.clone()
    }

    /// The motors this session spun, oldest first.
    pub fn motors_tested(&self) -> Vec<i64> {
        self.state().motors_tested.clone()
    }

    /// The calibrations the connected stack can actually run.
    pub fn available_calibrations(&self, host: &dyn SetupHost) -> Vec<&'static str> {
        host.dialect().available_calibrations()
    }

    // ------------------------------------------------------ calibration

    /// Start a sensor calibration. Interactive: the ACCEPTED ack arrives
    /// quickly (the firmware starts a worker) and the operator then follows
    /// the guidance that flows through STATUSTEXT.
    pub fn calibrate(&self, host: &dyn SetupHost, sensor: &str) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        let dialect = host.dialect();
        let Some(plan) = dialect.calibration(sensor) else {
            host.set_error(&if Dialect::px4_calibration_exists(sensor) {
                format!("{} has no {sensor} calibration", dialect.label())
            } else {
                format!("unknown calibration: {sensor}")
            });
            return false;
        };
        if !plan.ok() {
            host.set_error(&plan.unsupported);
            return false;
        }
        // Defense in depth: refuse while armed (both stacks also reject).
        if host.snapshot().armed {
            host.set_error("cannot calibrate while armed");
            return false;
        }
        if !host.connection_ready() {
            return host.command_failure(&format!("Calibrate {sensor}"), -2);
        }
        let result = host.send_command(plan.command, &plan.params, 5.0, 0, false, &[]);
        if result != accepted() {
            return host.command_failure(&format!("Calibrate {sensor}"), result);
        }
        self.state().active_calibration = sensor.to_string();
        host.console("CALIBRATE", &format!("{sensor} calibration started"), "success");
        true
    }

    /// Abort whatever calibration is running. Not gated on the armed state:
    /// this only ever stops work the vehicle is doing.
    pub fn cancel_calibration(&self, host: &dyn SetupHost) -> bool {
        let _gate = host.lock().priority();
        host.set_error("");
        if !host.connection_ready() {
            return host.command_failure("Cancel calibration", -2);
        }
        let active = self.active_calibration();
        let plan = host.dialect().cancel_calibration(&active);
        if !plan.ok() {
            host.set_error(&plan.unsupported);
            return false;
        }
        // PX4 answers a cancel twice while a calibration runs: TEMPORARILY_REJECTED
        // at once (its worker is busy), and ACCEPTED when the routine next
        // checks for a cancel. The first answer is not the verdict.
        let interim = [constant_or("MAV_RESULT_TEMPORARILY_REJECTED", 1)];
        let result = host.send_command(plan.command, &plan.params, CALIBRATION_CANCEL_ACK_S, 0, false, &interim);
        if result != accepted() {
            return host.command_failure("Cancel calibration", result);
        }
        self.state().active_calibration.clear();
        host.console("CALIBRATE", "calibration cancelled", "warning");
        true
    }

    /// Tell the autopilot the aircraft is now in the position it asked for
    /// (ArduPilot waits for this before it measures; PX4 detects the side).
    pub fn accel_calibration_position(&self, host: &dyn SetupHost, position: &str) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        let plan = host.dialect().accel_position(position);
        if !plan.ok() {
            host.set_error(&plan.unsupported);
            return false;
        }
        if !host.connection_ready() {
            return host.command_failure("Calibration position", -2);
        }
        let result = host.send_command(plan.command, &plan.params, 5.0, 0, false, &[]);
        if result != accepted() {
            return host.command_failure("Calibration position", result);
        }
        host.console("CALIBRATE", &format!("position confirmed: {position}"), "info");
        true
    }

    // ------------------------------------------------------- motor test

    /// Spin one motor on the bench so the operator can identify it.
    /// PROPELLERS MUST BE OFF. Disarmed only, a valid motor number, a
    /// throttle inside the protocol range, and a duration the vehicle counts down.
    pub fn motor_test(&self, host: &dyn SetupHost, motor: i64, throttle_pct: f64, duration_s: f64) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        if !(1..=16).contains(&motor) {
            host.set_error("motor must be between 1 and 16");
            return false;
        }
        if !(0.0..=100.0).contains(&throttle_pct) {
            host.set_error("throttle must be between 0 and 100 percent");
            return false;
        }
        let duration = MOTOR_TEST_MAX_DURATION_S.min(duration_s).max(0.0);
        if host.snapshot().armed {
            host.set_error("cannot test motors while armed");
            return false;
        }
        if !host.connection_ready() {
            return host.command_failure(&format!("Motor test {motor}"), -2);
        }
        let plan = host.dialect().motor_test(motor, throttle_pct, duration);
        let result = host.send_command(plan.command, &plan.params, 5.0, 0, false, &[]);
        if result != accepted() {
            return host.command_failure(&format!("Motor test {motor}"), result);
        }
        {
            let mut state = self.state();
            state.motors_tested.retain(|m| *m != motor);
            state.motors_tested.push(motor);
        }
        host.console(
            "MOTOR",
            &format!("motor {motor} test at {}% for {}s", py_g(throttle_pct), py_g(duration)),
            "warning",
        );
        true
    }

    /// Stop a running motor test. Not gated on the armed state, nor on a
    /// preceding test: this only ever stops a motor.
    pub fn stop_motor_test(&self, host: &dyn SetupHost) -> bool {
        let _gate = host.lock().priority();
        host.set_error("");
        if !host.connection_ready() {
            return host.command_failure("Stop motor test", -2);
        }
        let mut ok = true;
        // Every motor, not just the last one tested: the operator pressing
        // Stop wants silence. The ones this session spun go first, the last
        // one first, and a motor above 8 is among them.
        let mut order: Vec<i64> = self.state().motors_tested.iter().rev().copied().collect();
        let tested = order.clone();
        order.extend((1..9).filter(|m| !tested.contains(m)));
        let dialect = host.dialect();
        for motor in order {
            let plan = dialect.motor_stop(motor);
            let result = host.send_command(plan.command, &plan.params, 2.0, 0, false, &[]);
            if result != accepted() {
                ok = false;
            }
        }
        if !ok {
            host.set_error("motor stop not confirmed for every motor");
        }
        host.console("MOTOR", "motor test stopped", "warning");
        ok
    }

    // --------------------------------------------------------- autotune

    /// Start or stop the autotune, however the connected stack runs one.
    /// PX4 runs it as `DO_AUTOTUNE_ENABLE`; ArduPilot's is a flight mode, so
    /// starting is a mode change to AUTOTUNE and stopping one back to where
    /// the vehicle was. Starting is gated on armed and airborne; stopping on nothing.
    pub fn autotune(&self, host: &dyn SetupHost, axis: &str, enable: bool) -> bool {
        if host.dialect().autotune_style() == "mode" {
            return self.autotune_by_mode(host, enable);
        }
        let _gate = host.lock().lock();
        host.set_error("");
        // PX4's multicopter autotune always tunes all three axes: "all" is the
        // only value on the wire.
        let axis_val = if axis == "all" {
            0.0
        } else {
            host.set_error(&format!("unknown autotune axis: {axis}"));
            return false;
        };
        let plan = host.dialect().autotune_plan(host.mav_type(), enable);
        if !plan.ok() {
            host.set_error(&plan.unsupported);
            return false;
        }
        if enable {
            let problem = self.autotune_precondition(host);
            if !problem.is_empty() {
                host.set_error(problem);
                return false;
            }
        }
        if !host.connection_ready() {
            return host.command_failure(&format!("Autotune {axis}"), -2);
        }
        let nan = f64::NAN;
        let result = host.send_command(
            plan.command,
            &[if enable { 1.0 } else { 0.0 }, axis_val, nan, nan, nan, nan, nan],
            5.0,
            0,
            true,
            &[],
        );
        if result != accepted() && result != constant_or("MAV_RESULT_IN_PROGRESS", 5) {
            if enable {
                host.update_autotune("failed", Some(0));
            }
            return host.command_failure(&format!("Autotune {axis}"), result);
        }
        if enable {
            host.update_autotune("running", Some(0));
            host.console("AUTOTUNE", "Autotune started", "success");
        } else {
            host.update_autotune("", Some(0));
            host.console("AUTOTUNE", "Autotune stopped", "warning");
        }
        true
    }

    fn autotune_by_mode(&self, host: &dyn SetupHost, enable: bool) -> bool {
        let dialect = host.dialect();
        let mode = dialect.autotune_mode(host.mav_type());
        if mode.is_empty() {
            host.set_error(&format!("{} has no autotune mode on this vehicle", dialect.label()));
            return false;
        }
        if enable {
            let problem = self.autotune_precondition(host);
            if !problem.is_empty() {
                host.set_error(problem);
                return false;
            }
            let previous = host.snapshot().mode;
            if !host.set_mode(mode) {
                host.update_autotune("failed", Some(0));
                return false;
            }
            // Only after the switch is accepted, so a refused tune does not
            // leave a return mode nobody is going to use.
            self.state().autotune_return_mode = if previous != mode { previous } else { String::new() };
            host.update_autotune("running", Some(0));
            host.console(
                "AUTOTUNE",
                &format!("{mode} engaged. ArduPilot narrates the tune over the console"),
                "success",
            );
            return true;
        }
        // Stopping is leaving the mode. Falling back to LOITER rather than
        // refusing: an operator pressing Stop is flying.
        let mut target = self.autotune_return_mode();
        if !host.has_mode(&target) {
            target = if host.has_mode("LOITER") {
                "LOITER".to_string()
            } else {
                String::new()
            };
        }
        if target.is_empty() {
            host.set_error("no mode to return to. Select one on the mode selector to leave the autotune");
            return false;
        }
        if !host.set_mode(&target) {
            return false;
        }
        self.state().autotune_return_mode.clear();
        host.update_autotune("", Some(0));
        host.console("AUTOTUNE", &format!("Autotune left, now in {target}"), "warning");
        true
    }

    /// Why the autotune cannot start right now, or "" when it can. An unknown
    /// landed state is not treated as being on the ground: PX4 remains the
    /// authority and rejects the tune itself if the vehicle really is grounded.
    pub fn autotune_precondition(&self, host: &dyn SetupHost) -> &'static str {
        let snap = host.snapshot();
        if !snap.armed {
            return "the autotune runs in flight: arm the vehicle and take off before starting it";
        }
        if snap.landed_state.unwrap_or(0) == constant_or("MAV_LANDED_STATE_ON_GROUND", 1) {
            return "the vehicle is still on the ground: hold a stable hover before starting the autotune";
        }
        ""
    }

    /// Latch the autotune's progress and outcome out of its COMMAND_ACK. PX4
    /// re-acknowledges the command for the whole run: one IN_PROGRESS ack per
    /// step carrying `progress` 0 to 100, then a final one with the outcome.
    pub fn handle_autotune_ack(&self, host: &dyn SetupHost, result: i64, progress: i64) {
        if result == constant_or("MAV_RESULT_IN_PROGRESS", 5) {
            host.update_autotune("running", Some(progress.clamp(0, 100)));
            return;
        }
        if result == accepted() {
            // ACCEPTED closes a run that was already reporting progress; on
            // the very first ack it only means "command taken".
            if host.autotune_state() == "running" {
                host.update_autotune("done", Some(100));
            }
            return;
        }
        host.update_autotune("failed", None);
        let text = known_result_text(result).map_or_else(|| format!("result={result}"), str::to_string);
        host.merge_warning(&format!("Autotune refused: {text}"), "warning");
    }

    // ----------------------------------------------------------- reboot

    /// Restart the autopilot so settings read at boot take effect. Refused
    /// while armed, since both stacks would deny it. The parameter cache is
    /// dropped once the vehicle accepts.
    pub fn reboot_autopilot(&self, host: &dyn SetupHost) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        if host.snapshot().armed {
            host.set_error("cannot reboot while armed");
            return false;
        }
        if !host.connection_ready() {
            return host.command_failure("Reboot", -2);
        }
        let result = host.send_command(
            constant_or("MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN", 246),
            &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            3.0,
            0,
            false,
            &[],
        );
        if result != accepted() {
            return host.command_failure("Reboot", result);
        }
        host.reset_parameter_cache();
        host.console("REBOOT", "Reboot accepted. The autopilot restarts now", "success");
        true
    }

    /// Reboot into the USB bootloader, for firmware flashing: param1 = 3
    /// (PX4 v1.16 to v1.18, Commander.cpp). One shot with a short timeout,
    /// since the ACK arrives before the board resets. The caller owns the
    /// teardown order so the bootloader stays reachable on the same device.
    pub fn reboot_to_bootloader(&self, host: &dyn SetupHost) -> bool {
        let _gate = host.lock().lock();
        host.set_error("");
        if host.snapshot().armed {
            host.set_error("cannot reboot to bootloader while armed");
            return false;
        }
        if !host.connection_ready() {
            return host.command_failure("Reboot to bootloader", -2);
        }
        let nan = f64::NAN;
        let result = host.send_command(
            constant_or("MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN", 246),
            &[3.0, nan, nan, nan, nan, nan, nan],
            3.0,
            0,
            false,
            &[],
        );
        if result != accepted() {
            return host.command_failure("Reboot to bootloader", result);
        }
        host.console("REBOOT", "Reboot to bootloader accepted", "success");
        true
    }
}
