//! Flight mode tables and decoding, as pymavlink's `mavutil` has them.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use crate::defs::constant_or;

pub const MODE_MAPPING_APM: &[(i64, &str)] = &[
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
    (25, "LOITERALTQLAND"),
    (26, "AUTOLAND"),
];

pub const MODE_MAPPING_ACM: &[(i64, &str)] = &[
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
    (29, "RATE_ACRO"),
];

pub const MODE_MAPPING_ROVER: &[(i64, &str)] = &[
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

pub const MODE_MAPPING_TRACKER: &[(i64, &str)] = &[
    (0, "MANUAL"),
    (1, "STOP"),
    (2, "SCAN"),
    (4, "GUIDED"),
    (10, "AUTO"),
    (16, "INITIALISING"),
];

pub const MODE_MAPPING_SUB: &[(i64, &str)] = &[
    (0, "STABILIZE"),
    (1, "ACRO"),
    (2, "ALT_HOLD"),
    (3, "AUTO"),
    (4, "GUIDED"),
    (7, "CIRCLE"),
    (9, "SURFACE"),
    (16, "POSHOLD"),
    (19, "MANUAL"),
];

pub const MODE_MAPPING_BLIMP: &[(i64, &str)] = &[
    (0, "LAND"),
    (1, "MANUAL"),
    (2, "VELOCITY"),
    (3, "LOITER"),
    (4, "RTL"),
    (5, "AUTO"),
    (6, "HOLD"),
];

pub const MAINSTATE_MAPPING_PX4: &[(i64, &str)] = &[
    (0, "MANUAL"),
    (1, "ALTCTL"),
    (2, "POSCTL"),
    (3, "AUTO_MISSION"),
    (4, "AUTO_LOITER"),
    (5, "AUTO_RTL"),
    (6, "ACRO"),
    (7, "OFFBOARD"),
    (8, "STAB"),
    (9, "RATTITUDE"),
    (10, "AUTO_TAKEOFF"),
    (11, "AUTO_LAND"),
    (12, "AUTO_FOLLOW_TARGET"),
    (13, "MAX"),
];

pub const PX4_CUSTOM_MAIN_MODE_MANUAL: i64 = 1;
pub const PX4_CUSTOM_MAIN_MODE_ALTCTL: i64 = 2;
pub const PX4_CUSTOM_MAIN_MODE_POSCTL: i64 = 3;
pub const PX4_CUSTOM_MAIN_MODE_AUTO: i64 = 4;
pub const PX4_CUSTOM_MAIN_MODE_ACRO: i64 = 5;
pub const PX4_CUSTOM_MAIN_MODE_OFFBOARD: i64 = 6;
pub const PX4_CUSTOM_MAIN_MODE_STABILIZED: i64 = 7;
pub const PX4_CUSTOM_MAIN_MODE_RATTITUDE: i64 = 8;

pub const PX4_CUSTOM_SUB_MODE_OFFBOARD: i64 = 0;
pub const PX4_CUSTOM_SUB_MODE_AUTO_READY: i64 = 1;
pub const PX4_CUSTOM_SUB_MODE_AUTO_TAKEOFF: i64 = 2;
pub const PX4_CUSTOM_SUB_MODE_AUTO_LOITER: i64 = 3;
pub const PX4_CUSTOM_SUB_MODE_AUTO_MISSION: i64 = 4;
pub const PX4_CUSTOM_SUB_MODE_AUTO_RTL: i64 = 5;
pub const PX4_CUSTOM_SUB_MODE_AUTO_LAND: i64 = 6;
pub const PX4_CUSTOM_SUB_MODE_AUTO_RTGS: i64 = 7;
pub const PX4_CUSTOM_SUB_MODE_AUTO_FOLLOW_TARGET: i64 = 8;

/// `AP_MAV_TYPE_MODE_MAP`: MAV_TYPE to ArduPilot mode table.
pub fn ap_mode_table(mav_type: i64) -> Option<&'static [(i64, &'static str)]> {
    let copter = [
        "MAV_TYPE_HELICOPTER",
        "MAV_TYPE_TRICOPTER",
        "MAV_TYPE_QUADROTOR",
        "MAV_TYPE_HEXAROTOR",
        "MAV_TYPE_OCTOROTOR",
        "MAV_TYPE_DECAROTOR",
        "MAV_TYPE_DODECAROTOR",
        "MAV_TYPE_COAXIAL",
    ];
    let plane = [
        "MAV_TYPE_FIXED_WING",
        "MAV_TYPE_VTOL_TILTROTOR",
        "MAV_TYPE_VTOL_DUOROTOR",
        "MAV_TYPE_VTOL_TAILSITTER_DUOROTOR",
        "MAV_TYPE_VTOL_QUADROTOR",
        "MAV_TYPE_VTOL_TAILSITTER_QUADROTOR",
    ];
    let is = |names: &[&str]| names.iter().any(|n| crate::defs::constant(n) == Some(mav_type));
    if is(&copter) {
        Some(MODE_MAPPING_ACM)
    } else if is(&plane) {
        Some(MODE_MAPPING_APM)
    } else if is(&["MAV_TYPE_GROUND_ROVER", "MAV_TYPE_SURFACE_BOAT"]) {
        Some(MODE_MAPPING_ROVER)
    } else if is(&["MAV_TYPE_ANTENNA_TRACKER"]) {
        Some(MODE_MAPPING_TRACKER)
    } else if is(&["MAV_TYPE_SUBMARINE"]) {
        Some(MODE_MAPPING_SUB)
    } else if is(&["MAV_TYPE_AIRSHIP"]) {
        Some(MODE_MAPPING_BLIMP)
    } else {
        None
    }
}

