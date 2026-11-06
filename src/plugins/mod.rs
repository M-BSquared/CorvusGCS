//! The plugin system of the native interface.
//!
//! A plugin is a Rust type behind the [`Plugin`] trait, compiled into the
//! program and listed in [`builtin`]; there is no loader, so a plugin works
//! the same on every platform the program is built for. It is the native
//! counterpart of `Corvus.plugins.register` (`python/src/js/plugins.js`): a
//! card under PLUGINS (or a tab of its own), saved settings, options in
//! Settings, shapes on the Home map, notifications, the console, a terminal
//! over a saved SSH connection and calls to the backend. How to write one is
//! in `src/README.md`.
//!
//! The [`Registry`] owns the plugins and everything the operator decides
//! about them: their options, which of them have a tab, what they saved. It
//! runs each plugin's `start` once the saved settings have come from
//! `GET /api/plugins`, calls `tick` on every frame whether or not the plugin
//! is open (what a telemetry subscription is in the browser), and gives the
//! app what the plugins asked for through the [`Outbox`].

pub mod layer;
pub mod map_entities;
pub mod options;
pub mod schwalby;
pub mod shapes;
pub mod ssh_launcher;
mod ssh_setup;
pub mod vibration;
mod view;

use eframe::egui;
use serde_json::{json, Map, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use options::{OptionSpec, OPTIONS_KEY, TAB_OPTION};
use shapes::{Shape, ShapeSet};

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

/// The compiled-in plugins, in the order of their `order`.
pub fn builtin() -> Vec<Box<dyn Plugin>> {
    let mut all: Vec<Box<dyn Plugin>> = vec![
        Box::new(vibration::Vibration::default()),
        Box::new(ssh_launcher::SshLauncher::default()),
        Box::new(map_entities::MapEntities::default()),
        Box::new(schwalby::Schwalby::default()),
    ];
    all.sort_by(|a, b| {
        let (a, b) = (a.info(), b.info());
        (a.order, a.name.to_lowercase()).cmp(&(b.order, b.name.to_lowercase()))
    });
    all
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
    pub ssh_setups: Vec<(&'static str, u64, ssh_launcher::model::SshNeed)>,
    /// Frame these `[lng, lat]` on the Home map.
    pub fit: Option<Vec<[f64; 2]>>,
    /// Who waits for a click on the map.
    pub pick: Option<&'static str>,
    pub cancel_pick: bool,
}

/// A save of one option still on its way: what to put back if it fails.
struct Saving {
    entry: usize,
    before: Map<String, Value>,
    answer: Reply,
}

struct Entry {
    plugin: Box<dyn Plugin>,
    info: Info,
    specs: Vec<OptionSpec>,
    shapes: ShapeSet,
    /// Everything saved: the plugin's own state and `_options`.
    saved: Value,
    started: bool,
    /// Whether `open` has run and `close` has not.
    live: bool,
}

/// The registry.
pub struct Registry {
    server: Server,
    entries: Vec<Entry>,
    load: Option<Reply>,
    loaded: bool,
    tabs_allowed: bool,
    /// The last `GET /api/plugins`, which lists what the backend knows of.
    pub backend: Option<Value>,
    out: Outbox,
    saving: Vec<Saving>,
    option_error: Option<String>,
    setups: Vec<ssh_setup::SshSetup>,
    /// The card opened under PLUGINS.
    open: Option<usize>,
    shapes_dirty: bool,
    /// Calls from an AI client that wait on a plugin: `(token, entry, call id)`.
    mcp_waiting: Vec<(u64, usize, String)>,
    mcp_failed: Vec<(String, String)>,
    mcp_next: u64,
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
    pub fn ssh_setup(&mut self, need: ssh_launcher::model::SshNeed, tag: u64) {
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

impl Registry {
    pub fn new(server: Server, ctx: &egui::Context, config: &Value) -> Registry {
        let entries = builtin()
            .into_iter()
            .map(|plugin| {
                let info = plugin.info();
                Entry {
                    specs: options::clean(plugin.options()),
                    shapes: ShapeSet::new(info.id),
                    saved: json!({}),
                    started: false,
                    live: false,
                    info,
                    plugin,
                }
            })
            .collect();
        let tabs_allowed = config
            .get("ui")
            .and_then(|u| u.get("plugin_tabs"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let s = server.clone();
        Registry {
            load: Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/plugins"))),
            server,
            entries,
            loaded: false,
            tabs_allowed,
            backend: None,
            out: Outbox::default(),
            saving: Vec::new(),
            option_error: None,
            setups: Vec::new(),
            open: None,
            shapes_dirty: true,
            mcp_waiting: Vec::new(),
            mcp_failed: Vec::new(),
            mcp_next: 0,
        }
    }

    /// The saved settings that came with `GET /api/plugins`, or `{}`.
    fn adopt(&mut self, data: &Value) {
        if let Some(all) = data.get("settings").and_then(Value::as_object) {
            for e in &mut self.entries {
                if let Some(s) = all.get(e.info.id).filter(|s| s.is_object()) {
                    e.saved = s.clone();
                }
            }
        }
    }

    fn values_of(e: &Entry, tabs_allowed: bool) -> Map<String, Value> {
        options::values(&e.specs, &e.saved, e.info.can_tab, tabs_allowed)
    }

    /// Whether plugin `i` has a tab of its own right now.
    pub fn in_tab(&self, i: usize) -> bool {
        let e = &self.entries[i];
        e.info.can_tab && Self::values_of(e, self.tabs_allowed).get(TAB_OPTION) == Some(&json!(true))
    }

    /// The plugins that have a tab, in order.
    pub fn tabbed(&self) -> Vec<usize> {
        (0..self.entries.len()).filter(|&i| self.in_tab(i)).collect()
    }

    pub fn info(&self, i: usize) -> Info {
        self.entries[i].info
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.info.id == id)
    }

    /// Runs `f` with plugin `i` and a host for it.
    fn with<R>(
        &mut self,
        i: usize,
        ctx: &egui::Context,
        state: &Value,
        f: impl FnOnce(&mut dyn Plugin, &mut Host) -> R,
    ) -> R {
        let Registry {
            server,
            entries,
            out,
            tabs_allowed,
            ..
        } = self;
        let e = &mut entries[i];
        let opts = Self::values_of(e, *tabs_allowed);
        let mut host = Host {
            ctx,
            server,
            state,
            now: now_secs(),
            id: e.info.id,
            name: e.info.name,
            shapes: &mut e.shapes,
            saved: &mut e.saved,
            opts: &opts,
            out,
        };
        f(e.plugin.as_mut(), &mut host)
    }

    /// One frame: takes in the answers, starts the plugins that have not been
    /// started, ticks them all.
    pub fn update(&mut self, ctx: &egui::Context, state: &Value) {
        if let Some(r) = self.load.as_ref().and_then(Pending::take) {
            self.load = None;
            if let Ok(data) = &r {
                self.adopt(data);
                self.backend = Some(data.clone());
            }
            self.loaded = true;
        }
        self.poll_saves();
        if !self.loaded {
            return;
        }
        for i in 0..self.entries.len() {
            if !self.entries[i].started {
                self.entries[i].started = true;
                self.with(i, ctx, state, |p, h| p.start(h));
            }
            self.with(i, ctx, state, |p, h| p.tick(h));
        }
        self.run_setups(ctx, state);
        if self.entries.iter_mut().fold(false, |a, e| e.shapes.take_changed() | a) {
            self.shapes_dirty = true;
        }
    }

    // ---- tools for an AI client (`Corvus.mcp`) ----------------------------

    /// Every plugin's tools, as the MCP window announces them.
    pub fn mcp_tools(&mut self, ctx: &egui::Context, state: &Value) -> Vec<Value> {
        let mut out = Vec::new();
        if !self.loaded {
            return out;
        }
        for i in 0..self.entries.len() {
            let info = self.entries[i].info;
            for t in self.with(i, ctx, state, |p, h| p.mcp_tools(h)) {
                out.push(json!({
                    "plugin": info.id, "plugin_name": info.name, "name": t.name, "title": t.title,
                    "description": t.description, "input_schema": t.input_schema,
                    "confirm": t.confirm, "read_only": t.read_only,
                }));
            }
        }
        out
    }

    /// An AI client's call (`runMcpTool`); its answer comes from `mcp_answers`.
    pub fn mcp_call(
        &mut self,
        call_id: &str,
        plugin: &str,
        tool: &str,
        args: &Value,
        ctx: &egui::Context,
        state: &Value,
    ) {
        let Some(i) = self.index_of(plugin) else {
            self.mcp_failed.push((
                call_id.to_string(),
                format!("The plugin offers no tool {tool} right now."),
            ));
            return;
        };
        self.mcp_next += 1;
        let token = self.mcp_next;
        self.with(i, ctx, state, |p, h| p.mcp_call(tool, args, token, h));
        self.mcp_waiting.push((token, i, call_id.to_string()));
    }

    /// The calls that have an answer now: `(call id, result)`.
    pub fn mcp_answers(&mut self, ctx: &egui::Context, state: &Value) -> Vec<(String, Result<Value, String>)> {
        let mut out: Vec<(String, Result<Value, String>)> = std::mem::take(&mut self.mcp_failed)
            .into_iter()
            .map(|(id, e)| (id, Err(e)))
            .collect();
        let waiting = std::mem::take(&mut self.mcp_waiting);
        let mut keep = Vec::new();
        for (token, i, id) in waiting {
            match self.with(i, ctx, state, |p, _| p.mcp_take(token)) {
                Some(r) => out.push((id, r)),
                None => keep.push((token, i, id)),
            }
        }
        self.mcp_waiting = keep;
        out
    }

    /// The login dialogs plugins asked for, one per connection name.
    fn run_setups(&mut self, ctx: &egui::Context, state: &Value) {
        for (owner, tag, need) in std::mem::take(&mut self.out.ssh_setups) {
            let asker = ssh_setup::Asker { owner, tag };
            match self.setups.iter_mut().find(|s| s.need.name == need.name) {
                Some(s) => s.askers.push(asker),
                None => self
                    .setups
                    .push(ssh_setup::SshSetup::new(ctx, &self.server, need, asker)),
            }
        }
        let server = self.server.clone();
        let mut done: Vec<(usize, bool)> = Vec::new();
        for (i, s) in self.setups.iter_mut().enumerate() {
            match s.show(ctx, &server) {
                ssh_setup::Outcome::Open => {}
                ssh_setup::Outcome::Saved => done.push((i, true)),
                ssh_setup::Outcome::Cancelled => done.push((i, false)),
            }
        }
        for (i, saved) in done.into_iter().rev() {
            let s = self.setups.remove(i);
            for a in s.askers {
                if let Some(k) = self.index_of(a.owner) {
                    self.with(k, ctx, state, |p, h| p.ssh_setup_done(a.tag, saved, h));
                }
            }
        }
    }

    /// Every plugin's shapes, when they changed since the last call.
    pub fn take_shapes(&mut self) -> Option<Vec<Shape>> {
        std::mem::take(&mut self.shapes_dirty).then(|| {
            self.entries
                .iter()
                .flat_map(|e| e.shapes.shapes().iter().cloned())
                .collect()
        })
    }

    pub fn take_out(&mut self) -> Outbox {
        std::mem::take(&mut self.out)
    }

    /// Hands a map event to the plugin that owns the shape (or the pick).
    pub fn dispatch(&mut self, ctx: &egui::Context, state: &Value, owner: &str, event: MapEvent) {
        if let Some(i) = self.index_of(owner) {
            self.with(i, ctx, state, |p, h| p.map_event(event, h));
        }
    }

    // ---- views --------------------------------------------------------------

    /// Brings plugin `i`'s view up if it is not.
    pub(crate) fn ensure_live(&mut self, i: usize, ctx: &egui::Context, state: &Value) {
        if !self.entries[i].live {
            self.entries[i].live = true;
            self.with(i, ctx, state, |p, h| p.open(h));
        }
    }

    pub(crate) fn end_live(&mut self, i: usize, ctx: &egui::Context, state: &Value) {
        if self.entries[i].live {
            self.entries[i].live = false;
            self.with(i, ctx, state, |p, h| p.close(h));
        }
    }

    /// Opens the card of plugin `i` under PLUGINS, closing the one open.
    pub(crate) fn open_card(&mut self, i: usize, ctx: &egui::Context, state: &Value) {
        if self.open == Some(i) {
            return;
        }
        self.close_card(ctx, state);
        self.open = Some(i);
        self.ensure_live(i, ctx, state);
    }

    pub(crate) fn close_card(&mut self, ctx: &egui::Context, state: &Value) {
        if let Some(i) = self.open.take() {
            self.end_live(i, ctx, state);
        }
    }

    /// A plugin that moved into a tab leaves the card; one that left its tab
    /// is closed with it (`syncTabs`).
    fn sync_tabs(&mut self) {
        for i in 0..self.entries.len() {
            if self.in_tab(i) && self.open == Some(i) {
                self.open = None;
                self.entries[i].live = false;
            }
        }
    }

    // ---- options --------------------------------------------------------------

    /// The schema the dialog shows (the tab switch first) and the values.
    pub fn option_form(&self, i: usize) -> (Vec<OptionSpec>, Map<String, Value>) {
        let e = &self.entries[i];
        let mut schema = e.specs.clone();
        if e.info.can_tab {
            schema.insert(0, options::tab_spec());
        }
        (schema, Self::values_of(e, self.tabs_allowed))
    }

    /// Whether the gear shows: the plugin has options or can have a tab.
    pub fn has_options(&self, i: usize) -> bool {
        let e = &self.entries[i];
        e.info.can_tab || !e.specs.is_empty()
    }

    /// `setOption`: in force at once, saved through `/api/plugins/settings`,
    /// taken back by [`Registry::poll_saves`] if the save fails.
    pub fn set_option(
        &mut self,
        ctx: &egui::Context,
        state: &Value,
        i: usize,
        key: &str,
        value: &Value,
    ) -> Result<(), String> {
        let spec = {
            let e = &self.entries[i];
            if key == TAB_OPTION && e.info.can_tab {
                options::tab_spec()
            } else {
                e.specs
                    .iter()
                    .find(|o| o.key == key)
                    .cloned()
                    .ok_or(format!("No option {key}."))?
            }
        };
        let next = options::coerce(&spec, value).ok_or(format!("Not a valid value for {}.", spec.label))?;
        let before = options::saved(&self.entries[i].saved);
        let mut after = before.clone();
        after.insert(key.to_string(), next);
        self.apply_options(i, after.clone(), key, ctx, state);
        let server = self.server.clone();
        let body = json!({"id": self.entries[i].info.id, "settings": {OPTIONS_KEY: after}});
        self.saving.push(Saving {
            entry: i,
            before,
            answer: Pending::spawn(ctx, move || http::post_json(&server, "/api/plugins/settings", &body)),
        });
        Ok(())
    }

    fn apply_options(&mut self, i: usize, opts: Map<String, Value>, key: &str, ctx: &egui::Context, state: &Value) {
        if let Some(o) = self.entries[i].saved.as_object_mut() {
            o.insert(OPTIONS_KEY.into(), Value::Object(opts));
        } else {
            self.entries[i].saved = json!({ OPTIONS_KEY: opts });
        }
        if key == TAB_OPTION {
            if self.in_tab(i) {
                self.sync_tabs();
            } else {
                self.end_live(i, ctx, state);
            }
        } else {
            self.with(i, ctx, state, |p, h| p.options_changed(h));
        }
        self.shapes_dirty = true;
    }

    /// Takes in the answers to option saves; a refused one is put back.
    pub fn poll_saves(&mut self) {
        let mut undo = Vec::new();
        self.saving.retain(|s| match s.answer.take() {
            None => true,
            Some(Ok(r)) if r.get("ok") != Some(&json!(false)) && r.get("error").is_none() => false,
            Some(Ok(r)) => {
                let e = r
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Could not save the setting.");
                undo.push((s.entry, s.before.clone(), e.to_string()));
                false
            }
            Some(Err(e)) => {
                let e = if e.is_empty() {
                    "Could not save the setting.".into()
                } else {
                    e
                };
                undo.push((s.entry, s.before.clone(), e));
                false
            }
        });
        for (i, before, msg) in undo {
            if let Some(o) = self.entries[i].saved.as_object_mut() {
                o.insert(OPTIONS_KEY.into(), Value::Object(before));
            }
            self.option_error = Some(msg);
            self.shapes_dirty = true;
        }
    }

    /// Ends every plugin's view.
    pub fn shut_down(&mut self, ctx: &egui::Context, state: &Value) {
        for i in 0..self.entries.len() {
            self.end_live(i, ctx, state);
        }
        self.open = None;
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_bundled_plugins_are_listed_in_the_order_of_their_manifests() {
        let ids: Vec<_> = builtin().iter().map(|p| p.info().id).collect();
        assert_eq!(ids, ["vibration", "ssh-launcher", "schwalby", "map-entities"]);
    }

    #[test]
    fn schwalby_says_what_its_manifest_says() {
        let manifest: Value = serde_json::from_str(include_str!("../../python/plugins/schwalby/plugin.json")).unwrap();
        let info = schwalby::Schwalby::default().info();
        assert_eq!(manifest["id"], info.id);
        assert_eq!(manifest["name"], info.name);
        assert_eq!(manifest["icon"], info.icon);
        assert_eq!(manifest["description"], info.description);
        assert_eq!(manifest["version"], info.version);
        assert_eq!(manifest["order"], info.order);
    }

    #[test]
    fn plugin_ids_are_safe_folder_names() {
        for p in builtin() {
            let id = p.info().id;
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'),
                "{id}"
            );
            assert!(!id.starts_with('.'));
        }
    }
}
