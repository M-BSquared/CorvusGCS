//! The registry as the operator sees it: the cards under PLUGINS and the
//! opened plugin (`renderGrid` and `renderPluginView` of
//! `python/src/js/plugins.js`, `.plugin-*` of `panel.css`), a plugin's tab,
//! and the options dialog opened from its gear in Settings
//! (`openPluginOptions` of `python/src/js/sidenav.js`).

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use super::options::{Kind, OptionSpec};
use super::Registry;
use crate::ui::form::{self, Form};
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::message::Tone;
use crate::ui::style::{self, Weight};
use crate::ui::{icons, modal, pagekit};
use corvus_api::plugin::number_text;

/// `.plugin-grid`: 12 px around, 10 px between, columns of at least 200 px.
const GRID_PAD: f32 = 12.0;
const GRID_GAP: f32 = 10.0;
const GRID_MIN: f32 = 200.0;
/// `.plugin-card`: 14 px padding, 8 px between the icon and the text.
const CARD_PAD: f32 = 14.0;

/// How many columns of `minmax(200px, 1fr)` fit in `w`.
pub fn columns(w: f32) -> usize {
    (((w + GRID_GAP) / (GRID_MIN + GRID_GAP)).floor() as usize).max(1)
}

impl Registry {
    /// The PLUGINS tab: the cards, or the opened plugin.
    pub fn show_cards(&mut self, ui: &mut egui::Ui, rect: Rect, state: &Value) {
        let ctx = ui.ctx().clone();
        if let Some(i) = self.open {
            if self.in_tab(i) {
                self.close_card(&ctx, state);
            }
        }
        match self.open {
            Some(i) => self.show_opened(ui, rect, state, i),
            None => self.show_grid(ui, rect, state),
        }
    }

