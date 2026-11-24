//! The Plugins and About pills (`renderPluginsSection`, `renderAboutSection`
//! and `updatesCard` of `python/src/js/sidenav.js`).

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use super::{credits, model, Dialog, SettingsPage};
use crate::plugins::store::From;
use crate::plugins::Registry;
use crate::ui::form::{self, Form};
use crate::ui::kit::{Button, Variant};
use crate::ui::message::Tone;
use crate::ui::style::{self, Weight};

const PLUGIN_FOLDER_INFO: &str = "One folder per plugin, each with a plugin.json. After copying one in, changing one or deleting one, press Reload plugins.\nEach plugin keeps its settings in its own config.json in there (<id>/config.json), apart from the app's config. Copy that file to the same place on another computer to set it up the same way.\nThe folder's own README.md describes the format, and the plugins that ship with Corvus are worked examples.";

const UPDATES_INFO: &str = "Compares the running version against the published releases on GitHub and shows a notice when a newer one exists. Nothing is downloaded and nothing is sent about this machine beyond the request itself. The check never runs while the vehicle is armed, and with no internet it fails silently. Corvus never needs the network to fly.";

impl SettingsPage {
    /// The gear's dialog: a plugin's options, each in force and saved at once.
    pub(super) fn plugin_options(&mut self, ctx: &egui::Context, plugins: &mut Registry, state: &Value) {
        let Some(id) = self.plugin_dialog.clone() else {
            return;
        };
        match plugins.index_of(&id) {
            Some(i) => {
                if plugins.options_dialog(ctx, state, i) {
                    self.plugin_dialog = None;
                }
            }
            None => self.plugin_dialog = None,
        }
    }

    pub(super) fn plugins_tab(&mut self, ui: &mut egui::Ui, f: &mut Form, registry: &mut Registry) {
        registry.store.want(ui.ctx());
        let data = self.want(ui.ctx(), "plugins", "/api/plugins");
        let failed = data.is_none() && self.failed("plugins");
        let plugins: Vec<Value> = model::plugin_rows(
            data.as_ref()
                .and_then(|d| d.get("plugins"))
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            registry,
        );
        let dir = data
            .as_ref()
            .and_then(|d| d.get("user_dir"))
            .and_then(Value::as_str)
            .unwrap_or("~/.corvus/plugins")
            .to_string();
        let msg = self.msgs.get("plugins").cloned();
        let (reloading, opening) = (self.is_busy("plugins_reload"), self.is_busy("plugins_folder"));
        let mut hit = None;
        let mut gear: Option<String> = None;
        form::section(ui, f, "Plugins", |ui, f| {
            form::card(ui, f, "", |ui, f| {
                if failed {
                    form::desc(ui, f, "Plugin list unavailable.");
                } else if data.is_none() {
                    form::desc(ui, f, "Loading\u{2026}");
                } else if plugins.is_empty() {
                    form::desc(ui, f, "No plugins installed.");
                }
                for (i, p) in plugins.iter().enumerate() {
                    if plugin_row(ui, f, p, i == 0) {
                        gear = p.get("id").and_then(Value::as_str).map(str::to_string);
                    }
                }
                f.skip(18.0);
                form::field(ui, f, "Plugin folder", PLUGIN_FOLDER_INFO, "", |ui, x, y, w| {
                    let path = style::galley_mono(ui, &dir, 12.0, style::text_2());
                    let buttons = [
                        Button::new("Reload plugins", Variant::Secondary)
                            .icon("refresh-cw")
                            .enabled(!reloading),
                        Button::new("Open plugin folder", Variant::Secondary)
                            .icon("folder-open")
                            .enabled(!opening),
                    ];
                    let bw: f32 = buttons.iter().map(|b| b.width(ui)).sum::<f32>() + 8.0;
                    let bh = buttons[0].height();
                    let wrap = path.size().x + 14.0 + bw > w;
                    let (px, py) = (x, y + 2.0);
                    let g = kit_wrapped_path(ui, &dir, if wrap { w } else { w - bw - 14.0 });
                    let ph = g.size().y;
                    ui.painter().galley(
                        Pos2::new(px, py + if wrap { 0.0 } else { (bh - ph) / 2.0 }),
                        g,
                        style::text_2(),
                    );
                    let (mut bx, by) = if wrap { (x, py + ph + 8.0) } else { (x + w - bw, py) };
                    for (i, b) in buttons.into_iter().enumerate() {
                        let width = b.width(ui);
                        if b.show(
                            ui,
                            Rect::from_min_size(Pos2::new(bx, by), Vec2::new(width, bh)),
                            ("pluginbtn", i),
                        ) {
                            hit = Some(i);
                        }
                        bx += width + 8.0;
                    }
                    if wrap {
                        ph + 8.0 + bh + 2.0
                    } else {
                        bh.max(ph) + 2.0
                    }
                });
                if let Some((t, tone)) = &msg {
                    f.skip(12.0);
                    form::message(ui, f, t, *tone);
                }
            });
        });
        self.store_section(ui, f, registry);
        if gear.is_some() {
            self.plugin_dialog = gear;
        }
        match hit {
            Some(0) => {
                self.hush("plugins");
                self.act_get(ui.ctx(), "plugins_reload", "", "/api/plugins");
            }
            Some(1) => {
                self.hush("plugins");
                self.act(ui.ctx(), "plugins_folder", "", "/api/plugins/folder", json!({}));
            }
            _ => {}
        }
    }

