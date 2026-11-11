//! The ArduPilot PID tuning schema of Setup, PID Tuning (`ardupilot_tuning.py`):
//! the twin of [`super::tuning`], with the same shape and the same contract.
//!
//! ArduPilot's autotune is a flight mode, not a command: there is nothing to
//! switch on, nothing to reboot for and no progress field (the autopilot narrates
//! the tune over STATUSTEXT), so the group carries the settings that shape the
//! tune, states the procedure, and the start button is a mode change. Every group
//! is a candidate only, so one schema serves Copter, Plane and Rover.

use serde_json::{json, Value};

use super::fields::*;
use super::tables::{tuning, Table};
use super::tuning::{group, text_row, tune};

fn tbl() -> Table {
    tuning("ardupilot")
}

/// Every parameter the page may need, across Copter, Plane and Rover.
pub fn param_names() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for table in [
        "COPTER_RATE_SECTIONS",
        "COPTER_ATTITUDE_SECTIONS",
        "COPTER_VELOCITY_SECTIONS",
        "COPTER_POSITION_SECTIONS",
        "PLANE_RATE_SECTIONS",
        "PLANE_ATTITUDE_SECTIONS",
        "ROVER_RATE_SECTIONS",
        "ROVER_NAV_SECTIONS",
    ] {
        for section in tbl().array(table) {
            for field in section[2].as_array().map_or(&[][..], Vec::as_slice) {
                names.push(text_row(field, 0).to_string());
            }
        }
    }
    for table in ["COPTER_AUTOTUNE_FIELDS", "PLANE_AUTOTUNE_FIELDS"] {
        names.extend(tbl().array(table).iter().map(|f| text_row(f, 0).to_string()));
    }
    names.push("AUTOTUNE_AXES".to_string());
    names.push("AUTOTUNE_OPTIONS".to_string());
    let mut seen = std::collections::HashSet::new();
    names.retain(|n| seen.insert(n.clone()));
    names
}

/// A field row `(parameter, label, unit, hint)`: unlike the PX4 gains these
/// carry `unit` and `hint` even when empty, and no step or floor.
fn fields_of(table: &[Value], values: &Values) -> Vec<Value> {
    table
        .iter()
        .filter_map(|r| {
            number(text_row(r, 0), text_row(r, 1), values)
                .kv("unit", text_row(r, 2))
                .kv("hint", text_row(r, 3))
                .into_value()
        })
        .collect()
}

fn sections_of(table: &[Value], values: &Values) -> Vec<Value> {
    let mut out = Vec::new();
    for section in table {
        let built = fields_of(section[2].as_array().map_or(&[][..], Vec::as_slice), values);
        if !built.is_empty() {
            out.push(json!({"id": section[0], "title": section[1], "fields": built}));
        }
    }
    out
}

fn first_of(candidates: [Vec<Value>; 3]) -> Vec<Value> {
    candidates.into_iter().find(|c| !c.is_empty()).unwrap_or_default()
}

fn rate_group(values: &Values) -> Option<Value> {
    let t = tbl();
    group(
        "rate",
        "Rate Controller",
        "The innermost loop: it turns a rate setpoint into control output, and it is the loop responsible for how the aircraft feels. Tune it first, because every outer loop is built on top of it, and this is the one the autotune tunes.",
        first_of([
            sections_of(t.array("COPTER_RATE_SECTIONS"), values),
            sections_of(t.array("PLANE_RATE_SECTIONS"), values),
            sections_of(t.array("ROVER_RATE_SECTIONS"), values),
        ]),
        t.array("RATE_CHARTS"),
    )
}

fn attitude_group(values: &Values) -> Option<Value> {
    let t = tbl();
    group(
        "attitude",
        "Attitude Controller",
        "The loop above the rate controller: it turns an attitude error into the rate setpoints the loop below chases.",
        first_of([
            sections_of(t.array("COPTER_ATTITUDE_SECTIONS"), values),
            sections_of(t.array("PLANE_ATTITUDE_SECTIONS"), values),
            sections_of(t.array("ROVER_NAV_SECTIONS"), values),
        ]),
        t.array("ATTITUDE_CHARTS"),
    )
}

fn velocity_group(values: &Values) -> Option<Value> {
    let t = tbl();
    group(
        "velocity",
        "Velocity Controller",
        "Turns a velocity demand into an attitude and a throttle. Multicopter only. A fixed wing does this through TECS instead.",
        sections_of(t.array("COPTER_VELOCITY_SECTIONS"), values),
        t.array("VELOCITY_CHARTS"),
    )
}

