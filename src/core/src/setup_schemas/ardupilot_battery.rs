//! The ArduPilot battery schema of Setup, Battery and Power
//! (`ardupilot_battery.py`): the twin of [`super::battery`] with the same output
//! shape, so the interface cannot tell which stack built the page.
//!
//! ArduPilot has no cell count (Corvus's own estimator carries one) and its
//! thresholds are absolute volts, where PX4's are fractions; both travel to the
//! interface unconverted, because the value the page writes has to be the value
//! the parameter holds. Every `BATT*` parameter belongs to this page, including
//! the two battery failsafe actions (`BATT_FS_*`).

use serde_json::{json, Value};

use super::fields::*;
use crate::protocol::common::py_round;

fn monitor_options() -> Vec<Value> {
    opts(&[
        (0, "Disabled"),
        (3, "Analog voltage only"),
        (4, "Analog voltage and current"),
        (5, "Solo"),
        (6, "Bebop"),
        (7, "SMBus generic"),
        (8, "DroneCAN BatteryInfo"),
        (9, "ESC telemetry"),
        (10, "Sum of selected monitors"),
        (11, "Fuel flow"),
        (12, "Fuel level (PWM)"),
        (14, "SMBus SUI6"),
        (15, "NeoDesign"),
        (16, "SMBus Maxell"),
        (17, "Generator (electrical)"),
        (18, "Generator (fuel)"),
        (19, "Rotoye"),
        (20, "MPPT"),
        (21, "INA2xx"),
        (22, "LTC2946"),
        (23, "Torqeedo"),
        (24, "Fuel level (analog)"),
        (25, "Synthetic current and voltage"),
        (27, "EFI"),
        (28, "AD7091R5"),
        (29, "Scripting"),
    ])
}

/// BATT_FS_LOW_ACT and BATT_FS_CRT_ACT; per vehicle because "land" is not an
/// option a rover has. An unrecognised class gets Copter's.
fn action_options(vehicle: &str) -> Vec<Value> {
    match vehicle {
        "plane" => opts(&[
            (0, "Nothing"),
            (1, "RTL"),
            (2, "Land"),
            (3, "Terminate"),
            (4, "QLand"),
            (6, "Loiter, then QLand"),
        ]),
        "rover" => opts(&[
            (0, "Nothing"),
            (1, "RTL"),
            (2, "Hold"),
            (3, "SmartRTL, or RTL"),
            (4, "SmartRTL, or hold"),
            (5, "Terminate"),
        ]),
        _ => opts(&[
            (0, "Nothing"),
            (1, "Land"),
            (2, "RTL"),
            (3, "SmartRTL, or RTL"),
            (4, "SmartRTL, or land"),
            (5, "Terminate"),
            (6, "Auto landing sequence, or RTL"),
            (7, "Brake, or land"),
        ]),
    }
}

fn fs_source_options() -> Vec<Value> {
    opts(&[
        (0, "Disabled"),
        (1, "Raw voltage"),
        (2, "Voltage compensated for current draw"),
    ])
}

const INSTANCES: [&str; 2] = ["BATT", "BATT2"];

const PACK_SUFFIXES: [&str; 18] = [
    "MONITOR",
    "CAPACITY",
    "LOW_VOLT",
    "CRT_VOLT",
    "LOW_MAH",
    "CRT_MAH",
    "FS_LOW_ACT",
    "FS_CRT_ACT",
    "FS_VOLTSRC",
    "LOW_TIMER",
    "ARM_VOLT",
    "ARM_MAH",
    "VOLT_PIN",
    "CURR_PIN",
    "VOLT_MULT",
    "AMP_PERVLT",
    "AMP_OFFSET",
    "WATT_MAX",
];

/// Every parameter the page may need, in one flat list.
pub fn param_names() -> Vec<String> {
    INSTANCES
        .iter()
        .flat_map(|prefix| PACK_SUFFIXES.iter().map(move |s| format!("{prefix}_{s}")))
        .collect()
}

fn pack_fields(prefix: &str, values: &Values) -> Vec<Value> {
    present(vec![
        enum_(&format!("{prefix}_MONITOR"), "Monitor", values, &monitor_options())
            .hint("What the autopilot reads the pack with. A change here needs a reboot before the rest of these fields appear."),
        number(&format!("{prefix}_CAPACITY"), "Capacity", values)
            .unit("mAh")
            .step(50)
            .min(0)
            .hint("The pack's rated capacity. Without it ArduPilot has no capacity figure and reports no remaining percentage at all."),
        number(&format!("{prefix}_WATT_MAX"), "Maximum power draw", values)
            .unit("W")
            .step(10)
            .min(0)
            .hint("Throttle is limited to hold the pack under this. 0 is off."),
    ])
}

fn sensing_fields(prefix: &str, values: &Values) -> Vec<Value> {
    present(vec![
        number(&format!("{prefix}_VOLT_PIN"), "Voltage pin", values)
            .step(1)
            .min(-1)
            .hint("The board's analog pin for voltage. -1 on a monitor that is not analog."),
        number(&format!("{prefix}_CURR_PIN"), "Current pin", values).step(1).min(-1),
        number(&format!("{prefix}_VOLT_MULT"), "Voltage multiplier", values)
            .step(0.01)
            .min(0)
            .hint("Calibrate against a meter on the pack: a multiplier that is 3% out is 3% of every reading on this page."),
        number(&format!("{prefix}_AMP_PERVLT"), "Amps per volt", values)
            .unit("A/V")
            .step(0.01)
            .min(0),
        number(&format!("{prefix}_AMP_OFFSET"), "Current offset", values)
            .unit("V")
            .step(0.001)
            .min(0)
            .hint("The voltage the current sensor reads at zero amps."),
    ])
}

