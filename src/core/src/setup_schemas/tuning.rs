//! The PX4 PID tuning schema of Setup, PID Tuning (`tuning_config.py`).
//!
//! The page is split the way the controller itself is: one group per loop,
//! innermost first (`rate`, `attitude`, `velocity`, `position`, `autotune`). A
//! field whose parameter the firmware does not answer for is dropped, a section
//! with no surviving field is dropped, and a group with no surviving section is
//! dropped, which is what makes one schema serve a multicopter and a fixed wing,
//! and PX4 v1.16 to v1.18, without a version switch.
//!
//! PX4 runs the autotune in flight, so the preconditions travel with the schema
//! and the page states them before the operator takes off.

use serde_json::{json, Value};

use super::fields::*;
use super::tables::{tuning, Table};

/// MAV_LANDED_STATE: 0 means the firmware does not publish it, which is never
/// treated as "on the ground".
pub const LANDED_STATE_ON_GROUND: i64 = 1;
pub const LANDED_STATE_IN_AIR: i64 = 2;

fn tbl() -> Table {
    tuning("px4")
}

/// How one controller is tuned, for the button beside it: `run` when the
/// autotune tunes this loop, `with` when it tunes it as part of another loop's
/// run (named by `together_with`), `manual` when no autotune covers it.
pub(super) fn tune(mode: &str, text: &str, together_with: &str) -> Value {
    let mut out = json!({"mode": mode, "text": text});
    if !together_with.is_empty() {
        out["with"] = json!(together_with);
    }
    out
}

pub(super) fn text_row(item: &Value, i: usize) -> &str {
    item.get(i).and_then(Value::as_str).unwrap_or("")
}

/// Every parameter the page may need, in one flat list for one batched read.
pub fn param_names() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for table in [
        "MC_RATE_SECTIONS",
        "MC_ATTITUDE_SECTIONS",
        "MC_VELOCITY_SECTIONS",
        "MC_POSITION_SECTIONS",
        "FW_RATE_SECTIONS",
        "FW_ATTITUDE_SECTIONS",
    ] {
        for section in tbl().array(table) {
            for field in section[2].as_array().map_or(&[][..], Vec::as_slice) {
                names.push(text_row(field, 0).to_string());
            }
        }
    }
    for table in ["MC_AUTOTUNE_FIELDS", "FW_AUTOTUNE_FIELDS"] {
        names.extend(tbl().array(table).iter().map(|f| text_row(f, 0).to_string()));
    }
    names.extend(
        ["MC_AT_EN", "MC_AT_APPLY", "FW_AT_EN", "FW_AT_APPLY", "FW_AT_AXES"]
            .iter()
            .map(|s| s.to_string()),
    );
    let mut seen = std::collections::HashSet::new();
    names.retain(|n| seen.insert(n.clone()));
    names
}

/// A tuning gain: always a number field with step 0.001 and a floor of 0.
fn gain(name: &str, label: &str, values: &Values, unit: &str, hint: &str) -> Field {
    let mut f = number(name, label, values).step(0.001).min(0);
    if !unit.is_empty() {
        f = f.unit(unit);
    }
    if !hint.is_empty() {
        f = f.hint(hint);
    }
    f
}

pub(super) fn fields_of(table: &[Value], values: &Values) -> Vec<Value> {
    table
        .iter()
        .filter_map(|r| gain(text_row(r, 0), text_row(r, 1), values, text_row(r, 2), text_row(r, 3)).into_value())
        .collect()
}

pub(super) fn sections_of(table: &[Value], values: &Values) -> Vec<Value> {
    let mut out = Vec::new();
    for section in table {
        let fields = fields_of(section[2].as_array().map_or(&[][..], Vec::as_slice), values);
        if !fields.is_empty() {
            out.push(json!({"id": section[0], "title": section[1], "fields": fields}));
        }
    }
    out
}

pub(super) fn group(id: &str, title: &str, hint: &str, sections: Vec<Value>, charts: &[Value]) -> Option<Value> {
    if sections.is_empty() {
        return None;
    }
    Some(json!({
        "id": id, "title": title, "hint": hint, "kind": "fields",
        "sections": sections, "charts": charts,
    }))
}

