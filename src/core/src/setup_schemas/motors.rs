//! The PX4 motor and actuator schema of Setup, Motors (`motor_config.py`).
//!
//! The page is motor centric, not pin centric. PX4 stores the mapping the other
//! way round (each output pin names the function it drives), but an operator
//! holding an aircraft thinks "which pin is motor 3 on?", so [`build`] inverts
//! the mapping into one entry per motor, each carrying its arm position, its
//! spin direction and the output it is wired to. A quad shows four motors, a
//! hexa six, and adding one is a write to `CA_ROTOR_COUNT`.
//!
//! Every field is a candidate only, whole output banks included (a board with no
//! AUX pins and no DroneCAN ESCs answers for neither), and both the per timer
//! protocol parameters (`PWM_MAIN_TIM*`, v1.14 and later) and the legacy
//! `DSHOT_CONFIG` are candidates. An enum value the schema does not know is kept
//! as an extra `Unknown (n)` option.

use serde_json::{json, Value};

use super::fields::*;
use super::tables::motors;
use crate::json::Map;
use crate::protocol::common::py_round;

/// Control allocation supports at most 12 rotors; the output functions reach
/// Motor 16, which is why the two caps differ.
pub const MAX_ROTORS: i64 = 12;
pub const MAX_MOTOR_FUNCTIONS: i64 = 16;
const MOTOR_FUNCTION_BASE: i64 = 100;
const SERVO_FUNCTION_BASE: i64 = 200;
const TIMER_GROUPS: i64 = 4;
const DEFAULT_FAMILY: &str = "multirotor";
/// Below this a frame has no measurable size: its motor positions are not set.
const MIN_FRAME_M: f64 = 1e-3;

fn tbl() -> super::tables::Table {
    motors("px4")
}

fn banks_table() -> &'static [Value] {
    tbl().array("BANKS")
}

fn bank(id: &str) -> Option<&'static Value> {
    banks_table().iter().find(|b| b["id"].as_str() == Some(id))
}

/// The output function value that means "this is Motor `motor`": PX4 numbers
/// them contiguously from 100.
pub fn motor_function_value(motor: i64) -> Option<f64> {
    if (1..=MAX_MOTOR_FUNCTIONS).contains(&motor) {
        Some((MOTOR_FUNCTION_BASE + motor) as f64)
    } else {
        None
    }
}

/// Name of the parameter that selects what output `pin` of bank `bank_id` drives.
pub fn function_param(bank_id: &str, pin: i64) -> Option<String> {
    let bank = bank(bank_id)?;
    if (1..=bank["pins"].as_i64().unwrap_or(0)).contains(&pin) {
        Some(format!("{}_FUNC{pin}", bank["prefix"].as_str().unwrap_or("")))
    } else {
        None
    }
}

/// Human name for a `PWM_*_FUNCn` value ("Motor 3", "Servo 1", "Disabled").
pub fn function_label(value: f64) -> String {
    let ivalue = round_int(value);
    if ivalue == 0 {
        return "Disabled".to_string();
    }
    let motor = ivalue - MOTOR_FUNCTION_BASE;
    if (1..=MAX_MOTOR_FUNCTIONS).contains(&motor) {
        return format!("Motor {motor}");
    }
    let servo = ivalue - SERVO_FUNCTION_BASE;
    if (1..=8).contains(&servo) {
        return format!("Servo {servo}");
    }
    format!("Function {ivalue}")
}

/// The 1 based motor a `PWM_*_FUNCn` value drives, or `None` if it is not a motor.
pub fn motor_of(value: f64) -> Option<i64> {
    let motor = round_int(value) - MOTOR_FUNCTION_BASE;
    (1..=MAX_MOTOR_FUNCTIONS).contains(&motor).then_some(motor)
}

