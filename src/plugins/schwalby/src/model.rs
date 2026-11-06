//! Schwalby's rules, apart from any window (`python/plugins/schwalby/schwalby.js`:
//! `coerceButton`, `normalizeButtons`, `terminalLine`, `previewLine`,
//! `localSummary`, `normalizeCompanion`, `companionState`, `mcpButton` and the
//! other pure helpers). Held to what the web functions answered, in
//! `model_cases.json`.

use serde_json::{json, Map, Value};

use corvus_api::plugin::ssh::{coerce_mode, Mode};
pub use corvus_api::plugin::ssh::{
    derived_name, new_connection_body, new_connection_error, new_id, NewConn, CTRL_C, MAX_BUTTONS, NEW_CONNECTION,
    RESTART_GRACE_SECS,
};

pub const PLUGIN_ID: &str = "schwalby";
/// What a terminal on this computer calls its host, in the window header.
pub const LOCAL_HOST: &str = "this computer";
/// How often the shelf asks which of its sessions are still up.
pub const LIVE_POLL_SECS: f64 = 4.0;
/// The session the companion's terminal runs in, apart from every button's.
pub const COMPANION_ID: &str = "companion";

/// The colours offered besides the theme's own.
pub const BUTTON_COLORS: [&str; 8] = [
    "#2F6FED", "#0F9D8A", "#2E9E4F", "#D98A00", "#E0572B", "#D62B4A", "#8E4FD6", "#5B6770",
];

pub const MCP_EDIT: &str = "mcp_edit";
pub const MCP_RUN: &str = "mcp_run";

/// Where a button runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Local,
    Ssh,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Local => "local",
            Target::Ssh => "ssh",
        }
    }
}

/// One launch button.
#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub id: String,
    pub label: String,
    pub target: Target,
    pub connection: String,
    pub directory: String,
    pub command: String,
    pub mode: Mode,
    pub restart: bool,
    /// `#RRGGBB` in upper case, or empty for the theme's own.
    pub color: String,
    /// Added by an AI client over MCP.
    pub by_mcp: bool,
    /// Not to be run until the operator allows it.
    pub pending: bool,
}

impl Button {
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("id".into(), json!(self.id));
        o.insert("label".into(), json!(self.label));
        o.insert("target".into(), json!(self.target.as_str()));
        o.insert("connection".into(), json!(self.connection));
        o.insert("directory".into(), json!(self.directory));
        o.insert("command".into(), json!(self.command));
        o.insert("mode".into(), json!(self.mode.as_str()));
        o.insert("restart".into(), json!(self.restart));
        o.insert("color".into(), json!(self.color));
        if self.by_mcp {
            o.insert("by_mcp".into(), json!(true));
            if self.pending {
                o.insert("pending".into(), json!(true));
            }
        }
        Value::Object(o)
    }

    /// The editor's blank button: it starts where the last one runs, in its
    /// folder, in its colour.
    pub fn blank(last: Option<&Button>, first_connection: &str) -> Button {
        Button {
            id: new_id(),
            label: String::new(),
            target: last.map_or(Target::Local, |l| l.target),
            connection: last
                .map(|l| l.connection.clone())
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| first_connection.to_string()),
            directory: last.map_or(String::new(), |l| l.directory.clone()),
            command: String::new(),
            mode: Mode::Terminal,
            restart: false,
            color: last.map_or(String::new(), |l| l.color.clone()),
            by_mcp: false,
            pending: false,
        }
    }
}

/// The session a button runs in, keyed by its id so a rename does not orphan
/// it, and by this plugin so it is never the SSH Launcher's.
pub fn session_name(id: &str) -> String {
    format!("{PLUGIN_ID}/{id}")
}

fn text(raw: &Value, key: &str) -> String {
    match raw.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.trim().to_string(),
        Some(other) => other.to_string().trim().to_string(),
    }
}

