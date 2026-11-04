//! What the Telemetry Radio page says and sends (`src/js/setup-sik.js`): the
//! port and baud pickers, the gate, the radio as the backend described it, the
//! edits staged per side, and the request bodies. The radio's register table
//! comes from the backend; the page owns three decisions only: what it sends,
//! when it refuses to send anything, and whether an edit shows as an edit
//! before it is written.

use serde_json::{json, Map, Value};

use crate::data::units::js_number;

pub const COLUMNS_INTRO: &str = "Left is the radio on the cable in front of you, normally the ground station's. Right is the one at the other end of the link, reached over the air through the near one. It needs power at both ends and a solid green LED on both radios.";

pub const PORT_INTRO: &str = "Pick the serial port the radio is on and press Load. While the radio is in command mode it is not relaying telemetry, so if this is the port the live link runs on, Corvus stops the link for the few seconds a session takes and reconnects afterwards.";

pub const ARMED_REASON: &str =
    "Radio configuration is refused while the vehicle is armed. A radio in command mode is not relaying telemetry.";

pub const FIRMWARE_WARNING: &str = "The two radios are running different firmware. A pair has to match on firmware version as well as on settings; update whichever is older.";

pub const MISMATCH_INTRO: &str = "These settings have to be identical at both ends or the radios cannot hear each other. Copying stages this radio's values into the remote column; nothing is written until you press Save.";

pub const UNREACHABLE: &str = "The remote radio did not answer. It is reached through this one over the air, so it needs power at the far end and a pair that still agrees on the air settings. A solid green LED on both radios means the link is up.";

pub const STAGED_NOTE: &str =
    "Staged in the remote column. Press Save to write them. The remote radio is written first, before this one.";

/// The two ends of the link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Local,
    Remote,
}

impl Side {
    pub const ALL: [Side; 2] = [Side::Local, Side::Remote];

    pub fn title(self) -> &'static str {
        match self {
            Side::Local => "This radio",
            Side::Remote => "Remote radio",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Side::Local => "local",
            Side::Remote => "remote",
        }
    }

    fn index(self) -> usize {
        match self {
            Side::Local => 0,
            Side::Remote => 1,
        }
    }

    /// What a reset names: `the remote radio`, `this radio`.
    pub fn label(self) -> &'static str {
        match self {
            Side::Local => "this radio",
            Side::Remote => "the remote radio",
        }
    }
}

/// One entry of the port picker.
#[derive(Debug, Clone, PartialEq)]
pub struct Port {
    pub device: String,
    pub label: String,
    pub is_link: bool,
    pub radio: bool,
}

/// The ports of GET /api/sik/status, labelled as `fillPorts` labels them.
pub fn ports(status: &Value) -> Vec<Port> {
    let rows = status
        .get("ports")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    rows.iter()
        .map(|p| {
            let device = p.get("device").and_then(Value::as_str).unwrap_or("").to_string();
            let is_link = p.get("is_link").and_then(Value::as_bool).unwrap_or(false);
            let radio = p.get("kind").and_then(Value::as_str) == Some("sik");
            let mut marks: Vec<&str> = Vec::new();
            if is_link {
                marks.push("live link");
            } else if radio {
                marks.push("radio");
            }
            let description = p.get("description").and_then(Value::as_str).unwrap_or("");
            if !description.is_empty() {
                marks.push(description);
            }
            let label = if marks.is_empty() {
                device.clone()
            } else {
                format!("{device} ({})", marks.join(", "))
            };
            Port {
                device,
                label,
                is_link,
                radio,
            }
        })
        .collect()
}

/// The port to preselect: what is selected, else the live link's own port
/// (the radio an operator wants to configure is the one they are talking to),
/// else a radio, else the first.
pub fn preferred_port(ports: &[Port], previous: &str) -> String {
    if !previous.is_empty() {
        return previous.to_string();
    }
    ports
        .iter()
        .find(|p| p.is_link)
        .or_else(|| ports.iter().find(|p| p.radio))
        .or_else(|| ports.first())
        .map(|p| p.device.clone())
        .unwrap_or_default()
}