/// Every parameter the page may need, in one flat list for one batched read.
pub fn param_names() -> Vec<String> {
    let mut names: Vec<String> = ["SYS_AUTOSTART", "CA_AIRFRAME", "CA_ROTOR_COUNT"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for i in 0..MAX_ROTORS {
        for suffix in ["PX", "PY", "PZ", "KM", "CT", "AX", "AY", "AZ"] {
            names.push(format!("CA_ROTOR{i}_{suffix}"));
        }
    }
    for b in banks_table() {
        for pin in 1..=b["pins"].as_i64().unwrap_or(0) {
            names.push(format!("{}_FUNC{pin}", b["prefix"].as_str().unwrap_or("")));
        }
    }
    for suffix in ["MAIN", "AUX"] {
        for i in 0..TIMER_GROUPS {
            names.push(format!("PWM_{suffix}_TIM{i}"));
        }
        for s in ["MIN", "MAX", "DISARM"] {
            names.push(format!("PWM_{suffix}_{s}"));
        }
    }
    names.push("DSHOT_CONFIG".to_string());
    names.push("DSHOT_MIN".to_string());
    names
}

/// One output pin the vehicle answered for, with what it currently drives.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub bank: String,
    pub pin: i64,
    pub param: String,
    pub label: String,
    pub value: f64,
    pub function: String,
    pub motor: Option<i64>,
}

impl Output {
    pub fn to_json(&self) -> Value {
        json!({
            "bank": self.bank, "pin": self.pin, "param": self.param,
            "label": self.label,
            "value": crate::json::num(self.value),
            "function": self.function,
            "motor": self.motor,
        })
    }
}

/// The per channel limits of every pin that drives a motor: the second read of
/// the Motors page, made once the first has said which pins are wired.
pub fn output_param_names(values: &Values) -> Vec<String> {
    let mut names = Vec::new();
    for entry in outputs(values) {
        if entry.motor.is_none() {
            continue;
        }
        let Some(b) = bank(&entry.bank) else {
            continue;
        };
        for limit in b["limits"].as_array().map_or(&[][..], Vec::as_slice) {
            names.push(format!(
                "{}_{}{}",
                b["prefix"].as_str().unwrap_or(""),
                limit["suffix"].as_str().unwrap_or(""),
                entry.pin
            ));
        }
    }
    names
}

/// The limits of the pin a motor is wired to, as far as the vehicle has them.
fn output_fields(values: &Values, entry: Option<&Output>) -> Vec<Value> {
    let Some(entry) = entry else {
        return Vec::new();
    };
    let Some(b) = bank(&entry.bank) else {
        return Vec::new();
    };
    let unit = b["unit"].as_str().unwrap_or("");
    let mut fields = Vec::new();
    for limit in b["limits"].as_array().map_or(&[][..], Vec::as_slice) {
        let suffix = limit["suffix"].as_str().unwrap_or("");
        let mut f = number(
            &format!("{}_{suffix}{}", b["prefix"].as_str().unwrap_or(""), entry.pin),
            limit["label"].as_str().unwrap_or(""),
            values,
        )
        .step(1)
        .min(limit["min"].clone())
        .max(limit["max"].clone());
        if !unit.is_empty() {
            f = f.unit(unit);
        }
        if suffix == "FAIL" {
            f = f.hint("-1 keeps the default for the output's function.");
        }
        if let Some(v) = f.into_value() {
            fields.push(v);
        }
    }
    fields
}

