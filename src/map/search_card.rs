//! The place search as drawn on the Home map (`.map-search`): a glass square
//! that grows a field out of its left edge, the answers under the field, a
//! pin where the chosen one is. Every keystroke shows what this laptop can
//! answer, the lookup of a name follows 350 ms after the last one.

use eframe::egui::{self, Pos2, Rect, Vec2};
use serde_json::Value;

use crate::data::units::Units;
use crate::map::measure;
use crate::map::search::{self as search, Row, MAX_ROWS};
use crate::map::Map;
use crate::net::bg::Pending;
use crate::net::http;
use crate::ui::field::{Surface, TextField};
use crate::ui::icons;
use crate::ui::listrow::{self, Mark};
use crate::ui::menu;
use crate::ui::style;

const DEBOUNCE: f64 = 0.35;
const GAP: f32 = 8.0;

/// The search box's state.
#[derive(Default)]
pub struct SearchCard {
    pub open: bool,
    text: String,
    rows: Vec<Row>,
    index: Option<usize>,
    note: String,
    busy: bool,
    edited_at: Option<f64>,
    asked: String,
    lookup: Option<Pending<Result<Value, String>>>,
    local: Vec<Row>,
    fresh: bool,
    list_at: Option<Rect>,
}

impl SearchCard {
    fn set_rows(&mut self, rows: Vec<Row>, note: String) {
        let previous = self.index.and_then(|i| self.rows.get(i)).map(|r| r.label.clone());
        self.index = search::keep_selection(previous.as_deref(), &rows);
        self.rows = rows;
        self.note = note;
        self.busy = false;
    }

    fn changed(&mut self, map: &Map, now: f64) {
        self.local = search::local_rows(map.regions(), &self.text);
        self.set_rows(self.local.clone(), String::new());
        let text = self.text.trim();
        self.edited_at = (!text.is_empty() && search::parse_coordinates(text).is_none()).then_some(now);
    }

    fn lookup_now(&mut self, ctx: &egui::Context, map: &Map) {
        self.edited_at = None;
        let text = self.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.asked = text.clone();
        self.busy = true;
        let near = map
            .camera()
            .map(|(lng, lat, _)| format!("&near={lng:.5},{lat:.5}"))
            .unwrap_or_default();
        let path = format!(
            "/api/geocode?q={}&limit={MAX_ROWS}{near}",
            crate::term::session::encode(&text)
        );
        let server = map.server().clone();
        self.lookup = Some(Pending::spawn(ctx, move || http::get_json(&server, &path)));
    }

    fn close(&mut self) {
        self.open = false;
        self.edited_at = None;
        self.lookup = None;
        self.rows.clear();
        self.index = None;
        self.note.clear();
        self.busy = false;
    }

    fn go(&mut self, i: usize, map: &mut Map, area: Rect) {
        let Some(row) = self.rows.get(i).cloned() else {
            return;
        };
        match row.bounds.filter(|b| b.e - b.w > 1e-6 || b.n - b.s > 1e-6) {
            Some(b) => {
                let z = search::fit_zoom(
                    &b,
                    f64::from(area.width()),
                    f64::from(area.height()),
                    80.0,
                    search::MAX_ZOOM,
                );
                let (cx, cy) = b.centre();
                map.go_to(cx, cy, z);
            }
            None => {
                let z = map
                    .camera()
                    .map_or(search::RESULT_ZOOM, |c| c.2)
                    .max(search::RESULT_ZOOM);
                map.go_to(row.lon, row.lat, z);
            }
        }
        map.pin = Some([row.lon, row.lat]);
        self.close();
    }

    /// Draws the trigger at `trigger` and, when open, the field and the list.
    pub fn show(&mut self, ui: &mut egui::Ui, trigger: Rect, area: Rect, map: &mut Map, units: &Units) {
        let ctx = ui.ctx().clone();
        let now = ui.input(|i| i.time);
        if let Some(r) = self.lookup.as_ref().and_then(Pending::take) {
            self.lookup = None;
            let asked = self.asked.clone();
            if asked == self.text.trim() {
                match r {
                    Ok(doc) => {
                        let (found, error) = search::read_geocode(&doc);
                        let mut all = self.local.clone();
                        all.extend(found);
                        all.truncate(MAX_ROWS);
                        let note = if all.is_empty() {
                            if error.is_empty() {
                                "Nothing found.".to_string()
                            } else {
                                error
                            }
                        } else {
                            String::new()
                        };
                        self.set_rows(all, note);
                    }
                    Err(e) => {
                        let note = if self.local.is_empty() {
                            if e.is_empty() {
                                "The place search failed.".into()
                            } else {
                                e
                            }
                        } else {
                            String::new()
                        };
                        self.set_rows(self.local.clone(), note);
                    }
                }
            }
        }
        if self.open {
            if let Some(t) = self.edited_at {
                if now - t >= DEBOUNCE {
                    self.lookup_now(&ctx, map);
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(60));
                }
            }
        }

        let painter = ui.painter().clone();
        let resp = ui.interact(trigger, ui.scope_id().with("searchtrigger"), egui::Sense::click());
        let (fill, color) = if self.open {
            (style::accent_dark(), style::accent_bright())
        } else if resp.hovered() {
            (style::surface_hover(), style::text_1())
        } else {
            (egui::Color32::TRANSPARENT, style::text_2())
        };
        style::glass_panel(&painter, ("trigger", 1), trigger, style::RADIUS, 0.55);
        painter.rect_filled(trigger, style::RADIUS, fill);
        icons::paint(
            ui,
            "search",
            Rect::from_center_size(trigger.center(), Vec2::splat(17.0)),
            color,
        );
        resp.clone().on_hover_text("Search for a place");
        if resp.clicked() {
            if self.open {
                self.close();
            } else {
                self.open = true;
                self.fresh = true;
                self.changed(map, now);
            }
        }
        if !self.open {
            return;
        }

