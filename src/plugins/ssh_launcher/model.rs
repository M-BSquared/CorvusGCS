//! The SSH Launcher's rules, apart from any window
//! (`python/plugins/ssh-launcher/ssh-launcher.js`: `coerceButton`,
//! `normalizeButtons`, `remoteLine`, `previewLine`, `newConnectionError` and
//! the other pure helpers). Held to what the web functions answered.

use serde_json::{json, Value};

/// Bounded so the shelf stays a shelf.
pub const MAX_BUTTONS: usize = 12;
/// How often the shelf asks which sessions are still up.
pub const LIVE_POLL_SECS: f64 = 4.0;
pub const CTRL_C: &str = "\x03";
/// How long the shell gets to take the prompt back before the line is typed again.
pub const RESTART_GRACE_SECS: f64 = 1.0;
/// The connection select's last entry: a way to type one in.
pub const NEW_CONNECTION: &str = "__new__";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Terminal,
    Background,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Terminal => "terminal",
            Mode::Background => "background",
        }
    }
}

/// One launch button.
#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub id: String,
    pub label: String,
    pub connection: String,
    pub directory: String,
    pub command: String,
    pub mode: Mode,
    pub restart: bool,
}

impl Button {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "label": self.label, "connection": self.connection,
            "directory": self.directory, "command": self.command,
            "mode": self.mode.as_str(), "restart": self.restart,
        })
    }

    /// A blank button for the editor.
    pub fn blank(connection: &str, directory: &str) -> Button {
        Button {
            id: new_id(),
            label: String::new(),
            connection: connection.into(),
            directory: directory.into(),
            command: String::new(),
            mode: Mode::Terminal,
            restart: false,
        }
    }
}

/// A short, collision-free id for a new button. Never shown.
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("b{ms:x}{:x}", N.fetch_add(1, Ordering::Relaxed))
}

/// The SSH session a button runs in, keyed by its id so a rename does not
/// orphan the session its program is running in.
pub fn session_name(id: &str) -> String {
    format!("ssh-launcher/{id}")
}

fn text(raw: &Value, key: &str) -> String {
    match raw.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.trim().to_string(),
        Some(other) => other.to_string().trim().to_string(),
    }
}

/// A button's run mode, including the migration from the `detach` boolean
/// the plugin used before it grew a terminal.
pub fn coerce_mode(raw: &Value) -> Mode {
    match raw.get("mode").and_then(Value::as_str) {
        Some("background") => return Mode::Background,
        Some("terminal") => return Mode::Terminal,
        _ => {}
    }
    match raw.get("detach").and_then(Value::as_bool) {
        Some(true) => Mode::Background,
        _ => Mode::Terminal,
    }
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
    Some(Button {
        id: raw
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map_or_else(new_id, str::to_string),
        label: if label.is_empty() { command.clone() } else { label },
        connection: text(raw, "connection"),
        directory: text(raw, "directory"),
        command,
        mode: coerce_mode(raw),
        restart: raw.get("restart") == Some(&Value::Bool(true)),
    })
}

/// The button list from the plugin's saved settings, migrating the
/// single-command shape the plugin started with into a one-entry shelf.
pub fn normalize_buttons(saved: &Value) -> Vec<Button> {
    let one = [saved.clone()];
    let source: &[Value] = saved
        .get("buttons")
        .and_then(Value::as_array)
        .map_or(&one, Vec::as_slice);
    let mut out = Vec::new();
    for raw in source {
        if let Some(b) = coerce_button(raw) {
            if out.len() < MAX_BUTTONS {
                out.push(b);
            }
        }
    }
    out
}

/// Whether pressing a button whose program is up stops it first (Ctrl-C).
/// Only a terminal has a program to stop.
pub fn restarts(b: &Button) -> bool {
    b.restart && b.mode == Mode::Terminal
}