    fn show_grid(&mut self, ui: &mut egui::Ui, rect: Rect, state: &Value) {
        let shown: Vec<usize> = (0..self.entries.len()).filter(|&i| !self.in_tab(i)).collect();
        if shown.is_empty() {
            crate::panel::plugins_empty(
                ui,
                rect,
                if self.entries.is_empty() {
                    "Extensions and tools plug in here."
                } else {
                    "Every installed plugin has a tab of its own."
                },
            );
            return;
        }
        let ctx = ui.ctx().clone();
        let mut open = None;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        egui::ScrollArea::vertical()
            .id_salt("plugingrid")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                let top = ui.max_rect().top();
                let w = rect.width() - 2.0 * GRID_PAD;
                let cols = columns(w);
                let cw = (w - GRID_GAP * (cols as f32 - 1.0)) / cols as f32;
                let mut y = top + GRID_PAD;
                for row in shown.chunks(cols) {
                    let heights: Vec<f32> = row.iter().map(|&i| card_height(ui, self.entries[i].info, cw)).collect();
                    let rh = heights.iter().copied().fold(0.0, f32::max);
                    for (n, &i) in row.iter().enumerate() {
                        let r = Rect::from_min_size(
                            Pos2::new(rect.left() + GRID_PAD + n as f32 * (cw + GRID_GAP), y),
                            Vec2::new(cw, rh),
                        );
                        if card(ui, r, self.entries[i].info) {
                            open = Some(i);
                        }
                    }
                    y += rh + GRID_GAP;
                }
                ui.allocate_space(Vec2::new(w, y - top - GRID_GAP + GRID_PAD));
            });
        if let Some(i) = open {
            self.open_card(i, &ctx, state);
        }
    }

    fn show_opened(&mut self, ui: &mut egui::Ui, rect: Rect, state: &Value, i: usize) {
        let ctx = ui.ctx().clone();
        let info = self.entries[i].info;
        let (bh, back) = pagekit::back_button(ui, Pos2::new(rect.left() + 10.0, rect.top() + 10.0), "Plugins");
        if back {
            self.close_card(&ctx, state);
            return;
        }
        // .plugin-header: 12 14 10 padding, a 30 px icon and the name
        let hy = rect.top() + 10.0 + bh;
        let icon = Rect::from_min_size(Pos2::new(rect.left() + CARD_PAD, hy + 12.0), Vec2::splat(30.0));
        ui.painter().rect_filled(icon, style::RADIUS_BTN, style::accent_dark());
        icons::paint(
            ui,
            info.icon,
            Rect::from_center_size(icon.center(), Vec2::splat(16.0)),
            style::accent_bright(),
        );
        let g = style::galley(ui, info.name, 14.0, Weight::W600, style::text_1(), 0.3);
        ui.painter().galley(
            Pos2::new(icon.right() + 9.0, icon.center().y - g.size().y / 2.0),
            g,
            style::text_1(),
        );
        let body = Rect::from_min_max(Pos2::new(rect.left(), icon.bottom() + 10.0), rect.max);
        self.show_body(ui, body, state, i, 0.0);
    }

    /// A plugin that has a tab: the ruled header, then the plugin.
    pub fn show_own(&mut self, ui: &mut egui::Ui, rect: Rect, state: &Value, i: usize) {
        let name = self.entries[i].info.name;
        let header = crate::panel::link::section_header(ui, rect, name, 0.0);
        let body = Rect::from_min_max(Pos2::new(rect.left(), header.bottom()), rect.max);
        self.ensure_live(i, &ui.ctx().clone(), state);
        self.show_body(ui, body, state, i, 12.0);
    }

    /// `.plugin-container`: the plugin's own area, scrolling, 14 px at the sides.
    fn show_body(&mut self, ui: &mut egui::Ui, body: Rect, state: &Value, i: usize, pad_top: f32) {
        let ctx = ui.ctx().clone();
        self.ensure_live(i, &ctx, state);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
        child.set_clip_rect(body);
        egui::ScrollArea::vertical()
            .id_salt(("pluginbody", self.entries[i].info.id))
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                let top = ui.max_rect().top() + pad_top;
                let area = Rect::from_min_size(
                    Pos2::new(body.left() + CARD_PAD, top),
                    Vec2::new(body.width() - 2.0 * CARD_PAD - 8.0, 0.0),
                );
                let h = self.with(i, &ctx, state, |p, h| p.show(ui, area, h));
                ui.allocate_space(Vec2::new(area.width(), pad_top + h + CARD_PAD));
            });
    }

    /// The dialog behind a plugin's gear. Every change is in force and saved
    /// at once; a save that fails puts the control back and says why.
    /// Returns true once it is closed.
    pub fn options_dialog(&mut self, ctx: &egui::Context, state: &Value, i: usize) -> bool {
        let (schema, values) = self.option_form(i);
        let title = self.entries[i].info.name;
        let id = self.entries[i].info.id;
        let msg = self.option_error.clone();
        let mut change: Option<(String, Value)> = None;
        let mut done = false;
        let mut closed = modal::show(ctx, &format!("plugin-{id}"), modal::Size::Md, title, |ui, x, y, w| {
            let mut f = Form::new(x, y, w);
            for (n, spec) in schema.iter().enumerate() {
                f.skip(if n == 0 { 0.0 } else { 14.0 });
                if let Some(v) = option_field(ui, &mut f, id, spec, values.get(&spec.key)) {
                    change = Some((spec.key.clone(), v));
                }
            }
            if let Some(m) = &msg {
                f.skip(12.0);
                form::message(ui, &mut f, m, Tone::Err);
            }
            f.skip(20.0);
            let (hit, bottom) = modal::actions(ui, x, f.y, w, vec![Button::new("Done", Variant::Primary)]);
            done = hit == Some(0);
            bottom
        });
        closed |= done;
        if let Some((key, value)) = change {
            self.option_error = None;
            if let Err(e) = self.set_option(ctx, state, i, &key, &value) {
                self.option_error = Some(e);
            }
        }
        closed
    }
}

