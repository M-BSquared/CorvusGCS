//! Flight Review and Telemetry Review (`buildReview`, `buildTlogReview`,
//! `renderReview` of `python/src/js/analysis.js`): the backend reads one log into a
//! summary, the flight modes, findings, plots and the aircraft's own
//! messages, and this draws them in that order. The ground track is drawn as
//! a chart only; the web's Satellite switch over imagery is not in the native
//! page yet.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use super::plot::{self, Band, Plot, PlotState};
use super::{lists, model, AnalysisPage, View};
use crate::data::units::Units;
use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::ui::card::{self, Look};
use crate::ui::form::{self, Form, Tone};
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::style::{self, Weight};

/// One log read into what the page shows.
pub struct ReviewData {
    pub summary: Value,
    pub modes: Vec<Value>,
    pub bands: Vec<Band>,
    pub findings: Vec<Value>,
    pub plots: Vec<Plot>,
    pub states: Vec<PlotState>,
    pub groups: Vec<String>,
    pub messages: Vec<Value>,
}

fn list(v: &Value, key: &str) -> Vec<Value> {
    v.get(key).and_then(Value::as_array).cloned().unwrap_or_default()
}

impl ReviewData {
    /// The backend's review payload.
    pub fn from_json(v: &Value) -> ReviewData {
        let modes = list(v, "modes");
        let bands = modes
            .iter()
            .map(|m| {
                let name = m.get("mode").and_then(Value::as_str).unwrap_or("");
                Band {
                    start: m.get("start").and_then(Value::as_f64).unwrap_or(0.0),
                    end: m.get("end").and_then(Value::as_f64).unwrap_or(0.0),
                    label: name.to_string(),
                    color: model::mode_color(name),
                }
            })
            .collect();
        let plots: Vec<Plot> = list(v, "plots").iter().map(Plot::from_json).collect();
        let mut groups: Vec<String> = list(v, "groups")
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect();
        // a plot grouped under a name the backend did not list is still drawn
        for p in &plots {
            if !groups.contains(&p.group) {
                groups.push(p.group.clone());
            }
        }
        ReviewData {
            summary: v.get("summary").cloned().unwrap_or(Value::Null),
            modes,
            bands,
            findings: list(v, "findings"),
            states: plots.iter().map(|_| PlotState::default()).collect(),
            plots,
            groups,
            messages: list(v, "messages"),
        }
    }

    /// The findings that need reading, most severe first, and the notes.
    pub fn ranked(&self) -> (Ranked<'_>, Ranked<'_>) {
        let level = |f: &Value| f.get("level").and_then(Value::as_str).unwrap_or("ok").to_string();
        let mut all: Vec<(usize, &Value)> = self.findings.iter().enumerate().collect();
        all.sort_by_key(|(_, f)| model::finding_rank(&level(f)));
        all.into_iter().partition(|(_, f)| level(f) != "note")
    }
}

/// Findings with their place in the list.
pub type Ranked<'a> = Vec<(usize, &'a Value)>;

/// What the review views hold between frames.
pub struct Review {
    server: Server,
    load: Option<Pending<Result<Value, String>>>,
    label: String,
    note: (String, bool),
    data: Option<ReviewData>,
    pick: String,
    browse_open: bool,
    path: String,
    open: HashSet<usize>,
    notes_open: bool,
    msg_filter: String,
    group_y: HashMap<String, f32>,
}

impl Review {
    pub fn new(server: Server) -> Review {
        Review {
            server,
            load: None,
            label: String::new(),
            note: (String::new(), false),
            data: None,
            pick: String::new(),
            browse_open: false,
            path: String::new(),
            open: HashSet::new(),
            notes_open: false,
            msg_filter: "all".into(),
            group_y: HashMap::new(),
        }
    }

    /// Drops what was read, and abandons a read still in flight.
    pub fn clear(&mut self) {
        self.load = None;
        self.data = None;
        self.note = (String::new(), false);
        self.label.clear();
        self.open.clear();
        self.notes_open = false;
        self.browse_open = false;
    }

    fn start(&mut self, label: &str, work: impl FnOnce() -> Result<Value, String> + Send + 'static) {
        self.data = None;
        self.open.clear();
        self.notes_open = false;
        self.label = label.to_string();
        self.note = (String::new(), false);
        self.load = Some(Pending::spawn(&egui::Context::default(), work));
    }

    /// A ULog from the download folder.
    pub fn load_log(&mut self, name: &str) {
        self.pick = name.to_string();
        let (server, path) = (
            self.server.clone(),
            format!("/api/logs/review?file={}", http::encode_component(name)),
        );
        self.start(name, move || {
            http::get_json_wait(&server, &path, Duration::from_secs(300))
        });
    }

