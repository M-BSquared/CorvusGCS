//! The Analysis page (`python/src/js/analysis.js`): live telemetry, the download
//! folder, and four places to go: the logs on the vehicle, the tlogs Corvus
//! recorded, Flight Review and Live Messages. Opening one replaces the page
//! and Back is the way out, as on Setup.

mod lists;
pub mod model;
pub mod plot;
mod review;
mod scope;
pub mod trackmap;

use std::collections::BTreeSet;
use std::time::Duration;

use eframe::egui::{self, Pos2, Rect, Vec2};
use serde_json::{json, Value};

use crate::data::units::Units;
use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::ui::card::{self, Look};
use crate::ui::form::{self, Form};
use crate::ui::kit::{Button, Variant};
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

/// The views of the page.
#[derive(Debug, Clone, PartialEq)]
enum View {
    Tiles,
    Ulog,
    Tlog,
    Review(Option<String>),
    TlogReview(String),
    Scope,
}

/// `POLL_BUSY_MS` and `POLL_IDLE_MS`.
const POLL_BUSY: f64 = 0.7;
const POLL_IDLE: f64 = 5.0;

/// A request whose answer the page acts on.
struct Job {
    what: &'static str,
    answer: Pending<Result<Value, String>>,
}

/// The page's state.
pub struct AnalysisPage {
    server: Server,
    view: View,
    status: Value,
    status_load: Option<Pending<Result<Value, String>>>,
    status_at: f64,
    selected: BTreeSet<i64>,
    ulog_sort: String,
    tlog_sort: String,
    dir_text: String,
    dir_touched: bool,
    dir_note: (String, bool),
    jobs: Vec<Job>,
    in_flight: bool,
    ulog_line: (String, bool),
    erase_open: bool,
    scope: scope::Scope,
    review: review::Review,
    errors: Vec<String>,
    scroll_to: Option<f32>,
    scroll_top: bool,
    offset: i64,
    content_top: f32,
    deferred_scroll: Option<f32>,
}

impl AnalysisPage {
    pub fn new(server: Server) -> AnalysisPage {
        AnalysisPage {
            server: server.clone(),
            view: View::Tiles,
            status: Value::Null,
            status_load: None,
            status_at: f64::NEG_INFINITY,
            selected: BTreeSet::new(),
            ulog_sort: "time-desc".into(),
            tlog_sort: "time-desc".into(),
            dir_text: String::new(),
            dir_touched: false,
            dir_note: (String::new(), false),
            jobs: Vec::new(),
            in_flight: false,
            ulog_line: (String::new(), false),
            erase_open: false,
            scope: scope::Scope::default(),
            review: review::Review::new(server),
            errors: Vec::new(),
            scroll_to: None,
            scroll_top: false,
            offset: crate::panel::console::local_offset(),
            content_top: 0.0,
            deferred_scroll: None,
        }
    }

    /// Leaving the page drops the sub-page, its timers and its reviews, as
    /// the web page is destroyed on every exit.
    pub fn reset(&mut self) {
        self.view = View::Tiles;
        self.scope = scope::Scope::default();
        self.review.clear();
        self.selected.clear();
        self.erase_open = false;
        self.status_at = f64::NEG_INFINITY;
        self.dir_touched = false;
    }

    /// Opens a view from the `--page` flag.
    pub fn open(&mut self, name: &str) {
        self.view = match name {
            "ulog" => View::Ulog,
            "tlog" => View::Tlog,
            "review" => View::Review(None),
            "scope" => View::Scope,
            _ => View::Tiles,
        };
    }

    /// Opens the page scrolled by `px` (the --scroll flag).
    pub fn scroll(&mut self, px: f32) {
        self.deferred_scroll = Some(px);
    }

    /// Errors for the notification board.
    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    fn set_view(&mut self, v: View) {
        self.scope = scope::Scope::default();
        self.review.clear();
        if let View::Review(Some(f)) = &v {
            self.review.load_log(&f.clone());
        }
        if let View::TlogReview(f) = &v {
            self.review.load_tlog(&f.clone());
        }
        self.view = v;
        self.scroll_top = true;
    }