fn level_fields(prefix: &str, values: &Values, vehicle: &str) -> Vec<Value> {
    let actions = action_options(vehicle);
    present(vec![
        number(&format!("{prefix}_LOW_VOLT"), "Low voltage", values)
            .unit("V")
            .step(0.1)
            .min(0)
            .hint("Pack volts, not per cell. On a 6S LiPo, 21.0 V is 3.5 V a cell."),
        number(&format!("{prefix}_CRT_VOLT"), "Critical voltage", values).unit("V").step(0.1).min(0),
        number(&format!("{prefix}_LOW_MAH"), "Low capacity remaining", values)
            .unit("mAh")
            .step(50)
            .min(0),
        number(&format!("{prefix}_CRT_MAH"), "Critical capacity remaining", values)
            .unit("mAh")
            .step(50)
            .min(0),
        enum_(&format!("{prefix}_FS_LOW_ACT"), "Low battery action", values, &actions),
        enum_(&format!("{prefix}_FS_CRT_ACT"), "Critical battery action", values, &actions),
        enum_(&format!("{prefix}_FS_VOLTSRC"), "Voltage source", values, &fs_source_options())
            .hint("Compensated voltage subtracts the sag a high current draw causes, so a hard climb does not trigger a low-battery failsafe."),
        number(&format!("{prefix}_LOW_TIMER"), "Low voltage must persist for", values)
            .unit("s")
            .step(1)
            .min(0),
        number(&format!("{prefix}_ARM_VOLT"), "Minimum voltage to arm", values)
            .unit("V")
            .step(0.1)
            .min(0),
        number(&format!("{prefix}_ARM_MAH"), "Minimum capacity to arm", values)
            .unit("mAh")
            .step(50)
            .min(0),
    ])
}

/// The numbers the battery diagram is drawn from, normalised: same keys as the
/// PX4 summary. `cells`, `full_cell` and `empty_cell` are 0 here and always
/// will be, because ArduPilot has no parameter for any of them.
pub fn pack_summary(values: &Values) -> Value {
    let mut thresholds = Vec::new();
    for (param, label) in [("BATT_LOW_VOLT", "Low"), ("BATT_CRT_VOLT", "Critical")] {
        if let Some(&v) = values.get(param).filter(|v| **v > 0.0) {
            thresholds.push(json!({
                "id": param.split('_').nth(1).unwrap_or("").to_lowercase(),
                "label": label, "param": param,
                "volts": crate::json::num(py_round(v, 2)),
            }));
        }
    }
    for (param, label) in [("BATT_LOW_MAH", "Low capacity"), ("BATT_CRT_MAH", "Critical capacity")] {
        if let Some(&v) = values.get(param).filter(|v| **v > 0.0) {
            thresholds.push(json!({
                "id": format!("{}_mah", param.split('_').nth(1).unwrap_or("").to_lowercase()),
                "label": label, "param": param,
                "mah": crate::json::num(py_round(v, 0)),
            }));
        }
    }
    let capacity = values.get("BATT_CAPACITY").copied().unwrap_or(0.0);
    let capacity = if capacity.is_nan() { 0.0 } else { capacity };
    json!({
        "cells": 0,
        "cells_param": "",
        "capacity_mah": if capacity > 0.0 { crate::json::num(py_round(capacity, 1)) } else { json!(0.0) },
        "capacity_param": if values.contains_key("BATT_CAPACITY") { "BATT_CAPACITY" } else { "" },
        "full_cell": 0.0,
        "empty_cell": 0.0,
        "load_drop": 0.0,
        "resistance_ohm": 0.0,
        "source": values.get("BATT_MONITOR").map_or(String::new(), |m| option_label(&monitor_options(), *m)),
        "thresholds": thresholds,
    })
}

/// Turns the raw `{param: value}` into the Battery and Power page description,
/// the same shape as the PX4 one. `vehicle` picks the failsafe action labels
/// that differ per firmware family.
pub fn build(values: &Values, vehicle: &str) -> Value {
    let mut sections: Vec<Option<Value>> = vec![
        section(
            "pack",
            "Pack",
            pack_fields("BATT", values),
            "What is plugged in, and what reads it. ArduPilot has no cell count of its own. Corvus keeps one below, for the estimate and the diagram.",
        )
        .map(Value::Object),
        section(
            "sensing",
            "Measurement",
            sensing_fields("BATT", values),
            "Calibrate the multiplier against a meter before trusting anything above.",
        )
        .map(Value::Object),
        section(
            "levels",
            "Low-battery levels",
            level_fields("BATT", values, vehicle),
            "Pack volts and remaining capacity, and what the vehicle does when one of them is crossed.",
        )
        .map(Value::Object),
    ];
    let mut second = pack_fields("BATT2", values);
    second.extend(sensing_fields("BATT2", values));
    second.extend(level_fields("BATT2", values, vehicle));
    if !second.is_empty() {
        sections.push(
            section(
                "pack2",
                "Second pack",
                second,
                "The second monitor. Only fill this in on an aircraft that actually carries two packs the autopilot measures separately.",
            )
            .map(Value::Object),
        );
    }
    json!({
        "stack": "ardupilot",
        "vehicle": vehicle,
        "sections": sections.into_iter().flatten().collect::<Vec<_>>(),
        "pack": pack_summary(values),
        "received": values.len(),
    })
}
