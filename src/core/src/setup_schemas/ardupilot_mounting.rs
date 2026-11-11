//! The ArduPilot sensor mounting schema (`ardupilot_mounting.py`): the twin of
//! [`super::mounting`], with the same two halves and the same output shape, so
//! neither page can tell which stack built it.
//!
//! On ArduPilot 4.3 to 4.6, `INS_POS1_*` to `INS_POS3_*` exist on all four; the
//! GPS antenna was renamed in 4.6 (`GPS_POS1_X` became `GPS1_POS_X`, and
//! `GPS_TYPE2` became `GPS2_TYPE`), both spellings are candidates and the 4.6 one
//! wins. ArduPilot keeps a position per IMU, so the flight controller's fields
//! write `INS_POS1_*` and carry the matching `INS_POS2_*` and `INS_POS3_*` in
//! `also`, but only an IMU that currently agrees with the first one: one that was
//! given a position of its own (an IMU on a separate board) is left as it is.

use serde_json::{json, Value};

use super::fields::*;
use super::mounting::{axis_label, gps_hint, orientation_hint, position_hint, AXES};
use super::tables::mounting as tbl;

const IMU_COUNT: i64 = 3;

/// `AHRS_ORIENTATION` carries the MAV_SENSOR_ORIENTATION table plus a few of its
/// own (41 is internal and never a parameter value).
pub fn ahrs_rotation_options() -> Vec<Value> {
    tbl().options("AHRS_ROTATION_OPTIONS")
}

fn imu_names(imu: i64) -> Vec<String> {
    AXES.iter().map(|a| format!("INS_POS{imu}_{a}")).collect()
}

fn gps_names(receiver: i64, legacy: bool) -> Vec<String> {
    AXES.iter()
        .map(|a| {
            if legacy {
                format!("GPS_POS{receiver}_{a}")
            } else {
                format!("GPS{receiver}_POS_{a}")
            }
        })
        .collect()
}

pub fn orientation_param_names() -> Vec<String> {
    ["AHRS_ORIENTATION", "AHRS_TRIM_X", "AHRS_TRIM_Y"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The lever arm parameters alone: what the Motors page adds to its read.
pub fn position_param_names() -> Vec<String> {
    let mut names = Vec::new();
    for imu in 1..=IMU_COUNT {
        names.extend(imu_names(imu));
    }
    for receiver in [1, 2] {
        names.extend(gps_names(receiver, true));
        names.extend(gps_names(receiver, false));
    }
    names.push("GPS_TYPE2".to_string());
    names.push("GPS2_TYPE".to_string());
    names
}

/// Every parameter the mounting description may need, in one flat list.
pub fn param_names() -> Vec<String> {
    let mut names = orientation_param_names();
    names.extend(position_param_names());
    names
}

/// The board rotation and the level trims, or `None` when neither answered.
pub fn orientation(values: &Values) -> Option<Value> {
    let fields = present(vec![
        enum_("AHRS_ORIENTATION", "Flight controller rotation", values, &ahrs_rotation_options())
            .kv("reboot", true)
            .hint("How the board is turned relative to the airframe, when its arrow does not point forward or it is mounted upside down."),
        number("AHRS_TRIM_X", "Roll trim", values).unit("rad").step(0.001).min(-0.1745).max(0.1745),
        number("AHRS_TRIM_Y", "Pitch trim", values)
            .unit("rad")
            .step(0.001)
            .min(-0.1745)
            .max(0.1745)
            .hint("The level calibration sets both trims. 0.1745 rad is 10\u{b0}, the most ArduPilot accepts: anything larger is a rotation."),
    ]);
    if fields.is_empty() {
        return None;
    }
    Some(json!({"fields": fields, "hint": orientation_hint()}))
}

fn axis_field(name: &str, axis: &str, values: &Values, also: &[String]) -> Field {
    let f = number(name, axis_label(axis), values)
        .unit("m")
        .step(0.001)
        .min(-5)
        .max(5);
    if also.is_empty() {
        f
    } else {
        f.kv("also", also.to_vec())
    }
}

/// The further IMUs whose position on `axis` agrees with the first IMU's.
fn matching_imus(values: &Values, axis: &str) -> Vec<String> {
    let Some(&first) = values.get(&format!("INS_POS1_{axis}")) else {
        return Vec::new();
    };
    (2..=IMU_COUNT)
        .map(|imu| format!("INS_POS{imu}_{axis}"))
        .filter(|name| values.get(name).is_some_and(|v| (v - first).abs() < 1e-6))
        .collect()
}

fn flight_controller(values: &Values) -> Option<Value> {
    let fields = present(
        AXES.iter()
            .map(|a| axis_field(&format!("INS_POS1_{a}"), a, values, &matching_imus(values, a)))
            .collect(),
    );
    if fields.is_empty() {
        return None;
    }
    let at = |axis: &str| crate::json::num(values.get(&format!("INS_POS1_{axis}")).copied().unwrap_or(0.0));
    Some(json!({
        "id": "fc", "label": "Flight controller", "tag": "FC", "kind": "fc",
        "x": at("X"), "y": at("Y"), "z": at("Z"),
        "fields": fields,
        "hint": "Where the IMUs sit. Every IMU of the board that shares the first one's position is written with it. Keep the board close to the centre of gravity: every centimetre off it adds acceleration from turning to what the accelerometer measures.",
    }))
}

fn gps(values: &Values, receiver: i64, label: &str, tag: &str, hint: &str) -> Option<Value> {
    let mut names = gps_names(receiver, false);
    if !names.iter().any(|n| values.contains_key(n)) {
        names = gps_names(receiver, true);
    }
    let fields = present(
        names
            .iter()
            .zip(AXES)
            .map(|(name, axis)| axis_field(name, axis, values, &[]))
            .collect(),
    );
    if fields.is_empty() {
        return None;
    }
    let at = |i: usize| crate::json::num(values.get(&names[i]).copied().unwrap_or(0.0));
    let mut out = json!({
        "id": format!("gps{receiver}"), "label": label, "tag": tag, "kind": "gps",
        "x": at(0), "y": at(1), "z": at(2),
        "fields": fields,
    });
    if !hint.is_empty() {
        out["hint"] = json!(hint);
    }
    Some(out)
}

fn second_receiver(values: &Values) -> bool {
    let kind = values
        .get("GPS2_TYPE")
        .or_else(|| values.get("GPS_TYPE2"))
        .copied()
        .unwrap_or(0.0);
    round_int(kind) != 0
}

/// The flight controller and every configured GPS antenna.
pub fn positions(values: &Values) -> Vec<Value> {
    let mut sensors = vec![
        flight_controller(values),
        gps(values, 1, "GPS antenna", "GPS", gps_hint()),
    ];
    if second_receiver(values) {
        sensors.push(gps(values, 2, "GPS 2 antenna", "GPS 2", ""));
    }
    sensors.into_iter().flatten().collect()
}

/// The same shape as the PX4 description.
pub fn build(values: &Values) -> Value {
    json!({
        "orientation": orientation(values).unwrap_or(Value::Null),
        "positions": positions(values),
        "position_hint": position_hint(),
        "received": values.len(),
    })
}
