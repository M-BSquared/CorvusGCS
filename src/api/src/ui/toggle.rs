//! The two-state controls: the checkbox (`.link-toggle`) and the switch
//! (`.ui-toggle`).

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::ui::card::{self, Look};
use crate::ui::style::{self, Weight};

/// The 14 px square of a checkbox: the accent with a tick when on, the
/// sunken surface with a strong border when off.
pub fn check_box(painter: &egui::Painter, bx: Rect, on: bool, accent: Color32) {
    if on {
        painter.rect_filled(bx, 3, accent);
        let a = bx.left_top() + Vec2::new(3.2, 7.2);
        let b = bx.left_top() + Vec2::new(6.0, 10.0);
        let c = bx.left_top() + Vec2::new(11.0, 4.2);
        painter.line(vec![a, b, c], Stroke::new(1.8, Color32::WHITE));
    } else {
        card::paint(painter, bx, Look::custom(style::surface_1(), style::border_strong(), 3));
    }
}

/// A `.link-toggle`: a 14 px checkbox in the accent (or the warning colour)
/// and its 12 px label. Returns true when flipped.
pub fn checkbox(
    ui: &mut egui::Ui,
    at: Pos2,
    width: f32,
    label: &str,
    on: &mut bool,
    warn: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    let g = style::galley(ui, label, 12.0, Weight::W400, style::text_1(), 0.0);
    let h = g.size().y.max(14.0);
    let rect = Rect::from_min_size(at, Vec2::new(width, h));
    let resp = ui.interact(rect, ui.scope_id().with(id), egui::Sense::click());
    let painter = ui.painter();
    let bx = Rect::from_min_size(Pos2::new(at.x, at.y + (h - 14.0) / 2.0), Vec2::splat(14.0));
    let accent = if warn { style::warning() } else { style::accent() };
    check_box(painter, bx, *on, accent);
    painter.galley(
        Pos2::new(bx.right() + 8.0, at.y + (h - g.size().y) / 2.0),
        g,
        style::text_1(),
    );
    if resp.clicked() {
        *on = !*on;
        return true;
    }
    false
}

/// A `.ui-toggle`: a 38 by 22 pill, the accent when on, with a 16 px knob.
/// Drawn at `at`; returns true when pressed (the caller flips what it holds).
pub fn switch(
    ui: &mut egui::Ui,
    at: Pos2,
    on: bool,
    enabled: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    let rect = Rect::from_min_size(at, Vec2::new(38.0, 22.0));
    let resp = ui.interact(
        rect,
        ui.scope_id().with(id),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let fade = if enabled { 1.0 } else { 0.5 };
    let (fill, line) = if on {
        (style::accent(), style::accent())
    } else if resp.hovered() && enabled {
        (style::input(0.6), style::border_strong())
    } else {
        (style::input(0.6), style::border())
    };
    let painter = ui.painter();
    card::paint(
        painter,
        rect,
        Look::custom(fill.gamma_multiply(fade), line.gamma_multiply(fade), 255),
    );
    let knob = if on { style::text_inverted() } else { style::text_3() };
    let slide = ui.ctx().animate_bool_with_time(resp.id.with("knob"), on, 0.13);
    let painter = ui.painter();
    painter.circle_filled(
        Pos2::new(rect.left() + 11.0 + 16.0 * slide, rect.center().y),
        8.0,
        knob.gamma_multiply(fade),
    );
    enabled && resp.clicked()
}
