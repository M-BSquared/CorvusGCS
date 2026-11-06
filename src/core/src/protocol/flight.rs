//! Flight commands: flight mode, arming, takeoff, landing, return, home,
//! hold, the plugin command table and velocity setpoints, and the sequences
//! that start a mission (fly to points, mission start, reposition).
//!
//! The twin of `FlightEngine` in `corvus_link` (`protocol/flight.rs`). A
//! sequence is several commands with telemetry waits between them, and the
//! operation lock is held for each command's own round trip only, so Disarm,
//! Land, RTL or a mode change never queue behind a wait. Every later step
//! first asks whether one of those ran meanwhile (`sequence_interrupted`).
//! The stack differences come from the dialect; what belongs to the bridge
//! (the command round trips, the store, the clock, the mission upload) is
//! asked of `FlightHost`.

use corvus_link::plain::defs::{constant_or, enum_entry_name};
use corvus_link::plain::{Message, Value};
use serde_json::Value as Json;

use super::common::{
    e7, fixed, result_text, TAKEOFF_ALTITUDE_MAX_M, TAKEOFF_ALTITUDE_MIN_M, VELOCITY_MAX_MS, YAW_RATE_MAX_DEG_S,
};
use super::missions::MissionItem;
use super::targeted;
use crate::dialect::{is_ground_vehicle, plugin_position_commands, Dialect, ModeCommand};
use crate::priority_lock::PriorityLock;

/// How long to watch for the arm or disarm to show up in telemetry: six
/// heartbeats at 1 Hz, because on a lossy radio three were sometimes one.
pub const ARMED_STATE_CONFIRM_S: f64 = 6.0;
/// Height above home that counts as airborne when the firmware does not
/// publish EXTENDED_SYS_STATE.
pub const AIRBORNE_FALLBACK_M: f64 = 1.5;
/// How long takeoff waits for an altitude reference before it gives up on
/// converting AGL to AMSL.
pub const TAKEOFF_REFERENCE_WAIT_S: f64 = 2.0;
/// How long a mode change is given to show up in telemetry.
pub const MODE_CONFIRM_S: f64 = 3.0;
const DO_REPOSITION_CHANGE_MODE: f64 = 1.0;
const VELOCITY_TYPE_MASK: i64 = 1 | 2 | 4 | 64 | 128 | 256 | 1024;
const YAW_RATE_IGNORE: i64 = 2048;
/// MAV_LANDED_STATE: IN_AIR, TAKEOFF, LANDING.
const AIRBORNE_LANDED_STATES: [i64; 3] = [2, 3, 4];

/// What the store says about the vehicle right now.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlightSnapshot {
    pub connected: bool,
    pub armed: bool,
    pub mode: String,
    pub landed_state: Option<i64>,
    /// Height above home; 0 when the vehicle has not said.
    pub altitude_agl: f64,
}

/// What the flight commands ask of the bridge they run on.
pub trait FlightHost: Send + Sync {
    fn connection_ready(&self) -> bool;
    fn set_error(&self, text: &str);
    fn last_error(&self) -> String;
    fn console(&self, source: &str, text: &str, level: &str);
    fn log(&self, level: &str, text: &str);
    /// A line for the notification board.
    fn store_warning(&self, text: &str, level: &str);
    /// Records "`action` failed: `result`" for the operator; always false.
    fn command_failure(&self, action: &str, result: i64) -> bool;
    /// COMMAND_LONG and its ACK: the MAV_RESULT, or a negative code of Corvus's own.
    fn send_command(&self, command: i64, params: &[f64], timeout: f64, retries: i64, accept_in_progress: bool) -> i64;
    /// COMMAND_INT and its ACK.
    #[allow(clippy::too_many_arguments)]
    fn send_command_int(
        &self,
        command: i64,
        frame: i64,
        params: &[f64],
        x: i64,
        y: i64,
        z: f64,
        timeout: f64,
        retries: i64,
    ) -> i64;
    /// Sends one message under the link's send lock. `Ok(false)` is "the link
    /// is no longer this bridge's", `Err` a failed write.
    fn send(&self, msg: Message) -> Result<bool, String>;
    fn target(&self) -> (u8, u8);
    /// The bridge's command lock.
    fn lock(&self) -> &PriorityLock;
    fn now(&self) -> f64;
    fn sleep(&self, seconds: f64);
    fn stopping(&self) -> bool;
    fn snapshot(&self) -> FlightSnapshot;
    /// Has the store gone without news for `timeout` seconds?
    fn is_stale(&self, timeout: f64) -> bool;
    fn dialect(&self) -> Dialect;
    fn mav_type(&self) -> i64;
    /// The command for a mode the vehicle offers, by name.
    fn mode_command(&self, mode: &str) -> Option<ModeCommand>;
    /// Does the bridge know any modes at all?
    fn has_modes(&self) -> bool;
    /// Does this autopilot encode modes in a way Corvus does not know?
    fn modes_unsupported(&self) -> bool;
    fn home_alt_amsl(&self) -> Option<f64>;
    fn position_home_alt_amsl(&self) -> Option<f64>;
    fn request_home(&self);