    /// A tlog Corvus recorded.
    pub fn load_tlog(&mut self, name: &str) {
        let (server, path) = (
            self.server.clone(),
            format!("/api/logs/tlog-review?file={}", http::encode_component(name)),
        );
        self.start(name, move || {
            http::get_json_wait(&server, &path, Duration::from_secs(300))
        });
    }

    /// A ULog from anywhere on this computer: the bytes are posted, so the
    /// backend never names a path of its own accord.
    pub fn load_file(&mut self, raw: &str) {
        let raw = raw.trim().trim_matches('"');
        let path = raw.strip_prefix("~/").map_or(raw.to_string(), |r| {
            format!("{}/{r}", std::env::var("HOME").unwrap_or_default())
        });
        let name = std::path::Path::new(&path)
            .file_name()
            .map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let server = self.server.clone();
        let label = if name.is_empty() {
            "the selected log".to_string()
        } else {
            name.clone()
        };
        self.start(&label, move || {
            let bytes = std::fs::read(&path).map_err(|_| "Could not send that file".to_string())?;
            let url = format!(
                "/api/logs/review/upload{}",
                if name.is_empty() {
                    String::new()
                } else {
                    format!("?name={}", http::encode_component(&name))
                }
            );
            let v = http::post_bytes(&server, &url, &bytes, Duration::from_secs(600))?;
            if v.get("ok").and_then(Value::as_bool) == Some(false) {
                return Err(v.get("error").and_then(Value::as_str).unwrap_or("Rejected").to_string());
            }
            Ok(v)
        });
    }

    /// Whether a review is on the page.
    pub fn has_data(&self) -> bool {
        self.data.is_some()
    }

    fn poll(&mut self) {
        if let Some(r) = self.load.as_ref().and_then(Pending::take) {
            self.load = None;
            match r {
                Ok(v) => self.data = Some(ReviewData::from_json(&v)),
                Err(e) => {
                    self.note = (
                        if e.is_empty() {
                            "Could not read that log".into()
                        } else {
                            e
                        },
                        true,
                    );
                }
            }
        }
    }
}

fn level_colors(level: &str) -> (Color32, Color32, Color32) {
    match level {
        "ok" => (style::healthy(), style::healthy_dark(), style::healthy()),
        "warning" => (style::warning(), style::warning_dark(), style::warning()),
        "critical" => (style::critical(), style::critical_dark(), style::critical()),
        _ => (style::text_2(), style::surface_2(), style::border_soft()),
    }
}

