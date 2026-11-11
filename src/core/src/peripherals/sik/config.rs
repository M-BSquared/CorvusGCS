//! SiK telemetry radio schema (`sik_config.py`): what a radio's AT replies
//! mean and which writes are acceptable. The register table is data, dumped
//! from the Python module into `registers.json`.

use std::sync::OnceLock;

use serde_json::{json, Map, Value};

use crate::peripherals::util::py_strip;
use crate::protocol::common::py_round;

const TABLE: &str = include_str!("registers.json");

pub const DEFAULT_BAUD: i64 = 57600;

/// A requested write is not one this radio will accept; the words are the
/// operator's, and the route turns them into a 400.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SikConfigError(pub String);

impl std::fmt::Display for SikConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

struct Table {
    registers: Vec<Map<String, Value>>,
    must_match: Vec<String>,
    read_only: Vec<String>,
    bauds: Vec<i64>,
    serial_speeds: Vec<(i64, i64)>,
}

fn table() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| {
        let doc: Value = serde_json::from_str(TABLE).expect("registers.json");
        let strings = |k: &str| -> Vec<String> {
            doc[k]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        Table {
            registers: doc["registers"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_object().cloned()).collect())
                .unwrap_or_default(),
            must_match: strings("must_match"),
            read_only: strings("read_only"),
            bauds: doc["bauds"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default(),
            serial_speeds: doc["serial_speeds"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|p| Some((p[0].as_i64()?, p[1].as_i64()?)))
                        .collect()
                })
                .unwrap_or_default(),
        }
    })
}

/// The bauds a radio is looked for at, the caller's own first.
pub fn baud_candidates() -> &'static [i64] {
    &table().bauds
}

fn is_must_match(name: &str) -> bool {
    table().must_match.iter().any(|n| n == name)
}

fn is_read_only(name: &str) -> bool {
    table().read_only.iter().any(|n| n == name)
}

fn spec_of(name: &str) -> Option<&'static Map<String, Value>> {
    table().registers.iter().find(|r| r["name"] == name)
}

/// The real rate a SERIAL_SPEED register stands for.
pub fn baud_from_register(value: i64) -> i64 {
    if value > 2000 {
        return value;
    }
    table()
        .serial_speeds
        .iter()
        .find(|(r, _)| *r == value)
        .map(|(_, b)| *b)
        .unwrap_or(value * 1000)
}

/// The inverse, for a real baud rate.
pub fn register_from_baud(baud: i64) -> i64 {
    if let Some((r, _)) = table().serial_speeds.iter().find(|(_, b)| *b == baud) {
        return *r;
    }
    if baud >= 2000 {
        return baud.div_euclid(1000);
    }
    baud
}

/// A raw RSSI count in dBm: `(rssi / 1.9) - 127`.
pub fn rssi_to_dbm(rssi: f64) -> f64 {
    rssi / 1.9 - 127.0
}

/// `^\s*S(\d+)\s*[:=]?\s*([A-Za-z0-9_]+)\s*=\s*(-?\d+)\s*$` with the
/// regular expression's own backtracking (`S12=5` reads as register 1, name
/// `2`).
fn register_line(line: &str) -> Option<(i64, String, i64)> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    if chars.get(i) != Some(&'S') {
        return None;
    }
    let digits_start = i + 1;
    let mut digits_end = digits_start;
    while digits_end < chars.len() && chars[digits_end].is_ascii_digit() {
        digits_end += 1;
    }
    for end in (digits_start + 1..=digits_end).rev() {
        let mut j = end;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if j < chars.len() && (chars[j] == ':' || chars[j] == '=') {
            j += 1;
        }
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let name_start = j;
        while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
            j += 1;
        }
        if j == name_start {
            continue;
        }
        let name: String = chars[name_start..j].iter().collect();
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if chars.get(j) != Some(&'=') {
            continue;
        }
        j += 1;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let value_start = j;
        if chars.get(j) == Some(&'-') {
            j += 1;
        }
        let num_start = j;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        if j == num_start {
            continue;
        }
        let value: String = chars[value_start..j].iter().collect();
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if j != chars.len() {
            continue;
        }
        let number: String = chars[digits_start..end].iter().collect();
        return Some((number.parse().ok()?, name, value.parse().ok()?));
    }
    None
}

fn lines_of(text: &str) -> Vec<String> {
    text.replace('\r', "\n").split('\n').map(str::to_string).collect()
}

/// An `ATI5` / `RTI5` report as `{name: {number, value}}`.
pub fn parse_registers(text: &str) -> Map<String, Value> {
    let mut out = Map::new();
    for line in lines_of(text) {
        if let Some((number, name, value)) = register_line(&line) {
            out.insert(name.to_uppercase(), json!({"number": number, "value": value}));
        }
    }
    out
}