    pub(super) fn plugins_done(&mut self, _ctx: &egui::Context, name: &'static str, answer: Result<Value, String>) {
        if name == "plugins_reload" {
            match answer {
                Ok(v) => {
                    let n = v.get("plugins").and_then(Value::as_array).map_or(0, Vec::len);
                    self.say(
                        "plugins",
                        format!("Reloaded. {n} plugin{} loaded.", if n == 1 { "" } else { "s" }),
                        Tone::Ok,
                    );
                    self.res.entry("plugins").or_default().value = Some(v);
                }
                Err(e) => self.say(
                    "plugins",
                    if e.is_empty() {
                        "Could not reload the plugins".to_string()
                    } else {
                        e
                    },
                    Tone::Err,
                ),
            }
            return;
        }
        match answer {
            Ok(v) if v.get("ok").and_then(Value::as_bool) == Some(true) => {
                let path = v.get("path").and_then(Value::as_str).unwrap_or("the plugin folder");
                self.say("plugins", format!("Opened {path}"), Tone::Ok);
            }
            Ok(v) => {
                let t = match v.get("path").and_then(Value::as_str) {
                    Some(p) => format!("Plugin folder: {p}"),
                    None => "Could not open the folder".to_string(),
                };
                self.say("plugins", t, Tone::Warn);
            }
            Err(e) => self.say(
                "plugins",
                if e.is_empty() {
                    "Could not open the folder".to_string()
                } else {
                    e
                },
                Tone::Err,
            ),
        }
    }

    pub(super) fn about(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let v = self.res.get("version").and_then(|s| s.value.clone());
        let failed = self.failed("version") && v.is_none();
        let mut credits_open = false;
        form::section(ui, f, "About", |ui, f| {
            form::card(ui, f, "", |ui, f| {
                let get = |k: &str| {
                    v.as_ref()
                        .and_then(|v| v.get(k))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                };
                if failed {
                    form::row(ui, f, "Version", "Unavailable", true);
                } else if v.is_some() {
                    let dash = || "\u{2014}".to_string();
                    form::row(
                        ui,
                        f,
                        "Product",
                        &get("product").unwrap_or_else(|| "Corvus GCS".into()),
                        false,
                    );
                    form::row(ui, f, "Version", &get("version").unwrap_or_else(dash), false);
                    form::row(ui, f, "PX4 Profile", &get("px4_profile").unwrap_or_else(dash), true);
                }
                f.skip(12.0);
                credits_open =
                    form::actions(ui, f, vec![Button::new("Credits", Variant::Secondary).icon("info")]).is_some();
            });
        });
        if credits_open {
            self.want(ui.ctx(), "sources", "/api/tiles/sources");
            self.dialog = Some(Dialog::Credits);
        }
        form::section(ui, f, "Updates", |ui, f| self.updates_card(ui, f));
    }

