//! What a plugin is built from: the [`Plugin`] trait, the [`Host`] it is handed
//! to talk to the app, its options and the shapes it draws on the map. It is
//! the native counterpart of `Corvus.plugins.register` and the `api` object a
//! web plugin receives (`python/src/js/plugins.js`): a card under PLUGINS (or a
//! tab of its own), saved settings, options in Settings, shapes on the Home map,
//! notifications, the console, a terminal over a saved SSH connection and calls
//! to the backend. A plugin is a crate of its own that depends on this one and
//! on nothing inside the program; how to write one is in `src/README.md`.

pub mod options;
pub mod shapes;
pub mod ssh;

use eframe::egui;
use serde_json::{json, Map, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use options::{OptionSpec, OPTIONS_KEY};
use shapes::ShapeSet;

/// An answer from the backend still on its way.
pub type Reply = Pending<Result<Value, String>>;

/// What the registry shows of a plugin before it is opened (`plugin.json`).
#[derive(Debug, Clone, Copy)]
pub struct Info {
    pub id: &'static str,
    pub name: &'static str,
    /// A Lucide icon name.
    pub icon: &'static str,
    pub description: &'static str,
    pub version: &'static str,
    /// Where the card sits in the grid: lower is earlier, ties by name.
    pub order: i32,
    /// Whether it can live in a tab of its own, switched on with "Own tab".
    pub can_tab: bool,
}

/// What the pointer did to a shape on the map.
#[derive(Debug, Clone, PartialEq)]
pub enum MapEvent {
    /// A marker or button was pressed.
    Click(String),
    /// A marker is being dragged, now over this point.
    Drag(String, [f64; 2]),
    /// A dragged marker was let go over this point.
    DragEnd(String, [f64; 2]),
    /// The click `pick_point` waited for.
    Picked([f64; 2]),
    /// Escape or a right click gave the pick up.
    PickCancelled,
}

/// One tool a plugin offers an AI client through Corvus' MCP server
/// (`mcpTools` of a plugin's spec).
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub title: String,
    pub description: String,
    /// A JSON schema of type `object`.
    pub input_schema: Value,
    /// The operator is asked before every call.
    pub confirm: bool,
    pub read_only: bool,
}

/// What a plugin may do. Everything but `info` and `show` is optional.
pub trait Plugin {
    fn info(&self) -> Info;

    /// The settings the operator sets from the plugin's gear in Settings.
    fn options(&self) -> Vec<OptionSpec> {
        Vec::new()
    }

    /// Once per session, as soon as the saved settings are known, whether or
    /// not the plugin is ever opened: a line restored on the map.
    fn start(&mut self, _h: &mut Host) {}

    /// Every frame, open or not. Telemetry is `h.state`.
    fn tick(&mut self, _h: &mut Host) {}

    /// The view is about to be shown (`init`).
    fn open(&mut self, _h: &mut Host) {}

    /// Draws the view into `area`, from its top, and returns the height used.
    fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect, h: &mut Host) -> f32;

    /// The view is gone (`destroy`): leave nothing behind.
    fn close(&mut self, _h: &mut Host) {}

    /// The operator changed an option; `h.options()` has them all.
    fn options_changed(&mut self, _h: &mut Host) {}

    /// The pointer did something to one of this plugin's shapes.
    fn map_event(&mut self, _e: MapEvent, _h: &mut Host) {}

    /// The operator answered the login dialog this plugin asked for with
    /// `ssh_setup`: `saved` is whether a login was saved under that name.
    fn ssh_setup_done(&mut self, _tag: u64, _saved: bool, _h: &mut Host) {}

    /// The tools an AI client may use right now (`mcpTools`); they follow the
    /// plugin's options, so they are asked for again when those change.
    fn mcp_tools(&self, _h: &Host) -> Vec<McpTool> {
        Vec::new()
    }

    /// An AI client calls tool `name`. The answer is taken with `mcp_take`
    /// under the same `token` once there is one.
    fn mcp_call(&mut self, _name: &str, _args: &Value, _token: u64, _h: &mut Host) {}

    /// The answer of a call, or `None` while it is still under way.
    fn mcp_take(&mut self, _token: u64) -> Option<Result<Value, String>> {
        Some(Err("The plugin offers no such tool right now.".into()))
    }
}

/// What the plugins asked of the app during a frame.
#[derive(Default)]
pub struct Outbox {
    /// `(plugin name, level, text)`: a toast and a line on the board.
    pub notices: Vec<(String, &'static str, String)>,
    /// `(level, text)` lines for the console.
    pub console: Vec<(String, String)>,
    /// Saved SSH connections to show in a terminal window of their own, with
    /// whether the window is made again (`reattach`) and whether none is
    /// opened when there is not one already (`existing_only`).
    pub terminals: Vec<(Value, bool, bool)>,
    /// Login dialogs asked for: `(plugin, tag, what is missing)`.
    pub ssh_setups: Vec<(&'static str, u64, ssh::SshNeed)>,
    /// Frame these `[lng, lat]` on the Home map.
    pub fit: Option<Vec<[f64; 2]>>,
    /// Who waits for a click on the map.
    pub pick: Option<&'static str>,
    pub cancel_pick: bool,
}

/// What a plugin is handed to talk to the app (`api`).
pub struct Host<'a> {
    pub ctx: &'a egui::Context,
    pub server: &'a Server,
    /// The latest telemetry state.
    pub state: &'a Value,
    /// Seconds, wall clock.
    pub now: f64,
    id: &'static str,
    name: &'static str,
    shapes: &'a mut ShapeSet,
    saved: &'a mut Value,
    opts: &'a Map<String, Value>,
    out: &'a mut Outbox,
}

impl<'a> Host<'a> {
    /// A host for the plugin `info` names: what the registry hands a plugin
    /// for the length of one call.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx: &'a egui::Context,
        server: &'a Server,
        state: &'a Value,
        info: Info,
        shapes: &'a mut ShapeSet,
        saved: &'a mut Value,
        opts: &'a Map<String, Value>,
        out: &'a mut Outbox,
    ) -> Host<'a> {
        Host {
            ctx,
            server,
            state,
            now: now_secs(),
            id: info.id,
            name: info.name,
            shapes,
            saved,
            opts,
            out,
        }
    }
}