        let vw = ctx.content_rect().width();
        let fw = 272.0f32.min(0.44 * vw);
        let field = Rect::from_min_size(
            Pos2::new(trigger.left() - GAP - fw, trigger.top()),
            Vec2::new(fw, crate::map::rail::btn()),
        );
        let focused_id = ui.scope_id().with("searchinput");
        egui::Area::new(ui.scope_id().with("searchbox"))
            .order(egui::Order::Foreground)
            .fixed_pos(field.min)
            .show(&ctx, |ui| {
                let painter = ui.painter().clone();
                let clear = Rect::from_center_size(
                    Pos2::new(field.right() - 6.0 - 11.0, field.center().y),
                    Vec2::splat(22.0),
                );
                let r = TextField::new(&mut self.text)
                    .surface(Surface::Floating {
                        rest: 0.72,
                        focus: 0.88,
                    })
                    .named("searchfield")
                    .hint("Place or coordinates")
                    .icon("search", 14.0, 9.0, 7.0)
                    .pad(30.0, 6.0 + 22.0 + 2.0)
                    .show_with_id(ui, field, focused_id);
                if self.fresh {
                    self.fresh = false;
                    r.request_focus();
                    if let Some(mut st) = egui::TextEdit::load_state(&ctx, focused_id) {
                        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(self.text.chars().count()),
                        )));
                        st.store(&ctx, focused_id);
                    }
                }
                if r.changed() {
                    self.changed(map, now);
                }
                if !self.text.is_empty() {
                    let hot = ui.rect_contains_pointer(clear);
                    if hot {
                        listrow::paint(&painter, clear, style::RADIUS_BTN, Mark::Hover);
                    }
                    icons::paint(
                        ui,
                        "x",
                        Rect::from_center_size(clear.center(), Vec2::splat(13.0)),
                        if hot { style::text_1() } else { style::text_3() },
                    );
                    if ui
                        .interact(clear, ui.scope_id().with("searchclear"), egui::Sense::click())
                        .clicked()
                    {
                        self.text.clear();
                        self.changed(map, now);
                        r.request_focus();
                    }
                }
                if r.has_focus() {
                    let (down, up, esc) = ui.input_mut(|i| {
                        (
                            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                            i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                        )
                    });
                    if !self.rows.is_empty() && (down || up) {
                        let n = self.rows.len();
                        let i = self.index.unwrap_or(0);
                        self.index = Some(if down { (i + 1) % n } else { (i + n - 1) % n });
                    }
                    if esc {
                        self.close();
                        return;
                    }
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let text = self.text.trim().to_string();
                    if self.edited_at.is_some() && !text.is_empty() && search::parse_coordinates(&text).is_none() {
                        self.lookup_now(&ctx, map);
                        r.request_focus();
                    } else {
                        let i = self.index.unwrap_or(0);
                        self.go(i, map, area);
                        return;
                    }
                }
                self.list(&ctx, field, area, map, units);
            });
        let outside = ui.input(|i| i.pointer.any_pressed())
            && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| {
                !field.contains(p) && !trigger.contains(p) && !self.list_at.unwrap_or(Rect::NOTHING).contains(p)
            });
        if outside {
            self.close();
        }
    }

    /// `.map-search-results`: one row per answer, or the note.
    fn list(&mut self, ctx: &egui::Context, field: Rect, area: Rect, map: &mut Map, units: &Units) {
        let from = map.camera().map(|c| [c.0, c.1]);
        let items: Vec<menu::Item> = if self.rows.is_empty() {
            if self.note.is_empty() {
                Vec::new()
            } else {
                vec![menu::Item::Note(self.note.clone())]
            }
        } else {
            self.rows
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    let icon = match row.kind.as_str() {
                        "coordinates" => "crosshair",
                        "downloaded area" => "frame",
                        _ => "map-pin",
                    };
                    let (name, context) = if row.local {
                        (row.label.clone(), String::new())
                    } else {
                        search::split_label(&row.label)
                    };
                    let detail = [context, row.kind.clone()]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" \u{b7} ");
                    let dist = from
                        .map(|f| units.format_distance_coarse(measure::distance_m(f, [row.lon, row.lat])))
                        .unwrap_or_default();
                    menu::Row::new(i.to_string(), name)
                        .icon(icon)
                        .note(detail)
                        .trail(dist)
                        .into()
                })
                .collect()
        };
        if items.is_empty() {
            self.list_at = None;
            return;
        }
        let selected = self.index.map(|i| i.to_string()).unwrap_or_default();
        let id = egui::Id::NULL.with("searchlist");
        let out = menu::Menu::new(id, menu::Anchor::below_by(field, GAP), &menu::Look::SEARCH, &items)
            .selected(&selected)
            .opacity(if self.busy { 0.55 } else { 1.0 })
            .show(ctx);
        self.list_at = out.rect;
        if let Some(i) = out.picked.and_then(|v| v.parse::<usize>().ok()) {
            self.go(i, map, area);
        }
    }
}