    fn updates_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let data = self.want(ui.ctx(), "update", "/api/update");
        let enabled = data
            .as_ref()
            .and_then(|d| d.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let busy = self.is_busy("update_check");
        let msg = self.msgs.get("update").cloned().or_else(|| {
            let d = data.as_ref()?;
            let latest = d.get("latest").and_then(Value::as_str).unwrap_or("");
            (enabled && d.get("update_available").and_then(Value::as_bool) == Some(true))
                .then(|| (format!("Version {latest} is available."), Tone::Ok))
                .or_else(|| {
                    self.failed("update")
                        .then(|| ("Update status unavailable.".to_string(), Tone::Warn))
                })
        });
        let mut toggled = None;
        let mut check = false;
        form::card(ui, f, "", |ui, f| {
            toggled = form::switch_field(
                ui,
                f,
                "updatecheck",
                form::Switch::new("Check for updates", "", enabled).info(UPDATES_INFO),
                style::text_1(),
                12.0,
            );
            if let Some(d) = data.as_ref() {
                f.skip(4.0);
                let s = |k: &str| {
                    d.get(k)
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                };
                let dash = || "\u{2014}".to_string();
                let available = d.get("update_available").and_then(Value::as_bool) == Some(true);
                let latest = s("latest");
                let mut rows = vec![("Installed", s("current").unwrap_or_else(dash))];
                if available {
                    rows.push(("Available", latest.clone().unwrap_or_else(dash)));
                } else if let Some(l) = latest {
                    rows.push(("Latest release", l));
                }
                let n = rows.len();
                for (i, (k, val)) in rows.iter().enumerate() {
                    form::row(ui, f, k, val, i + 1 == n);
                }
            }
            f.skip(12.0);
            check = form::actions(
                ui,
                f,
                vec![Button::new("Check now", Variant::Secondary)
                    .icon("refresh-cw")
                    .enabled(!busy)],
            )
            .is_some();
            if let Some((t, tone)) = &msg {
                f.skip(10.0);
                form::message(ui, f, t, *tone);
            }
        });
        if let Some(v) = toggled {
            self.write(ui.ctx(), json!({ "updates": { "check": v } }), true);
            if let Some(d) = self.res.get_mut("update").and_then(|s| s.value.as_mut()) {
                d["enabled"] = json!(v);
            }
        }
        if check {
            self.hush("update");
            self.act_get(ui.ctx(), "update_check", "", "/api/update?refresh=1");
        }
    }

    pub(super) fn update_done(&mut self, _ctx: &egui::Context, answer: Result<Value, String>) {
        match answer {
            Ok(v) => {
                let (t, tone) = model::update_verdict(&v);
                self.say("update", t, tone);
                self.res.entry("update").or_default().value = Some(v);
            }
            Err(_) => self.say("update", "Could not reach the release server.", Tone::Warn),
        }
    }