    // The mission side, which the missions engine answers.
    /// The operator's points as `(lat, lon, alt_agl)`, or None with the error set.
    fn validate_fly_to_points(&self, points: &Json) -> Option<Vec<(f64, f64, f64)>>;
    /// Where a prepended takeoff item goes: the vehicle, else home, else the first point.
    fn takeoff_item_position(&self, first: (f64, f64, f64)) -> (f64, f64);
    #[allow(clippy::too_many_arguments)]
    fn build_mission_item_spec(
        &self,
        seq: i64,
        command: i64,
        lat: f64,
        lon: f64,
        alt: f64,
        p1: f64,
        p2: f64,
        p3: f64,
        p4: f64,
    ) -> MissionItem;
    /// Reserves ArduPilot's home slot at sequence 0; PX4's items pass through.
    fn with_mission_home_slot(&self, items: Vec<MissionItem>) -> Vec<MissionItem>;
    /// Uploads a mission; the MAV_MISSION_RESULT, or a negative code.
    fn upload_mission(&self, items: &[MissionItem]) -> i64;
    fn mission_ack_to_result(&self, ack: i64) -> i64;
    /// What is on the vehicle is no longer the Mission page's plan.
    fn note_vehicle_mission_cleared(&self);
}

fn accepted() -> i64 {
    constant_or("MAV_RESULT_ACCEPTED", 0)
}

/// The flight commands of one bridge.
#[derive(Debug, Default, Clone, Copy)]
pub struct FlightEngine;

type Point = (f64, f64, f64);

impl FlightEngine {
    pub fn new() -> FlightEngine {
        FlightEngine
    }

    fn command(&self, host: &dyn FlightHost, command: i64, params: &[f64], timeout: f64, retries: i64) -> i64 {
        host.send_command(command, params, timeout, retries, false)
    }

    // ------------------------------------------------------------ modes

    /// Change flight mode, ACK confirmed. A mode change is always a direct
    /// operator instruction and a single command: the priority tier, because
    /// reaching for LOITER or RTL on the selector is how a flight is taken back.
    pub fn set_mode(&self, host: &dyn FlightHost, mode: &str) -> bool {
        let _gate = host.lock().priority();
        host.set_error("");
        if !host.connection_ready() {
            return host.command_failure("Mode change", -2);
        }
        let Some(command) = host.mode_command(mode) else {
            let text = if host.modes_unsupported() {
                "Mode changes are not supported on this autopilot: Corvus does not know how it encodes a mode"
                    .to_string()
            } else {
                format!("Unknown or unsupported {} mode: {mode}", host.dialect().label())
            };
            host.log("error", &text);
            host.set_error(&text);
            return false;
        };
        let result = self.command(host, constant_or("MAV_CMD_DO_SET_MODE", 176), &command.params(), 3.0, 1);
        if result != accepted() {
            return host.command_failure("Mode change", result);
        }
        host.console("MODE", &format!("Mode accepted: {mode}"), "success");
        true
    }

    /// Did another thread's abort run since `mark`? Then the sequence stops
    /// with the reason rather than send its next step on top of it.
    pub fn sequence_interrupted(&self, host: &dyn FlightHost, mark: u64, action: &str) -> bool {
        if !host.lock().aborted_since(mark) {
            return false;
        }
        let text = format!("{action} stopped: another command took over");
        host.set_error(&text);
        host.console(&action.to_uppercase(), &text, "warning");
        true
    }

    /// Does fresh telemetry already say the vehicle is `armed` (or not)?
    pub fn reports_armed_state(&self, host: &dyn FlightHost, armed: bool) -> bool {
        let snap = host.snapshot();
        snap.connected && snap.armed == armed && !host.is_stale(2.0)
    }

    /// Watch the store until it reports `armed`, for up to `timeout` seconds.
    pub fn wait_for_armed_state(&self, host: &dyn FlightHost, armed: bool, timeout: f64) -> bool {
        let deadline = host.now() + timeout;
        while host.now() < deadline {
            let snap = host.snapshot();
            if snap.connected && snap.armed == armed {
                return true;
            }
            host.sleep(0.05);
        }
        false
    }

