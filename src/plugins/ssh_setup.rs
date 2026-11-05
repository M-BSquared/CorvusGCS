//! `api.sshSetup`: the dialog that asks for what a saved SSH connection lacks
//! on this computer (`askSSHConnection` of `python/src/js/panel.js`), for the
//! plugins that run things over a connection they only know by name. The
//! login is saved under the same name and the plugin tries again.

use eframe::egui;
use serde_json::{json, Value};

use super::ssh_launcher::model::{parse_int, SshNeed};
use super::Reply;
use crate::net::http::{self, Server};
use crate::ui::form::{self, Form};
use crate::ui::kit::{Button, Variant};
use crate::ui::message::Tone;
use crate::ui::modal;

/// Who asked, and what for, so the answer finds its way back.
#[derive(Debug, Clone, PartialEq)]
pub struct Asker {
    pub owner: &'static str,
    pub tag: u64,
}

/// One dialog open.
pub struct SshSetup {
    pub need: SshNeed,
    pub askers: Vec<Asker>,
    host: String,
    port: String,
    username: String,
    password: String,
    key_path: String,
    has_password: bool,
    error: String,
    load: Option<Reply>,
    save: Option<Reply>,
    filled: bool,
}

/// What the dialog came to.
pub enum Outcome {
    Open,
    Saved,
    Cancelled,
}

impl SshSetup {
    pub fn new(ctx: &egui::Context, server: &Server, need: SshNeed, asker: Asker) -> SshSetup {
        let s = server.clone();
        SshSetup {
            need,
            askers: vec![asker],
            host: String::new(),
            port: "22".into(),
            username: String::new(),
            password: String::new(),
            key_path: String::new(),
            has_password: false,
            error: String::new(),
            load: Some(crate::net::bg::Pending::spawn(ctx, move || {
                http::get_json(&s, "/api/ssh/connections")
            })),
            save: None,
            filled: false,
        }
    }

    /// The prefill from the connection this computer already has, if it has one.
    fn fill(&mut self, list: &Value) {
        let known = list.get("connections").and_then(Value::as_array).and_then(|l| {
            l.iter()
                .find(|c| c.get("name").and_then(Value::as_str) == Some(self.need.name.as_str()))
        });
        if let Some(c) = known {
            let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            self.host = s("host");
            self.username = s("username");
            self.key_path = s("key_path");
            if let Some(p) = c.get("port").and_then(Value::as_i64) {
                self.port = p.to_string();
            }
            self.has_password = c.get("has_password").and_then(Value::as_bool) == Some(true);
        }
        self.filled = true;
    }

