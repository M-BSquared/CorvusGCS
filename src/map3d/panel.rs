//! The 3D panel beside the map rail's cube button (`python/src/js/map.js`
//! `renderThreeDRows`, map.css `.three-d-popover`): the buildings switch with
//! its hint, and the list of elevation models. The rail opens it; what is
//! picked here is kept in the config like the web map keeps it.

use eframe::egui::{self, Pos2, Rect, Vec2};
use serde_json::Value;

use crate::map3d::Map3d;
use crate::ui::kit;
use crate::ui::menu;
use crate::ui::style::{self, Weight};

const WIDTH: f32 = 260.0;
const HINT: &str = "OpenStreetMap buildings, kept for offline use. Relief is always shown. Own elevation profiles, GeoTIFF and surface model (DOM) data can be added in the settings.";

/// What the panel did this frame.
#[derive(Debug, Default, PartialEq)]
pub struct Out {
    /// A press outside it, or Escape.
    pub close: bool,
}

fn head(ui: &egui::Ui, x: f32, y: f32, text: &str) -> f32 {
    let g = style::galley(ui, &text.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.8);
    let h = g.size().y;
    ui.painter().galley(Pos2::new(x + 6.0, y + 4.0), g, style::text_3());
    y + 4.0 + h + 6.0
}

/// Draws the panel left of the rail button `anchor`.
pub fn show(ui: &mut egui::Ui, anchor: Rect, t: &mut Map3d, sources: &Value) -> Out {
    let rows = t.elevation_rows(sources);
    let hint_h = kit::wrapped_weight(ui, HINT, 10.5, Weight::W500, style::text_3(), WIDTH - 22.0, 1.4)
        .size()
        .y;
    let row_h = 7.0 * 2.0 + 12.0 * 1.21;
    let head_h = 4.0 + 10.0 * 1.21 + 6.0;
    let field_h = 2.0 + 22.0 + 6.0 + 6.0 + hint_h;
    let h = 5.0
        + head_h
        + field_h
        + if rows.is_empty() {
            0.0
        } else {
            head_h + rows.len() as f32 * row_h
        }
        + 5.0;
    let menu = Rect::from_min_size(
        Pos2::new(anchor.left() - 7.0 - 8.0 - WIDTH, anchor.top() - 7.0),
        Vec2::new(WIDTH, h),
    );
    let ctx = ui.ctx().clone();
    let mut picked = None;
    let mut flip = false;
    let mut out = Out::default();
    egui::Area::new(ui.scope_id().with("threed"))
        .order(egui::Order::Foreground)
        .fixed_pos(menu.min)
        .show(&ctx, |ui| {
            let painter = ui.painter().clone();
            style::glass_panel(&painter, "threed", menu, style::RADIUS, 0.6);
            ui.allocate_rect(menu, egui::Sense::hover());
            let x = menu.left() + 5.0;
            let mut y = head(ui, x, menu.top() + 5.0, "3D mode");
            let lg = style::galley(ui, "Buildings", 12.0, Weight::W600, style::text_1(), 0.0);
            painter.galley(
                Pos2::new(x + 6.0, y + 2.0 + (22.0 - lg.size().y) / 2.0),
                lg,
                style::text_1(),
            );
            if kit::switch(
                ui,
                Pos2::new(menu.right() - 5.0 - 6.0 - 38.0, y + 2.0),
                t.full,
                true,
                "threedbuildings",
            ) {
                flip = true;
            }
            y += 2.0 + 22.0 + 6.0;
            let hg = kit::wrapped_weight(ui, HINT, 10.5, Weight::W500, style::text_3(), WIDTH - 22.0, 1.4);
            painter.galley(Pos2::new(x + 6.0, y), hg.clone(), style::text_3());
            y += hg.size().y + 6.0;
            if !rows.is_empty() {
                y = head(ui, x, y, "Elevation");
                for (id, label) in &rows {
                    let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(WIDTH - 10.0, row_h));
                    let active = *id == t.choice || (t.choice.is_empty() && id == "auto");
                    if menu::row(ui, r, ("elev", id.as_str()), label, active, true, &menu::Look::PANEL) {
                        picked = Some(id.clone());
                    }
                    y += row_h;
                }
            }
            let outside = ui.input(|i| i.pointer.any_pressed())
                && !ui
                    .input(|i| i.pointer.interact_pos())
                    .is_some_and(|p| menu.contains(p) || anchor.contains(p));
            out.close = outside || ui.input(|i| i.key_pressed(egui::Key::Escape));
        });
    if let Some(id) = picked {
        t.set_choice(&id);
    }
    if flip {
        t.set_full(!t.full);
    }
    out
}