    pub(super) fn credits_dialog(&mut self, ctx: &egui::Context) -> bool {
        let version = self
            .res
            .get("version")
            .and_then(|s| s.value.as_ref())
            .and_then(|v| v.get("version"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let tiles = self.res.get("sources").and_then(|s| s.value.clone());
        let failed = self.failed("sources");
        let screen_h = ctx.content_rect().height();
        let mut close = false;
        let closed = crate::ui::modal::show(ctx, "credits", crate::ui::modal::Size::Lg, "Credits", |ui, x, y, w| {
            let h = (screen_h - 220.0).clamp(160.0, 560.0);
            let tiles_ref = if failed { Some(&Value::Null) } else { tiles.as_ref() };
            let bottom = credits::scroll(ui, x, y, w, h, |ui, left, top| {
                credits::draw(ui, left, top, w - 12.0, &version, tiles_ref)
            });
            let (hit, bottom) =
                crate::ui::modal::actions(ui, x, bottom + 16.0, w, vec![Button::new("Close", Variant::Secondary)]);
            close = hit.is_some();
            bottom
        });
        closed || close
    }
}

fn kit_wrapped_path(ui: &egui::Ui, text: &str, w: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat {
            font_id: style::mono(12.0),
            color: style::text_2(),
            ..Default::default()
        },
    );
    job.wrap.max_width = w;
    job.wrap.break_anywhere = true;
    ui.painter().layout_job(job)
}

/// One plugin: its name, where it came from, and what it does.
fn plugin_row(ui: &mut egui::Ui, f: &mut Form, p: &Value, first: bool) -> bool {
    let name = p
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .or_else(|| p.get("id").and_then(Value::as_str))
        .unwrap_or("");
    let top = f.y + if first { 2.0 } else { 14.0 };
    let n = style::galley(ui, name, 13.0, Weight::W500, style::text_1(), 0.0);
    let o = style::galley_mono(ui, &model::plugin_origin(p), 12.0, style::text_3());
    let head_h = 28.0;
    let gear = p.get("gear").and_then(Value::as_bool).unwrap_or(false);
    let right = f.x + f.w - if gear { 28.0 + 8.0 } else { 0.0 };
    ui.painter()
        .galley(Pos2::new(f.x, top + (head_h - n.size().y) / 2.0), n, style::text_1());
    ui.painter().galley(
        Pos2::new(right - o.size().x, top + (head_h - o.size().y) / 2.0),
        o,
        style::text_3(),
    );
    let mut pressed = false;
    if gear {
        let id = p.get("id").and_then(Value::as_str).unwrap_or("");
        let name = p.get("name").and_then(Value::as_str).unwrap_or(id);
        pressed = crate::ui::kit::icon_btn(
            ui,
            Rect::from_min_size(Pos2::new(f.x + f.w - 28.0, top), Vec2::splat(28.0)),
            "settings",
            &format!("Settings of {name}"),
            false,
            ("plugingear", id),
        );
    }
    let mut y = top + head_h;
    if let Some(d) = p.get("description").and_then(Value::as_str).filter(|d| !d.is_empty()) {
        let g = crate::ui::kit::wrapped_weight(ui, d, 12.0, Weight::W400, style::text_3(), f.w, 1.5);
        let h = g.size().y;
        ui.painter().galley(Pos2::new(f.x, y + 4.0), g, style::text_3());
        y += 4.0 + h;
    }
    y += 14.0;
    ui.painter().hline(
        egui::Rangef::new(f.x, f.x + f.w),
        y + 0.5,
        Stroke::new(1.0, style::border_soft()),
    );
    f.y = y + 1.0;
    pressed
}

const STORE_INFO: &str = "Install what you need. A plugin comes from its own repository on GitHub over HTTPS, from the plugin folder beside the program, or from a file, and is checked before it is installed. A new plugin starts at once; an update or a removal takes effect the next time Corvus GCS starts. Corvus GCS never needs the network to fly.";

const PLUGIN_FILE: &str = "Install a plugin from a file";

impl SettingsPage {
    /// The plugins on offer, each with its Install, Update or Remove button.
    fn store_section(&mut self, ui: &mut egui::Ui, f: &mut Form, registry: &mut Registry) {
        let rows = registry.store.rows();
        let running: Vec<Option<String>> = rows
            .iter()
            .map(|r| registry.package_of(&r.id).map(str::to_string))
            .collect();
        let working: Vec<bool> = rows.iter().map(|r| registry.store.working_on(&r.id)).collect();
        let refused: Vec<Option<String>> = rows
            .iter()
            .map(|r| registry.refused.iter().find(|x| x.id == r.id).map(|x| x.why.clone()))
            .collect();
        let missing = rows.iter().any(|r| r.offer.is_none() && r.installed.is_none());
        let updates: Vec<String> = rows
            .iter()
            .filter(|r| r.update_available())
            .map(|r| r.id.clone())
            .collect();
        let busy = registry.store.busy();
        let problem = registry.store.problem().map(str::to_string);
        let loading = registry.store.loading();
        let message = registry.store.message.clone();
        let mut act: Option<(bool, String)> = None;
        let (mut refresh, mut update_all, mut file) = (false, false, false);
        form::section(ui, f, "Available plugins", |ui, f| {
            form::card(ui, f, "", |ui, f| {
                form::desc(ui, f, STORE_INFO);
                f.skip(10.0);
                if let (Some(why), true) = (&problem, missing) {
                    form::message(
                        ui,
                        f,
                        &format!("Some plugins cannot be downloaded now: {why}."),
                        Tone::Warn,
                    );
                    f.skip(10.0);
                }
                for (i, row) in rows.iter().enumerate() {
                    let now = Now {
                        running: running[i].as_deref(),
                        refused: refused[i].as_deref(),
                        working: working[i],
                        looking: loading,
                        busy,
                    };
                    if let Some(install) = store_row(ui, f, row, &now, i == 0) {
                        act = Some((install, row.id.clone()));
                    }
                }
                f.skip(12.0);
                let mut x = f.x;
                let mut tall = 0.0_f32;
                let mut buttons = vec![(
                    "storerefresh",
                    Button::new("Refresh list", Variant::Secondary)
                        .icon("refresh-cw")
                        .enabled(!busy),
                )];
                buttons.push((
                    "storefile",
                    Button::new("Install from file", Variant::Secondary)
                        .icon("folder-open")
                        .enabled(!busy),
                ));
                if updates.len() > 1 {
                    buttons.push((
                        "storeupdate",
                        Button::new("Update all", Variant::Primary)
                            .icon("download")
                            .enabled(!busy),
                    ));
                }
                for (id, b) in buttons {
                    let (w, h) = (b.width(ui), b.height());
                    if b.show(ui, Rect::from_min_size(Pos2::new(x, f.y), Vec2::new(w, h)), id) {
                        refresh |= id == "storerefresh";
                        update_all |= id == "storeupdate";
                        file |= id == "storefile";
                    }
                    x += w + 8.0;
                    tall = tall.max(h);
                }
                f.skip(tall + 2.0);
                if let Some((t, ok)) = &message {
                    f.skip(12.0);
                    form::message(ui, f, t, if *ok { Tone::Ok } else { Tone::Warn });
                }
            });
        });
        let ctx = ui.ctx().clone();
        if refresh {
            registry.store.refresh(&ctx);
        }
        if file {
            let filter = crate::ui::filepicker::Filter {
                name: "Corvus plugin",
                extensions: &[corvus_package::library_extension()],
            };
            if let Some(path) = crate::ui::filepicker::pick_file(PLUGIN_FILE, Some(filter), None) {
                registry.store.install_file(path);
            }
        }
        match act {
            Some((true, id)) => registry.store.install(&id),
            Some((false, id)) => registry.store.remove(&id),
            None => {}
        }
        if update_all {
            for id in updates {
                registry.store.install(&id);
            }
        }
    }
}

/// What is going on with a plugin of the store.
struct Now<'a> {
    /// The checksum of the library it runs from.
    running: Option<&'a str>,
    /// Why the installed one did not load.
    refused: Option<&'a str>,
    working: bool,
    /// Where it can be had is still being looked up.
    looking: bool,
    busy: bool,
}