/// Where a saved button runs. An entry without a target that names a
/// connection is one copied over from the SSH Launcher, so it runs over SSH.
pub fn coerce_target(raw: &Value) -> Target {
    match raw.get("target").and_then(Value::as_str) {
        Some("local") => return Target::Local,
        Some("ssh") => return Target::Ssh,
        _ => {}
    }
    if text(raw, "connection").is_empty() {
        Target::Local
    } else {
        Target::Ssh
    }
}

/// Whether pressing a button whose program is up stops it first (Ctrl-C).
pub fn restarts(b: &Button) -> bool {
    b.restart && b.mode == Mode::Terminal
}

/// A saved colour as `#RRGGBB` in upper case, or empty for the theme's own.
pub fn coerce_color(raw: &Value) -> String {
    let v = match raw {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        other => other.to_string().trim().to_string(),
    };
    color_text(&v)
}

fn color_text(v: &str) -> String {
    let ok = v.len() == 7 && v.starts_with('#') && v[1..].chars().all(|c| c.is_ascii_hexdigit());
    if ok {
        v.to_uppercase()
    } else {
        String::new()
    }
}

/// The label colour that reads on a button of `hex`: white on a dark fill,
/// near black on a light one. Empty when there is no colour.
pub fn readable_on(hex: &str) -> &'static str {
    let c = color_text(hex.trim());
    if c.is_empty() {
        return "";
    }
    let lin = |i: usize| {
        let v = f64::from(u8::from_str_radix(&c[i..i + 2], 16).unwrap_or(0)) / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * lin(1) + 0.7152 * lin(3) + 0.0722 * lin(5);
    if luminance > 0.4 {
        "#111111"
    } else {
        "#FFFFFF"
    }
}

/// The shelf with one button moved to a new place. An unknown id or a slot
/// outside the list leaves the order as it was.
pub fn move_button(list: &[Button], id: &str, slot: usize) -> Vec<Button> {
    let mut out = list.to_vec();
    let Some(from) = out.iter().position(|b| b.id == id) else {
        return out;
    };
    if slot >= out.len() {
        return out;
    }
    let b = out.remove(from);
    out.insert(slot, b);
    out
}

/// One saved entry as a complete button, or `None` to drop it.
pub fn coerce_button(raw: &Value) -> Option<Button> {
    if !raw.is_object() {
        return None;
    }
    let command = text(raw, "command");
    if command.is_empty() {
        return None;
    }
    let label = text(raw, "label");
    let target = coerce_target(raw);
    let by_mcp = raw.get("by_mcp") == Some(&Value::Bool(true));
    Some(Button {
        id: raw
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map_or_else(new_id, str::to_string),
        label: if label.is_empty() { command.clone() } else { label },
        target,
        connection: if target == Target::Local {
            String::new()
        } else {
            text(raw, "connection")
        },
        directory: text(raw, "directory"),
        command,
        mode: coerce_mode(raw),
        restart: raw.get("restart") == Some(&Value::Bool(true)),
        color: coerce_color(raw.get("color").unwrap_or(&Value::Null)),
        by_mcp,
        pending: by_mcp && raw.get("pending") == Some(&Value::Bool(true)),
    })
}

/// The button list from the plugin's saved settings, at most `MAX_BUTTONS`.
pub fn normalize_buttons(saved: &Value) -> Vec<Button> {
    let mut out = Vec::new();
    if let Some(list) = saved.get("buttons").and_then(Value::as_array) {
        for raw in list {
            if let Some(b) = coerce_button(raw) {
                if out.len() < MAX_BUTTONS {
                    out.push(b);
                }
            }
        }
    }
    out
}