fn match_ci(chars: &[char], at: usize, word: &str) -> bool {
    let w: Vec<char> = word.chars().collect();
    at + w.len() <= chars.len()
        && chars[at..at + w.len()]
            .iter()
            .zip(&w)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn skip_ws(chars: &[char], mut at: usize) -> usize {
    while at < chars.len() && chars[at].is_whitespace() {
        at += 1;
    }
    at
}

fn uint(chars: &[char], at: usize) -> Option<(i64, usize)> {
    let mut j = at;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    if j == at {
        return None;
    }
    chars[at..j].iter().collect::<String>().parse().ok().map(|n| (n, j))
}

/// `X\s*(\d+)\s*/\s*(\d+)` after the label `head` (`L/R\s+RSSI:` and so on).
fn pair_after(chars: &[char], at: usize, head: &str) -> Option<(i64, i64, usize)> {
    // `L/R` then at least one white space then the label then `:`.
    let mut j = at;
    if !match_ci(chars, j, "L/R") {
        return None;
    }
    j += 3;
    let before = j;
    j = skip_ws(chars, j);
    if j == before {
        return None;
    }
    if !match_ci(chars, j, head) {
        return None;
    }
    j += head.chars().count();
    j = skip_ws(chars, j);
    let (a, j2) = uint(chars, j)?;
    let mut j = skip_ws(chars, j2);
    if chars.get(j) != Some(&'/') {
        return None;
    }
    j = skip_ws(chars, j + 1);
    let (b, j3) = uint(chars, j)?;
    Some((a, b, j3))
}

/// An `ATI7` link report as signal, noise and fade margin, or `None`.
pub fn parse_rssi(text: &str) -> Option<Value> {
    let chars: Vec<char> = text.chars().collect();
    for start in 0..chars.len() {
        let Some((l_rssi, r_rssi, end)) = pair_after(&chars, start, "RSSI:") else {
            continue;
        };
        for t in end..chars.len() {
            if let Some((l_noise, r_noise, _)) = pair_after(&chars, t, "noise:") {
                let dbm = |c: i64| py_round(rssi_to_dbm(c as f64), 1);
                let margin = |s: i64, n: i64| py_round((s - n) as f64 / 2.0, 1);
                return Some(json!({
                    "local_rssi": l_rssi,
                    "remote_rssi": r_rssi,
                    "local_noise": l_noise,
                    "remote_noise": r_noise,
                    "local_dbm": dbm(l_rssi),
                    "remote_dbm": dbm(r_rssi),
                    "local_margin_db": margin(l_rssi, l_noise),
                    "remote_margin_db": margin(r_rssi, r_noise),
                }));
            }
        }
    }
    None
}

/// The firmware banner of an `ATI` reply, or "".
pub fn parse_version(text: &str) -> String {
    for line in lines_of(text) {
        let candidate = py_strip(&line);
        if candidate.is_empty() {
            continue;
        }
        let upper = candidate.to_uppercase();
        if upper == "OK" || upper == "ERROR" || upper.starts_with("AT") || upper.starts_with("RT") {
            continue;
        }
        return candidate.to_string();
    }
    String::new()
}

fn field(spec: &Map<String, Value>, name: &str, entry: &Value) -> Value {
    let value = entry["value"].as_i64().unwrap_or(0);
    let kind = spec.get("kind").and_then(Value::as_str).unwrap_or("number");
    let mut f = Map::new();
    f.insert("name".into(), json!(name));
    f.insert("register".into(), json!(entry["number"].as_i64().unwrap_or(0)));
    f.insert("label".into(), spec.get("label").cloned().unwrap_or(json!(name)));
    f.insert("kind".into(), json!(kind));
    f.insert("value".into(), json!(value));
    f.insert(
        "advanced".into(),
        json!(spec.get("advanced").is_some_and(|a| a.as_bool() == Some(true))),
    );
    f.insert("must_match".into(), json!(is_must_match(name)));
    f.insert("read_only".into(), json!(is_read_only(name) || kind == "readonly"));
    for key in ["unit", "min", "max", "step", "hint"] {
        if let Some(v) = spec.get(key) {
            f.insert(key.into(), v.clone());
        }
    }
    if kind == "enum" {
        let mut options: Vec<Value> = spec
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !options.iter().any(|o| o["value"].as_i64() == Some(value)) {
            options.push(json!({"value": value, "label": format!("Unknown ({value})")}));
        }
        f.insert("options".into(), Value::Array(options));
    }
    Value::Object(f)
}

/// Turns parsed registers into the field list for one end of the link: the
/// table's order first, then whatever the radio had that the table does not
/// know about, by register number.
pub fn describe_radio(registers: &Map<String, Value>, version: &str, board: &str) -> Value {
    let mut fields = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for spec in &table().registers {
        let name = spec["name"].as_str().unwrap_or("");
        if let Some(entry) = registers.get(name) {
            seen.insert(name.to_string());
            fields.push(field(spec, name, entry));
        }
    }
    let mut rest: Vec<(&String, &Value)> = registers.iter().filter(|(k, _)| !seen.contains(*k)).collect();
    rest.sort_by_key(|(_, e)| e["number"].as_i64().unwrap_or(0));
    for (name, entry) in rest {
        let label: String = {
            let spaced = name.replace('_', " ");
            title_case(&spaced)
        };
        let spec = json!({
            "name": name, "label": label, "kind": "number", "advanced": true,
            "hint": "Reported by this radio's firmware; not described by Corvus."
        });
        fields.push(field(spec.as_object().expect("object"), name, entry));
    }
    let values: Map<String, Value> = registers.iter().map(|(k, e)| (k.clone(), e["value"].clone())).collect();
    json!({"version": version, "board": board, "fields": fields, "values": values})
}

/// `str.title()`: each run of letters starts upper case, the rest lower.
fn title_case(s: &str) -> String {
    let mut out = String::new();
    let mut prev_alpha = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            if prev_alpha {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            prev_alpha = true;
        } else {
            out.push(c);
            prev_alpha = false;
        }
    }
    out
}

