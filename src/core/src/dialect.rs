//! Flight stack dialects: the one place that knows PX4 from ArduPilot
//! (`autopilot.py`).
//!
//! The mode decoder, the takeoff order, the calibration commands, the shell,
//! the log format and the capability flags differ between stacks, and every
//! difference fails quietly when it is wrong: a mode shown as `MODE_5`, a
//! 10 m takeoff read as 10 m above sea level, a compass calibration that is
//! acknowledged and never starts. The bridge, the HTTP layer and the
//! interface ask this module instead of branching on the autopilot id, so
//! there is one table to correct when a stack changes its mind.
//!
//! Verified against PX4 v1.16 to v1.18 and ArduPilot 4.3 to 4.6 (Copter,
//! Plane, Rover and Boat, Sub, Tracker, Blimp). An autopilot that is neither
//! is `Generic`, which sends only what `common.xml` guarantees. Held to the
//! Python module by `python/tools/dump_core_cases.py dialect`, which calls
//! both on a matrix of inputs.

use corvus_link::plain::defs::constant_or;
pub use corvus_link::plain::param_value::{
    decode_value as decode_param_value, encode_value as encode_param_value, is_integer_type as param_type_is_integer,
    values_match as param_values_match, PARAM_ENCODING_BYTEWISE, PARAM_ENCODING_C_CAST,
};
use serde_json::{json, Value};

use crate::json::Map;

const MAV_AUTOPILOT_ARDUPILOTMEGA: i64 = 3;
const MAV_AUTOPILOT_PX4: i64 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stack {
    Px4,
    ArduPilot,
    Generic,
    /// A generic heartbeat that the link says comes from Betaflight.
    Betaflight,
}

impl Stack {
    pub fn as_str(self) -> &'static str {
        match self {
            Stack::Px4 => "px4",
            Stack::ArduPilot => "ardupilot",
            Stack::Generic => "generic",
            Stack::Betaflight => "betaflight",
        }
    }

    pub fn from_name(name: &str) -> Stack {
        match name {
            "px4" => Stack::Px4,
            "ardupilot" => Stack::ArduPilot,
            "betaflight" => Stack::Betaflight,
            _ => Stack::Generic,
        }
    }
}

/// The dialect that speaks a MAV_AUTOPILOT id. Anything unrecognised is
/// generic, never PX4: guessing PX4 is how a ground station sends packed
/// mode words to firmware that reads them as something else.
pub fn stack_for_autopilot_id(id: i64) -> Stack {
    match id {
        MAV_AUTOPILOT_PX4 => Stack::Px4,
        MAV_AUTOPILOT_ARDUPILOTMEGA => Stack::ArduPilot,
        _ => Stack::Generic,
    }
}

/// The dialect for the name an autopilot goes by.
pub fn stack_for_autopilot_name(name: &str) -> Stack {
    match name.trim().to_lowercase().as_str() {
        "px4" => Stack::Px4,
        "ardupilotmega" | "ardupilot" | "apm" | "arducopter" | "arduplane" => Stack::ArduPilot,
        "betaflight" => Stack::Betaflight,
        _ => Stack::Generic,
    }
}

/// The stack a link's hint (`fc=betaflight`) names, when the heartbeat does
/// not name one. Only ever fills in a stack that would be generic.
pub fn stack_for_hint(hint: &str) -> Option<Stack> {
    match hint.trim().to_lowercase().as_str() {
        "betaflight" => Some(Stack::Betaflight),
        _ => None,
    }
}

type Table = &'static [(i64, &'static str)];

const PX4_MAIN_MODE: Table = &[
    (1, "MANUAL"),
    (2, "ALTCTL"),
    (3, "POSCTL"),
    (4, "AUTO"),
    (5, "ACRO"),
    (6, "OFFBOARD"),
    (7, "STABILIZED"),
    (8, "RATTITUDE"),
    (10, "TERMINATION"),
    (11, "ALTITUDE_CRUISE"),
];
const PX4_POSCTL_SUBMODE: Table = &[(0, "POSCTL"), (1, "ORBIT"), (2, "POSITION_SLOW")];
const PX4_AUTO_SUBMODE: Table = &[
    (1, "READY"),
    (2, "TAKEOFF"),
    (3, "LOITER"),
    (4, "MISSION"),
    (5, "RTL"),
    (6, "LAND"),
    (7, "RTGS"),
    (8, "FOLLOWME"),
    (9, "PRECLAND"),
    (10, "VTOL_TAKEOFF"),
    (11, "EXTERNAL1"),
    (12, "EXTERNAL2"),
    (13, "EXTERNAL3"),
    (14, "EXTERNAL4"),
    (15, "EXTERNAL5"),
    (16, "EXTERNAL6"),
    (17, "EXTERNAL7"),
    (18, "EXTERNAL8"),
    (19, "GUIDED_COURSE"),
];
const PX4_MODE_VALUES: &[(&str, (i64, i64, i64))] = &[
    ("MANUAL", (81, 1, 0)),
    ("ALTCTL", (81, 2, 0)),
    ("POSCTL", (81, 3, 0)),
    ("POSITION_SLOW", (81, 3, 2)),
    ("ALTITUDE_CRUISE", (81, 11, 0)),
    ("STABILIZED", (81, 7, 0)),
    ("ACRO", (65, 5, 0)),
    ("LOITER", (29, 4, 3)),
    ("MISSION", (29, 4, 4)),
    ("RTL", (29, 4, 5)),
    ("LAND", (29, 4, 6)),
    ("TAKEOFF", (29, 4, 2)),
    ("OFFBOARD", (29, 6, 0)),
    ("FOLLOWME", (29, 4, 8)),
];
const PX4_AVAILABLE_MODES: &[&str] = &[
    "MANUAL",
    "ALTCTL",
    "ALTITUDE_CRUISE",
    "POSCTL",
    "POSITION_SLOW",
    "STABILIZED",
    "ACRO",
    "LOITER",
    "MISSION",
    "RTL",
    "LAND",
    "TAKEOFF",
    "OFFBOARD",
    "FOLLOWME",
];
const PX4_RETIRED_MODES: &[&str] = &["RTGS", "RATTITUDE"];
const PX4_MODE_SINCE: &[(&str, (i64, i64))] = &[("POSITION_SLOW", (1, 15)), ("ALTITUDE_CRUISE", (1, 17))];
const PX4_MODE_LABELS: &[(&str, &str)] = &[
    ("ALTCTL", "ALTITUDE"),
    ("POSCTL", "POSITION"),
    ("LOITER", "HOLD"),
    ("RTL", "RETURN"),
    ("FOLLOWME", "FOLLOW ME"),
    ("PRECLAND", "PRECISION LAND"),
    ("POSITION_SLOW", "POSITION SLOW"),
    ("ALTITUDE_CRUISE", "ALTITUDE CRUISE"),
    ("ORBIT", "ORBIT"),
    ("TERMINATION", "TERMINATION"),
    ("GUIDED_COURSE", "GUIDED COURSE"),
];

