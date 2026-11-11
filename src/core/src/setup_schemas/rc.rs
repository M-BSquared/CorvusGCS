//! The PX4 radio control schema of Setup, Radio Control (`rc_config.py`).
//!
//! The page is split the way an operator sets a transmitter up: `input` (which
//! input the vehicle listens to and what it does when it stops arriving),
//! `sticks`, `modes` (the flight mode switch and the six modes its positions
//! select), `switches`, `aux` and `channels` (the per channel calibration
//! numbers).
//!
//! PX4 has no "calibrate RC" command: the ground station watches `RC_CHANNELS`
//! while the operator sweeps every control and writes the endpoints it observed.
//! [`calibration_writes`] is the one place that decides what a measurement is
//! allowed to become, and it is strict: a channel whose measured travel is
//! implausibly small is rejected by name rather than written, because
//! `min == max` would turn the stick into a step function.

use std::collections::HashSet;

use serde_json::{json, Value};

use super::fields::*;
use super::tables::{rc, Table};
use crate::json::Map;
use crate::protocol::common::{py_float_of, py_int_of, py_str, py_truthy};

pub const MAX_CHANNELS: i64 = 18;
pub const PWM_ABS_MIN: i64 = 500;
pub const PWM_ABS_MAX: i64 = 2500;
pub const MIN_TRAVEL_US: i64 = 200;
const REV_NORMAL: f64 = 1.0;
const REV_REVERSED: f64 = -1.0;
pub const CHANNEL_PARAM_SUFFIXES: [&str; 5] = ["MIN", "MAX", "TRIM", "DZ", "REV"];

fn tbl() -> Table {
    rc("px4")
}

/// A measured calibration that must not be written to the vehicle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalibrationError(pub String);

impl std::fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CalibrationError {}

fn fail<T>(text: impl Into<String>) -> Result<T, CalibrationError> {
    Err(CalibrationError(text.into()))
}

/// `max(1, min(int(count or 18), 18))`.
pub fn channel_limit(count: i64) -> i64 {
    let count = if count == 0 { MAX_CHANNELS } else { count };
    count.clamp(1, MAX_CHANNELS)
}

/// The `RC_MAP_*` option list: "Unassigned" plus one entry per channel.
pub fn channel_options(count: i64) -> Vec<Value> {
    let mut options = vec![opt(0, "Unassigned")];
    options.extend((1..=channel_limit(count)).map(|n| opt(n, &format!("Channel {n}"))));
    options
}

