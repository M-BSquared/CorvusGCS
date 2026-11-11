//! The PX4 sensor mounting schema (`mounting_config.py`): how the flight
//! controller is turned, and where the flight controller and the GPS antenna sit
//! on the airframe.
//!
//! Two different things, which is why [`build`] returns them apart and two pages
//! show them. `orientation` is the rotation of the board (`SENS_BOARD_ROT`) and
//! the fine trims the level calibration writes; every accelerometer and compass
//! calibration is measured through it. `positions` are lever arms in metres, body
//! frame, which the Motors page draws on the same airframe.
//!
//! On PX4 v1.16 to v1.18 `EKF2_IMU_POS_*` exists on all three; the GPS antenna
//! moved in v1.18 from `EKF2_GPS_POS_X/Y/Z` to the per receiver `SENS_GPS0_OFFX/Y/Z`
//! and `SENS_GPS1_OFFX/Y/Z`. Both sets are candidates and the per receiver one
//! wins when a vehicle answers for both.

use serde_json::{json, Value};

use super::fields::*;
use super::tables::mounting as tbl;

pub const AXES: [&str; 3] = ["X", "Y", "Z"];

pub fn axis_label(axis: &str) -> &'static str {
    tbl().text("AXIS_LABELS", axis)
}

pub fn position_hint() -> &'static str {
    tbl().get("POSITION_HINT").as_str().unwrap_or("")
}

pub fn orientation_hint() -> &'static str {
    tbl().get("ORIENTATION_HINT").as_str().unwrap_or("")
}

pub fn gps_hint() -> &'static str {
    tbl().get("GPS_HINT").as_str().unwrap_or("")
}

/// MAV_SENSOR_ORIENTATION, the numbering of both `SENS_BOARD_ROT` and
/// `AHRS_ORIENTATION`.
pub fn rotation_options() -> Vec<Value> {
    tbl().options("ROTATION_OPTIONS")
}

fn imu_names() -> Vec<String> {
    AXES.iter().map(|a| format!("EKF2_IMU_POS_{a}")).collect()
}

fn gps_names() -> Vec<String> {
    let mut names: Vec<String> = AXES.iter().map(|a| format!("EKF2_GPS_POS_{a}")).collect();
    for receiver in [0, 1] {
        names.extend(AXES.iter().map(|a| format!("SENS_GPS{receiver}_OFF{a}")));
    }
    // Whether a second receiver is configured at all: a serial one has a port, a
    // DroneCAN one is matched by device ID.
    names.push("SENS_GPS1_ID".to_string());
    names.push("GPS_2_CONFIG".to_string());
    names
}

/// Every parameter the mounting description may need, in one flat list.
pub fn param_names() -> Vec<String> {
    let mut names = orientation_param_names();
    names.extend(position_param_names());
    names
}

pub fn orientation_param_names() -> Vec<String> {
    [
        "SENS_BOARD_ROT",
        "SENS_BOARD_X_OFF",
        "SENS_BOARD_Y_OFF",
        "SENS_BOARD_Z_OFF",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// The lever arm parameters alone: what the Motors page adds to its read.
pub fn position_param_names() -> Vec<String> {
    let mut names = imu_names();
    names.extend(gps_names());
    names
}

/// The board rotation and its fine trims, or `None` when the vehicle has neither.
pub fn orientation(values: &Values) -> Option<Value> {
    let fields = present(vec![
        enum_("SENS_BOARD_ROT", "Flight controller rotation", values, &rotation_options())
            .kv("reboot", true)
            .hint("How the board is turned relative to the airframe, when its arrow does not point forward or it is mounted upside down."),
        number("SENS_BOARD_X_OFF", "Roll trim", values).unit("deg").step(0.1),
        number("SENS_BOARD_Y_OFF", "Pitch trim", values)
            .unit("deg")
            .step(0.1)
            .hint("The level horizon calibration sets roll and pitch trim. A few degrees at most: anything larger is a rotation, not a trim."),
        number("SENS_BOARD_Z_OFF", "Yaw trim", values).unit("deg").step(0.1),
    ]);
    if fields.is_empty() {
        return None;
    }
    Some(json!({"fields": fields, "hint": orientation_hint()}))
}

fn axis_fields(values: &Values, names: &[String]) -> Vec<Value> {
    present(
        names
            .iter()
            .zip(AXES)
            .map(|(name, axis)| {
                number(name, axis_label(axis), values)
                    .unit("m")
                    .step(0.001)
                    .min(-5)
                    .max(5)
            })
            .collect(),
    )
}

/// One placed sensor: its position for the drawing and its fields to edit.
/// `kind` picks the marker the drawing uses (`fc` or `gps`), `tag` is the few
/// letters written beside it.
fn sensor(id: &str, label: &str, tag: &str, values: &Values, names: &[String], hint: &str) -> Option<Value> {
    let fields = axis_fields(values, names);
    if fields.is_empty() {
        return None;
    }
    let at = |i: usize| crate::json::num(values.get(&names[i]).copied().unwrap_or(0.0));
    let mut out = json!({
        "id": id, "label": label, "tag": tag,
        "kind": if id == "fc" { "fc" } else { "gps" },
        "x": at(0), "y": at(1), "z": at(2), "fields": fields,
    });
    if !hint.is_empty() {
        out["hint"] = json!(hint);
    }
    Some(out)
}

/// Whether a second GPS is configured, or already has an offset of its own.
fn second_receiver(values: &Values) -> bool {
    let rounded = |n: &str| round_int(values.get(n).copied().unwrap_or(0.0));
    if rounded("SENS_GPS1_ID") != 0 || rounded("GPS_2_CONFIG") != 0 {
        return true;
    }
    AXES.iter()
        .any(|a| values.get(&format!("SENS_GPS1_OFF{a}")).copied().unwrap_or(0.0).abs() > 1e-9)
}

/// The flight controller and every GPS antenna the firmware has an offset for.
pub fn positions(values: &Values) -> Vec<Value> {
    let mut sensors = vec![sensor(
        "fc",
        "Flight controller",
        "FC",
        values,
        &imu_names(),
        "Where the IMU sits. Keep it close to the centre of gravity: every centimetre off it adds acceleration from turning to what the accelerometer measures.",
    )];
    let per_receiver: Vec<String> = AXES.iter().map(|a| format!("SENS_GPS0_OFF{a}")).collect();
    if per_receiver.iter().any(|n| values.contains_key(n)) {
        sensors.push(sensor("gps1", "GPS antenna", "GPS", values, &per_receiver, gps_hint()));
        if second_receiver(values) {
            let second: Vec<String> = AXES.iter().map(|a| format!("SENS_GPS1_OFF{a}")).collect();
            sensors.push(sensor("gps2", "GPS 2 antenna", "GPS 2", values, &second, ""));
        }
    } else {
        let legacy: Vec<String> = AXES.iter().map(|a| format!("EKF2_GPS_POS_{a}")).collect();
        sensors.push(sensor("gps1", "GPS antenna", "GPS", values, &legacy, gps_hint()));
    }
    sensors.into_iter().flatten().collect()
}

/// Turns the raw `{param: value}` into the mounting description: `orientation`
/// (`{fields, hint}` or null: the board rotation, for the Calibration page),
/// `positions` (one entry per placed sensor, for the Motors page),
/// `position_hint` and `received`.
pub fn build(values: &Values) -> Value {
    json!({
        "orientation": orientation(values).unwrap_or(Value::Null),
        "positions": positions(values),
        "position_hint": position_hint(),
        "received": values.len(),
    })
}