/// The baud rates on offer.
pub fn bauds(status: &Value) -> Vec<i64> {
    let list: Vec<i64> = status
        .get("bauds")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    if list.is_empty() {
        vec![57600]
    } else {
        list
    }
}

/// The baud to preselect (`fillBauds`): the link's own on its own port.
pub fn preferred_baud(status: &Value, port: &str) -> i64 {
    let link_baud = status.get("link_baud").and_then(Value::as_i64).unwrap_or(0);
    let link_device = status.get("link_device").and_then(Value::as_str).unwrap_or("");
    if !port.is_empty() && link_device == port && link_baud != 0 {
        return link_baud;
    }
    status
        .get("default_baud")
        .and_then(Value::as_i64)
        .filter(|b| *b != 0)
        .unwrap_or(57600)
}

/// Why the actions are shut, or empty (`recomputeGate`): arming closes the gate
/// at once, the backend's own reason otherwise.
pub fn gate_reason(armed: bool, status: &Value) -> String {
    if armed {
        return ARMED_REASON.to_string();
    }
    if status.get("can_configure") == Some(&Value::Bool(false)) {
        return status
            .get("blocked_reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
    }
    String::new()
}

/// Whether the actions are shut.
pub fn blocked(reason: &str, busy: bool, port: &str) -> bool {
    !reason.is_empty() || busy || port.is_empty()
}

/// `target()`: the device and, when it is a number, the baud.
pub fn target(port: &str, baud: i64) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("device".into(), json!(port));
    if baud > 0 {
        body.insert("baud".into(), json!(baud));
    }
    body
}

/// The body of POST /api/sik/load.
pub fn load_body(port: &str, baud: i64, remote: bool) -> Value {
    let mut b = target(port, baud);
    b.insert("remote".into(), json!(remote));
    Value::Object(b)
}

/// The body of POST /api/sik/reset.
pub fn reset_body(port: &str, baud: i64, side: Side) -> Value {
    let mut b = target(port, baud);
    b.insert("target".into(), json!(side.id()));
    Value::Object(b)
}

/// `parseInt(text, 10)`: leading whitespace, a sign, then digits.
pub fn parse_int(text: &str) -> Option<i64> {
    let t = text.trim_start();
    let (neg, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let n: i64 = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

/// How a register is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Enum,
    Number,
}

/// One register of a radio.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub label: String,
    pub kind: Kind,
    pub value: f64,
    pub advanced: bool,
    pub must_match: bool,
    pub read_only: bool,
    pub options: Vec<(i64, String)>,
    pub unit: String,
    pub hint: String,
}

impl Field {
    fn from_json(f: &Value) -> Field {
        let text = |k: &str| f.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let flag = |k: &str| f.get(k).and_then(Value::as_bool).unwrap_or(false);
        Field {
            name: text("name"),
            label: text("label"),
            kind: if text("kind") == "enum" {
                Kind::Enum
            } else {
                Kind::Number
            },
            value: f.get("value").and_then(Value::as_f64).unwrap_or(0.0),
            advanced: flag("advanced"),
            must_match: flag("must_match"),
            read_only: flag("read_only"),
            options: f
                .get("options")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|o| {
                            (
                                o.get("value").and_then(Value::as_i64).unwrap_or(0),
                                o.get("label").and_then(Value::as_str).unwrap_or("").to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            unit: text("unit"),
            hint: text("hint"),
        }
    }

    /// The caption: the label and its unit in brackets.
    pub fn caption(&self) -> String {
        if self.unit.is_empty() {
            self.label.clone()
        } else {
            format!("{} ({})", self.label, self.unit)
        }
    }

    /// Whether the field is on screen.
    pub fn shown(&self, advanced: bool) -> bool {
        !self.advanced || advanced
    }
}

/// One end of the link as the radio reported it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Radio {
    pub version: String,
    pub board: String,
    pub fields: Vec<Field>,
}

impl Radio {
    fn from_json(r: &Value) -> Option<Radio> {
        if !r.is_object() {
            return None;
        }
        Some(Radio {
            version: r.get("version").and_then(Value::as_str).unwrap_or("").into(),
            board: r.get("board").and_then(Value::as_str).unwrap_or("").into(),
            fields: r
                .get("fields")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(Field::from_json).collect())
                .unwrap_or_default(),
        })
    }
}

