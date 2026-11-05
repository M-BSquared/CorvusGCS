//! `.field-select` with the app's dropdown (`.ui-menu`): the one select.

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};

use crate::ui::card::{self, Look};
use crate::ui::icons;
use crate::ui::menu;
use crate::ui::style::{self, Weight};

/// One row of a select.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub value: String,
    pub label: String,
    pub enabled: bool,
}

impl Choice {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Choice {
        Choice {
            value: value.into(),
            label: label.into(),
            enabled: true,
        }
    }
}

/// A `.field-select` with the app's dropdown (`.ui-menu`, `.option-item`).
/// Returns the value picked this frame. `placeholder` shows while nothing is
/// selected, in the muted colour.
#[allow(clippy::too_many_arguments)]
pub fn select(
    ui: &mut egui::Ui,
    rect: Rect,
    choices: &[Choice],
    selected: &str,
    placeholder: &str,
    mono: bool,
    title: &str,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> Option<String> {
    select_padded(ui, rect, choices, selected, placeholder, mono, title, id, (11.0, 26.0))
}

/// `select` with the control's own padding: `.field-select` pads 10 px left
/// and leaves 26 on the right for the chevron; a table cell pads 7 and 24.
#[allow(clippy::too_many_arguments)]
pub fn select_padded(
    ui: &mut egui::Ui,
    rect: Rect,
    choices: &[Choice],
    selected: &str,
    placeholder: &str,
    mono: bool,
    title: &str,
    id: impl std::hash::Hash + std::fmt::Debug,
    pad: (f32, f32),
) -> Option<String> {
    let id = ui.scope_id().with(id);
    let open_id = id.with("open");
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    let mut resp = ui.interact(rect, id, egui::Sense::click());
    if !title.is_empty() {
        resp = resp.on_hover_text(title);
    }
    let painter = ui.painter().clone();
    if open {
        painter.rect_stroke(
            rect.expand(2.0),
            style::RADIUS_BTN + 2,
            Stroke::new(2.0, style::accent_dim()),
            egui::StrokeKind::Inside,
        );
    }
    {
        let r = rect;
        card::paint(
            &painter,
            r,
            Look::custom(
                style::input(0.6),
                if open { style::accent() } else { style::border() },
                style::RADIUS_BTN,
            ),
        );
    }
    let current = choices.iter().find(|c| c.value == selected);
    // an empty value is the prompt row ("Select serial port"), not a choice
    let (text, color) = match current {
        Some(c) if c.value.is_empty() => (c.label.as_str(), style::text_3()),
        Some(c) => (c.label.as_str(), style::text_1()),
        None => (placeholder, style::text_3()),
    };
    let g = if mono {
        // a port name is data and is not in any language file; the prompt row is
        style::galley_mono(ui, &crate::i18n::tr(text), 12.0, color)
    } else {
        style::galley(ui, text, 12.0, Weight::W400, color, 0.0)
    };
    let clip = Rect::from_min_max(rect.min, Pos2::new(rect.right() - pad.1, rect.bottom()));
    ui.painter_at(clip).galley(
        Pos2::new(rect.left() + pad.0, rect.center().y - g.size().y / 2.0),
        g,
        color,
    );
    icons::paint(
        ui,
        "chevron-down",
        Rect::from_center_size(Pos2::new(rect.right() - 14.0, rect.center().y), Vec2::splat(12.0)),
        style::text_3(),
    );
    if resp.clicked() {
        open = !open;
    }
    let mut picked = None;
    if open {
        let rows: Vec<menu::Row> = choices
            .iter()
            .map(|c| menu::Row {
                value: c.value.clone(),
                label: c.label.clone(),
                enabled: c.enabled,
            })
            .collect();
        let look = if mono {
            menu::Look::SELECT_MONO
        } else {
            menu::Look::SELECT
        };
        let out = menu::dropdown(ui, id.with("menu"), rect, &rows, selected, &look);
        if out.picked.is_some() || out.dismissed {
            open = false;
        }
        picked = out.picked;
    }
    ui.ctx().data_mut(|d| d.insert_temp(open_id, open));
    picked
}