    /// Arm or disarm with ACK confirmation. Disarm is an abort and takes the
    /// priority tier; arming queues. The command is always sent, since
    /// telemetry lags the vehicle, and the wait for the vehicle's own report
    /// runs outside the lock.
    pub fn arm(&self, host: &dyn FlightHost, arm: bool) -> bool {
        let verdict = {
            let _gate = if arm {
                host.lock().lock()
            } else {
                host.lock().priority()
            };
            self.arm_command(host, arm)
        };
        match verdict {
            Some(v) => v,
            None => self.arm_confirm(host, arm),
        }
    }

    fn arm_command(&self, host: &dyn FlightHost, arm: bool) -> Option<bool> {
        let action = if arm { "Arm" } else { "Disarm" };
        host.set_error("");
        if !host.connection_ready() {
            return Some(host.command_failure(action, -2));
        }
        let nan = f64::NAN;
        let result = self.command(
            host,
            constant_or("MAV_CMD_COMPONENT_ARM_DISARM", 400),
            &[if arm { 1.0 } else { 0.0 }, 0.0, nan, nan, nan, nan, nan],
            3.0,
            2,
        );
        if result != accepted() {
            // ArduPilot answers FAILED to arming an armed vehicle; fresh
            // telemetry in the state asked for is not a failure.
            if result >= 0 && self.reports_armed_state(host, arm) {
                return Some(true);
            }
            return Some(host.command_failure(action, result));
        }
        None
    }

    fn arm_confirm(&self, host: &dyn FlightHost, arm: bool) -> bool {
        let action = if arm { "Arm" } else { "Disarm" };
        if !self.wait_for_armed_state(host, arm, ARMED_STATE_CONFIRM_S) {
            let text = format!(
                "{action} accepted by the vehicle, but no telemetry confirmation within {}s. Check the armed indicator",
                fixed(ARMED_STATE_CONFIRM_S, 0)
            );
            host.console("ARM", &text, "warning");
            host.store_warning(&text, "warning");
            return true;
        }
        let completed = if arm { "Armed" } else { "Disarmed" };
        host.console("ARM", &format!("{completed} successfully"), "success");
        true
    }

    /// Is the aircraft off the ground? Its landed state first, else height
    /// above home over the fallback.
    pub fn is_airborne(&self, host: &dyn FlightHost) -> bool {
        let snap = host.snapshot();
        if let Some(landed) = snap.landed_state {
            if AIRBORNE_LANDED_STATES.contains(&landed) {
                return true;
            }
            if landed == 1 {
                return false;
            }
        }
        snap.altitude_agl > AIRBORNE_FALLBACK_M
    }

    /// Home altitude plus `altitude_agl`, or None without a reference.
    pub fn takeoff_altitude_amsl(&self, host: &dyn FlightHost, altitude_agl: f64) -> Option<f64> {
        host.home_alt_amsl()
            .or_else(|| host.position_home_alt_amsl())
            .map(|reference| reference + altitude_agl)
    }

    /// Ask for home once and wait briefly for an altitude reference.
    pub fn await_takeoff_reference(&self, host: &dyn FlightHost, altitude_agl: f64) -> Option<f64> {
        host.request_home();
        let deadline = host.now() + TAKEOFF_REFERENCE_WAIT_S;
        while host.now() < deadline {
            if host.stopping() || !host.connection_ready() {
                return None;
            }
            if let Some(amsl) = self.takeoff_altitude_amsl(host, altitude_agl) {
                return Some(amsl);
            }
            host.sleep(0.05);
        }
        self.takeoff_altitude_amsl(host, altitude_agl)
    }

    /// Watch the store until the vehicle reports `mode`; sends nothing.
    /// ArduPilot refuses a guided takeoff outside GUIDED, and from the old
    /// mode if the command overtakes the mode change on the wire, so the
    /// takeoff waits for the aircraft's own word, not for the ACK.
    pub fn wait_for_mode(&self, host: &dyn FlightHost, mode: &str, timeout: f64) -> bool {
        let deadline = host.now() + timeout;
        while host.now() < deadline {
            if host.stopping() || !host.connection_ready() {
                return false;
            }
            if host.snapshot().mode == mode {
                return true;
            }
            host.sleep(0.05);
        }
        host.snapshot().mode == mode
    }

