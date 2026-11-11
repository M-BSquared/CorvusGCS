//! The ArduPilot motor and output schema of Setup, Motors
//! (`ardupilot_motors.py`): the twin of [`super::motors`], and the one of the
//! four that could not simply be a second parameter table, because the two
//! stacks disagree about what a motor is.
//!
//! PX4 describes the airframe (`CA_ROTOR3_PX`, `PY`, `PZ`, `KM`). ArduPilot
//! compiles its mixer in, so there is nothing on the wire to draw from: this
//! module returns motors with no positions, which the Motors page spreads evenly
//! and rings with a dashed line that says the geometry was not measured. What an
//! operator changes here is the frame class and type, which output pin each motor
//! is wired to (`SERVO<n>_FUNCTION`), the output protocol and PWM range, and the
//! spin thresholds that have to be set per airframe.

use serde_json::{json, Value};

use super::fields::*;
use super::tables::motors;

pub const MAX_MOTOR_FUNCTIONS: i64 = 12;
pub const MAX_ROTORS: i64 = MAX_MOTOR_FUNCTIONS;
/// How many `SERVO<n>_FUNCTION` parameters to ask about. Boards vary from 8 to
/// 32; a parameter the firmware does not carry is simply absent from the answer.
pub const MAX_SERVO_OUTPUTS: i64 = 16;

fn tbl() -> super::tables::Table {
    motors("ardupilot")
}

/// The `SERVO<n>_FUNCTION` value that means "this is Motor `motor`".
pub fn motor_function_value(motor: i64) -> Option<f64> {
    tbl()
        .get("MOTOR_FUNCTIONS")
        .get(motor.to_string())
        .and_then(Value::as_i64)
        .map(|v| v as f64)
}

/// Which motor an output function value drives, or `None`.
pub fn motor_of(value: f64) -> Option<i64> {
    let ivalue = round_int(value);
    tbl()
        .get("MOTOR_FUNCTIONS")
        .as_object()?
        .iter()
        .find(|(_, v)| v.as_i64() == Some(ivalue))
        .and_then(|(k, _)| k.parse().ok())
}

/// Operator facing name for one output function value.
pub fn function_label(value: f64) -> String {
    let ivalue = round_int(value);
    if let Some(motor) = motor_of(value) {
        return format!("Motor {motor}");
    }
    tbl()
        .get("SERVO_FUNCTIONS")
        .get(ivalue.to_string())
        .and_then(Value::as_str)
        .map_or_else(|| format!("Function {ivalue}"), str::to_string)
}

/// `SERVO<pin>_FUNCTION` for a valid bank and pin, else `None`.
pub fn function_param(bank_id: &str, pin: i64) -> Option<String> {
    if bank_id.to_uppercase() != "SERVO" {
        return None;
    }
    if !(1..=MAX_SERVO_OUTPUTS).contains(&pin) {
        return None;
    }
    Some(format!("SERVO{pin}_FUNCTION"))
}

pub fn frame_class(values: &Values) -> i64 {
    values.get("FRAME_CLASS").map_or(0, |v| round_int(*v))
}

fn frame_class_entry(values: &Values) -> Option<&'static Value> {
    tbl().get("FRAME_CLASSES").get(frame_class(values).to_string())
}

/// How many motors this frame class has: derived, not read, because the count is
/// a property of the compiled in layout.
pub fn rotor_count(values: &Values) -> i64 {
    frame_class_entry(values).and_then(|e| e[1].as_i64()).unwrap_or(0)
}

pub fn airframe_family(values: &Values) -> String {
    frame_class_entry(values)
        .and_then(|e| e[2].as_str())
        .unwrap_or("multirotor")
        .to_string()
}

