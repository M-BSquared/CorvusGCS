//! The ArduPilot radio control schema of Setup, Radio Control
//! (`ardupilot_rc.py`): the twin of [`super::rc`]. Three things do not transfer
//! and each fails silently rather than loudly: the reversal is `RC<n>_REVERSED`
//! (a 0/1 flag) and not `RC<n>_REV` (the sign of a float); the stick mapping is
//! `RCMAP_*`; and flight modes are numbers that differ per vehicle, so the option
//! list is built from the dialect's mode table for the connected airframe.

use std::collections::HashSet;

use serde_json::{json, Value};

use super::fields::*;
use super::rc::{
    channel_limit, channel_options, channel_row, channels_section, validate_channel, validate_mapping_with,
    CalibrationError, Measured, MAX_CHANNELS,
};
use super::tables::{rc, Table};
use crate::dialect::{dialect_for_stack, Stack};
use crate::protocol::common::py_int_of;

const REV_NORMAL: f64 = 0.0;
const REV_REVERSED: f64 = 1.0;
const CHANNEL_PARAM_SUFFIXES: [&str; 5] = ["MIN", "MAX", "TRIM", "DZ", "REVERSED"];
/// Channels 1 to 4 are the sticks and are deliberately left out: binding a
/// function to the throttle channel is not a configuration, it is an accident.
const AUX_CHANNELS: std::ops::RangeInclusive<i64> = 5..=16;

fn tbl() -> Table {
    rc("ardupilot")
}

fn triples(name: &str) -> Vec<(String, String, String)> {
    tbl()
        .array(name)
        .iter()
        .map(|t| {
            let at = |i: usize| t.get(i).and_then(Value::as_str).unwrap_or("").to_string();
            (at(0), at(1), at(2))
        })
        .collect()
}