    /// Put the vehicle in the mode a guided command needs, if the stack needs
    /// one (ArduPilot's GUIDED); a vehicle already in it is left alone.
    pub fn enter_guided(&self, host: &dyn FlightHost, action: &str) -> bool {
        let wanted = host.dialect().guided_mode(host.mav_type());
        if wanted.is_empty() {
            return true;
        }
        if host.snapshot().mode == wanted {
            return true;
        }
        if !self.set_mode(host, wanted) {
            let existing = host.last_error();
            let mut text = format!("{action} needs {wanted} mode, which the vehicle refused");
            if !existing.is_empty() {
                text.push_str(&format!(": {existing}"));
            }
            host.set_error(&text);
            return false;
        }
        if !self.wait_for_mode(host, wanted, MODE_CONFIRM_S) {
            // The vehicle ACCEPTED the change; its heartbeat has not caught up.
            // The command that follows is ACK confirmed on its own.
            let text = format!(
                "{wanted} accepted but not yet confirmed in telemetry, continuing with {}",
                action.to_lowercase()
            );
            host.console("MODE", &text, "warning");
        }
        true
    }

    /// Switch to the mode that runs a stored mission (MISSION on PX4, AUTO on
    /// ArduPilot), from the dialect rather than hardcoded.
    pub fn enter_mission_mode(&self, host: &dyn FlightHost, action: &str) -> bool {
        let mode = host.dialect().mission_mode();
        if mode.is_empty() {
            host.set_error(&format!("{action}: Corvus does not know this autopilot's mission mode"));
            return false;
        }
        if host.mode_command(mode).is_none() && host.has_modes() {
            host.set_error(&format!("{action}: this vehicle does not offer a {mode} mode"));
            return false;
        }
        self.set_mode(host, mode)
    }

    // ---------------------------------------------------------- takeoff

    /// A guided takeoff to `altitude_agl` metres, in the stack's order: PX4
    /// takes the command (param7 AMSL) and is then armed; ArduPilot is put in
    /// GUIDED, armed, then commanded (param7 relative to home) and disarmed
    /// again if it refuses.
    pub fn takeoff(&self, host: &dyn FlightHost, altitude_agl: f64) -> bool {
        host.set_error("");
        let mav = host.mav_type();
        if is_ground_vehicle(mav) {
            host.set_error("Takeoff is not available on a rover. Arm it and drive.");
            return false;
        }
        if !altitude_agl.is_finite() {
            host.set_error("Takeoff altitude must be finite");
            return false;
        }
        if !(TAKEOFF_ALTITUDE_MIN_M..=TAKEOFF_ALTITUDE_MAX_M).contains(&altitude_agl) {
            host.set_error(&format!(
                "Takeoff altitude must be between {} and {} m AGL",
                fixed(TAKEOFF_ALTITUDE_MIN_M, 0),
                fixed(TAKEOFF_ALTITUDE_MAX_M, 0)
            ));
            return false;
        }
        if !host.connection_ready() {
            return host.command_failure("Takeoff", -2);
        }
        let mark = host.lock().abort_mark();
        let nan = f64::NAN;
        let plan = host.dialect().takeoff_plan(mav);
        let (takeoff_alt, note) = if plan.altitude_frame == "relative" {
            (
                altitude_agl,
                format!("Requesting {} m above home", fixed(altitude_agl, 0)),
            )
        } else {
            let mut amsl = self.takeoff_altitude_amsl(host, altitude_agl);
            if amsl.is_none() {
                amsl = self.await_takeoff_reference(host, altitude_agl);
            }
            match amsl {
                None => {
                    // NaN is the protocol's "use your own default": the vehicle
                    // takes off to its configured altitude rather than Corvus
                    // refusing a flight over a conversion.
                    let text = format!(
                        "No altitude reference yet (no HOME_POSITION or global position), so asking the vehicle to take off to its own configured altitude instead of {} m",
                        fixed(altitude_agl, 0)
                    );
                    host.log("warning", &text);
                    host.console("TAKEOFF", &text, "warning");
                    host.store_warning(&text, "warning");
                    (nan, String::new())
                }
                Some(amsl) => (
                    amsl,
                    format!(
                        "Requesting {} m AGL ({} m AMSL)",
                        fixed(altitude_agl, 0),
                        fixed(amsl, 1)
                    ),
                ),
            }
        };
        if plan.order == "mode_arm_takeoff" && !self.enter_guided(host, "Takeoff") {
            return false;
        }
        let params = [plan.min_pitch_deg, nan, nan, nan, nan, nan, takeoff_alt];
        if self.sequence_interrupted(host, mark, "Takeoff") {
            return false;
        }
        if !host.connection_ready() {
            return host.command_failure("Takeoff", -2);
        }
        if !note.is_empty() {
            host.console("TAKEOFF", &note, "info");
        }
        let takeoff = constant_or("MAV_CMD_NAV_TAKEOFF", 22);
        if plan.order == "takeoff_then_arm" {
            let result = {
                let _gate = host.lock().lock();
                self.command(host, takeoff, &params, 5.0, 1)
            };
            if result != accepted() {
                return host.command_failure("Takeoff", result);
            }
            host.console("TAKEOFF", "Takeoff target accepted; arming …", "info");
            if self.sequence_interrupted(host, mark, "Takeoff") {
                return false;
            }
            if !self.arm(host, true) {
                return false;
            }
        } else {
            host.console("TAKEOFF", "Arming before takeoff …", "info");
            if !self.arm(host, true) {
                return false;
            }
            if self.sequence_interrupted(host, mark, "Takeoff") {
                return false;
            }
            let result = {
                let _gate = host.lock().lock();
                self.command(host, takeoff, &params, 5.0, 1)
            };
            if result != accepted() {
                let failure = format!("Takeoff failed: {}", result_text(result));
                host.console("TAKEOFF", "Takeoff refused after arming, disarming again", "warning");
                self.arm(host, false);
                host.set_error(&failure);
                host.console("TAKEOFF", &failure, "error");
                host.store_warning(&failure, "critical");
                return false;
            }
        }
        host.console("TAKEOFF", "Takeoff accepted and vehicle armed", "success");
        true
    }

