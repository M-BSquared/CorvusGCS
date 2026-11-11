//! Field builders shared by the setup page schemas (`param_fields.py`).
//!
//! A field is `{param, label, kind, value, ...}` and the interface renders it
//! without knowing what the parameter means. A parameter the firmware did not
//! answer for gives no field, which is what lets one schema serve several
//! firmware versions.

use std::collections::HashMap;

use serde_json::Value;

use crate::json::{num, Map};

/// The parameters a vehicle answered for, name to value.
pub type Values = HashMap<String, f64>;

/// One option of an enum field, `{"value": n, "label": text}`.
pub fn opt(value: i64, label: &str) -> Value {
    let mut m = Map::new();
    m.insert("value".into(), Value::from(value));
    m.insert("label".into(), Value::from(label));
    Value::Object(m)
}

/// A table of `(value, label)` pairs as the list of options a field carries.
pub fn opts(table: &[(i64, &str)]) -> Vec<Value> {
    table.iter().map(|(v, l)| opt(*v, l)).collect()
}

/// `int(round(value))`: round half to even, and a value that is not a number
/// counts as 0 (Python would raise; a vehicle never sends one that is not).
pub fn round_int(value: f64) -> i64 {
    if value.is_finite() {
        value.round_ties_even() as i64
    } else {
        0
    }
}

fn option_value(option: &Value) -> i64 {
    match option.get("value") {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)).unwrap_or(0),
        _ => 0,
    }
}

/// Options guaranteed to contain `value`. A firmware may use an enum member
/// this build has never heard of, and snapping it to the first option would
/// rewrite a real aircraft's failsafe action the moment the operator touched
/// an unrelated field, so the unknown number is appended as its own option.
pub fn enum_options(options: &[Value], value: f64) -> Vec<Value> {
    let ivalue = round_int(value);
    let mut out = options.to_vec();
    if !options.iter().any(|o| option_value(o) == ivalue) {
        out.push(opt(ivalue, &format!("Unknown ({ivalue})")));
    }
    out
}

/// The label of the option holding `value`, or `Unknown (n)`.
pub fn option_label(options: &[Value], value: f64) -> String {
    let ivalue = round_int(value);
    for option in options {
        if option_value(option) == ivalue {
            return option.get("label").and_then(Value::as_str).unwrap_or("").to_string();
        }
    }
    format!("Unknown ({ivalue})")
}

/// A field, or nothing when the firmware did not answer for its parameter.
/// Every method is a no-op on nothing, so a builder chain reads like the
/// keyword arguments of the Python call.
#[derive(Clone, Debug, Default)]
pub struct Field(pub Option<Map>);

impl Field {
    /// Sets a key (`field.update(extra)`: an existing key keeps its place).
    pub fn kv(mut self, key: &str, value: impl Into<Value>) -> Field {
        if let Some(m) = self.0.as_mut() {
            m.insert(key.to_string(), value.into());
        }
        self
    }

    pub fn unit(self, v: &str) -> Field {
        self.kv("unit", v)
    }

    pub fn hint(self, v: &str) -> Field {
        self.kv("hint", v)
    }

    pub fn step(self, v: impl Into<Value>) -> Field {
        self.kv("step", v)
    }

    pub fn min(self, v: impl Into<Value>) -> Field {
        self.kv("min", v)
    }

    pub fn max(self, v: impl Into<Value>) -> Field {
        self.kv("max", v)
    }

    pub fn into_value(self) -> Option<Value> {
        self.0.map(Value::Object)
    }
}

fn base(name: &str, label: &str, kind: &str, value: f64) -> Map {
    let mut m = Map::new();
    m.insert("param".into(), Value::from(name));
    m.insert("label".into(), Value::from(label));
    m.insert("kind".into(), Value::from(kind));
    m.insert("value".into(), num(value));
    m
}

pub fn enum_(name: &str, label: &str, values: &Values, options: &[Value]) -> Field {
    let Some(&value) = values.get(name) else {
        return Field(None);
    };
    let mut m = base(name, label, "enum", value);
    m.insert("options".into(), Value::Array(enum_options(options, value)));
    Field(Some(m))
}

pub fn number(name: &str, label: &str, values: &Values) -> Field {
    match values.get(name) {
        Some(&value) => Field(Some(base(name, label, "number", value))),
        None => Field(None),
    }
}

/// A parameter whose bits are independent switches (`FENCE_TYPE`, `FS_OPTIONS`,
/// `ARMING_CHECK`): rendering one as a number asks an operator to do binary
/// arithmetic on their aircraft's safety settings.
pub fn bitmask(name: &str, label: &str, values: &Values, bits: Vec<Value>) -> Field {
    let Some(&value) = values.get(name) else {
        return Field(None);
    };
    let mut m = base(name, label, "bitmask", value);
    m.insert("bits".into(), Value::Array(bits));
    Field(Some(m))
}

/// The fields that exist.
pub fn present(fields: Vec<Field>) -> Vec<Value> {
    fields.into_iter().filter_map(Field::into_value).collect()
}

pub const FLOW_WITHOUT_RANGE: &str = "Flow is set up, but no distance sensor is on. Without a height above the ground the flow rate cannot be turned into a speed.";

fn by_id<'a>(sections: &'a mut [Value], id: &str) -> Option<&'a mut Value> {
    sections
        .iter_mut()
        .find(|s| s.get("id").and_then(Value::as_str) == Some(id))
}

/// Marks a complete flow setup as partial while the rangefinder is not on.
/// The flow switch stays on, because its own chain is complete; what is
/// missing is on the other sensor's page, and the overview has to say so.
pub fn flag_flow_without_range(sections: &mut [Value]) {
    let range_on = by_id(sections, "rangefinder")
        .and_then(|s| s.get("toggle"))
        .and_then(|t| t.get("state"))
        .and_then(Value::as_str)
        == Some("on");
    let Some(flow) = by_id(sections, "flow") else {
        return;
    };
    let Some(toggle) = flow.get_mut("toggle").and_then(Value::as_object_mut) else {
        return;
    };
    if toggle.get("state").and_then(Value::as_str) != Some("on") {
        return;
    }
    if range_on {
        return;
    }
    toggle.insert("state".into(), Value::from("partial"));
    let mut problems = match toggle.get("problems") {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    problems.push(Value::from(FLOW_WITHOUT_RANGE));
    toggle.insert("problems".into(), Value::Array(problems));
}

/// A plain form section, or nothing when no field in it survived.
pub fn section(id: &str, title: &str, fields: Vec<Value>, hint: &str) -> Option<Map> {
    if fields.is_empty() {
        return None;
    }
    let mut m = Map::new();
    m.insert("id".into(), Value::from(id));
    m.insert("title".into(), Value::from(title));
    m.insert("kind".into(), Value::from("fields"));
    m.insert("fields".into(), Value::Array(fields));
    if !hint.is_empty() {
        m.insert("hint".into(), Value::from(hint));
    }
    Some(m)
}

/// `Values` from `(name, value)` pairs, for tests and callers that hold a list.
pub fn values_of<I, S>(pairs: I) -> Values
where
    I: IntoIterator<Item = (S, f64)>,
    S: Into<String>,
{
    pairs.into_iter().map(|(k, v)| (k.into(), v)).collect()
}