/// Will a PX4 of this version switch to `name` on DO_SET_MODE? `firmware` is
/// `(major, minor, ...)` from AUTOPILOT_VERSION, `None` before it arrived: an
/// unknown version gets only the modes every supported release accepts.
pub fn px4_mode_supported(name: &str, firmware: Option<&[i64]>) -> bool {
    if PX4_RETIRED_MODES.contains(&name) {
        return false;
    }
    let Some((_, since)) = PX4_MODE_SINCE.iter().find(|(n, _)| *n == name) else {
        return true;
    };
    match firmware {
        Some(f) if f.len() >= 2 => (f[0], f[1]) >= *since,
        // A tuple shorter than two compares as the Python slice does.
        Some(f) => match f {
            [] => false,
            [major] => (*major, i64::MIN) >= *since,
            _ => false,
        },
        None => false,
    }
}

fn plain_mode_label(name: &str) -> String {
    name.replace('_', " ")
}

const AP_COPTER_MODES: Table = &[
    (0, "STABILIZE"),
    (1, "ACRO"),
    (2, "ALT_HOLD"),
    (3, "AUTO"),
    (4, "GUIDED"),
    (5, "LOITER"),
    (6, "RTL"),
    (7, "CIRCLE"),
    (8, "POSITION"),
    (9, "LAND"),
    (10, "OF_LOITER"),
    (11, "DRIFT"),
    (13, "SPORT"),
    (14, "FLIP"),
    (15, "AUTOTUNE"),
    (16, "POSHOLD"),
    (17, "BRAKE"),
    (18, "THROW"),
    (19, "AVOID_ADSB"),
    (20, "GUIDED_NOGPS"),
    (21, "SMART_RTL"),
    (22, "FLOWHOLD"),
    (23, "FOLLOW"),
    (24, "ZIGZAG"),
    (25, "SYSTEMID"),
    (26, "AUTOROTATE"),
    (27, "AUTO_RTL"),
    (28, "TURTLE"),
];
const AP_PLANE_MODES: Table = &[
    (0, "MANUAL"),
    (1, "CIRCLE"),
    (2, "STABILIZE"),
    (3, "TRAINING"),
    (4, "ACRO"),
    (5, "FBWA"),
    (6, "FBWB"),
    (7, "CRUISE"),
    (8, "AUTOTUNE"),
    (10, "AUTO"),
    (11, "RTL"),
    (12, "LOITER"),
    (13, "TAKEOFF"),
    (14, "AVOID_ADSB"),
    (15, "GUIDED"),
    (16, "INITIALISING"),
    (17, "QSTABILIZE"),
    (18, "QHOVER"),
    (19, "QLOITER"),
    (20, "QLAND"),
    (21, "QRTL"),
    (22, "QAUTOTUNE"),
    (23, "QACRO"),
    (24, "THERMAL"),
    (25, "LOITER_ALT_QLAND"),
    (26, "AUTOLAND"),
];
const AP_ROVER_MODES: Table = &[
    (0, "MANUAL"),
    (1, "ACRO"),
    (2, "LEARNING"),
    (3, "STEERING"),
    (4, "HOLD"),
    (5, "LOITER"),
    (6, "FOLLOW"),
    (7, "SIMPLE"),
    (8, "DOCK"),
    (9, "CIRCLE"),
    (10, "AUTO"),
    (11, "RTL"),
    (12, "SMART_RTL"),
    (15, "GUIDED"),
    (16, "INITIALISING"),
];
const AP_SUB_MODES: Table = &[
    (0, "STABILIZE"),
    (1, "ACRO"),
    (2, "ALT_HOLD"),
    (3, "AUTO"),
    (4, "GUIDED"),
    (5, "LOITER"),
    (7, "CIRCLE"),
    (9, "SURFACE"),
    (16, "POSHOLD"),
    (19, "MANUAL"),
    (20, "MOTOR_DETECT"),
];
const AP_TRACKER_MODES: Table = &[
    (0, "MANUAL"),
    (1, "STOP"),
    (2, "SCAN"),
    (3, "SERVO_TEST"),
    (4, "GUIDED"),
    (10, "AUTO"),
    (16, "INITIALISING"),
];
const AP_BLIMP_MODES: Table = &[(0, "LAND"), (1, "MANUAL"), (2, "VELOCITY"), (3, "LOITER"), (4, "RTL")];

const COPTER_TYPES: [i64; 8] = [2, 3, 4, 13, 14, 15, 29, 35];
const PLANE_TYPES: [i64; 9] = [1, 19, 20, 21, 22, 23, 24, 25, 28];
const ROVER_TYPES: [i64; 2] = [10, 11];

/// The ArduPilot mode table of a MAV_TYPE; a type nobody listed gets the
/// copter's (what the Python module does after it finds no live table).
fn ap_table(mav_type: i64) -> Table {
    if COPTER_TYPES.contains(&mav_type) {
        AP_COPTER_MODES
    } else if PLANE_TYPES.contains(&mav_type) {
        AP_PLANE_MODES
    } else if ROVER_TYPES.contains(&mav_type) {
        AP_ROVER_MODES
    } else if mav_type == 12 {
        AP_SUB_MODES
    } else if mav_type == 5 {
        AP_TRACKER_MODES
    } else if mav_type == 7 {
        AP_BLIMP_MODES
    } else {
        AP_COPTER_MODES
    }
}

fn ap_names(mav_type: i64) -> Vec<&'static str> {
    ap_table(mav_type).iter().map(|(_, n)| *n).collect()
}