/// One plugin of the store. `Some(true)` when Install or Update was pressed, `Some(false)` for Remove.
fn store_row(
    ui: &mut egui::Ui,
    f: &mut Form,
    row: &crate::plugins::store::Row,
    now: &Now,
    first: bool,
) -> Option<bool> {
    let top = f.y + if first { 2.0 } else { 14.0 };
    let n = style::galley(ui, &row.name, 13.0, Weight::W500, style::text_1(), 0.0);
    let busy = now.busy;
    let state = match (&row.installed, now.running) {
        _ if now.working => "working\u{2026}".to_string(),
        (None, Some(_)) => "removed, runs until the next start".to_string(),
        (None, None) => match row.offer {
            Some((From::Beside, _)) => format!("{} \u{b7} on this computer", row.version),
            Some((From::Release, _)) => row.version.clone(),
            None if now.looking => "looking\u{2026}".to_string(),
            None => "not available".to_string(),
        },
        (Some(m), _) if row.update_available() && m.version != row.version => {
            format!("{} \u{b7} {} available", m.version, row.version)
        }
        (Some(m), _) if row.update_available() => format!("{} \u{b7} needs an update", m.version),
        (Some(m), Some(sha)) if sha != m.sha256 => format!("{} \u{b7} runs after the next start", m.version),
        (Some(m), Some(_)) => format!("{} \u{b7} installed", m.version),
        (Some(m), None) => format!("{} \u{b7} not loaded", m.version),
    };
    let o = style::galley_mono(ui, &crate::i18n::tr_joined(&state), 12.0, style::text_3());
    let primary = match &row.installed {
        None => Some("Install"),
        Some(_) if row.update_available() => Some("Update"),
        Some(_) => None,
    };
    let mut buttons = Vec::new();
    if let Some(label) = primary {
        buttons.push((
            true,
            Button::new(label, Variant::Primary).enabled(!busy && row.offer.is_some()),
        ));
    }
    if row.installed.is_some() {
        buttons.push((false, Button::new("Remove", Variant::Secondary).enabled(!busy)));
    }
    let head_h = buttons.first().map_or(28.0, |(_, b)| b.height()).max(28.0);
    let bw: f32 = buttons.iter().map(|(_, b)| b.width(ui) + 8.0).sum();
    ui.painter()
        .galley(Pos2::new(f.x, top + (head_h - n.size().y) / 2.0), n, style::text_1());
    ui.painter().galley(
        Pos2::new(f.x + f.w - bw - o.size().x - 8.0, top + (head_h - o.size().y) / 2.0),
        o,
        style::text_3(),
    );
    let mut pressed = None;
    let mut x = f.x + f.w - bw + 8.0;
    for (install, b) in buttons {
        let (w, h) = (b.width(ui), b.height());
        if b.show(
            ui,
            Rect::from_min_size(Pos2::new(x, top + (head_h - h) / 2.0), Vec2::new(w, h)),
            ("storebtn", &row.id, install),
        ) {
            pressed = Some(install);
        }
        x += w + 8.0;
    }
    let mut y = top + head_h;
    let why = row
        .installed
        .as_ref()
        .and(now.refused)
        .map(|w| format!("Not loaded: {w}."));
    for text in [Some(row.description.as_str()).filter(|d| !d.is_empty()), why.as_deref()]
        .into_iter()
        .flatten()
    {
        let g = crate::ui::kit::wrapped_weight(ui, text, 12.0, Weight::W400, style::text_3(), f.w, 1.5);
        let h = g.size().y;
        ui.painter().galley(Pos2::new(f.x, y + 4.0), g, style::text_3());
        y += 4.0 + h;
    }
    y += 14.0;
    ui.painter().hline(
        egui::Rangef::new(f.x, f.x + f.w),
        y + 0.5,
        Stroke::new(1.0, style::border_soft()),
    );
    f.y = y + 1.0;
    pressed
}
