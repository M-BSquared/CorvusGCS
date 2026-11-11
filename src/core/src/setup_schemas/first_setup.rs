//! First setup: is the connected controller one nobody has configured yet
//! (`first_setup.py`)?
//!
//! Detection reads facts, not a flag Corvus set earlier, so a board configured by
//! another ground station is recognised as configured and a wiped board as fresh.
//! PX4 `SYS_AUTOSTART` is 0 until an airframe is chosen; PX4 stores the device id a
//! calibration was made for (`CAL_ACC0_ID` and friends), 0 meaning none ever done;
//! ArduPilot Copter ships with `FRAME_CLASS` 1, and its accelerometer and compass
//! are judged by the offsets a calibration writes (its gyro calibrates itself at
//! every boot and is not asked about). Every parameter is a candidate only: one the
//! vehicle does not answer for makes that fact `unknown`, never an error, and
//! `unknown` never counts towards a prompt.
//!
//! `needs_setup` is the fact ("this looks fresh"); `prompt` adds the policy (offer
//! the wizard unprompted). A simulator over UDP or TCP is never prompted.

use serde_json::{json, Value};

use super::fields::*;
use crate::dialect::{vehicle_class, VehicleClass};

pub const DONE: &str = "done";
pub const TODO: &str = "todo";
pub const UNKNOWN: &str = "unknown";
pub const NOT_REQUIRED: &str = "not_required";
const STACK_ARDUPILOT: &str = "ardupilot";

/// Links where a fresh board can be sitting on the other end of the cable.
const PHYSICAL_TRANSPORTS: [&str; 2] = ["usb", "sik"];

const PX4_PARAMS: [&str; 5] = [
    "SYS_AUTOSTART",
    "CAL_ACC0_ID",
    "CAL_GYRO0_ID",
    "CAL_MAG0_ID",
    "SYS_HAS_MAG",
];
const ARDUPILOT_PARAMS: [&str; 10] = [
    "FRAME_CLASS",
    "FRAME_TYPE",
    "INS_ACCOFFS_X",
    "INS_ACCOFFS_Y",
    "INS_ACCOFFS_Z",
    "COMPASS_OFS_X",
    "COMPASS_OFS_Y",
    "COMPASS_OFS_Z",
    "COMPASS_DEV_ID",
    "COMPASS_USE",
];

/// SYS_AUTOSTART presets, the generic airframes of PX4's airframe reference. Ids
/// are the same on v1.16, v1.17 and v1.18. Vendor frames are deliberately absent:
/// a vendor airframe is a specific product with tuned gains.
const PX4_PRESETS: [(i64, &str, &str); 13] = [
    (4001, "Multirotor", "Quadcopter X"),
    (5001, "Multirotor", "Quadcopter +"),
    (6001, "Multirotor", "Hexarotor X"),
    (7001, "Multirotor", "Hexarotor +"),
    (8001, "Multirotor", "Octocopter X"),
    (9001, "Multirotor", "Octocopter +"),
    (2100, "Fixed wing", "Standard plane"),
    (3000, "Fixed wing", "Flying wing"),
    (13000, "VTOL", "Standard VTOL"),
    (13200, "VTOL", "Tailsitter VTOL"),
    (13100, "VTOL", "Tiltrotor VTOL"),
    (50000, "Rover", "Rover, differential"),
    (51000, "Rover", "Rover, Ackermann"),
];

/// ArduPilot picks the vehicle by firmware, so only the Copter has a frame to
/// choose here: `FRAME_CLASS` and `FRAME_TYPE`. Plane and Rover are set up on the
/// pages that follow. `(id, group, label, class, type)`.
const ARDUPILOT_COPTER_PRESETS: [(i64, &str, &str, i64, i64); 6] = [
    (101, "Multirotor", "Quadcopter X", 1, 1),
    (100, "Multirotor", "Quadcopter +", 1, 0),
    (201, "Multirotor", "Hexacopter X", 2, 1),
    (200, "Multirotor", "Hexacopter +", 2, 0),
    (301, "Multirotor", "Octocopter X", 3, 1),
    (300, "Multirotor", "Octocopter +", 3, 0),
];

/// The firmware a vehicle of this MAV_TYPE runs, or "" when none fits
/// (`ardupilot_firmware.vehicle_for_mav_type`): a helicopter is told apart because
/// its firmware is a separate image.
pub fn vehicle_for_mav_type(mav_type: i64) -> &'static str {
    if mav_type == 4 {
        return "Heli";
    }
    match vehicle_class(mav_type) {
        VehicleClass::Copter => "Copter",
        VehicleClass::Plane => "Plane",
        VehicleClass::Rover => "Rover",
        VehicleClass::Sub => "Sub",
        VehicleClass::Other => "",
    }
}

/// Every parameter the assessment reads on `stack`.
pub fn param_names(stack: &str) -> Vec<String> {
    let names: &[&str] = if stack == STACK_ARDUPILOT {
        &ARDUPILOT_PARAMS
    } else {
        &PX4_PARAMS
    };
    names.iter().map(|s| s.to_string()).collect()
}

/// The airframes offered for the connected `stack` and `vehicle` (the firmware
/// family, see [`vehicle_for_mav_type`]). PX4 offers the same list whatever it
/// reports, because a fresh board's heartbeat says nothing true about the airframe
/// it has not been given yet.
pub fn presets(stack: &str, vehicle: &str) -> Vec<Value> {
    if stack == STACK_ARDUPILOT {
        if vehicle == "Copter" {
            return ARDUPILOT_COPTER_PRESETS
                .iter()
                .map(|(id, group, label, _, _)| json!({"id": id, "group": group, "label": label}))
                .collect();
        }
        return Vec::new();
    }
    PX4_PRESETS
        .iter()
        .map(|(id, group, label)| json!({"id": id, "group": group, "label": label}))
        .collect()
}