/// Every parameter the page may need, in one flat list.
pub fn param_names() -> Vec<String> {
    let mut names: Vec<String> = [
        "FRAME_CLASS",
        "FRAME_TYPE",
        "MOT_PWM_TYPE",
        "MOT_PWM_MIN",
        "MOT_PWM_MAX",
        "MOT_SPIN_ARM",
        "MOT_SPIN_MIN",
        "MOT_SPIN_MAX",
        "MOT_THST_HOVER",
        "MOT_THST_EXPO",
        "MOT_BAT_VOLT_MIN",
        "MOT_BAT_VOLT_MAX",
        "MOT_SAFE_DISARM",
        "SERVO_DSHOT_RATE",
        "SERVO_DSHOT_ESC",
        "SERVO_BLH_AUTO",
        "SERVO_BLH_MASK",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for pin in 1..=MAX_SERVO_OUTPUTS {
        names.push(format!("SERVO{pin}_FUNCTION"));
        names.push(format!("SERVO{pin}_MIN"));
        names.push(format!("SERVO{pin}_MAX"));
    }
    names
}

pub use super::motors::Output;

/// Every output pin the vehicle answered for, with what it currently drives.
pub fn outputs(values: &Values) -> Vec<Output> {
    let mut entries = Vec::new();
    for b in tbl().array("BANKS") {
        let prefix = b["prefix"].as_str().unwrap_or("");
        let label = b["label"].as_str().unwrap_or("");
        for pin in 1..=b["pins"].as_i64().unwrap_or(0) {
            let param = format!("{prefix}{pin}_FUNCTION");
            let Some(&value) = values.get(&param) else {
                continue;
            };
            entries.push(Output {
                bank: b["id"].as_str().unwrap_or("").to_string(),
                pin,
                param,
                label: format!("{label} {pin}"),
                value,
                function: function_label(value),
                motor: motor_of(value),
            });
        }
    }
    entries
}

/// The banks that actually exist on this board.
pub fn banks(entries: &[Output]) -> Vec<Value> {
    tbl()
        .array("BANKS")
        .iter()
        .filter(|b| entries.iter().any(|e| Some(e.bank.as_str()) == b["id"].as_str()))
        .map(|b| json!({"id": b["id"], "label": b["label"], "hint": b["hint"]}))
        .collect()
}

/// One entry per motor: which output drives it, and its endpoints. No position
/// and no spin direction, deliberately: the interface draws motors with no
/// geometry on an evenly spread ring inside a dashed circle, which is the
/// truthful rendering of "the firmware did not tell us where these are".
fn motor_entries(values: &Values, entries: &[Output]) -> Vec<Value> {
    let mut motors = Vec::new();
    for number_ in 1..=rotor_count(values) {
        let assigned = entries.iter().rev().find(|e| e.motor == Some(number_));
        let fields = match assigned {
            Some(a) => present(vec![
                number(&format!("SERVO{}_MIN", a.pin), "Output minimum", values)
                    .unit("us")
                    .step(1),
                number(&format!("SERVO{}_MAX", a.pin), "Output maximum", values)
                    .unit("us")
                    .step(1),
            ]),
            None => Vec::new(),
        };
        motors.push(json!({
            "index": number_ - 1,
            "number": number_,
            "label": format!("Motor {number_}"),
            "x": 0.0, "y": 0.0, "z": 0.0,
            "axis": Value::Null,
            "thrust": Value::Null,
            "spin": Value::Null,
            "fields": fields,
            "output": assigned.map_or(Value::Null, |a| json!({
                "bank": a.bank, "pin": a.pin, "param": a.param, "label": a.label,
            })),
        }));
    }
    motors
}

/// Turns the raw `{param: value}` into the Motors page description, the same
/// shape as the PX4 one so the page renders both stacks with one code path.
pub fn build(values: &Values) -> Value {
    let geometry = present(vec![
        enum_("FRAME_CLASS", "Frame class", values, &tbl().options("FRAME_CLASS_OPTIONS"))
            .kv("reload", true)
            .hint("Which layout the firmware mixes for. Changing it redefines what Motor 3 means and takes effect after a reboot."),
        enum_("FRAME_TYPE", "Frame type", values, &tbl().options("FRAME_TYPE_OPTIONS"))
            .kv("reload", true)
            .hint("The arrangement within the class: X or Plus on a quad."),
    ]);
    let on_off = tbl().options("ON_OFF_OPTIONS");

    let mut sections: Vec<Value> = Vec::new();
    let protocol = present(vec![
        enum_(
            "MOT_PWM_TYPE",
            "Output protocol",
            values,
            &tbl().options("MOT_PWM_TYPE_OPTIONS"),
        )
        .hint("A protocol change takes effect after a reboot of the autopilot."),
        number("MOT_PWM_MIN", "PWM minimum", values).unit("us").step(1),
        number("MOT_PWM_MAX", "PWM maximum", values).unit("us").step(1),
        number("SERVO_DSHOT_RATE", "DShot rate", values).step(1),
        enum_("SERVO_BLH_AUTO", "BLHeli passthrough", values, &on_off)
            .hint("Lets a configurator reach the ESCs through the autopilot."),
        number("SERVO_BLH_MASK", "BLHeli channel mask", values).step(1),
    ]);
    if !protocol.is_empty() {
        sections.push(json!({
            "id": "protocol", "title": "Output protocol", "fields": protocol,
            "hint": "ArduPilot sets the protocol for all motor outputs at once, not per timer group.",
        }));
    }

    let spin = present(vec![
        number("MOT_SPIN_ARM", "Spin when armed", values)
            .step(0.01)
            .min(0)
            .max(1)
            .hint("Throttle, 0-1, that the motors turn at the moment the vehicle arms. Just enough that every motor is visibly turning. This is a safety cue as much as a setting."),
        number("MOT_SPIN_MIN", "Minimum flight throttle", values)
            .step(0.01)
            .min(0)
            .max(1)
            .hint("The lowest throttle used in flight. Must be above the arming spin, or a descent can stop a motor."),
        number("MOT_SPIN_MAX", "Maximum throttle", values).step(0.01).min(0).max(1),
        number("MOT_THST_HOVER", "Hover throttle", values)
            .step(0.01)
            .min(0)
            .max(1)
            .hint("Learned in flight. A value near 0.2 or near 0.8 means the aircraft is badly over- or under-powered."),
        number("MOT_THST_EXPO", "Thrust curve expo", values)
            .step(0.01)
            .hint("Linearises the ESC's thrust curve. 0.55 suits most hobby ESCs; smaller propellers want less."),
        number("MOT_BAT_VOLT_MIN", "Battery voltage, minimum", values)
            .unit("V")
            .step(0.1)
            .min(0),
        number("MOT_BAT_VOLT_MAX", "Battery voltage, maximum", values)
            .unit("V")
            .step(0.1)
            .min(0),
        enum_("MOT_SAFE_DISARM", "Stop PWM when disarmed", values, &on_off),
    ]);
    if !spin.is_empty() {
        sections.push(json!({
            "id": "spin", "title": "Throttle and thrust", "fields": spin,
            "hint": "The group that genuinely has to be set per airframe. Every value is a fraction of full throttle, 0 to 1.",
        }));
    }

    let entries = outputs(values);
    let label = frame_class_entry(values).and_then(|e| e[0].as_str()).unwrap_or("");
    json!({
        "stack": "ardupilot",
        "sections": sections,
        "geometry": geometry,
        "motors": motor_entries(values, &entries),
        // Reported rather than left out: an operator looking for the arm length
        // needs to hear that ArduPilot has no such setting, not find nothing.
        "frame": {"adjustable": false, "note": tbl().get("FRAME_NOTE")},
        "outputs": entries.iter().map(Output::to_json).collect::<Vec<_>>(),
        "banks": banks(&entries),
        "airframe_family": airframe_family(values),
        "airframe_label": label,
        "airframe_preset": Value::Null,
        "rotor_count": rotor_count(values),
        "max_motors": MAX_ROTORS,
        "fixed_motor_count": true,
        "received": values.len(),
    })
}