/// A setting that differs between the two radios.
#[derive(Debug, Clone, PartialEq)]
pub struct Mismatch {
    pub name: String,
    pub label: String,
    pub local: Value,
    pub remote: Value,
}

impl Mismatch {
    /// The line of the disagreement list.
    pub fn sentence(&self) -> String {
        format!(
            "{}: {} here, {} on the remote radio",
            self.label,
            show(&self.local),
            show(&self.remote)
        )
    }
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => js_number(n.as_f64().unwrap_or(f64::NAN)),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The link report: signal, noise and fade margin of both ends.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub rssi: String,
    pub noise: String,
    pub margin: String,
}

fn num(link: &Value, key: &str) -> String {
    link.get(key)
        .and_then(Value::as_f64)
        .map(js_number)
        .unwrap_or_else(|| "undefined".into())
}

impl Link {
    fn from_json(l: &Value) -> Option<Link> {
        if !l.is_object() {
            return None;
        }
        Some(Link {
            rssi: format!(
                "{} / {}  ({} / {} dBm)",
                num(l, "local_rssi"),
                num(l, "remote_rssi"),
                num(l, "local_dbm"),
                num(l, "remote_dbm")
            ),
            noise: format!("{} / {}", num(l, "local_noise"), num(l, "remote_noise")),
            margin: format!("{} / {} dB", num(l, "local_margin_db"), num(l, "remote_margin_db")),
        })
    }
}

/// Everything the page has read: the answer of POST /api/sik/load.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub local: Option<Radio>,
    pub remote: Option<Radio>,
    pub remote_reachable: bool,
    pub mismatches: Vec<Mismatch>,
    pub link: Option<Link>,
}

impl Loaded {
    pub fn from_json(d: &Value) -> Loaded {
        Loaded {
            local: d.get("local").and_then(Radio::from_json),
            remote: d.get("remote").and_then(Radio::from_json),
            remote_reachable: d.get("remote_reachable").and_then(Value::as_bool).unwrap_or(false),
            mismatches: d
                .get("mismatches")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|m| Mismatch {
                            name: m.get("name").and_then(Value::as_str).unwrap_or("").into(),
                            label: m.get("label").and_then(Value::as_str).unwrap_or("").into(),
                            local: m.get("local").cloned().unwrap_or(Value::Null),
                            remote: m.get("remote").cloned().unwrap_or(Value::Null),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            link: d.get("link").and_then(Link::from_json),
        }
    }

    pub fn radio(&self, side: Side) -> Option<&Radio> {
        match side {
            Side::Local => self.local.as_ref(),
            Side::Remote => self.remote.as_ref(),
        }
    }

    /// Whether the two firmware versions are both known and differ.
    pub fn firmware_differs(&self) -> bool {
        match (&self.local, &self.remote) {
            (Some(l), Some(r)) => !l.version.is_empty() && !r.version.is_empty() && l.version != r.version,
            _ => false,
        }
    }

    /// The text of the status line after a load, and its tone.
    pub fn summary(&self, asked_remote: bool) -> (&'static str, String) {
        if !self.mismatches.is_empty() {
            (
                "warn",
                format!(
                    "Settings loaded. {} setting(s) differ between the two radios.",
                    self.mismatches.len()
                ),
            )
        } else if asked_remote && !self.remote_reachable {
            (
                "ok",
                "This radio's settings loaded; the remote radio did not answer.".into(),
            )
        } else {
            ("ok", "Settings loaded.".into())
        }
    }
}

/// The settings staged per side: only these are sent, so a save never rewrites
/// a register the operator did not touch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Edits {
    sides: [Vec<(String, Value)>; 2],
}

impl Edits {
    pub fn clear(&mut self) {
        self.sides = Default::default();
    }

