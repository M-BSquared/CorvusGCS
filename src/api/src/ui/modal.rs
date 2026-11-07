//! `.modal` (components.css): a centred card on a scrim, a title row with a
//! close button, a body and a row of actions at the right.

use eframe::egui::{self, Pos2, Rect, Vec2};

use crate::ui::card;
use crate::ui::kit;
use crate::ui::style::{self, Weight};

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
    show_toned(ctx, id, size, title, false, body)
}

/// `show` with the critical cue on the title (`.modal:has(.motor-calib-warning)
/// .modal-title`): a 3 px red border to its left and 10 px of padding, for the
/// dialog that carries a danger warning.
pub fn show_toned(
    ctx: &egui::Context,
    id: &str,
    size: Size,
    title: &str,
    danger: bool,
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
    let rise = kit::enter(ctx, ("modal", id), 0.18);
    let area_id = egui::Id::NULL.with(("modal", id));
    egui::Area::new(area_id)
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            kit::enter_layer(ui.ctx(), ui.layer_id(), rise);
            ui.set_opacity(rise);
            ui.set_clip_rect(screen);
            let scrim = ui.allocate_rect(screen, egui::Sense::click());
            backdrop(ui, id, screen);
            let bg = ui.painter().add(egui::Shape::Noop);
            let (ix, iw) = (panel.left() + 20.0, w - 40.0);
            let mut cy = panel.top() + 20.0;
            let tg = style::galley(ui, title, 15.0, Weight::W700, style::text_1(), 0.3);
            let tx = if danger { ix + 3.0 + 10.0 } else { ix };
            if danger {
                ui.painter().rect_filled(
                    Rect::from_min_size(Pos2::new(ix, cy), Vec2::new(3.0, tg.size().y)),
                    0,
                    style::critical(),
                );
            }
            ui.painter().galley(Pos2::new(tx, cy), tg.clone(), style::text_1());
            let xr = Rect::from_min_size(
                Pos2::new(ix + iw - 28.0, cy + tg.size().y / 2.0 - 14.0),
                Vec2::splat(28.0),
            );
            if kit::icon_btn(ui, xr, "x", "Close", false, ("modalclose", id)) {
                close = true;
            }
            cy += tg.size().y.max(28.0) + 16.0 - (28.0 - tg.size().y).max(0.0) / 2.0;
            let max_h = (screen.height() - 32.0).max(120.0);
            let body_top = cy;
            let view_h = (max_h - (body_top - panel.top()) - 20.0).max(60.0);
            let okey = egui::Id::NULL.with(("modal-scroll", id));
            let mut offset = ui.ctx().data(|d| d.get_temp::<f32>(okey)).unwrap_or(0.0);
            let outer = ui.clip_rect();
            ui.set_clip_rect(Rect::from_min_max(
                Pos2::new(panel.left(), body_top),
                Pos2::new(panel.right(), body_top + view_h),
            ));
            style::set_in_modal(true);
            let bottom = body(ui, ix, body_top - offset, iw);
            style::set_in_modal(false);
            ui.set_clip_rect(outer);
            let content_h = bottom - (body_top - offset);
            let overflow = (content_h - view_h).max(0.0);
            if overflow > 0.0 {
                let over = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| panel.contains(p));
                if over {
                    offset -= ui.input(|i| i.smooth_scroll_delta.y);
                }
                offset = offset.clamp(0.0, overflow);
                cy = body_top + view_h;
            } else {
                offset = 0.0;
                cy = body_top + content_h;
            }
            ui.ctx().data_mut(|d| d.insert_temp(okey, offset));
            let rect = Rect::from_min_max(panel.min, Pos2::new(panel.right(), cy + 20.0));
            height = rect.height();
            ui.painter().set(
                bg,
                card::window(("modal", id), rect, style::modal_fill(), style::tint(0.06)),
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

/// `.modal-overlay`: the scene under the dialog blurred 8 px and dimmed.
pub fn backdrop(ui: &egui::Ui, id: &str, screen: Rect) {
    use std::hash::{BuildHasher, BuildHasherDefault};
    let key = BuildHasherDefault::<std::collections::hash_map::DefaultHasher>::default().hash_one(("modal-scrim", id));
    let blur = crate::ui::frost::Filter {
        blur: 8.0,
        saturate: 1.0,
        brightness: 1.0,
    };
    ui.painter().add(crate::ui::frost::copy_shape(screen, key, blur));
    ui.painter()
        .add(crate::ui::frost::draw_shape(screen, key, 0.into(), blur));
    ui.painter().rect_filled(screen, 0, style::scrim());
}

/// A row of buttons at the right of `x .. x + w`, the last one rightmost:
/// `.modal-actions`. Returns the index pressed and the bottom.
pub fn actions(ui: &mut egui::Ui, x: f32, y: f32, w: f32, buttons: Vec<kit::Button<'_>>) -> (Option<usize>, f32) {
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    let default = buttons
        .iter()
        .rposition(|b| b.enabled && matches!(b.variant, kit::Variant::Primary | kit::Variant::Danger));
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
    if enter && hit.is_none() {
        hit = default;
    }
    (hit, y + h)
}
