//! The SSH Launcher's rules, apart from any window
//! (`python/plugins/ssh-launcher/ssh-launcher.js`: `coerceButton`,
//! `normalizeButtons`, `remoteLine`, `previewLine`, `newConnectionError` and
//! the other pure helpers). Held to what the web functions answered.

pub use corvus_api::plugin::ssh::*;
use serde_json::{json, Value};

/// How often the shelf asks which sessions are still up.
pub const LIVE_POLL_SECS: f64 = 4.0;

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

#[cfg(test)]
mod tests {
    use super::*;

    // What the web functions answered for the same inputs (`model_cases.json`,
    // written by `src/plugins/dump_cases.js` from python/plugins).
    fn cases(name: &str) -> Vec<(Value, Value)> {
        let all: Value = serde_json::from_str(include_str!("model_cases.json")).expect("model_cases.json");
        all[name]
            .as_array()
            .unwrap_or_else(|| panic!("no cases for {name}"))
            .iter()
            .map(|c| (c["in"].clone(), c["out"].clone()))
            .collect()
    }

    fn text_of(v: &Value, k: &str) -> String {
        v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
    }

    fn conn_of(v: &Value) -> NewConn {
        NewConn {
            name: text_of(v, "name"),
            host: text_of(v, "host"),
            port: text_of(v, "port"),
            username: text_of(v, "username"),
            password: text_of(v, "password"),
            key_path: text_of(v, "key_path"),
        }
    }

    #[test]
    fn a_button_is_made_as_the_web_makes_it() {
        for (i, out) in cases("coerceButton") {
            let got = coerce_button(&i[0]).map_or(Value::Null, |b| b.to_json());
            assert_eq!(got, out, "{i}");
        }
        for (i, out) in cases("normalizeButtons") {
            let got: Vec<Value> = normalize_buttons(&i[0]).iter().map(Button::to_json).collect();
            let mut want: Vec<Value> = out.as_array().unwrap().clone();
            let (got, want): (Vec<Value>, Vec<Value>) = (
                got.into_iter()
                    .map(|mut b| {
                        b["id"] = Value::Null;
                        b
                    })
                    .collect(),
                want.iter_mut()
                    .map(|b| {
                        b["id"] = Value::Null;
                        b.clone()
                    })
                    .collect(),
            );
            assert_eq!(got, want, "{i}");
        }
    }

    #[test]
    fn the_lines_are_the_ones_the_web_builds() {
        for (i, out) in cases("remoteLine") {
            let e = &i[0];
            assert_eq!(
                remote_line(&text_of(e, "directory"), &text_of(e, "command")),
                out,
                "{i}"
            );
        }
        for (i, out) in cases("previewLine") {
            let e = &i[0];
            let got = preview_line(
                &text_of(e, "connection"),
                &text_of(e, "directory"),
                &text_of(e, "command"),
                coerce_mode(e),
            );
            assert_eq!(got, out, "{i}");
        }
    }

    #[test]
    fn the_modes_and_restarts_are_the_webs() {
        for (i, out) in cases("coerceMode") {
            assert_eq!(coerce_mode(&i[0]).as_str(), out, "{i}");
        }
        for (i, out) in cases("restarts") {
            let e = &i[0];
            let b = Button {
                restart: e.get("restart") == Some(&json!(true)),
                mode: if e.is_object() {
                    coerce_mode(e)
                } else {
                    Mode::Background
                },
                ..Button::blank("", "")
            };
            assert_eq!(restarts(&b), out.as_bool().unwrap(), "{i}");
        }
    }

    #[test]
    fn a_new_connection_is_named_checked_and_sent_as_the_web_does() {
        for (i, out) in cases("derivedName") {
            assert_eq!(derived_name(&conn_of(&i[0])), out, "{i}");
        }
        for (i, out) in cases("newConnectionError") {
            let taken: Vec<String> = i[1]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap().into())
                .collect();
            assert_eq!(new_connection_error(&conn_of(&i[0]), &taken), out, "{i}");
        }
        for (i, out) in cases("newConnectionBody") {
            assert_eq!(new_connection_body(&conn_of(&i[0])), out, "{i}");
        }
    }

    #[test]
    fn a_background_run_and_the_missing_connections_read_as_the_web_reads_them() {
        for (i, out) in cases("resultSummary") {
            let (text, kind) = result_summary(&i[0]);
            assert_eq!(json!({"text": text, "kind": kind}), out, "{i}");
        }
        for (i, out) in cases("missingConnections") {
            let buttons: Vec<Button> = i[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| Button::blank(&text_of(b, "connection"), ""))
                .collect();
            let known = i[1].as_array().cloned().unwrap_or_default();
            assert_eq!(json!(missing_connections(&buttons, &known)), out, "{i}");
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
}