/// A CW/CCW choice backed by the sign of a coefficient parameter. `CA_ROTORn_KM`
/// encodes the spin direction in its sign and the moment strength in its
/// magnitude, so the field is matched on the sign and written back as
/// `-value` or `+value`: flipping the propeller direction must never quietly
/// rewrite the tuned magnitude to 1.
fn sign(name: &str, label: &str, values: &Values) -> Field {
    let Some(&value) = values.get(name) else {
        return Field(None);
    };
    Field(Some(
        json!({
            "param": name, "label": label, "kind": "sign", "value": crate::json::num(value),
            "options": [{"value": 1, "label": "CCW"}, {"value": -1, "label": "CW"}],
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    ))
}

/// Which silhouette the page should draw behind the motors. An airframe class
/// this build has never heard of falls back to the multirotor drawing.
pub fn airframe_family(values: &Values) -> String {
    let Some(&raw) = values.get("CA_AIRFRAME") else {
        return DEFAULT_FAMILY.to_string();
    };
    tbl()
        .get("AIRFRAME_FAMILIES")
        .get(round_int(raw).to_string())
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_FAMILY)
        .to_string()
}

/// A rotor's thrust axis, or `None` when this firmware does not report one. PX4's
/// default is (0, 0, -1), straight up since body Z points down; a VTOL pusher is
/// (1, 0, 0), which is the distinction the drawing needs to show a propeller
/// edge on instead of as a lift disc.
fn axis(values: &Values, index: i64) -> Option<Value> {
    let names: Vec<String> = ["X", "Y", "Z"]
        .iter()
        .map(|a| format!("CA_ROTOR{index}_A{a}"))
        .collect();
    if !names.iter().any(|n| values.contains_key(n)) {
        return None;
    }
    let get = |i: usize| values.get(&names[i]).copied().unwrap_or(0.0);
    let (x, y, z) = (get(0), get(1), get(2));
    let length = (x * x + y * y + z * z).powf(0.5);
    if length < 1e-6 {
        return None;
    }
    let kind = if z.abs() >= x.abs().max(y.abs()) {
        "lift"
    } else {
        "horizontal"
    };
    Some(json!({
        "x": crate::json::num(x / length), "y": crate::json::num(y / length), "z": crate::json::num(z / length),
        "kind": kind,
    }))
}

/// How many motors the geometry has: the vehicle's `CA_ROTOR_COUNT`, and where
/// it has none the number of `CA_ROTOR*_PX` parameters that arrived.
pub fn rotor_count(values: &Values) -> i64 {
    if let Some(&raw) = values.get("CA_ROTOR_COUNT") {
        return round_int(raw).clamp(0, MAX_ROTORS);
    }
    (0..MAX_ROTORS)
        .filter(|i| values.contains_key(&format!("CA_ROTOR{i}_PX")))
        .count() as i64
}

/// Every output pin the vehicle answered for, with what it currently drives. A
/// bank the board does not have contributes nothing.
pub fn outputs(values: &Values) -> Vec<Output> {
    let mut entries = Vec::new();
    for b in banks_table() {
        let prefix = b["prefix"].as_str().unwrap_or("");
        let label = b["label"].as_str().unwrap_or("");
        for pin in 1..=b["pins"].as_i64().unwrap_or(0) {
            let param = format!("{prefix}_FUNC{pin}");
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

/// The banks that actually exist on this board, in table order.
pub fn banks(entries: &[Output]) -> Vec<Value> {
    banks_table()
        .iter()
        .filter(|b| entries.iter().any(|e| Some(e.bank.as_str()) == b["id"].as_str()))
        .map(|b| json!({"id": b["id"], "label": b["label"], "hint": b["hint"]}))
        .collect()
}

/// One entry per motor: where it sits, which way it turns, what drives it. The
/// position triple is the motor's offset from the centre of gravity in metres,
/// the distances the airframe diagram is drawn from. `CA_ROTORn_KM` carries the
/// spin direction in its sign (negative is clockwise).
fn motor_entries(values: &Values, entries: &[Output]) -> Vec<Value> {
    let mut motors = Vec::new();
    for i in 0..rotor_count(values) {
        let number_ = i + 1;
        let km = values.get(&format!("CA_ROTOR{i}_KM")).copied();
        let fields = present(vec![
            number(&format!("CA_ROTOR{i}_PX"), "X", values).unit("m").step(0.001),
            number(&format!("CA_ROTOR{i}_PY"), "Y", values).unit("m").step(0.001),
            number(&format!("CA_ROTOR{i}_PZ"), "Z", values).unit("m").step(0.001),
            sign(&format!("CA_ROTOR{i}_KM"), "Spin", values),
            number(&format!("CA_ROTOR{i}_CT"), "Thrust coef.", values).step(0.1),
        ]);
        let assigned = entries.iter().rev().find(|e| e.motor == Some(number_));
        let axis = axis(values, i);
        let mut position = Map::new();
        for a in ["X", "Y"] {
            let name = format!("CA_ROTOR{i}_P{a}");
            if values.contains_key(&name) {
                position.insert(a.to_lowercase(), json!(name));
            }
        }
        let at = |name: String| crate::json::num(values.get(&name).copied().unwrap_or(0.0));
        motors.push(json!({
            "index": i,
            "number": number_,
            "label": format!("Motor {number_}"),
            "x": at(format!("CA_ROTOR{i}_PX")),
            "y": at(format!("CA_ROTOR{i}_PY")),
            "z": at(format!("CA_ROTOR{i}_PZ")),
            "thrust": axis.as_ref().map_or(Value::Null, |a| a["kind"].clone()),
            "axis": axis,
            "position_params": if position.len() == 2 { Value::Object(position) } else { Value::Null },
            "spin": match km { None => Value::Null, Some(k) => json!(if k < 0.0 { "CW" } else { "CCW" }) },
            "fields": fields,
            "output_fields": output_fields(values, assigned),
            "output": assigned.map_or(Value::Null, |a| json!({
                "bank": a.bank, "pin": a.pin, "param": a.param, "label": a.label,
            })),
        }));
    }
    motors
}

const FRAME_NOTE_FEW: &str =
    "The frame size needs at least two lift rotors with positions. Set each motor's position below instead.";
const FRAME_NOTE_UNSET: &str = "No motor positions are set yet, so there is no size to scale. Set each motor's position below, or pick an airframe preset.";

/// A distance to a tenth of a millimetre: finer than anyone measures an arm.
fn metres(value: f64) -> Value {
    crate::json::num(py_round(value, 4))
}

/// The frame's measured size and the rotors a size change moves. Only lift rotors
/// count: a quadplane's pusher sits on the tail boom, and stretching the lift
/// frame must not move it. `length` is the extent along X, `width` along Y,
/// `diagonal` the largest distance between two rotors; an axis along which every
/// rotor sits in one line has no extent to scale and comes back as `null`.
pub fn frame(motors: &[Value]) -> Value {
    let lift: Vec<&Value> = motors
        .iter()
        .filter(|m| m["thrust"].as_str() != Some("horizontal") && !m["position_params"].is_null())
        .collect();
    if lift.len() < 2 {
        return json!({"adjustable": false, "note": FRAME_NOTE_FEW});
    }
    let f = |m: &Value, k: &str| m[k].as_f64().unwrap_or(0.0);
    let xs: Vec<f64> = lift.iter().map(|m| f(m, "x")).collect();
    let ys: Vec<f64> = lift.iter().map(|m| f(m, "y")).collect();
    let mut diagonal = f64::NEG_INFINITY;
    for (i, a) in lift.iter().enumerate() {
        for b in &lift[i + 1..] {
            let dx = f(a, "x") - f(b, "x");
            let dy = f(a, "y") - f(b, "y");
            diagonal = diagonal.max((dx * dx + dy * dy).powf(0.5));
        }
    }
    if diagonal < MIN_FRAME_M {
        return json!({"adjustable": false, "note": FRAME_NOTE_UNSET});
    }
    let max = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let min = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
    let length = max(&xs) - min(&xs);
    let width = max(&ys) - min(&ys);
    json!({
        "adjustable": true,
        "length": if length >= MIN_FRAME_M { metres(length) } else { Value::Null },
        "width": if width >= MIN_FRAME_M { metres(width) } else { Value::Null },
        "diagonal": metres(diagonal),
        "center": {"x": crate::json::num((max(&xs) + min(&xs)) / 2.0), "y": crate::json::num((max(&ys) + min(&ys)) / 2.0)},
        "motors": lift.iter().map(|m| json!({
            "number": m["number"], "x": m["x"], "y": m["y"],
            "x_param": m["position_params"]["x"], "y_param": m["position_params"]["y"],
        })).collect::<Vec<_>>(),
        "hint": tbl().get("FRAME_HINT"),
    })
}

fn protocol_options(values: &Values, name: &str) -> Vec<Value> {
    let mut options = tbl().options("PROTOCOL_OPTIONS");
    if let Some(&held) = values.get(name) {
        let held = round_int(held);
        options.extend(
            tbl()
                .array("BIDIR_PROTOCOL_OPTIONS")
                .iter()
                .filter(|o| o["value"].as_i64() == Some(held))
                .cloned(),
        );
    }
    options
}

/// Output protocol and endpoints for one bank (DShot, OneShot, PWM rate).
fn protocol(values: &Values, bank: &str) -> Vec<Value> {
    let mut fields = Vec::new();
    for i in 0..TIMER_GROUPS {
        let name = format!("PWM_{bank}_TIM{i}");
        if let Some(f) = enum_(
            &name,
            &format!("Timer group {i}"),
            values,
            &protocol_options(values, &name),
        )
        .into_value()
        {
            fields.push(f);
        }
    }
    // One limit for the whole bank: firmware before control allocation only. From
    // v1.14 the limits are per pin and shown on the motor they drive.
    for (suffix, label) in [
        ("MIN", "PWM minimum"),
        ("MAX", "PWM maximum"),
        ("DISARM", "PWM disarmed"),
    ] {
        if let Some(f) = number(&format!("PWM_{bank}_{suffix}"), label, values)
            .unit("us")
            .step(1)
            .into_value()
        {
            fields.push(f);
        }
    }
    fields
}

/// Turns the raw `{param: value}` into the Motors page description
/// (`GET /api/motors`): `sections` (output protocol per bank), `geometry` (the
/// airframe class and motor count), `airframe_family`, `motors` (position, spin,
/// per motor fields and the output each is wired to, with that output's limits
/// once [`output_param_names`] has been read too), `frame`, `outputs`, `banks`
/// and the counts.
pub fn build(values: &Values) -> Value {
    let mut sections: Vec<Value> = Vec::new();

    // CA_AIRFRAME is the airframe class the allocator solves for; it defines
    // what "Motor 3" even means and which silhouette the page draws, so it is
    // returned beside the drawing. SYS_AUTOSTART, the airframe preset, is a
    // different act: it rewrites a whole config bundle and needs a reboot, so it
    // is reported as read-only context.
    let geometry = present(vec![
        enum_(
            "CA_AIRFRAME",
            "Airframe type",
            values,
            &tbl().options("AIRFRAME_OPTIONS"),
        )
        .kv("reload", true)
        .hint("The geometry the control allocator solves for. Changing it redefines the motor layout."),
        number("CA_ROTOR_COUNT", "Motor count", values)
            .step(1)
            .min(0)
            .max(MAX_ROTORS)
            .kv("reload", true),
    ]);

    let mut main_protocol = protocol(values, "MAIN");
    if let Some(f) = enum_(
        "DSHOT_CONFIG",
        "DShot mode",
        values,
        &tbl().options("DSHOT_CONFIG_OPTIONS"),
    )
    .hint("Legacy DShot selector. Newer firmware uses the timer groups above.")
    .into_value()
    {
        main_protocol.push(f);
    }
    if let Some(f) = number("DSHOT_MIN", "DShot minimum output", values)
        .step(0.01)
        .min(0)
        .max(1)
        .hint("The lowest command a DShot ESC gets while armed, as a fraction of full. High enough that every motor keeps turning.")
        .into_value()
    {
        main_protocol.push(f);
    }
    if !main_protocol.is_empty() {
        sections.push(json!({
            "id": "protocol_main", "title": "Output protocol: MAIN",
            "fields": main_protocol,
            "hint": "Timer groups are shared by several outputs. A protocol change takes effect after a reboot of the autopilot.",
        }));
    }
    let aux_protocol = protocol(values, "AUX");
    if !aux_protocol.is_empty() {
        sections.push(json!({
            "id": "protocol_aux", "title": "Output protocol: AUX",
            "fields": aux_protocol,
        }));
    }

    let entries = outputs(values);
    let autostart = values.get("SYS_AUTOSTART").copied();
    let airframe = values.get("CA_AIRFRAME").copied();
    let motors = motor_entries(values, &entries);
    let airframe_label = tbl()
        .array("AIRFRAME_OPTIONS")
        .iter()
        .find(|o| airframe.is_some_and(|a| o["value"].as_i64() == Some(round_int(a))))
        .and_then(|o| o["label"].as_str())
        .unwrap_or("");
    json!({
        "sections": sections,
        "geometry": geometry,
        "frame": frame(&motors),
        "motors": motors,
        "outputs": entries.iter().map(Output::to_json).collect::<Vec<_>>(),
        "banks": banks(&entries),
        "airframe_family": airframe_family(values),
        "airframe_label": airframe_label,
        "airframe_preset": autostart.map_or(Value::Null, |a| json!(round_int(a))),
        "rotor_count": rotor_count(values),
        "max_motors": MAX_ROTORS,
        "received": values.len(),
    })
}