fn values_of(radio: &Value) -> Map<String, Value> {
    radio
        .get("values")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn truthy(v: Option<&Value>) -> bool {
    v.is_some_and(crate::protocol::common::py_truthy)
}

/// Registers that must match at both ends and do not.
pub fn compare(local: &Value, remote: Option<&Value>) -> Vec<Value> {
    let Some(remote) = remote.filter(|r| truthy(Some(r))) else {
        return Vec::new();
    };
    let (lv, rv) = (values_of(local), values_of(remote));
    let mut out = Vec::new();
    for spec in &table().registers {
        let name = spec["name"].as_str().unwrap_or("");
        if !is_must_match(name) {
            continue;
        }
        let (Some(l), Some(r)) = (
            lv.get(name).and_then(Value::as_i64),
            rv.get(name).and_then(Value::as_i64),
        ) else {
            continue;
        };
        if l == r {
            continue;
        }
        out.push(
            json!({"name": name, "label": spec.get("label").cloned().unwrap_or(json!(name)), "local": l, "remote": r}),
        );
    }
    out
}

/// The values that would make the remote radio agree with the local one.
pub fn copy_to_remote(local: &Value, remote: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(remote) = remote.filter(|r| truthy(Some(r))) else {
        return out;
    };
    let (lv, rv) = (values_of(local), values_of(remote));
    let mut names: Vec<&String> = table().must_match.iter().collect();
    names.sort();
    for name in names {
        if let (Some(l), Some(r)) = (
            lv.get(name).and_then(Value::as_i64),
            rv.get(name).and_then(Value::as_i64),
        ) {
            if l != r {
                out.insert(name.clone(), json!(l));
            }
        }
    }
    out
}

fn check_against_spec(spec: &Map<String, Value>, key: &str, value: i64) -> Result<(), SikConfigError> {
    let label = spec.get("label").and_then(Value::as_str).unwrap_or(key);
    if spec.get("kind").and_then(Value::as_str) == Some("enum") {
        let allowed: Vec<i64> = spec
            .get("options")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|o| o["value"].as_i64()).collect())
            .unwrap_or_default();
        if !allowed.contains(&value) {
            let names: Vec<String> = allowed.iter().map(i64::to_string).collect();
            return Err(SikConfigError(format!(
                "{label} does not accept {value}; allowed values are {}",
                names.join(", ")
            )));
        }
        return Ok(());
    }
    if let Some(low) = spec.get("min").and_then(Value::as_i64) {
        if value < low {
            return Err(SikConfigError(format!("{label} must be at least {low}")));
        }
    }
    if let Some(high) = spec.get("max").and_then(Value::as_i64) {
        if value > high {
            return Err(SikConfigError(format!("{label} must be at most {high}")));
        }
    }
    Ok(())
}

/// Refuse a frequency band that is inverted or empty, looked at across the
/// pair because either half may be absent from the request.
fn check_band(settings: &Map<String, Value>, present: &Map<String, Value>) -> Result<(), SikConfigError> {
    let resolve = |name: &str| -> Option<i64> {
        if let Some(v) = settings.get(name) {
            return match v {
                Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
                Value::String(s) => crate::protocol::common::py_int_of(&Value::String(s.clone())),
                Value::Bool(b) => Some(i64::from(*b)),
                _ => None,
            };
        }
        present.get(name).and_then(|e| e["value"].as_i64())
    };
    if let (Some(low), Some(high)) = (resolve("MIN_FREQ"), resolve("MAX_FREQ")) {
        if high <= low {
            return Err(SikConfigError(
                "the maximum frequency must be above the minimum frequency".into(),
            ));
        }
    }
    Ok(())
}

