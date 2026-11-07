//! The facade of the shared controls (python/src/css/components.css). Each
//! lives in its own file; `kit::` keeps every path a page already uses valid:
//!
//! * `button.rs`: `Button`, `Variant`, `icon_btn` (`.btn`, `.icon-btn`)
//! * `select.rs`: `Choice`, `select`, `select_padded` (`.field-select`)
//! * `toggle.rs`: `checkbox`, `switch` (`.link-toggle`, `.ui-toggle`)
//! * `text.rs`: `wrapped`, `wrapped_weight`, `dot`
//! * `field.rs`: `text_field`, `text_field_padded` (`.field-input`)

use eframe::egui;

pub use crate::ui::button::{icon_btn, Button, Variant};
pub use crate::ui::field::{text_field, text_field_padded};
pub use crate::ui::select::{select, select_padded, Choice};
pub use crate::ui::text::{dot, wrapped, wrapped_weight};
pub use crate::ui::toggle::{checkbox, switch};

/// `corvus-enter`: 0 to 1 over `secs` (ease out, no overshoot) from the frame
/// a surface starts being drawn; it starts again when the surface was absent
/// for a few frames. Asks for repaints while it runs.
pub fn enter(ctx: &egui::Context, key: impl std::hash::Hash + std::fmt::Debug, secs: f64) -> f32 {
    let id = egui::Id::NULL.with(("corvus-enter", key));
    let now = ctx.input(|i| i.time);
    let (start, seen) = ctx.data(|d| d.get_temp::<(f64, f64)>(id)).unwrap_or((now, now));
    let start = if now - seen > 0.3 { now } else { start };
    ctx.data_mut(|d| d.insert_temp(id, (start, now)));
    let t: f32 = ((now - start) / secs).clamp(0.0, 1.0) as f32;
    if t < 1.0 {
        ctx.request_repaint();
    }
    1.0 - (1.0 - t).powi(3)
}

/// Applies `enter`'s progress to a layer: fades it in and lifts it from 4 px
/// below.
pub fn enter_layer(ctx: &egui::Context, layer: egui::LayerId, rise: f32) {
    ctx.set_transform_layer(
        layer,
        egui::emath::TSTransform::from_translation(egui::vec2(0.0, 4.0 * (1.0 - rise))),
    );
}
