//! Schwalby: one-press buttons that start programs on this computer or over
//! SSH (`python/plugins/schwalby/schwalby.js` and `.css`).
//!
//! The job is the SSH Launcher's, with the other half of the ground station
//! added: a program that runs on the laptop itself. Where a button runs is
//! picked per button, Local or SSH, and how it runs is the SSH Launcher's
//! too: TERMINAL types `cd -- <folder> && <command>` into a session of the
//! button's own (`schwalby/<id>`), BACKGROUND starts it without a window
//! (`POST /api/local/run`, stopped when Corvus closes, or `POST /api/ssh/run`
//! with nohup). Above the shelf sits the companion computer: its name and
//! address with a status dot, pinged from this computer (`POST
//! /api/local/ping`) while the plugin is open, with a popup to set it and a
//! power menu on a right click. An AI client can use the shelf through the MCP
//! server when the operator allows it twice (`mcp.rs`).
//!
//! Every press is a small task that moves one step per frame as its backend
//! answers arrive: the native interface never waits on the network.

mod companion;
pub mod mcp;
pub mod model;
mod view;

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use self::companion::{CompDialog, Power, Probe};
use self::model::{
    coerce_button, companion_entry, local_summary, normalize_buttons, normalize_companion, restarts, session_name,
    ssh_summary, terminal_line, Button, Companion, NewConn, Target, CTRL_C, LIVE_POLL_SECS, LOCAL_HOST, MAX_BUTTONS,
    MCP_EDIT, MCP_RUN, NEW_CONNECTION, PLUGIN_ID, RESTART_GRACE_SECS,
};
use super::options::OptionSpec;
use super::ssh_launcher::model::{reply_of, ssh_needs, Mode};
use super::{Host, Info, McpTool, Plugin, Reply};
use crate::net::bg::Pending;

pub(crate) type Rep = Pending<Result<(u16, Value), String>>;

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
    /// A session closed without Ctrl-C: a deleted or moved button's.
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
pub(crate) struct Editor {
    pub draft: Button,
    pub is_new: bool,
    /// The connection select's value.
    pub choice: String,
    pub conn: NewConn,
    pub saving: Option<Rep>,
    pub message: Option<(String, &'static str)>,
    /// The mode the operator chose; where a terminal cannot be had the draft
    /// is background while that is the target, and given back after.
    pub wanted: Mode,
}

/// What a yes answers.
#[derive(Clone)]
pub(crate) enum After {
    /// Remove a running button and close its session.
    Remove(Button),
    /// Save an edit that moves or detaches a running button.
    Leave {
        before: Button,
        entry: Button,
        is_new: bool,
    },
    /// Allow a button an AI client added.
    Approve(String),
    /// Reboot or shut the companion down.
    Power(Power),
}

/// A yes or no question in a dialog.
pub(crate) struct Confirm {
    pub title: String,
    pub lines: Vec<String>,
    pub label: String,
    pub icon: &'static str,
    pub danger: bool,
    pub then: After,
}

pub struct Schwalby {
    open: bool,
    pub(crate) buttons: Vec<Button>,
    pub(crate) companion: Option<Companion>,
    pub(crate) connections: Vec<Value>,
    pub(crate) conn_error: String,
    conn_loaded: bool,
    conn_load: Option<Reply>,
    pub(crate) local_status: Option<Value>,
    local_load: Option<Reply>,
    pub(crate) live: HashSet<String>,
    live_load: Option<Reply>,
    next_poll: f64,
    pub(crate) errors: HashMap<String, String>,
    tasks: Vec<Task>,
    pub(crate) status: Option<(String, &'static str)>,
    pub(crate) output: Option<String>,
    pub(crate) editor: Option<Editor>,
    pub(crate) edit_mode: bool,
    pub(crate) drag: Option<view::RowDrag>,
    tag: u64,
    setups: Vec<(u64, bool)>,
    pub(crate) confirm: Option<Confirm>,
    pub(crate) probe: Probe,
    pub(crate) dialog: Option<CompDialog>,
    pub(crate) power_menu: bool,
    pub(crate) power: Option<companion::PowerRun>,
    /// Where the companion strip was drawn, for the power menu to hang from.
    pub(crate) strip: eframe::egui::Rect,
    /// The calls an AI client made that wait on a save or a press.
    pub(crate) mcp: mcp::State,
}

impl Default for Schwalby {
    fn default() -> Self {
        Schwalby {
            open: false,
            buttons: Vec::new(),
            companion: None,
            connections: Vec::new(),
            conn_error: String::new(),
            conn_loaded: false,
            conn_load: None,
            local_status: None,
            local_load: None,
            live: HashSet::new(),
            live_load: None,
            next_poll: 0.0,
            errors: HashMap::new(),
            tasks: Vec::new(),
            status: None,
            output: None,
            editor: None,
            edit_mode: false,
            drag: None,
            tag: 0,
            setups: Vec::new(),
            confirm: None,
            probe: Probe::default(),
            dialog: None,
            power_menu: false,
            power: None,
            strip: eframe::egui::Rect::NOTHING,
            mcp: mcp::State::default(),
        }
    }
}

impl Plugin for Schwalby {
    fn info(&self) -> Info {
        Info {
            id: PLUGIN_ID,
            name: "Schwalby",
            icon: "bird",
            description: "One-press buttons that start programs on this computer or over SSH",
            version: "1.1.0",
            order: 30,
            can_tab: true,
        }
    }

