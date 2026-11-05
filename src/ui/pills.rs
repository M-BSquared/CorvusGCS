//! `.settings-tab`: the row of icon pills of the Settings page.

use eframe::egui::{self, Color32, Pos2, Rect, Vec2};

use crate::ui::card::{self, Look};
use crate::ui::icons;
use crate::ui::kit;
use crate::ui::style::{self, Weight};

/// `.settings-tab`: a row of pills with an icon each, wrapping. Returns the
/// bottom and the index pressed. `dots` marks a pill with the notice dot (a release waiting).
pub fn show(
    ui: &mut egui::Ui,
    left: f32,
    top: f32,
    w: f32,
    items: &[(&str, &'static str)],
    selected: usize,
    dots: &[bool],
) -> (f32, Option<usize>) {
    let mut x = left;
    let mut y = top;
    let mut hit = None;
    let mut line_h = 0.0f32;
    for (i, (label, icon)) in items.iter().enumerate() {
        let on = i == selected;
        let color = if on { style::text_1() } else { style::text_3() };
        let g = style::galley(ui, label, 11.5, Weight::W600, color, 0.0);
        let pw = 2.0 + 11.0 + 14.0 + 7.0 + g.size().x + 13.0;
        let h = 2.0 + 14.0 + g.size().y.max(14.0);
        if x > left && x + pw > left + w {
            x = left;
            y += line_h + 6.0;
            line_h = 0.0;
        }
        let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(pw, h));
        let resp = ui.interact(r, ui.scope_id().with(("pill", i)), egui::Sense::click());
        let (fill, line) = if on {
            (style::accent_dim(), style::accent())
        } else if resp.hovered() {
            (style::surface_hover(), style::border_soft())
        } else {
            (Color32::TRANSPARENT, style::border_soft())
        };
        let color = if !on && resp.hovered() { style::text_1() } else { color };
        let g = style::galley(ui, label, 11.5, Weight::W600, color, 0.0);
        let p = ui.painter();
        card::paint(p, r, Look::custom(fill, line, 255));
        icons::paint(
            ui,
            icon,
            Rect::from_center_size(Pos2::new(r.left() + 12.0 + 7.0, r.center().y), Vec2::splat(14.0)),
            if on { style::accent_bright() } else { color },
        );
        ui.painter().galley(
            Pos2::new(r.left() + 12.0 + 14.0 + 7.0, r.center().y - g.size().y / 2.0),
            g,
            color,
        );
        if dots.get(i).copied().unwrap_or(false) {
            kit::dot(
                ui.painter(),
                Pos2::new(r.right() - 6.5, r.top() + 6.5),
                style::accent(),
                true,
            );
        }
        if resp.clicked() {
            hit = Some(i);
        }
        x += pw + 6.0;
        line_h = line_h.max(h);
    }
    (y + line_h, hit)
}
