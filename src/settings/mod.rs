//! The Settings page (`renderSettingsPage` of `python/src/js/sidenav.js`): nine
//! pills, and under each the cards that change one part of Corvus.
//!
//! Everything applies at once and is written in the background with
//! `POST /api/config`, as the web page writes it. A write that must not be
//! left half done (`strict`) is undone locally when the backend refuses it.
//! What the rest of the interface reads from the config (theme, size, units,
//! the Mission entry, the distance tool, the side panel's tabs) comes back to
//! the window as `Effects`.

mod about;
mod conn_cards;
mod credits;
mod data_cards;
mod dialogs;
mod map_cards;
pub mod model;
mod tabs;

use std::collections::HashMap;

use eframe::egui;
use serde_json::Value;

use crate::data::units::Units;
use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::ui::form::{self, Form, Tone};
use crate::ui::{pagekit, style};

use model::TABS;

/// What the window takes from the config as the page changes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Effects {
    pub theme: &'static str,
    pub scale: f32,
    pub units: Units,
    pub mission_page: bool,
    pub measure_tool: bool,
    pub console_tab: bool,
    pub ssh_tab: bool,
    pub base_layer: String,
    pub controls: crate::control::model::Config,
    pub flight_hud: bool,
    pub hud_square: bool,
}

impl Effects {
    /// The values a config names, with the web interface's defaults.
    pub fn of(cfg: &Value) -> Effects {
        Effects {
            theme: style::theme_from_config(cfg),
            scale: model::scale_from_config(cfg),
            units: model::units_in_force(cfg),
            mission_page: model::flag(cfg, "ui", "mission_page", false),
            measure_tool: model::flag(cfg, "ui", "measure_tool", true),
            console_tab: model::flag(cfg, "ui", "console_tab", true),
            ssh_tab: model::flag(cfg, "ui", "ssh_tab", true),
            base_layer: model::text(cfg, "map", "base_layer").to_string(),
            controls: crate::control::model::Config::from_config(cfg),
            flight_hud: model::flag(cfg, "ui", "flight_hud", true),
            hud_square: model::flag(cfg, "ui", "hud_square", false),
        }
    }
}