    fn options(&self) -> Vec<OptionSpec> {
        vec![
            OptionSpec::toggle(MCP_EDIT, "AI clients may add buttons", false).hint(
                "Through Corvus' MCP server an AI client can add buttons that run on this computer, and change or remove the ones it added. A new or changed one waits on the shelf until you press it and allow it.",
            ),
            OptionSpec::toggle(MCP_RUN, "AI clients may press buttons", false).hint(
                "Through Corvus' MCP server an AI client can press any allowed button, which runs its program as your own press would.",
            ),
        ]
    }

    fn open(&mut self, h: &mut Host) {
        let saved = h.settings();
        // a press an AI client made while the shelf was closed is still under way
        let (tasks, mcp, live, errors) = (
            std::mem::take(&mut self.tasks),
            std::mem::take(&mut self.mcp),
            std::mem::take(&mut self.live),
            std::mem::take(&mut self.errors),
        );
        *self = Schwalby {
            tasks,
            mcp,
            live,
            errors,
            open: true,
            buttons: normalize_buttons(&saved),
            companion: saved.get("companion").and_then(normalize_companion),
            conn_load: Some(h.get("/api/ssh/connections")),
            local_load: Some(h.get("/api/local/status")),
            live_load: Some(h.get("/api/ssh/sessions")),
            next_poll: h.now + LIVE_POLL_SECS,
            ..Default::default()
        };
        self.start_probing(h.now);
    }

    fn close(&mut self, _h: &mut Host) {
        self.open = false;
        self.confirm = None;
        self.tasks.clear();
        self.editor = None;
        self.dialog = None;
        self.drag = None;
        self.power_menu = false;
        self.stop_probing();
    }

    fn tick(&mut self, h: &mut Host) {
        if !self.open && self.tasks.is_empty() && self.mcp_idle() {
            return;
        }
        self.poll_lists();
        self.run_tasks(h);
        self.mcp_run_jobs(h);
        if !self.open {
            return;
        }
        self.run_probe(h);
        self.run_power(h);
        self.poll_dialog(h);
        self.run_saves(h);
        if self.editor.is_none() && self.drag.is_none() && h.now >= self.next_poll && self.live_load.is_none() {
            self.live_load = Some(h.get("/api/ssh/sessions"));
            self.next_poll = h.now + LIVE_POLL_SECS;
        }
    }

    fn ssh_setup_done(&mut self, tag: u64, saved: bool, h: &mut Host) {
        self.setups.push((tag, saved));
        if saved {
            self.conn_load = Some(h.get("/api/ssh/connections"));
        }
    }

    fn mcp_tools(&self, h: &Host) -> Vec<McpTool> {
        Self::mcp_tools_for(h)
    }

    fn mcp_call(&mut self, name: &str, args: &Value, token: u64, h: &mut Host) {
        self.mcp_start(name, args, token, h);
    }

    fn mcp_take(&mut self, token: u64) -> Option<Result<Value, String>> {
        self.mcp_result(token)
    }

