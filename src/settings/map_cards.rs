//! The Map pill: the map service and its API keys, the operator's own
//! elevation models, and what is drawn on the map (`mapServiceCard`,
//! `openMapTokenDialog`, `elevationDataCard`, `trackCard`, `mapToolsCard` and
//! `mapMotionCard` of `python/src/js/sidenav.js`).

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use super::tabs::{MEASURE, TRACK};
use super::{model, refused, Dialog, SettingsPage};
use crate::ui::form::{self, Form, CONTROL_H};
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::message::Tone;
use crate::ui::optioncards::Card;
use crate::ui::style::{self, Weight};

const MAP_RATE_HINT: &str = "How many times a second the aircraft on the map move, in 3D with their height line and shadow. 30 Hz needs about a third less CPU in 3D, which a laptop on battery notices. 60 Hz (the default) moves them more smoothly. The flight instruments and moving the map by hand always run at the display's full rate.";

const DEM_NOTE: &str = "Your own surface or terrain model as GeoTIFF, XYZ text or ESRI ASCII grid. Several files load into one model. Corvus stores and serves it on this computer, so it works offline. Choose it in the 3D view under Elevation; around your data the free elevation fills in.";

const DEM_ACCEPT: [&str; 6] = ["tif", "tiff", "txt", "xyz", "asc", "csv"];