/// A request in flight and what to do with its answer.
enum Tag {
    /// `POST /api/config`: the patch that undoes it, and whether a refusal
    /// undoes it.
    Config(Value, bool),
    /// `GET`: fills the resource slot of this name.
    Get(&'static str),
    /// Anything else: the action's name and an argument for its handler.
    Act(&'static str, String),
}

struct Call {
    tag: Tag,
    answer: Pending<Result<Value, String>>,
}

/// One fetched resource.
#[derive(Default)]
struct Slot {
    value: Option<Value>,
    error: Option<String>,
    loading: bool,
    at: f64,
}

/// A dialog open over the page.
pub(crate) enum Dialog {
    Tokens,
    DemRename { id: String },
    DemRemove { id: String, name: String },
    DemFiles { into: Option<String> },
    Ssh { original: Option<String> },
    SshRemove { name: String },
    Notice { title: String, lines: Vec<String> },
    LogoFile,
    McpPerms,
    McpConnect { shown: bool },
    Credits,
    Export,
    Import,
    Reset,
}

/// The page's state.
pub struct SettingsPage {
    server: Server,
    cfg: Value,
    tab: usize,
    calls: Vec<Call>,
    res: HashMap<&'static str, Slot>,
    msgs: HashMap<&'static str, (String, Tone)>,
    texts: HashMap<&'static str, String>,
    choice: HashMap<&'static str, String>,
    ticks: HashMap<String, bool>,
    busy: HashMap<&'static str, bool>,
    dialog: Option<Dialog>,
    effects: Effects,
    effects_due: bool,
    errors: Vec<String>,
    scroll_top: bool,
    entered: bool,
    sources_dirty: bool,
    show_on_map: Option<(f64, f64, f64)>,
    ssh_request: Option<Value>,
    dem_progress: std::sync::Arc<std::sync::Mutex<String>>,
    logo: Option<Vec<u8>>,
    logo_load: Option<Pending<Option<Vec<u8>>>>,
    logo_rev: u32,
    import_bundle: Option<Value>,
    notes: Vec<String>,
}

impl SettingsPage {
    pub fn new(server: Server, config: &Value) -> SettingsPage {
        SettingsPage {
            server,
            cfg: config.clone(),
            tab: 0,
            calls: Vec::new(),
            res: HashMap::new(),
            msgs: HashMap::new(),
            texts: HashMap::new(),
            choice: HashMap::new(),
            ticks: HashMap::new(),
            busy: HashMap::new(),
            dialog: None,
            effects: Effects::of(config),
            effects_due: false,
            errors: Vec::new(),
            scroll_top: false,
            entered: false,
            sources_dirty: false,
            show_on_map: None,
            ssh_request: None,
            dem_progress: Default::default(),
            logo: None,
            logo_load: None,
            logo_rev: 0,
            import_bundle: None,
            notes: Vec::new(),
        }
    }

    /// The page was opened: the config and everything the cards list is read
    /// again, as the web page reads them each time it renders.
    pub fn enter(&mut self, ctx: &egui::Context) {
        self.entered = true;
        self.res.clear();
        self.msgs.clear();
        self.get(ctx, "config", "/api/config");
        self.get(ctx, "version", "/api/version");
    }

    /// Opens on a pill by its id (the `--page settings:map` flag).
    pub fn open_tab(&mut self, id: &str) {
        if let Some(i) = TABS.iter().position(|t| t.0 == id) {
            self.tab = i;
        }
    }

    /// Errors for the notification board.
    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    /// Notices for the notification board.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// The new values for the window, once, when the config changed them.
    pub fn take_effects(&mut self) -> Option<Effects> {
        std::mem::take(&mut self.effects_due).then(|| self.effects.clone())
    }

    /// Set once when a map service key or an elevation model changed what the
    /// map can show; the window reads the catalogue again.
    pub fn take_sources_dirty(&mut self) -> bool {
        std::mem::take(&mut self.sources_dirty)
    }

    /// A place to frame on the Home map (longitude, latitude, zoom), once.
    pub fn take_show_on_map(&mut self) -> Option<(f64, f64, f64)> {
        self.show_on_map.take()
    }

    /// A saved SSH connection to open in the side panel, once.
    pub fn take_ssh_request(&mut self) -> Option<Value> {
        self.ssh_request.take()
    }

    /// Whether a newer release is waiting, for the rail's SET dot.
    pub fn update_available(&self) -> bool {
        self.res.get("update").and_then(|s| s.value.as_ref()).is_some_and(|u| {
            u.get("enabled").and_then(Value::as_bool) != Some(false)
                && u.get("update_available").and_then(Value::as_bool) == Some(true)
        })
    }

    // ---- requests ----

    fn get(&mut self, ctx: &egui::Context, name: &'static str, path: &str) {
        let slot = self.res.entry(name).or_default();
        if slot.loading {
            return;
        }
        slot.loading = true;
        slot.at = ctx.input(|i| i.time);
        let server = self.server.clone();
        let path = path.to_string();
        self.calls.push(Call {
            tag: Tag::Get(name),
            answer: Pending::spawn(ctx, move || http::get_json(&server, &path)),
        });
    }

    /// Fetches `name` once; later frames find it in the slot.
    fn want(&mut self, ctx: &egui::Context, name: &'static str, path: &str) -> Option<Value> {
        if !self.res.contains_key(name) {
            self.get(ctx, name, path);
        }
        self.res.get(name).and_then(|s| s.value.clone())
    }

    /// Like `want`, and fetched again when the last answer is older than `secs`.
    fn live(&mut self, ctx: &egui::Context, name: &'static str, path: &str, secs: f64) -> Option<Value> {
        let now = ctx.input(|i| i.time);
        if self.res.get(name).is_some_and(|s| !s.loading && now - s.at > secs) {
            self.get(ctx, name, path);
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(secs));
        self.want(ctx, name, path)
    }

    fn failed(&self, name: &'static str) -> bool {
        self.res.get(name).is_some_and(|s| s.error.is_some())
    }

    fn act(&mut self, ctx: &egui::Context, name: &'static str, arg: &str, path: &str, body: Value) {
        let server = self.server.clone();
        let path = path.to_string();
        self.busy.insert(name, true);
        self.calls.push(Call {
            tag: Tag::Act(name, arg.to_string()),
            answer: Pending::spawn(ctx, move || http::post_json(&server, &path, &body)),
        });
    }

    /// A GET whose answer is handled like an action's.
    fn act_get(&mut self, ctx: &egui::Context, name: &'static str, arg: &str, path: &str) {
        let server = self.server.clone();
        let path = path.to_string();
        self.busy.insert(name, true);
        self.calls.push(Call {
            tag: Tag::Act(name, arg.to_string()),
            answer: Pending::spawn(ctx, move || http::get_json(&server, &path)),
        });
    }

    fn act_bytes(&mut self, ctx: &egui::Context, name: &'static str, arg: &str, path: &str, bytes: Vec<u8>) {
        let server = self.server.clone();
        let path = path.to_string();
        self.busy.insert(name, true);
        self.calls.push(Call {
            tag: Tag::Act(name, arg.to_string()),
            answer: Pending::spawn(ctx, move || {
                http::post_bytes(&server, &path, &bytes, std::time::Duration::from_secs(600))
            }),
        });
    }

    fn is_busy(&self, name: &'static str) -> bool {
        self.busy.get(name).copied().unwrap_or(false)
    }

    /// Writes a patch to the config: in force here at once, posted in the
    /// background. `strict` takes it back when the backend refuses.
    fn write(&mut self, ctx: &egui::Context, patch: Value, strict: bool) {
        let undo = model::merge(&mut self.cfg, &patch);
        self.sync_effects();
        self.post(ctx, patch, undo, strict);
    }

    /// Only the post, for a value already in force here (a slider's release).
    fn post(&mut self, ctx: &egui::Context, patch: Value, undo: Value, strict: bool) {
        let server = self.server.clone();
        self.calls.push(Call {
            tag: Tag::Config(undo, strict),
            answer: Pending::spawn(ctx, move || http::post_json(&server, "/api/config", &patch)),
        });
    }

    fn sync_effects(&mut self) {
        let now = Effects::of(&self.cfg);
        if now != self.effects {
            self.effects = now;
            self.effects_due = true;
        }
    }

    fn say(&mut self, card: &'static str, text: impl Into<String>, tone: Tone) {
        self.msgs.insert(card, (text.into(), tone));
    }

    fn hush(&mut self, card: &'static str) {
        self.msgs.remove(card);
    }

    fn text(&mut self, key: &'static str) -> &mut String {
        self.texts.entry(key).or_default()
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.calls.len() {
            let Some(answer) = self.calls[i].answer.take() else {
                i += 1;
                continue;
            };
            let call = self.calls.remove(i);
            match call.tag {
                Tag::Config(undo, strict) => {
                    if let Err(e) = answer {
                        if strict {
                            model::merge(&mut self.cfg, &undo);
                            self.sync_effects();
                        }
                        self.errors.push(format!("Could not save the setting: {e}"));
                    }
                }
                Tag::Get(name) => {
                    let slot = self.res.entry(name).or_default();
                    slot.loading = false;
                    match answer {
                        Ok(v) => {
                            slot.error = None;
                            if name == "config" && self.calls.iter().all(|c| !matches!(c.tag, Tag::Config(..))) {
                                self.cfg = crate::app::unwrap_config(&v);
                                self.sync_effects();
                            }
                            self.res.get_mut(name).expect("slot").value = Some(v);
                        }
                        Err(e) => slot.error = Some(e),
                    }
                }
                Tag::Act(name, arg) => {
                    self.busy.insert(name, false);
                    self.done(ctx, name, &arg, answer);
                }
            }
        }
    }

    // ---- drawing ----

    /// Draws the page into `rect`.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if !self.entered {
            self.enter(ui.ctx());
        }
        self.poll(ui.ctx());
        ui.painter().rect_filled(rect, 0, style::bg());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.set_clip_rect(rect);
        let mut area = egui::ScrollArea::vertical()
            .id_salt("settingsscroll")
            .auto_shrink([false, false]);
        if std::mem::take(&mut self.scroll_top) {
            area = area.vertical_scroll_offset(0.0);
        }
        area.show(&mut child, |ui| {
            let x = rect.left() + pagekit::PAD_X;
            let w = rect.width() - 2.0 * pagekit::PAD_X;
            let top = ui.max_rect().top();
            let mut f = Form::new(x, top + pagekit::PAD_Y, w);
            f.y = pagekit::header(ui, f.x, f.y, "Settings", "Application configuration") + 28.0;
            self.body(ui, &mut f);
            ui.allocate_space(egui::vec2(rect.width() - 1.0, f.y + pagekit::PAD_Y - top));
        });
        self.dialogs(ui.ctx());
        if !self.calls.is_empty() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn body(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        if self.failed("config") && self.res.get("config").is_some_and(|s| s.value.is_none()) {
            form::card(ui, f, "", |ui, f| {
                form::desc(
                    ui,
                    f,
                    "Settings unavailable. Could not load configuration from the backend.",
                );
            });
            self.about(ui, f);
            return;
        }
        // The pills carry no label strings the cards read back: a pill is its
        // index, and the update dot sits on About.
        let items: Vec<(&str, &'static str)> = TABS.iter().map(|t| (t.1, t.2)).collect();
        let dots: Vec<bool> = TABS.iter().map(|t| t.0 == "about" && self.update_available()).collect();
        f.y -= 12.0;
        if let Some(i) = form::pills(ui, f, &items, self.tab, &dots) {
            if i != self.tab {
                self.tab = i;
                self.scroll_top = true;
            }
        }
        f.y += 24.0;
        match TABS[self.tab].0 {
            "interface" => self.interface_tab(ui, f),
            "layout" => self.layout_tab(ui, f),
            "flight" => self.flight_tab(ui, f),
            "map" => self.map_tab(ui, f),
            "controls" => self.controls_tab(ui, f),
            "connections" => self.connections_tab(ui, f),
            "data" => self.data_tab(ui, f),
            "plugins" => self.plugins_tab(ui, f),
            _ => self.about(ui, f),
        }
    }

    /// Answers of the actions that are not config writes.
    fn done(&mut self, ctx: &egui::Context, name: &'static str, arg: &str, answer: Result<Value, String>) {
        match name {
            "tokens" | "token_remove" => self.token_done(ctx, name, arg, answer),
            "dem_import" | "dem_rename" | "dem_remove" => self.dem_done(ctx, name, arg, answer),
            "logo" | "logo_remove" => self.logo_done(ctx, name, answer),
            "ssh_connect" | "ssh_remove" => self.ssh_done(ctx, name, arg, answer),
            "fleet_add" | "fleet_remove" => self.fleet_done(ctx, name, answer),
            "mcp_save" | "mcp_token" => self.mcp_done(ctx, name, answer),
            "cache_clear" => self.cache_done(ctx, answer),
            "update_check" => self.update_done(ctx, answer),
            "plugins_folder" | "plugins_reload" => self.plugins_done(ctx, name, answer),
            "export" | "import" | "reset" => self.transfer_done(ctx, name, answer),
            _ => {}
        }
    }

    /// A refused write shown where the card is, not on the board.
    fn fail(&mut self, card: &'static str, text: &str) {
        self.say(card, text, Tone::Err);
    }
}

/// An error text for an answer that came back refused.
fn refused(answer: &Result<Value, String>, fallback: &str) -> Option<String> {
    match answer {
        Err(e) => Some(if e.is_empty() { fallback.into() } else { e.clone() }),
        Ok(v) if v.get("ok").and_then(Value::as_bool) == Some(false) => {
            Some(v.get("error").and_then(Value::as_str).unwrap_or(fallback).to_string())
        }
        Ok(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_window_takes_what_the_config_names() {
        let e = Effects::of(&json!({
            "theme": {"name": "blue"},
            "ui": {"scale": 1.25, "mission_page": true, "measure_tool": false, "ssh_tab": false,
                   "units": {"length": "ft"}}
        }));
        assert_eq!(e.theme, "blue");
        assert_eq!(e.scale, 1.25);
        assert!(e.mission_page && !e.measure_tool && e.console_tab && !e.ssh_tab);
        assert_eq!(e.units.length.id, "ft");
        let d = Effects::of(&json!({}));
        assert_eq!((d.theme, d.scale), ("light", 1.0));
        assert!(!d.mission_page && d.measure_tool && d.console_tab && d.ssh_tab);
    }

    #[test]
    fn a_refused_answer_is_told_in_the_backends_words() {
        assert_eq!(refused(&Err("boom".into()), "x").as_deref(), Some("boom"));
        assert_eq!(
            refused(&Ok(json!({"ok": false, "error": "Bad key"})), "x").as_deref(),
            Some("Bad key")
        );
        assert_eq!(
            refused(&Ok(json!({"ok": false})), "Could not save the key.").as_deref(),
            Some("Could not save the key.")
        );
        assert!(refused(&Ok(json!({"ok": true})), "x").is_none());
    }

    #[test]
    fn a_page_writes_locally_and_takes_a_refusal_back() {
        let ctx = egui::Context::default();
        let mut page = SettingsPage::new(Server::local(1), &json!({"ui": {"scale": 1.0}}));
        page.write(&ctx, model::patch("ui", "scale", json!(1.25)), true);
        assert_eq!(page.cfg["ui"]["scale"], 1.25);
        assert_eq!(page.take_effects().map(|e| e.scale), Some(1.25));
        assert!(page.take_effects().is_none());
        for _ in 0..100 {
            page.poll(&ctx);
            if page.calls.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(page.cfg["ui"]["scale"], 1.0);
        assert_eq!(page.take_effects().map(|e| e.scale), Some(1.0));
        assert_eq!(page.take_errors().len(), 1);
    }
}