/// The connections SSH buttons name that this computer has not saved, in
/// shelf order, each once.
pub fn missing_connections(buttons: &[Button], connections: &[Value]) -> Vec<String> {
    let known: Vec<&str> = connections
        .iter()
        .filter_map(|c| c.get("name").and_then(Value::as_str))
        .collect();
    let mut out: Vec<String> = Vec::new();
    for b in buttons {
        if b.target != Target::Ssh {
            continue;
        }
        let name = b.connection.trim();
        if !name.is_empty() && !known.contains(&name) && !out.iter().any(|o| o == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// A folder as a POSIX shell should read it: quoted, except for a leading
/// `~`, which only means "home" outside the quotes.
pub fn shell_folder(directory: &str) -> String {
    let quote = |t: &str| format!("'{}'", t.replace('\'', "'\\''"));
    if directory == "~" {
        return "~".into();
    }
    if let Some(rest) = directory.strip_prefix("~/") {
        return if rest.is_empty() {
            "~/".into()
        } else {
            format!("~/{}", quote(rest))
        };
    }
    quote(directory)
}

/// The line a button types into its terminal: `cd -- <folder> && <command>`.
pub fn terminal_line(b: &Button) -> String {
    let command = b.command.trim();
    if command.is_empty() {
        return String::new();
    }
    let directory = b.directory.trim();
    if directory.is_empty() {
        return command.to_string();
    }
    format!("cd -- {} && {command}", shell_folder(directory))
}

/// The human-readable preview of what a button will run. Shown, not sent.
pub fn preview_line(target: Target, connection: &str, directory: &str, command: &str, mode: Mode) -> String {
    let command = command.trim();
    if command.is_empty() {
        return String::new();
    }
    let directory = directory.trim();
    let local = target == Target::Local;
    let body = if mode == Mode::Background {
        if local {
            format!("{command} &")
        } else {
            format!("nohup {command} &")
        }
    } else {
        command.to_string()
    };
    let line = if directory.is_empty() {
        body
    } else {
        format!("cd {directory} && {body}")
    };
    if local {
        return line;
    }
    let target = connection.trim();
    if target.is_empty() {
        line
    } else {
        format!("ssh {target} '{line}'")
    }
}

/// What a button will run, for its title and the editor.
pub fn preview_of(b: &Button) -> String {
    preview_line(b.target, &b.connection, &b.directory, &b.command, b.mode)
}

fn first_line(v: &str) -> String {
    v.trim().split('\n').next().unwrap_or("").trim().to_string()
}

fn string_of(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// One line for a background run over SSH: the text and `"ok"` or `"err"`.
pub fn ssh_summary(res: &Value) -> (String, &'static str) {
    if res.get("ok") != Some(&Value::Bool(true)) {
        let stderr = string_of(res.get("stderr"));
        let said = if stderr.is_empty() {
            string_of(res.get("error"))
        } else {
            stderr
        };
        let detail = first_line(&said);
        return (
            if detail.is_empty() {
                "The command failed.".into()
            } else {
                detail
            },
            "err",
        );
    }
    let pid = string_of(res.get("stdout")).trim().to_string();
    (
        if pid.is_empty() {
            "Running in the background".into()
        } else {
            format!("Running in the background (pid {pid})")
        },
        "ok",
    )
}

/// One line for a background run on this computer. The backend waits a second
/// before it answers, so a program that cannot start answers with its exit
/// code and its own words instead of a pid that is already gone.
pub fn local_summary(res: &Value) -> (String, &'static str) {
    let truthy = |k: &str| match res.get(k) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    };
    if truthy("exited") {
        if truthy("ok") {
            return ("Ran and finished".into(), "ok");
        }
        let said = first_line(&string_of(res.get("output")));
        let code = match res.get("code") {
            Some(Value::Null) | None => "undefined".to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        };
        return (
            if said.is_empty() {
                format!("Ended at once with exit code {code}")
            } else {
                said
            },
            "err",
        );
    }
    if !truthy("ok") {
        let said = first_line(&string_of(res.get("error")));
        return (
            if said.is_empty() {
                "The command failed.".into()
            } else {
                said
            },
            "err",
        );
    }
    let pid = match res.get("pid") {
        Some(Value::Number(n)) if n.as_f64() != Some(0.0) => Some(n.to_string()),
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };
    (
        match pid {
            Some(p) => format!("Running in the background (pid {p}), stops with Corvus"),
            None => "Running in the background, stops with Corvus".into(),
        },
        "ok",
    )
}

// ---- the companion computer -------------------------------------------------

pub const INTERVAL_STEPS: [u32; 5] = [1, 2, 3, 5, 10];
pub const TIMEOUT_STEPS: [u32; 7] = [3, 4, 6, 10, 15, 20, 30];
pub const DEFAULT_INTERVAL_S: u32 = 2;
pub const DEFAULT_TIMEOUT_S: u32 = 6;
pub const MAX_COMPANION_NAME: usize = 40;

/// The companion computer as the indicator uses it.
#[derive(Debug, Clone, PartialEq)]
pub struct Companion {
    pub name: String,
    pub host: String,
    pub interval_s: u32,
    pub timeout_s: u32,
    pub connection: String,
}

impl Companion {
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("name".into(), json!(self.name));
        o.insert("host".into(), json!(self.host));
        o.insert("interval_s".into(), json!(self.interval_s));
        o.insert("timeout_s".into(), json!(self.timeout_s));
        if !self.connection.is_empty() {
            o.insert("connection".into(), json!(self.connection));
        }
        Value::Object(o)
    }

    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            "Companion computer"
        } else {
            &self.name
        }
    }
}