const AP_RETIRED_MODES: &[&str] = &["POSITION", "OF_LOITER", "LEARNING", "INITIALISING"];
const AP_GUIDED_MODE: &str = "GUIDED";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VehicleClass {
    Copter,
    Plane,
    Rover,
    Sub,
    Other,
}

impl VehicleClass {
    pub fn as_str(self) -> &'static str {
        match self {
            VehicleClass::Copter => "copter",
            VehicleClass::Plane => "plane",
            VehicleClass::Rover => "rover",
            VehicleClass::Sub => "sub",
            VehicleClass::Other => "other",
        }
    }
}

/// MAV_TYPE to which firmware family's parameters the vehicle carries.
pub fn vehicle_class(mav_type: i64) -> VehicleClass {
    if COPTER_TYPES.contains(&mav_type) {
        VehicleClass::Copter
    } else if PLANE_TYPES.contains(&mav_type) {
        VehicleClass::Plane
    } else if ROVER_TYPES.contains(&mav_type) {
        VehicleClass::Rover
    } else if mav_type == 12 {
        VehicleClass::Sub
    } else {
        VehicleClass::Other
    }
}

/// Does the vehicle drive or sail instead of fly? Everything unknown counts as
/// an aircraft: the app is tuned for them.
pub fn is_ground_vehicle(mav_type: i64) -> bool {
    vehicle_class(mav_type) == VehicleClass::Rover
}

/// One DO_SET_MODE payload in the shape the wire wants. PX4 packs `main_mode`
/// and `sub_mode`; ArduPilot sends a flat `custom_mode` and ignores the sub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeCommand {
    pub base_mode: i64,
    pub custom_mode: i64,
    pub custom_sub: i64,
}

impl ModeCommand {
    pub fn params(&self) -> [f64; 7] {
        let nan = f64::NAN;
        [
            self.base_mode as f64,
            self.custom_mode as f64,
            self.custom_sub as f64,
            nan,
            nan,
            nan,
            nan,
        ]
    }
}

/// A command to send, or the reason this stack cannot run it. `unsupported`
/// is the sentence for the operator: a page that says why a button is missing
/// is worth more than one that hides it.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandPlan {
    pub command: i64,
    pub params: Vec<f64>,
    pub unsupported: String,
}

impl CommandPlan {
    fn new(command: i64, params: &[f64]) -> CommandPlan {
        CommandPlan {
            command,
            params: params.to_vec(),
            unsupported: String::new(),
        }
    }

    fn unsupported(text: &str) -> CommandPlan {
        CommandPlan {
            command: 0,
            params: Vec::new(),
            unsupported: text.to_string(),
        }
    }

    pub fn ok(&self) -> bool {
        self.unsupported.is_empty()
    }
}

/// A STATUSTEXT that reports a change of the vehicle's arming or motor state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleEvent {
    /// `kill`, `unkill` or `disarm`.
    pub kind: String,
    pub level: String,
    /// The line for the board and the toast.
    pub message: String,
}

fn event(kind: &str, level: &str, message: &str) -> VehicleEvent {
    VehicleEvent {
        kind: kind.into(),
        level: level.into(),
        message: message.into(),
    }
}

fn px4_killed() -> VehicleEvent {
    event("kill", "critical", "Kill switch engaged. Motors stopped")
}

fn px4_released() -> VehicleEvent {
    event("unkill", "info", "Kill switch released")
}

fn ap_stopped() -> VehicleEvent {
    event("kill", "critical", "Emergency stop engaged. Motors stopped")
}

fn ap_released() -> VehicleEvent {
    event("unkill", "info", "Emergency stop released")
}

const PX4_DISARM_REASONS: &[(&str, (&str, &str))] = &[
    ("kill-switch", ("warning", "the kill switch")),
    ("lockdown", ("warning", "lockdown")),
    ("failure detector", ("warning", "the failure detector")),
    ("failsafe", ("warning", "a failsafe")),
    ("auto preflight disarming", ("info", "auto disarm (no takeoff)")),
    ("landing", ("info", "auto disarm after landing")),
    ("external command", ("info", "command")),
];

/// `^...$` of Python's `re`: `$` also matches before one trailing newline.
fn chomp(text: &str) -> &str {
    text.strip_suffix('\n').unwrap_or(text)
}

fn px4_vehicle_event(text: &str) -> Option<VehicleEvent> {
    let t = chomp(text);
    let lower = t.to_lowercase();
    for head in ["kill ", "kill-switch ", "kill switch "] {
        if let Some(rest) = lower.strip_prefix(head) {
            match rest {
                "engaged" => return Some(px4_killed()),
                "disengaged" => return Some(px4_released()),
                _ => {}
            }
        }
    }
    const BY: &str = "disarmed by ";
    if t.len() >= BY.len() && t.is_char_boundary(BY.len()) && t[..BY.len()].to_lowercase() == BY {
        let rest = &t[BY.len()..];
        if !rest.is_empty() && !rest.contains('\n') {
            let reason = rest.trim();
            let (level, words) = PX4_DISARM_REASONS
                .iter()
                .find(|(k, _)| *k == reason.to_lowercase())
                .map_or(("info", reason), |(_, v)| (v.0, v.1));
            return Some(event("disarm", level, &format!("Disarmed by {words}")));
        }
    }
    None
}

fn ardupilot_vehicle_event(text: &str) -> Option<VehicleEvent> {
    let t = chomp(text);
    let rest = t.strip_prefix("RC")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    match rest[digits..].strip_prefix(": MotorEStop ")? {
        "HIGH" => Some(ap_stopped()),
        "LOW" => Some(ap_released()),
        _ => None,
    }
}

fn px4_prearm_event(reason: &str) -> Option<VehicleEvent> {
    (reason.trim().to_lowercase() == "kill switch engaged").then(px4_killed)
}

fn ardupilot_prearm_event(reason: &str) -> Option<VehicleEvent> {
    (reason.trim().to_lowercase() == "motors emergency stopped").then(ap_stopped)
}

/// How a stack wants a guided takeoff staged. PX4 takes the command from any
/// mode, reads the altitude as AMSL and arms after; ArduPilot reads it
/// relative to home, refuses it outside GUIDED and while disarmed, so it goes
/// mode, arm, takeoff, and has to be able to disarm again if the last fails.
#[derive(Debug, Clone, PartialEq)]
pub struct TakeoffPlan {
    /// `takeoff_then_arm` or `mode_arm_takeoff`.
    pub order: &'static str,
    /// `amsl` or `relative`.
    pub altitude_frame: &'static str,
    pub guided_mode: &'static str,
    pub min_pitch_deg: f64,
}

