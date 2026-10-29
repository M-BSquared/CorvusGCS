//! The single-line text field of the web interface (`.field-input`), and the
//! variants that sit on other surfaces. Every text box in the app is a
//! `TextField`, so the focus ring, the padding, the fonts and the colours
//! of the placeholder and the text come from one place.

use eframe::egui::{self, Align2, Color32, Id, Pos2, Rect, Stroke, Vec2};

use crate::ui::icons;
use crate::ui::style::{self, Weight};

/// What the field is drawn on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Surface {
    /// `.field-input`: the recessed well with its border and a focus ring.
    Well,
    /// A field on a toolbar: the glass of `.page-card` with the tint border.
    Glass,
    /// A search box floating over the map: frosted glass whose alpha is for
    /// the unfocused and the focused state, a ring-less accent border.
    Floating { rest: f32, focus: f32 },
}

/// A single-line text field.
pub struct TextField<'a> {
    text: &'a mut String,
    hint: &'a str,
    surface: Surface,
    mono: bool,
    size: f32,
    weight: Weight,
    color: Color32,
    pad: (f32, f32),
    icon: Option<(&'static str, f32, f32, f32)>,
    align_right: bool,
    name: &'static str,
    focus: Option<Color32>,
}

impl<'a> TextField<'a> {
    pub fn new(text: &'a mut String) -> Self {
        TextField {
            text,
            hint: "",
            surface: Surface::Well,
            mono: false,
            size: 12.0,
            weight: Weight::W400,
            color: style::text_1(),
            pad: (11.0, 11.0),
            icon: None,
            align_right: false,
            name: "field",
            focus: None,
        }
    }

    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }

    /// In JetBrains Mono, like `.field-input-mono`.
    pub fn mono(mut self, on: bool) -> Self {
        self.mono = on;
        self
    }

    pub fn surface(mut self, surface: Surface) -> Self {
        self.surface = surface;
        self
    }

    /// Names a floating field's glass, so its frost is its own.
    pub fn named(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// The font size and weight of the text.
    pub fn font(mut self, size: f32, weight: Weight) -> Self {
        self.size = size;
        self.weight = weight;
        self
    }

    pub fn color(mut self, color: Color32) -> Self {
        self.color = color;
        self
    }

    /// The padding at the left and at the right of the text.
    pub fn pad(mut self, left: f32, right: f32) -> Self {
        self.pad = (left, right);
        self
    }

    /// A leading icon of `size` px, `left` px from the edge, and `gap` px
    /// before the text.
    pub fn icon(mut self, name: &'static str, size: f32, left: f32, gap: f32) -> Self {
        self.icon = Some((name, size, left, gap));
        self
    }

    /// The border on focus, in place of the accent.
    pub fn focus_color(mut self, color: Color32) -> Self {
        self.focus = Some(color);
        self
    }

    pub fn align_right(mut self) -> Self {
        self.align_right = true;
        self
    }

    /// The field at `rect`, identified by `id` within this ui.
    pub fn show(self, ui: &mut egui::Ui, rect: Rect, id: impl std::hash::Hash + std::fmt::Debug) -> egui::Response {
        let id = ui.scope_id().with(id);
        self.show_with_id(ui, rect, id)
    }

    /// The field at `rect` with a ready made widget id, for a caller that
    /// shares the id with something else (the selection of a fresh search).
    pub fn show_with_id(self, ui: &mut egui::Ui, rect: Rect, id: Id) -> egui::Response {
        let focused = ui.memory(|m| m.has_focus(id));
        let painter = ui.painter().clone();
        match self.surface {
            Surface::Well => {
                if focused {
                    painter.rect_stroke(
                        rect.expand(2.0),
                        style::RADIUS_BTN + 2,
                        Stroke::new(2.0, style::accent_dim()),
                        egui::StrokeKind::Inside,
                    );
                }
                painter.rect_filled(rect, style::RADIUS_BTN, style::input(0.6));
                painter.rect_stroke(
                    rect,
                    style::RADIUS_BTN,
                    Stroke::new(
                        1.0,
                        if focused {
                            self.focus.unwrap_or_else(style::accent)
                        } else {
                            style::border()
                        },
                    ),
                    egui::StrokeKind::Inside,
                );
            }
            Surface::Glass => {
                if focused {
                    painter.rect_stroke(
                        rect.expand(2.0),
                        style::RADIUS_BTN + 2,
                        Stroke::new(2.0, style::accent_dim()),
                        egui::StrokeKind::Inside,
                    );
                }
                painter.rect_filled(rect, style::RADIUS_BTN, style::glass(0.82));
                painter.rect_stroke(
                    rect,
                    style::RADIUS_BTN,
                    Stroke::new(1.0, if focused { style::accent() } else { style::tint(0.09) }),
                    egui::StrokeKind::Inside,
                );
            }
            Surface::Floating { rest, focus } => {
                style::glass_panel(
                    &painter,
                    self.name,
                    rect,
                    style::RADIUS,
                    if focused { focus } else { rest },
                );
                painter.rect_stroke(
                    rect,
                    style::RADIUS,
                    Stroke::new(
                        1.0,
                        if focused {
                            style::accent_bright()
                        } else {
                            style::border()
                        },
                    ),
                    egui::StrokeKind::Inside,
                );
            }
        }
        let mut left = rect.left() + self.pad.0;
        if let Some((name, size, from, gap)) = self.icon {
            icons::paint(
                ui,
                name,
                Rect::from_center_size(
                    Pos2::new(rect.left() + from + size / 2.0, rect.center().y),
                    Vec2::splat(size),
                ),
                style::text_3(),
            );
            left = rect.left() + from + size + gap;
        }
        let text_rect = Rect::from_min_max(
            Pos2::new(left, rect.top()),
            Pos2::new(rect.right() - self.pad.1, rect.bottom()),
        );
        let font = if self.mono {
            style::mono(self.size)
        } else {
            style::font(self.size, self.weight)
        };
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect));
        let edit = egui::TextEdit::singleline(self.text)
            .id(id)
            .frame(egui::Frame::NONE)
            .font(font)
            .text_color(self.color)
            .hint_text(egui::RichText::new(self.hint).color(style::text_3()))
            .align(if self.align_right {
                Align2::RIGHT_CENTER
            } else {
                Align2::LEFT_CENTER
            })
            .desired_width(text_rect.width())
            .min_size(Vec2::new(0.0, rect.height()));
        child.add(edit)
    }
}