    fn logs(&self) -> Vec<Value> {
        self.status
            .get("logs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn tlogs(&self) -> Vec<Value> {
        self.status
            .get("tlogs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn saved(&self) -> Vec<Value> {
        self.status
            .get("saved")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn connected(&self) -> bool {
        self.status.get("connected").and_then(Value::as_bool).unwrap_or(false)
    }

    fn job(&mut self, ctx: &egui::Context, what: &'static str, path: &str, body: Value) {
        let server = self.server.clone();
        let path = path.to_string();
        self.in_flight = true;
        self.jobs.push(Job {
            what,
            answer: Pending::spawn(ctx, move || http::post_json(&server, &path, &body)),
        });
    }

    /// The status poll and the answers of the jobs.
    fn poll(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if let Some(r) = self.status_load.as_ref().and_then(Pending::take) {
            self.status_load = None;
            if let Ok(v) = r {
                if !self.dir_touched {
                    if let Some(d) = v.get("dir").and_then(Value::as_str) {
                        if self.dir_text != d {
                            self.dir_text = d.to_string();
                        }
                    }
                }
                self.status = v;
            }
        }
        let every = if model::busy(&self.status) {
            POLL_BUSY
        } else {
            POLL_IDLE
        };
        if self.status_load.is_none() && now - self.status_at >= every {
            self.status_at = now;
            let server = self.server.clone();
            self.status_load = Some(Pending::spawn(ctx, move || http::get_json(&server, "/api/logs/status")));
        }
        let mut i = 0;
        while i < self.jobs.len() {
            let Some(answer) = self.jobs[i].answer.take() else {
                i += 1;
                continue;
            };
            let job = self.jobs.remove(i);
            self.in_flight = !self.jobs.is_empty();
            self.status_at = f64::NEG_INFINITY;
            match (job.what, answer) {
                ("dir", Ok(v)) => {
                    self.dir_touched = false;
                    let warn = v.get("warning").and_then(Value::as_str).filter(|w| !w.is_empty());
                    let dir = v
                        .get("dir")
                        .and_then(Value::as_str)
                        .unwrap_or(&self.dir_text)
                        .to_string();
                    self.dir_note = (
                        warn.map_or_else(|| format!("Saved. Logs go to {dir}."), String::from),
                        false,
                    );
                }
                ("dir", Err(e)) => {
                    self.dir_note = (
                        if e.is_empty() {
                            "Could not use that folder".into()
                        } else {
                            e
                        },
                        true,
                    );
                }
                (what, Err(e)) => {
                    let fallback = match what {
                        "refresh" => "Could not list logs",
                        "download" => "Download refused",
                        "erase" => "Erase refused",
                        _ => "The vehicle did not answer",
                    };
                    let msg = if e.is_empty() { fallback.to_string() } else { e };
                    self.ulog_line = (msg.clone(), true);
                    self.errors.push(msg);
                }
                _ => {}
            }
        }
        if !self.jobs.is_empty() || self.status_load.is_some() || self.view != View::Tiles {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    /// Draws the page into `rect`.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, state: &Value, units: &Units, imagery: &trackmap::Imagery) {
        self.poll(ui.ctx());
        let ready = !matches!(self.view, View::Review(_) | View::TlogReview(_)) || self.review.has_data();
        if ready && self.deferred_scroll.is_some() {
            self.scroll_to = self.deferred_scroll.take();
        }
        pagekit::background(ui, rect);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.set_clip_rect(rect);
        let mut area = egui::ScrollArea::vertical()
            .id_salt("analysisscroll")
            .auto_shrink([false, false]);
        if std::mem::take(&mut self.scroll_top) {
            area = area.vertical_scroll_offset(0.0);
        } else if let Some(y) = self.scroll_to.take() {
            area = area.vertical_scroll_offset(y);
        }
        area.show(&mut child, |ui| {
            let top = ui.max_rect().top();
            self.content_top = top;
            let mut f = Form::new(
                rect.left() + pagekit::PAD_X,
                top + pagekit::PAD_Y,
                rect.width() - 2.0 * pagekit::PAD_X,
            );
            f.y = pagekit::header(ui, f.x, f.y, "Analysis", "Flight logs and telemetry analysis") + 28.0;
            match self.view.clone() {
                View::Tiles => self.landing(ui, &mut f, state, units),
                View::Ulog => self.ulog_view(ui, &mut f),
                View::Tlog => self.tlog_view(ui, &mut f),
                View::Review(_) => self.review_view(ui, &mut f, false, units, imagery),
                View::TlogReview(_) => self.review_view(ui, &mut f, true, units, imagery),
                View::Scope => self.scope_view(ui, &mut f),
            }
            ui.allocate_space(Vec2::new(rect.width() - 1.0, f.y + pagekit::PAD_Y - top));
        });
        if self.erase_open {
            self.erase_dialog(ui.ctx());
        }
    }

    /// Back, and the page's width kept: returns true when pressed.
    fn back(&mut self, ui: &mut egui::Ui, f: &mut Form, label: &str) -> bool {
        let (h, hit) = pagekit::back_button(ui, Pos2::new(f.x, f.y), label);
        f.y += h + 22.0;
        hit
    }

    fn landing(&mut self, ui: &mut egui::Ui, f: &mut Form, state: &Value, units: &Units) {
        // Current Telemetry
        f.y = pagekit::section_title(ui, f.x, f.y, f.w, "Current Telemetry") - 0.0;
        let tiles = model::telemetry_tiles(state, units);
        let cols = (((f.w + 10.0) / (150.0 + 10.0)).floor() as usize)
            .max(1)
            .min(tiles.len());
        let tw = (f.w - 10.0 * (cols as f32 - 1.0)) / cols as f32;
        let th = 12.0 * 2.0 + 2.0 + 6.0 + 6.0 + 13.0 * 1.21 + 20.0 * 1.1 + 14.0 + 1.0;
        let mut bottom = f.y;
        for (i, t) in tiles.iter().enumerate() {
            let (c, r) = ((i % cols) as f32, (i / cols) as f32);
            let rect = Rect::from_min_size(
                Pos2::new(f.x + c * (tw + 10.0), f.y + r * (th + 10.0)),
                Vec2::new(tw, th),
            );
            bottom = rect.bottom();
            tele_tile(ui, rect, t);
        }
        f.y = bottom;
        if state.get("connected").and_then(Value::as_bool) != Some(true) {
            f.y += 8.0;
            form::desc(ui, f, "Vehicle not connected.");
        }
        f.y += 12.0;

        // Flight logs
        f.y = pagekit::section_title(ui, f.x, f.y, f.w, "Flight logs");
        self.folder_row(ui, f);
        f.y += 12.0;
        let cols = (((f.w + 14.0) / (280.0 + 14.0)).floor() as usize).max(1);
        let tw = (f.w - 14.0 * (cols as f32 - 1.0)) / cols as f32;
        let th = 18.0 * 2.0 + (14.0 * 1.21f32 + 3.0 + 2.0 * 11.5 * 1.45).max(40.0);
        let items = [
            (
                View::Ulog,
                "hard-drive-download",
                "Vehicle logs (ULog)",
                if self.status.is_null() {
                    "PX4's own flight log, on the flight controller's SD card.".to_string()
                } else {
                    model::ulog_tile_desc(&self.status)
                },
            ),
            (
                View::Tlog,
                "file-clock",
                "Recorded tlogs",
                if self.status.is_null() {
                    "The MAVLink stream Corvus recorded on this laptop.".to_string()
                } else {
                    model::tlog_tile_desc(&self.status)
                },
            ),
            (
                View::Review(None),
                "activity",
                "Flight Review",
                if self.status.is_null() {
                    "Plot a downloaded ULog: motors, clipping, EKF, battery.".to_string()
                } else {
                    model::review_tile_desc(&self.status)
                },
            ),
            (
                View::Scope,
                "radar",
                "Live Messages",
                "Every MAVLink message on the link, live: rate, age and fields.".to_string(),
            ),
        ];
        let mut go = None;
        let mut bottom = f.y;
        for (i, (view, icon, title, desc)) in items.iter().enumerate() {
            let (c, r) = ((i % cols) as f32, (i / cols) as f32);
            let rect = Rect::from_min_size(
                Pos2::new(f.x + c * (tw + 14.0), f.y + r * (th + 14.0)),
                Vec2::new(tw, th),
            );
            bottom = rect.bottom();
            if pagekit::tile(ui, rect, icon, title, desc) {
                go = Some(view.clone());
            }
        }
        f.y = bottom;
        if let Some(v) = go {
            self.set_view(v);
        }
    }

    /// The slim `Save to` row and the note under it.
    fn folder_row(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let h = 6.0 * 2.0 + 2.0 + 25.0;
        let row = f.rect(h);
        card::paint(ui.painter(), row, Look::Quiet);
        crate::ui::icons::paint(
            ui,
            "folder",
            Rect::from_center_size(Pos2::new(row.left() + 11.0 + 7.0, row.center().y), Vec2::splat(14.0)),
            style::text_3(),
        );
        let label = style::galley(ui, "SAVE TO", 11.0, Weight::W600, style::text_3(), 0.3);
        let lx = row.left() + 11.0 + 14.0 + 8.0;
        ui.painter().galley(
            Pos2::new(lx, row.center().y - label.size().y / 2.0),
            label.clone(),
            style::text_3(),
        );
        let save = Button::new("Save", Variant::Secondary).small();
        let (bw, bh) = (save.width(ui), save.height());
        let ix = lx + label.size().x + 8.0;
        let input = Rect::from_min_max(
            Pos2::new(ix, row.top() + 6.0 + (h - 12.0 - 2.0 - 25.0) / 2.0 + 1.0),
            Pos2::new(row.right() - 11.0 - bw - 8.0, row.bottom() - 7.0),
        );
        let focused = ui.memory(|m| m.has_focus(ui.scope_id().with("logdir")));
        if focused {
            card::paint(
                ui.painter(),
                input,
                Look::custom(style::input(0.6), style::accent(), style::RADIUS_BTN),
            );
        }
        let before = self.dir_text.clone();
        let resp = {
            let buf = &mut self.dir_text;
            let mut t = crate::ui::field::TextField::new(buf)
                .hint("~/.corvus/flightlogs")
                .mono(true)
                .font(11.5, Weight::W400)
                .pad(8.0, 8.0)
                .surface(crate::ui::field::Surface::Bare);
            t = t.focus_color(style::accent());
            t.show_with_id(ui, input, ui.scope_id().with("logdir"))
        };
        if self.dir_text != before {
            self.dir_touched = true;
        }
        let pressed = save.show(
            ui,
            Rect::from_min_size(
                Pos2::new(row.right() - 11.0 - bw, row.center().y - bh / 2.0),
                Vec2::new(bw, bh),
            ),
            "savedir",
        );
        f.y = row.bottom();
        if (pressed || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))))
            && !self.dir_text.trim().is_empty()
        {
            let dir = self.dir_text.trim().to_string();
            self.job(ui.ctx(), "dir", "/api/logs/dir", json!({ "dir": dir }));
        }
        if !self.dir_note.0.is_empty() {
            f.skip(8.0);
            let color = if self.dir_note.1 {
                style::critical()
            } else {
                style::text_3()
            };
            let g = crate::ui::kit::wrapped_weight(ui, &self.dir_note.0, 11.0, Weight::W400, color, f.w - 4.0, 1.4);
            let gh = g.size().y;
            ui.painter().galley(Pos2::new(f.x + 2.0, f.y), g, color);
            f.y += gh;
        }
    }
}

/// One tile of Current Telemetry.
fn tele_tile(ui: &egui::Ui, rect: Rect, t: &model::Tile) {
    card::paint(ui.painter(), rect, card::Look::Page);
    let pad = Pos2::new(rect.left() + 15.0, rect.top() + 13.0);
    crate::ui::icons::paint(
        ui,
        t.icon,
        Rect::from_min_size(Pos2::new(pad.x, pad.y), Vec2::splat(13.0)),
        style::text_3(),
    );
    let l = style::galley(ui, &t.label.to_uppercase(), 9.5, Weight::W700, style::text_3(), 0.6);
    let lh = l.size().y;
    ui.painter()
        .with_clip_rect(Rect::from_min_max(
            Pos2::new(pad.x + 19.0, rect.top()),
            Pos2::new(rect.right() - 14.0, rect.bottom()),
        ))
        .galley(Pos2::new(pad.x + 19.0, pad.y + (13.0 - lh) / 2.0), l, style::text_3());
    let color = if t.value == "\u{2014}" {
        style::text_3()
    } else {
        style::text_1()
    };
    let v = style::galley_mono(ui, &t.value, 20.0, color);
    let vy = pad.y + 13.0f32.max(lh) + 6.0;
    let vw = v.size().x;
    let vh = v.size().y;
    ui.painter().galley(Pos2::new(pad.x, vy), v, color);
    if !t.unit.is_empty() {
        let u = style::galley(ui, &t.unit, 11.0, Weight::W400, style::text_3(), 0.0);
        let uh = u.size().y;
        ui.painter().galley(
            Pos2::new(pad.x + vw + 4.0, vy + vh * 0.75 - uh * 0.8),
            u,
            style::text_3(),
        );
    }
    if !t.detail.is_empty() {
        let d = style::galley(ui, &t.detail, 10.5, Weight::W400, style::text_3(), 0.0);
        ui.painter().galley(Pos2::new(pad.x, vy + vh + 6.0), d, style::text_3());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_opens_on_its_tiles_and_a_flag_opens_a_view() {
        let mut p = AnalysisPage::new(Server::local(1));
        assert_eq!(p.view, View::Tiles);
        p.open("scope");
        assert_eq!(p.view, View::Scope);
        p.open("nonsense");
        assert_eq!(p.view, View::Tiles);
        p.open("review");
        assert_eq!(p.view, View::Review(None));
        p.reset();
        assert_eq!(p.view, View::Tiles);
    }
}
