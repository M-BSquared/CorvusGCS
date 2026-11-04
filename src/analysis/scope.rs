//! Live Messages (`buildScope` of `python/src/js/analysis.js`): every MAVLink
//! message type on the link with its rate, age and count, and the fields of
//! the selected one. The backend keeps the newest message per type; this view
//! asks for the table twice a second while it is open and not paused.

use eframe::egui::{self, Pos2, Rect, Vec2};
use serde_json::{json, Value};

use super::{model, AnalysisPage, View};
use crate::net::bg::Pending;
use crate::net::http;
use crate::ui::card::{self, Look};
use crate::ui::form::{self, Form, Tone};
use crate::ui::kit::{self, Button, Variant};
use crate::ui::style::{self, Weight};

/// `SCOPE_POLL_MS`.
const POLL: f64 = 0.5;

/// What the view holds between frames.
#[derive(Default)]
pub struct Scope {
    selected: String,
    paused: bool,
    filter: String,
    data: Value,
    load: Option<Pending<Result<Value, String>>>,
    at: f64,
}

/// The rows a filter lets through: a case blind substring of the name.
pub fn filtered<'a>(rows: &'a [Value], filter: &str) -> Vec<&'a Value> {
    let q = filter.trim().to_lowercase();
    rows.iter()
        .filter(|r| {
            q.is_empty()
                || r.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&q)
        })
        .collect()
}

/// The line under the filter (`paintRows`).
pub fn summary(data: &Value) -> String {
    let rows = data.get("rows").and_then(Value::as_array).map_or(0, Vec::len);
    let total = data.get("total").and_then(Value::as_f64).unwrap_or(0.0);
    let dropped = data.get("dropped").and_then(Value::as_f64).unwrap_or(0.0);
    if data.get("connected").and_then(Value::as_bool) != Some(true) {
        "Connect to a vehicle to see its messages.".into()
    } else if rows > 0 {
        format!(
            "{rows} message type(s), {} received{}",
            crate::data::units::js_number(total),
            if dropped > 0.0 {
                format!(", {} not tracked (table full)", crate::data::units::js_number(dropped))
            } else {
                String::new()
            }
        )
    } else {
        "Nothing heard yet.".into()
    }
}