    fn show(&mut self, ui: &mut eframe::egui::Ui, area: eframe::egui::Rect, h: &mut Host) -> f32 {
        let height = view::show(self, ui, area, h);
        view::dialogs(self, ui.ctx(), h);
        height
    }
}

impl Schwalby {
    fn poll_lists(&mut self) {
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
                        "No saved SSH connections yet. Add a button, pick SSH and choose \u{201C}New connection\u{2026}\u{201D} to enter one here.".into()
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
        if let Some(r) = self.local_load.as_ref().and_then(Pending::take) {
            self.local_load = None;
            self.local_status = r.ok();
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
    }

    /// Why a terminal on this computer cannot be had here, or empty.
    pub(crate) fn local_terminal_reason(&self) -> String {
        let Some(s) = &self.local_status else {
            return String::new();
        };
        if s.get("terminal") != Some(&Value::Bool(false)) {
            return String::new();
        }
        s.get("reason")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
            .unwrap_or("A terminal on this computer is not available here.")
            .to_string()
    }

    /// A stop or a disconnect of this session is under way.
    pub(crate) fn closing(&self, session: &str) -> bool {
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

    /// Whether this button's press is under way.
    pub(crate) fn pressing(&self, id: &str) -> bool {
        self.tasks
            .iter()
            .any(|t| t.button.id == id && matches!(t.kind, Kind::Launch | Kind::Detach))
    }

    /// Whether this session's arrow is connecting.
    pub(crate) fn opening(&self, session: &str) -> bool {
        self.tasks
            .iter()
            .any(|t| t.kind == Kind::Open && session_name(&t.button.id) == session)
    }

    fn where_of(b: &Button) -> String {
        if b.target == Target::Local {
            LOCAL_HOST.into()
        } else if b.connection.is_empty() {
            "ssh".into()
        } else {
            b.connection.clone()
        }
    }

    /// The console line, naming where the program was started.
    fn console_line(b: &Button, line: &str) -> String {
        format!("{PLUGIN_ID} ({}): {line}", Self::where_of(b))
    }

    pub(crate) fn failed(&mut self, b: &Button, detail: &str, h: &mut Host) {
        let detail = if detail.is_empty() { "It failed." } else { detail };
        self.errors.insert(b.id.clone(), detail.to_string());
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
        if self.pressing(&b.id) {
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
        let line = terminal_line(&t.button);
        t.stage = Some(match t.kind {
            Kind::Detach => Stage::Run(Self::run_request(&t.button, h), false),
            _ if restarts(&t.button) => Stage::Interrupt(Self::send(h, &session, CTRL_C)),
            _ => Stage::Send(Self::send(h, &session, &format!("{line}\n")), false),
        });
        t.waiting = false;
    }

    fn run_request(b: &Button, h: &Host) -> Rep {
        if b.target == Target::Local {
            h.post_any(
                "/api/local/run",
                json!({"directory": b.directory, "command": b.command}),
            )
        } else {
            h.post_any(
                "/api/ssh/run",
                json!({"name": b.connection, "directory": b.directory, "command": b.command, "detach": true}),
            )
        }
    }

    fn connect_request(b: &Button, h: &Host) -> Rep {
        if b.target == Target::Local {
            h.post_any("/api/local/connect", json!({"name": session_name(&b.id)}))
        } else {
            h.post_any(
                "/api/ssh/connect",
                json!({"name": session_name(&b.id), "from": b.connection}),
            )
        }
    }

    /// The arrow when nothing runs: connect and open a plain shell.
    pub(crate) fn connect_and_open(&mut self, b: &Button, h: &mut Host) {
        let session = session_name(&b.id);
        if self.opening(&session) {
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
        self.tasks.push(Task {
            button: b.clone(),
            kind: Kind::Stop,
            stage: Some(Stage::Interrupt(Self::send(h, &session, CTRL_C))),
            waiting: false,
        });
    }

    /// Closes a button's session (a deleted button's, or one that moved).
    pub(crate) fn close_session(&mut self, b: &Button, h: &mut Host) {
        let session = session_name(&b.id);
        self.live.remove(&session);
        self.tasks.push(Task {
            button: b.clone(),
            kind: Kind::Close,
            stage: Some(Stage::Disconnect(
                h.post_any("/api/ssh/disconnect", json!({"name": session})),
            )),
            waiting: false,
        });
    }

    /// Removes a button; a running one has its session closed with it.
    pub(crate) fn remove(&mut self, b: &Button, h: &mut Host) {
        let running = self.live.contains(&session_name(&b.id));
        self.buttons.retain(|x| x.id != b.id);
        self.errors.remove(&b.id);
        self.persist(h);
        if running {
            self.close_session(b, h);
        }
    }

    pub(crate) fn open_terminal(&mut self, b: &Button, reattach: bool, existing_only: bool, h: &mut Host) {
        let session = if b.target == Target::Local {
            json!({"name": session_name(&b.id), "title": b.label, "host": LOCAL_HOST})
        } else {
            let c = self
                .connections
                .iter()
                .find(|c| c.get("name").and_then(Value::as_str) == Some(b.connection.as_str()))
                .cloned()
                .unwrap_or_else(|| json!({}));
            json!({
                "name": session_name(&b.id), "title": b.label,
                "host": c.get("host"), "port": c.get("port"), "username": c.get("username"),
            })
        };
        h.terminal(session, reattach, existing_only);
    }

    /// The arrow when the session is up: the window again.
    pub(crate) fn show_terminal(&mut self, b: &Button, h: &mut Host) {
        self.open_terminal(b, false, false, h);
    }

    /// Saves the shelf and the companion. `replace`, because they ARE the
    /// plugin's settings.
    pub(crate) fn persist(&mut self, h: &mut Host) {
        let list: Vec<Value> = self.buttons.iter().map(Button::to_json).collect();
        let mut s = json!({"buttons": list});
        if let (Some(c), Some(o)) = (&self.companion, s.as_object_mut()) {
            o.insert("companion".into(), c.to_json());
        }
        let _ = h.save_settings(&s, true);
    }

    /// A finished draft: it asks first when it would end a running program.
    pub(crate) fn commit(&mut self, draft: Button, is_new: bool, h: &mut Host) {
        let Some(mut entry) = coerce_button(&draft.to_json()) else {
            return;
        };
        // The operator has read it in the editor and saved it: theirs now.
        entry.pending = false;
        let before = if is_new {
            None
        } else {
            self.buttons.iter().find(|b| b.id == entry.id).cloned()
        };
        if let Some(before) = before {
            if self.live.contains(&session_name(&before.id)) {
                let moved = before.target != entry.target
                    || (entry.target == Target::Ssh && before.connection != entry.connection);
                let detached = before.mode != Mode::Background && entry.mode == Mode::Background;
                if moved || detached {
                    self.confirm = Some(Confirm {
                        title: format!("Save \"{}\"?", before.label),
                        lines: vec![
                            "It is still running in its terminal.".into(),
                            "Saving this closes that session, which stops what it is running.".into(),
                        ],
                        label: "Save and close it".into(),
                        icon: "check",
                        danger: true,
                        then: After::Leave { before, entry, is_new },
                    });
                    return;
                }
            }
        }
        self.apply_commit(entry, is_new, h);
    }

    fn apply_commit(&mut self, entry: Button, is_new: bool, h: &mut Host) {
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

    /// The answer of a yes or no dialog.
    pub(crate) fn answer(&mut self, c: Confirm, yes: bool, h: &mut Host) {
        if !yes {
            return;
        }
        match c.then {
            After::Remove(b) => {
                self.remove(&b, h);
                self.editor = None;
            }
            After::Leave { before, entry, is_new } => {
                self.close_session(&before, h);
                self.apply_commit(entry, is_new, h);
            }
            After::Approve(id) => {
                if let Some(b) = self.buttons.iter_mut().find(|b| b.id == id) {
                    b.pending = false;
                    let line = Self::console_line(b, &format!("allowed {}, added by an AI client", b.label));
                    h.console(&line, "info");
                }
                self.persist(h);
            }
            After::Power(p) => self.start_power(p, false, h),
        }
    }

    /// Moves a button to a slot and saves the order.
    pub(crate) fn commit_order(&mut self, id: &str, slot: usize, h: &mut Host) {
        let next = model::move_button(&self.buttons, id, slot);
        if next.iter().zip(&self.buttons).all(|(a, b)| a.id == b.id) {
            return;
        }
        self.buttons = next;
        self.persist(h);
    }

    fn ran(&mut self, b: &Button, line: &str, reconnected: bool, restarted: bool, h: &mut Host) {
        let session = session_name(&b.id);
        let was = self.live.contains(&session);
        self.live.insert(session);
        if reconnected && self.open {
            self.open_terminal(b, true, true, h);
        }
        let said = if restarted && !reconnected {
            format!("{} restarted", b.label)
        } else if was && !reconnected {
            format!("{} sent again to its terminal", b.label)
        } else {
            format!("{} is running", b.label)
        };
        self.status = Some((said, "ok"));
        h.console(&Self::console_line(b, line), "success");
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
        let line = terminal_line(&b);
        let local = b.target == Target::Local;
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
                    (_, Ok((s, _))) if s < 400 => {
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
                        self.status = Some((format!("{}: terminal ready, press the button to start", b.label), "ok"));
                        return true;
                    }
                    t.stage = Some(Stage::Send(Self::send(h, &session, &format!("{line}\n")), true));
                    return false;
                }
                if !retried && !local {
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
                if !local && !retried && res.get("needs").is_some() {
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
                        self.status = Some((format!("{} stopped", b.label), "err"));
                        h.console(&Self::console_line(&b, &format!("stopped {}", b.label)), "info");
                    }
                }
                true
            }
        }
    }

    /// The answer of a background run, in the status line, the console and the
    /// output box.
    fn report(&mut self, b: &Button, res: &Value, h: &mut Host) {
        let (text, kind) = if b.target == Target::Local {
            local_summary(res)
        } else {
            ssh_summary(res)
        };
        if kind == "err" {
            self.failed(b, &text, h);
        } else {
            self.status = Some((format!("{}: {text}", b.label), kind));
        }
        let body: Vec<String> = ["stdout", "stderr", "output"]
            .iter()
            .map(|k| res.get(*k).and_then(Value::as_str).unwrap_or("").trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let ok = res.get("ok") == Some(&json!(true));
        if !body.is_empty() && !ok {
            self.output = Some(body.join("\n"));
        }
        let cmd = res
            .get("command")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .unwrap_or(&b.command)
            .to_string();
        h.console(&Self::console_line(b, &cmd), if ok { "success" } else { "error" });
    }

    /// The saved connection list changed under a dialog's own save.
    pub(crate) fn adopt_connections(&mut self, list: &Value) {
        if let Some(l) = list.as_array() {
            self.connections = l.clone();
            if !self.connections.is_empty() {
                self.conn_error.clear();
            }
        }
    }

    pub(crate) fn conn_loaded(&self) -> bool {
        self.conn_loaded
    }

    pub(crate) fn companion_button(&self) -> Option<Button> {
        self.companion.as_ref().and_then(companion_entry)
    }

    /// The connection select's starting value for a new editor.
    pub(crate) fn first_connection(&self) -> String {
        self.connections
            .first()
            .and_then(|c| c.get("name").and_then(Value::as_str))
            .unwrap_or("")
            .to_string()
    }

    pub(crate) fn open_editor(&mut self, draft: Button, is_new: bool) {
        let choice = if !self.connections.is_empty() || !draft.connection.is_empty() {
            draft.connection.clone()
        } else {
            NEW_CONNECTION.to_string()
        };
        self.status = None;
        let wanted = draft.mode;
        self.editor = Some(Editor {
            draft,
            is_new,
            choice,
            conn: NewConn::blank(),
            saving: None,
            message: None,
            wanted,
        });
    }

    /// Writes from the editor's connection save: the editor finishes the
    /// button once the connection is saved.
    fn run_saves(&mut self, h: &mut Host) {
        let Some(e) = &mut self.editor else {
            return;
        };
        let Some(r) = e.saving.as_ref().and_then(Pending::take) else {
            return;
        };
        e.saving = None;
        let res = reply_of(r);
        if res.get("ok") == Some(&json!(true)) {
            let name = model::new_connection_body(&e.conn)["name"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let list = res.get("connections").cloned().unwrap_or(Value::Null);
            e.draft.connection = name;
            let draft = e.draft.clone();
            let is_new = e.is_new;
            self.adopt_connections(&list);
            self.commit(draft, is_new, h);
        } else {
            e.message = Some((
                res.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("The connection could not be saved.")
                    .to_string(),
                "err",
            ));
        }
    }
}