/// Every MAV_TYPE with a table, in the order pymavlink builds the map.
pub fn ap_mode_types() -> Vec<i64> {
    let names = [
        "MAV_TYPE_HELICOPTER",
        "MAV_TYPE_TRICOPTER",
        "MAV_TYPE_QUADROTOR",
        "MAV_TYPE_HEXAROTOR",
        "MAV_TYPE_OCTOROTOR",
        "MAV_TYPE_DECAROTOR",
        "MAV_TYPE_DODECAROTOR",
        "MAV_TYPE_COAXIAL",
        "MAV_TYPE_FIXED_WING",
        "MAV_TYPE_VTOL_TILTROTOR",
        "MAV_TYPE_GROUND_ROVER",
        "MAV_TYPE_SURFACE_BOAT",
        "MAV_TYPE_ANTENNA_TRACKER",
        "MAV_TYPE_SUBMARINE",
        "MAV_TYPE_AIRSHIP",
        "MAV_TYPE_VTOL_DUOROTOR",
        "MAV_TYPE_VTOL_TAILSITTER_DUOROTOR",
        "MAV_TYPE_VTOL_QUADROTOR",
        "MAV_TYPE_VTOL_TAILSITTER_QUADROTOR",
    ];
    let mut out = Vec::new();
    for n in names {
        if let Some(v) = crate::defs::constant(n) {
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
}

pub fn table_dict<'py>(py: Python<'py>, table: &[(i64, &str)]) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    for (k, v) in table {
        d.set_item(k, v)?;
    }
    Ok(d)
}

fn auto_mode_flags() -> i64 {
    constant_or("MAV_MODE_FLAG_AUTO_ENABLED", 4)
        | constant_or("MAV_MODE_FLAG_STABILIZE_ENABLED", 16)
        | constant_or("MAV_MODE_FLAG_GUIDED_ENABLED", 8)
}

/// `px4_map`: mode name to (base_mode, main_mode, sub_mode).
pub fn px4_map(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let custom = constant_or("MAV_MODE_FLAG_CUSTOM_MODE_ENABLED", 1);
    let stabilize = constant_or("MAV_MODE_FLAG_STABILIZE_ENABLED", 16);
    let manual = constant_or("MAV_MODE_FLAG_MANUAL_INPUT_ENABLED", 64);
    let auto = custom | auto_mode_flags();
    let rows: [(&str, i64, i64, i64); 14] = [
        ("MANUAL", custom | stabilize | manual, PX4_CUSTOM_MAIN_MODE_MANUAL, 0),
        (
            "STABILIZED",
            custom | stabilize | manual,
            PX4_CUSTOM_MAIN_MODE_STABILIZED,
            0,
        ),
        ("ACRO", custom | manual, PX4_CUSTOM_MAIN_MODE_ACRO, 0),
        ("RATTITUDE", custom | manual, PX4_CUSTOM_MAIN_MODE_RATTITUDE, 0),
        ("ALTCTL", custom | stabilize | manual, PX4_CUSTOM_MAIN_MODE_ALTCTL, 0),
        ("POSCTL", custom | stabilize | manual, PX4_CUSTOM_MAIN_MODE_POSCTL, 0),
        (
            "LOITER",
            auto,
            PX4_CUSTOM_MAIN_MODE_AUTO,
            PX4_CUSTOM_SUB_MODE_AUTO_LOITER,
        ),
        (
            "MISSION",
            auto,
            PX4_CUSTOM_MAIN_MODE_AUTO,
            PX4_CUSTOM_SUB_MODE_AUTO_MISSION,
        ),
        ("RTL", auto, PX4_CUSTOM_MAIN_MODE_AUTO, PX4_CUSTOM_SUB_MODE_AUTO_RTL),
        ("LAND", auto, PX4_CUSTOM_MAIN_MODE_AUTO, PX4_CUSTOM_SUB_MODE_AUTO_LAND),
        ("RTGS", auto, PX4_CUSTOM_MAIN_MODE_AUTO, PX4_CUSTOM_SUB_MODE_AUTO_RTGS),
        (
            "FOLLOWME",
            auto,
            PX4_CUSTOM_MAIN_MODE_AUTO,
            PX4_CUSTOM_SUB_MODE_AUTO_FOLLOW_TARGET,
        ),
        ("OFFBOARD", auto, PX4_CUSTOM_MAIN_MODE_OFFBOARD, 0),
        (
            "TAKEOFF",
            auto,
            PX4_CUSTOM_MAIN_MODE_AUTO,
            PX4_CUSTOM_SUB_MODE_AUTO_TAKEOFF,
        ),
    ];
    let d = PyDict::new(py);
    for (name, base, main, sub) in rows {
        d.set_item(name, PyTuple::new(py, [base, main, sub])?)?;
    }
    Ok(d)
}

