//! The offline map dialog (`src/js/tiles.js`, map.css `.tiles-*`): frame an
//! area on the map, pick a source and a zoom range, see what it comes to,
//! download it with its elevation and buildings, follow the jobs, and manage
//! the areas already on this laptop (look at, rename, delete).

use std::collections::HashMap;

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::bg::Pending;
use crate::http::{self, Server};
use crate::kit::{self, Button, Choice, Variant};
use crate::map::Map;
use crate::map_search as fit;
use crate::modal;
use crate::regions::Bounds;
use crate::style::{self, Weight};
use crate::tiles_model as model;

const FIELD_H: f32 = 34.5;
const TERRAIN_PHASE: &str = "Imagery done. Downloading elevation data\u{2026}";

type Answer = Pending<Result<Value, String>>;

#[derive(PartialEq, Clone, Copy)]
enum Kind {
    Ok,
    Warn,
    Err,
    Info,
}

/// The dialog's state.
pub struct TilesCard {
    pub open: bool,
    sources: Value,
    regions: Vec<Value>,
    sources_load: Option<Answer>,
    regions_load: Option<Answer>,
    source: String,
    name: String,
    min_text: String,
    max_text: String,
    terrain: bool,
    buildings: bool,
    captured: Option<(Bounds, i64)>,
    start: Option<Answer>,
    chain: Vec<String>,
    followed: usize,
    results: HashMap<String, Value>,
    running: bool,
    poll: Option<Answer>,
    last_poll: f64,
    progress: (i64, i64),
    message: Option<(Kind, String)>,
    building_blocks: i64,
    rename: Option<(String, String, String)>,
    confirm: Option<String>,
    work: Vec<Answer>,
    scroll_to: Option<Bounds>,
    server: Option<Server>,
}

impl Default for TilesCard {
    fn default() -> TilesCard {
        TilesCard {
            open: false,
            sources: Value::Null,
            regions: Vec::new(),
            sources_load: None,
            regions_load: None,
            source: String::new(),
            name: String::new(),
            min_text: String::new(),
            max_text: String::new(),
            terrain: true,
            buildings: true,
            captured: None,
            start: None,
            chain: Vec::new(),
            followed: 0,
            results: HashMap::new(),
            running: false,
            poll: None,
            last_poll: f64::NEG_INFINITY,
            progress: (0, 0),
            message: None,
            building_blocks: 0,
            rename: None,
            confirm: None,
            work: Vec::new(),
            scroll_to: None,
            server: None,
        }
    }
}

fn clamp_int(text: &str, lo: i64, hi: i64) -> i64 {
    text.trim().parse::<i64>().map_or(lo, |n| n.clamp(lo, hi))
}

