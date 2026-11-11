//! The PX4 battery schema of Setup, Battery and Power (`battery_config.py`).
//!
//! The ownership rule between this page and Safety and Sensors is a prefix:
//! every `BAT*` parameter belongs here, everything else there. Three groups in
//! the order somebody sets a vehicle up: `pack` (what is plugged in),
//! `sensing` (how the autopilot measures it) and `levels` (the fractions at
//! which the failsafe fires). Every field is a candidate only, so one schema
//! serves PX4 v1.16 to v1.18 and the pre v1.11 un-indexed names listed beside
//! their modern replacements.
//!
//! [`build`] also returns a `pack` summary: the few numbers the page's battery
//! diagram is drawn from, normalised so the same drawing code serves this
//! schema and the ArduPilot one.

use serde_json::{json, Value};

use super::fields::*;
use crate::protocol::common::py_round;

fn source_options() -> Vec<Value> {
    opts(&[
        (-1, "Disabled"),
        (0, "Power module (analog)"),
        (1, "External, over MAVLink"),
        (2, "ESCs"),
    ])
}

const INSTANCES: [i64; 2] = [1, 2];

/// The pack description for one battery instance.
fn pack_fields(prefix: &str, values: &Values) -> Vec<Value> {
    present(vec![
        number(&format!("{prefix}_N_CELLS"), "Cells in series", values)
            .unit("S")
            .step(1)
            .min(0)
            .max(24)
            .hint("How many cells the pack has. Everything the autopilot decides about the battery is per cell, so this is the one number that must be right."),
        number(&format!("{prefix}_CAPACITY"), "Capacity", values)
            .unit("mAh")
            .step(50)
            .min(-1)
            .hint("The pack's rated capacity. -1 leaves the autopilot with no capacity figure, which is why its own remaining estimate can only ever be a voltage reading."),
        number(&format!("{prefix}_V_CHARGED"), "Full cell voltage", values)
            .unit("V")
            .step(0.01)
            .min(0)
            .max(5)
            .hint("Resting voltage of one fully charged cell. 4.05 V is the PX4 default for LiPo, 4.2 V is the charger's."),
        number(&format!("{prefix}_V_EMPTY"), "Empty cell voltage", values)
            .unit("V")
            .step(0.01)
            .min(0)
            .max(5)
            .hint("Resting voltage of one cell at 0% remaining. This is the landing decision, not the cell's datasheet minimum."),
        number(&format!("{prefix}_V_LOAD_DROP"), "Load drop allowance", values)
            .unit("V")
            .step(0.01)
            .min(0)
            .max(1.5)
            .hint("How far one cell is expected to sag at full throttle. Used to keep a hard climb from reading as an empty pack."),
        number(&format!("{prefix}_R_INTERNAL"), "Internal resistance", values)
            .unit("Ohm")
            .step(0.001)
            .min(-1)
            .max(1)
            .hint("Measured pack resistance, used to correct voltage back to rest. -1 lets PX4 fall back to the load-drop figure above."),
    ])
}

/// How the autopilot measures one battery instance.
fn sensing_fields(prefix: &str, values: &Values) -> Vec<Value> {
    present(vec![
        enum_(&format!("{prefix}_SOURCE"), "Measured by", values, &source_options())
            .hint("Where voltage and current are read from."),
        number(&format!("{prefix}_V_DIV"), "Voltage divider", values)
            .step(0.01)
            .min(-1)
            .hint("The board's voltage divider. Calibrate it against a meter on the pack: a divider that is 3% out is 3% of every reading on this page."),
        number(&format!("{prefix}_A_PER_V"), "Amps per volt", values)
            .unit("A/V")
            .step(0.01)
            .min(-1)
            .hint("The power module's current scaling, from its datasheet or measured against a clamp meter."),
        number(&format!("{prefix}_V_CHANNEL"), "Voltage ADC channel", values)
            .step(1)
            .min(-1)
            .hint("-1 uses the board default."),
        number(&format!("{prefix}_I_CHANNEL"), "Current ADC channel", values)
            .step(1)
            .min(-1)
            .hint("-1 uses the board default."),
    ])
}

/// The un-indexed names PX4 used before multi-battery support (before v1.11).
/// Listed beside the modern ones rather than behind a version switch: exactly
/// one set survives the candidate filter.
fn legacy_pack_fields(values: &Values) -> Vec<Value> {
    present(vec![
        number("BAT_N_CELLS", "Cells in series", values)
            .unit("S")
            .step(1)
            .min(0)
            .max(24),
        number("BAT_CAPACITY", "Capacity", values).unit("mAh").step(50).min(-1),
        number("BAT_V_CHARGED", "Full cell voltage", values)
            .unit("V")
            .step(0.01)
            .min(0)
            .max(5),
        number("BAT_V_EMPTY", "Empty cell voltage", values)
            .unit("V")
            .step(0.01)
            .min(0)
            .max(5),
        number("BAT_V_LOAD_DROP", "Load drop allowance", values)
            .unit("V")
            .step(0.01)
            .min(0)
            .max(1.5),
        number("BAT_R_INTERNAL", "Internal resistance", values)
            .unit("Ohm")
            .step(0.001)
            .min(-1)
            .max(1),
    ])
}

