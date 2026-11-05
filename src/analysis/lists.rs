//! The two log lists: the logs on the vehicle, downloaded one after another,
//! and the tlogs Corvus recorded, plus the erase confirmation
//! (`buildUlog`, `buildTlog`, `confirmErase` of `python/src/js/analysis.js`).

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use super::{model, AnalysisPage, View};
use crate::ui::card::{self, Look};
use crate::ui::form::{self, Form};
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::message::Tone;
use crate::ui::style::{self, Weight};

/// `TLOG_ROWS`.
const TLOG_ROWS: usize = 60;
const ROW_H: f32 = 7.0 * 2.0 + 12.5 * 1.21 + 1.0 + 10.5 * 1.21;

fn nav_dark() -> Color32 {
    style::nav().gamma_multiply(0.16)
}

/// A box that scrolls past `max_h`, on the `.logs-list` surface. `content`
/// draws from `top` and returns how tall it is; the box's bottom comes back.
pub(super) fn list_box(
    ui: &mut egui::Ui,
    at: Pos2,
    w: f32,
    max_h: f32,
    id: &str,
    fill: Color32,
    content: impl FnOnce(&mut egui::Ui, f32, f32, f32) -> f32,
) -> f32 {
    let slot = card::reserve(ui.painter());
    let (x, y) = (at.x, at.y);
    let inner = Rect::from_min_size(Pos2::new(x + 5.0, y + 5.0), Vec2::new(w - 10.0, max_h - 10.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
    child.set_clip_rect(inner);
    let out = egui::ScrollArea::vertical()
        .id_salt(("listbox", id))
        .max_height(max_h - 10.0)
        .auto_shrink([false, true])
        .show(&mut child, |ui| {
            let top = ui.max_rect().top();
            let h = content(ui, ui.max_rect().left(), top, w - 10.0 - 8.0);
            ui.allocate_space(Vec2::new(w - 10.0 - 8.0, h));
            h
        });
    let shown = out.inner.min(max_h - 10.0);
    let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, shown + 10.0));
    ui.painter()
        .set(slot, egui::Shape::rect_filled(rect, style::RADIUS_BTN, fill));
    ui.painter().rect_stroke(
        rect,
        style::RADIUS_BTN,
        Stroke::new(1.0, style::border_soft()),
        egui::StrokeKind::Inside,
    );
    rect.bottom()
}

/// The `Sort` label and select at the right edge of a row ending at `right`.
/// Returns the value picked.
fn sort_control(
    ui: &mut egui::Ui,
    right: f32,
    y: f32,
    options: &[(&str, &str)],
    current: &str,
    id: &str,
) -> Option<String> {
    let label = style::galley(ui, "SORT", 10.0, Weight::W600, style::text_3(), 0.7);
    let lw = label.size().x;
    let h = 26.0;
    let sel = Rect::from_min_size(Pos2::new(right - 150.0, y), Vec2::new(150.0, h));
    ui.painter().galley(
        Pos2::new(sel.left() - 6.0 - lw, y + (h - label.size().y) / 2.0),
        label,
        style::text_3(),
    );
    let choices: Vec<Choice> = options.iter().map(|o| Choice::new(o.0, o.1)).collect();
    kit::select_padded(ui, sel, &choices, current, "", false, "Sort logs by", id, (11.0, 26.0))
}