fn position_group(values: &Values) -> Option<Value> {
    let t = tbl();
    group(
        "position",
        "Position Controller",
        "The outermost loop: it turns a position error into a velocity demand, and it is what holds a point and flies a waypoint.",
        sections_of(t.array("COPTER_POSITION_SECTIONS"), values),
        t.array("VELOCITY_CHARTS"),
    )
}

/// The autotune group: its settings and its procedure. `None` for a vehicle whose
/// firmware has no autotune (Rover and Sub), so the page says "this vehicle has
/// no autotune" rather than offering a button that always fails.
fn autotune_group(values: &Values, vehicle: &str) -> Option<Value> {
    let t = tbl();
    let (fields, steps, running) = match vehicle {
        "plane" => ("PLANE_AUTOTUNE_FIELDS", "PLANE_AUTOTUNE_STEPS", "PLANE_RUNNING_TEXT"),
        "copter" => ("COPTER_AUTOTUNE_FIELDS", "COPTER_AUTOTUNE_STEPS", "COPTER_RUNNING_TEXT"),
        _ => return None,
    };
    // The axis selection sits next to the start button rather than among the
    // settings: it is the one choice made before every tune.
    let bits = t.options(if vehicle == "plane" {
        "PLANE_AUTOTUNE_AXES_BITS"
    } else {
        "AUTOTUNE_AXES_BITS"
    });
    let axes = match bitmask("AUTOTUNE_AXES", "Axes to tune", values, bits)
        .hint(super::tuning::axes_hint())
        .into_value()
    {
        None => Value::Null,
        Some(f) => json!({"selectable": true, "label": "Axes to tune", "field": f}),
    };
    let mut settings = present(vec![enum_(
        "AUTOTUNE_OPTIONS",
        "Options",
        values,
        &opts(&[(0, "Default"), (1, "Re-tune after a failed attempt")]),
    )]);
    settings.extend(fields_of(t.array(fields), values));
    Some(json!({
        "id": "autotune", "title": "Autotune", "kind": "autotune",
        "hint": "ArduPilot's autotune is a flight mode, not a command: starting it switches the vehicle into AUTOTUNE and the autopilot takes over from there. It narrates its progress over the console.",
        "label": "AUTOTUNE mode",
        "family": "ARDUPILOT",
        // There is no module to switch on and no reboot to wait for, so the
        // "module is off" warning the PX4 page shows never applies here.
        "enabled": true,
        "enable_param": Value::Null,
        "axes": axes,
        "progress": false,
        "running_text": t.get(running),
        "done_text": t.get(running),
        "steps": t.get(steps),
        "sections": if settings.is_empty() { json!([]) } else { json!([{"id": "settings", "title": "Autotune settings", "fields": settings}]) },
        "charts": [],
    }))
}

fn attach_tune(groups: &mut [Value], vehicle: &str, has_autotune: bool) {
    let none_text = super::tuning::none_text();
    for g in groups.iter_mut() {
        if g["kind"].as_str() != Some("fields") {
            continue;
        }
        let lp = g["id"].as_str().unwrap_or("").to_string();
        let entry = if !has_autotune {
            tune("manual", none_text, "")
        } else if lp == "rate" {
            tune(
                "run",
                if vehicle == "copter" {
                    "Tunes the rate gains of the chosen axes in AUTOTUNE mode. It needs a stable hover."
                } else {
                    "Tunes the rate gains from your own stick inputs in AUTOTUNE mode."
                },
                "",
            )
        } else if lp == "attitude" && vehicle == "copter" {
            tune(
                "with",
                "ArduPilot tunes the angle P gains in the same run as the rate controller, so this starts that run.",
                "rate",
            )
        } else {
            tune(
                "manual",
                "ArduPilot has no autotune for this loop. Set the gains by hand, on the ground.",
                "",
            )
        };
        g["tune"] = entry;
    }
}

/// Turns the raw `{param: value}` into the PID Tuning page description, the same
/// shape as the PX4 one: `groups` of `{id, title, hint, kind, sections, charts}`,
/// innermost loop first.
pub fn build(values: &Values, vehicle: &str) -> Value {
    let mut groups: Vec<Value> = [
        rate_group(values),
        attitude_group(values),
        velocity_group(values),
        position_group(values),
        autotune_group(values, vehicle),
    ]
    .into_iter()
    .flatten()
    .collect();
    let has_autotune = groups.iter().any(|g| g["kind"].as_str() == Some("autotune"));
    attach_tune(&mut groups, vehicle, has_autotune);
    json!({"groups": groups, "received": values.len(), "stack": "ardupilot"})
}
