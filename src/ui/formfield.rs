//! `Corvus.ui.field` of the web interface (`.field`, `.field-label`,
//! `.field-hint`, `.field-input`, `.field-select`): a small uppercase caption,
//! the control and a hint under it, 5 px apart. The pages of Setup that are a
//! plain form (RTK, the telemetry radio, Remote ID, Video) are built from
//! these, so a caption, a box or a hint is the same on each of them.

use eframe::egui::{self, Pos2, Rect, Vec2};

use crate::ui::field::TextField;
use crate::ui::kit::{self, Choice};
use crate::ui::style::{self, Weight};

/// A `.field-input` and `.field-select` are 32.5 px tall: 8 px of padding, a
/// 14.5 px line and the border.
pub const CONTROL_H: f32 = 32.5;
/// The gap between a field's caption, control and hint.
pub const GAP: f32 = 5.0;

/// The caption at `x, y`; returns its height.
pub fn label(ui: &egui::Ui, x: f32, y: f32, text: &str) -> f32 {
    let g = style::galley(ui, &text.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.7);
    let h = g.size().y;
    ui.painter().galley(Pos2::new(x, y), g, style::text_3());
    h
}

/// The hint wrapped to `w`; returns its height.
pub fn hint(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
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
    let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, CONTROL_H));
    TextField::new(text)
        .hint(placeholder)
        .mono(mono)
        .secret(secret)
        .pad(11.0, 11.0)
        .show(ui, rect, id)
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
    let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, CONTROL_H));
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
    caption: &str,
    hint_text: &str,
    control: impl FnOnce(&mut egui::Ui, f32) -> f32,
) -> f32 {
    let mut cy = y;
    if !caption.is_empty() {
        cy += label(ui, x, cy, caption) + GAP;
    }
    cy += control(ui, cy) + if hint_text.is_empty() { 0.0 } else { GAP };
    if !hint_text.is_empty() {
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
    caption: &str,
    info_text: &str,
    id: impl std::hash::Hash + std::fmt::Debug,
    control: impl FnOnce(&mut egui::Ui, f32) -> f32,
) -> f32 {
    let g = style::galley(ui, &caption.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.7);
    let (gw, gh) = (g.size().x, g.size().y);
    ui.painter()
        .galley(Pos2::new(x, y + (INFO_ROW - gh) / 2.0), g, style::text_3());
    crate::ui::popover::info(
        ui,
        Pos2::new(x + gw + 6.0 + 8.0, y + INFO_ROW / 2.0),
        caption,
        info_text,
        id,
    );
    let top = y + INFO_ROW + GAP;
    top + control(ui, top)
}

/// The caption row of a field that has an info icon.
pub const INFO_ROW: f32 = 16.0;