/// The "Review" shortcut at the right of a row. Returns true when pressed.
fn review_button(
    ui: &mut egui::Ui,
    row: Rect,
    right: f32,
    title: &str,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> (f32, bool) {
    let label = style::galley(ui, "Review", 11.0, Weight::W600, style::nav(), 0.1);
    let w = 2.0 + 16.0 + label.size().x + 5.0 + 13.0;
    let h = 29.0;
    let r = Rect::from_min_size(Pos2::new(right - w, row.center().y - h / 2.0), Vec2::new(w, h));
    let resp = ui
        .interact(r, ui.scope_id().with(id), egui::Sense::click())
        .on_hover_text(title);
    if resp.hovered() {
        ui.painter().rect_filled(r, style::RADIUS_BTN, nav_dark());
    }
    ui.painter().galley(
        Pos2::new(r.left() + 9.0, r.center().y - label.size().y / 2.0),
        label.clone(),
        style::nav(),
    );
    crate::ui::icons::paint(
        ui,
        "arrow-right",
        Rect::from_center_size(
            Pos2::new(r.left() + 9.0 + label.size().x + 5.0 + 6.5, r.center().y),
            Vec2::splat(13.0),
        ),
        style::nav(),
    );
    (w, resp.clicked())
}

impl AnalysisPage {
    pub(super) fn ulog_view(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        if self.back(ui, f, "Analysis") {
            self.set_view(View::Tiles);
            return;
        }
        let running = model::busy(&self.status);
        let connected = self.connected();
        let logs = model::sorted_logs(&self.logs(), &self.ulog_sort);
        let done: Vec<(i64, bool)> = self
            .status
            .get("completed")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|c| {
                        Some((
                            c.get("id")?.as_i64()?,
                            c.get("ok").and_then(Value::as_bool).unwrap_or(false),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let current = self.status.get("current").and_then(Value::as_i64);
        let percent = self
            .status
            .get("percent")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .clamp(0.0, 100.0);
        let message = self
            .status
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let failed = self.status.get("state").and_then(Value::as_str) == Some("failed");
        let (offset, in_flight) = (self.offset, self.in_flight);

        let slot = card::reserve(ui.painter());
        let top = f.y;
        let mut inner = Form::new(f.x + 19.0, top + 19.0, f.w - 38.0);
        // head
        let (_, th) = form::head_title(ui, Pos2::new(inner.x, inner.y), "Vehicle logs (ULog)");
        let refresh = Button::new("Read from vehicle", Variant::Secondary)
            .icon("refresh-cw")
            .small()
            .enabled(!running && !in_flight && connected);
        let (bw, bh) = (refresh.width(ui), refresh.height());
        let read = refresh.show(
            ui,
            Rect::from_min_size(
                Pos2::new(inner.x + inner.w - bw, inner.y + (th - bh) / 2.0),
                Vec2::new(bw, bh),
            ),
            "readvehicle",
        );
        inner.y += th.max(bh) + 10.0;
        form::box_note(
            ui,
            &mut inner,
            "Times are this computer's local clock, converted from the vehicle's UTC stamp.",
            Tone::Plain,
        );
        if !connected {
            inner.skip(10.0);
            form::box_note(
                ui,
                &mut inner,
                "No link to the vehicle. Connect to read its logs.",
                Tone::Warn,
            );
        }
        inner.skip(10.0);
        // selection row
        let mut mode: Option<&str> = None;
        let mut x = inner.x;
        for (label, which) in [("Select missing", "missing"), ("Select all", "all"), ("Clear", "none")] {
            let b = Button::new(label, Variant::Ghost).small().enabled(!running);
            let w = b.width(ui);
            if b.show(
                ui,
                Rect::from_min_size(Pos2::new(x, inner.y), Vec2::new(w, 26.0)),
                ("sel", which),
            ) {
                mode = Some(which);
            }
            x += w + 6.0;
        }
        if !self.selected.is_empty() {
            let g = style::galley(
                ui,
                &format!("{} selected. They download one after another.", self.selected.len()),
                11.0,
                Weight::W400,
                style::text_3(),
                0.0,
            );
            let gh = g.size().y;
            ui.painter()
                .galley(Pos2::new(x - 2.0, inner.y + (26.0 - gh) / 2.0), g, style::text_3());
        }
        let picked_sort = sort_control(
            ui,
            inner.x + inner.w,
            inner.y,
            &model::ULOG_SORTS,
            &self.ulog_sort,
            "ulogsort",
        );
        inner.y += 26.0 + 10.0;
        // the list
        let mut toggled: Option<i64> = None;
        let mut review: Option<String> = None;
        let selected = self.selected.clone();
        let bottom = list_box(
            ui,
            Pos2::new(inner.x, inner.y),
            inner.w,
            320.0,
            "ulog",
            style::surface_1(),
            |ui, left, top, w| {
                if logs.is_empty() {
                    let g = style::galley(
                        ui,
                        "No logs read yet. Press \u{201c}Read from vehicle\u{201d}.",
                        12.0,
                        Weight::W400,
                        style::text_3(),
                        0.0,
                    );
                    let h = g.size().y + 8.0;
                    ui.painter()
                        .galley(Pos2::new(left + 4.0, top + 4.0), g, style::text_3());
                    return h;
                }
                let mut y = top;
                for l in &logs {
                    let id = l.get("id").and_then(Value::as_i64).unwrap_or(0);
                    let row = Rect::from_min_size(Pos2::new(left, y), Vec2::new(w, ROW_H));
                    let resp = ui.interact(row, ui.scope_id().with(("ulogrow", id)), egui::Sense::click());
                    let state = if current == Some(id) {
                        "active"
                    } else if let Some((_, ok)) = done.iter().find(|d| d.0 == id) {
                        if *ok {
                            "done"
                        } else {
                            "failed"
                        }
                    } else {
                        ""
                    };
                    if state == "active" {
                        ui.painter().rect_filled(row, style::RADIUS_BTN, nav_dark());
                    } else if resp.hovered() {
                        ui.painter().rect_filled(row, style::RADIUS_BTN, style::surface_hover());
                    }
                    let on = selected.contains(&id);
                    let bx = Rect::from_min_size(Pos2::new(row.left() + 9.0, row.center().y - 7.0), Vec2::splat(14.0));
                    if on {
                        ui.painter().rect_filled(bx, 3, style::accent());
                        let a = bx.left_top();
                        ui.painter().line(
                            vec![
                                a + Vec2::new(3.2, 7.2),
                                a + Vec2::new(6.0, 10.0),
                                a + Vec2::new(11.0, 4.2),
                            ],
                            Stroke::new(1.8, Color32::WHITE),
                        );
                    } else {
                        card::paint(
                            ui.painter(),
                            bx,
                            Look::custom(style::surface_1(), style::border_strong(), 3),
                        );
                    }
                    let tx = bx.right() + 10.0;
                    let title = style::galley(ui, &format!("Log {id}"), 12.5, Weight::W600, style::text_1(), 0.0);
                    let tw = title.size().x;
                    let th = title.size().y;
                    ui.painter()
                        .galley(Pos2::new(tx, row.top() + 7.0), title, style::text_1());
                    let downloaded = l.get("downloaded").and_then(Value::as_bool).unwrap_or(false);
                    if downloaded {
                        let tag = style::galley(ui, "IN FOLDER", 9.0, Weight::W700, style::healthy(), 0.5);
                        let r = Rect::from_min_size(
                            Pos2::new(tx + tw + 7.0, row.top() + 7.0 + (th - (tag.size().y + 2.0)) / 2.0),
                            Vec2::new(tag.size().x + 12.0, tag.size().y + 2.0),
                        );
                        card::paint(
                            ui.painter(),
                            r,
                            Look::custom(style::healthy_dark(), style::healthy(), 4),
                        );
                        ui.painter().galley(r.min + Vec2::new(6.0, 1.0), tag, style::healthy());
                    }
                    let mut meta = format!(
                        "{}  \u{b7}  {}",
                        model::format_stamp(l.get("utc").and_then(Value::as_f64).unwrap_or(0.0), false, offset),
                        model::format_size(l.get("size").and_then(Value::as_f64).unwrap_or(0.0))
                    );
                    if downloaded {
                        if let Some(file) = l.get("file").and_then(Value::as_str).filter(|f| !f.is_empty()) {
                            meta.push_str(&format!("  \u{b7}  {file}"));
                        }
                    }
                    let mark_x = row.right() - 9.0 - 3.5;
                    let mut right = mark_x - 3.5 - 10.0;
                    let file_name = l.get("file_name").and_then(Value::as_str).filter(|_| downloaded);
                    if let Some(name) = file_name {
                        let (w, hit) = review_button(
                            ui,
                            row,
                            right,
                            &format!("Open {name} in Flight Review"),
                            ("reviewlog", id),
                        );
                        if hit {
                            review = Some(name.to_string());
                        }
                        right -= w + 10.0;
                    }
                    let m = style::galley_mono(ui, &meta, 10.5, style::text_3());
                    let clip = Rect::from_min_max(Pos2::new(tx, row.top()), Pos2::new(right, row.bottom()));
                    ui.painter_at(clip)
                        .galley(Pos2::new(tx, row.top() + 7.0 + th + 1.0), m, style::text_3());
                    match state {
                        "active" => kit::dot(ui.painter(), Pos2::new(mark_x, row.center().y), style::nav(), true),
                        "done" => kit::dot(ui.painter(), Pos2::new(mark_x, row.center().y), style::healthy(), true),
                        "failed" => kit::dot(ui.painter(), Pos2::new(mark_x, row.center().y), style::critical(), true),
                        _ => {}
                    }
                    if resp.clicked() && review.is_none() {
                        toggled = Some(id);
                    }
                    y += ROW_H + 2.0;
                }
                y - top
            },
        );
        inner.y = bottom + 10.0;
        // actions
        let mut hit = None;
        let n = self.selected.len();
        let dl = Button::new("Download selected", Variant::Primary)
            .icon("hard-drive-download")
            .enabled(!(running || in_flight || n == 0 || !connected));
        let dlw = dl.width(ui);
        let dlh = dl.height();
        if dl.show(
            ui,
            Rect::from_min_size(Pos2::new(inner.x, inner.y), Vec2::new(dlw, dlh)),
            "dlselected",
        ) {
            hit = Some("download");
        }
        let mut x = inner.x + dlw + 8.0;
        if running {
            let b = Button::new("Cancel", Variant::Secondary).icon("x").small();
            let (w, h) = (b.width(ui), b.height());
            if b.show(
                ui,
                Rect::from_min_size(Pos2::new(x, inner.y + (dlh - h) / 2.0), Vec2::new(w, h)),
                "cancellogs",
            ) {
                hit = Some("cancel");
            }
            x += w + 8.0;
        }
        let _ = x;
        let erase = Button::new("Erase all on vehicle", Variant::Danger)
            .icon("trash-2")
            .small()
            .enabled(!(running || in_flight || !connected || logs.is_empty()));
        let (ew, eh) = (erase.width(ui), erase.height());
        if erase.show(
            ui,
            Rect::from_min_size(
                Pos2::new(inner.x + inner.w - ew, inner.y + (dlh - eh) / 2.0),
                Vec2::new(ew, eh),
            ),
            "eraselogs",
        ) {
            hit = Some("erase");
        }
        inner.y += dlh;
        if running {
            inner.skip(10.0 + 4.0);
            let bar = inner.rect(5.0);
            ui.painter().rect_filled(bar, 3, style::surface_3());
            let fill = Rect::from_min_size(bar.min, Vec2::new(bar.width() * (percent as f32 / 100.0), 5.0));
            ui.painter().rect_filled(fill, 3, style::nav());
            inner.y = bar.bottom();
        }
        inner.skip(10.0);
        let (line, err) = if !message.is_empty() {
            (message, failed)
        } else {
            self.ulog_line.clone()
        };
        let color = if err { style::critical() } else { style::text_2() };
        let g = kit::wrapped_weight(ui, &line, 11.5, Weight::W400, color, inner.w, 1.5);
        let gh = g.size().y.max(16.0);
        ui.painter().galley(Pos2::new(inner.x, inner.y), g, color);
        inner.y += gh;
        let rect = Rect::from_min_max(Pos2::new(f.x, top), Pos2::new(f.x + f.w, inner.y + 19.0));
        card::fill(ui.painter(), slot, rect, Look::Page);
        f.y = rect.bottom() + 12.0;

        if let Some(m) = mode {
            self.selected.clear();
            let missing = |l: &Value| l.get("downloaded").and_then(Value::as_bool) != Some(true);
            for l in self.logs() {
                let id = l.get("id").and_then(Value::as_i64).unwrap_or(0);
                if m == "all" || (m == "missing" && missing(&l)) {
                    self.selected.insert(id);
                }
            }
        }
        if let Some(s) = picked_sort {
            self.ulog_sort = s;
        }
        if let Some(id) = toggled {
            if !self.selected.remove(&id) {
                self.selected.insert(id);
            }
        }
        if let Some(name) = review {
            self.set_view(View::Review(Some(name)));
            return;
        }
        match hit {
            Some("download") => {
                let ids: Vec<i64> = self.selected.iter().copied().collect();
                self.ulog_line = (String::new(), false);
                self.job(ui.ctx(), "download", "/api/logs/download", json!({ "ids": ids }));
            }
            Some("cancel") => self.job(ui.ctx(), "cancel", "/api/logs/cancel", json!({})),
            Some("erase") => self.erase_open = true,
            _ => {}
        }
        if read {
            self.ulog_line = ("Asking the vehicle for its logs\u{2026}".into(), false);
            self.job(ui.ctx(), "refresh", "/api/logs/refresh", json!({}));
        }
    }

    pub(super) fn tlog_view(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        if self.back(ui, f, "Analysis") {
            self.set_view(View::Tiles);
            return;
        }
        let items = model::sorted_tlogs(&self.tlogs(), &self.tlog_sort);
        let offset = self.offset;
        let slot = card::reserve(ui.painter());
        let top = f.y;
        let mut inner = Form::new(f.x + 19.0, top + 19.0, f.w - 38.0);
        let (_, th) = form::head_title(ui, Pos2::new(inner.x, inner.y), "Recorded tlogs");
        let picked = sort_control(
            ui,
            inner.x + inner.w,
            inner.y + (th - 26.0) / 2.0,
            &model::TLOG_SORTS,
            &self.tlog_sort,
            "tlogsort",
        );
        inner.y += th.max(26.0) + 10.0;
        form::box_note(
            ui,
            &mut inner,
            "Every session Corvus recorded from the MAVLink stream, already on disk, nothing to download. Review reads one as plots, the same way a ULog is read. Times are when the recording opened, on this computer's clock.",
            Tone::Plain,
        );
        inner.skip(10.0);
        let mut open: Option<String> = None;
        let shown: Vec<Value> = items.iter().take(TLOG_ROWS).cloned().collect();
        let bottom = list_box(
            ui,
            Pos2::new(inner.x, inner.y),
            inner.w,
            320.0,
            "tlog",
            style::surface_1(),
            |ui, left, top, w| {
                if shown.is_empty() {
                    let g = style::galley(ui, "No tlogs recorded yet.", 12.0, Weight::W400, style::text_3(), 0.0);
                    let h = g.size().y + 8.0;
                    ui.painter()
                        .galley(Pos2::new(left + 4.0, top + 4.0), g, style::text_3());
                    return h;
                }
                let mut y = top;
                for t in &shown {
                    let name = t.get("name").and_then(Value::as_str).unwrap_or("");
                    let row = Rect::from_min_size(Pos2::new(left, y), Vec2::new(w, ROW_H));
                    let started = {
                        let s = t.get("started").and_then(Value::as_f64).unwrap_or(0.0);
                        if s != 0.0 {
                            s
                        } else {
                            t.get("mtime").and_then(Value::as_f64).unwrap_or(0.0)
                        }
                    };
                    let title = style::galley(
                        ui,
                        &model::format_stamp(started, true, offset),
                        12.5,
                        Weight::W600,
                        style::text_1(),
                        0.0,
                    );
                    let th = title.size().y;
                    ui.painter()
                        .galley(Pos2::new(row.left() + 9.0, row.top() + 7.0), title, style::text_1());
                    let meta = format!(
                        "{name}  \u{b7}  {}  \u{b7}  {}",
                        model::format_size(t.get("size").and_then(Value::as_f64).unwrap_or(0.0)),
                        t.get("path").and_then(Value::as_str).unwrap_or("")
                    );
                    let right = row.right() - 9.0;
                    let (_, hit) = review_button(
                        ui,
                        row,
                        right,
                        &format!("Read {name} as a Telemetry Review"),
                        ("reviewtlog", name),
                    );
                    if hit {
                        open = Some(name.to_string());
                    }
                    let m = style::galley_mono(ui, &meta, 10.5, style::text_3());
                    let clip = Rect::from_min_max(
                        Pos2::new(row.left() + 9.0, row.top()),
                        Pos2::new(right - 90.0, row.bottom()),
                    );
                    ui.painter_at(clip).galley(
                        Pos2::new(row.left() + 9.0, row.top() + 7.0 + th + 1.0),
                        m,
                        style::text_3(),
                    );
                    y += ROW_H + 2.0;
                }
                y - top
            },
        );
        inner.y = bottom;
        let note = model::tlog_note(items.len(), TLOG_ROWS);
        inner.skip(10.0);
        let g = kit::wrapped_weight(ui, &note, 11.5, Weight::W400, style::text_2(), inner.w, 1.5);
        let gh = g.size().y.max(16.0);
        ui.painter().galley(Pos2::new(inner.x, inner.y), g, style::text_2());
        inner.y += gh;
        let rect = Rect::from_min_max(Pos2::new(f.x, top), Pos2::new(f.x + f.w, inner.y + 19.0));
        card::fill(ui.painter(), slot, rect, Look::Page);
        f.y = rect.bottom() + 12.0;
        if let Some(s) = picked {
            self.tlog_sort = s;
        }
        if let Some(n) = open {
            self.set_view(View::TlogReview(n));
        }
    }

    pub(super) fn erase_dialog(&mut self, ctx: &egui::Context) {
        let logs = self.logs();
        let missing = logs
            .iter()
            .filter(|l| l.get("downloaded").and_then(Value::as_bool) != Some(true))
            .count();
        let lines = model::erase_lines(logs.len(), missing);
        let (mut keep, mut erase) = (false, false);
        let confirm = if missing > 0 { "Erase anyway" } else { "Erase all logs" };
        let closed = crate::ui::modal::show(
            ctx,
            "erase",
            crate::ui::modal::Size::Sm,
            "Erase all logs on the vehicle",
            |ui, x, y, w| {
                let mut f = Form::new(x, y, w);
                for (i, l) in lines.iter().enumerate() {
                    if i > 0 {
                        f.skip(8.0);
                    }
                    form::box_note(ui, &mut f, l, Tone::Warn);
                }
                f.skip(18.0);
                let (hit, bottom) = crate::ui::modal::actions(
                    ui,
                    f.x,
                    f.y,
                    f.w,
                    vec![
                        Button::new("Keep the logs", Variant::Secondary),
                        Button::new(confirm, Variant::Danger),
                    ],
                );
                match hit {
                    Some(0) => keep = true,
                    Some(1) => erase = true,
                    _ => {}
                }
                bottom
            },
        );
        if closed || keep || erase {
            self.erase_open = false;
        }
        if erase {
            self.selected.clear();
            self.job(ctx, "erase", "/api/logs/erase", json!({}));
        }
    }
}