fn arr(v: Option<&Value>, key: &str) -> Vec<Value> {
    v.and_then(|d| d.get(key))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

impl SettingsPage {
    pub(super) fn map_tab(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        form::section(ui, f, "Map data", |ui, f| {
            self.map_service_card(ui, f);
            self.elevation_card(ui, f);
        });
        form::section(ui, f, "On the map", |ui, f| {
            form::card(ui, f, "Flown track", |ui, f| {
                for r in &TRACK {
                    let on = model::flag(&self.cfg, "ui", r.key, r.default);
                    if let Some(v) = form::switch(ui, f, ("sw", r.key), r.label, r.hint, on) {
                        self.write(ui.ctx(), model::patch("ui", r.key, json!(v)), true);
                    }
                }
            });
            form::card(ui, f, "Map tools", |ui, f| {
                let on = model::flag(&self.cfg, "ui", MEASURE.key, MEASURE.default);
                if let Some(v) = form::switch(ui, f, "measure", MEASURE.label, MEASURE.hint, on) {
                    self.write(ui.ctx(), model::patch("ui", MEASURE.key, json!(v)), true);
                }
            });
            let rate = model::map_rate(&self.cfg).to_string();
            let mut picked = None;
            form::card(ui, f, "Map animation", |ui, f| {
                let opts = [
                    Card::new("30", "30 Hz", "Saves power"),
                    Card::new("60", "60 Hz", "Smoother"),
                ];
                form::field(ui, f, "Aircraft on the map", "", MAP_RATE_HINT, |ui, x, y, w| {
                    let mut inner = Form::new(x, y, w);
                    picked = form::option_cards(ui, &mut inner, "maprate", &opts, &rate, 2);
                    inner.y - y
                });
            });
            if let Some(r) = picked {
                self.write(
                    ui.ctx(),
                    model::patch("ui", "map_animation_rate", json!(r.parse::<u32>().unwrap_or(60))),
                    true,
                );
            }
        });
    }

    fn map_service_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let data = self.want(ui.ctx(), "sources", "/api/tiles/sources");
        let loading = data.is_none() && !self.failed("sources");
        let failed = data.is_none() && self.failed("sources");
        let providers = arr(data.as_ref(), "providers");
        let sources = arr(data.as_ref(), "sources");
        let fallback = data
            .as_ref()
            .and_then(|d| d.get("default_provider"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let active = model::active_provider(&self.cfg, &providers, &sources, &fallback);
        let saved = model::text(&self.cfg, "map", "base_layer").to_string();
        let layer = if sources.iter().any(|x| s(x, "id") == saved) {
            saved
        } else {
            providers
                .iter()
                .find(|p| s(p, "id") == active)
                .and_then(|p| {
                    arr(Some(p), "sources")
                        .first()
                        .and_then(Value::as_str)
                        .map(String::from)
                })
                .unwrap_or_default()
        };
        let mut apply: Option<String> = None;
        let mut open_keys = false;
        form::card(ui, f, "Map service", |ui, f| {
            if loading {
                form::desc(ui, f, "Loading map services\u{2026}");
                return;
            }
            if failed {
                form::desc(ui, f, "Could not load map services.");
                return;
            }
            if providers.is_empty() {
                form::desc(ui, f, "No map services available.");
                return;
            }
            let shown = model::shown_providers(&providers, &active);
            let opts: Vec<Card> = shown
                .iter()
                .map(|p| Card::new(s(p, "id"), s(p, "label"), model::describe_provider(p, &sources)).icon("map"))
                .collect();
            if let Some(id) = form::option_cards(ui, f, "provider", &opts, &active, 2) {
                if let Some(p) = providers.iter().find(|p| s(p, "id") == id) {
                    let ids = arr(Some(p), "sources");
                    let style_of = |id: &str| {
                        sources
                            .iter()
                            .find(|x| s(x, "id") == id)
                            .map(|x| s(x, "style").to_string())
                    };
                    let want = style_of(&layer);
                    let hit = ids
                        .iter()
                        .filter_map(Value::as_str)
                        .find(|i| want.is_some() && style_of(i) == want)
                        .or_else(|| ids.first().and_then(Value::as_str));
                    apply = hit.map(String::from);
                }
            }
            let ids = providers
                .iter()
                .find(|p| s(p, "id") == active)
                .map(|p| arr(Some(p), "sources"))
                .unwrap_or_default();
            let mut gap = 14.0;
            if ids.len() >= 2 {
                f.skip(14.0);
                let choices: Vec<Choice> = ids
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|id| {
                        let label = sources
                            .iter()
                            .find(|x| s(x, "id") == id)
                            .map(|x| s(x, "label"))
                            .filter(|l| !l.is_empty())
                            .unwrap_or(id);
                        Choice::new(id, label)
                    })
                    .collect();
                if let Some(id) = SettingsPage::select_field(ui, f, "Layer", "", &choices, &layer, "maplayer") {
                    apply = Some(id);
                }
                gap = 6.0;
            }
            if let Some(src) = sources.iter().find(|x| s(x, "id") == layer) {
                f.skip(gap);
                form::note(ui, f, &model::describe_cache(src));
            }
            let keyed: Vec<Value> = providers
                .iter()
                .filter(|p| p.get("token").is_some_and(|t| !t.is_null()))
                .cloned()
                .collect();
            if !keyed.is_empty() {
                f.skip(14.0);
                let b = Button::new("API KEYS", Variant::Secondary).icon("key-round");
                open_keys = form::block_button(ui, f, b, "apikeys");
                f.skip(6.0);
                form::note(ui, f, &model::keys_note(&keyed));
            }
        });
        if let Some(layer) = apply {
            if let Some(src) = sources.iter().find(|x| s(x, "id") == layer) {
                let provider = s(src, "provider").to_string();
                self.write(
                    ui.ctx(),
                    json!({ "map": { "base_layer": layer, "provider": provider } }),
                    false,
                );
            }
        }
        if open_keys {
            self.texts.retain(|k, _| !k.starts_with("token_"));
            self.hush("tokens");
            self.dialog = Some(Dialog::Tokens);
        }
    }

    // ---- the API keys dialog ----

    pub(super) fn tokens_dialog(&mut self, ctx: &egui::Context) -> bool {
        let data = self.res.get("sources").and_then(|s| s.value.clone());
        let keyed: Vec<Value> = arr(data.as_ref(), "providers")
            .into_iter()
            .filter(|p| p.get("token").is_some_and(|t| !t.is_null()))
            .collect();
        let mut close = false;
        let mut save = false;
        let mut remove: Option<String> = None;
        let busy = self.is_busy("tokens") || self.is_busy("token_remove");
        let msg = self.msgs.get("tokens").cloned();
        let closed = crate::ui::modal::show(
            ctx,
            "tokens",
            crate::ui::modal::Size::Md,
            "Map service API keys",
            |ui, x, y, w| {
                let mut f = Form::new(x, y, w);
                form::note(
                ui,
                &mut f,
                "Some map services need an API key from the provider. The key is stored on this machine only, is sent to the provider by Corvus rather than by the browser, and is never shown again once saved.",
            );
                for p in &keyed {
                    f.skip(14.0);
                    let meta = p.get("token").cloned().unwrap_or(Value::Null);
                    let label = {
                        let l = s(&meta, "label");
                        if l.is_empty() {
                            format!("{} API key", s(p, "label"))
                        } else {
                            l.to_string()
                        }
                    };
                    let info = format!(
                        "{}{}",
                        s(&meta, "help"),
                        if s(&meta, "signup").is_empty() {
                            String::new()
                        } else {
                            format!(" Get one at {}", s(&meta, "signup"))
                        }
                    )
                    .trim()
                    .to_string();
                    let saved = p.get("token_set").and_then(Value::as_bool).unwrap_or(false);
                    let key: &'static str = Box::leak(format!("token_{}", s(p, "id")).into_boxed_str());
                    let hint = if saved {
                        "A key is saved. Type to replace it"
                    } else {
                        "Paste the key here"
                    };
                    let buf = self.texts.entry(key).or_default();
                    form::field(ui, &mut f, &label, &info, "", |ui, x, y, w| {
                        let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, CONTROL_H));
                        crate::ui::field::TextField::new(buf)
                            .hint(hint)
                            .mono(true)
                            .pad(11.0, 11.0)
                            .password()
                            .show(ui, r, key);
                        CONTROL_H
                    });
                    f.skip(8.0);
                    let state = style::galley(
                        ui,
                        if saved { "Key saved" } else { "No key" },
                        11.0,
                        Weight::W400,
                        if saved { style::healthy() } else { style::text_3() },
                        0.0,
                    );
                    let b = Button::new("REMOVE", Variant::Secondary)
                        .small()
                        .enabled(saved && !busy);
                    let (bw, bh) = (b.width(ui), b.height());
                    let by = f.y;
                    ui.painter().galley(
                        Pos2::new(f.x + f.w - bw - 8.0 - state.size().x, by + (bh - state.size().y) / 2.0),
                        state,
                        style::text_3(),
                    );
                    if b.show(
                        ui,
                        Rect::from_min_size(Pos2::new(f.x + f.w - bw, by), Vec2::new(bw, bh)),
                        ("tokenrm", s(p, "id")),
                    ) {
                        remove = Some(s(p, "id").to_string());
                    }
                    f.y += bh;
                }
                if let Some((t, tone)) = &msg {
                    f.skip(14.0);
                    form::message(ui, &mut f, t, *tone);
                }
                f.skip(18.0);
                let (hit, bottom) = crate::ui::modal::actions(
                    ui,
                    f.x,
                    f.y,
                    f.w,
                    vec![
                        Button::new("CLOSE", Variant::Secondary),
                        Button::new("SAVE KEYS", Variant::Primary).icon("save").enabled(!busy),
                    ],
                );
                match hit {
                    Some(0) => close = true,
                    Some(1) => save = true,
                    _ => {}
                }
                bottom
            },
        );
        if save {
            let pending: Vec<(String, String)> = keyed
                .iter()
                .filter_map(|p| {
                    let id = s(p, "id").to_string();
                    let v = self.texts.get(format!("token_{id}").as_str()).map_or("", |t| t.trim());
                    (!v.is_empty()).then(|| (id, v.to_string()))
                })
                .collect();
            if pending.is_empty() {
                close = true;
            } else {
                self.hush("tokens");
                self.save_tokens(ctx, pending);
            }
        }
        if let Some(id) = remove {
            self.hush("tokens");
            self.act(
                ctx,
                "token_remove",
                &id,
                "/api/tiles/token",
                json!({ "provider": id, "token": "" }),
            );
        }
        closed || close
    }

    fn save_tokens(&mut self, ctx: &egui::Context, pending: Vec<(String, String)>) {
        // One request per key, in order, on one thread; the answer lists the
        // services that were refused and why.
        let server = self.server.clone();
        let labels: Vec<(String, String)> = pending
            .iter()
            .map(|(id, _)| {
                let label = self
                    .res
                    .get("sources")
                    .and_then(|r| r.value.as_ref())
                    .map(|d| arr(Some(d), "providers"))
                    .unwrap_or_default()
                    .iter()
                    .find(|p| s(p, "id") == id)
                    .map(|p| s(p, "label").to_string())
                    .unwrap_or_else(|| id.clone());
                (id.clone(), label)
            })
            .collect();
        self.busy.insert("tokens", true);
        self.calls.push(super::Call {
            tag: super::Tag::Act("tokens", String::new()),
            answer: crate::net::bg::Pending::spawn(ctx, move || {
                let mut failures = Vec::new();
                let mut saved = Vec::new();
                for ((id, token), (_, label)) in pending.iter().zip(&labels) {
                    let r = crate::net::http::post_json(
                        &server,
                        "/api/tiles/token",
                        &json!({"provider": id, "token": token}),
                    );
                    match refused(&r, "Could not save the key.") {
                        Some(e) => failures.push(format!("{label}: {e}")),
                        None => saved.push(id.clone()),
                    }
                }
                Ok(json!({ "failures": failures, "saved": saved }))
            }),
        });
    }

    pub(super) fn token_done(
        &mut self,
        ctx: &egui::Context,
        name: &'static str,
        arg: &str,
        answer: Result<Value, String>,
    ) {
        if name == "token_remove" {
            match refused(&answer, "Could not save the key.") {
                Some(e) => self.fail("tokens", &e),
                None => {
                    self.texts.remove(format!("token_{arg}").as_str());
                    self.keys_changed(ctx);
                }
            }
            return;
        }
        let v = answer.unwrap_or(Value::Null);
        let failures: Vec<String> = v
            .get("failures")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
            .unwrap_or_default();
        for id in arr(Some(&v), "saved").iter().filter_map(Value::as_str) {
            self.texts.remove(format!("token_{id}").as_str());
        }
        if !arr(Some(&v), "saved").is_empty() {
            self.keys_changed(ctx);
        }
        if failures.is_empty() {
            self.dialog = None;
        } else {
            self.fail("tokens", &failures.join(" \u{b7} "));
        }
    }

    /// A key just set can change both the layers and the elevation models.
    fn keys_changed(&mut self, ctx: &egui::Context) {
        self.res.remove("sources");
        self.get(ctx, "sources", "/api/tiles/sources");
        self.sources_dirty = true;
    }

    // ---- the operator's elevation models ----

    fn elevation_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let units = self.effects.units;
        let list = self.want(ui.ctx(), "dem", "/api/dem");
        let failed = list.is_none() && self.failed("dem");
        let models = arr(list.as_ref(), "models");
        let mut load = false;
        let mut actions: Vec<(&'static str, Value)> = Vec::new();
        let crs = self.choice.get("dem_crs").cloned().unwrap_or_else(|| "auto".into());
        let progress = self.dem_progress.lock().map(|p| p.clone()).unwrap_or_default();
        let msg = self.msgs.get("dem").cloned();
        let busy = self.is_busy("dem_import");
        let mut new_crs = None;
        form::card(ui, f, "Elevation data", |ui, f| {
            if failed {
                form::desc(ui, f, "Elevation data unavailable.");
            } else if list.is_none() {
                form::desc(ui, f, "Loading\u{2026}");
            } else if models.is_empty() {
                form::desc(ui, f, "No elevation data loaded yet.");
            } else {
                for (i, m) in models.iter().enumerate() {
                    if let Some(a) = dem_row(ui, f, m, &units, i == 0) {
                        actions.push(a);
                    }
                }
            }
            f.skip(14.0);
            form::field(
                ui,
                f,
                "Coordinate system",
                "",
                "A GeoTIFF names its own. XYZ and ASCII files do not: German state data is usually ETRS89 / UTM zone 32N.",
                |ui, x, y, w| {
                    let choices: Vec<Choice> = model::DEM_CRS.iter().map(|c| Choice::new(c.0, c.1)).collect();
                    let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, CONTROL_H));
                    new_crs = kit::select(ui, r, &choices, &crs, "", false, "", "demcrs");
                    let mut h = CONTROL_H;
                    if crs == "other" {
                        let r2 = Rect::from_min_size(Pos2::new(x, y + h + 8.0), Vec2::new(w, CONTROL_H));
                        let buf = self.texts.entry("dem_epsg").or_default();
                        crate::ui::field::TextField::new(buf)
                            .hint("EPSG code, for example 25832")
                            .mono(true)
                            .pad(11.0, 11.0)
                            .show(ui, r2, "demepsg");
                        h += 8.0 + CONTROL_H;
                    }
                    h
                },
            );
            f.skip(14.0);
            form::field(ui, f, "Name", "", "", |ui, x, y, w| {
                let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, CONTROL_H));
                let buf = self.texts.entry("dem_name").or_default();
                crate::ui::field::TextField::new(buf)
                    .hint("From the first file name")
                    .pad(11.0, 11.0)
                    .show(ui, r, "demname");
                CONTROL_H
            });
            f.skip(14.0);
            let b = Button::new("LOAD ELEVATION FILES\u{2026}", Variant::Secondary)
                .icon("mountain")
                .enabled(!busy);
            load = form::block_button(ui, f, b, "demload");
            if busy && !progress.is_empty() {
                f.skip(10.0);
                form::message(ui, f, &progress, Tone::Plain);
            } else if let Some((t, tone)) = &msg {
                f.skip(10.0);
                form::message(ui, f, t, *tone);
            }
            f.skip(8.0);
            form::note(ui, f, DEM_NOTE);
        });
        if let Some(c) = new_crs {
            self.choice.insert("dem_crs", c);
        }
        if load {
            self.hush("dem");
            self.text("dem_files").clear();
            self.dialog = Some(Dialog::DemFiles { into: None });
        }
        for (what, v) in actions {
            let id = s(&v, "id").to_string();
            let name = s(&v, "name").to_string();
            match what {
                "add" => {
                    self.hush("dem");
                    self.text("dem_files").clear();
                    self.dialog = Some(Dialog::DemFiles { into: Some(id) });
                }
                "rename" => {
                    *self.text("dem_rename") = name.clone();
                    self.dialog = Some(Dialog::DemRename { id });
                }
                "remove" => self.dialog = Some(Dialog::DemRemove { id, name }),
                _ => {
                    // "show": frame the model on the Home map
                    let b = arr(Some(&v), "bounds");
                    let n = |i: usize| b.get(i).and_then(Value::as_f64);
                    if let (Some(w), Some(s_), Some(e), Some(n_)) = (n(0), n(1), n(2), n(3)) {
                        let bounds = crate::map::regions::Bounds { w, s: s_, e, n: n_ };
                        let zoom = crate::map::search::fit_zoom(&bounds, 1000.0, 700.0, 40.0, 18.0);
                        self.show_on_map = Some(((w + e) / 2.0, (s_ + n_) / 2.0, zoom));
                    }
                }
            }
        }
    }

    pub(super) fn dem_import(&mut self, ctx: &egui::Context, into: Option<String>) {
        let raw = self.texts.get("dem_files").cloned().unwrap_or_default();
        let files = expand_paths(raw.trim());
        let choice = self.choice.get("dem_crs").cloned().unwrap_or_else(|| "auto".into());
        let other = self.texts.get("dem_epsg").cloned().unwrap_or_default();
        let crs = model::crs_param(&choice, &other);
        if choice == "other" && !model::epsg_ok(&crs) {
            self.say(
                "dem",
                "Enter the EPSG code of the coordinate system, for example 25832.",
                Tone::Err,
            );
            return;
        }
        if files.is_empty() {
            self.say("dem", "No elevation file found there.", Tone::Err);
            return;
        }
        self.hush("dem");
        let name = self
            .texts
            .get("dem_name")
            .map_or(String::new(), |n| n.trim().to_string());
        let server = self.server.clone();
        let progress = self.dem_progress.clone();
        self.busy.insert("dem_import", true);
        self.calls.push(super::Call {
            tag: super::Tag::Act("dem_import", String::new()),
            answer: crate::net::bg::Pending::spawn(ctx, move || {
                let mut model_id = into;
                let (mut done, mut failures) = (0usize, Vec::<String>::new());
                for (i, path) in files.iter().enumerate() {
                    let fname = path
                        .file_name()
                        .map_or(String::new(), |n| n.to_string_lossy().into_owned());
                    if let Ok(mut p) = progress.lock() {
                        *p = format!("Loading {} of {}: {fname}\u{2026}", i + 1, files.len());
                    }
                    let mut query = format!(
                        "filename={}&crs={}",
                        crate::net::http::encode_component(&fname),
                        crate::net::http::encode_component(&crs)
                    );
                    if let Some(m) = &model_id {
                        query.push_str(&format!("&model={}", crate::net::http::encode_component(m)));
                    } else if !name.is_empty() {
                        query.push_str(&format!("&name={}", crate::net::http::encode_component(&name)));
                    }
                    let r = std::fs::read(path).map_err(|e| e.to_string()).and_then(|bytes| {
                        crate::net::http::post_bytes(
                            &server,
                            &format!("/api/dem/import?{query}"),
                            &bytes,
                            std::time::Duration::from_secs(900),
                        )
                    });
                    match r {
                        Ok(v) => {
                            if let Some(id) = v.get("model").and_then(|m| m.get("id")).and_then(Value::as_str) {
                                model_id = Some(id.to_string());
                            }
                            done += 1;
                        }
                        Err(e) => failures.push(format!(
                            "{fname}: {}",
                            if e.is_empty() { "could not be loaded" } else { &e }
                        )),
                    }
                }
                Ok(json!({ "done": done, "total": files.len(), "failures": failures }))
            }),
        });
    }

    pub(super) fn dem_done(
        &mut self,
        ctx: &egui::Context,
        name: &'static str,
        _arg: &str,
        answer: Result<Value, String>,
    ) {
        match name {
            "dem_import" => {
                let v = answer.unwrap_or(Value::Null);
                let done = v.get("done").and_then(Value::as_u64).unwrap_or(0);
                let total = v.get("total").and_then(Value::as_u64).unwrap_or(0);
                let failures: Vec<String> = arr(Some(&v), "failures")
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect();
                if failures.is_empty() {
                    let t = if done == 1 {
                        "Loaded. Choose it in the 3D view under Elevation.".to_string()
                    } else {
                        format!("{done} files loaded. Choose the model in the 3D view under Elevation.")
                    };
                    self.say("dem", t, Tone::Ok);
                } else {
                    let head = if done > 0 {
                        format!("{done} of {total} files loaded. ")
                    } else {
                        String::new()
                    };
                    self.say("dem", head + &failures.join(" \u{b7} "), Tone::Err);
                }
                if done > 0 {
                    self.texts.remove("dem_name");
                    self.dem_changed(ctx);
                }
            }
            _ => match refused(
                &answer,
                if name == "dem_rename" {
                    "Could not rename the model."
                } else {
                    "Could not remove the model."
                },
            ) {
                Some(e) => self.fail("dem_dialog", &e),
                None => {
                    self.dialog = None;
                    self.dem_changed(ctx);
                }
            },
        }
    }

    fn dem_changed(&mut self, ctx: &egui::Context) {
        self.res.remove("dem");
        self.get(ctx, "dem", "/api/dem");
        self.res.remove("sources");
        self.get(ctx, "sources", "/api/tiles/sources");
        self.sources_dirty = true;
    }

    pub(super) fn dem_dialogs(&mut self, ctx: &egui::Context) -> bool {
        let msg = self.msgs.get("dem_dialog").cloned();
        match self.dialog.as_ref() {
            Some(Dialog::DemFiles { into }) => {
                let into = into.clone();
                let mut go = false;
                let mut close = false;
                let buf = self.texts.entry("dem_files").or_default();
                let closed = crate::ui::modal::show(
                    ctx,
                    "demfiles",
                    crate::ui::modal::Size::Md,
                    "Load elevation files",
                    |ui, x, y, w| {
                        let mut f = Form::new(x, y, w);
                        form::field(
                        ui,
                        &mut f,
                        "File or folder",
                        "",
                        "The path of a GeoTIFF, XYZ text or ESRI ASCII grid file on this computer, or of a folder: every file in it that ends in .tif, .tiff, .txt, .xyz, .asc or .csv loads into one model.",
                        |ui, x, y, w| {
                            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, CONTROL_H));
                            crate::ui::field::TextField::new(buf).hint("/path/to/elevation").mono(true).pad(11.0, 11.0).show(ui, r, "demfilespath");
                            CONTROL_H
                        },
                    );
                        f.skip(18.0);
                        let (hit, bottom) = crate::ui::modal::actions(
                            ui,
                            f.x,
                            f.y,
                            f.w,
                            vec![
                                Button::new("CANCEL", Variant::Secondary),
                                Button::new("LOAD", Variant::Primary).icon("mountain"),
                            ],
                        );
                        match hit {
                            Some(0) => close = true,
                            Some(1) => go = true,
                            _ => {}
                        }
                        bottom
                    },
                );
                if go {
                    self.dialog = None;
                    self.dem_import(ctx, into);
                }
                closed || close
            }
            Some(Dialog::DemRename { id, .. }) => {
                let id = id.clone();
                let mut go = false;
                let mut close = false;
                let buf = self.texts.entry("dem_rename").or_default();
                let closed = crate::ui::modal::show(
                    ctx,
                    "demrename",
                    crate::ui::modal::Size::Md,
                    "Rename elevation model",
                    |ui, x, y, w| {
                        let mut f = Form::new(x, y, w);
                        form::field(ui, &mut f, "Name", "", "", |ui, x, y, w| {
                            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, CONTROL_H));
                            let resp =
                                crate::ui::field::TextField::new(buf)
                                    .pad(11.0, 11.0)
                                    .show(ui, r, "demrenamefield");
                            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                go = true;
                            }
                            CONTROL_H
                        });
                        if let Some((t, tone)) = &msg {
                            f.skip(10.0);
                            form::message(ui, &mut f, t, *tone);
                        }
                        f.skip(18.0);
                        let (hit, bottom) = crate::ui::modal::actions(
                            ui,
                            f.x,
                            f.y,
                            f.w,
                            vec![
                                Button::new("CANCEL", Variant::Secondary),
                                Button::new("SAVE", Variant::Primary).icon("save"),
                            ],
                        );
                        match hit {
                            Some(0) => close = true,
                            Some(1) => go = true,
                            _ => {}
                        }
                        bottom
                    },
                );
                if go {
                    let name = self
                        .texts
                        .get("dem_rename")
                        .map_or(String::new(), |n| n.trim().to_string());
                    if name.is_empty() {
                        self.fail("dem_dialog", "Enter a name.");
                    } else {
                        self.hush("dem_dialog");
                        self.act(
                            ctx,
                            "dem_rename",
                            &id,
                            "/api/dem/rename",
                            json!({"id": id, "name": name}),
                        );
                    }
                }
                closed || close
            }
            Some(Dialog::DemRemove { id, name }) => {
                let (id, name) = (id.clone(), name.clone());
                let mut go = false;
                let mut close = false;
                let busy = self.is_busy("dem_remove");
                let closed = crate::ui::modal::show(
                    ctx,
                    "demremove",
                    crate::ui::modal::Size::Md,
                    "Remove elevation model",
                    |ui, x, y, w| {
                        let mut f = Form::new(x, y, w);
                        form::note(
                        ui,
                        &mut f,
                        &format!("{name} and its converted files are deleted from this computer. The files you loaded it from are not touched."),
                    );
                        if let Some((t, tone)) = &msg {
                            f.skip(10.0);
                            form::message(ui, &mut f, t, *tone);
                        }
                        f.skip(18.0);
                        let (hit, bottom) = crate::ui::modal::actions(
                            ui,
                            f.x,
                            f.y,
                            f.w,
                            vec![
                                Button::new("CANCEL", Variant::Secondary),
                                Button::new("REMOVE", Variant::Danger).icon("trash-2").enabled(!busy),
                            ],
                        );
                        match hit {
                            Some(0) => close = true,
                            Some(1) => go = true,
                            _ => {}
                        }
                        bottom
                    },
                );
                if go {
                    self.hush("dem_dialog");
                    self.act(ctx, "dem_remove", &id, "/api/dem/remove", json!({"id": id}));
                }
                closed || close
            }
            _ => false,
        }
    }
}

