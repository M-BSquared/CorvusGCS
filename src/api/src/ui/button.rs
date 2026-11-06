//! `.btn` and its variants, and `.icon-btn`: every button of the app.

use crate::ui::tip::HoverTr;
use std::sync::Arc;

use eframe::egui::{self, Color32, Galley, Pos2, Rect, Stroke, Vec2};

use crate::ui::icons;
use crate::ui::style::{self, Weight};

/// `.btn[data-variant]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Primary,
    Secondary,
    Ghost,
    /// A primary button that cannot be pressed: `.btn.params-apply:disabled`
    /// is flat grey rather than a faded accent.
    Inert,
    /// `.btn.link-disconnect`: outlined in the critical colour.
    DangerOutline,
    /// `.btn[data-variant="danger"]`: the critical colour, filled.
    Danger,
}

/// One button: `label` with an optional icon before it. `size_sm` is
/// `data-size="sm"` (7 px 11 px, 11 px text); the default is 10 px 14 px, 12 px.
pub struct Button<'a> {
    pub label: &'a str,
    pub icon: Option<&'static str>,
    pub variant: Variant,
    pub small: bool,
    pub weight: Weight,
    pub enabled: bool,
    pub title: &'a str,
    /// A page's own padding, `(vertical, horizontal)`, where its CSS departs
    /// from `data-size` (`.params-actions .btn`).
    pub pad: Option<(f32, f32)>,
    /// A page's own icon size and the gap after it.
    pub icon_gap: Option<(f32, f32)>,
    /// `justify-content: flex-start`: the content starts at the left padding.
    pub start: bool,
}