    /// Land where the vehicle is. One of the three commands an operator uses
    /// to end a flight that is going wrong: the priority tier.
    pub fn land(&self, host: &dyn FlightHost) -> bool {
        let _gate = host.lock().priority();
        host.set_error("");
        if is_ground_vehicle(host.mav_type()) {
            host.set_error("Land is not available on a rover. Disarm it to stop.");
            return false;
        }
        let nan = f64::NAN;
        let result = self.command(
            host,
            constant_or("MAV_CMD_NAV_LAND", 21),
            &[0.0, 0.0, nan, nan, nan, nan, nan],
            5.0,
            1,
        );
        if result != accepted() {
            return host.command_failure("Land", result);
        }
        host.console("LAND", "Land accepted", "success");
        true
    }

    /// Return to launch; the priority tier, as `land`.
    pub fn rtl(&self, host: &dyn FlightHost) -> bool {
        let _gate = host.lock().priority();
        host.set_error("");
        let result = self.command(
            host,
            constant_or("MAV_CMD_NAV_RETURN_TO_LAUNCH", 20),
            &[f64::NAN; 7],
            5.0,
            1,
        );
        if result != accepted() {
            return host.command_failure("RTL", result);
        }
        host.console("RTL", "RTL accepted", "success");
        true
    }