impl Host<'_> {
    /// `api.map`: shapes on the Home map, under this plugin's own keys.
    pub fn map(&mut self) -> &mut ShapeSet {
        self.shapes
    }

    /// `getSettings`: this plugin's saved state, without the options.
    pub fn settings(&self) -> Value {
        let mut s = self.saved.clone();
        if let Some(o) = s.as_object_mut() {
            o.remove(OPTIONS_KEY);
        }
        if s.is_object() {
            s
        } else {
            json!({})
        }
    }

    /// `getOptions`: every declared option, as set or by default.
    pub fn options(&self) -> &Map<String, Value> {
        self.opts
    }

    /// `saveSettings`: merges `patch` into the saved state (or stores it as
    /// the whole with `replace`) and persists it through
    /// `POST /api/plugins/settings`. The options are the operator's and are
    /// neither written nor cleared from here.
    pub fn save_settings(&mut self, patch: &Value, replace: bool) -> Reply {
        let mut own = patch.as_object().cloned().unwrap_or_default();
        own.remove(OPTIONS_KEY);
        let options = self.saved.get(OPTIONS_KEY).cloned();
        let mut merged = if replace {
            Map::new()
        } else {
            self.saved.as_object().cloned().unwrap_or_default()
        };
        merged.extend(own.clone());
        merged.remove(OPTIONS_KEY);
        if let Some(o) = options.filter(|o| o.as_object().is_some_and(|m| !m.is_empty())) {
            merged.insert(OPTIONS_KEY.into(), o);
        }
        *self.saved = Value::Object(merged);
        self.post(
            "/api/plugins/settings",
            json!({"id": self.id, "settings": own, "replace": replace}),
        )
    }

    /// A GET on the backend, off the interface thread.
    pub fn get(&self, path: &str) -> Reply {
        let (server, path) = (self.server.clone(), path.to_string());
        Pending::spawn(self.ctx, move || http::get_json(&server, &path))
    }

    /// A POST that answers with its body; an HTTP error is the error text.
    pub fn post(&self, path: &str, body: Value) -> Reply {
        let (server, path) = (self.server.clone(), path.to_string());
        Pending::spawn(self.ctx, move || http::post_json(&server, &path, &body))
    }

    /// `postJson`: a POST whose body is the answer even when it says
    /// `ok: false`, for endpoints whose failure detail is worth showing. The
    /// status comes with it.
    pub fn post_any(&self, path: &str, body: Value) -> Pending<Result<(u16, Value), String>> {
        let (server, path) = (self.server.clone(), path.to_string());
        Pending::spawn(self.ctx, move || http::post_json_any(&server, &path, &body))
    }

    /// `notification`: a toast and a line on the board, both marked with the
    /// plugin's name. `level` is "info", "warning" or "critical".
    pub fn notify(&mut self, level: &str, text: &str) {
        let level = match level {
            "warning" => "warning",
            "critical" => "critical",
            _ => "info",
        };
        self.out.notices.push((self.name.to_string(), level, text.to_string()));
    }

    /// `console`: a line in the MAVLink console. `level` is one of "", "cmd",
    /// "success", "warning", "error", "nav", "info".
    pub fn console(&mut self, text: &str, level: &str) {
        let level = if ["", "cmd", "success", "warning", "error", "nav", "info"].contains(&level) {
            level
        } else {
            ""
        };
        self.out.console.push((level.into(), text.to_string()));
    }

    /// `terminal`: show the live terminal of an SSH session this plugin
    /// opened, in a window of its own. `session` is `{name, title, host,
    /// port, username}`.
    /// `terminal`: show the live terminal of an SSH session this plugin
    /// opened, in a window of its own; `session` is `{name, title, host, port,
    /// username}`. `reattach` right after (re)connecting,
    /// so an open window takes the new shell instead of a dead stream, and
    /// `existing_only` to repair a window that is already open without
    /// opening one.
    pub fn terminal(&mut self, session: Value, reattach: bool, existing_only: bool) {
        self.out.terminals.push((session, reattach, existing_only));
    }

    /// `sshSetup`: asks the operator for what a failed connect or run said a
    /// saved connection lacks. The answer comes back to `ssh_setup_done` with
    /// the same `tag`.
    pub fn ssh_setup(&mut self, need: ssh::SshNeed, tag: u64) {
        self.out.ssh_setups.push((self.id, tag, need));
    }

    /// `fit`: bring the Home page forward and frame these points.
    pub fn fit(&mut self, coords: Vec<[f64; 2]>) {
        self.out.fit = Some(coords);
    }

    /// `pickPoint`: the next click on the map comes back as `MapEvent::Picked`;
    /// Escape or a right click as `PickCancelled`.
    pub fn pick_point(&mut self) {
        self.out.pick = Some(self.id);
        self.out.cancel_pick = false;
    }

    /// Drops a pick in progress without telling anyone.
    pub fn cancel_pick(&mut self) {
        if self.out.pick == Some(self.id) {
            self.out.pick = None;
        }
        self.out.cancel_pick = true;
    }
}

/// A number the way a browser prints it in an input.
pub fn number_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}