impl AnalysisPage {
    pub(super) fn scope_view(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        if self.back(ui, f, "Analysis") {
            self.set_view(View::Tiles);
            return;
        }
        let now = ui.ctx().input(|i| i.time);
        if let Some(r) = self.scope.load.as_ref().and_then(Pending::take) {
            self.scope.load = None;
            if let Ok(v) = r {
                self.scope.data = v;
            }
        }
        if !self.scope.paused && self.scope.load.is_none() && now - self.scope.at >= POLL {
            self.scope.at = now;
            let mut path = "/api/mavlink/scope".to_string();
            if !self.scope.selected.is_empty() {
                path.push_str(&format!(
                    "?select={}",
                    crate::net::http::encode_component(&self.scope.selected)
                ));
            }
            let server = self.server.clone();
            self.scope.load = Some(Pending::spawn(ui.ctx(), move || http::get_json(&server, &path)));
        }
        let rows: Vec<Value> = self
            .scope
            .data
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let shown = filtered(&rows, &self.scope.filter);
        let fields: Vec<Value> = self
            .scope
            .data
            .get("fields")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let stacked = f.w < 400.0 + 280.0 + 14.0;
        let main_w = if stacked {
            f.w
        } else {
            ((f.w - 14.0) * 1.3 / 2.3).max(400.0)
        };
        let detail_w = if stacked { f.w } else { f.w - 14.0 - main_w };

        // the table card
        let slot = card::reserve(ui.painter());
        let top = f.y;
        let mut inner = Form::new(f.x + 19.0, top + 19.0, main_w - 38.0);
        let (_, th) = form::head_title(ui, Pos2::new(inner.x, inner.y), "Live Messages");
        let clear = Button::new("Clear", Variant::Secondary).icon("eraser").small();
        let pause = Button::new(if self.scope.paused { "Resume" } else { "Pause" }, Variant::Secondary)
            .icon(if self.scope.paused { "play" } else { "pause" })
            .small();
        let (cw, pw, bh) = (clear.width(ui), pause.width(ui), clear.height());
        let by = inner.y + (th - bh) / 2.0;
        let mut do_clear = clear.show(
            ui,
            Rect::from_min_size(Pos2::new(inner.x + inner.w - cw, by), Vec2::new(cw, bh)),
            "scopeclear",
        );
        let toggled = pause.show(
            ui,
            Rect::from_min_size(Pos2::new(inner.x + inner.w - cw - 6.0 - pw, by), Vec2::new(pw, bh)),
            "scopepause",
        );
        inner.y += th.max(bh) + 10.0;
        form::box_note(
            ui,
            &mut inner,
            "Every MAVLink message type heard on the link, with how often it arrives and how long ago the last one came. Select a row to read the fields of its newest message. Messages are only counted while this page is open and not paused.",
            Tone::Plain,
        );
        inner.skip(10.0);
        let r = Rect::from_min_size(
            Pos2::new(inner.x, inner.y),
            Vec2::new(inner.w.min(360.0), form::CONTROL_H),
        );
        crate::ui::field::TextField::new(&mut self.scope.filter)
            .hint("Filter by name, for example ATTITUDE")
            .mono(true)
            .pad(11.0, 11.0)
            .show(ui, r, "scopefilter");
        inner.y += form::CONTROL_H + 10.0;
        let g = style::galley(ui, &summary(&self.scope.data), 11.5, Weight::W400, style::text_2(), 0.0);
        let gh = g.size().y.max(16.0);
        ui.painter().galley(Pos2::new(inner.x, inner.y), g, style::text_2());
        inner.y += gh + 10.0;
        // the table
        let cols = scope_columns(inner.w);
        let head_y = inner.y;
        for (i, label) in ["Message", "ID", "From", "Hz", "Age", "Count"].iter().enumerate() {
            let g = style::galley(ui, &label.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.5);
            let (cx, cw2) = cols[i];
            let x = if matches!(i, 1 | 3 | 4 | 5) {
                inner.x + 10.0 + cx + cw2 - g.size().x
            } else {
                inner.x + 10.0 + cx + if i == 2 { 8.0 } else { 0.0 }
            };
            ui.painter().galley(Pos2::new(x, head_y + 4.0), g, style::text_3());
        }
        inner.y += 4.0 * 2.0 + 12.0 + 1.0;
        ui.painter().hline(
            egui::Rangef::new(inner.x, inner.x + inner.w),
            inner.y - 0.5,
            egui::Stroke::new(1.0, style::border()),
        );
        let vh = ui.ctx().content_rect().height();
        let max_h = (vh * 0.62).max(200.0);
        let row_h = 4.0 * 2.0 + 12.0 * 1.21;
        let mut pick: Option<String> = None;
        let selected = self.scope.selected.clone();
        let rect = Rect::from_min_size(Pos2::new(inner.x, inner.y), Vec2::new(inner.w, max_h));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.set_clip_rect(rect);
        let out = egui::ScrollArea::vertical()
            .id_salt("scoperows")
            .max_height(max_h)
            .auto_shrink([false, true])
            .show(&mut child, |ui| {
                let top = ui.max_rect().top();
                let left = ui.max_rect().left();
                let mut y = top;
                for r in &shown {
                    let key = r.get("key").and_then(Value::as_str).unwrap_or("");
                    let row = Rect::from_min_size(Pos2::new(left, y), Vec2::new(inner.w - 8.0, row_h));
                    let resp = ui.interact(row, ui.scope_id().with(("scoperow", key)), egui::Sense::click());
                    let sel = key == selected;
                    if sel {
                        ui.painter().rect_filled(row, 6, style::nav().gamma_multiply(0.16));
                    } else if resp.hovered() {
                        ui.painter().rect_filled(row, 6, style::surface_hover());
                    }
                    let n = |k: &str| r.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                    let quiet = n("rate").is_nan() || n("rate") <= 0.0;
                    let color = if quiet { style::text_3() } else { style::text_1() };
                    let cells = [
                        r.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                        crate::data::units::js_number(n("id")),
                        format!(
                            "{}:{}",
                            crate::data::units::js_number(n("system")),
                            crate::data::units::js_number(n("component"))
                        ),
                        model::format_rate(n("rate")),
                        model::format_age(n("age")),
                        crate::data::units::js_number(n("count")),
                    ];
                    for (i, text) in cells.iter().enumerate() {
                        let g = if i == 0 {
                            style::galley(ui, text, 12.0, Weight::W600, color, 0.0)
                        } else {
                            style::galley_mono(ui, text, 12.0, color)
                        };
                        let (cx, cw2) = cols[i];
                        let x = if matches!(i, 1 | 3 | 4 | 5) {
                            row.left() + 10.0 + cx + cw2 - g.size().x
                        } else {
                            row.left() + 10.0 + cx + if i == 2 { 8.0 } else { 0.0 }
                        };
                        let clip =
                            Rect::from_min_size(Pos2::new(row.left() + 10.0 + cx, row.top()), Vec2::new(cw2, row_h));
                        ui.painter_at(clip).galley(
                            Pos2::new(x.max(clip.left()), row.center().y - g.size().y / 2.0),
                            g,
                            color,
                        );
                    }
                    if resp.clicked() {
                        pick = Some(key.to_string());
                    }
                    y += row_h;
                }
                ui.allocate_space(Vec2::new(inner.w - 8.0, y - top));
                y - top
            });
        let table_h = out.inner.min(max_h);
        inner.y += table_h;
        let card_rect = Rect::from_min_max(Pos2::new(f.x, top), Pos2::new(f.x + main_w, inner.y + 19.0));
        card::fill(ui.painter(), slot, card_rect, Look::Page);

        // the fields card
        let (dx, dy) = if stacked {
            (f.x, card_rect.bottom() + 12.0)
        } else {
            (f.x + main_w + 14.0, top)
        };
        let slot2 = card::reserve(ui.painter());
        let mut d = Form::new(dx + 19.0, dy + 19.0, detail_w - 38.0);
        let (_, th) = form::head_title(ui, Pos2::new(d.x, d.y), "Fields");
        d.y += th + 12.0;
        if self.scope.selected.is_empty() || fields.is_empty() {
            let t = if self.scope.selected.is_empty() {
                "Select a message above to read its fields."
            } else {
                "This message has not been heard yet."
            };
            form::desc(ui, &mut d, t);
        } else {
            let (name_w, unit_w) = (d.w.min(120.0), 48.0);
            for fl in &fields {
                let name = style::galley(
                    ui,
                    fl.get("name").and_then(Value::as_str).unwrap_or(""),
                    12.0,
                    Weight::W400,
                    style::text_2(),
                    0.0,
                );
                let val = crate::ui::kit::wrapped_weight(
                    ui,
                    &model::format_field(fl.get("value").unwrap_or(&Value::Null)),
                    12.0,
                    Weight::W400,
                    style::text_1(),
                    (d.w - name_w - unit_w - 20.0).max(40.0),
                    1.21,
                );
                let h = val.size().y.max(name.size().y) + 6.0;
                ui.painter()
                    .galley(Pos2::new(d.x + 4.0, d.y + 3.0), name, style::text_2());
                let mono = style::galley_mono(
                    ui,
                    &model::format_field(fl.get("value").unwrap_or(&Value::Null)),
                    12.0,
                    style::text_1(),
                );
                let _ = val;
                ui.painter()
                    .galley(Pos2::new(d.x + 4.0 + name_w + 10.0, d.y + 3.0), mono, style::text_1());
                let unit = style::galley(
                    ui,
                    fl.get("unit").and_then(Value::as_str).unwrap_or(""),
                    11.0,
                    Weight::W400,
                    style::text_3(),
                    0.0,
                );
                ui.painter()
                    .galley(Pos2::new(d.x + d.w - unit_w, d.y + 3.0), unit, style::text_3());
                d.y += h;
                ui.painter().hline(
                    egui::Rangef::new(d.x, d.x + d.w),
                    d.y - 0.5,
                    egui::Stroke::new(1.0, style::border()),
                );
            }
        }
        let drect = Rect::from_min_max(Pos2::new(dx, dy), Pos2::new(dx + detail_w, d.y + 19.0));
        card::fill(ui.painter(), slot2, drect, Look::Page);
        f.y = card_rect.bottom().max(drect.bottom()) + 12.0;

        if toggled {
            self.scope.paused = !self.scope.paused;
            self.scope.at = f64::NEG_INFINITY;
        }
        if do_clear {
            let server = self.server.clone();
            let ctx = ui.ctx().clone();
            let _ = Pending::spawn(&ctx, move || {
                http::post_json(&server, "/api/mavlink/scope/clear", &json!({}))
            });
            self.scope.at = f64::NEG_INFINITY;
            do_clear = false;
        }
        let _ = do_clear;
        if let Some(k) = pick {
            self.scope.selected = if self.scope.selected == k { String::new() } else { k };
            self.scope.at = f64::NEG_INFINITY;
        }
        let _ = kit::dot;
    }
}