/// Checks requested values against the radio in front of us, ordered for
/// writing; unchanged registers are dropped.
pub fn validate_writes(
    settings: &Map<String, Value>,
    present: &Map<String, Value>,
) -> Result<Vec<Value>, SikConfigError> {
    let mut writes: Vec<Value> = Vec::new();
    for (name, raw) in settings {
        let key = name.to_uppercase();
        let Some(entry) = present.get(&key) else {
            return Err(SikConfigError(format!("this radio has no {key} setting")));
        };
        if is_read_only(&key) {
            return Err(SikConfigError(format!(
                "{key} is set by the radio firmware and cannot be written"
            )));
        }
        let Value::Number(n) = raw else {
            return Err(SikConfigError(format!("{key} must be a number")));
        };
        let value = match n.as_i64() {
            Some(i) => i,
            None => match n.as_f64() {
                Some(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e18 => f as i64,
                Some(f) if f.is_finite() && f.fract() == 0.0 => {
                    return Err(SikConfigError(format!(
                        "{key} must be at {} {}",
                        if f > 0.0 { "most" } else { "least" },
                        spec_of(&key)
                            .and_then(|s| s.get(if f > 0.0 { "max" } else { "min" }))
                            .map(|v| v.to_string())
                            .unwrap_or_default()
                    )))
                }
                _ => return Err(SikConfigError(format!("{key} must be a whole number"))),
            },
        };
        if let Value::Number(n) = raw {
            if n.as_i64().is_none() && n.as_f64().is_some_and(|f| f.fract() != 0.0) {
                return Err(SikConfigError(format!("{key} must be a whole number")));
            }
        }
        if let Some(spec) = spec_of(&key) {
            check_against_spec(spec, &key, value)?;
        }
        if value != entry["value"].as_i64().unwrap_or(i64::MIN) {
            writes.push(json!({"name": key, "register": entry["number"].as_i64().unwrap_or(0), "value": value}));
        }
    }
    check_band(settings, present)?;
    writes.sort_by_key(|w| w["register"].as_i64().unwrap_or(0));
    Ok(writes)
}

/// The AT command sequence that commits `writes`: `ATSn=X` for each register,
/// then `AT&W` to move them into EEPROM, then `ATZ` to reboot into them, with
/// `RT` for the far radio.
pub fn write_plan(writes: &[Value], remote: bool) -> Vec<String> {
    let prefix = if remote { "RT" } else { "AT" };
    let mut commands: Vec<String> = writes
        .iter()
        .map(|w| format!("{prefix}S{}={}", w["register"], w["value"]))
        .collect();
    if commands.is_empty() {
        return commands;
    }
    commands.push(format!("{prefix}&W"));
    commands.push(format!("{prefix}Z"));
    commands
}

/// The static half of the page description: the table, without a radio.
pub fn schema() -> Value {
    let registers: Vec<Value> = table()
        .registers
        .iter()
        .map(|spec| {
            let name = spec["name"].as_str().unwrap_or("");
            let kind = spec.get("kind").and_then(Value::as_str).unwrap_or("number");
            let mut m = Map::new();
            m.insert("name".into(), json!(name));
            m.insert("label".into(), spec.get("label").cloned().unwrap_or(json!(name)));
            m.insert("kind".into(), json!(kind));
            m.insert(
                "advanced".into(),
                json!(spec.get("advanced").is_some_and(|a| a.as_bool() == Some(true))),
            );
            m.insert("must_match".into(), json!(is_must_match(name)));
            m.insert("read_only".into(), json!(is_read_only(name) || kind == "readonly"));
            for key in ["unit", "min", "max", "step", "hint", "options"] {
                if let Some(v) = spec.get(key) {
                    m.insert(key.into(), v.clone());
                }
            }
            Value::Object(m)
        })
        .collect();
    let mut must: Vec<&String> = table().must_match.iter().collect();
    must.sort();
    json!({
        "registers": registers,
        "must_match": must,
        "bauds": table().bauds,
        "default_baud": DEFAULT_BAUD,
    })
}

/// `{name: {number, value}}` rebuilt from a described radio.
pub fn registers_of(described: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(fields) = described.get("fields").and_then(Value::as_array) {
        for f in fields {
            if let Some(name) = f["name"].as_str() {
                out.insert(name.to_string(), json!({"number": f["register"], "value": f["value"]}));
            }
        }
    }
    out
}