/// `nearestStep`: the step closest to `value`, the first on a tie, or the
/// fallback when `value` is not a number.
pub fn nearest_step(steps: &[u32], value: Option<&Value>, fallback: u32) -> u32 {
    let n = match value {
        None => None,
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                Some(0.0)
            } else {
                t.parse::<f64>().ok()
            }
        }
        Some(Value::Bool(b)) => Some(f64::from(u8::from(*b))),
        Some(Value::Null) => Some(0.0),
        _ => None,
    };
    let Some(n) = n.filter(|n| n.is_finite()) else {
        return fallback;
    };
    let mut best = steps[0];
    for s in steps {
        if (f64::from(*s) - n).abs() < (f64::from(best) - n).abs() {
            best = *s;
        }
    }
    best
}

/// The shortest timeout that is still longer than `interval`.
pub fn timeout_for(interval: u32) -> u32 {
    TIMEOUT_STEPS
        .iter()
        .copied()
        .find(|t| *t > interval)
        .unwrap_or(TIMEOUT_STEPS[TIMEOUT_STEPS.len() - 1])
}

/// The longest interval that is still shorter than `timeout`.
pub fn interval_for(timeout: u32) -> u32 {
    INTERVAL_STEPS
        .iter()
        .copied()
        .rfind(|i| *i < timeout)
        .unwrap_or(INTERVAL_STEPS[0])
}

/// Why a companion address cannot be pinged, or empty when it can. The rule
/// the backend applies (`corvus/net_probe.py`).
pub fn host_error(host: &str) -> &'static str {
    let h = host.trim();
    if h.is_empty() {
        return "Enter an IP address or host name.";
    }
    let chars: Vec<char> = h.chars().collect();
    let edge = |c: char, first: bool| c.is_ascii_alphanumeric() || (first && c == ':');
    let inner = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '%' | '-');
    let valid = edge(chars[0], true)
        && chars.iter().all(|c| inner(*c))
        && (chars.len() == 1 || chars[chars.len() - 1].is_ascii_alphanumeric());
    if h.chars().count() > 253 || !valid {
        return "That is not an IP address or host name.";
    }
    ""
}

/// The saved companion as the indicator uses it, or `None` when none is set.
pub fn normalize_companion(raw: &Value) -> Option<Companion> {
    if !raw.is_object() {
        return None;
    }
    let host = text(raw, "host");
    if !host_error(&host).is_empty() {
        return None;
    }
    let interval = nearest_step(&INTERVAL_STEPS, raw.get("interval_s"), DEFAULT_INTERVAL_S);
    let mut timeout = nearest_step(&TIMEOUT_STEPS, raw.get("timeout_s"), DEFAULT_TIMEOUT_S);
    if timeout <= interval {
        timeout = timeout_for(interval);
    }
    Some(Companion {
        name: text(raw, "name").chars().take(MAX_COMPANION_NAME).collect(),
        host,
        interval_s: interval,
        timeout_s: timeout,
        connection: text(raw, "connection").chars().take(120).collect(),
    })
}