/// The columns of the table, as `(x, width)` inside the row's padding:
/// `minmax(110px, 1fr) 40px 52px 44px 52px 60px` with 6 px between.
pub fn scope_columns(w: f32) -> [(f32, f32); 6] {
    let inner = w - 20.0 - 8.0;
    let fixed = [40.0f32, 52.0, 44.0, 52.0, 60.0];
    let first = (inner - fixed.iter().sum::<f32>() - 6.0 * 5.0).max(110.0);
    let mut out = [(0.0, 0.0); 6];
    let mut x = 0.0;
    for (i, wd) in std::iter::once(first).chain(fixed).enumerate() {
        out[i] = (x, wd);
        x += wd + 6.0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_matches_a_part_of_the_name_in_either_case() {
        let rows = vec![
            json!({"name": "ATTITUDE"}),
            json!({"name": "GPS_RAW_INT"}),
            json!({"name": "HEARTBEAT"}),
        ];
        assert_eq!(filtered(&rows, "").len(), 3);
        assert_eq!(filtered(&rows, " att ").len(), 1);
        assert_eq!(filtered(&rows, "raw").len(), 1);
        assert_eq!(filtered(&rows, "zzz").len(), 0);
    }

    #[test]
    fn the_summary_says_how_much_was_heard() {
        assert_eq!(summary(&json!({})), "Connect to a vehicle to see its messages.");
        assert_eq!(summary(&json!({"connected": true, "rows": []})), "Nothing heard yet.");
        assert_eq!(
            summary(&json!({"connected": true, "rows": [1, 2], "total": 480, "dropped": 3})),
            "2 message type(s), 480 received, 3 not tracked (table full)"
        );
        assert_eq!(
            summary(&json!({"connected": true, "rows": [1], "total": 9})),
            "1 message type(s), 9 received"
        );
    }

    #[test]
    fn the_columns_fill_the_row() {
        let c = scope_columns(600.0);
        assert_eq!(c[1].1, 40.0);
        assert!((c[5].0 + c[5].1 - (600.0 - 28.0)).abs() < 1e-3);
        assert_eq!(scope_columns(200.0)[0].1, 110.0);
    }
}