/// `.plugin-card` over a `.tile`: the icon, the name and the description,
/// stacked.
fn card(ui: &mut egui::Ui, rect: Rect, info: super::Info) -> bool {
    let resp = ui.interact(rect, ui.scope_id().with(("plugincard", info.id)), egui::Sense::click());
    let (fill, stroke) = if resp.hovered() {
        (style::glass(0.95), style::tint(0.15))
    } else {
        (style::glass(0.82), style::tint(0.09))
    };
    let p = ui.painter().clone();
    p.rect_filled(rect, style::RADIUS, fill);
    p.rect_stroke(rect, style::RADIUS, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    let inner = rect.shrink(CARD_PAD);
    let tile = Rect::from_min_size(inner.min, Vec2::splat(34.0));
    p.rect_filled(tile, style::RADIUS_BTN, style::accent_dark());
    icons::paint(
        ui,
        info.icon,
        Rect::from_center_size(tile.center(), Vec2::splat(17.0)),
        style::accent_bright(),
    );
    let t = style::galley(ui, info.name, 13.0, Weight::W600, style::text_1(), 0.2);
    let ty = tile.bottom() + 8.0;
    p.galley(Pos2::new(inner.left(), ty), t.clone(), style::text_1());
    let d = kit::wrapped_weight(
        ui,
        info.description,
        11.0,
        Weight::W400,
        style::text_3(),
        inner.width(),
        1.45,
    );
    p.galley(Pos2::new(inner.left(), ty + t.size().y + 3.0), d, style::text_3());
    resp.clicked()
}

fn card_height(ui: &egui::Ui, info: super::Info, w: f32) -> f32 {
    let t = style::galley(ui, info.name, 13.0, Weight::W600, style::text_1(), 0.2);
    let d = kit::wrapped_weight(
        ui,
        info.description,
        11.0,
        Weight::W400,
        style::text_3(),
        w - 2.0 * CARD_PAD,
        1.45,
    );
    2.0 * CARD_PAD + 34.0 + 8.0 + t.size().y + 3.0 + d.size().y
}

/// One option as a field of the dialog (`pluginOptionField`). Returns the
/// value to save when the operator changed it.
fn option_field(
    ui: &mut egui::Ui,
    f: &mut Form,
    plugin: &str,
    spec: &OptionSpec,
    value: Option<&Value>,
) -> Option<Value> {
    let key = (plugin.to_string(), spec.key.clone());
    let mut out = None;
    match &spec.kind {
        Kind::Toggle => {
            let on = value.and_then(Value::as_bool).unwrap_or(false);
            if let Some(next) = form::switch(ui, f, ("pluginopt", key), &spec.label, &spec.hint, on) {
                out = Some(json!(next));
            }
        }
        Kind::Select(choices) => {
            let current = value.and_then(Value::as_str).unwrap_or("").to_string();
            let list: Vec<Choice> = choices
                .iter()
                .map(|(v, l)| Choice::new(v.as_str(), l.as_str()))
                .collect();
            form::field(ui, f, &spec.label, "", &spec.hint, |ui, x, y, w| {
                let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, form::CONTROL_H));
                if let Some(v) = kit::select(ui, r, &list, &current, "", false, "", ("pluginopt", &key)) {
                    out = Some(json!(v));
                }
                form::CONTROL_H
            });
        }
        Kind::Text { placeholder } => {
            let shown = value.and_then(Value::as_str).unwrap_or("").to_string();
            form::field(ui, f, &spec.label, "", &spec.hint, |ui, x, y, w| {
                let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, form::CONTROL_H));
                if let Some(t) = crate::ui::field::committed(ui, r, ("pluginopt", &key), &shown, placeholder, false) {
                    out = Some(json!(t));
                }
                form::CONTROL_H
            });
        }
        Kind::Number { .. } => {
            let shown = value.and_then(Value::as_f64).map(number_text).unwrap_or_default();
            form::field(ui, f, &spec.label, "", &spec.hint, |ui, x, y, w| {
                let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, form::CONTROL_H));
                if let Some(t) = crate::ui::field::committed(ui, r, ("pluginopt", &key), &shown, "", true) {
                    out = Some(json!(t));
                }
                form::CONTROL_H
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_has_as_many_columns_of_at_least_200_px_as_fit() {
        assert_eq!(columns(300.0), 1);
        assert_eq!(columns(409.0), 1);
        assert_eq!(columns(410.0), 2);
        assert_eq!(columns(620.0), 3);
        assert_eq!(columns(50.0), 1);
    }

    #[test]
    fn numbers_print_without_a_trailing_zero() {
        assert_eq!(number_text(5.0), "5");
        assert_eq!(number_text(2.5), "2.5");
        assert_eq!(number_text(-3.0), "-3");
    }
}