    /// One coordinate pair range checked, or None with the error set.
    pub fn validate_lat_lon(&self, host: &dyn FlightHost, lat: f64, lon: f64, action: &str) -> Option<(f64, f64)> {
        if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
            host.set_error(&format!("{action} failed: lat out of range"));
            return None;
        }
        if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
            host.set_error(&format!("{action} failed: lon out of range"));
            return None;
        }
        Some((lat, lon))
    }

    /// Move home to (`lat`, `lon`) at its current altitude, a purely horizontal
    /// move; refused rather than sent with a made up altitude. Allowed while
    /// armed: it is how RTL is redirected in flight.
    pub fn set_home(&self, host: &dyn FlightHost, lat: f64, lon: f64) -> bool {
        host.set_error("");
        let Some((lat_f, lon_f)) = self.validate_lat_lon(host, lat, lon, "Set home") else {
            return false;
        };
        let mut alt = self.takeoff_altitude_amsl(host, 0.0);
        if alt.is_none() {
            alt = self.await_takeoff_reference(host, 0.0);
        }
        let _gate = host.lock().lock();
        let Some(alt) = alt else {
            let text = "Set home failed: no home or global altitude reference";
            host.set_error("no home or global altitude reference");
            host.console("SETHOME", text, "error");
            host.store_warning(text, "critical");
            return false;
        };
        let nan = f64::NAN;
        let result = host.send_command_int(
            constant_or("MAV_CMD_DO_SET_HOME", 179),
            constant_or("MAV_FRAME_GLOBAL", 0),
            &[0.0, nan, nan, nan],
            e7(lat_f),
            e7(lon_f),
            alt,
            5.0,
            1,
        );
        if result != accepted() {
            return host.command_failure("Set home", result);
        }
        host.request_home();
        host.console(
            "SETHOME",
            &format!("Home set to {}, {}", fixed(lat_f, 7), fixed(lon_f, 7)),
            "success",
        );
        true
    }

    // ---------------------------------------------------------- plugins

    /// Stop and stay, in the stack's hold mode; a plain mode change.
    pub fn hold(&self, host: &dyn FlightHost) -> bool {
        let mode = host.dialect().hold_mode(host.mav_type());
        if mode.is_empty() {
            host.set_error("Hold is not available on this vehicle");
            return false;
        }
        self.set_mode(host, mode)
    }

    /// One MAV_CMD from the dialect's plugin table, ACK confirmed. `params`
    /// are p1 to p7 (p1 to p4 for a command that names a place), NaN where
    /// unset. `position` is `(lat, lon, alt, frame)`, required for and only
    /// for the position commands, which travel as COMMAND_INT; a "relative"
    /// altitude is converted to AMSL where the stack reads it so.
    pub fn plugin_command(
        &self,
        host: &dyn FlightHost,
        command: i64,
        params: Option<&[f64]>,
        position: Option<(f64, f64, f64, &str)>,
    ) -> bool {
        host.set_error("");
        let mav = host.mav_type();
        let dialect = host.dialect();
        let Some((_, name)) = dialect.plugin_commands(mav).into_iter().find(|(id, _)| *id == command) else {
            let label = enum_entry_name("MAV_CMD", command)
                .map(|n| n.strip_prefix("MAV_CMD_").unwrap_or(n).to_string())
                .unwrap_or_else(|| command.to_string());
            host.set_error(&format!("Command {label} is not available on this autopilot"));
            return false;
        };
        let label = format!("Command {name}");
        let positional = plugin_position_commands().contains(&command);
        if positional && position.is_none() {
            host.set_error(&format!("{label} needs lat and lon"));
            return false;
        }
        if !positional && position.is_some() {
            host.set_error(&format!("{label} takes no position"));
            return false;
        }
        let nan = f64::NAN;
        let mut values: Vec<f64> = params.map(<[f64]>::to_vec).unwrap_or_default();
        let result = if let Some((lat, lon, alt, frame)) = position {
            values.resize(values.len().max(4), nan);
            values.truncate(4);
            let Some((lat_f, lon_f)) = self.validate_lat_lon(host, lat, lon, &label) else {
                return false;
            };
            let mut altitude = alt;
            let mut wire_frame = constant_or("MAV_FRAME_GLOBAL", 0);
            if frame == "relative" {
                if dialect.reposition_altitude_frame(mav) == "amsl" {
                    let mut amsl = self.takeoff_altitude_amsl(host, altitude);
                    if amsl.is_none() {
                        amsl = self.await_takeoff_reference(host, altitude);
                    }
                    let Some(amsl) = amsl else {
                        host.set_error(&format!("{label} failed: no home altitude to measure the height from"));
                        return false;
                    };
                    altitude = amsl;
                } else {
                    wire_frame = constant_or("MAV_FRAME_GLOBAL_RELATIVE_ALT", 3);
                }
            }
            host.send_command_int(command, wire_frame, &values, e7(lat_f), e7(lon_f), altitude, 5.0, 1)
        } else {
            values.resize(values.len().max(7), nan);
            values.truncate(7);
            host.send_command(command, &values, 5.0, 1, true)
        };
        if result != accepted() && result != constant_or("MAV_RESULT_IN_PROGRESS", 5) {
            return host.command_failure(&label, result);
        }
        host.console("COMMAND", &format!("{name} accepted"), "success");
        true
    }

    /// One velocity setpoint as SET_POSITION_TARGET_LOCAL_NED, no ACK: a
    /// stream whose end the vehicle's own timeout catches. In metres per
    /// second: north, east, down, or with `body` forward, right, down.
    pub fn velocity_setpoint(
        &self,
        host: &dyn FlightHost,
        vx: f64,
        vy: f64,
        vz: f64,
        yaw_rate_deg: Option<f64>,
        body: bool,
    ) -> bool {
        host.set_error("");
        if host.dialect().velocity_mode(host.mav_type()).is_empty() {
            host.set_error("Velocity control is not available on this vehicle");
            return false;
        }
        let finite = [vx, vy, vz].iter().all(|v| v.is_finite()) && yaw_rate_deg.is_none_or(f64::is_finite);
        if !finite {
            host.set_error("Velocity control failed: not a finite number");
            return false;
        }
        if [vx, vy, vz].iter().any(|v| v.abs() > VELOCITY_MAX_MS)
            || yaw_rate_deg.is_some_and(|y| y.abs() > YAW_RATE_MAX_DEG_S)
        {
            host.set_error("Velocity control failed: out of range");
            return false;
        }
        let lost = format!("Velocity control failed: {}", result_text(-2));
        if !host.connection_ready() {
            host.set_error(&lost);
            return false;
        }
        let mut mask = VELOCITY_TYPE_MASK;
        if yaw_rate_deg.is_none() {
            mask |= YAW_RATE_IGNORE;
        }
        let frame = if body {
            constant_or("MAV_FRAME_BODY_NED", 8)
        } else {
            constant_or("MAV_FRAME_LOCAL_NED", 1)
        };
        let int = |v: i64| Value::Int(v as i128);
        let msg = targeted("SET_POSITION_TARGET_LOCAL_NED", host.target())
            .with("time_boot_ms", int(0))
            .with("coordinate_frame", int(frame))
            .with("type_mask", int(mask))
            .with("x", Value::Float(0.0))
            .with("y", Value::Float(0.0))
            .with("z", Value::Float(0.0))
            .with("vx", Value::Float(vx))
            .with("vy", Value::Float(vy))
            .with("vz", Value::Float(vz))
            .with("afx", Value::Float(0.0))
            .with("afy", Value::Float(0.0))
            .with("afz", Value::Float(0.0))
            .with("yaw", Value::Float(0.0))
            .with("yaw_rate", Value::Float(yaw_rate_deg.map_or(0.0, f64::to_radians)));
        match host.send(msg) {
            Ok(true) => true,
            Ok(false) => {
                host.set_error(&lost);
                false
            }
            Err(e) => {
                host.log("error", &format!("velocity setpoint send failed: {e}"));
                host.set_error(&format!("Velocity control send failed: {e}"));
                false
            }
        }
    }

    // ---------------------------------------------------------- missions

    /// MISSION_START, the mission mode and arming in the stack's order. PX4's
    /// MISSION_START switches and arms by itself and is then only watched;
    /// ArduPilot is put in its arming mode and armed first, and disarmed
    /// again if the start is refused after that.
    pub fn start_uploaded_mission(&self, host: &dyn FlightHost, action: &str, count: i64, mark: Option<u64>) -> bool {
        let mark = mark.unwrap_or_else(|| host.lock().abort_mark());
        let nan = f64::NAN;
        let dialect = host.dialect();
        let mav = host.mav_type();
        let (first, last) = dialect.mission_start_params(count);
        let arm_mode = dialect.mission_arm_mode(mav);
        let mut armed_here = false;
        if !arm_mode.is_empty() && !host.snapshot().armed {
            let entered = if arm_mode == dialect.mission_mode() {
                self.enter_mission_mode(host, action)
            } else {
                self.enter_guided(host, action)
            };
            if !entered {
                return false;
            }
            if self.sequence_interrupted(host, mark, action) {
                return false;
            }
            if !self.arm(host, true) {
                return false;
            }
            armed_here = true;
        }
        if self.sequence_interrupted(host, mark, action) {
            return false;
        }
        let result = {
            let _gate = host.lock().lock();
            self.command(
                host,
                constant_or("MAV_CMD_MISSION_START", 300),
                &[first, last, nan, nan, nan, nan, nan],
                5.0,
                1,
            )
        };
        if result != accepted() {
            if armed_here {
                host.console(
                    "MISSION",
                    "Mission start refused after arming, disarming again",
                    "warning",
                );
                self.arm(host, false);
            }
            return host.command_failure(action, result);
        }
        if !arm_mode.is_empty() {
            // ArduPilot's MISSION_START is itself the switch to AUTO, and the
            // vehicle was armed above (or already was).
            return true;
        }
        // PX4's Commander accepts MISSION_START only after it has switched to
        // AUTO.MISSION and armed, so the ACK already says both. Telemetry is
        // watched and nothing more is sent: a second mode change on a short
        // hop that had already finished restarted the mission.
        let mode = dialect.mission_mode();
        if !mode.is_empty() && !self.wait_for_mode(host, mode, MODE_CONFIRM_S) {
            let reported = host.snapshot().mode;
            if reported != mode {
                let shown = if reported.is_empty() {
                    "no mode"
                } else {
                    reported.as_str()
                };
                host.console(
                    "MISSION",
                    &format!("{action}: accepted, but the vehicle reports {shown} rather than {mode}"),
                    "warning",
                );
            }
        }
        true
    }

    /// One point for an airborne vehicle as MAV_CMD_DO_REPOSITION; None when
    /// the vehicle cannot take it, so the caller falls back to a mission.
    pub fn reposition_to(&self, host: &dyn FlightHost, lat: f64, lon: f64, alt_agl: f64) -> Option<bool> {
        let mav = host.mav_type();
        let (frame, altitude) = if host.dialect().reposition_altitude_frame(mav) == "relative" {
            (constant_or("MAV_FRAME_GLOBAL_RELATIVE_ALT", 3), alt_agl)
        } else {
            let amsl = self.takeoff_altitude_amsl(host, alt_agl)?;
            (constant_or("MAV_FRAME_GLOBAL", 0), amsl)
        };
        let ground = is_ground_vehicle(mav);
        let action = if ground { "Drive to point" } else { "Fly to point" };
        if !self.enter_guided(host, action) {
            return Some(false);
        }
        let result = host.send_command_int(
            constant_or("MAV_CMD_DO_REPOSITION", 192),
            frame,
            &[-1.0, DO_REPOSITION_CHANGE_MODE, 0.0, f64::NAN],
            e7(lat),
            e7(lon),
            altitude,
            5.0,
            1,
        );
        if result == constant_or("MAV_RESULT_UNSUPPORTED", 3) {
            return None;
        }
        if result != accepted() {
            return Some(host.command_failure(action, result));
        }
        if ground {
            host.console("GOTOPOINTS", "Driving to point", "success");
        } else {
            host.console(
                "GOTOPOINTS",
                &format!("Flying to point at {} m AGL", fixed(alt_agl, 0)),
                "success",
            );
        }
        Some(true)
    }

    /// Is one point best sent as a reposition rather than a mission? An
    /// aircraft in the air; a rover only when armed and the stack reads the
    /// altitude as relative.
    pub fn can_reposition(&self, host: &dyn FlightHost, ground: bool, on_ground: bool) -> bool {
        if !ground {
            return !on_ground;
        }
        let relative = host.dialect().reposition_altitude_frame(host.mav_type()) == "relative";
        relative && host.snapshot().armed
    }

    /// Fly to the given points in order by uploading a mission and starting it
    /// in the stack's order; one point for an airborne vehicle is a
    /// reposition, which leaves the stored mission alone.
    pub fn fly_to_points(&self, host: &dyn FlightHost, points: &Json) -> bool {
        host.set_error("");
        let mark = host.lock().abort_mark();
        let ground = is_ground_vehicle(host.mav_type());
        let label = if ground { "Drive to points" } else { "Fly to points" };
        let Some(cleaned) = host.validate_fly_to_points(points) else {
            return false;
        };
        if !host.connection_ready() {
            return host.command_failure(label, -2);
        }
        let on_ground = !ground && !self.is_airborne(host);
        if cleaned.len() == 1 && self.can_reposition(host, ground, on_ground) {
            let (lat, lon, alt) = cleaned[0];
            if let Some(handled) = self.reposition_to(host, lat, lon, alt) {
                return handled;
            }
        }
        let nan = f64::NAN;
        let mut items: Vec<MissionItem> = Vec::new();
        let mut seq = 0;
        if on_ground {
            let first: Point = cleaned[0];
            let (lat, lon) = host.takeoff_item_position(first);
            items.push(host.build_mission_item_spec(
                seq,
                constant_or("MAV_CMD_NAV_TAKEOFF", 22),
                lat,
                lon,
                first.2,
                nan,
                nan,
                nan,
                nan,
            ));
            seq += 1;
        }
        for (lat, lon, alt) in &cleaned {
            items.push(host.build_mission_item_spec(
                seq,
                constant_or("MAV_CMD_NAV_WAYPOINT", 16),
                *lat,
                *lon,
                *alt,
                0.0,
                nan,
                nan,
                nan,
            ));
            seq += 1;
        }
        let planned = items.len() as i64;
        let items = host.with_mission_home_slot(items);
        {
            // The upload is a handshake and holds the lock; it gives way to an
            // abort by cancelling itself. Starting is a sequence, and does not.
            let _gate = host.lock().lock();
            if !host.connection_ready() {
                return host.command_failure(label, -2);
            }
            host.console("GOTOPOINTS", &format!("Uploading {planned} mission item(s) …"), "info");
            let ack = host.upload_mission(&items);
            if ack != constant_or("MAV_MISSION_ACCEPTED", 0) {
                let result = if ack < 0 { ack } else { host.mission_ack_to_result(ack) };
                return host.command_failure(label, result);
            }
            host.note_vehicle_mission_cleared();
        }
        host.console("GOTOPOINTS", "Mission accepted; starting …", "info");
        if !self.start_uploaded_mission(host, label, planned, Some(mark)) {
            return false;
        }
        host.console("GOTOPOINTS", &format!("{label} started"), "success");
        true
    }

    /// Run the mission already on the vehicle; `count` is how many items were uploaded.
    pub fn start_mission(&self, host: &dyn FlightHost, count: i64) -> bool {
        host.set_error("");
        let mark = host.lock().abort_mark();
        if count <= 0 {
            host.set_error("mission item count must be positive");
            return false;
        }
        if !host.connection_ready() {
            return host.command_failure("Mission start", -2);
        }
        if !self.start_uploaded_mission(host, "Mission start", count, Some(mark)) {
            return false;
        }
        host.console("MISSION", "Mission started", "success");
        true
    }
}