/// One loaded model: name with its kind chip, the summary and four buttons,
/// the kind's sentence, the detail line.
fn dem_row(
    ui: &mut egui::Ui,
    f: &mut Form,
    m: &Value,
    units: &crate::data::units::Units,
    first: bool,
) -> Option<(&'static str, Value)> {
    let name = s(m, "name");
    let id = s(m, "id");
    let kind = model::dem_kind(s(m, "kind"));
    let mut hit = None;
    let top = f.y + if first { 2.0 } else { 14.0 };
    let summary = style::galley_mono(ui, &model::dem_summary(m, units), 12.0, style::text_3());
    let n = style::galley(ui, name, 13.0, Weight::W500, style::text_1(), 0.0);
    let chip = kind.map(|k| style::galley(ui, k.0, 9.5, Weight::W700, style::text_3(), 0.6));
    let name_w = n.size().x + chip.as_ref().map_or(0.0, |c| 8.0 + c.size().x + 10.0 + 2.0);
    let meta_w = summary.size().x + 8.0 + 4.0 * 28.0 + 3.0 * 8.0;
    let wrap = name_w + 12.0 + meta_w > f.w;
    let head_h = 28.0;
    // the name and chip
    let ny = top + (head_h - n.size().y) / 2.0;
    ui.painter().galley(Pos2::new(f.x, ny), n.clone(), style::text_1());
    if let (Some(c), Some(k)) = (chip, kind) {
        let color = if s(m, "kind") == "surface" {
            style::accent()
        } else {
            style::text_3()
        };
        let cw = c.size().x + 10.0;
        let ch = c.size().y + 4.0;
        let r = Rect::from_min_size(
            Pos2::new(f.x + n.size().x + 8.0, top + (head_h - ch) / 2.0),
            Vec2::new(cw, ch),
        );
        ui.painter()
            .rect_stroke(r, 255, Stroke::new(1.0, color), egui::StrokeKind::Inside);
        let c = style::galley(ui, k.0, 9.5, Weight::W700, color, 0.6);
        ui.painter()
            .galley(Pos2::new(r.left() + 5.0, r.center().y - c.size().y / 2.0), c, color);
    }
    // the summary and the buttons, right aligned (below when they do not fit)
    let my = if wrap { top + head_h + 4.0 } else { top };
    let mut x = f.x + f.w - 28.0;
    let icons: [(&'static str, &'static str, String); 4] = [
        ("trash-2", "remove", format!("Remove {name}")),
        ("pencil", "rename", format!("Rename {name}")),
        ("file-plus", "add", format!("Add files to {name}")),
        ("map-pin", "show", format!("Show {name} on the map")),
    ];
    for (icon, what, title) in icons {
        if kit::icon_btn(
            ui,
            Rect::from_min_size(Pos2::new(x, my), Vec2::splat(28.0)),
            icon,
            &title,
            false,
            ("dem", what, id),
        ) {
            hit = Some((what, m.clone()));
        }
        x -= 36.0;
    }
    ui.painter().galley(
        Pos2::new(x + 36.0 - 8.0 - summary.size().x, my + (28.0 - summary.size().y) / 2.0),
        summary,
        style::text_3(),
    );
    let mut y = my + 28.0;
    let desc = |ui: &egui::Ui, y: f32, text: &str, color| {
        let g = kit::wrapped_weight(ui, text, 12.0, Weight::W400, color, f.w, 1.5);
        let h = g.size().y;
        ui.painter().galley(Pos2::new(f.x, y), g, color);
        h
    };
    if let Some(k) = kind {
        y += 4.0;
        y += desc(ui, y, k.1, style::text_2());
    }
    y += 4.0;
    y += desc(ui, y, &model::dem_detail(m), style::text_3());
    y += 14.0;
    ui.painter().hline(
        egui::Rangef::new(f.x, f.x + f.w),
        y + 0.5,
        Stroke::new(1.0, style::border_soft()),
    );
    f.y = y + 1.0;
    hit
}

