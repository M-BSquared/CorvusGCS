//! SSH Launcher: one-press buttons that start programs on a companion
//! computer (`python/plugins/ssh-launcher/ssh-launcher.js` and `.css`).
//!
//! A button either types `cd -- <folder> && <command>` into an SSH session of
//! its own (TERMINAL, the default; pressing it again types the line into the
//! same shell, and RESTART sends Ctrl-C first) or starts the program with
//! nohup over `POST /api/ssh/run` (BACKGROUND). The login is a saved SSH
//! connection resolved by the backend, so a button holds only its name.
//!
//! Every press is a small task that moves one step per frame as its backend
//! answers arrive: the native interface never waits on the network.

pub mod model;
mod view;

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use self::model::{
    coerce_button, normalize_buttons, reply_of, restarts, session_name, ssh_needs, Button, Mode, NewConn, CTRL_C,
    LIVE_POLL_SECS, MAX_BUTTONS, RESTART_GRACE_SECS,
};
use corvus_api::net::bg::Pending;
use corvus_api::plugin::{Host, Info, Plugin, Reply};

type Rep = Pending<Result<(u16, Value), String>>;

/// What a press is doing.
enum Stage {
    /// Ctrl-C sent, and what comes after a grace period.
    Interrupt(Rep),
    Grace(f64),
    /// A line typed into the session; `true` once it was a fresh connection.
    Send(Rep, bool),
    Connect(Rep, bool),
    /// The login dialog is open for this tag; the reply that asked for it.
    Setup(u64, Value),
    Run(Rep, bool),
    Disconnect(Rep),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Launch,
    Open,
    Stop,
    /// A deleted button's session, closed without Ctrl-C.
    Close,
    Detach,
}

struct Task {
    button: Button,
    kind: Kind,
    stage: Option<Stage>,
    /// Waits until the session's earlier connect or disconnect is over.
    waiting: bool,
}

/// One button being edited: a copy, so Cancel really cancels.
struct Editor {
    draft: Button,
    is_new: bool,
    /// The connection select's value.
    choice: String,
    conn: NewConn,
    saving: Option<Rep>,
    message: Option<(String, &'static str)>,
}

#[derive(Default)]
pub struct SshLauncher {
    open: bool,
    buttons: Vec<Button>,
    connections: Vec<Value>,
    conn_error: String,
    conn_loaded: bool,
    conn_load: Option<Reply>,
    live: HashSet<String>,
    live_load: Option<Reply>,
    next_poll: f64,
    errors: HashMap<String, String>,
    tasks: Vec<Task>,
    status: Option<(String, &'static str)>,
    output: Option<String>,
    editor: Option<Editor>,
    tag: u64,
    setups: Vec<(u64, bool)>,
    /// A running button's removal waits for a yes.
    confirm_delete: Option<Button>,
}

impl Plugin for SshLauncher {
    fn info(&self) -> Info {
        Info {
            id: "ssh-launcher",
            name: "SSH Launcher",
            icon: "rocket",
            description: "One-press buttons that start programs on a companion computer",
            version: "1.3.0",
            order: 20,
            can_tab: false,
        }
    }

    fn open(&mut self, h: &mut Host) {
        *self = SshLauncher {
            open: true,
            buttons: normalize_buttons(&h.settings()),
            conn_load: Some(h.get("/api/ssh/connections")),
            live_load: Some(h.get("/api/ssh/sessions")),
            next_poll: h.now + LIVE_POLL_SECS,
            ..Default::default()
        };
    }

    fn close(&mut self, _h: &mut Host) {
        self.open = false;
        self.confirm_delete = None;
        self.tasks.clear();
        self.editor = None;
    }