/// The five calibration parameters of one channel, in write order.
pub fn channel_params(channel: i64) -> Vec<String> {
    CHANNEL_PARAM_SUFFIXES
        .iter()
        .map(|s| format!("RC{channel}_{s}"))
        .collect()
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

fn map_fields_all() -> Vec<(String, String, String)> {
    let mut all = triples("STICK_FIELDS");
    for table in ["MODE_SWITCH_FIELDS", "SWITCH_FIELDS", "AUX_FIELDS"] {
        all.extend(triples(table));
    }
    all
}

/// Every parameter the page may need, in one flat list for one batched read.
pub fn param_names(channels: i64) -> Vec<String> {
    let mut names: Vec<String> = ["COM_RC_IN_MODE", "NAV_RCL_ACT", "RC_MAP_FAILSAFE"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    names.extend(
        tbl()
            .array("INPUT_NUMBER_FIELDS")
            .iter()
            .map(|t| t[0].as_str().unwrap_or("").to_string()),
    );
    names.extend(map_fields_all().into_iter().map(|t| t.0));
    names.extend(triples("MODE_SLOT_FIELDS").into_iter().map(|t| t.0));
    for channel in 1..=channel_limit(channels) {
        names.extend(channel_params(channel));
    }
    let mut seen = HashSet::new();
    names.retain(|n| seen.insert(n.clone()));
    names
}

fn map_fields(table: &[(String, String, String)], values: &Values, options: &[Value]) -> Vec<Value> {
    present(
        table
            .iter()
            .map(|(param, label, hint)| enum_(param, label, values, options).hint(hint).kv("role", "channel"))
            .collect(),
    )
}

fn make_section(id: &str, title: &str, hint: &str, fields: Vec<Value>) -> Option<Value> {
    if fields.is_empty() {
        return None;
    }
    Some(json!({"id": id, "title": title, "hint": hint, "kind": "fields", "fields": fields}))
}

fn input_section(values: &Values) -> Option<Value> {
    let mut modes = tbl().options("RC_IN_MODE_OPTIONS");
    if let Some(&held) = values.get("COM_RC_IN_MODE") {
        let held = round_int(held);
        modes.extend(
            tbl()
                .array("RC_IN_MODE_PRIORITY_OPTIONS")
                .iter()
                .filter(|o| o["value"].as_i64() == Some(held))
                .cloned(),
        );
    }
    let mut fields = present(vec![
        enum_("COM_RC_IN_MODE", "Accepted input", values, &modes)
            .hint("Which manual input the vehicle listens to at all."),
        enum_(
            "NAV_RCL_ACT",
            "Action on RC loss",
            values,
            &tbl().options("RC_LOSS_ACTION_OPTIONS"),
        )
        .hint("What the vehicle does when the transmitter goes unheard for longer than the timeout below."),
    ]);
    for item in tbl().array("INPUT_NUMBER_FIELDS") {
        let text = |i: usize| item[i].as_str().unwrap_or("");
        let mut f = number(text(0), text(1), values);
        if !text(2).is_empty() {
            f = f.unit(text(2));
        }
        if !text(3).is_empty() {
            f = f.hint(text(3));
        }
        fields.extend(f.into_value());
    }
    make_section(
        "input",
        "Input and failsafe",
        "What the vehicle accepts from a transmitter, and what it does when the transmitter stops arriving.",
        fields,
    )
}

fn modes_section(values: &Values, channels: i64) -> Option<Value> {
    let options = channel_options(channels);
    let mut fields = map_fields(&triples("MODE_SWITCH_FIELDS"), values, &options);
    let slots = tbl().options("FLIGHT_MODE_OPTIONS");
    fields.extend(present(
        triples("MODE_SLOT_FIELDS")
            .iter()
            .map(|(param, label, hint)| enum_(param, label, values, &slots).hint(hint).kv("role", "mode"))
            .collect(),
    ));
    make_section(
        "modes",
        "Flight mode settings",
        "One channel, six positions, one flight mode each. A position left unassigned keeps whatever mode the vehicle was already in.",
        fields,
    )
}

/// One channel's calibration numbers, or `None` when the firmware has none.
/// `calibrated` is the honest half of this row: a channel whose travel is
/// implausibly narrow has never actually been calibrated, and the page says so
/// rather than drawing a bar that looks configured.
pub(super) fn channel_row(
    channel: i64,
    values: &Values,
    suffixes: &[&str],
    rev: (&str, f64),
    reversed: impl Fn(f64) -> bool,
) -> Option<Value> {
    let mut fields: Vec<(String, f64)> = Vec::new();
    for suffix in suffixes {
        if let Some(&v) = values.get(&format!("RC{channel}_{suffix}")) {
            fields.push((suffix.to_lowercase(), v));
        }
    }
    if fields.is_empty() {
        return None;
    }
    let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| *v);
    let (minimum, maximum) = (get("min"), get("max"));
    let travel = match (minimum, maximum) {
        (Some(a), Some(b)) => Some((b - a).abs()),
        _ => None,
    };
    let num = |v: Option<f64>| v.map_or(Value::Null, crate::json::num);
    let mut params = Map::new();
    for suffix in suffixes {
        let name = format!("RC{channel}_{suffix}");
        if values.contains_key(&name) {
            params.insert(suffix.to_lowercase(), json!(name));
        }
    }
    Some(json!({
        "channel": channel,
        "min": num(minimum),
        "max": num(maximum),
        "trim": num(get("trim")),
        "dz": num(get("dz")),
        "reversed": reversed(get(rev.0).unwrap_or(rev.1)),
        "has_rev": get(rev.0).is_some(),
        "travel": num(travel),
        "calibrated": travel.is_some_and(|t| t >= MIN_TRAVEL_US as f64),
        "params": params,
    }))
}

pub(super) fn channels_section(values: &Values, channels: i64, rows: impl Fn(i64) -> Option<Value>) -> Option<Value> {
    let rows: Vec<Value> = (1..=channel_limit(channels)).filter_map(rows).collect();
    if rows.is_empty() {
        return None;
    }
    let _ = values;
    Some(json!({
        "id": "channels", "title": "Channel calibration", "kind": "channels",
        "hint": "The endpoints and centre the calibration measured. Every value here is editable, but the wizard is the reliable way to set them.",
        "rows": rows,
    }))
}

fn assignments(values: &Values) -> Value {
    let mut out = Map::new();
    for (param, _, _) in map_fields_all() {
        if let Some(&v) = values.get(&param) {
            out.insert(param, json!(round_int(v)));
        }
    }
    Value::Object(out)
}

/// Turns the raw `{param: value}` into the Radio Control page description
/// (`GET /api/rc`): `sections` (`kind` is `"fields"` for a schema driven form and
/// `"channels"` for the calibration table), `assignments` (`{RC_MAP_*: channel}`,
/// so the page can flag a channel bound to two actions at once), `channel_limit`
/// and `received`. `channels` caps the pickers and the table at what the receiver
/// actually delivers.
pub fn build(values: &Values, channels: i64) -> Value {
    let options = channel_options(channels);
    let sections: Vec<Value> = [
        input_section(values),
        make_section(
            "sticks",
            "Stick channels",
            "Which channel carries each axis. The calibration sets these for you; change one here only to correct a transmitter you re-ordered.",
            map_fields(&triples("STICK_FIELDS"), values, &options),
        ),
        modes_section(values, channels),
        make_section(
            "switches",
            "Switch settings",
            "Every other action PX4 can bind to a switch. Unassigned means the action is unavailable from the transmitter.",
            map_fields(&triples("SWITCH_FIELDS"), values, &options),
        ),
        make_section(
            "aux",
            "Auxiliary channels and knobs",
            "Channels passed through to an actuator, a payload, or a tuning knob.",
            map_fields(&triples("AUX_FIELDS"), values, &options),
        ),
        channels_section(values, channels, |n| {
            channel_row(n, values, &CHANNEL_PARAM_SUFFIXES, ("rev", REV_NORMAL), |v| v < 0.0)
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    json!({
        "sections": sections,
        "assignments": assignments(values),
        "channel_limit": channel_limit(channels),
        "received": values.len(),
    })
}

// ---------------------------------------------------------------------------
// Calibration
// ---------------------------------------------------------------------------

/// A validated measured channel.
#[derive(Debug, Clone)]
pub struct Measured {
    pub channel: i64,
    pub min: i64,
    pub max: i64,
    pub trim: i64,
    pub reversed: bool,
    pub set_reverse: bool,
}

/// Validates one measured channel, returning it normalised. What makes a
/// measurement implausible is a fact about receivers, not about firmware, so the
/// ArduPilot twin shares this validator.
pub fn validate_channel(entry: &Value) -> Result<Measured, CalibrationError> {
    let Some(obj) = entry.as_object() else {
        return fail("each channel must be an object");
    };
    let numeric = || -> Option<(i64, i64, i64, i64)> {
        let rounded = |k: &str| py_float_of(obj.get(k)?).filter(|f| f.is_finite()).map(round_int);
        Some((
            py_int_of(obj.get("channel")?)?,
            rounded("min")?,
            rounded("max")?,
            rounded("trim")?,
        ))
    };
    let Some((channel, minimum, maximum, trim)) = numeric() else {
        return fail("each channel needs numeric channel, min, max and trim");
    };
    if !(1..=MAX_CHANNELS).contains(&channel) {
        return fail(format!("channel {channel} is outside 1-{MAX_CHANNELS}"));
    }
    for (label, value) in [("min", minimum), ("max", maximum), ("trim", trim)] {
        if !(PWM_ABS_MIN..=PWM_ABS_MAX).contains(&value) {
            return fail(format!(
                "channel {channel} {label} of {value} us is outside {PWM_ABS_MIN}-{PWM_ABS_MAX} us"
            ));
        }
    }
    if maximum - minimum < MIN_TRAVEL_US {
        return fail(format!(
            "channel {channel} moved only {} us. Move every stick and switch through its full travel and measure again",
            maximum - minimum
        ));
    }
    if !(minimum..=maximum).contains(&trim) {
        return fail(format!(
            "channel {channel} centre of {trim} us is outside its own {minimum}-{maximum} us travel"
        ));
    }
    Ok(Measured {
        channel,
        min: minimum,
        max: maximum,
        trim,
        reversed: obj.get("reversed").is_some_and(py_truthy),
        set_reverse: obj.get("set_reverse").map_or(obj.contains_key("reversed"), py_truthy),
    })
}

/// The `{RC_MAP_*: channel}` half of a measured calibration, limited to the
/// stick maps the wizard has evidence for (a kill switch is bound by hand).
pub fn validate_mapping_with(
    mapping: Option<&Value>,
    known: Option<&HashSet<String>>,
    calibratable: &[String],
) -> Result<Vec<Value>, CalibrationError> {
    let Some(mapping) = mapping.filter(|m| !m.is_null()) else {
        return Ok(Vec::new());
    };
    let Some(map) = mapping.as_object() else {
        return fail("mapping must be an object");
    };
    let mut names: Vec<&String> = map.keys().collect();
    names.sort();
    let mut writes = Vec::new();
    for name in names {
        if !calibratable.contains(name) {
            return fail(format!("{name} is not set by the radio calibration"));
        }
        let value = &map[name];
        let Value::Number(n) = value else {
            return fail(format!("{name} must be a channel number"));
        };
        let f = n.as_f64().unwrap_or(f64::NAN);
        let channel = f.trunc();
        if channel != f || !(1.0..=MAX_CHANNELS as f64).contains(&channel) {
            return fail(format!("{name} channel {} is outside 1-{MAX_CHANNELS}", py_str(value)));
        }
        if known.is_none_or(|k| k.contains(name)) {
            writes.push(json!({"name": name, "value": channel}));
        }
    }
    Ok(writes)
}

/// Turns a measured calibration into the ordered parameter write list. `count`
/// is written to `RC_CHAN_CNT` when given; `known`, when given, is the set of
/// parameter names the firmware answered for, and a write outside it is dropped
/// rather than sent. Never returns a partial list: a rejected calibration leaves
/// the vehicle exactly as it was, which is the only safe outcome.
pub fn calibration_writes(
    channels: &Value,
    count: Option<&Value>,
    known: Option<&HashSet<String>>,
    mapping: Option<&Value>,
) -> Result<Vec<Value>, CalibrationError> {
    let list = match channels.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return fail("no channels were measured"),
    };
    if list.len() as i64 > MAX_CHANNELS {
        return fail(format!("at most {MAX_CHANNELS} channels can be calibrated"));
    }
    let validated: Vec<Measured> = list.iter().map(validate_channel).collect::<Result<_, _>>()?;
    let calibratable: Vec<String> = triples("STICK_FIELDS").into_iter().map(|t| t.0).collect();
    let map_writes = validate_mapping_with(mapping, known, &calibratable)?;
    let mut seen = HashSet::new();
    for entry in &validated {
        if !seen.insert(entry.channel) {
            return fail(format!("channel {} was measured twice", entry.channel));
        }
    }
    let allowed = |name: &str| known.is_none_or(|k| k.contains(name));

    let mut writes: Vec<Value> = Vec::new();
    let mut sorted = validated;
    sorted.sort_by_key(|e| e.channel);
    for entry in &sorted {
        let c = entry.channel;
        let mut pairs = vec![
            (format!("RC{c}_MIN"), entry.min as f64),
            (format!("RC{c}_MAX"), entry.max as f64),
            (format!("RC{c}_TRIM"), entry.trim as f64),
        ];
        if entry.set_reverse {
            pairs.push((
                format!("RC{c}_REV"),
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
    if let Some(count) = count {
        let Some(resolved) = py_int_of(count) else {
            return fail("channel count must be a number");
        };
        if !(1..=MAX_CHANNELS).contains(&resolved) {
            return fail(format!("channel count {resolved} is outside 1-{MAX_CHANNELS}"));
        }
        if allowed("RC_CHAN_CNT") {
            writes.push(json!({"name": "RC_CHAN_CNT", "value": resolved as f64}));
        }
    }
    // Endpoints first, mapping last: a channel pointed at a stick before its
    // travel is known is briefly a stick with PX4's defaults on the wrong channel.
    writes.extend(map_writes);
    if writes.is_empty() {
        return fail("the connected firmware has none of the calibration parameters");
    }
    Ok(writes)
}