    pub fn get(&self, side: Side, name: &str) -> Option<&Value> {
        self.sides[side.index()].iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    pub fn count(&self) -> usize {
        self.sides[0].len() + self.sides[1].len()
    }

    pub fn is_dirty(&self, side: Side, name: &str) -> bool {
        self.get(side, name).is_some()
    }

    fn put(&mut self, side: Side, name: &str, value: Option<Value>) {
        let list = &mut self.sides[side.index()];
        list.retain(|(n, _)| n != name);
        if let Some(v) = value {
            list.push((name.to_string(), v));
        }
    }

    /// `stage`: records one edit, or drops it when the original was typed back
    /// (an unchanged register in the batch would still cost a reboot). A value
    /// that is not a number is ignored.
    pub fn stage(&mut self, side: Side, field: &Field, value: Option<i64>) {
        let Some(v) = value else {
            return;
        };
        if (v as f64 - field.value).abs() < f64::EPSILON {
            self.put(side, &field.name, None);
        } else {
            self.put(side, &field.name, Some(json!(v)));
        }
    }

    /// `copyToRemote`: stages this radio's value of every setting that differs.
    pub fn copy_to_remote(&mut self, mismatches: &[Mismatch]) {
        for m in mismatches {
            self.put(Side::Remote, &m.name, Some(m.local.clone()));
        }
    }

    /// The value on screen for a field: the staged one, else the radio's.
    pub fn shown_value(&self, side: Side, field: &Field) -> Value {
        match self.get(side, &field.name) {
            Some(v) => v.clone(),
            None => json!(field.value),
        }
    }

    /// The label of the save button.
    pub fn save_label(&self) -> String {
        match self.count() {
            0 => "Save".into(),
            n => format!("Save {n} change(s)"),
        }
    }

    /// The body of POST /api/sik/save, or none when nothing was changed.
    pub fn save_body(&self, port: &str, baud: i64) -> Option<Value> {
        let mut b = target(port, baud);
        for side in [Side::Remote, Side::Local] {
            let list = &self.sides[side.index()];
            if !list.is_empty() {
                let m: Map<String, Value> = list.iter().cloned().collect();
                b.insert(side.id().into(), Value::Object(m));
            }
        }
        if b.contains_key("local") || b.contains_key("remote") {
            Some(Value::Object(b))
        } else {
            None
        }
    }
}

/// What the save answered, as a status line: the warnings, else a note that
/// the radios are read again.
pub fn save_notice(answer: &Value) -> (&'static str, String) {
    let warned: Vec<&str> = answer
        .get("warnings")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let text = warned.join(" ");
    if text.is_empty() {
        ("ok", "Settings written. Re-reading the radios\u{2026}".into())
    } else {
        ("warn", text)
    }
}

/// The detail lines of the reset confirmation.
pub fn reset_detail(side: Side) -> [&'static str; 2] {
    [
        match side {
            Side::Remote => "The remote radio will return to its firmware defaults. If those differ from this radio's settings the link will drop, and the only way to reach it again is a cable to the radio itself.",
            Side::Local => "This radio will return to its firmware defaults, including its baud rate. If the remote radio keeps different settings the link will not come back until they match again.",
        },
        "Both radios are read again afterwards, so you will see the result rather than have to trust it.",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, value: f64) -> Field {
        Field {
            name: name.into(),
            label: name.into(),
            kind: Kind::Number,
            value,
            advanced: false,
            must_match: false,
            read_only: false,
            options: vec![],
            unit: String::new(),
            hint: String::new(),
        }
    }

    #[test]
    fn the_port_list_is_labelled_and_the_links_own_port_is_preferred() {
        let status = json!({"ports": [
            {"device": "/dev/ttyUSB0", "description": "FT232", "kind": "sik", "is_link": false},
            {"device": "/dev/ttyUSB1", "description": "", "kind": "unknown", "is_link": true},
            {"device": "/dev/ttyUSB2", "description": "", "kind": "unknown", "is_link": false}]});
        let p = ports(&status);
        assert_eq!(p[0].label, "/dev/ttyUSB0 (radio, FT232)");
        assert_eq!(p[1].label, "/dev/ttyUSB1 (live link)");
        assert_eq!(p[2].label, "/dev/ttyUSB2");
        assert_eq!(preferred_port(&p, ""), "/dev/ttyUSB1");
        assert_eq!(preferred_port(&p, "/dev/ttyUSB2"), "/dev/ttyUSB2");
        assert_eq!(preferred_port(&p[..1], ""), "/dev/ttyUSB0");
        assert_eq!(preferred_port(&[p[2].clone()], ""), "/dev/ttyUSB2");
        assert_eq!(preferred_port(&[], ""), "");
    }

    #[test]
    fn the_baud_follows_the_links_port() {
        let s = json!({"bauds": [57600, 115200], "link_device": "/dev/a", "link_baud": 115200, "default_baud": 57600});
        assert_eq!(bauds(&s), vec![57600, 115200]);
        assert_eq!(bauds(&json!({})), vec![57600]);
        assert_eq!(preferred_baud(&s, "/dev/a"), 115200);
        assert_eq!(preferred_baud(&s, "/dev/b"), 57600);
        assert_eq!(preferred_baud(&json!({}), ""), 57600);
    }

    #[test]
    fn the_gate() {
        let ok = json!({"can_configure": true});
        assert_eq!(gate_reason(false, &ok), "");
        assert_eq!(gate_reason(true, &ok), ARMED_REASON);
        let shut = json!({"can_configure": false, "blocked_reason": "busy"});
        assert_eq!(gate_reason(false, &shut), "busy");
        assert_eq!(gate_reason(true, &shut), ARMED_REASON);
        assert!(!blocked("", false, "/dev/a"));
        assert!(blocked("", false, ""));
        assert!(blocked("", true, "/dev/a"));
        assert!(blocked("x", false, "/dev/a"));
    }

    #[test]
    fn request_bodies() {
        assert_eq!(
            load_body("/dev/a", 57600, true),
            json!({"device": "/dev/a", "baud": 57600, "remote": true})
        );
        assert_eq!(
            load_body("/dev/a", 0, false),
            json!({"device": "/dev/a", "remote": false})
        );
        assert_eq!(
            reset_body("/dev/a", 57600, Side::Remote),
            json!({"device": "/dev/a", "baud": 57600, "target": "remote"})
        );
    }

    #[test]
    fn parse_int_is_javascripts() {
        assert_eq!(parse_int("25"), Some(25));
        assert_eq!(parse_int("  -7x"), Some(-7));
        assert_eq!(parse_int("12.9"), Some(12));
        assert_eq!(parse_int("+3"), Some(3));
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_int("-"), None);
        assert_eq!(parse_int("abc"), None);
    }

