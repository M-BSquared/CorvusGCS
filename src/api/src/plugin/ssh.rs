//! The pieces of the SSH plugins that are the same in both: how a button runs
//! (`Mode`), the ad-hoc connection form, what a failed connect says is missing
//! and how a reply is read. Held to what the web functions answered
//! (`python/plugins/ssh-launcher/ssh-launcher.js`: `coerceButton`'s mode,
//! `newConnectionError`, `sshNeeds`, `replyOf`).

use serde_json::{json, Value};

/// Bounded so the shelf stays a shelf.
pub const MAX_BUTTONS: usize = 12;

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

/// A short, collision-free id for a new button. Never shown.
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("b{ms:x}{:x}", N.fetch_add(1, Ordering::Relaxed))
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
    if taken.contains(&name) {
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

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Terminal => "terminal",
            Mode::Background => "background",
        }
    }
}

impl NewConn {
    pub fn blank() -> NewConn {
        NewConn {
            port: "22".into(),
            ..Default::default()
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