/// A file, or the elevation files directly inside a folder, in name order.
fn expand_paths(raw: &str) -> Vec<std::path::PathBuf> {
    let raw = raw.trim().trim_matches('"');
    let raw = raw.strip_prefix("~/").map_or(raw.to_string(), |rest| {
        format!("{}/{rest}", std::env::var("HOME").unwrap_or_default())
    });
    let p = std::path::PathBuf::from(&raw);
    if raw.is_empty() {
        return Vec::new();
    }
    if p.is_dir() {
        let mut v: Vec<std::path::PathBuf> = std::fs::read_dir(&p)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|f| {
                        f.is_file()
                            && f.extension()
                                .and_then(|e| e.to_str())
                                .is_some_and(|e| DEM_ACCEPT.contains(&e.to_ascii_lowercase().as_str()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    } else if p.is_file() {
        vec![p]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_escaped_like_encode_uri_component() {
        assert_eq!(crate::net::http::encode_component("a b&c.tif"), "a%20b%26c.tif");
        assert_eq!(crate::net::http::encode_component("K\u{f6}ln"), "K%C3%B6ln");
        assert_eq!(crate::net::http::encode_component("25832"), "25832");
    }

    #[test]
    fn a_folder_loads_its_elevation_files_in_order() {
        let dir = std::env::temp_dir().join(format!("corvus-dem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["b.tif", "a.XYZ", "c.png", "d.asc"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        let names: Vec<String> = expand_paths(dir.to_str().unwrap())
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.XYZ", "b.tif", "d.asc"]);
        assert_eq!(expand_paths(dir.join("b.tif").to_str().unwrap()).len(), 1);
        assert!(expand_paths(dir.join("none.tif").to_str().unwrap()).is_empty());
        assert!(expand_paths("").is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