    /// What `submit` checks before it asks the backend.
    pub fn problem(&self) -> Option<&'static str> {
        if self.host.trim().is_empty() {
            Some("Host is required.")
        } else if self.username.trim().is_empty() {
            Some("Username is required.")
        } else {
            None
        }
    }

    /// The body saved under the same name; a blank password keeps the stored one.
    pub fn body(&self) -> Value {
        let mut b = json!({
            "name": self.need.name, "original_name": self.need.name,
            "host": self.host.trim(), "port": parse_int(self.port.trim()).filter(|p| *p != 0).unwrap_or(22),
            "username": self.username.trim(), "key_path": self.key_path.trim(),
        });
        if !self.password.is_empty() {
            b["password"] = json!(self.password);
        }
        b
    }

    pub fn show(&mut self, ctx: &egui::Context, server: &Server) -> Outcome {
        if let Some(r) = self.load.as_ref().and_then(Reply::take) {
            self.load = None;
            if let Ok(list) = r {
                self.fill(&list);
            } else {
                self.filled = true;
            }
        }
        if let Some(r) = self.save.as_ref().and_then(Reply::take) {
            self.save = None;
            match r {
                Ok(v) if v.get("ok") == Some(&json!(true)) => return Outcome::Saved,
                Ok(v) => {
                    self.error = v
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Save failed")
                        .to_string()
                }
                Err(e) => self.error = if e.is_empty() { "Save failed".into() } else { e },
            }
        }
        let creds = self.need.needs == "credentials";
        let intro = if creds {
            format!(
                "The login for \"{}\" was refused. Enter the user and the password or key file it needs. They are saved on this computer only.",
                self.need.name
            )
        } else {
            format!(
                "This computer has no address for \"{}\" yet. Enter it and the login once. Everything that uses this connection works from then on.",
                self.need.name
            )
        };
        let (mut go, mut cancel) = (false, false);
        let busy = self.save.is_some();
        let placeholder = if self.has_password {
            "leave blank to keep the current password"
        } else {
            "or use a key file"
        };
        let why = if creds { self.need.error.clone() } else { String::new() };
        let error = self.error.clone();
        let title = format!("Set up {}", self.need.name);
        let id = format!("sshsetup-{}", self.need.name);
        let closed = modal::show(ctx, &id, modal::Size::Sm, &title, |ui, x, y, w| {
            let mut f = Form::new(x, y, w);
            form::desc(ui, &mut f, &intro);
            f.skip(14.0);
            if !why.is_empty() {
                form::message(ui, &mut f, &why, Tone::Warn);
                f.skip(14.0);
            }
            form::input(
                ui,
                &mut f,
                "Host",
                "",
                "",
                &mut self.host,
                "192.168.2.10",
                true,
                false,
                "ssetup-host",
            );
            f.skip(14.0);
            form::input(
                ui,
                &mut f,
                "Port",
                "",
                "",
                &mut self.port,
                "22",
                true,
                false,
                "ssetup-port",
            );
            f.skip(14.0);
            form::input(
                ui,
                &mut f,
                "Username",
                "",
                "",
                &mut self.username,
                "corvus",
                false,
                false,
                "ssetup-user",
            );
            f.skip(14.0);
            form::input(
                ui,
                &mut f,
                "Password",
                "",
                "",
                &mut self.password,
                placeholder,
                true,
                true,
                "ssetup-pass",
            );
            f.skip(14.0);
            form::input(
                ui,
                &mut f,
                "Key file",
                "",
                "",
                &mut self.key_path,
                "/home/user/.ssh/id_rsa",
                true,
                false,
                "ssetup-key",
            );
            if !error.is_empty() {
                f.skip(14.0);
                form::message(ui, &mut f, &error, Tone::Err);
            }
            f.skip(20.0);
            let (hit, bottom) = modal::actions(
                ui,
                x,
                f.y,
                w,
                vec![
                    Button::new("CANCEL", Variant::Secondary),
                    Button::new("SAVE", Variant::Primary).icon("check").enabled(!busy),
                ],
            );
            match hit {
                Some(0) => cancel = true,
                Some(1) => go = true,
                _ => {}
            }
            bottom
        });
        if closed || cancel {
            return Outcome::Cancelled;
        }
        if go && !busy {
            match self.problem() {
                Some(p) => self.error = p.into(),
                None => {
                    self.error.clear();
                    let (s, body) = (server.clone(), self.body());
                    self.save = Some(crate::net::bg::Pending::spawn(ctx, move || {
                        http::post_json(&s, "/api/ssh/connections", &body)
                    }));
                }
            }
        }
        Outcome::Open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(host: &str, user: &str, password: &str) -> SshSetup {
        SshSetup {
            need: SshNeed {
                name: "pi".into(),
                needs: "connection".into(),
                error: String::new(),
            },
            askers: Vec::new(),
            host: host.into(),
            port: "2222".into(),
            username: user.into(),
            password: password.into(),
            key_path: " k ".into(),
            has_password: false,
            error: String::new(),
            load: None,
            save: None,
            filled: true,
        }
    }

    #[test]
    fn host_and_user_are_required_before_anything_is_sent() {
        assert_eq!(setup("", "u", "").problem(), Some("Host is required."));
        assert_eq!(setup("h", " ", "").problem(), Some("Username is required."));
        assert_eq!(setup("h", "u", "").problem(), None);
    }

    #[test]
    fn a_blank_password_keeps_the_stored_one() {
        let b = setup("h", "u", "").body();
        assert!(b.get("password").is_none());
        assert_eq!(b["original_name"], "pi");
        assert_eq!(b["port"], 2222);
        assert_eq!(b["key_path"], "k");
        assert_eq!(setup("h", "u", "pw").body()["password"], "pw");
    }
}