fn first_non_empty(a: Vec<Value>, b: Vec<Value>) -> Vec<Value> {
    if a.is_empty() {
        b
    } else {
        a
    }
}

fn rate_group(values: &Values) -> Option<Value> {
    let sections = first_non_empty(
        sections_of(tbl().array("MC_RATE_SECTIONS"), values),
        sections_of(tbl().array("FW_RATE_SECTIONS"), values),
    );
    group(
        "rate",
        "Rate Controller",
        "The innermost loop: it turns a rate setpoint into motor torque, and it is the loop responsible for how the aircraft feels. Tune it first, because every outer loop is built on top of it, and this is the one the autotune tunes.",
        sections,
        tbl().array("MC_RATE_CHARTS"),
    )
}

fn attitude_group(values: &Values) -> Option<Value> {
    let sections = first_non_empty(
        sections_of(tbl().array("MC_ATTITUDE_SECTIONS"), values),
        sections_of(tbl().array("FW_ATTITUDE_SECTIONS"), values),
    );
    group(
        "attitude",
        "Attitude Controller",
        "Turns an angle error into the rate setpoint the loop above feeds to the rate controller. Only worth touching once the rate loop tracks cleanly.",
        sections,
        tbl().array("ATTITUDE_CHARTS"),
    )
}

fn velocity_group(values: &Values) -> Option<Value> {
    group(
        "velocity",
        "Velocity Controller",
        "Holds a commanded speed. This is the loop behind a hover that drifts in wind or an altitude that sags as the battery does. PX4 has no autotune for it. These gains are set by hand.",
        sections_of(tbl().array("MC_VELOCITY_SECTIONS"), values),
        tbl().array("VELOCITY_CHARTS"),
    )
}

fn position_group(values: &Values) -> Option<Value> {
    group(
        "position",
        "Position Controller",
        "The outermost loop: position error to velocity setpoint, plus the acceleration and jerk limits that decide how abrupt the vehicle is.",
        sections_of(tbl().array("MC_POSITION_SECTIONS"), values),
        tbl().array("POSITION_CHARTS"),
    )
}

/// Which autotune the connected firmware has, as (prefix, label): decided by
/// which enable parameter answered rather than by the vehicle type, because a
/// firmware built without the module has neither and a VTOL has both.
fn autotune_family(values: &Values) -> Option<(&'static str, &'static str)> {
    if values.contains_key("MC_AT_EN") {
        Some(("MC", "Multicopter autotune"))
    } else if values.contains_key("FW_AT_EN") {
        Some(("FW", "Fixed-wing autotune"))
    } else {
        None
    }
}

/// Which axes the next tune covers, and whether the operator can choose.
fn autotune_axes(prefix: &str, values: &Values) -> Value {
    if prefix == "FW" {
        let field = bitmask("FW_AT_AXES", "Axes to tune", values, tbl().options("FW_AXES_BITS"))
            .hint(tbl().get("AXES_HINT").as_str().unwrap_or(""));
        return match field.into_value() {
            Some(f) => json!({"selectable": true, "label": "Axes to tune", "field": f}),
            None => Value::Null,
        };
    }
    json!({
        "selectable": false, "label": "Axes to tune",
        "labels": tbl().get("MC_AUTOTUNE_AXES"),
        "reason": "PX4 tunes roll, pitch and yaw one after the other in a single run. Its multicopter autotune has no axis selection, so one axis cannot be tuned on its own. Any gain can still be set by hand in the Rate Controller below.",
    })
}