impl<'a> Button<'a> {
    pub fn new(label: &'a str, variant: Variant) -> Self {
        Button {
            label,
            icon: None,
            variant,
            small: false,
            weight: Weight::W600,
            enabled: true,
            title: "",
            pad: None,
            icon_gap: None,
            start: false,
        }
    }
    /// The content at the left instead of the middle.
    pub fn align_start(mut self) -> Self {
        self.start = true;
        self
    }
    /// `.params-actions .btn`: 8 px 13 px, a 13 px icon and a 6 px gap.
    pub fn actions(mut self) -> Self {
        self.pad = Some((8.0, 13.0));
        self.icon_gap = Some((13.0, 6.0));
        self.weight = Weight::W700;
        self
    }
    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }
    pub fn bold(mut self) -> Self {
        self.weight = Weight::W700;
        self
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn title(mut self, t: &'a str) -> Self {
        self.title = t;
        self
    }

    fn text(&self, ui: &egui::Ui, color: Color32) -> Arc<Galley> {
        let size = if self.small { 11.0 } else { 12.0 };
        style::galley(ui, self.label, size, self.weight, color, 0.1)
    }

    /// The width it wants at its natural size.
    pub fn width(&self, ui: &egui::Ui) -> f32 {
        let pad = self.pad.map_or(if self.small { 11.0 } else { 14.0 }, |p| p.1);
        let icon = if self.icon.is_some() {
            self.icon_size() + if self.label.is_empty() { 0.0 } else { self.gap() }
        } else {
            0.0
        };
        2.0 + 2.0 * pad + icon + self.text(ui, style::text_1()).size().x
    }

    /// Its height at its natural size.
    pub fn height(&self) -> f32 {
        let pad = self.pad.map_or(if self.small { 7.0 } else { 10.0 }, |p| p.0);
        let line: f32 = if self.small { 11.0 } else { 12.0 } * 1.21;
        2.0 + 2.0 * pad + line.max(if self.icon.is_some() { self.icon_size() } else { 0.0 })
    }

    fn gap(&self) -> f32 {
        self.icon_gap.map_or(7.0, |g| g.1)
    }

    fn icon_size(&self) -> f32 {
        if let Some((size, _)) = self.icon_gap {
            size
        } else if self.small {
            13.0
        } else {
            15.0
        }
    }

    /// Paints it into `rect`; true when clicked.
    pub fn show(self, ui: &mut egui::Ui, rect: Rect, id: impl std::hash::Hash + std::fmt::Debug) -> bool {
        let sense = if self.enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let mut resp = ui.interact(rect, ui.scope_id().with(id), sense);
        if !self.title.is_empty() {
            resp = resp.on_hover_tr(self.title);
        }
        let hover = resp.hovered() && self.enabled;
        let (fill, text, stroke) = match self.variant {
            Variant::Primary => (
                if hover { style::accent_hover() } else { style::accent() },
                style::text_inverted(),
                Stroke::NONE,
            ),
            Variant::Secondary => (style::glass(0.6), style::text_1(), Stroke::new(1.0, style::tint(0.09))),
            Variant::Ghost => (
                if hover { style::tint(0.04) } else { Color32::TRANSPARENT },
                if hover { style::text_1() } else { style::text_2() },
                Stroke::NONE,
            ),
            Variant::Inert => (style::surface_3(), style::text_3(), Stroke::NONE),
            Variant::Danger => (
                if hover {
                    style::critical().gamma_multiply(0.85)
                } else {
                    style::critical()
                },
                Color32::WHITE,
                Stroke::new(1.0, style::tint(0.08)),
            ),
            Variant::DangerOutline => (
                style::critical().gamma_multiply(if hover { 0.12 } else { 0.07 }),
                style::critical(),
                Stroke::new(1.0, style::critical().gamma_multiply(0.30)),
            ),
        };
        // `.btn:disabled { opacity: .45 }` fades the whole button as one
        // group: the label is the label blended into the page, not a label
        // drawn thinly over the faded fill
        let under = style::surface_1();
        let fade = |c: Color32| {
            if self.enabled {
                c
            } else if c.a() == 255 {
                let mix = |a: u8, b: u8| (f32::from(a) * 0.45 + f32::from(b) * 0.55).round() as u8;
                Color32::from_rgb(mix(c.r(), under.r()), mix(c.g(), under.g()), mix(c.b(), under.b()))
            } else {
                c.gamma_multiply(0.45)
            }
        };
        let painter = ui.painter();
        {
            let r = rect;
            painter.rect_filled(r, style::RADIUS_BTN, fade(fill));
            painter.rect_stroke(
                r,
                style::RADIUS_BTN,
                Stroke::new(stroke.width, fade(stroke.color)),
                egui::StrokeKind::Inside,
            );
        }
        let g = self.text(ui, fade(text));
        let icon_w = if self.icon.is_some() { self.icon_size() } else { 0.0 };
        let gap = if self.icon.is_some() && !self.label.is_empty() {
            self.gap()
        } else {
            0.0
        };
        let total = icon_w + gap + g.size().x;
        let x = if self.start {
            rect.left() + 1.0 + self.pad.map_or(if self.small { 11.0 } else { 14.0 }, |p| p.1)
        } else {
            rect.center().x - total / 2.0
        };
        if let Some(icon) = self.icon {
            let r = Rect::from_min_size(Pos2::new(x, rect.center().y - icon_w / 2.0), Vec2::splat(icon_w));
            icons::paint(ui, icon, r, fade(text));
        }
        painter.galley(
            Pos2::new(x + icon_w + gap, rect.center().y - g.size().y / 2.0),
            g,
            fade(text),
        );
        resp.clicked()
    }
}

/// `.icon-btn`: a square (28 px unless told) with a 15 px icon.
pub fn icon_btn(
    ui: &mut egui::Ui,
    rect: Rect,
    icon: &'static str,
    title: &str,
    active: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    let resp = ui
        .interact(rect, ui.scope_id().with(id), egui::Sense::click())
        .on_hover_tr(title);
    let (fill, color) = if active {
        (style::accent_dark(), style::accent_bright())
    } else if resp.hovered() {
        (style::surface_hover(), style::text_1())
    } else {
        (Color32::TRANSPARENT, style::text_3())
    };
    ui.painter().rect_filled(rect, style::RADIUS_BTN, fill);
    let s = if rect.width() < 24.0 { 13.0 } else { 15.0 };
    icons::paint(ui, icon, Rect::from_center_size(rect.center(), Vec2::splat(s)), color);
    resp.clicked()
}