    #[test]
    fn an_edit_is_dropped_when_the_original_is_typed_back() {
        let f = field("NETID", 25.0);
        let mut e = Edits::default();
        e.stage(Side::Local, &f, Some(31));
        assert!(e.is_dirty(Side::Local, "NETID"));
        assert_eq!(e.save_label(), "Save 1 change(s)");
        e.stage(Side::Local, &f, None);
        assert!(e.is_dirty(Side::Local, "NETID"));
        e.stage(Side::Local, &f, Some(25));
        assert!(!e.is_dirty(Side::Local, "NETID"));
        assert_eq!(e.save_label(), "Save");
        assert_eq!(e.save_body("/dev/a", 57600), None);
    }

    #[test]
    fn a_save_sends_only_what_was_staged_per_side() {
        let (n, t) = (field("NETID", 25.0), field("TXPOWER", 20.0));
        let mut e = Edits::default();
        e.stage(Side::Local, &n, Some(31));
        e.stage(Side::Remote, &t, Some(10));
        e.stage(Side::Remote, &n, Some(31));
        assert_eq!(
            e.save_body("/dev/a", 57600).unwrap(),
            json!({"device": "/dev/a", "baud": 57600, "local": {"NETID": 31},
                   "remote": {"NETID": 31, "TXPOWER": 10}})
        );
        let mut only_local = Edits::default();
        only_local.stage(Side::Local, &n, Some(31));
        let b = only_local.save_body("/dev/a", 0).unwrap();
        assert!(b.get("remote").is_none());
        assert!(b.get("baud").is_none());
        e.clear();
        assert_eq!(e.count(), 0);
    }