/// What the operator does once the tune is over, from the apply setting: with
/// "Never" the tune changes nothing, which must not be reported as "new gains
/// active".
fn done_text(values: &Values, prefix: &str) -> String {
    let apply = values.get(&format!("{prefix}_AT_APPLY")).map_or(1, |v| round_int(*v));
    let texts = tbl().get("APPLY_DONE_TEXT");
    texts
        .get(apply.to_string())
        .or_else(|| texts.get("1"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn autotune_group(values: &Values) -> Option<Value> {
    let (prefix, label) = autotune_family(values)?;
    let enable_param = format!("{prefix}_AT_EN");
    let enabled = values.get(&enable_param).map_or(0, |v| round_int(*v)) == 1;

    let mut settings = present(vec![
        enum_(&enable_param, "Autotune module", values, &tbl().options("ON_OFF_OPTIONS")).hint(
            "The module has to be running before it can be commanded. Switching this on takes effect after the autopilot reboots.",
        ),
        enum_(
            &format!("{prefix}_AT_APPLY"),
            "Apply the tuned gains",
            values,
            &tbl().options("AUTOTUNE_APPLY_OPTIONS"),
        )
        .hint("When the result reaches the controller. Applying in flight is supported but leaves you flying gains nobody has checked."),
    ]);
    let table = tbl().array(if prefix == "MC" {
        "MC_AUTOTUNE_FIELDS"
    } else {
        "FW_AUTOTUNE_FIELDS"
    });
    settings.extend(fields_of(table, values));

    Some(json!({
        "id": "autotune", "title": "Autotune", "kind": "autotune",
        "hint": "PX4 tunes the rate and attitude controllers together, in flight, by injecting steps and measuring what came back.",
        "label": label,
        "family": prefix,
        "enabled": enabled,
        "enable_param": if values.contains_key(&enable_param) { json!(enable_param) } else { Value::Null },
        "axes": autotune_axes(prefix, values),
        "progress": true,
        "running_text": tbl().get("AUTOTUNE_RUNNING_TEXT"),
        "done_text": done_text(values, prefix),
        "steps": tbl().get("AUTOTUNE_STEPS"),
        "sections": if settings.is_empty() { json!([]) } else { json!([{"id": "settings", "title": "Autotune settings", "fields": settings}]) },
        "charts": [],
    }))
}

/// Adds a `tune` entry to every controller group, from the autotune the firmware
/// carries (`"MC"`, `"FW"` or none).
fn attach_tune(groups: &mut [Value], family: Option<&str>) {
    let none_text = tbl().get("TUNE_NONE_TEXT").as_str().unwrap_or("").to_string();
    for g in groups.iter_mut() {
        if g["kind"].as_str() != Some("fields") {
            continue;
        }
        let lp = g["id"].as_str().unwrap_or("").to_string();
        let entry = match family {
            None => tune("manual", &none_text, ""),
            Some(_) if lp == "rate" => tune(
                "run",
                "Tunes the roll, pitch and yaw rate gains in one flight. It needs a stable hover.",
                "",
            ),
            Some("MC") if lp == "attitude" => tune(
                "with",
                "PX4 sets the attitude gains in the same run as the rate controller, so this starts that run.",
                "rate",
            ),
            Some(_) if lp == "attitude" => tune(
                "manual",
                "The fixed-wing autotune does not change the attitude time constants. Set them by hand, on the ground.",
                "",
            ),
            Some(_) => tune(
                "manual",
                "PX4 has no autotune for this loop. Set the gains by hand, on the ground.",
                "",
            ),
        };
        g["tune"] = entry;
    }
}

/// Turns the raw `{param: value}` into the PID Tuning page description
/// (`GET /api/tuning`): `groups` (innermost loop first; a controller also carries
/// `tune`, how the autotune reaches it) and `received`.
pub fn build(values: &Values) -> Value {
    let mut groups: Vec<Value> = [
        rate_group(values),
        attitude_group(values),
        velocity_group(values),
        position_group(values),
        autotune_group(values),
    ]
    .into_iter()
    .flatten()
    .collect();
    attach_tune(&mut groups, autotune_family(values).map(|f| f.0));
    json!({"groups": groups, "received": values.len()})
}

pub(super) fn axes_hint() -> &'static str {
    tbl().get("AXES_HINT").as_str().unwrap_or("")
}

pub(super) fn none_text() -> &'static str {
    tbl().get("TUNE_NONE_TEXT").as_str().unwrap_or("")
}
