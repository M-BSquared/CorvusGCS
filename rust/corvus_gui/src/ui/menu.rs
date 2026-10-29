//! The dropdown list of the web interface (`.ui-menu` of `.option-item` rows,
//! components.css) on a glass surface. Every list in the app is made here: a
//! select's choices, the flight mode picker, the elevation models of the 3D
//! panel, so they open, close, scroll, hover and mark the active row alike.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::ui::kit;
use crate::ui::style::{self, Weight};

/// `.option-item`: 7 px above and below a 12 px line of 1.21.
pub const ROW_H: f32 = 7.0 * 2.0 + 12.0 * 1.21;
/// `.ui-menu` padding.
pub const PAD: f32 = 5.0;

/// How the active row is marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// The accent fill with a 2 px ring and the accent dot (`.field-select`).
    Ring,
    /// The accent fill alone.
    Fill,
    /// The hover grey, held.
    Soft,
}

/// How the rows look.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub mono: bool,
    pub weight: Weight,
    /// A 7 px state dot before the label.
    pub dots: bool,
    pub mark: Mark,
    /// The list is at least this wide; it is as wide as its widest row
    /// otherwise, and never narrower than the control it hangs from.
    pub min_width: f32,
}

impl Look {
    /// The list of a `.field-select`.
    pub const SELECT: Look = Look {
        mono: false,
        weight: Weight::W500,
        dots: true,
        mark: Mark::Ring,
        min_width: 0.0,
    };
    /// The same set in the monospace of a mono field.
    pub const SELECT_MONO: Look = Look {
        mono: true,
        ..Look::SELECT
    };
    /// A plain list: the flight modes.
    pub const PLAIN: Look = Look {
        mono: false,
        weight: Weight::W400,
        dots: false,
        mark: Mark::Soft,
        min_width: 160.0,
    };
    /// The rows of a panel's own list: the elevation models.
    pub const PANEL: Look = Look {
        mono: false,
        weight: Weight::W400,
        dots: false,
        mark: Mark::Fill,
        min_width: 0.0,
    };
}

/// One row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub value: String,
    pub label: String,
    pub enabled: bool,
}

impl Row {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Row {
        Row {
            value: value.into(),
            label: label.into(),
            enabled: true,
        }
    }

    pub fn disabled(mut self) -> Row {
        self.enabled = false;
        self
    }
}

fn label_galley(ui: &egui::Ui, label: &str, color: Color32, look: &Look) -> std::sync::Arc<egui::Galley> {
    if look.mono {
        style::galley_mono(ui, label, 12.0, color)
    } else {
        style::galley(ui, label, 12.0, look.weight, color, 0.0)
    }
}

/// One row painted in `r`; true when pressed. A disabled row only hovers.
pub fn row(
    ui: &mut egui::Ui,
    r: Rect,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    active: bool,
    enabled: bool,
    look: &Look,
) -> bool {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let resp = ui.interact(r, ui.scope_id().with(id), sense);
    if active {
        match look.mark {
            Mark::Ring => {
                ui.painter().rect_filled(r, style::RADIUS_BTN, style::accent_dark());
                ui.painter().rect_stroke(
                    r,
                    style::RADIUS_BTN,
                    Stroke::new(2.0, style::accent_bright()),
                    egui::StrokeKind::Inside,
                );
            }
            Mark::Fill => {
                ui.painter().rect_filled(r, style::RADIUS_BTN, style::accent_dark());
            }
            Mark::Soft => {
                ui.painter().rect_filled(r, style::RADIUS_BTN, style::surface_hover());
            }
        }
    } else if resp.hovered() && enabled {
        ui.painter().rect_filled(r, style::RADIUS_BTN, style::tint(0.06));
    }
    let color = if !enabled {
        style::text_3()
    } else if active || resp.hovered() {
        style::text_1()
    } else {
        style::text_2()
    };
    let mut x = r.left() + 8.0;
    if look.dots {
        let dot_color = if active { style::accent() } else { style::text_3() };
        kit::dot(ui.painter(), Pos2::new(x + 3.5, r.center().y), dot_color, active);
        x += 7.0 + 8.0;
    }
    let g = label_galley(ui, label, color, look);
    ui.painter_at(r)
        .galley(Pos2::new(x, r.center().y - g.size().y / 2.0), g, color);
    enabled && resp.clicked()
}

/// What a dropdown did this frame.
#[derive(Debug, Default)]
pub struct Outcome {
    pub picked: Option<String>,
    /// True when a press outside, or Escape, asks to close it.
    pub dismissed: bool,
}

/// The list hanging 4 px under `anchor`, as wide as `look` asks, scrolling
/// when the screen has no room for all its rows. `selected` is the active
/// value. Pressing outside the list and the anchor, or Escape, dismisses it.
pub fn dropdown(
    ui: &egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    anchor: Rect,
    rows: &[Row],
    selected: &str,
    look: &Look,
) -> Outcome {
    let id = ui.scope_id().with(id);
    let widest = rows
        .iter()
        .map(|c| label_galley(ui, &c.label, style::text_2(), look).size().x)
        .fold(0.0, f32::max);
    let dots = if look.dots { 7.0 + 8.0 } else { 0.0 };
    let width = anchor.width().max(look.min_width).max(PAD * 2.0 + 16.0 + dots + widest);
    let screen = ui.ctx().content_rect();
    let full = PAD * 2.0 + ROW_H * rows.len() as f32;
    let room = (screen.bottom() - anchor.bottom() - 4.0 - 8.0).max(ROW_H * 3.0);
    let menu = Rect::from_min_size(
        anchor.left_bottom() + Vec2::new(0.0, 4.0),
        Vec2::new(width, full.min(room)),
    );
    let mut out = Outcome::default();
    egui::Area::new(id.with("menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(menu.min)
        .show(ui.ctx(), |ui| {
            let painter = ui.painter().clone();
            style::glass_panel(&painter, "dropdown", menu, style::RADIUS, 0.6);
            ui.allocate_rect(menu, egui::Sense::hover());
            let inner = menu.shrink(PAD);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
            child.set_clip_rect(inner);
            egui::ScrollArea::vertical()
                .id_salt(id.with("scroll"))
                .auto_shrink([false, false])
                .show(&mut child, |ui| {
                    let origin = ui.max_rect().min;
                    for (i, c) in rows.iter().enumerate() {
                        let r = Rect::from_min_size(
                            origin + Vec2::new(0.0, i as f32 * ROW_H),
                            Vec2::new(inner.width(), ROW_H),
                        );
                        if row(ui, r, ("row", i), &c.label, c.value == selected, c.enabled, look) {
                            out.picked = Some(c.value.clone());
                        }
                    }
                    ui.allocate_space(Vec2::new(inner.width(), full - PAD * 2.0));
                });
            out.dismissed = dismissed(ui, &[menu, anchor]);
        });
    out
}

/// True when the pointer pressed outside every one of `inside` this frame,
/// or Escape was pressed.
pub fn dismissed(ui: &egui::Ui, inside: &[Rect]) -> bool {
    let outside = ui.input(|i| i.pointer.any_pressed())
        && !ui
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| inside.iter().any(|r| r.contains(p)));
    outside || ui.input(|i| i.key_pressed(egui::Key::Escape))
}
