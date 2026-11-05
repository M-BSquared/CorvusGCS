//! `.fa-btn`: the icon over a caption on a glass bar (python/src/css/main.css), as
//! the Home flight bar and the planner's tool bar set them. The bar's own
//! glass, its padding and the fitting of its buttons to the room are the
//! caller's (`home::flight::fit_bar`); this paints one button.

use eframe::egui::{self, Color32, Rect, Stroke, Vec2};

use crate::ui::icons;
use crate::ui::style::{self, Weight};

/// What a button shows besides its icon and caption.
#[derive(Debug, Clone, Copy, Default)]
pub struct Look {
    /// `.active`: the armed tool, filled in the accent.
    pub active: bool,
    /// `.is-blocked`: 40 % and pressable, for the reason it is blocked.
    pub blocked: bool,
    /// `.is-next`: outlined in the accent, the step the eye should land on.
    pub next: bool,
    /// `.armed`: the Arm button while the aircraft is armed, filled green.
    pub armed: bool,
    /// A command is in flight: 50 %, no hover and a pulsing ring.
    pub busy: bool,
    /// Nothing to press: 40 % and no click, only the hint.
    pub disabled: bool,
}

/// One button in `r`; true when pressed. `fit` is the bar's caption scale and
/// whether it has dropped its captions.
#[allow(clippy::too_many_arguments)]
pub fn button(
    ui: &mut egui::Ui,
    r: Rect,
    icon: &'static str,
    caption: &str,
    title: &str,
    look: Look,
    fit: (f32, bool),
    id: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    let sense = if look.disabled {
        egui::Sense::hover()
    } else {
        egui::Sense::click()
    };
    let resp = ui.interact(r, ui.scope_id().with(id), sense);
    let resp = if title.is_empty() {
        resp
    } else {
        resp.on_hover_text(title)
    };
    let hovered = resp.hovered() && !look.disabled && !look.busy;
    let (fill, color) = if look.armed {
        (
            if hovered {
                style::healthy_bright()
            } else {
                style::healthy()
            },
            style::text_inverted(),
        )
    } else if look.active {
        (
            if hovered {
                style::accent_hover()
            } else {
                style::accent()
            },
            style::text_inverted(),
        )
    } else if look.next {
        (
            if hovered {
                style::surface_hover()
            } else {
                Color32::TRANSPARENT
            },
            style::accent_bright(),
        )
    } else if hovered && !look.blocked {
        (style::surface_hover(), style::text_1())
    } else {
        (Color32::TRANSPARENT, style::text_2())
    };
    let alpha = if look.disabled || look.blocked {
        0.4
    } else if look.busy {
        0.5
    } else {
        1.0
    };
    let painter = ui.painter();
    painter.rect_filled(r, style::RADIUS_BTN, fill.gamma_multiply(alpha));
    if look.next && !look.active {
        painter.rect_stroke(
            r.shrink(0.75),
            style::RADIUS_BTN,
            Stroke::new(1.5, style::accent()),
            egui::StrokeKind::Inside,
        );
    }
    let (scale, compact) = fit;
    if compact {
        icons::paint(
            ui,
            icon,
            Rect::from_center_size(r.center(), Vec2::splat(17.0)),
            color.gamma_multiply(alpha),
        );
    } else {
        let g = style::galley(
            ui,
            caption,
            10.0 * scale,
            Weight::W600,
            color.gamma_multiply(alpha),
            0.6 * scale,
        );
        let top = r.center().y - (17.0 + 3.0 + g.size().y) / 2.0;
        icons::paint(
            ui,
            icon,
            Rect::from_center_size(egui::Pos2::new(r.center().x, top + 8.5), Vec2::splat(17.0)),
            color.gamma_multiply(alpha),
        );
        ui.painter().galley(
            egui::Pos2::new(r.center().x - g.size().x / 2.0, top + 20.0),
            g,
            color.gamma_multiply(alpha),
        );
    }
    if look.busy {
        let t = ui.input(|i| i.time) as f32;
        let pulse = 0.675 + 0.325 * (t / 1.6 * std::f32::consts::TAU).cos();
        ui.painter().rect_stroke(
            r,
            style::RADIUS_BTN,
            Stroke::new(2.0, style::accent_bright().gamma_multiply(pulse)),
            egui::StrokeKind::Outside,
        );
        ui.ctx().request_repaint();
    }
    resp.clicked()
}