fn hexc(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

impl AnalysisPage {
    pub(super) fn review_view(&mut self, ui: &mut egui::Ui, f: &mut Form, tlog: bool, units: &Units) {
        let _ = units;
        self.review.poll();
        let loading = self.review.load.is_some();
        if loading {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        if self.back(ui, f, if tlog { "Recorded tlogs" } else { "Analysis" }) {
            self.set_view(if tlog { View::Tlog } else { View::Tiles });
            return;
        }
        // the picker card
        let slot = card::reserve(ui.painter());
        let top = f.y;
        let mut inner = Form::new(f.x + 19.0, top + 19.0, f.w - 38.0);
        let (_, th) = form::head_title(
            ui,
            Pos2::new(inner.x, inner.y),
            if tlog { "Telemetry Review" } else { "Flight Review" },
        );
        inner.y += th + 10.0;
        form::box_note(
            ui,
            &mut inner,
            if tlog {
                "What Corvus heard over the link, plotted. Lower rate than a ULog and without motor or per-IMU data, but it exists for every session, including the ones where the ULog never came off the aircraft, and it is the only log that can show the radio link itself."
            } else {
                "Pick a downloaded ULog, or open one from anywhere on this computer. Corvus reads it locally (nothing leaves this machine) and plots the handful of things that decide whether a flight was healthy."
            },
            Tone::Plain,
        );
        let mut go: Option<String> = None;
        let mut file: Option<String> = None;
        if !tlog {
            inner.skip(10.0);
            let saved = self.saved();
            let choices: Vec<Choice> = if saved.is_empty() {
                vec![Choice::new("", "No downloaded logs yet")]
            } else {
                saved
                    .iter()
                    .map(|s| {
                        let g = |k: &str| s.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                        Choice::new(
                            s.get("name").and_then(Value::as_str).unwrap_or(""),
                            format!(
                                "{}  \u{b7}  {}  \u{b7}  {}",
                                s.get("name").and_then(Value::as_str).unwrap_or(""),
                                model::format_stamp(g("mtime"), false, self.offset),
                                model::format_size(g("size"))
                            ),
                        )
                    })
                    .collect()
            };
            let usable = !saved.is_empty() && !loading;
            let b = Button::new("Review", Variant::Primary).icon("activity").enabled(usable);
            let bw = b.width(ui);
            let sel = Rect::from_min_size(
                Pos2::new(inner.x, inner.y),
                Vec2::new(inner.w - bw - 8.0, form::CONTROL_H),
            );
            let current = if saved
                .iter()
                .any(|s| s.get("name").and_then(Value::as_str) == Some(self.review.pick.as_str()))
            {
                self.review.pick.clone()
            } else {
                saved
                    .first()
                    .and_then(|s| s.get("name").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string()
            };
            if let Some(v) = kit::select(
                ui,
                sel,
                &choices,
                &current,
                "",
                false,
                "Downloaded ULog to review",
                "reviewfile",
            ) {
                self.review.pick = v;
            } else if self.review.pick != current {
                self.review.pick = current.clone();
            }
            let bh = b.height();
            if b.show(
                ui,
                Rect::from_min_size(
                    Pos2::new(sel.right() + 8.0, inner.y + (form::CONTROL_H - bh) / 2.0),
                    Vec2::new(bw, bh),
                ),
                "reviewgo",
            ) {
                go = Some(self.review.pick.clone());
            }
            inner.y += form::CONTROL_H + 6.0;
            let browse = Button::new("Open a .ulg from this computer", Variant::Ghost)
                .icon("folder-open")
                .small()
                .enabled(!loading);
            let bw2 = browse.width(ui);
            let bh2 = browse.height();
            if browse.show(
                ui,
                Rect::from_min_size(Pos2::new(inner.x, inner.y), Vec2::new(bw2, bh2)),
                "reviewbrowse",
            ) {
                self.review.browse_open = !self.review.browse_open;
            }
            inner.y += bh2;
            if self.review.browse_open {
                inner.skip(8.0);
                let ob = Button::new("Open", Variant::Secondary).enabled(!loading);
                let ow = ob.width(ui);
                let r = Rect::from_min_size(
                    Pos2::new(inner.x, inner.y),
                    Vec2::new(inner.w - ow - 8.0, form::CONTROL_H),
                );
                let resp = crate::ui::field::TextField::new(&mut self.review.path)
                    .hint("~/Downloads/flight.ulg")
                    .mono(true)
                    .pad(11.0, 11.0)
                    .show(ui, r, "reviewpath");
                let h = ob.height();
                let open = ob.show(
                    ui,
                    Rect::from_min_size(
                        Pos2::new(r.right() + 8.0, inner.y + (form::CONTROL_H - h) / 2.0),
                        Vec2::new(ow, h),
                    ),
                    "reviewopen",
                );
                if (open || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))))
                    && !self.review.path.trim().is_empty()
                {
                    file = Some(self.review.path.clone());
                }
                inner.y += form::CONTROL_H;
            }
        }
        inner.skip(10.0);
        let (note, err) = if loading {
            (format!("Reading {}\u{2026}", self.review.label), false)
        } else {
            self.review.note.clone()
        };
        let color = if err { style::critical() } else { style::text_2() };
        let g = kit::wrapped_weight(ui, &note, 11.5, Weight::W400, color, inner.w, 1.5);
        let gh = g.size().y.max(16.0);
        ui.painter().galley(Pos2::new(inner.x, inner.y), g, color);
        inner.y += gh;
        let rect = Rect::from_min_max(Pos2::new(f.x, top), Pos2::new(f.x + f.w, inner.y + 19.0));
        card::fill(ui.painter(), slot, rect, Look::Page);
        f.y = rect.bottom() + 12.0;

        if let Some(name) = go.filter(|n| !n.is_empty()) {
            self.review.load_log(&name);
        }
        if let Some(p) = file {
            self.review.load_file(&p);
        }
        // what was read
        if loading {
            let g = style::galley(
                ui,
                &format!("Reading {}. This takes a moment for a long flight.", self.review.label),
                12.0,
                Weight::W400,
                style::text_3(),
                0.0,
            );
            let r = f.rect(26.0 * 2.0 + g.size().y + 16.0);
            card::paint(ui.painter(), r, Look::Page);
            ui.painter().galley(
                Pos2::new(r.center().x - g.size().x / 2.0, r.top() + 26.0),
                g.clone(),
                style::text_3(),
            );
            let t = ui.input(|i| i.time) as f32;
            let sweep = (t * 0.77).fract();
            let line = Rect::from_center_size(
                Pos2::new(r.center().x, r.top() + 26.0 + g.size().y + 14.0),
                Vec2::new(200.0, 2.0),
            );
            ui.painter().rect_filled(line, 2, style::nav().gamma_multiply(0.16));
            let w = 90.0;
            let x = line.left() - w / 2.0 + (200.0 + w) * sweep;
            ui.painter().with_clip_rect(line).rect_filled(
                Rect::from_center_size(Pos2::new(x, line.center().y), Vec2::new(w, 2.0)),
                2,
                style::nav(),
            );
            f.y = r.bottom();
            return;
        }
        if self.review.data.is_some() {
            self.draw_review(ui, f);
        }
    }

    fn draw_review(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let Some(data) = self.review.data.as_mut() else {
            return;
        };
        let content_top = self.content_top;
        let mut toggle_finding: Option<usize> = None;
        let mut toggle_notes = false;
        let mut jump: Option<String> = None;

        // the summary card
        let slot = card::reserve(ui.painter());
        let top = f.y;
        let mut inner = Form::new(f.x + 19.0, top + 19.0, f.w - 38.0);
        let name = data
            .summary
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
            .unwrap_or("Flight Review");
        let (_, th) = form::head_title(ui, Pos2::new(inner.x, inner.y), name);
        inner.y += th + 10.0;
        // facts
        let facts = model::facts(&data.summary);
        let cols = (((inner.w + 10.0) / 130.0).floor() as usize).clamp(1, facts.len().max(1));
        let cw = (inner.w - 10.0 * (cols as f32 - 1.0)) / cols as f32;
        let fh = 9.5 * 1.21 + 2.0 + 12.0 * 1.21;
        for (i, (k, v)) in facts.iter().enumerate() {
            let (c, r) = ((i % cols) as f32, (i / cols) as f32);
            let at = Pos2::new(inner.x + c * (cw + 10.0), inner.y + r * (fh + 10.0 + 2.0));
            let l = style::galley(ui, &k.to_uppercase(), 9.5, Weight::W700, style::text_3(), 0.6);
            let lh = l.size().y;
            ui.painter().galley(at, l, style::text_3());
            let val = style::galley_mono(ui, v, 12.0, style::text_1());
            ui.painter()
                .galley(Pos2::new(at.x, at.y + lh + 2.0), val, style::text_1());
        }
        let rows = facts.len().div_ceil(cols);
        inner.y += rows as f32 * (fh + 2.0) + (rows as f32 - 1.0).max(0.0) * 10.0 + 10.0;
        // the mode strip, its ruler and key
        if !data.modes.is_empty() {
            let first = data.bands.first().map_or(0.0, |b| b.start);
            let last = data.bands.last().map_or(0.0, |b| b.end);
            let span = (last - first).max(0.001);
            let strip = Rect::from_min_size(Pos2::new(inner.x, inner.y), Vec2::new(inner.w, 22.0));
            let p = ui.painter().with_clip_rect(strip);
            let total: f64 = data.bands.iter().map(|b| (b.end - b.start).max(0.001)).sum();
            let mut x = strip.left();
            for b in &data.bands {
                let wd = (b.end - b.start).max(0.001);
                let w = (strip.width() as f64 * wd / total).max(2.0) as f32;
                let seg = Rect::from_min_size(Pos2::new(x, strip.top()), Vec2::new(w, 22.0));
                p.rect_filled(seg, 0, hexc(b.color));
                if wd / span > 0.12 {
                    let g = style::galley(
                        ui,
                        &b.label.to_uppercase(),
                        9.5,
                        Weight::W700,
                        Color32::from_rgb(0x10, 0x14, 0x1C),
                        0.4,
                    );
                    p.galley(seg.center() - g.size() / 2.0, g, Color32::BLACK);
                }
                x += w;
            }
            ui.painter().rect_stroke(
                strip,
                4,
                Stroke::new(1.0, style::border_soft()),
                egui::StrokeKind::Inside,
            );
            inner.y += 22.0 + 6.0;
            let ticks: Vec<String> = (0..=4)
                .map(|i| model::clock_text(first + span * f64::from(i) / 4.0))
                .collect();
            let gs: Vec<_> = ticks
                .iter()
                .map(|t| style::galley_mono(ui, t, 9.5, style::text_3()))
                .collect();
            let sum: f32 = gs.iter().map(|g| g.size().x).sum();
            let gap = ((inner.w - sum) / 4.0).max(0.0);
            let mut tx = inner.x;
            let gh = gs[0].size().y;
            for g in gs {
                let w = g.size().x;
                ui.painter().galley(Pos2::new(tx, inner.y), g, style::text_3());
                tx += w + gap;
            }
            inner.y += gh + 10.0;
            // key
            let (mut kx, mut ky) = (inner.x, inner.y);
            for (mode, secs) in model::mode_totals(&data.modes) {
                let n = style::galley(ui, &mode, 11.0, Weight::W400, style::text_2(), 0.0);
                let t = style::galley_mono(
                    ui,
                    &format!("{} s", crate::data::units::js_to_fixed(secs, 0)),
                    10.0,
                    style::text_3(),
                );
                let w = 8.0 + 5.0 + n.size().x + 5.0 + t.size().x;
                if kx > inner.x && kx + w > inner.x + inner.w {
                    kx = inner.x;
                    ky += n.size().y + 4.0;
                }
                let lh = n.size().y;
                ui.painter().rect_filled(
                    Rect::from_min_size(Pos2::new(kx, ky + (lh - 8.0) / 2.0), Vec2::splat(8.0)),
                    2,
                    hexc(model::mode_color(&mode)),
                );
                ui.painter()
                    .galley(Pos2::new(kx + 13.0, ky), n.clone(), style::text_2());
                ui.painter().galley(
                    Pos2::new(kx + 13.0 + n.size().x + 5.0, ky + (lh - t.size().y) / 2.0),
                    t,
                    style::text_3(),
                );
                kx += w + 14.0;
            }
            inner.y = ky + 11.0 * 1.21 + 10.0;
        }
        // the findings
        let (shown, notes) = data.ranked();
        for (idx, fnd) in &shown {
            if finding_row(
                ui,
                &mut inner,
                fnd,
                !data.findings.is_empty() && self.review.open.contains(idx),
            ) {
                toggle_finding = Some(*idx);
            }
            inner.skip(10.0);
        }
        if !notes.is_empty() {
            let label = if notes.len() == 1 {
                "1 more observation".to_string()
            } else {
                format!("{} more observations", notes.len())
            };
            let g = style::galley(ui, &label, 11.0, Weight::W400, style::text_3(), 0.2);
            let r = Rect::from_min_size(
                Pos2::new(inner.x, inner.y),
                Vec2::new(13.0 + 6.0 + g.size().x, g.size().y.max(13.0) + 4.0),
            );
            let resp = ui.interact(r, ui.scope_id().with("morenotes"), egui::Sense::click());
            let c = if resp.hovered() {
                style::text_2()
            } else {
                style::text_3()
            };
            crate::ui::icons::paint(
                ui,
                if self.review.notes_open {
                    "chevron-down"
                } else {
                    "chevron-right"
                },
                Rect::from_center_size(Pos2::new(r.left() + 6.5, r.center().y), Vec2::splat(13.0)),
                c,
            );
            ui.painter()
                .galley(Pos2::new(r.left() + 19.0, r.center().y - g.size().y / 2.0), g, c);
            toggle_notes = resp.clicked();
            inner.y = r.bottom() + 10.0;
            if self.review.notes_open {
                for (idx, fnd) in &notes {
                    if finding_row(ui, &mut inner, fnd, self.review.open.contains(idx)) {
                        toggle_finding = Some(*idx);
                    }
                    inner.skip(8.0);
                }
                inner.y += 2.0;
            }
        }
        let line = model::sensitivity_line(&data.summary);
        if !line.is_empty() {
            let g = style::galley(ui, &line, 11.0, Weight::W400, style::text_3(), 0.2);
            let h = g.size().y;
            ui.painter().galley(Pos2::new(inner.x, inner.y), g, style::text_3());
            inner.y += h + 10.0;
        }
        // jump chips
        let mut groups_with: Vec<(String, usize)> = data
            .groups
            .iter()
            .map(|g| (g.clone(), data.plots.iter().filter(|p| &p.group == g).count()))
            .filter(|g| g.1 > 0)
            .collect();
        if data.groups.len() > 1 {
            let (mut x, mut y) = (inner.x, inner.y);
            let mut line_h = 0.0f32;
            for (g, _) in &groups_with {
                let b = Button::new(g, Variant::Secondary).small();
                let (w, h) = (b.width(ui), b.height());
                if x > inner.x && x + w > inner.x + inner.w {
                    x = inner.x;
                    y += line_h + 6.0;
                    line_h = 0.0;
                }
                if b.show(
                    ui,
                    Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h)),
                    ("chip", g.as_str()),
                ) {
                    jump = Some(g.clone());
                }
                x += w + 6.0;
                line_h = line_h.max(h);
            }
            inner.y = y + line_h + 10.0;
        }
        let rect = Rect::from_min_max(Pos2::new(f.x, top), Pos2::new(f.x + f.w, inner.y - 10.0 + 19.0));
        card::fill(ui.painter(), slot, rect, Look::Page);
        f.y = rect.bottom() + 12.0;

        // the plots, group by group
        let vh = ui.ctx().content_rect().height();
        let mut seen: Vec<usize> = Vec::new();
        for (g, count) in groups_with.drain(..) {
            self.review.group_y.insert(g.clone(), f.y - content_top);
            let t = style::galley(ui, &g.to_uppercase(), 12.0, Weight::W700, style::text_1(), 0.8);
            let c = style::galley(ui, &model::plot_count(count), 10.5, Weight::W400, style::text_3(), 0.0);
            let h = t.size().y;
            ui.painter().galley(Pos2::new(f.x, f.y + 6.0), t, style::text_1());
            ui.painter().galley(
                Pos2::new(f.x + f.w - c.size().x, f.y + 6.0 + (h - c.size().y)),
                c,
                style::text_3(),
            );
            let y = f.y + 6.0 + h + 5.0;
            ui.painter().hline(
                egui::Rangef::new(f.x, f.x + f.w),
                y + 0.5,
                Stroke::new(1.0, style::border_soft()),
            );
            f.y = y + 1.0 + 12.0;
            let idxs: Vec<usize> = data
                .plots
                .iter()
                .enumerate()
                .filter(|(_, p)| p.group == g)
                .map(|(i, _)| i)
                .collect();
            for i in idxs {
                seen.push(i);
                plot_card(ui, f, data, i, vh);
            }
        }
        // a plot in no group is still drawn
        for i in 0..data.plots.len() {
            if !seen.contains(&i) {
                plot_card(ui, f, data, i, vh);
            }
        }
        // the aircraft's own messages
        let mut new_filter: Option<String> = None;
        messages_card(ui, f, &data.messages, &self.review.msg_filter, &mut new_filter);

        if let Some(i) = toggle_finding {
            if !self.review.open.remove(&i) {
                self.review.open.insert(i);
            }
        }
        if toggle_notes {
            self.review.notes_open = !self.review.notes_open;
        }
        if let Some(v) = new_filter {
            self.review.msg_filter = v;
        }
        if let Some(g) = jump {
            if let Some(y) = self.review.group_y.get(&g) {
                self.scroll_to = Some((*y - 12.0).max(0.0));
            }
        }
    }
}