/// A PREFLIGHT_CALIBRATION plan padded to seven params with zeros, which the
/// message defines as "no calibration" (NaN is undefined behaviour in PX4's cast).
fn cal(params: &[f64]) -> CommandPlan {
    let mut v = params.to_vec();
    v.resize(7, 0.0);
    CommandPlan::new(constant_or("MAV_CMD_PREFLIGHT_CALIBRATION", 241), &v[..7])
}

fn mag_cal_start() -> i64 {
    constant_or("MAV_CMD_DO_START_MAG_CAL", 42424)
}

fn mag_cal_cancel() -> i64 {
    constant_or("MAV_CMD_DO_CANCEL_MAG_CAL", 42426)
}

fn accelcal_pos() -> i64 {
    constant_or("MAV_CMD_ACCELCAL_VEHICLE_POS", 42429)
}

fn actuator_test(motor: i64, value: f64, timeout_s: f64) -> CommandPlan {
    CommandPlan::new(
        constant_or("MAV_CMD_ACTUATOR_TEST", 310),
        &[value, timeout_s, 0.0, 0.0, motor as f64, 0.0, 0.0],
    )
}

fn do_motor_test(motor: i64, throttle_pct: f64, timeout_s: f64) -> CommandPlan {
    CommandPlan::new(
        constant_or("MAV_CMD_DO_MOTOR_TEST", 209),
        &[motor as f64, 0.0, throttle_pct, timeout_s, 0.0, 0.0, 0.0],
    )
}

const ACCELCAL_POSITIONS: &[(&str, i64)] = &[
    ("level", 1),
    ("left", 2),
    ("right", 3),
    ("nosedown", 4),
    ("noseup", 5),
    ("back", 6),
];

fn plugin_commands_named(names: &[&str]) -> Vec<(i64, String)> {
    names
        .iter()
        .filter_map(|n| corvus_link::plain::defs::constant(&format!("MAV_CMD_{n}")).map(|v| (v, (*n).to_string())))
        .collect()
}

const COMMON_COMMANDS: &[&str] = &[
    "DO_CHANGE_SPEED",
    "DO_LAND_START",
    "DO_REPOSITION",
    "DO_SET_ROI_LOCATION",
    "DO_SET_ROI_NONE",
    "DO_DIGICAM_CONTROL",
    "DO_SET_CAM_TRIGG_DIST",
    "DO_GRIPPER",
    "DO_SET_MISSION_CURRENT",
    "REQUEST_MESSAGE",
    "DO_GIMBAL_MANAGER_PITCHYAW",
    "DO_VTOL_TRANSITION",
];
const PX4_COMMANDS: &[&str] = &["DO_ORBIT", "DO_SET_ACTUATOR"];
const AP_COMMANDS: &[&str] = &[
    "CONDITION_YAW",
    "DO_SET_RELAY",
    "DO_REPEAT_RELAY",
    "DO_SET_SERVO",
    "DO_REPEAT_SERVO",
    "DO_PAUSE_CONTINUE",
    "DO_AUX_FUNCTION",
];

/// The MAV_CMDs whose first three params are a position, for a plugin to convert.
pub fn plugin_position_commands() -> Vec<i64> {
    plugin_commands_named(&["DO_REPOSITION", "DO_SET_ROI_LOCATION", "DO_ORBIT"])
        .into_iter()
        .map(|(v, _)| v)
        .collect()
}

/// What one flight stack does. Cheap to copy; `dialect_for` makes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dialect {
    stack: Stack,
}

pub fn dialect_for_stack(stack: Stack) -> Dialect {
    Dialect { stack }
}

/// The dialect for a MAV_AUTOPILOT id.
pub fn dialect_for_id(autopilot: i64) -> Dialect {
    dialect_for_id_hinted(autopilot, "")
}

/// The dialect for a MAV_AUTOPILOT id and what the link said about the flight
/// controller. An autopilot that names itself PX4 or ArduPilot beats the hint.
pub fn dialect_for_id_hinted(autopilot: i64, hint: &str) -> Dialect {
    let mut stack = stack_for_autopilot_id(autopilot);
    if stack == Stack::Generic {
        stack = stack_for_hint(hint).unwrap_or(stack);
    }
    dialect_for_stack(stack)
}

/// Every setup tile that reads or writes the controller through MAVLink.
/// Betaflight is configured over MSP, so none of them can reach it from here.
const BETAFLIGHT_BLOCKED_TILES: [&str; 11] = [
    "firstsetup",
    "calibration",
    "control",
    "tuning",
    "motors",
    "safety",
    "battery",
    "rtk",
    "remoteid",
    "parameters",
    "firmware",
];

const PX4_MISSION_HINT: &str = "PX4 names the reason in its event log only. The usual ones are a takeoff or a landing the vehicle's mission settings require, or a fixed wing landing approach steeper than it allows.";

impl Dialect {
    pub fn stack(&self) -> Stack {
        self.stack
    }