/// The shell line a button types into its terminal: the folder quoted for
/// the remote shell, the command as typed.
pub fn remote_line(directory: &str, command: &str) -> String {
    let command = command.trim();
    if command.is_empty() {
        return String::new();
    }
    let directory = directory.trim();
    if directory.is_empty() {
        return command.to_string();
    }
    format!("cd -- '{}' && {command}", directory.replace('\'', "'\\''"))
}

/// What a button will run, as the editor shows it (shown, not sent).
pub fn preview_line(connection: &str, directory: &str, command: &str, mode: Mode) -> String {
    let command = command.trim();
    if command.is_empty() {
        return String::new();
    }
    let body = if mode == Mode::Background {
        format!("nohup {command} &")
    } else {
        command.to_string()
    };
    let directory = directory.trim();
    let remote = if directory.is_empty() {
        body
    } else {
        format!("cd {directory} && {body}")
    };
    let target = connection.trim();
    if target.is_empty() {
        remote
    } else {
        format!("ssh {target} '{remote}'")
    }
}

/// One line for a finished background run: the text and `"ok"` or `"err"`.
pub fn result_summary(res: &Value) -> (String, &'static str) {
    if res.get("ok").and_then(Value::as_bool) != Some(true) {
        let said = |k: &str| {
            res.get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let detail = said("stderr").or_else(|| said("error")).unwrap_or_default();
        let first = detail.trim().split('\n').next().unwrap_or("").to_string();
        return (
            if first.is_empty() {
                "The command failed.".into()
            } else {
                first
            },
            "err",
        );
    }
    let pid = res.get("stdout").and_then(Value::as_str).unwrap_or("").trim();
    (
        if pid.is_empty() {
            "Started in the background.".into()
        } else {
            format!("Started in the background (pid {pid}).")
        },
        "ok",
    )
}

/// The connections the buttons name that this computer has not saved, in
/// shelf order, each once.
pub fn missing_connections(buttons: &[Button], connections: &[Value]) -> Vec<String> {
    let known: Vec<&str> = connections
        .iter()
        .filter_map(|c| c.get("name").and_then(Value::as_str))
        .collect();
    let mut out: Vec<String> = Vec::new();
    for b in buttons {
        let name = b.connection.trim();
        if !name.is_empty() && !known.contains(&name) && !out.iter().any(|o| o == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// The ad-hoc connection form.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewConn {
    pub name: String,
    pub host: String,
    pub port: String,
    pub username: String,
    pub password: String,
    pub key_path: String,
}

impl NewConn {
    pub fn blank() -> NewConn {
        NewConn {
            port: "22".into(),
            ..Default::default()
        }
    }
}

/// `user@host`, the name an ad-hoc connection is saved under when none is typed.
pub fn derived_name(c: &NewConn) -> String {
    let host = c.host.trim();
    if host.is_empty() {
        return String::new();
    }
    let user = c.username.trim();
    if user.is_empty() {
        host.to_string()
    } else {
        format!("{user}@{host}")
    }
}

/// Why an ad-hoc connection cannot be saved yet, or `""`. A name already
/// taken is refused: saving under it would replace that connection's login.
pub fn new_connection_error(c: &NewConn, taken: &[String]) -> String {
    if c.host.trim().is_empty() {
        return "Name the host to connect to.".into();
    }
    let name = if c.name.trim().is_empty() {
        derived_name(c)
    } else {
        c.name.trim().to_string()
    };
    let port = c.port.trim();
    if !port.is_empty() {
        match port.parse::<f64>() {
            Ok(n) if n.is_finite() && n.floor() == n && (1.0..=65535.0).contains(&n) => {}
            _ => return "The port has to be a number between 1 and 65535.".into(),
        }
    }
    if taken.iter().any(|t| *t == name) {
        return format!("\"{name}\" is already saved. Pick it above, or save this one as another name.");
    }
    String::new()
}

/// The body of `POST /api/ssh/connections` for an ad-hoc connection.
pub fn new_connection_body(c: &NewConn) -> Value {
    let name = if c.name.trim().is_empty() {
        derived_name(c)
    } else {
        c.name.trim().to_string()
    };
    json!({
        "name": name,
        "host": c.host.trim(),
        "port": parse_int(c.port.trim()).filter(|p| *p != 0).unwrap_or(22),
        "username": c.username.trim(),
        "password": c.password,
        "key_path": c.key_path.trim(),
    })
}

/// `parseInt(text, 10)`: the leading digits, with a sign.
pub fn parse_int(text: &str) -> Option<i64> {
    let t = text.trim_start();
    let (sign, rest) = match t.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, t.strip_prefix('+').unwrap_or(t)),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<i64>().ok().map(|n| sign * n)
}

/// The select's choices: the saved connections (with their address), then a
/// name this computer lacks, then the way to type one.
pub fn connection_options(connections: &[Value], current: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = connections
        .iter()
        .filter_map(|c| {
            let name = c.get("name").and_then(Value::as_str)?;
            let host = c.get("host").and_then(Value::as_str).unwrap_or("");
            let user = c.get("username").and_then(Value::as_str).unwrap_or("");
            let address = if user.is_empty() {
                host.to_string()
            } else {
                format!("{user}@{host}")
            };
            let label = if !address.is_empty() && address != name {
                format!("{name} ({address})")
            } else {
                name.to_string()
            };
            Some((name.to_string(), label))
        })
        .collect();
    let name = current.trim();
    if !name.is_empty()
        && !connections
            .iter()
            .any(|c| c.get("name").and_then(Value::as_str) == Some(name))
    {
        out.insert(0, (name.to_string(), format!("{name} (not set up on this computer)")));
    }
    out.push((NEW_CONNECTION.into(), "New connection\u{2026}".into()));
    out
}

/// What a failed connect or run reply asks for (`sshNeeds`): the connection's
/// name, `"connection"` or `"credentials"`, and the host's own words.
#[derive(Debug, Clone, PartialEq)]
pub struct SshNeed {
    pub name: String,
    pub needs: String,
    pub error: String,
}

pub fn ssh_needs(reply: &Value) -> Option<SshNeed> {
    let body = if reply.get("needs").is_some() {
        reply
    } else {
        reply.get("body")?
    };
    let needs = match body.get("needs").and_then(Value::as_str) {
        Some(n @ ("connection" | "credentials")) => n,
        _ => return None,
    };
    let name = body.get("connection").and_then(Value::as_str)?.trim();
    if name.is_empty() {
        return None;
    }
    Some(SshNeed {
        name: name.into(),
        needs: needs.into(),
        error: body.get("error").and_then(Value::as_str).unwrap_or("").into(),
    })
}

/// A reply as the plugin reads it whatever went wrong on the way: the body
/// when there is one, `{ok: false, error}` for a network error, and an HTTP
/// error is `ok: false` unless the body says otherwise (`replyOf`).
pub fn reply_of(r: Result<(u16, Value), String>) -> Value {
    match r {
        Ok((status, mut body)) => {
            if status >= 400 {
                if let Some(o) = body.as_object_mut() {
                    o.entry("ok").or_insert(json!(false));
                }
            }
            body
        }
        Err(e) => {
            json!({"ok": false, "error": if e.is_empty() { "Could not reach the backend".to_string() } else { e }})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(host: &str, user: &str) -> NewConn {
        NewConn {
            host: host.into(),
            username: user.into(),
            ..NewConn::blank()
        }
    }

    #[test]
    fn the_remote_line_quotes_the_folder_for_the_far_shell() {
        assert_eq!(remote_line("/home/p'x", " ./a.sh "), "cd -- '/home/p'\\''x' && ./a.sh");
        assert_eq!(remote_line("", "x"), "x");
        assert_eq!(remote_line("/d", "  "), "");
    }

    #[test]
    fn the_preview_shows_what_the_button_will_run() {
        assert_eq!(
            preview_line("pi", "/m", "./s.sh", Mode::Background),
            "ssh pi 'cd /m && nohup ./s.sh &'"
        );
        assert_eq!(preview_line("", "", "./s.sh", Mode::Terminal), "./s.sh");
        assert_eq!(
            preview_line("pi", "/m", "./s.sh", Mode::Terminal),
            "ssh pi 'cd /m && ./s.sh'"
        );
        assert_eq!(preview_line("pi", "/m", " ", Mode::Terminal), "");
    }

    #[test]
    fn a_connection_is_named_user_at_host_when_no_name_is_typed() {
        assert_eq!(derived_name(&conn(" 10.0.0.7 ", "pilot")), "pilot@10.0.0.7");
        assert_eq!(derived_name(&conn("h", "")), "h");
        assert_eq!(derived_name(&conn("", "u")), "");
    }

    #[test]
    fn a_new_connection_is_refused_for_the_reasons_the_web_gives() {
        let none: Vec<String> = Vec::new();
        assert_eq!(
            new_connection_error(&conn("", ""), &none),
            "Name the host to connect to."
        );
        let mut c = conn("h", "");
        c.port = "70000".into();
        assert_eq!(
            new_connection_error(&c, &none),
            "The port has to be a number between 1 and 65535."
        );
        c.port = "2.5".into();
        assert_eq!(
            new_connection_error(&c, &none),
            "The port has to be a number between 1 and 65535."
        );
        c.port = "22".into();
        assert_eq!(
            new_connection_error(&conn("h", "u"), &["u@h".to_string()]),
            "\"u@h\" is already saved. Pick it above, or save this one as another name."
        );
        let mut named = conn("h", "");
        named.name = "mine".into();
        assert_eq!(new_connection_error(&named, &["u@h".to_string()]), "");
        assert_eq!(new_connection_error(&c, &none), "");
    }

    #[test]
    fn the_request_to_save_a_connection_keeps_the_password_as_typed() {
        let c = NewConn {
            host: " h ".into(),
            username: " u ".into(),
            port: String::new(),
            password: " p ".into(),
            key_path: " k ".into(),
            name: String::new(),
        };
        assert_eq!(
            new_connection_body(&c),
            json!({"name": "u@h", "host": "h", "port": 22, "username": "u", "password": " p ", "key_path": "k"})
        );
        let c = NewConn {
            host: "h".into(),
            name: "x".into(),
            port: "2222".into(),
            ..Default::default()
        };
        assert_eq!(
            new_connection_body(&c),
            json!({"name": "x", "host": "h", "port": 2222, "username": "", "password": "", "key_path": ""})
        );
    }

    #[test]
    fn the_old_single_command_becomes_a_one_button_shelf() {
        let b = normalize_buttons(&json!({"connection": "pi", "directory": "/d", "command": "run", "detach": true}));
        assert_eq!(b.len(), 1);
        assert_eq!(
            (b[0].label.as_str(), b[0].mode, b[0].restart),
            ("run", Mode::Background, false)
        );
        assert_eq!((b[0].connection.as_str(), b[0].directory.as_str()), ("pi", "/d"));
    }

    #[test]
    fn a_shelf_drops_what_has_nothing_to_run_and_migrates_detach() {
        let b = normalize_buttons(&json!({"buttons": [
            {"id": "a", "label": "", "command": "c", "mode": "background"},
            {"command": ""}, null,
            {"id": "b", "command": "d", "restart": true, "detach": false},
        ]}));
        let v: Vec<_> = b
            .iter()
            .map(|b| (b.id.as_str(), b.label.as_str(), b.mode, b.restart))
            .collect();
        assert_eq!(
            v,
            [("a", "c", Mode::Background, false), ("b", "d", Mode::Terminal, true)]
        );
    }

    #[test]
    fn a_shelf_holds_twelve_buttons() {
        let many: Vec<Value> = (0..20)
            .map(|i| json!({"id": format!("b{i}"), "command": "x"}))
            .collect();
        assert_eq!(normalize_buttons(&json!({"buttons": many})).len(), MAX_BUTTONS);
    }

    #[test]
    fn a_background_run_is_summed_up_in_one_line() {
        assert_eq!(
            result_summary(&json!({"ok": true, "stdout": " 123\n"})),
            ("Started in the background (pid 123).".into(), "ok")
        );
        assert_eq!(
            result_summary(&json!({"ok": true})),
            ("Started in the background.".into(), "ok")
        );
        assert_eq!(
            result_summary(&json!({"ok": false, "stderr": "boom\nmore"})),
            ("boom".into(), "err")
        );
        assert_eq!(
            result_summary(&json!({"ok": false})),
            ("The command failed.".into(), "err")
        );
        assert_eq!(result_summary(&json!({"ok": false, "error": "E"})), ("E".into(), "err"));
    }

    #[test]
    fn missing_connections_are_listed_once_in_shelf_order() {
        let mk = |c: &str| Button {
            connection: c.into(),
            ..Button::blank("", "")
        };
        let shelf = [mk("a"), mk("b"), mk("a"), mk("")];
        assert_eq!(missing_connections(&shelf, &[json!({"name": "b"})]), ["a"]);
    }

    #[test]
    fn only_a_terminal_button_restarts() {
        let mut b = Button::blank("", "");
        b.restart = true;
        assert!(restarts(&b));
        b.mode = Mode::Background;
        assert!(!restarts(&b));
        b.mode = Mode::Terminal;
        b.restart = false;
        assert!(!restarts(&b));
        assert_eq!(session_name("x"), "ssh-launcher/x");
    }

    #[test]
    fn the_connection_choices_end_with_the_way_to_type_one() {
        let saved = [
            json!({"name": "pi", "host": "10.0.0.2", "username": "corvus"}),
            json!({"name": "a@b", "host": "b", "username": "a"}),
        ];
        let o = connection_options(&saved, "gone");
        assert_eq!(o[0], ("gone".into(), "gone (not set up on this computer)".into()));
        assert_eq!(o[1], ("pi".into(), "pi (corvus@10.0.0.2)".into()));
        assert_eq!(o[2], ("a@b".into(), "a@b".into()));
        assert_eq!(
            o.last().unwrap(),
            &("__new__".to_string(), "New connection\u{2026}".to_string())
        );
        assert_eq!(connection_options(&[], "").len(), 1);
    }

    #[test]
    fn a_failed_reply_asks_for_what_more_typing_can_fix() {
        let r = json!({"needs": "credentials", "connection": " pi ", "error": "refused"});
        assert_eq!(
            ssh_needs(&r),
            Some(SshNeed {
                name: "pi".into(),
                needs: "credentials".into(),
                error: "refused".into()
            })
        );
        assert_eq!(
            ssh_needs(&json!({"body": {"needs": "connection", "connection": "x"}}))
                .unwrap()
                .needs,
            "connection"
        );
        assert_eq!(ssh_needs(&json!({"needs": "other", "connection": "x"})), None);
        assert_eq!(ssh_needs(&json!({"needs": "connection"})), None);
        assert_eq!(ssh_needs(&json!({})), None);
    }

    #[test]
    fn a_reply_reads_the_same_whatever_went_wrong() {
        assert_eq!(reply_of(Ok((200, json!({"ok": true})))), json!({"ok": true}));
        assert_eq!(
            reply_of(Ok((409, json!({"error": "no"})))),
            json!({"ok": false, "error": "no"})
        );
        assert_eq!(reply_of(Err("down".into())), json!({"ok": false, "error": "down"}));
        assert_eq!(parse_int("22abc"), Some(22));
        assert_eq!(parse_int("x"), None);
    }
}