/// One finding: icon, headline with its time and mode, and the reasoning
/// when it is open. Returns true when the headline was pressed.
fn finding_row(ui: &mut egui::Ui, f: &mut Form, fnd: &Value, open: bool) -> bool {
    let level = fnd.get("level").and_then(Value::as_str).unwrap_or("ok");
    let (color, fill, line) = level_colors(level);
    let text = fnd.get("text").and_then(Value::as_str).unwrap_or("");
    let detail = fnd.get("detail").and_then(Value::as_str).filter(|d| !d.is_empty());
    let when = fnd.get("t").and_then(Value::as_f64);
    let mode = fnd.get("mode").and_then(Value::as_str).filter(|m| !m.is_empty());
    let inner_w = f.w - 20.0 - 14.0 - 8.0;
    let g = kit::wrapped_weight(ui, text, 12.0, Weight::W400, color, inner_w - 24.0, 1.5);
    let mut head_h = g.size().y;
    let mut extra_w = 0.0;
    let tg = when.map(|t| style::galley_mono(ui, &model::clock_text(t), 10.0, color));
    if let Some(t) = &tg {
        extra_w += 8.0 + t.size().x;
    }
    let mg = mode
        .zip(when)
        .map(|(m, _)| style::galley(ui, &m.to_uppercase(), 9.0, Weight::W700, color, 0.4));
    if let Some(m) = &mg {
        extra_w += 8.0 + m.size().x + 12.0;
    }
    let _ = extra_w;
    head_h = head_h.max(14.0);
    let dg = detail.map(|d| kit::wrapped_weight(ui, d, 11.5, Weight::W400, color, inner_w - 12.0, 1.6));
    let detail_h = if open {
        dg.as_ref().map_or(0.0, |d| 4.0 + 2.0 + d.size().y)
    } else {
        0.0
    };
    let h = 16.0 + 2.0 + head_h + detail_h;
    let r = f.rect(h);
    ui.painter().rect_filled(r, style::RADIUS_BTN, fill);
    ui.painter()
        .rect_stroke(r, style::RADIUS_BTN, Stroke::new(1.0, line), egui::StrokeKind::Inside);
    let icon = match level {
        "ok" => "circle-check",
        "note" => "info",
        _ => "triangle-alert",
    };
    crate::ui::icons::paint(
        ui,
        icon,
        Rect::from_min_size(Pos2::new(r.left() + 11.0, r.top() + 10.0), Vec2::splat(14.0)),
        color,
    );
    let tx = r.left() + 11.0 + 14.0 + 8.0;
    let head = Rect::from_min_size(Pos2::new(tx, r.top() + 9.0), Vec2::new(r.right() - tx - 10.0, head_h));
    let resp = ui.interact(
        head,
        ui.scope_id().with(("finding", text, when.map(|t| t as i64))),
        if detail.is_some() {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let tw = g.size().x.min(inner_w - 24.0);
    ui.painter().galley(head.min, g, color);
    let mut hx = head.left() + tw + 8.0;
    let by = head.top() + 3.0;
    if let Some(t) = tg {
        let w = t.size().x;
        ui.painter().galley(Pos2::new(hx, by), t, color);
        hx += w + 8.0;
    }
    if let Some(m) = mg {
        let rr = Rect::from_min_size(Pos2::new(hx, by - 1.0), Vec2::new(m.size().x + 12.0, m.size().y + 2.0));
        ui.painter().rect_stroke(
            rr,
            255,
            Stroke::new(1.0, mode.map_or(style::border_soft(), |n| hexc(model::mode_color(n)))),
            egui::StrokeKind::Inside,
        );
        ui.painter().galley(rr.min + Vec2::new(6.0, 1.0), m, color);
        hx += rr.width() + 8.0;
    }
    if detail.is_some() {
        crate::ui::icons::paint(
            ui,
            if open { "chevron-up" } else { "chevron-down" },
            Rect::from_center_size(
                Pos2::new((hx + 6.5).min(r.right() - 18.0), head.top() + 9.0),
                Vec2::splat(13.0),
            ),
            color,
        );
    }
    if let (true, Some(d)) = (open, dg) {
        let y = head.bottom() + 6.0;
        ui.painter()
            .vline(tx + 1.0, y..=y + d.size().y, Stroke::new(2.0, color));
        ui.painter()
            .galley(Pos2::new(tx + 12.0, y), d, color.gamma_multiply(0.88));
    }
    f.y = r.bottom();
    resp.clicked()
}

/// One plot in its card.
fn plot_card(ui: &mut egui::Ui, f: &mut Form, data: &mut ReviewData, i: usize, vh: f32) {
    let p = data.plots[i].clone();
    let slot = card::reserve(ui.painter());
    let top = f.y;
    let mut inner = Form::new(f.x + 19.0, top + 19.0, f.w - 38.0);
    let (_, th) = form::head_title(ui, Pos2::new(inner.x, inner.y), &p.title);
    inner.y += th + 8.0;
    let h = if p.equal { 460.0f32.min(vh * 0.7) } else { 268.0 };
    let rect = inner.rect(h);
    if ui.is_rect_visible(rect) {
        let bands = if p.is_time() { data.bands.clone() } else { Vec::new() };
        plot::show(ui, rect, &p, &bands, &mut data.states[i], i);
    }
    inner.y += h;
    if !p.note.is_empty() {
        inner.skip(8.0);
        form::note(ui, &mut inner, &p.note);
    }
    let r = Rect::from_min_max(Pos2::new(f.x, top), Pos2::new(f.x + f.w, inner.y + 19.0));
    card::fill(ui.painter(), slot, r, Look::Page);
    f.y = r.bottom() + 12.0;
}

/// The aircraft's own commentary with a severity filter.
fn messages_card(ui: &mut egui::Ui, f: &mut Form, messages: &[Value], filter: &str, new_filter: &mut Option<String>) {
    let slot = card::reserve(ui.painter());
    let top = f.y;
    let mut inner = Form::new(f.x + 19.0, top + 19.0, f.w - 38.0);
    let (_, th) = form::head_title(ui, Pos2::new(inner.x, inner.y), "Flight log messages");
    let choices = [
        Choice::new("all", "All messages"),
        Choice::new("warning", "Warnings and worse"),
        Choice::new("error", "Errors only"),
    ];
    let sel = Rect::from_min_size(
        Pos2::new(inner.x + inner.w - 190.0, inner.y + (th - form::CONTROL_H) / 2.0),
        Vec2::new(190.0, form::CONTROL_H),
    );
    *new_filter = kit::select(
        ui,
        sel,
        &choices,
        filter,
        "",
        false,
        "Minimum message severity",
        "msgfilter",
    );
    inner.y += th.max(form::CONTROL_H) + 10.0;
    let limit = model::message_limit(filter);
    let shown: Vec<&Value> = messages
        .iter()
        .filter(|m| model::message_rank(m.get("level").and_then(Value::as_str).unwrap_or("")) <= limit)
        .collect();
    let bottom = lists::list_box(
        ui,
        Pos2::new(inner.x, inner.y),
        inner.w,
        320.0,
        "messages",
        style::terminal_bg(),
        |ui, left, top, w| {
            if shown.is_empty() {
                let t = if messages.is_empty() {
                    "The aircraft logged no messages."
                } else {
                    "Nothing at that severity."
                };
                let g = style::galley_mono(ui, t, 11.5, style::text_3());
                let h = g.size().y + 8.0;
                ui.painter()
                    .galley(Pos2::new(left + 5.0, top + 4.0), g, style::text_3());
                return h;
            }
            let mut y = top + 3.0;
            for m in &shown {
                let level = m.get("level").and_then(Value::as_str).unwrap_or("info");
                let lc = match level {
                    "warning" => style::warning(),
                    "critical" | "error" | "emergency" | "alert" => style::critical(),
                    "info" => style::nav(),
                    "notice" => style::healthy(),
                    _ => style::text_3(),
                };
                let mut x = left + 5.0;
                if let Some(t) = m.get("t").and_then(Value::as_f64) {
                    let g = style::galley_mono(ui, &model::clock_text(t), 9.5, style::text_3());
                    ui.painter()
                        .galley(Pos2::new(x + 34.0 - g.size().x, y + 2.0), g, style::text_3());
                }
                x += 34.0 + 8.0;
                let lg = style::galley(ui, &level.to_uppercase(), 9.0, Weight::W700, lc, 0.5);
                ui.painter().galley(Pos2::new(x, y + 3.0), lg, lc);
                x += 44.0 + 8.0;
                let text = m.get("text").and_then(Value::as_str).unwrap_or("");
                let mut job = egui::text::LayoutJob::single_section(
                    text.to_string(),
                    egui::TextFormat {
                        font_id: style::mono(11.5),
                        color: style::terminal_text(),
                        line_height: Some(17.25),
                        ..Default::default()
                    },
                );
                job.wrap.max_width = (left + w - x - 5.0).max(40.0);
                job.wrap.break_anywhere = true;
                let g = ui.painter().layout_job(job);
                let h = g.size().y;
                ui.painter().galley(Pos2::new(x, y), g, style::terminal_text());
                y += h.max(17.25) + 2.0;
            }
            y - top + 3.0
        },
    );
    let r = Rect::from_min_max(Pos2::new(f.x, top), Pos2::new(f.x + f.w, bottom + 19.0));
    card::fill(ui.painter(), slot, r, Look::Page);
    f.y = r.bottom() + 12.0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Value {
        json!({
            "summary": {"name": "log_3.ulg", "duration_s": 100},
            "modes": [{"mode": "Manual", "start": 0, "end": 10}, {"mode": "Mission", "start": 10, "end": 100}],
            "findings": [
                {"level": "note", "text": "Logged at 250 Hz."},
                {"level": "ok", "text": "Healthy."},
                {"level": "critical", "text": "Motor 3 saturated.", "detail": "Why.", "t": 42.0, "mode": "Mission"},
                {"level": "warning", "text": "Vibration high."}],
            "groups": ["Position", "Motors"],
            "plots": [
                {"title": "Altitude", "group": "Position", "unit": "m", "series": [{"name": "alt", "x": [0, 1], "y": [1, 2]}]},
                {"title": "Thrust", "group": "Motors", "series": []},
                {"title": "Strays", "group": "Other", "series": []}],
            "messages": [{"level": "info", "text": "Armed", "t": 3.0}],
        })
    }

    #[test]
    fn a_review_is_read_into_its_parts() {
        let d = ReviewData::from_json(&payload());
        assert_eq!(d.plots.len(), 3);
        assert_eq!(d.states.len(), 3);
        assert_eq!(d.bands[1].label, "Mission");
        assert_eq!(d.bands[1].color, 0x45D483);
        // a group the backend did not list is still reached
        assert_eq!(d.groups, ["Position", "Motors", "Other"]);
        assert_eq!(d.messages.len(), 1);
    }

    #[test]
    fn findings_come_by_severity_with_the_notes_apart() {
        let d = ReviewData::from_json(&payload());
        let (shown, notes) = d.ranked();
        let levels: Vec<&str> = shown.iter().map(|(_, f)| f["level"].as_str().unwrap()).collect();
        assert_eq!(levels, ["critical", "warning", "ok"]);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].0, 0);
    }

    #[test]
    fn nothing_read_is_an_empty_review() {
        let d = ReviewData::from_json(&json!({}));
        assert!(d.plots.is_empty() && d.findings.is_empty() && d.groups.is_empty());
        assert_eq!(model::facts(&d.summary)[4].1, "none");
    }
}