    pub fn label(&self) -> &'static str {
        match self.stack {
            Stack::Px4 => "PX4",
            Stack::ArduPilot => "ArduPilot",
            Stack::Generic => "Generic",
            Stack::Betaflight => "Betaflight",
        }
    }

    /// A stack that only reports: no parameters, commands or missions.
    pub fn telemetry_only(&self) -> bool {
        self.stack == Stack::Betaflight
    }

    /// Setup tiles this stack cannot serve, each with the sentence the greyed
    /// tile carries. Reported, not hidden.
    pub fn setup_unavailable(&self) -> Map {
        let mut out = Map::new();
        if self.stack == Stack::Betaflight {
            for tile in BETAFLIGHT_BLOCKED_TILES {
                out.insert(
                    tile.to_string(),
                    json!("Not for Betaflight. Use the Betaflight Configurator for this."),
                );
            }
        }
        out
    }

    pub fn supports_shell(&self) -> bool {
        self.stack == Stack::Px4
    }

    pub fn supports_esc_calibration(&self) -> bool {
        self.stack == Stack::Px4
    }

    pub fn accel_cal_positions_are_prompted(&self) -> bool {
        self.stack == Stack::ArduPilot
    }

    /// `command`, `mode` or `none`.
    pub fn autotune_style(&self) -> &'static str {
        match self.stack {
            Stack::Px4 => "command",
            Stack::ArduPilot => "mode",
            Stack::Generic | Stack::Betaflight => "none",
        }
    }

    pub fn log_format(&self) -> &'static str {
        match self.stack {
            Stack::Px4 => "ulog",
            Stack::ArduPilot => "dataflash",
            Stack::Generic | Stack::Betaflight => "unknown",
        }
    }

    pub fn log_suffix(&self) -> &'static str {
        match self.stack {
            Stack::Px4 => ".ulg",
            Stack::ArduPilot => ".bin",
            Stack::Generic | Stack::Betaflight => ".log",
        }
    }

    pub fn firmware_vendor(&self) -> &'static str {
        match self.stack {
            Stack::Px4 => "px4",
            Stack::ArduPilot => "ardupilot",
            Stack::Generic | Stack::Betaflight => "",
        }
    }

    pub fn mission_seq0_is_home(&self) -> bool {
        self.stack == Stack::ArduPilot
    }

    pub fn mission_mode(&self) -> &'static str {
        match self.stack {
            Stack::Px4 => "MISSION",
            Stack::ArduPilot => "AUTO",
            Stack::Generic | Stack::Betaflight => "",
        }
    }

    pub fn mission_circles_as_waypoints(&self) -> bool {
        self.stack == Stack::Px4
    }

    pub fn mission_validity_reported(&self) -> bool {
        self.stack == Stack::Px4
    }

    pub fn mission_rejection_hint(&self) -> &'static str {
        if self.stack == Stack::Px4 {
            PX4_MISSION_HINT
        } else {
            ""
        }
    }

    pub fn param_encoding(&self) -> &'static str {
        if self.stack == Stack::Px4 {
            PARAM_ENCODING_BYTEWISE
        } else {
            PARAM_ENCODING_C_CAST
        }
    }

    pub fn param_metadata_path(&self) -> &'static str {
        match self.stack {
            Stack::Px4 => "/etc/extras/parameters.json.xz",
            Stack::ArduPilot => "@PARAM/param.pck?withdefaults=1",
            Stack::Generic | Stack::Betaflight => "",
        }
    }

    pub fn param_metadata_format(&self) -> &'static str {
        match self.stack {
            Stack::Px4 => "px4-json",
            Stack::ArduPilot => "ardupilot-pck",
            Stack::Generic | Stack::Betaflight => "",
        }
    }

    pub fn param_metadata_cacheable(&self) -> bool {
        self.stack == Stack::Px4
    }

    pub fn param_file_format(&self) -> &'static str {
        match self.stack {
            Stack::Px4 | Stack::Generic | Stack::Betaflight => "qgc",
            Stack::ArduPilot => "mission-planner",
        }
    }

    pub fn prearm_prefixes(&self) -> &'static [&'static str] {
        match self.stack {
            Stack::Px4 => &["Preflight Fail: "],
            Stack::ArduPilot => &["PreArm: ", "Arm: "],
            Stack::Generic | Stack::Betaflight => &["Preflight Fail: ", "PreArm: "],
        }
    }

    pub fn prearm_report_complete(&self) -> bool {
        self.stack == Stack::ArduPilot
    }

    pub fn routine_statustext_prefixes(&self) -> &'static [&'static str] {
        if self.stack == Stack::Px4 {
            &["[logger] "]
        } else {
            &[]
        }
    }

    /// The reason in a failing preflight check's STATUSTEXT.
    pub fn prearm_failure(&self, text: &str) -> Option<String> {
        for prefix in self.prearm_prefixes() {
            if let Some(rest) = text.strip_prefix(prefix) {
                let rest = rest.trim();
                return (!rest.is_empty()).then(|| rest.to_string());
            }
        }
        None
    }

    /// Is this informational line housekeeping the board can do without?
    pub fn is_routine_statustext(&self, text: &str) -> bool {
        self.routine_statustext_prefixes().iter().any(|p| text.starts_with(p))
    }

    /// The kill switch or disarm a STATUSTEXT reports.
    pub fn vehicle_event(&self, text: &str) -> Option<VehicleEvent> {
        match self.stack {
            Stack::Px4 => px4_vehicle_event(text),
            Stack::ArduPilot => ardupilot_vehicle_event(text),
            Stack::Generic | Stack::Betaflight => px4_vehicle_event(text).or_else(|| ardupilot_vehicle_event(text)),
        }
    }

    /// The kill switch a failing preflight check names.
    pub fn prearm_event(&self, reason: &str) -> Option<VehicleEvent> {
        match self.stack {
            Stack::Px4 => px4_prearm_event(reason),
            Stack::ArduPilot => ardupilot_prearm_event(reason),
            Stack::Generic | Stack::Betaflight => px4_prearm_event(reason).or_else(|| ardupilot_prearm_event(reason)),
        }
    }

    /// A HEARTBEAT's mode as a name an operator would recognise.
    pub fn decode_mode(&self, custom_mode: i64, base_mode: i64, mav_type: i64) -> String {
        match self.stack {
            Stack::Px4 => {
                let main = (custom_mode >> 16) & 0xFF;
                let sub = (custom_mode >> 24) & 0xFF;
                let find = |t: Table, k: i64| t.iter().find(|(n, _)| *n == k).map(|(_, v)| (*v).to_string());
                if main == 4 {
                    return find(PX4_AUTO_SUBMODE, sub).unwrap_or_else(|| format!("SUBMODE_{sub}"));
                }
                if main == 3 {
                    return find(PX4_POSCTL_SUBMODE, sub).unwrap_or_else(|| format!("POSCTL_SUBMODE_{sub}"));
                }
                find(PX4_MAIN_MODE, main).unwrap_or_else(|| format!("MODE_{custom_mode}"))
            }
            Stack::ArduPilot => {
                let enabled = constant_or("MAV_MODE_FLAG_CUSTOM_MODE_ENABLED", 1);
                let manual = constant_or("MAV_MODE_FLAG_MANUAL_INPUT_ENABLED", 64);
                if base_mode & enabled == 0 {
                    return if base_mode & manual != 0 {
                        "MANUAL".into()
                    } else {
                        String::new()
                    };
                }
                ap_table(mav_type)
                    .iter()
                    .find(|(n, _)| *n == custom_mode)
                    .map_or_else(|| format!("MODE_{custom_mode}"), |(_, v)| (*v).to_string())
            }
            Stack::Generic | Stack::Betaflight => {
                if custom_mode != 0 {
                    format!("MODE_{custom_mode}")
                } else {
                    String::new()
                }
            }
        }
    }

    /// The mode as the operator reads it (display only; the stack's own
    /// spelling is what a mode change sends back).
    pub fn mode_label(&self, name: &str) -> String {
        if self.stack == Stack::Px4 {
            if let Some((_, l)) = PX4_MODE_LABELS.iter().find(|(n, _)| *n == name) {
                return (*l).to_string();
            }
        }
        plain_mode_label(name)
    }

    /// Every mode this vehicle can be commanded into, by name, in the order
    /// of the stack's table.
    pub fn mode_table(&self, mav_type: i64, firmware: Option<&[i64]>) -> Vec<(String, ModeCommand)> {
        match self.stack {
            Stack::Px4 => PX4_MODE_VALUES
                .iter()
                .filter(|(n, _)| px4_mode_supported(n, firmware))
                .map(|(n, (b, m, s))| {
                    (
                        (*n).to_string(),
                        ModeCommand {
                            base_mode: *b,
                            custom_mode: *m,
                            custom_sub: *s,
                        },
                    )
                })
                .collect(),
            Stack::ArduPilot => {
                let enabled = constant_or("MAV_MODE_FLAG_CUSTOM_MODE_ENABLED", 1);
                ap_table(mav_type)
                    .iter()
                    .filter(|(_, n)| !AP_RETIRED_MODES.contains(n))
                    .map(|(num, n)| {
                        (
                            (*n).to_string(),
                            ModeCommand {
                                base_mode: enabled,
                                custom_mode: *num,
                                custom_sub: 0,
                            },
                        )
                    })
                    .collect()
            }
            Stack::Generic | Stack::Betaflight => Vec::new(),
        }
    }

    /// The selector's modes, most manual first for PX4, alphabetical for
    /// ArduPilot, none for an unknown stack.
    pub fn available_modes(&self, mav_type: i64, firmware: Option<&[i64]>) -> Vec<String> {
        match self.stack {
            Stack::Px4 => PX4_AVAILABLE_MODES
                .iter()
                .filter(|n| px4_mode_supported(n, firmware))
                .map(|n| (*n).to_string())
                .collect(),
            Stack::ArduPilot => {
                let mut names: Vec<String> = self.mode_table(mav_type, None).into_iter().map(|(n, _)| n).collect();
                names.sort();
                names
            }
            Stack::Generic | Stack::Betaflight => Vec::new(),
        }
    }

    /// Turns the link's own mode mapping into commands, or nothing when it
    /// cannot: PX4 adopts the `(base, main, sub)` triples (through the same
    /// version check as its built-in table, which fills in what it lacks),
    /// ArduPilot the flat numbers, anything else adopts nothing.
    pub fn adopt_live_mapping(
        &self,
        mapping: &[(String, LiveMode)],
        mav_type: i64,
        firmware: Option<&[i64]>,
    ) -> Vec<(String, ModeCommand)> {
        match self.stack {
            Stack::Px4 => {
                let live: Vec<(String, ModeCommand)> = mapping
                    .iter()
                    .filter_map(|(n, m)| match m {
                        LiveMode::Triple(b, c, s) => Some((
                            n.clone(),
                            ModeCommand {
                                base_mode: *b,
                                custom_mode: *c,
                                custom_sub: *s,
                            },
                        )),
                        _ => None,
                    })
                    .collect();
                if live.is_empty() {
                    return Vec::new();
                }
                let mut usable = self.mode_table(mav_type, firmware);
                for (name, command) in live {
                    if !px4_mode_supported(&name, firmware) {
                        continue;
                    }
                    match usable.iter_mut().find(|(n, _)| *n == name) {
                        Some(slot) => slot.1 = command,
                        None => usable.push((name, command)),
                    }
                }
                usable
            }
            Stack::ArduPilot => {
                let enabled = constant_or("MAV_MODE_FLAG_CUSTOM_MODE_ENABLED", 1);
                let mut usable: Vec<(String, ModeCommand)> = Vec::new();
                for (name, m) in mapping {
                    let LiveMode::Flat(value) = m else { continue };
                    if AP_RETIRED_MODES.contains(&name.as_str()) {
                        continue;
                    }
                    let command = ModeCommand {
                        base_mode: enabled,
                        custom_mode: *value,
                        custom_sub: 0,
                    };
                    match usable.iter_mut().find(|(n, _)| n == name) {
                        Some(slot) => slot.1 = command,
                        None => usable.push((name.clone(), command)),
                    }
                }
                usable
            }
            Stack::Generic | Stack::Betaflight => Vec::new(),
        }
    }

    pub fn takeoff_plan(&self, _mav_type: i64) -> TakeoffPlan {
        match self.stack {
            Stack::ArduPilot => TakeoffPlan {
                order: "mode_arm_takeoff",
                altitude_frame: "relative",
                guided_mode: AP_GUIDED_MODE,
                min_pitch_deg: 0.0,
            },
            _ => TakeoffPlan {
                order: "takeoff_then_arm",
                altitude_frame: "amsl",
                guided_mode: "",
                min_pitch_deg: f64::NAN,
            },
        }
    }

    /// The mode a position command needs the vehicle in, or "" (PX4 switches itself).
    pub fn guided_mode(&self, mav_type: i64) -> &'static str {
        match self.stack {
            Stack::ArduPilot if mav_type != 5 => AP_GUIDED_MODE,
            _ => "",
        }
    }

    /// The mode that flies from MANUAL_CONTROL and holds position, or "".
    pub fn stick_mode(&self, mav_type: i64) -> &'static str {
        match self.stack {
            Stack::Px4 => "POSCTL",
            Stack::ArduPilot => {
                if (COPTER_TYPES.contains(&mav_type) || mav_type == 12) && ap_names(mav_type).contains(&"POSHOLD") {
                    "POSHOLD"
                } else {
                    ""
                }
            }
            Stack::Generic | Stack::Betaflight => "",
        }
    }

    /// Every mode that already answers the sticks.
    pub fn stick_modes(&self, mav_type: i64) -> Vec<&'static str> {
        match self.stack {
            Stack::Px4 => vec![
                "MANUAL",
                "ALTCTL",
                "POSCTL",
                "POSITION_SLOW",
                "STABILIZED",
                "ACRO",
                "ALTITUDE_CRUISE",
            ],
            Stack::ArduPilot => {
                let names = ap_names(mav_type);
                [
                    "STABILIZE",
                    "ACRO",
                    "ALT_HOLD",
                    "LOITER",
                    "POSHOLD",
                    "SPORT",
                    "DRIFT",
                    "FLOWHOLD",
                    "MANUAL",
                    "FBWA",
                    "FBWB",
                    "CRUISE",
                    "STEERING",
                    "QSTABILIZE",
                    "QHOVER",
                    "QLOITER",
                ]
                .into_iter()
                .filter(|n| names.contains(n))
                .collect()
            }
            Stack::Generic | Stack::Betaflight => Vec::new(),
        }
    }

    /// How DO_REPOSITION's altitude is sent: `amsl` (PX4 reads it so whatever
    /// the frame says, the station adds home itself) or `relative`.
    pub fn reposition_altitude_frame(&self, _mav_type: i64) -> &'static str {
        if self.stack == Stack::ArduPilot {
            "relative"
        } else {
            "amsl"
        }
    }

    /// The mode that stops the vehicle where it is and keeps it there, or "".
    pub fn hold_mode(&self, mav_type: i64) -> &'static str {
        match self.stack {
            Stack::Px4 => "LOITER",
            Stack::ArduPilot => {
                let wanted = match vehicle_class(mav_type) {
                    VehicleClass::Rover => "HOLD",
                    VehicleClass::Sub => "POSHOLD",
                    _ => "LOITER",
                };
                if ap_names(mav_type).contains(&wanted) {
                    wanted
                } else {
                    ""
                }
            }
            Stack::Generic | Stack::Betaflight => "",
        }
    }

    /// The mode that flies velocity setpoints, or "" when none does (not on a plane).
    pub fn velocity_mode(&self, mav_type: i64) -> &'static str {
        if vehicle_class(mav_type) == VehicleClass::Plane {
            return "";
        }
        match self.stack {
            Stack::Px4 => "OFFBOARD",
            Stack::ArduPilot => self.guided_mode(mav_type),
            Stack::Generic | Stack::Betaflight => "",
        }
    }

    /// The MAV_CMDs a plugin may send this vehicle, id to name, by id.
    pub fn plugin_commands(&self, _mav_type: i64) -> Vec<(i64, String)> {
        let mut out = plugin_commands_named(COMMON_COMMANDS);
        match self.stack {
            Stack::Px4 => out.extend(plugin_commands_named(PX4_COMMANDS)),
            Stack::ArduPilot => out.extend(plugin_commands_named(AP_COMMANDS)),
            Stack::Generic | Stack::Betaflight => return Vec::new(),
        }
        out
    }

    /// MISSION_START's (first, last) item for `count` uploaded items: PX4 is
    /// given both ends, ArduPilot 4.6 answers anything but (0, 0) with DENIED.
    pub fn mission_start_params(&self, count: i64) -> (f64, f64) {
        match self.stack {
            Stack::ArduPilot => (0.0, 0.0),
            _ => (0.0, (count.max(1) - 1) as f64),
        }
    }

    /// The mode to arm in before MISSION_START, or "" to arm afterwards.
    pub fn mission_arm_mode(&self, mav_type: i64) -> &'static str {
        match self.stack {
            Stack::ArduPilot => {
                if vehicle_class(mav_type) == VehicleClass::Plane {
                    self.mission_mode()
                } else {
                    self.guided_mode(mav_type)
                }
            }
            _ => "",
        }
    }

    fn calibrations(&self) -> Vec<(&'static str, CommandPlan)> {
        let nan = f64::NAN;
        match self.stack {
            Stack::Px4 => vec![
                ("gyro", cal(&[1.0])),
                ("compass", cal(&[0.0, 1.0])),
                ("baro", cal(&[0.0, 0.0, 1.0])),
                ("accel", cal(&[0.0, 0.0, 0.0, 0.0, 1.0])),
                ("level", cal(&[0.0, 0.0, 0.0, 0.0, 2.0])),
                ("accel_quick", cal(&[0.0, 0.0, 0.0, 0.0, 4.0])),
                ("airspeed", cal(&[0.0, 0.0, 0.0, 0.0, 0.0, 1.0])),
                ("motor", cal(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0])),
            ],
            Stack::ArduPilot => vec![
                ("gyro", cal(&[1.0])),
                ("baro", cal(&[0.0, 0.0, 1.0])),
                ("accel", cal(&[0.0, 0.0, 0.0, 0.0, 1.0])),
                ("level", cal(&[0.0, 0.0, 0.0, 0.0, 2.0])),
                ("accel_quick", cal(&[0.0, 0.0, 0.0, 0.0, 4.0])),
                ("airspeed", cal(&[0.0, 0.0, 1.0])),
                ("compass", CommandPlan::new(mag_cal_start(), &[0.0, 0.0, 1.0, 0.0, 0.0, nan, nan])),
                (
                    "motor",
                    CommandPlan::unsupported(
                        "ArduPilot calibrates ESCs by setting ESC_CALIBRATION to 3 and rebooting with the throttle high. It is not a MAVLink command, so Corvus will not do it behind a button",
                    ),
                ),
            ],
            Stack::Generic | Stack::Betaflight => vec![
                ("gyro", cal(&[1.0])),
                ("baro", cal(&[0.0, 0.0, 1.0])),
                ("accel", cal(&[0.0, 0.0, 0.0, 0.0, 1.0])),
                ("level", cal(&[0.0, 0.0, 0.0, 0.0, 2.0])),
            ],
        }
    }

    /// The calibrations this stack can actually run, sorted.
    pub fn available_calibrations(&self) -> Vec<&'static str> {
        let mut names: Vec<&str> = self
            .calibrations()
            .into_iter()
            .filter(|(_, p)| p.ok())
            .map(|(n, _)| n)
            .collect();
        names.sort_unstable();
        names
    }

    /// Does PX4 know a calibration by this name? (The names every page uses.)
    pub fn px4_calibration_exists(sensor: &str) -> bool {
        dialect_for_stack(Stack::Px4).calibration(sensor).is_some()
    }

    /// The calibration of a sensor, if this stack has one (a plan that cannot
    /// run carries its reason).
    pub fn calibration(&self, sensor: &str) -> Option<CommandPlan> {
        self.calibrations()
            .into_iter()
            .find(|(n, _)| *n == sensor)
            .map(|(_, p)| p)
    }

    /// Stops whatever calibration is running: all zeros, except ArduPilot's
    /// own command for a magnetometer fit.
    pub fn cancel_calibration(&self, sensor: &str) -> CommandPlan {
        if self.stack == Stack::ArduPilot && sensor == "compass" {
            let nan = f64::NAN;
            return CommandPlan::new(mag_cal_cancel(), &[0.0, 0.0, 0.0, nan, nan, nan, nan]);
        }
        cal(&[0.0; 7])
    }

    /// Tells ArduPilot the aircraft is in the position it asked for.
    pub fn accel_position(&self, position: &str) -> CommandPlan {
        if self.stack != Stack::ArduPilot {
            return CommandPlan::unsupported(
                "PX4 detects each calibration position on its own. There is nothing for the ground station to confirm",
            );
        }
        match ACCELCAL_POSITIONS.iter().find(|(n, _)| *n == position.to_lowercase()) {
            Some((_, v)) => CommandPlan::new(accelcal_pos(), &[*v as f64, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            None => CommandPlan::unsupported(&format!("unknown calibration position: {position}")),
        }
    }

    /// Spins one motor on the bench: PX4 through the actuator test (it has no
    /// DO_MOTOR_TEST handler), everyone else through DO_MOTOR_TEST.
    pub fn motor_test(&self, motor: i64, throttle_pct: f64, duration_s: f64) -> CommandPlan {
        match self.stack {
            Stack::Px4 => actuator_test(motor, throttle_pct / 100.0, duration_s),
            _ => do_motor_test(motor, throttle_pct, duration_s),
        }
    }

    pub fn motor_stop(&self, motor: i64) -> CommandPlan {
        match self.stack {
            Stack::Px4 => actuator_test(motor, f64::NAN, 0.0),
            _ => do_motor_test(motor, 0.0, 0.0),
        }
    }

    /// The flight mode that is the autotune on this vehicle, or "".
    pub fn autotune_mode(&self, mav_type: i64) -> &'static str {
        if self.stack == Stack::ArduPilot && ap_names(mav_type).contains(&"AUTOTUNE") {
            "AUTOTUNE"
        } else {
            ""
        }
    }

    pub fn autotune_plan(&self, mav_type: i64, enable: bool) -> CommandPlan {
        match self.stack {
            Stack::Px4 => {
                let nan = f64::NAN;
                CommandPlan::new(
                    constant_or("MAV_CMD_DO_AUTOTUNE_ENABLE", 212),
                    &[if enable { 1.0 } else { 0.0 }, 0.0, nan, nan, nan, nan, nan],
                )
            }
            Stack::ArduPilot => {
                if self.autotune_mode(mav_type).is_empty() {
                    CommandPlan::unsupported("this ArduPilot vehicle has no autotune mode")
                } else {
                    CommandPlan::unsupported("")
                }
            }
            Stack::Generic => CommandPlan::unsupported("Corvus does not know how this autopilot runs an autotune"),
            Stack::Betaflight => CommandPlan::unsupported("Betaflight takes no commands over MAVLink"),
        }
    }

    /// The mission items a kind of vehicle cannot do, by command id.
    pub fn mission_refusals(&self, mav_type: i64) -> Vec<(i64, &'static str)> {
        if is_ground_vehicle(mav_type) {
            return vec![
                (
                    constant_or("MAV_CMD_NAV_TAKEOFF", 22),
                    "A rover drives, it does not take off",
                ),
                (constant_or("MAV_CMD_NAV_LAND", 21), "A rover drives, it does not land"),
            ];
        }
        Vec::new()
    }

    /// The flags the HTTP layer publishes so the interface can adapt.
    pub fn capabilities(&self, mav_type: i64) -> Map {
        let takeoff = self.takeoff_plan(mav_type);
        let mut calibrations: Vec<&str> = self
            .calibrations()
            .into_iter()
            .filter(|(_, p)| p.ok())
            .map(|(n, _)| n)
            .collect();
        calibrations.sort_unstable();
        let mut commands: Vec<String> = self.plugin_commands(mav_type).into_iter().map(|(_, n)| n).collect();
        commands.sort();
        let v = json!({
            "stack": self.stack.as_str(),
            "vehicle": vehicle_class(mav_type).as_str(),
            "ground": is_ground_vehicle(mav_type),
            "label": self.label(),
            "telemetry_only": self.telemetry_only(),
            "setup_unavailable": self.setup_unavailable(),
            "shell": self.supports_shell(),
            "esc_calibration": self.supports_esc_calibration(),
            "accel_cal_prompted": self.accel_cal_positions_are_prompted(),
            "autotune": self.autotune_style(),
            "autotune_mode": self.autotune_mode(mav_type),
            "log_format": self.log_format(),
            "log_suffix": self.log_suffix(),
            "firmware_vendor": self.firmware_vendor(),
            "guided_mode": self.guided_mode(mav_type),
            "stick_mode": self.stick_mode(mav_type),
            "stick_modes": self.stick_modes(mav_type),
            "mission_mode": self.mission_mode(),
            "hold_mode": self.hold_mode(mav_type),
            "velocity_mode": self.velocity_mode(mav_type),
            "commands": commands,
            "takeoff_frame": takeoff.altitude_frame,
            "param_defaults": !self.param_metadata_path().is_empty(),
            "param_file_format": self.param_file_format(),
            "calibrations": calibrations,
        });
        match v {
            Value::Object(m) => m,
            _ => Map::new(),
        }
    }
}

/// One entry of the link's own mode mapping: PX4's `(base, main, sub)`, or
/// the flat number everyone else uses.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveMode {
    Triple(i64, i64, i64),
    Flat(i64),
    /// Anything else (a text, a float): never adopted.
    Other,
}