impl TilesCard {
    fn list(&self, key: &str) -> Vec<Value> {
        self.sources
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn cap_for(&self, id: &str) -> i64 {
        self.list("sources")
            .iter()
            .find(|s| s.get("id").and_then(Value::as_str) == Some(id))
            .and_then(|s| s.get("maxzoom").and_then(Value::as_i64))
            .unwrap_or(model::FALLBACK_ZOOM_CAP)
    }

    fn max_tiles(&self) -> i64 {
        self.sources
            .get("max_tiles_per_job")
            .and_then(Value::as_i64)
            .unwrap_or(model::FALLBACK_MAX_TILES)
    }

    fn provider(&self, id: &str) -> Option<Value> {
        self.list("providers")
            .into_iter()
            .find(|p| p.get("id").and_then(Value::as_str) == Some(id))
    }

    /// "Service \u{b7} Layer"; the service's name is left off when the layer carries it.
    fn source_label(&self, s: &Value) -> String {
        let layer = s
            .get("label")
            .or_else(|| s.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if self.list("providers").is_empty() {
            return layer;
        }
        let prov = s.get("provider").and_then(Value::as_str).unwrap_or("");
        let plabel = self
            .provider(prov)
            .and_then(|p| p.get("label").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| prov.to_string());
        if plabel == layer {
            layer
        } else {
            format!("{plabel} \u{b7} {layer}")
        }
    }

    fn label_by_id(&self, id: &str) -> String {
        if let Some(d) = self
            .list("terrain")
            .iter()
            .find(|d| d.get("id").and_then(Value::as_str) == Some(id))
        {
            return match d.get("label").and_then(Value::as_str) {
                Some(l) if !l.is_empty() => format!("Elevation \u{b7} {l}"),
                _ => "Elevation".into(),
            };
        }
        self.list("sources")
            .iter()
            .find(|s| s.get("id").and_then(Value::as_str) == Some(id))
            .map(|s| self.source_label(s))
            .unwrap_or_else(|| id.to_string())
    }

    fn usable_sources(&self) -> Vec<Value> {
        self.list("sources")
            .into_iter()
            .filter(|s| {
                let p = self.provider(s.get("provider").and_then(Value::as_str).unwrap_or(""));
                !p.is_some_and(|p| {
                    p.get("token_required") == Some(&Value::Bool(true))
                        && p.get("token_set") != Some(&Value::Bool(true))
                })
            })
            .collect()
    }

    fn default_terrain(&self) -> Option<Value> {
        let terrain = self.list("terrain");
        let id = self.sources.get("default_terrain").and_then(Value::as_str);
        terrain
            .iter()
            .find(|d| d.get("id").and_then(Value::as_str) == id)
            .or_else(|| terrain.first())
            .cloned()
    }

    fn say(&mut self, kind: Kind, text: impl Into<String>) {
        self.message = Some((kind, text.into()));
    }

    fn zooms(&self) -> (i64, i64) {
        let cap = self.cap_for(&self.source);
        let (mut lo, mut hi) = (clamp_int(&self.min_text, 0, cap), clamp_int(&self.max_text, 0, cap));
        if lo > hi {
            std::mem::swap(&mut lo, &mut hi);
        }
        (lo, hi)
    }

    fn capture(&mut self, map: &Map, area: Rect) {
        self.captured = map.camera().map(|(lng, lat, z)| {
            (
                model::view_bounds(lng, lat, z, f64::from(area.width()), f64::from(area.height())),
                z.floor() as i64,
            )
        });
        if let Some((_, z)) = self.captured {
            let cap = self.cap_for(&self.source);
            self.min_text = z.clamp(0, cap).to_string();
            self.max_text = (z + model::ZOOM_SPAN).min(cap).to_string();
        }
    }

    fn load(&mut self, ctx: &egui::Context, server: &Server) {
        let (a, b) = (server.clone(), server.clone());
        self.sources_load = Some(Pending::spawn(ctx, move || http::get_json(&a, "/api/tiles/sources")));
        self.regions_load = Some(Pending::spawn(ctx, move || http::get_json(&b, "/api/tiles/regions")));
    }

    fn reload_regions(&mut self, ctx: &egui::Context, server: &Server) {
        let s = server.clone();
        self.regions_load = Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/tiles/regions")));
    }

    /// Opens the dialog on the map's current view.
    pub fn open(&mut self, ctx: &egui::Context, map: &Map, area: Rect) {
        self.open = true;
        self.server = Some(map.server().clone());
        self.source = map.source.clone();
        self.capture(map, area);
        self.load(ctx, map.server());
        if self.running {
            self.say(Kind::Info, "");
            self.message = None;
        }
    }

    pub fn close_dialog(&mut self) {
        self.close();
    }

    fn close(&mut self) {
        self.open = false;
        self.rename = None;
        self.confirm = None;
    }

    fn estimate(&self) -> (i64, i64) {
        let Some((b, _)) = self.captured else {
            return (0, 0);
        };
        let (lo, hi) = self.zooms();
        let imagery = model::tile_count(&b, lo, hi);
        let mut count = imagery;
        if self.terrain {
            if let Some(d) = self.default_terrain() {
                let maxz = d.get("maxzoom").and_then(Value::as_i64).unwrap_or(0);
                let sparse = d.get("sparse_above").and_then(Value::as_i64);
                count += model::dem_count(&b, maxz, sparse, lo, hi, self.max_tiles());
            }
        }
        (imagery, count)
    }

    fn start_download(&mut self, ctx: &egui::Context, map: &Map) {
        let Some((b, _)) = self.captured else {
            return;
        };
        let (lo, hi) = self.zooms();
        if self.source.is_empty() || lo > hi || model::tile_count(&b, lo, hi) > self.max_tiles() {
            return;
        }
        let mut body = json!({
            "source": self.source,
            "name": self.name.trim(),
            "bounds": {"w": b.w, "s": b.s, "e": b.e, "n": b.n},
            "minzoom": lo,
            "maxzoom": hi,
            "terrain": self.terrain && self.default_terrain().is_some(),
            "buildings": self.buildings,
        });
        let _ = &mut body;
        self.chain.clear();
        self.followed = 0;
        self.results.clear();
        self.progress = (0, 0);
        self.message = None;
        self.running = true;
        let s = map.server().clone();
        self.start = Some(Pending::spawn(ctx, move || {
            http::post_json(&s, "/api/tiles/download", &body)
        }));
    }

    fn cancel(&mut self, ctx: &egui::Context, server: &Server) {
        for id in self.chain.clone() {
            let s = server.clone();
            self.work.push(Pending::spawn(ctx, move || {
                http::post_json(&s, "/api/tiles/cancel", &json!({ "id": id }))
            }));
        }
    }

    fn finish(&mut self, ctx: &egui::Context, server: &Server) {
        self.running = false;
        let ended: Vec<&Value> = self.chain.iter().filter_map(|id| self.results.get(id)).collect();
        let state_of = |v: &Value| v.get("state").and_then(Value::as_str).unwrap_or("").to_string();
        let failed_tiles: i64 = ended
            .iter()
            .map(|r| r.get("failed").and_then(Value::as_i64).unwrap_or(0))
            .sum();
        let imagery = self.chain.first().and_then(|id| self.results.get(id));
        let err_of = |v: &Value| {
            v.get("error")
                .and_then(Value::as_str)
                .filter(|e| !e.is_empty())
                .unwrap_or("unknown error")
                .to_string()
        };
        let message = if ended.iter().any(|r| state_of(r) == "cancelled") {
            (
                Kind::Warn,
                "Download cancelled. Tiles fetched so far stay cached.".to_string(),
            )
        } else if imagery.is_some_and(|r| state_of(r) == "failed") {
            (
                Kind::Err,
                format!("Download failed: {}", err_of(imagery.expect("imagery"))),
            )
        } else if let Some(dem) = ended.iter().find(|r| state_of(r) == "failed") {
            (
                Kind::Warn,
                format!(
                    "The map was downloaded, but the elevation data was not: {}. 3D terrain will be flat offline here.",
                    err_of(dem)
                ),
            )
        } else if failed_tiles > 0 {
            (
                Kind::Warn,
                format!(
                    "Download finished, but {} tiles could not be fetched. Start the same download again to fill the gaps.",
                    model::fmt_count(failed_tiles)
                ),
            )
        } else {
            let mut t = "Download complete. Tiles cached for offline use.".to_string();
            if self.building_blocks > 0 {
                t.push_str(&format!(
                    " Buildings for {} blocks are still being fetched in the background; leave Corvus running and online for a few minutes.",
                    model::fmt_count(self.building_blocks)
                ));
            }
            (Kind::Ok, t)
        };
        self.message = Some(message);
        self.reload_regions(ctx, server);
    }

    fn take_answers(&mut self, ctx: &egui::Context, map: &mut Map, now: f64) {
        let server = map.server().clone();
        if let Some(r) = self.sources_load.as_ref().and_then(Pending::take) {
            self.sources_load = None;
            if let Ok(d) = r {
                self.sources = d;
                let usable = self.usable_sources();
                let known = |id: &str| usable.iter().any(|s| s.get("id").and_then(Value::as_str) == Some(id));
                if !known(&self.source) {
                    self.source = if known(&map.source) {
                        map.source.clone()
                    } else {
                        usable
                            .first()
                            .and_then(|s| s.get("id").and_then(Value::as_str))
                            .unwrap_or("")
                            .to_string()
                    };
                }
                if let Some((_, z)) = self.captured {
                    let cap = self.cap_for(&self.source);
                    self.min_text = z.clamp(0, cap).to_string();
                    self.max_text = (z + model::ZOOM_SPAN).min(cap).to_string();
                }
            }
        }
        if let Some(r) = self.regions_load.as_ref().and_then(Pending::take) {
            self.regions_load = None;
            self.regions = r
                .ok()
                .and_then(|d| d.get("regions").and_then(Value::as_array).cloned())
                .unwrap_or_default();
        }
        if let Some(r) = self.start.as_ref().and_then(Pending::take) {
            self.start = None;
            match r {
                Ok(d) if d.get("job_id").and_then(Value::as_str).is_some() => {
                    self.chain = ["job_id", "terrain_job_id", "terrain_source_job_id"]
                        .iter()
                        .filter_map(|k| d.get(*k).and_then(Value::as_str).map(str::to_string))
                        .collect();
                    self.building_blocks = d.get("building_blocks").and_then(Value::as_i64).unwrap_or(0);
                    self.followed = 0;
                    self.reload_regions(ctx, &server);
                }
                Ok(d) => {
                    let e = d
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("No job id returned")
                        .to_string();
                    self.running = false;
                    self.say(Kind::Err, e);
                }
                Err(e) => {
                    self.running = false;
                    self.say(Kind::Err, if e.is_empty() { "Download failed".to_string() } else { e });
                }
            }
        }
        if let Some(r) = self.poll.as_ref().and_then(Pending::take) {
            self.poll = None;
            if let Ok(d) = r {
                let jobs = d.get("jobs").and_then(Value::as_array).cloned().unwrap_or_default();
                if let Some(id) = self.chain.get(self.followed).cloned() {
                    if let Some(job) = jobs
                        .iter()
                        .find(|j| j.get("job_id").and_then(Value::as_str) == Some(&id))
                    {
                        self.progress = (
                            job.get("done").and_then(Value::as_i64).unwrap_or(0),
                            job.get("total").and_then(Value::as_i64).unwrap_or(0),
                        );
                        let state = job.get("state").and_then(Value::as_str).unwrap_or("");
                        if matches!(state, "done" | "failed" | "cancelled") {
                            self.results.insert(id, job.clone());
                            if state != "cancelled" && self.followed + 1 < self.chain.len() {
                                self.followed += 1;
                                self.progress = (0, 0);
                                self.say(Kind::Info, TERRAIN_PHASE);
                                self.reload_regions(ctx, &server);
                            } else {
                                self.finish(ctx, &server);
                            }
                        }
                    } else if self.running && !self.chain.is_empty() {
                        self.running = false;
                        self.say(
                            Kind::Err,
                            "The download stopped. Corvus was restarted while it ran. Start it again to fetch the rest.",
                        );
                    }
                }
            }
        }
        if self.running && !self.chain.is_empty() && self.poll.is_none() && now - self.last_poll >= 1.0 {
            self.last_poll = now;
            let s = server.clone();
            self.poll = Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/tiles/jobs")));
        }
        let mut done = false;
        self.work.retain(|w| match w.take() {
            Some(_) => {
                done = true;
                false
            }
            None => true,
        });
        if done {
            self.reload_regions(ctx, &server);
        }
        if self.running || !self.work.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
    }

    /// Draws the dialog over the map.
    pub fn show(&mut self, ctx: &egui::Context, area: Rect, map: &mut Map) {
        if !self.open {
            return;
        }
        let now = ctx.input(|i| i.time);
        self.take_answers(ctx, map, now);
        let mut close = false;
        let mut action: Option<usize> = None;
        let height = ctx.content_rect().height();
        let close_asked = modal::show(ctx, "tilesdialog", modal::Size::Lg, "Offline map", |ui, x, y, w| {
            let mut cy = y;
            // source
            cy = label(ui, x, cy, "Source");
            let choices: Vec<Choice> = self
                .usable_sources()
                .iter()
                .map(|s| Choice::new(s.get("id").and_then(Value::as_str).unwrap_or(""), self.source_label(s)))
                .collect();
            if let Some(v) = kit::select(
                ui,
                Rect::from_min_size(Pos2::new(x, cy), Vec2::new(w, FIELD_H)),
                &choices,
                &self.source,
                "",
                false,
                "",
                "tilessource",
            ) {
                self.source = v;
                let (lo, hi) = self.zooms();
                self.min_text = lo.to_string();
                self.max_text = hi.to_string();
            }
            cy += FIELD_H + 14.0;
            // name
            cy = label(ui, x, cy, "Area name");
            kit::text_field(
                ui,
                Rect::from_min_size(Pos2::new(x, cy), Vec2::new(w, FIELD_H)),
                &mut self.name,
                "e.g. Landing site north",
                false,
                "tilesname",
            );
            cy += FIELD_H;
            cy = hint(ui, x, cy, w, "Optional. Defaults to the area's centre coordinates.") + 14.0;
            // bounds
            let bl = style::galley(ui, "REGION (CURRENT VIEW)", 10.0, Weight::W600, style::text_3(), 0.7);
            ui.painter().galley(Pos2::new(x, cy + 2.0), bl, style::text_3());
            let recapture = Rect::from_min_size(Pos2::new(x + w - 28.0, cy - 6.0), Vec2::splat(28.0));
            if kit::icon_btn(
                ui,
                recapture,
                "locate-fixed",
                "Recapture current view",
                false,
                "tilesrecapture",
            ) {
                action = Some(10);
            }
            cy += 17.0;
            let text = match &self.captured {
                Some((b, _)) => format!("W {:.2}  S {:.2}  E {:.2}  N {:.2}", b.w, b.s, b.e, b.n),
                None => "Map not ready".to_string(),
            };
            let g = style::galley_mono(ui, &text, 11.0, style::text_2());
            ui.painter().galley(Pos2::new(x, cy), g.clone(), style::text_2());
            cy += g.size().y.max(11.0 * 1.4) + 14.0;
            // zoom
            let half = (w - 10.0) / 2.0;
            let cap = self.cap_for(&self.source);
            for (i, (name, text)) in [("Min zoom", &mut self.min_text), ("Max zoom", &mut self.max_text)]
                .into_iter()
                .enumerate()
            {
                let fx = x + i as f32 * (half + 10.0);
                label(ui, fx, cy, name);
                let r = kit::text_field(
                    ui,
                    Rect::from_min_size(Pos2::new(fx, cy + 15.0), Vec2::new(half, FIELD_H)),
                    text,
                    "",
                    true,
                    ("tileszoom", i),
                );
                if r.lost_focus() {
                    *text = clamp_int(text, 0, cap).to_string();
                }
            }
            cy += 15.0 + FIELD_H + 14.0;
            // elevation and buildings
            if self.default_terrain().is_some() {
                cy = switch_row(
                    ui,
                    x,
                    cy,
                    w,
                    "Elevation (3D terrain)",
                    "Downloads the height model for the same area, so 3D mode keeps its relief offline.",
                    &mut self.terrain,
                    "tilesterrain",
                ) + 14.0;
            }
            cy = switch_row(
                ui,
                x,
                cy,
                w,
                "Buildings (3D)",
                "Fetches the OpenStreetMap buildings of the area in the background, nearest the centre first, so 3D shows them offline.",
                &mut self.buildings,
                "tilesbuildings",
            ) + 14.0;
            // estimate
            let (imagery, count) = self.estimate();
            let est = Rect::from_min_size(
                Pos2::new(x, cy),
                Vec2::new(w, 8.0 * 2.0 + 13.0 * 1.21 + 1.0 + 10.0 * 1.21 + 2.0),
            );
            ui.painter().rect_filled(est, style::RADIUS_BTN, style::glass(0.6));
            ui.painter().rect_stroke(
                est,
                style::RADIUS_BTN,
                Stroke::new(1.0, style::tint(0.09)),
                egui::StrokeKind::Inside,
            );
            let el = style::galley(ui, "ESTIMATE", 10.0, Weight::W600, style::text_3(), 0.7);
            ui.painter().galley(
                Pos2::new(est.left() + 10.0, est.center().y - el.size().y / 2.0),
                el,
                style::text_3(),
            );
            let (vt, st) = if self.captured.is_some() {
                (
                    format!("\u{2248} {} tiles", model::fmt_count(count)),
                    format!("~ {}", model::fmt_size(count as f64 * model::AVG_TILE_KB)),
                )
            } else {
                ("\u{2014}".to_string(), String::new())
            };
            let vg = style::galley(ui, &vt, 13.0, Weight::W700, style::accent_bright(), 0.0);
            let sg = style::galley(ui, &st, 10.0, Weight::W400, style::text_3(), 0.0);
            let blockh = vg.size().y + 1.0 + sg.size().y;
            let top = est.center().y - blockh / 2.0;
            ui.painter().galley(
                Pos2::new(est.right() - 10.0 - vg.size().x, top),
                vg.clone(),
                style::accent_bright(),
            );
            ui.painter().galley(
                Pos2::new(est.right() - 10.0 - sg.size().x, top + vg.size().y + 1.0),
                sg,
                style::text_3(),
            );
            cy = est.bottom();
            let too_big = imagery > self.max_tiles();
            if !self.running {
                if too_big {
                    self.message = Some((
                        Kind::Err,
                        format!(
                            "Too many tiles for one download: {}, the limit is {}. Choose a smaller area or a lower maximum zoom.",
                            model::fmt_count(imagery),
                            model::fmt_count(self.max_tiles())
                        ),
                    ));
                } else if count > model::BIG_JOB_TILES {
                    self.message = Some((Kind::Warn, "Very large region. Consider a smaller zoom range.".into()));
                } else if matches!(self.message, Some((Kind::Err | Kind::Warn, _)))
                    && self.chain.is_empty()
                    && self.results.is_empty()
                {
                    self.message = None;
                }
            }
            cy += 14.0;
            if self.running {
                let bar = Rect::from_min_size(Pos2::new(x, cy), Vec2::new(w, 8.0));
                ui.painter().rect_filled(bar, 4, style::surface_3());
                ui.painter()
                    .rect_stroke(bar, 4, Stroke::new(1.0, style::border_soft()), egui::StrokeKind::Inside);
                if self.progress.1 > 0 {
                    let f = (self.progress.0 as f32 / self.progress.1 as f32).clamp(0.0, 1.0);
                    ui.painter().rect_filled(
                        Rect::from_min_size(bar.min, Vec2::new(bar.width() * f, 8.0)),
                        4,
                        style::accent(),
                    );
                }
                cy = bar.bottom() + 6.0;
                let t = if self.progress.1 > 0 {
                    format!("{} / {} tiles", self.progress.0, self.progress.1)
                } else {
                    "Starting\u{2026}".into()
                };
                let pg = style::galley(ui, &t, 12.0, Weight::W400, style::text_2(), 0.0);
                ui.painter().galley(Pos2::new(x, cy), pg.clone(), style::text_2());
                cy += pg.size().y + 14.0;
            }
            if let Some((kind, text)) = &self.message {
                if !text.is_empty() {
                    let color = match kind {
                        Kind::Ok => style::healthy(),
                        Kind::Warn => style::warning(),
                        Kind::Err => style::critical(),
                        Kind::Info => style::nav(),
                    };
                    let g = kit::wrapped_weight(ui, text, 12.0, Weight::W400, color, w, 1.4);
                    ui.painter().galley(Pos2::new(x, cy), g.clone(), color);
                    cy += g.size().y + 14.0;
                }
            }
            // downloaded areas
            ui.painter().hline(
                egui::Rangef::new(x, x + w),
                cy + 0.5,
                Stroke::new(1.0, style::border_soft()),
            );
            cy += 1.0 + 6.0;
            let hg = style::galley(ui, "DOWNLOADED AREAS", 10.0, Weight::W600, style::text_3(), 0.7);
            ui.painter().galley(Pos2::new(x, cy), hg.clone(), style::text_3());
            if !self.regions.is_empty() {
                let n = self.regions.len();
                let cg = style::galley(
                    ui,
                    &format!("{n} area{}", if n == 1 { "" } else { "s" }),
                    10.0,
                    Weight::W400,
                    style::text_3(),
                    0.0,
                );
                ui.painter()
                    .galley(Pos2::new(x + w - cg.size().x, cy), cg, style::text_3());
            }
            cy += hg.size().y + 12.0;
            if self.regions.is_empty() {
                let g = kit::wrapped_weight(
                    ui,
                    "Nothing downloaded yet. Frame an area on the map and download it.",
                    11.0,
                    Weight::W400,
                    style::text_3(),
                    w - 4.0,
                    1.5,
                );
                ui.painter()
                    .galley(Pos2::new(x + 2.0, cy + 6.0), g.clone(), style::text_3());
                cy += g.size().y + 12.0;
            } else {
                let row_h = 4.0 * 2.0 + 5.0 * 2.0 + 12.0 * 1.21 + 1.0 + 10.0 * 1.21 + 2.0;
                let full = self.regions.len() as f32 * (row_h + 6.0) - 6.0;
                let room = (height - cy - 140.0).max(row_h * 2.0);
                let list_h = full.min(room);
                cy = self.region_list(ui, x, cy, w, list_h, full, row_h, &mut action, &mut close) + 14.0;
            }
            // actions
            let running = self.running;
            let can = self.captured.is_some() && !too_big && !self.source.is_empty() && !running;
            let (hit, bottom) = modal::actions(
                ui,
                x,
                cy + 4.0,
                w,
                vec![
                    Button::new("Download", if can { Variant::Primary } else { Variant::Inert })
                        .icon("download")
                        .enabled(can),
                    Button::new("Cancel", Variant::Danger).icon("x").enabled(running),
                ],
            );
            // the web puts Download first and Cancel second, rightmost last
            if let Some(i) = hit {
                action = Some(i);
            }
            bottom
        });
        let server = map.server().clone();
        match action {
            Some(0) => self.start_download(ctx, map),
            Some(1) => self.cancel(ctx, &server),
            Some(10) => self.capture(map, area),
            Some(100) => {}
            _ => {}
        }
        if let Some(b) = self.scroll_to.take() {
            let z = fit::fit_zoom(&b, f64::from(area.width()), f64::from(area.height()), 60.0, 22.0);
            let (cx, cy) = b.centre();
            map.go_to(cx, cy, z);
            close = true;
        }
        if close_asked || close {
            self.close();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn region_list(
        &mut self,
        ui: &mut egui::Ui,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        full: f32,
        row_h: f32,
        action: &mut Option<usize>,
        close: &mut bool,
    ) -> f32 {
        let area = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
        let regions = self.regions.clone();
        let mut rename_to: Option<(String, String, String)> = None;
        let mut delete: Option<(String, String)> = None;
        egui::ScrollArea::vertical()
            .id_salt("tilesregions")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(w, full), egui::Sense::hover());
                let painter = ui.painter_at(area);
                for (i, r) in regions.iter().enumerate() {
                    let id = r.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                    let source = r.get("source").and_then(Value::as_str).unwrap_or("").to_string();
                    let name = r.get("name").and_then(Value::as_str).unwrap_or("Region").to_string();
                    let running = r.get("state").and_then(Value::as_str) == Some("running");
                    let rr = Rect::from_min_size(
                        Pos2::new(rect.left(), rect.top() + i as f32 * (row_h + 6.0)),
                        Vec2::new(w, row_h),
                    );
                    let hot = ui.rect_contains_pointer(rr);
                    painter.rect_filled(rr, style::RADIUS_BTN, style::glass(if hot { 0.8 } else { 0.6 }));
                    painter.rect_stroke(
                        rr,
                        style::RADIUS_BTN,
                        Stroke::new(1.0, if running { style::warning() } else { style::tint(0.09) }),
                        egui::StrokeKind::Inside,
                    );
                    let tools_w = 2.0 * 28.0 + 2.0;
                    let main = Rect::from_min_max(
                        Pos2::new(rr.left() + 10.0, rr.top()),
                        Pos2::new(rr.right() - 6.0 - tools_w - 8.0, rr.bottom()),
                    );
                    if self.rename.as_ref().is_some_and(|(rid, _, _)| *rid == id) {
                        let edit_rect = Rect::from_min_max(
                            Pos2::new(main.left(), rr.top() + 4.0),
                            Pos2::new(rr.right() - 8.0 - 28.0 - 2.0, rr.bottom() - 4.0),
                        );
                        if let Some((_, _, text)) = &mut self.rename {
                            let resp =
                                kit::text_field(ui, edit_rect, text, "Name for this area", false, ("tilesrename", i));
                            if !resp.has_focus() && !resp.lost_focus() {
                                resp.request_focus();
                            }
                            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                            if enter {
                                rename_to = Some((id.clone(), source.clone(), text.trim().to_string()));
                            } else if esc || (resp.lost_focus() && !enter) {
                                self.rename = None;
                            }
                        }
                        continue;
                    }
                    if self.confirm.as_deref() == Some(id.as_str()) {
                        let g = style::galley(
                            ui,
                            &format!(
                                "Delete \"{name}\" and its cached tiles? Tiles shared with another area are kept."
                            ),
                            11.0,
                            Weight::W400,
                            style::text_2(),
                            0.0,
                        );
                        painter.with_clip_rect(main).galley(
                            Pos2::new(main.left(), rr.center().y - g.size().y / 2.0),
                            g,
                            style::text_2(),
                        );
                        let yes = Rect::from_min_size(
                            Pos2::new(rr.right() - 6.0 - 28.0, rr.center().y - 14.0),
                            Vec2::splat(28.0),
                        );
                        let no = yes.translate(Vec2::new(-30.0, 0.0));
                        if kit::icon_btn(ui, yes, "trash-2", "Delete", true, ("tilesyes", i)) {
                            delete = Some((id.clone(), source.clone()));
                        }
                        if kit::icon_btn(ui, no, "x", "Keep it", false, ("tilesno", i)) {
                            self.confirm = None;
                        }
                        continue;
                    }
                    let resp = ui.interact(main, ui.scope_id().with(("tilesmain", i)), egui::Sense::click());
                    resp.clone().on_hover_text("Show this area on the map");
                    let ng = style::galley(ui, &name, 12.0, Weight::W600, style::text_1(), 0.0);
                    let meta = model::region_meta(r, &self.label_by_id(&source));
                    let mc = if running { style::warning() } else { style::text_3() };
                    let mg = style::galley(ui, &meta, 10.0, Weight::W400, mc, 0.0);
                    let th = ng.size().y + 1.0 + mg.size().y;
                    let clip = painter.with_clip_rect(main);
                    clip.galley(
                        Pos2::new(main.left(), rr.center().y - th / 2.0),
                        ng.clone(),
                        style::text_1(),
                    );
                    clip.galley(
                        Pos2::new(main.left(), rr.center().y - th / 2.0 + ng.size().y + 1.0),
                        mg,
                        mc,
                    );
                    if resp.clicked() {
                        if let Some(b) = r.get("bounds").and_then(|b| {
                            let n = |k: &str| b.get(k).and_then(Value::as_f64);
                            Some(Bounds {
                                w: n("w")?,
                                s: n("s")?,
                                e: n("e")?,
                                n: n("n")?,
                            })
                        }) {
                            self.scroll_to = Some(b);
                        }
                    }
                    let del = Rect::from_min_size(
                        Pos2::new(rr.right() - 6.0 - 28.0, rr.center().y - 14.0),
                        Vec2::splat(28.0),
                    );
                    let ren = del.translate(Vec2::new(-30.0, 0.0));
                    if kit::icon_btn(ui, ren, "pencil", "Rename this area", false, ("tilesren", i)) {
                        self.rename = Some((id.clone(), source.clone(), name.clone()));
                    }
                    if kit::icon_btn(
                        ui,
                        del,
                        "trash-2",
                        "Delete this area and its tiles",
                        false,
                        ("tilesdel", i),
                    ) {
                        self.confirm = Some(id.clone());
                    }
                }
            });
        let server = self.server_hint();
        if let (Some((id, source, name)), Some(server)) = (rename_to, server.clone()) {
            self.rename = None;
            if !name.is_empty() {
                let ctx = ui.ctx().clone();
                self.work.push(Pending::spawn(&ctx, move || {
                    http::post_json(
                        &server,
                        "/api/tiles/regions/rename",
                        &json!({ "source": source, "id": id, "name": name }),
                    )
                }));
            }
        }
        if let (Some((id, source)), Some(server)) = (delete, server) {
            self.confirm = None;
            let ctx = ui.ctx().clone();
            self.work.push(Pending::spawn(&ctx, move || {
                http::post_json(
                    &server,
                    "/api/tiles/regions/remove",
                    &json!({ "source": source, "id": id, "delete_tiles": true }),
                )
            }));
        }
        let _ = (action, close);
        area.bottom()
    }

    fn server_hint(&self) -> Option<Server> {
        self.server.clone()
    }
}

fn label(ui: &egui::Ui, x: f32, y: f32, text: &str) -> f32 {
    let g = style::galley(ui, &text.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.7);
    let h = g.size().y;
    ui.painter().galley(Pos2::new(x, y), g, style::text_3());
    y + h + 5.0
}

fn hint(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let g = kit::wrapped_weight(ui, text, 10.5, Weight::W400, style::text_3(), w, 1.4);
    ui.painter().galley(Pos2::new(x, y + 5.0), g.clone(), style::text_3());
    y + 5.0 + g.size().y
}

#[allow(clippy::too_many_arguments)]
fn switch_row(ui: &mut egui::Ui, x: f32, y: f32, w: f32, caption: &str, note: &str, on: &mut bool, id: &str) -> f32 {
    let lg = style::galley(ui, caption, 12.0, Weight::W600, style::text_1(), 0.0);
    ui.painter()
        .galley(Pos2::new(x, y + (22.0 - lg.size().y) / 2.0), lg, style::text_1());
    if kit::switch(ui, Pos2::new(x + w - 38.0, y), *on, true, id) {
        *on = !*on;
    }
    hint(ui, x, y + 22.0, w - 50.0, note)
}
