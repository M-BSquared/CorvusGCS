//! `.modal` (components.css): a centred card on a scrim, a title row with a
//! close button, a body and a row of actions at the right.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::kit;
use crate::style::{self, Weight};

/// `data-size`: the card's widest width.
#[derive(Clone, Copy)]
pub enum Size {
    Sm,
    Md,
    Lg,
}

impl Size {
    fn width(self) -> f32 {
        match self {
            Size::Sm => 360.0,
            Size::Md => 420.0,
            Size::Lg => 560.0,
        }
    }
}

/// Draws the dialog `id` and returns true when it asked to close (the close
/// button, the scrim, Escape). `body` draws into the column `x .. x + w`
/// from `y` and returns the bottom.
pub fn show(
    ctx: &egui::Context,
    id: &str,
    size: Size,
    title: &str,
    body: impl FnOnce(&mut egui::Ui, f32, f32, f32) -> f32,
) -> bool {
    let screen = ctx.content_rect();
    let w = size.width().min(screen.width() - 32.0);
    let hkey = egui::Id::NULL.with(("modal-h", id));
    let last_h = ctx.data(|d| d.get_temp::<f32>(hkey)).unwrap_or(200.0);
    let top = ((screen.height() - last_h) / 2.0).max(16.0);
    let panel = Rect::from_min_size(Pos2::new(screen.center().x - w / 2.0, top), Vec2::new(w, last_h));
    let mut close = false;
    let mut height = last_h;
    egui::Area::new(egui::Id::NULL.with(("modal", id)))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.set_clip_rect(screen);
            let scrim = ui.allocate_rect(screen, egui::Sense::click());
            ui.painter().rect_filled(
                screen,
                0,
                Color32::from_black_alpha(if style::is_dark() { 150 } else { 90 }),
            );
            let bg = ui.painter().add(egui::Shape::Noop);
            let (ix, iw) = (panel.left() + 20.0, w - 40.0);
            let mut cy = panel.top() + 20.0;
            let tg = style::galley(ui, title, 15.0, Weight::W700, style::text_1(), 0.3);
            ui.painter().galley(Pos2::new(ix, cy), tg.clone(), style::text_1());
            let xr = Rect::from_min_size(
                Pos2::new(ix + iw - 28.0, cy + tg.size().y / 2.0 - 14.0),
                Vec2::splat(28.0),
            );
            if kit::icon_btn(ui, xr, "x", "Close", false, ("modalclose", id)) {
                close = true;
            }
            cy += tg.size().y.max(28.0) + 16.0 - (28.0 - tg.size().y).max(0.0) / 2.0;
            cy = body(ui, ix, cy, iw);
            let rect = Rect::from_min_max(panel.min, Pos2::new(panel.right(), cy + 20.0));
            height = rect.height();
            ui.painter().set(
                bg,
                egui::Shape::Vec(vec![
                    egui::Shape::Rect(style::shadow_pop().as_shape(rect, 12)),
                    egui::Shape::rect_filled(rect, 12, style::glass(0.96)),
                    egui::Shape::rect_stroke(rect, 12, Stroke::new(1.0, style::tint(0.06)), egui::StrokeKind::Inside),
                ]),
            );
            if scrim.clicked() && !rect.contains(scrim.interact_pointer_pos().unwrap_or(Pos2::ZERO)) {
                close = true;
            }
        });
    ctx.data_mut(|d| d.insert_temp(hkey, height));
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    close
}

/// A row of buttons at the right of `x .. x + w`, the last one rightmost:
/// `.modal-actions`. Returns the index pressed and the bottom.
pub fn actions(ui: &mut egui::Ui, x: f32, y: f32, w: f32, buttons: Vec<kit::Button<'_>>) -> (Option<usize>, f32) {
    let sizes: Vec<(f32, f32)> = buttons.iter().map(|b| (b.width(ui), b.height())).collect();
    let mut right = x + w;
    let mut hit = None;
    let h = sizes.iter().map(|s| s.1).fold(0.0, f32::max);
    for (i, b) in buttons.into_iter().enumerate().rev() {
        let (bw, bh) = sizes[i];
        right -= bw;
        if b.show(
            ui,
            Rect::from_min_size(Pos2::new(right, y + (h - bh) / 2.0), Vec2::new(bw, bh)),
            ("modalbtn", i),
        ) {
            hit = Some(i);
        }
        right -= 10.0;
    }
    (hit, y + h)
}
