//! `.ui-slider`: the stepped slider.

use eframe::egui::{self, Pos2, Rect, Vec2};

use crate::ui::style::{self, Weight};

/// `.ui-slider`: a stepped slider with a mark and a caption per step. Returns
/// the bottom, then the step index while it is dragged or clicked and whether
/// the pointer was released this frame.
pub fn show(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    id: &str,
    steps: &[String],
    current: usize,
) -> (f32, Option<(usize, bool)>) {
    let thumb = 16.0;
    let row = Rect::from_min_size(Pos2::new(x, y + 4.0), Vec2::new(w, thumb));
    let n = steps.len().max(2) as f32 - 1.0;
    let pos_of = |i: usize| row.left() + thumb / 2.0 + (w - thumb) * i as f32 / n;
    let resp = ui.interact(
        style::finger(row),
        ui.scope_id().with(("slider", id)),
        egui::Sense::click_and_drag(),
    );
    let mut out = None;
    if resp.dragged() || resp.clicked() || resp.drag_stopped() {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = ((p.x - row.left() - thumb / 2.0) / (w - thumb)).clamp(0.0, 1.0);
            let i = (t * n).round() as usize;
            out = Some((i, resp.drag_stopped() || resp.clicked()));
        }
    }
    let shown = out.map_or(current, |o| o.0);
    let p = ui.painter();
    let cy = row.center().y;
    p.rect_filled(
        Rect::from_min_max(Pos2::new(row.left(), cy - 2.0), Pos2::new(row.right(), cy + 2.0)),
        2,
        style::surface_3(),
    );
    let fill_to = pos_of(shown);
    p.rect_filled(
        Rect::from_min_max(Pos2::new(row.left(), cy - 2.0), Pos2::new(fill_to, cy + 2.0)),
        2,
        style::accent(),
    );
    for i in 0..steps.len() {
        let x = pos_of(i);
        let color = if i <= shown {
            style::text_inverted().gamma_multiply(0.6)
        } else {
            style::border_strong()
        };
        p.rect_filled(Rect::from_center_size(Pos2::new(x, cy), Vec2::new(2.0, 9.0)), 1, color);
    }
    p.circle_filled(Pos2::new(fill_to, cy + 1.0), thumb / 2.0, style::tint(0.12));
    p.circle_filled(Pos2::new(fill_to, cy), thumb / 2.0, style::surface_1());
    p.circle_filled(Pos2::new(fill_to, cy), thumb / 2.0 - 2.0, style::accent());
    let label_y = row.bottom() + 4.0 + 2.0 - 4.0;
    for (i, s) in steps.iter().enumerate() {
        let c = if i == shown { style::accent() } else { style::text_3() };
        let g = style::galley(ui, s, 9.5, Weight::W600, c, 0.3);
        let w = g.size().x + 4.0;
        let lr = Rect::from_min_size(Pos2::new(pos_of(i) - w / 2.0, label_y), Vec2::new(w, 15.0));
        let r = ui.interact(lr, ui.scope_id().with(("sliderstep", id, i)), egui::Sense::click());
        let c = if r.hovered() && i != shown { style::text_2() } else { c };
        let g = style::galley(ui, s, 9.5, Weight::W600, c, 0.3);
        ui.painter()
            .galley(Pos2::new(lr.left() + 2.0, lr.center().y - g.size().y / 2.0), g, c);
        if r.clicked() {
            out = Some((i, true));
        }
    }
    (label_y + 15.0, out)
}
