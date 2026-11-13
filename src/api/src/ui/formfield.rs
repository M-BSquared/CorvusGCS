//! `Corvus.ui.field` of the web interface (`.field`, `.field-label`,
//! `.field-hint`, `.field-input`, `.field-select`): a small uppercase caption,
//! the control and a hint under it, 5 px apart. The pages of Setup that are a
//! plain form (RTK, the telemetry radio, Remote ID, Video) are built from
//! these, so a caption, a box or a hint is the same on each of them.

use eframe::egui::{self, Pos2, Rect, Vec2};

use crate::ui::field::TextField;
use crate::ui::kit::{self, Choice};
use crate::ui::pathfield::{PathField, Picked};
use crate::ui::popover;
use crate::ui::style::{self, Weight};

/// A `.field-input` and `.field-select` are 32.5 px tall: 8 px of padding, a
/// 14.5 px line and the border.
pub const CONTROL_H: f32 = 32.5;

/// The height of a field's control: [`CONTROL_H`], or a finger's target in
/// tablet mode.
pub fn control_h() -> f32 {
    style::target(CONTROL_H)
}
/// The gap between a field's caption, control and hint.
pub const GAP: f32 = 5.0;

/// The caption at `x, y`; returns its height. With `info` the hint icon sits
/// 6 px after the words and the row is the icon's 16 px tall.
pub fn caption(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    text: &str,
    info_text: &str,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> f32 {
    let g = style::galley(ui, &text.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.7);
    let h = g.size().y.max(if info_text.is_empty() { 0.0 } else { INFO_ROW });
    let gw = g.size().x;
    ui.painter()
        .galley(Pos2::new(x, y + (h - g.size().y) / 2.0), g, style::text_3());
    if !info_text.is_empty() {
        popover::info(ui, Pos2::new(x + gw + 6.0 + 8.0, y + h / 2.0), text, info_text, id);
    }
    h
}

/// The hint wrapped to `w`; returns its height.
pub fn hint(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let g = kit::wrapped_weight(ui, text, 10.5, Weight::W400, style::text_3(), w, 1.4);
    let h = g.size().y;
    ui.painter().galley(Pos2::new(x, y), g, style::text_3());
    h
}

/// A text box `w` wide at `x, y`: mono for numbers, dots for a secret.
#[allow(clippy::too_many_arguments)]
pub fn input(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    text: &mut String,
    placeholder: &str,
    mono: bool,
    secret: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> egui::Response {
    let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, control_h()));
    TextField::new(text)
        .hint(placeholder)
        .mono(mono)
        .secret(secret)
        .pad(11.0, 11.0)
        .show(ui, rect, id)
}

/// A path field `w` wide at `x, y`: read only, with the buttons that open the
/// system file dialog. `value` becomes what is chosen.
pub fn path(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    field: &PathField<'_>,
    value: &mut String,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> Option<Picked> {
    field.show(ui, x, y, w, value, id)
}

/// A select `w` wide at `x, y`; returns the value picked this frame.
pub fn select(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    choices: &[Choice],
    selected: &str,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> Option<String> {
    let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, control_h()));
    kit::select(ui, rect, choices, selected, "", false, "", id)
}

/// A whole field: caption, the control (drawn by `control` at the y it is
/// given, returning its height) and the hint. Returns the bottom.
#[allow(clippy::too_many_arguments)]
pub fn field(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    caption_text: &str,
    hint_text: &str,
    control: impl FnOnce(&mut egui::Ui, f32) -> f32,
) -> f32 {
    field_with(ui, x, y, w, caption_text, "", hint_text, control)
}

/// `field` with a hint icon beside the caption when `info_text` is given.
#[allow(clippy::too_many_arguments)]
pub fn field_with(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    caption_text: &str,
    info_text: &str,
    hint_text: &str,
    control: impl FnOnce(&mut egui::Ui, f32) -> f32,
) -> f32 {
    let mut cy = y;
    if !caption_text.is_empty() {
        cy += caption(ui, x, cy, caption_text, info_text, caption_text) + GAP;
    }
    cy += control(ui, cy);
    if !hint_text.is_empty() {
        cy += GAP;
        cy += hint(ui, x, cy, w, hint_text);
    }
    cy
}

/// A field whose caption carries a hint icon (`info` of `Corvus.ui.field`):
/// the caption row is the icon's 16 px tall, the icon 6 px after the words.
/// `control` draws at the y it is given and returns its height; returns the
/// bottom.
#[allow(clippy::too_many_arguments)]
pub fn field_info(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    caption_text: &str,
    info_text: &str,
    id: impl std::hash::Hash + std::fmt::Debug,
    control: impl FnOnce(&mut egui::Ui, f32) -> f32,
) -> f32 {
    self::caption(ui, x, y, caption_text, info_text, id);
    let top = y + INFO_ROW + GAP;
    top + control(ui, top)
}

/// The caption row of a field that has an info icon.
pub const INFO_ROW: f32 = 16.0;