/// The companion as a shelf entry, so its terminal goes through the same
/// connect and window code a button's does. `None` without a connection.
pub fn companion_entry(c: &Companion) -> Option<Button> {
    if c.connection.is_empty() {
        return None;
    }
    Some(Button {
        id: COMPANION_ID.into(),
        label: c.display_name().to_string(),
        target: Target::Ssh,
        connection: c.connection.clone(),
        directory: String::new(),
        command: String::new(),
        mode: Mode::Terminal,
        restart: false,
        color: String::new(),
        by_mcp: false,
        pending: false,
    })
}

/// How long one probe may wait for its echo: the interval, within 1 to 5 s.
pub fn probe_wait_ms(interval_s: u32) -> u32 {
    if interval_s == 0 {
        return 2000;
    }
    (interval_s.saturating_mul(1000)).clamp(1000, 5000)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Unset,
    Checking,
    Online,
    Offline,
    Error,
}

impl State {
    #[cfg(test)]
    pub fn as_str(self) -> &'static str {
        match self {
            State::Unset => "unset",
            State::Checking => "checking",
            State::Online => "online",
            State::Offline => "offline",
            State::Error => "error",
        }
    }

    /// The word beside the dot.
    pub fn text(self) -> &'static str {
        match self {
            State::Unset => "Not set",
            State::Checking => "Checking",
            State::Online => "Online",
            State::Offline => "Offline",
            State::Error => "Cannot ping",
        }
    }
}

/// What the indicator shows. Online while the last reply is younger than the
/// timeout; before the first reply, checking until the timeout has passed;
/// offline after that. A probe that could not be made at all is an error, not
/// a verdict on the companion, unless a recent reply already answers.
pub fn companion_state(
    host: &str,
    started_at: f64,
    last_ok_at: Option<f64>,
    error: bool,
    now: f64,
    timeout: f64,
) -> State {
    if host.is_empty() {
        return State::Unset;
    }
    if let Some(ok) = last_ok_at {
        if now - ok <= timeout {
            return State::Online;
        }
    }
    if error {
        return State::Error;
    }
    if last_ok_at.is_none() && now - started_at < timeout {
        return State::Checking;
    }
    State::Offline
}

/// `formatRtt`: under 10 ms with a decimal, else whole.
pub fn format_rtt(ms: f64) -> String {
    if !ms.is_finite() {
        return String::new();
    }
    if ms < 10.0 {
        format!("{} ms", corvus_api::data::units::js_to_fixed(ms, 1))
    } else {
        format!("{} ms", corvus_api::data::units::js_to_fixed(ms.round(), 0))
    }
}

// ---- the AI client's half ----------------------------------------------------

/// What the AI client is told about one button.
pub fn describe_button(b: &Button) -> Value {
    json!({
        "id": b.id, "label": b.label,
        "runs_on": if b.target == Target::Local { "this computer".to_string() } else { format!("ssh {}", b.connection) },
        "directory": b.directory, "command": b.command, "mode": b.mode.as_str(), "restart": b.restart,
        "added_by_ai": b.by_mcp, "waiting_for_operator": b.pending,
    })
}

fn text_arg(args: &Value, key: &str, required: bool) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => {
            if required {
                Err(format!("{key} is required"))
            } else {
                Ok(None)
            }
        }
        Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