fn levels_section(values: &Values) -> Option<Value> {
    let fields = present(vec![
        number("BAT_LOW_THR", "Low level", values)
            .step(0.01)
            .min(0)
            .max(1)
            .hint("Remaining capacity, 0-1, at which the low-battery action fires. 0.15 is 15%."),
        number("BAT_CRIT_THR", "Critical level", values).step(0.01).min(0).max(1),
        number("BAT_EMERGEN_THR", "Emergency level", values).step(0.01).min(0).max(1),
        number("BAT_AVRG_CURRENT", "Average current", values)
            .unit("A")
            .step(0.1)
            .min(0)
            .hint("The current PX4 assumes when it has nothing better, which is what its remaining-time figure is built on."),
    ]);
    section(
        "levels",
        "Low-battery levels",
        fields,
        "A fraction of the pack, not a voltage. What happens when one of these is crossed is the low-battery action on Safety & Sensors.",
    )
    .map(Value::Object)
}

/// Every parameter the page may need, in one flat list for one batched read.
pub fn param_names() -> Vec<String> {
    let mut names: Vec<String> = ["BAT_LOW_THR", "BAT_CRIT_THR", "BAT_EMERGEN_THR", "BAT_AVRG_CURRENT"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for suffix in [
        "N_CELLS",
        "CAPACITY",
        "V_CHARGED",
        "V_EMPTY",
        "V_LOAD_DROP",
        "R_INTERNAL",
    ] {
        names.push(format!("BAT_{suffix}"));
    }
    for instance in INSTANCES {
        for suffix in [
            "N_CELLS",
            "CAPACITY",
            "V_CHARGED",
            "V_EMPTY",
            "V_LOAD_DROP",
            "R_INTERNAL",
            "SOURCE",
            "V_DIV",
            "A_PER_V",
            "V_CHANNEL",
            "I_CHANNEL",
        ] {
            names.push(format!("BAT{instance}_{suffix}"));
        }
    }
    names
}

/// The first of `names` this firmware answered for, with its value.
fn first<'a>(values: &Values, names: &[&'a str]) -> Option<(&'a str, f64)> {
    names.iter().find_map(|n| values.get(*n).map(|v| (*n, *v)))
}

/// The numbers the battery diagram is drawn from, normalised. Same keys as the
/// ArduPilot summary; a key the stack has no answer for is 0 (or an empty
/// list), which the page renders as "not known" rather than as a zero.
pub fn pack_summary(values: &Values) -> Value {
    let cells = first(values, &["BAT1_N_CELLS", "BAT_N_CELLS"]);
    let capacity = first(values, &["BAT1_CAPACITY", "BAT_CAPACITY"]);
    let full = first(values, &["BAT1_V_CHARGED", "BAT_V_CHARGED"]);
    let empty = first(values, &["BAT1_V_EMPTY", "BAT_V_EMPTY"]);
    let drop = first(values, &["BAT1_V_LOAD_DROP", "BAT_V_LOAD_DROP"]);
    let resistance = first(values, &["BAT1_R_INTERNAL", "BAT_R_INTERNAL"]);
    let source = first(values, &["BAT1_SOURCE"]);

    let mut thresholds = Vec::new();
    for (param, label) in [
        ("BAT_LOW_THR", "Low"),
        ("BAT_CRIT_THR", "Critical"),
        ("BAT_EMERGEN_THR", "Emergency"),
    ] {
        if let Some(v) = values.get(param) {
            thresholds.push(json!({
                "id": param.split('_').nth(1).unwrap_or("").to_lowercase(),
                "label": label, "param": param,
                "percent": crate::json::num(py_round(v * 100.0, 1)),
            }));
        }
    }

    let positive = |item: Option<(&str, f64)>, digits: usize| match item {
        Some((_, v)) if v > 0.0 => crate::json::num(py_round(v, digits)),
        _ => json!(0.0),
    };
    json!({
        "cells": match cells { Some((_, v)) if v > 0.0 => json!(v.trunc() as i64), _ => json!(0) },
        "cells_param": cells.map_or("", |c| c.0),
        "capacity_mah": positive(capacity, 1),
        "capacity_param": capacity.map_or("", |c| c.0),
        "full_cell": positive(full, 3),
        "empty_cell": positive(empty, 3),
        "load_drop": positive(drop, 3),
        "resistance_ohm": positive(resistance, 4),
        "source": source.map_or(String::new(), |s| option_label(&source_options(), s.1)),
        "thresholds": thresholds,
    })
}

/// Turns the raw `{param: value}` into the Battery and Power page description
/// (`GET /api/battery`): `sections` (ordered `{id, title, kind, fields[, hint]}`,
/// `kind` always `"fields"`), `pack` (the normalised diagram summary) and
/// `received`.
pub fn build(values: &Values) -> Value {
    let pack = {
        let modern = pack_fields("BAT1", values);
        if modern.is_empty() {
            legacy_pack_fields(values)
        } else {
            modern
        }
    };
    let mut sections: Vec<Option<Value>> = vec![
        section(
            "pack",
            "Pack",
            pack,
            "What is plugged in. The cell count is the number to get right first: every other figure here is per cell.",
        )
        .map(Value::Object),
        section(
            "sensing",
            "Measurement",
            sensing_fields("BAT1", values),
            "How the autopilot reads the pack. Calibrate the divider against a meter before trusting anything above.",
        )
        .map(Value::Object),
        levels_section(values),
    ];
    let mut second = pack_fields("BAT2", values);
    second.extend(sensing_fields("BAT2", values));
    if !pack_fields("BAT2", values).is_empty() || !sensing_fields("BAT2", values).is_empty() {
        sections.push(
            section(
                "pack2",
                "Second pack",
                second,
                "The second battery instance. Only fill this in on an aircraft that actually carries two packs the autopilot measures separately.",
            )
            .map(Value::Object),
        );
    }
    json!({
        "sections": sections.into_iter().flatten().collect::<Vec<_>>(),
        "pack": pack_summary(values),
        "received": values.len(),
    })
}