/// The parameter writes that choose preset `preset_id`, or `None` if unknown. The
/// tables are the whole allow list: the endpoint never writes a value the browser
/// supplied, only one of these. PX4 gets `SYS_AUTOCONFIG = 1` after
/// `SYS_AUTOSTART`, the way QGroundControl does it: without it the board keeps the
/// previous airframe's parameter defaults and only the id changes.
pub fn writes_for(stack: &str, vehicle: &str, preset_id: i64) -> Option<Vec<(String, f64)>> {
    if stack == STACK_ARDUPILOT {
        if vehicle != "Copter" {
            return None;
        }
        return ARDUPILOT_COPTER_PRESETS.iter().find(|p| p.0 == preset_id).map(|p| {
            vec![
                ("FRAME_CLASS".to_string(), p.3 as f64),
                ("FRAME_TYPE".to_string(), p.4 as f64),
            ]
        });
    }
    PX4_PRESETS.iter().any(|p| p.0 == preset_id).then(|| {
        vec![
            ("SYS_AUTOSTART".to_string(), preset_id as f64),
            ("SYS_AUTOCONFIG".to_string(), 1.0),
        ]
    })
}

fn calibrated_by_id(values: &Values, name: &str) -> &'static str {
    match values.get(name) {
        None => UNKNOWN,
        Some(v) if round_int(*v) != 0 => DONE,
        Some(_) => TODO,
    }
}

fn calibrated_by_offsets(values: &Values, names: &[&str]) -> &'static str {
    let seen: Vec<f64> = names.iter().filter_map(|n| values.get(*n).copied()).collect();
    if seen.is_empty() {
        UNKNOWN
    } else if seen.iter().any(|v| v.abs() > 1e-9) {
        DONE
    } else {
        TODO
    }
}

/// The four facts the wizard's steps are marked from.
pub fn facts(stack: &str, values: &Values) -> Value {
    if stack == STACK_ARDUPILOT {
        let accel = calibrated_by_offsets(values, &["INS_ACCOFFS_X", "INS_ACCOFFS_Y", "INS_ACCOFFS_Z"]);
        let has_compass = values.get("COMPASS_DEV_ID").is_some_and(|v| round_int(*v) != 0)
            && values.get("COMPASS_USE").map_or(1, |v| round_int(*v)) != 0;
        let mag = if has_compass {
            calibrated_by_offsets(values, &["COMPASS_OFS_X", "COMPASS_OFS_Y", "COMPASS_OFS_Z"])
        } else if values.contains_key("COMPASS_DEV_ID") {
            NOT_REQUIRED
        } else {
            UNKNOWN
        };
        let airframe = match values.get("FRAME_CLASS") {
            None => UNKNOWN,
            Some(f) if round_int(*f) != 0 => DONE,
            Some(_) => TODO,
        };
        return json!({"airframe": airframe, "accel": accel, "gyro": NOT_REQUIRED, "mag": mag});
    }
    let airframe = match values.get("SYS_AUTOSTART") {
        None => UNKNOWN,
        Some(a) if round_int(*a) != 0 => DONE,
        Some(_) => TODO,
    };
    let mut mag = calibrated_by_id(values, "CAL_MAG0_ID");
    if values.get("SYS_HAS_MAG").is_some_and(|v| round_int(*v) == 0) {
        mag = NOT_REQUIRED;
    }
    json!({
        "airframe": airframe,
        "accel": calibrated_by_id(values, "CAL_ACC0_ID"),
        "gyro": calibrated_by_id(values, "CAL_GYRO0_ID"),
        "mag": mag,
    })
}

/// Judges one controller from the parameters read off it. A controller is fresh
/// when its airframe is unset (PX4), or when nothing it carries has ever been
/// calibrated. One missing calibration on an otherwise configured board is
/// "incomplete", shown in the wizard but not worth an unprompted dialog on every
/// connect.
pub fn assess(stack: &str, vehicle: &str, transport: &str, values: &Values) -> Value {
    let found = facts(stack, values);
    let mut reasons: Vec<&str> = Vec::new();
    if found["airframe"] == TODO && stack != STACK_ARDUPILOT {
        reasons.push("No airframe is selected.");
    }
    let judged: Vec<&str> = ["accel", "gyro", "mag"]
        .iter()
        .filter_map(|k| found[*k].as_str())
        .filter(|v| *v == DONE || *v == TODO)
        .collect();
    if !judged.is_empty() && judged.iter().all(|v| *v == TODO) {
        reasons.push("No sensor has been calibrated.");
    }
    let needs = !reasons.is_empty();
    json!({
        "stack": stack,
        "vehicle": vehicle,
        "transport": transport,
        "facts": found,
        "reasons": reasons,
        "needs_setup": needs,
        "prompt": needs && PHYSICAL_TRANSPORTS.contains(&transport),
        "presets": presets(stack, vehicle),
        "preset_current": current_preset(stack, values),
    })
}

/// The preset id the vehicle holds now, or `null` when it matches none.
fn current_preset(stack: &str, values: &Values) -> Value {
    if stack == STACK_ARDUPILOT {
        let (Some(class), Some(kind)) = (values.get("FRAME_CLASS"), values.get("FRAME_TYPE")) else {
            return Value::Null;
        };
        let pair = (round_int(*class), round_int(*kind));
        return ARDUPILOT_COPTER_PRESETS
            .iter()
            .find(|p| (p.3, p.4) == pair)
            .map_or(Value::Null, |p| json!(p.0));
    }
    match values.get("SYS_AUTOSTART") {
        Some(v) if round_int(*v) != 0 => json!(round_int(*v)),
        _ => Value::Null,
    }
}