    fn tick(&mut self, h: &mut Host) {
        if !self.open {
            return;
        }
        self.poll_lists(h);
        self.run_tasks(h);
        if self.editor.is_none() && h.now >= self.next_poll && self.live_load.is_none() {
            self.live_load = Some(h.get("/api/ssh/sessions"));
            self.next_poll = h.now + LIVE_POLL_SECS;
        }
        if let Some(e) = &mut self.editor {
            if let Some(r) = e.saving.as_ref().and_then(Pending::take) {
                e.saving = None;
                let res = reply_of(r);
                if res.get("ok") == Some(&json!(true)) {
                    if let Some(list) = res.get("connections").and_then(Value::as_array) {
                        self.connections = list.clone();
                    }
                    if !self.connections.is_empty() {
                        self.conn_error.clear();
                    }
                    let name = model::new_connection_body(&e.conn)["name"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    e.draft.connection = name;
                    let draft = e.draft.clone();
                    let is_new = e.is_new;
                    self.commit(draft, is_new, h);
                } else {
                    let msg = res
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("The connection could not be saved.");
                    e.message = Some((msg.to_string(), "err"));
                }
            }
        }
    }

    fn ssh_setup_done(&mut self, tag: u64, saved: bool, h: &mut Host) {
        self.setups.push((tag, saved));
        if saved {
            self.conn_load = Some(h.get("/api/ssh/connections"));
        }
    }

    fn show(&mut self, ui: &mut eframe::egui::Ui, area: eframe::egui::Rect, h: &mut Host) -> f32 {
        let height = view::show(self, ui, area, h);
        view::confirm(self, ui.ctx(), h);
        height
    }
}

impl SshLauncher {
    fn poll_lists(&mut self, h: &mut Host) {
        if let Some(r) = self.conn_load.as_ref().and_then(Pending::take) {
            self.conn_load = None;
            match r {
                Ok(d) => {
                    self.connections = d
                        .get("connections")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    self.conn_loaded = true;
                    self.conn_error = if self.connections.is_empty() {
                        "No saved SSH connections yet. Add a button and choose \u{201C}New connection\u{2026}\u{201D} to enter one here.".into()
                    } else {
                        String::new()
                    };
                }
                Err(_) => {
                    self.connections.clear();
                    self.conn_error = "Could not read the saved SSH connections.".into();
                }
            }
        }
        if let Some(r) = self.live_load.as_ref().and_then(Pending::take) {
            self.live_load = None;
            self.live = match r {
                Ok(d) => d
                    .get("sessions")
                    .and_then(Value::as_array)
                    .map(|l| {
                        l.iter()
                            .filter(|s| s.get("connected").and_then(Value::as_bool) == Some(true))
                            .filter_map(|s| s.get("name").and_then(Value::as_str))
                            .filter(|n| !self.closing(n))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                Err(_) => HashSet::new(),
            };
        }
        let _ = h;
    }

    /// A stop or a disconnect of this session is under way.
    fn closing(&self, session: &str) -> bool {
        self.tasks
            .iter()
            .any(|t| matches!(t.kind, Kind::Stop | Kind::Close) && session_name(&t.button.id) == session)
    }

    /// A connect or a disconnect of this session is under way.
    fn busy_session(&self, session: &str) -> bool {
        self.tasks
            .iter()
            .any(|t| matches!(t.kind, Kind::Stop | Kind::Close | Kind::Open) && session_name(&t.button.id) == session)
    }

    fn failed(&mut self, b: &Button, detail: &str, h: &mut Host) {
        self.errors.insert(
            b.id.clone(),
            if detail.is_empty() {
                "It failed.".into()
            } else {
                detail.into()
            },
        );
        h.notify("warning", &format!("{}: {detail}", b.label));
    }

    fn send(h: &Host, session: &str, data: &str) -> Rep {
        h.post_any("/api/ssh/send", json!({"name": session, "data": data}))
    }

    /// The press of a button.
    pub(crate) fn launch(&mut self, b: &Button, h: &mut Host) {
        self.status = None;
        self.output = None;
        self.errors.remove(&b.id);
        if self
            .tasks
            .iter()
            .any(|t| t.button.id == b.id && matches!(t.kind, Kind::Launch | Kind::Detach))
        {
            return;
        }
        let kind = if b.mode == Mode::Background {
            Kind::Detach
        } else {
            Kind::Launch
        };
        let session = session_name(&b.id);
        let waiting = self.busy_session(&session);
        let mut task = Task {
            button: b.clone(),
            kind,
            stage: None,
            waiting,
        };
        if !waiting {
            Self::begin(&mut task, h);
        }
        self.tasks.push(task);
    }

    fn begin(t: &mut Task, h: &Host) {
        let session = session_name(&t.button.id);
        let line = model::remote_line(&t.button.directory, &t.button.command);
        t.stage = Some(match t.kind {
            Kind::Detach => Stage::Run(Self::run_request(&t.button, h), false),
            _ if restarts(&t.button) => Stage::Interrupt(Self::send(h, &session, CTRL_C)),
            _ => Stage::Send(Self::send(h, &session, &format!("{line}\n")), false),
        });
        t.waiting = false;
    }

    fn run_request(b: &Button, h: &Host) -> Rep {
        h.post_any(
            "/api/ssh/run",
            json!({"name": b.connection, "directory": b.directory, "command": b.command, "detach": true}),
        )
    }

    fn connect_request(b: &Button, h: &Host) -> Rep {
        h.post_any(
            "/api/ssh/connect",
            json!({"name": session_name(&b.id), "from": b.connection}),
        )
    }

    /// The arrow when nothing runs: connect and open a plain shell.
    pub(crate) fn connect_and_open(&mut self, b: &Button, h: &mut Host) {
        let session = session_name(&b.id);
        if self
            .tasks
            .iter()
            .any(|t| t.kind == Kind::Open && session_name(&t.button.id) == session)
        {
            return;
        }
        self.status = None;
        self.errors.remove(&b.id);
        let waiting = self.busy_session(&session);
        let mut task = Task {
            button: b.clone(),
            kind: Kind::Open,
            stage: None,
            waiting,
        };
        if !waiting {
            task.stage = Some(Stage::Connect(Self::connect_request(b, h), false));
        }
        self.tasks.push(task);
    }

    /// The stop button: Ctrl-C, then close the session.
    pub(crate) fn stop(&mut self, b: &Button, h: &mut Host) {
        let session = session_name(&b.id);
        if self.closing(&session) {
            return;
        }
        self.status = None;
        self.errors.remove(&b.id);
        self.live.remove(&session);
        self.tasks.push(Task {
            button: b.clone(),
            kind: Kind::Stop,
            stage: Some(Stage::Interrupt(Self::send(h, &session, CTRL_C))),
            waiting: false,
        });
    }

    /// Removes a button; a running one has its session closed with it.
    pub(crate) fn remove(&mut self, b: &Button, h: &mut Host) {
        self.buttons.retain(|x| x.id != b.id);
        self.errors.remove(&b.id);
        self.persist(h);
        let session = session_name(&b.id);
        if self.live.remove(&session) {
            self.tasks.push(Task {
                button: b.clone(),
                kind: Kind::Close,
                stage: Some(Stage::Disconnect(
                    h.post_any("/api/ssh/disconnect", json!({"name": session})),
                )),
                waiting: false,
            });
        }
    }

    fn open_terminal(&mut self, b: &Button, reattach: bool, existing_only: bool, h: &mut Host) {
        let c = self
            .connections
            .iter()
            .find(|c| c.get("name").and_then(Value::as_str) == Some(b.connection.as_str()))
            .cloned()
            .unwrap_or_else(|| json!({}));
        h.terminal(
            json!({
                "name": session_name(&b.id), "title": b.label,
                "host": c.get("host"), "port": c.get("port"), "username": c.get("username"),
            }),
            reattach,
            existing_only,
        );
    }

    /// The arrow when the session is up: the window again.
    pub(crate) fn show_terminal(&mut self, b: &Button, h: &mut Host) {
        self.open_terminal(b, false, false, h);
    }

    /// Saves the shelf. `replace`, because the shelf IS the plugin's settings.
    pub(crate) fn persist(&mut self, h: &mut Host) {
        let list: Vec<Value> = self.buttons.iter().map(Button::to_json).collect();
        let _ = h.save_settings(&json!({"buttons": list}), true);
    }

    /// Puts a finished draft on the shelf.
    fn commit(&mut self, draft: Button, is_new: bool, h: &mut Host) {
        let Some(entry) = coerce_button(&draft.to_json()) else {
            return;
        };
        if is_new {
            if self.buttons.len() < MAX_BUTTONS {
                self.buttons.push(entry);
            }
        } else {
            for b in &mut self.buttons {
                if b.id == entry.id {
                    *b = entry.clone();
                }
            }
        }
        self.persist(h);
        self.editor = None;
    }

    fn ran(&mut self, b: &Button, line: &str, reconnected: bool, restarted: bool, h: &mut Host) {
        let session = session_name(&b.id);
        let was = self.live.contains(&session);
        self.live.insert(session);
        if reconnected {
            self.open_terminal(b, true, true, h);
        }
        let said = if restarted && !reconnected {
            format!(
                "{}: stopped with Ctrl-C and started again. The arrow opens its terminal.",
                b.label
            )
        } else if was && !reconnected {
            format!("{}: sent again to its terminal. The arrow opens it.", b.label)
        } else {
            format!("{}: running. The arrow opens its terminal.", b.label)
        };
        self.status = Some((said, "ok"));
        h.console(&format!("ssh-launcher: {line}"), "success");
    }

    /// Moves every task one step.
    fn run_tasks(&mut self, h: &mut Host) {
        let tasks = std::mem::take(&mut self.tasks);
        let mut keep = Vec::new();
        for mut t in tasks {
            if t.waiting {
                let session = session_name(&t.button.id);
                let others = self.tasks.iter().chain(keep.iter()).any(|o: &Task| {
                    matches!(o.kind, Kind::Stop | Kind::Close | Kind::Open) && session_name(&o.button.id) == session
                });
                if others {
                    keep.push(t);
                    continue;
                }
                match t.kind {
                    Kind::Open => t.stage = Some(Stage::Connect(Self::connect_request(&t.button, h), false)),
                    _ => Self::begin(&mut t, h),
                }
            }
            if !self.step(&mut t, h) {
                keep.push(t);
            }
        }
        keep.append(&mut self.tasks);
        self.tasks = keep;
    }

    /// One step of one task; true when it is over.
    fn step(&mut self, t: &mut Task, h: &mut Host) -> bool {
        let Some(stage) = t.stage.take() else {
            return false;
        };
        let b = t.button.clone();
        let session = session_name(&b.id);
        let line = model::remote_line(&b.directory, &b.command);
        let transport = |e: String| {
            if e.is_empty() {
                "Could not reach the backend".to_string()
            } else {
                e
            }
        };
        match stage {
            Stage::Interrupt(rep) => {
                let Some(r) = rep.take() else {
                    t.stage = Some(Stage::Interrupt(rep));
                    return false;
                };
                match (t.kind, r) {
                    (Kind::Stop, r) => {
                        let ok = matches!(&r, Ok((s, v)) if *s < 400 && v.get("ok") == Some(&json!(true)));
                        t.stage = Some(if ok {
                            Stage::Grace(h.now + RESTART_GRACE_SECS)
                        } else {
                            Stage::Disconnect(h.post_any("/api/ssh/disconnect", json!({"name": session})))
                        });
                    }
                    (_, Ok((s, v))) if s < 400 && v.get("ok") == Some(&json!(true)) => {
                        t.stage = Some(Stage::Grace(h.now + RESTART_GRACE_SECS));
                    }
                    (_, Ok((s, v))) if s < 400 => {
                        let _ = v;
                        t.stage = Some(Stage::Connect(Self::connect_request(&b, h), false));
                    }
                    (_, Ok((s, v))) => {
                        let said = v
                            .get("error")
                            .and_then(Value::as_str)
                            .map_or(format!("Request failed ({s})"), str::to_string);
                        self.failed(&b, &said, h);
                        return true;
                    }
                    (_, Err(e)) => {
                        self.failed(&b, &transport(e), h);
                        return true;
                    }
                }
                false
            }
            Stage::Grace(until) => {
                if h.now < until {
                    t.stage = Some(Stage::Grace(until));
                    h.ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return false;
                }
                if t.kind == Kind::Stop {
                    t.stage = Some(Stage::Disconnect(
                        h.post_any("/api/ssh/disconnect", json!({"name": session})),
                    ));
                } else {
                    t.stage = Some(Stage::Send(Self::send(h, &session, &format!("{line}\n")), false));
                }
                false
            }
            Stage::Send(rep, reconnected) => {
                let Some(r) = rep.take() else {
                    t.stage = Some(Stage::Send(rep, reconnected));
                    return false;
                };
                match r {
                    Ok((s, v)) if s < 400 => {
                        if reconnected || v.get("ok") == Some(&json!(true)) {
                            self.ran(&b, &line, reconnected, restarts(&b), h);
                            true
                        } else {
                            t.stage = Some(Stage::Connect(Self::connect_request(&b, h), false));
                            false
                        }
                    }
                    Ok((s, v)) => {
                        let said = v
                            .get("error")
                            .and_then(Value::as_str)
                            .map_or(format!("Request failed ({s})"), str::to_string);
                        self.failed(&b, &said, h);
                        true
                    }
                    Err(e) => {
                        self.failed(&b, &transport(e), h);
                        true
                    }
                }
            }
            Stage::Connect(rep, retried) => {
                let Some(r) = rep.take() else {
                    t.stage = Some(Stage::Connect(rep, retried));
                    return false;
                };
                let res = reply_of(r);
                if res.get("ok") == Some(&json!(true)) && res.get("connected") == Some(&json!(true)) {
                    if t.kind == Kind::Open {
                        self.live.insert(session);
                        self.open_terminal(&b, true, false, h);
                        self.status = Some((
                            format!("{}: connected. The button starts it in this terminal.", b.label),
                            "ok",
                        ));
                        return true;
                    }
                    t.stage = Some(Stage::Send(Self::send(h, &session, &format!("{line}\n")), true));
                    return false;
                }
                if !retried {
                    if let Some(need) = ssh_needs(&res) {
                        self.tag += 1;
                        h.ssh_setup(need, self.tag);
                        t.stage = Some(Stage::Setup(self.tag, res));
                        return false;
                    }
                }
                let said = res
                    .get("error")
                    .and_then(Value::as_str)
                    .filter(|e| !e.is_empty())
                    .unwrap_or("Could not open the terminal");
                self.failed(&b, said, h);
                true
            }
            Stage::Setup(tag, res) => {
                let Some(i) = self.setups.iter().position(|s| s.0 == tag) else {
                    t.stage = Some(Stage::Setup(tag, res));
                    return false;
                };
                let (_, saved) = self.setups.remove(i);
                if saved {
                    t.stage = Some(if t.kind == Kind::Detach {
                        Stage::Run(Self::run_request(&b, h), true)
                    } else {
                        Stage::Connect(Self::connect_request(&b, h), true)
                    });
                    return false;
                }
                if t.kind == Kind::Detach {
                    self.report(&b, &res, h);
                } else {
                    let said = res
                        .get("error")
                        .and_then(Value::as_str)
                        .filter(|e| !e.is_empty())
                        .unwrap_or("Could not open the terminal");
                    self.failed(&b, said, h);
                }
                true
            }
            Stage::Run(rep, retried) => {
                let Some(r) = rep.take() else {
                    t.stage = Some(Stage::Run(rep, retried));
                    return false;
                };
                let res = reply_of(r);
                if !retried && res.get("needs").is_some() {
                    if let Some(need) = ssh_needs(&res) {
                        self.tag += 1;
                        h.ssh_setup(need, self.tag);
                        t.stage = Some(Stage::Setup(self.tag, res));
                        return false;
                    }
                }
                self.report(&b, &res, h);
                true
            }
            Stage::Disconnect(rep) => {
                let Some(r) = rep.take() else {
                    t.stage = Some(Stage::Disconnect(rep));
                    return false;
                };
                let reached = r.is_ok();
                if t.kind == Kind::Stop {
                    if !reached {
                        self.failed(&b, "Could not reach the backend to stop it", h);
                    } else {
                        self.status = Some((
                            format!("{}: stopped. Its terminal connection is closed.", b.label),
                            "ok",
                        ));
                        h.console(&format!("ssh-launcher: stopped {}", b.label), "info");
                    }
                }
                true
            }
        }
    }

    /// The answer of a background run, in the status line, the console and the output box.
    fn report(&mut self, b: &Button, res: &Value, h: &mut Host) {
        let (text, kind) = model::result_summary(res);
        if kind == "err" {
            self.failed(b, &text, h);
        } else {
            self.status = Some((format!("{}: {text}", b.label), kind));
        }
        let body: Vec<String> = ["stdout", "stderr"]
            .iter()
            .map(|k| res.get(*k).and_then(Value::as_str).unwrap_or("").trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !body.is_empty() && res.get("ok") != Some(&json!(true)) {
            self.output = Some(body.join("\n"));
        }
        let cmd = res
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or(&b.command)
            .to_string();
        h.console(
            &format!("ssh-launcher: {cmd}"),
            if res.get("ok") == Some(&json!(true)) {
                "success"
            } else {
                "error"
            },
        );
    }
}

/// The language files of this plugin, for the translation layer.
pub const LANG: &[(&str, &str)] = &[
    ("de", include_str!("../lang/de.json")),
    ("fr", include_str!("../lang/fr.json")),
    ("dl", include_str!("../lang/dl.json")),
];

/// One instance of the plugin, for the registry.
pub fn plugin() -> Box<dyn Plugin> {
    Box::new(SshLauncher::default())
}