/// A button from the AI client's arguments, merged over `base`. Always on
/// this computer: the AI client never gets a button that runs elsewhere.
pub fn mcp_button(args: &Value, base: Option<&Button>) -> Result<Button, String> {
    let mut next = base.cloned().unwrap_or_else(|| Button {
        id: new_id(),
        label: String::new(),
        target: Target::Local,
        connection: String::new(),
        directory: String::new(),
        command: String::new(),
        mode: Mode::Terminal,
        restart: false,
        color: String::new(),
        by_mcp: true,
        pending: false,
    });
    for k in ["label", "command", "directory"] {
        if let Some(v) = text_arg(args, k, base.is_none() && (k == "label" || k == "command"))? {
            match k {
                "label" => next.label = v,
                "command" => next.command = v,
                _ => next.directory = v,
            }
        }
    }
    match args.get("mode") {
        None => {}
        Some(Value::String(m)) if m == "terminal" => next.mode = Mode::Terminal,
        Some(Value::String(m)) if m == "background" => next.mode = Mode::Background,
        Some(_) => return Err("mode must be \"terminal\" or \"background\"".into()),
    }
    match args.get("restart") {
        None => {}
        Some(Value::Bool(b)) => next.restart = *b,
        Some(_) => return Err("restart must be true or false".into()),
    }
    if let Some(c) = args.get("color") {
        next.color = coerce_color(c);
    }
    next.target = Target::Local;
    next.connection = String::new();
    next.by_mcp = true;
    let runs = |b: &Button| (b.command.clone(), b.directory.clone(), b.mode, b.restart);
    if base.is_none_or(|b| runs(b) != runs(&next)) {
        next.pending = true;
    }
    // the same trimming and defaults a saved entry gets
    coerce_button(&next.to_json()).ok_or_else(|| "command must not be empty".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cases() -> Value {
        serde_json::from_str(include_str!("model_cases.json")).expect("model_cases.json")
    }

    fn each(name: &str) -> Vec<Value> {
        cases()[name]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("no cases {name}"))
    }

    #[test]
    fn buttons_are_coerced_as_the_web_does() {
        for c in each("coerceButton") {
            let got = coerce_button(&c["in"]).map(|b| b.to_json());
            let want = if c["out"].is_null() {
                None
            } else {
                Some(c["out"].clone())
            };
            assert_eq!(got, want, "{}", c["in"]);
        }
    }

    #[test]
    fn a_target_follows_the_connection_when_none_is_saved() {
        for c in each("coerceTarget") {
            assert_eq!(coerce_target(&c["in"]).as_str(), c["out"], "{}", c["in"]);
        }
    }

    #[test]
    fn colours_and_their_label_colour() {
        for c in each("coerceColor") {
            assert_eq!(coerce_color(&c["in"]), c["out"].as_str().unwrap(), "{}", c["in"]);
        }
        for c in each("readableOn") {
            assert_eq!(
                readable_on(c["in"].as_str().unwrap()),
                c["out"].as_str().unwrap(),
                "{}",
                c["in"]
            );
        }
        for h in BUTTON_COLORS {
            assert_eq!(coerce_color(&json!(h)), h);
        }
    }

    #[test]
    fn a_button_moves_to_a_slot_and_nothing_else_does() {
        let list: Vec<Button> = ["a", "b", "c", "d"]
            .iter()
            .map(|id| coerce_button(&json!({"id": id, "command": "x"})).unwrap())
            .collect();
        for c in each("moveButton") {
            let ids: Vec<String> = move_button(&list, c["id"].as_str().unwrap(), c["slot"].as_u64().unwrap() as usize)
                .into_iter()
                .map(|b| b.id)
                .collect();
            assert_eq!(json!(ids), c["out"], "{}", c);
        }
    }

    #[test]
    fn the_terminal_line_and_the_preview() {
        for c in each("terminalLine") {
            let b = coerce_button(&c["in"]).unwrap();
            assert_eq!(terminal_line(&b), c["out"].as_str().unwrap(), "{}", c["in"]);
        }
        for c in each("previewLine") {
            let b = coerce_button(&c["in"]).unwrap();
            assert_eq!(preview_of(&b), c["out"].as_str().unwrap(), "{}", c["in"]);
        }
    }

    #[test]
    fn a_background_run_is_summed_up_in_one_line() {
        for c in each("sshSummary") {
            let (t, k) = ssh_summary(&c["in"]);
            assert_eq!(json!({"text": t, "kind": k}), c["out"], "{}", c["in"]);
        }
        for c in each("localSummary") {
            let (t, k) = local_summary(&c["in"]);
            assert_eq!(json!({"text": t, "kind": k}), c["out"], "{}", c["in"]);
        }
    }

    #[test]
    fn connections_a_shelf_still_needs() {
        for c in each("missingConnections") {
            let buttons = normalize_buttons(&json!({"buttons": c["buttons"]}));
            let conns = c["connections"].as_array().cloned().unwrap();
            assert_eq!(
                json!(missing_connections(&buttons, &conns)),
                c["out"],
                "{}",
                c["buttons"]
            );
        }
    }

    #[test]
    fn a_host_is_checked_as_the_backend_checks_it() {
        for c in each("hostError") {
            assert_eq!(
                host_error(c["in"].as_str().unwrap()),
                c["out"].as_str().unwrap(),
                "{}",
                c["in"]
            );
        }
    }

    #[test]
    fn the_companion_lands_on_slider_stops_and_keeps_its_timeout_longer() {
        for c in each("normalizeCompanion") {
            let got = normalize_companion(&c["in"]).map(|c| c.to_json());
            let want = if c["out"].is_null() {
                None
            } else {
                Some(c["out"].clone())
            };
            assert_eq!(got, want, "{}", c["in"]);
        }
        for c in each("companionEntry") {
            let comp = normalize_companion(&c["in"]).unwrap();
            let e = companion_entry(&comp);
            assert_eq!(
                e.map(|b| json!([b.id, b.label, b.target.as_str(), b.connection])),
                if c["out"].is_null() {
                    None
                } else {
                    Some(c["out"].clone())
                },
                "{}",
                c["in"]
            );
        }
        for c in each("probeWaitMs") {
            assert_eq!(
                probe_wait_ms(c["in"].as_u64().unwrap() as u32),
                c["out"].as_u64().unwrap() as u32
            );
        }
        for t in TIMEOUT_STEPS {
            assert!(timeout_for(interval_for(t)) <= t.max(timeout_for(1)));
        }
        for c in each("timeoutFor") {
            assert_eq!(
                timeout_for(c["in"].as_u64().unwrap() as u32),
                c["out"].as_u64().unwrap() as u32
            );
        }
        for c in each("intervalFor") {
            assert_eq!(
                interval_for(c["in"].as_u64().unwrap() as u32),
                c["out"].as_u64().unwrap() as u32
            );
        }
    }

    #[test]
    fn the_indicator_follows_the_replies() {
        for c in each("companionState") {
            let i = &c["in"];
            let st = companion_state(
                i["host"].as_str().unwrap(),
                i["startedAt"].as_f64().unwrap(),
                i["lastOkAt"].as_f64(),
                !i["error"].as_str().unwrap_or("").is_empty(),
                i["now"].as_f64().unwrap(),
                i["timeoutMs"].as_f64().unwrap(),
            );
            assert_eq!(st.as_str(), c["out"].as_str().unwrap(), "{i}");
        }
        assert_eq!(format_rtt(0.34), "0.3 ms");
        assert_eq!(format_rtt(9.96), "10.0 ms");
        assert_eq!(format_rtt(10.4), "10 ms");
    }

    #[test]
    fn what_an_ai_client_may_add_is_always_local_and_waits_to_be_allowed() {
        for c in each("mcpButton") {
            let base = if c["base"].is_null() {
                None
            } else {
                Some(coerce_button(&c["base"]).unwrap())
            };
            let got = mcp_button(&c["args"], base.as_ref());
            match (&c["error"], got) {
                (Value::Null, Ok(b)) => {
                    let mut j = b.to_json();
                    j.as_object_mut().unwrap().remove("id");
                    assert_eq!(j, c["out"], "{}", c["args"]);
                }
                (e, Err(msg)) => assert_eq!(e.as_str().unwrap(), msg, "{}", c["args"]),
                (e, Ok(b)) => panic!("expected the error {e}, got {b:?}"),
            }
        }
        for c in each("describeButton") {
            let b = coerce_button(&c["in"]).unwrap();
            assert_eq!(describe_button(&b), c["out"], "{}", c["in"]);
        }
    }
}