pub fn interpret_px4_mode(custom_mode: i64) -> &'static str {
    let main = (custom_mode & 0xFF0000) >> 16;
    let sub = (custom_mode & 0xFF000000) >> 24;
    match main {
        PX4_CUSTOM_MAIN_MODE_MANUAL => "MANUAL",
        PX4_CUSTOM_MAIN_MODE_ACRO => "ACRO",
        PX4_CUSTOM_MAIN_MODE_RATTITUDE => "RATTITUDE",
        PX4_CUSTOM_MAIN_MODE_STABILIZED => "STABILIZED",
        PX4_CUSTOM_MAIN_MODE_ALTCTL => "ALTCTL",
        PX4_CUSTOM_MAIN_MODE_POSCTL => "POSCTL",
        PX4_CUSTOM_MAIN_MODE_OFFBOARD => "OFFBOARD",
        PX4_CUSTOM_MAIN_MODE_AUTO => match sub {
            PX4_CUSTOM_SUB_MODE_AUTO_MISSION => "MISSION",
            PX4_CUSTOM_SUB_MODE_AUTO_TAKEOFF => "TAKEOFF",
            PX4_CUSTOM_SUB_MODE_AUTO_LOITER => "LOITER",
            PX4_CUSTOM_SUB_MODE_AUTO_FOLLOW_TARGET => "FOLLOWME",
            PX4_CUSTOM_SUB_MODE_AUTO_RTL => "RTL",
            PX4_CUSTOM_SUB_MODE_AUTO_LAND => "LAND",
            PX4_CUSTOM_SUB_MODE_AUTO_RTGS => "RTGS",
            _ => "UNKNOWN",
        },
        _ => "UNKNOWN",
    }
}

fn int_attr(msg: &Bound<'_, PyAny>, name: &str) -> PyResult<i64> {
    msg.getattr(name)?.call_method0("__index__")?.extract()
}

/// `mode_string_v10`: the mode name a HEARTBEAT (or HIGH_LATENCY2) states.
pub fn mode_string_v10(msg: &Bound<'_, PyAny>) -> PyResult<String> {
    let autopilot = int_attr(msg, "autopilot")?;
    let msg_type: String = msg.call_method0("get_type")?.extract()?;
    let high_latency = msg_type == "HIGH_LATENCY2";
    let custom_mode = int_attr(msg, "custom_mode")?;
    if autopilot == constant_or("MAV_AUTOPILOT_PX4", 12) {
        if high_latency {
            return Ok(format!("Mode({custom_mode})"));
        }
        return Ok(interpret_px4_mode(custom_mode).to_string());
    }
    if !high_latency {
        let base_mode = int_attr(msg, "base_mode")?;
        if base_mode & constant_or("MAV_MODE_FLAG_CUSTOM_MODE_ENABLED", 1) == 0 {
            return Ok(format!("Mode(0x{base_mode:08x})"));
        }
    }
    let mav_type = int_attr(msg, "type")?;
    if let Some(table) = ap_mode_table(mav_type) {
        if let Some((_, name)) = table.iter().find(|(n, _)| *n == custom_mode) {
            return Ok(name.to_string());
        }
    }
    Ok(format!("Mode({custom_mode})"))
}
