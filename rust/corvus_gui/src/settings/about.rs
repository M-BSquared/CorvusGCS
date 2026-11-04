//! The Plugins and About pills (`renderPluginsSection`, `renderAboutSection`
//! and `updatesCard` of `src/js/sidenav.js`).

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use super::{credits, model, Dialog, SettingsPage};
use crate::ui::form::{self, Form, Tone};
use crate::ui::kit::{Button, Variant};
use crate::ui::style::{self, Weight};

const PLUGIN_FOLDER_INFO: &str = "One folder per plugin, each with a plugin.json. After copying one in, changing one or deleting one, press Reload plugins.\nEach plugin keeps its settings in its own config.json in there (<id>/config.json), apart from the app's config. Copy that file to the same place on another computer to set it up the same way.\nThe folder's own README.md describes the format, and the plugins that ship with Corvus are worked examples.";

const UPDATES_INFO: &str = "Compares the running version against the published releases on GitHub and shows a notice when a newer one exists. Nothing is downloaded and nothing is sent about this machine beyond the request itself. The check never runs while the vehicle is armed, and with no internet it fails silently. Corvus never needs the network to fly.";

impl SettingsPage {
    pub(super) fn plugins_tab(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let data = self.want(ui.ctx(), "plugins", "/api/plugins");
        let failed = data.is_none() && self.failed("plugins");
        let plugins: Vec<Value> = data
            .as_ref()
            .and_then(|d| d.get("plugins"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let dir = data
            .as_ref()
            .and_then(|d| d.get("user_dir"))
            .and_then(Value::as_str)
            .unwrap_or("~/.corvus/plugins")
            .to_string();
        let msg = self.msgs.get("plugins").cloned();
        let (reloading, opening) = (self.is_busy("plugins_reload"), self.is_busy("plugins_folder"));
        let mut hit = None;
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
                    plugin_row(ui, f, p, i == 0);
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
fn plugin_row(ui: &egui::Ui, f: &mut Form, p: &Value, first: bool) {
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
    ui.painter()
        .galley(Pos2::new(f.x, top + (head_h - n.size().y) / 2.0), n, style::text_1());
    ui.painter().galley(
        Pos2::new(f.x + f.w - o.size().x, top + (head_h - o.size().y) / 2.0),
        o,
        style::text_3(),
    );
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
}