/// Python's `str.title()` for the ASCII names of the mode tables.
fn title(text: &str) -> String {
    let mut out = String::new();
    let mut previous_letter = false;
    for c in text.chars() {
        if c.is_alphabetic() {
            if previous_letter {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            previous_letter = true;
        } else {
            out.push(c);
            previous_letter = false;
        }
    }
    out
}

/// The `FLTMODE1` to `FLTMODE6` option list for this airframe, from the same
/// per vehicle table the mode selector and the heartbeat decoder use, so the
/// number a mode switch writes and the name the status bar shows cannot disagree.
pub fn flight_mode_options(vehicle_type: i64) -> Vec<Value> {
    let mut numbered: Vec<(i64, String)> = dialect_for_stack(Stack::ArduPilot)
        .mode_table(vehicle_type, None)
        .into_iter()
        .map(|(name, cmd)| (cmd.custom_mode, name))
        .collect();
    numbered.sort_by_key(|(n, _)| *n);
    numbered
        .iter()
        .map(|(n, name)| opt(*n, &title(&name.replace('_', " "))))
        .collect()
}

/// Every parameter the page may need, in one flat list.
pub fn param_names(channels: i64) -> Vec<String> {
    let limit = channel_limit(channels);
    let mut names: Vec<String> = [
        "RC_OPTIONS",
        "RC_PROTOCOLS",
        "RC_OVERRIDE_TIME",
        "RC_FS_TIMEOUT",
        "THR_FAILSAFE",
        "THR_FS_VALUE",
        "FS_THR_ENABLE",
        "FS_THR_VALUE",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for table in ["STICK_FIELDS", "MODE_SWITCH_FIELDS", "MODE_SLOT_FIELDS"] {
        names.extend(triples(table).into_iter().map(|t| t.0));
    }
    for n in 1..=limit {
        names.extend(CHANNEL_PARAM_SUFFIXES.iter().map(|s| format!("RC{n}_{s}")));
    }
    for n in AUX_CHANNELS {
        if n <= limit {
            names.push(format!("RC{n}_OPTION"));
        }
    }
    let mut seen = HashSet::new();
    names.retain(|n| seen.insert(n.clone()));
    names
}

fn section(id: &str, title: &str, hint: &str, fields: Vec<Value>) -> Option<Value> {
    if fields.is_empty() {
        return None;
    }
    Some(json!({"id": id, "title": title, "kind": "fields", "fields": fields, "hint": hint}))
}

fn map_fields(table: &[(String, String, String)], values: &Values, channels: i64) -> Vec<Value> {
    let options = channel_options(channels);
    present(
        table
            .iter()
            .map(|(param, label, hint)| enum_(param, label, values, &options).hint(hint))
            .collect(),
    )
}

fn input_section(values: &Values) -> Option<Value> {
    let fields = present(vec![
        number("RC_PROTOCOLS", "Receiver protocols", values).step(1).hint(
            "A bitmask of the protocols the autopilot will listen for. 1 accepts all of them and is the usual answer.",
        ),
        number("RC_FS_TIMEOUT", "RC loss timeout", values)
            .unit("s")
            .step(0.1)
            .min(0),
        number("RC_OVERRIDE_TIME", "Override timeout", values)
            .unit("s")
            .step(0.1)
            .min(0)
            .hint("How long a ground-station stick override stays in force before the transmitter takes over again."),
        number("RC_OPTIONS", "Receiver options", values).step(1),
        enum_(
            "THR_FAILSAFE",
            "Throttle failsafe",
            values,
            &tbl().options("ON_OFF_OPTIONS"),
        ),
        number("THR_FS_VALUE", "Throttle failsafe threshold", values)
            .unit("PWM")
            .step(1),
        number("FS_THR_VALUE", "RC-loss throttle threshold", values)
            .unit("PWM")
            .step(1)
            .hint("A throttle channel below this counts as a lost receiver."),
    ]);
    section(
        "input",
        "Receiver",
        "Which input the vehicle listens to, and what it does when that input stops arriving.",
        fields,
    )
}

fn modes_section(values: &Values, channels: i64, vehicle_type: i64) -> Option<Value> {
    let mut fields = map_fields(&triples("MODE_SWITCH_FIELDS"), values, channels);
    let modes = flight_mode_options(vehicle_type);
    fields.extend(present(
        triples("MODE_SLOT_FIELDS")
            .iter()
            .map(|(param, label, hint)| enum_(param, label, values, &modes).hint(hint))
            .collect(),
    ));
    section(
        "modes",
        "Flight mode settings",
        "The six modes the mode switch selects. ArduPilot fixes the PWM band of each position; only the mode in it is yours to choose.",
        fields,
    )
}

fn aux_section(values: &Values, channels: i64) -> Option<Value> {
    let limit = channel_limit(channels);
    let options = tbl().options("RC_OPTION_VALUES");
    let fields = present(
        AUX_CHANNELS
            .filter(|n| *n <= limit)
            .map(|n| enum_(&format!("RC{n}_OPTION"), &format!("Channel {n}"), values, &options))
            .collect(),
    );
    section(
        "switches",
        "Switch settings",
        "ArduPilot binds a function to a channel rather than a channel to a function, so every auxiliary switch is one of these.",
        fields,
    )
}

fn assignments(values: &Values) -> Value {
    let mut out = crate::json::Map::new();
    for table in ["STICK_FIELDS", "MODE_SWITCH_FIELDS"] {
        for (param, _, _) in triples(table) {
            if let Some(&v) = values.get(&param) {
                out.insert(param, json!(round_int(v)));
            }
        }
    }
    Value::Object(out)
}

/// Turns the raw `{param: value}` into the Radio Control page description, the
/// same shape as the PX4 one. `vehicle_type` is the MAV_TYPE, needed because
/// ArduPilot's flight mode numbers mean different things on different airframes.
pub fn build(values: &Values, channels: i64, vehicle_type: i64) -> Value {
    let sections: Vec<Value> = [
        input_section(values),
        section(
            "sticks",
            "Sticks",
            "Which channel carries each stick axis. The calibration wizard sets these from the stick it asks you to move.",
            map_fields(&triples("STICK_FIELDS"), values, channels),
        ),
        modes_section(values, channels, vehicle_type),
        aux_section(values, channels),
        channels_section(values, channels, |n| {
            channel_row(n, values, &CHANNEL_PARAM_SUFFIXES, ("reversed", REV_NORMAL), |v| v != 0.0)
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    json!({
        "stack": "ardupilot",
        "sections": sections,
        "assignments": assignments(values),
        "channel_limit": channel_limit(channels),
        "received": values.len(),
    })
}

/// The ArduPilot twin of [`super::rc::calibration_writes`]: the same validation,
/// and two differences on the wire. The reversal goes to `RC<n>_REVERSED` as a 0/1
/// flag, and there is no channel count parameter to write (`count` is accepted
/// and ignored so the two stacks share one call site).
pub fn calibration_writes(
    channels: &Value,
    count: Option<&Value>,
    known: Option<&HashSet<String>>,
    mapping: Option<&Value>,
) -> Result<Vec<Value>, CalibrationError> {
    let _ = count.and_then(py_int_of);
    let list = match channels.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return Err(CalibrationError("no channels were measured".into())),
    };
    if list.len() as i64 > MAX_CHANNELS {
        return Err(CalibrationError(format!(
            "at most {MAX_CHANNELS} channels can be calibrated"
        )));
    }
    let validated: Vec<Measured> = list.iter().map(validate_channel).collect::<Result<_, _>>()?;
    let calibratable: Vec<String> = triples("STICK_FIELDS").into_iter().map(|t| t.0).collect();
    let map_writes = validate_mapping_with(mapping, known, &calibratable)?;
    let mut seen = HashSet::new();
    for entry in &validated {
        if !seen.insert(entry.channel) {
            return Err(CalibrationError(format!(
                "channel {} was measured twice",
                entry.channel
            )));
        }
    }
    let allowed = |name: &str| known.is_none_or(|k| k.contains(name));
    let mut sorted = validated;
    sorted.sort_by_key(|e| e.channel);
    let mut writes: Vec<Value> = Vec::new();
    for entry in &sorted {
        let c = entry.channel;
        let mut pairs = vec![
            (format!("RC{c}_MIN"), entry.min as f64),
            (format!("RC{c}_MAX"), entry.max as f64),
            (format!("RC{c}_TRIM"), entry.trim as f64),
        ];
        if entry.set_reverse {
            pairs.push((
                format!("RC{c}_REVERSED"),
                if entry.reversed { REV_REVERSED } else { REV_NORMAL },
            ));
        }
        writes.extend(
            pairs
                .into_iter()
                .filter(|(n, _)| allowed(n))
                .map(|(n, v)| json!({"name": n, "value": v})),
        );
    }
    writes.extend(map_writes);
    if writes.is_empty() {
        return Err(CalibrationError(
            "the connected firmware has none of the calibration parameters".into(),
        ));
    }
    Ok(writes)
}