    #[test]
    fn copying_stages_this_radios_values_in_the_remote_column() {
        let m = vec![Mismatch {
            name: "NETID".into(),
            label: "Network ID".into(),
            local: json!(25),
            remote: json!(30),
        }];
        let mut e = Edits::default();
        e.copy_to_remote(&m);
        assert_eq!(e.get(Side::Remote, "NETID"), Some(&json!(25)));
        assert_eq!(e.get(Side::Local, "NETID"), None);
        assert_eq!(m[0].sentence(), "Network ID: 25 here, 30 on the remote radio");
        assert_eq!(e.shown_value(Side::Remote, &field("NETID", 30.0)), json!(25));
        assert_eq!(e.shown_value(Side::Local, &field("NETID", 30.0)), json!(30.0));
    }

    #[test]
    fn a_load_answer() {
        let d = json!({
            "local": {"version": "SiK 2.0 on HM-TRP", "board": "HM-TRP", "fields": [
                {"name": "NETID", "label": "Network ID", "kind": "number", "value": 25, "must_match": true, "unit": "", "hint": "h"},
                {"name": "TXPOWER", "label": "Transmit power", "kind": "enum", "value": 20, "unit": "dBm", "advanced": true,
                 "options": [{"value": 20, "label": "20 dBm"}]}]},
            "remote": {"version": "SiK 2.1 on HM-TRP", "board": "HM-TRP", "fields": []},
            "remote_reachable": true,
            "mismatches": [{"name": "NETID", "label": "Network ID", "local": 25, "remote": 30}],
            "link": {"local_rssi": 208, "remote_rssi": 205, "local_noise": 41, "remote_noise": 38,
                     "local_dbm": -17.5, "remote_dbm": -19.1, "local_margin_db": 83.5, "remote_margin_db": 83.5}});
        let l = Loaded::from_json(&d);
        assert!(l.firmware_differs());
        let local = l.radio(Side::Local).unwrap();
        assert_eq!(local.fields[0].kind, Kind::Number);
        assert!(local.fields[0].must_match);
        assert_eq!(local.fields[1].caption(), "Transmit power (dBm)");
        assert_eq!(local.fields[1].options, vec![(20, "20 dBm".to_string())]);
        assert!(!local.fields[1].shown(false) && local.fields[1].shown(true));
        let link = l.link.as_ref().unwrap();
        assert_eq!(link.rssi, "208 / 205  (-17.5 / -19.1 dBm)");
        assert_eq!(link.noise, "41 / 38");
        assert_eq!(link.margin, "83.5 / 83.5 dB");
        assert_eq!(
            l.summary(true),
            (
                "warn",
                "Settings loaded. 1 setting(s) differ between the two radios.".into()
            )
        );
    }

    #[test]
    fn the_status_line_after_a_load() {
        let silent = Loaded::from_json(&json!({"local": {"fields": []}, "remote": null, "remote_reachable": false}));
        assert!(silent.remote.is_none() && !silent.firmware_differs());
        assert_eq!(
            silent.summary(true).1,
            "This radio's settings loaded; the remote radio did not answer."
        );
        assert_eq!(silent.summary(false), ("ok", "Settings loaded.".into()));
    }

    #[test]
    fn what_a_save_says() {
        assert_eq!(
            save_notice(&json!({"warnings": []})),
            ("ok", "Settings written. Re-reading the radios\u{2026}".into())
        );
        assert_eq!(
            save_notice(&json!({"warnings": ["a.", "b."]})),
            ("warn", "a. b.".into())
        );
        assert!(reset_detail(Side::Remote)[0].starts_with("The remote radio"));
        assert!(reset_detail(Side::Local)[0].starts_with("This radio"));
    }
}
