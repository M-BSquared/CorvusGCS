//! The web interface's shared controls (src/css/components.css), painted to
//! its measures: `.btn` and its variants, `.icon-btn`, `.field-input`,
//! `.field-select` with its `.ui-menu` list, and a checkbox row.

use std::sync::Arc;

use eframe::egui::{self, Color32, Galley, Pos2, Rect, Stroke, Vec2};

use crate::icons;
use crate::style::{self, Weight};

/// `.btn[data-variant]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Primary,
    Secondary,
    /// `.btn.link-disconnect`: outlined in the critical colour.
    DangerOutline,
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
        }
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

    /// Its height at its natural size.
    pub fn height(&self) -> f32 {
        let pad = if self.small { 7.0 } else { 10.0 };
        let line: f32 = if self.small { 11.0 } else { 12.0 } * 1.21;
        2.0 + 2.0 * pad + line.max(if self.icon.is_some() { self.icon_size() } else { 0.0 })
    }

    fn icon_size(&self) -> f32 {
        if self.small {
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
            resp = resp.on_hover_text(self.title);
        }
        let hover = resp.hovered() && self.enabled;
        let (fill, text, stroke) = match self.variant {
            Variant::Primary => (
                if hover { style::ACCENT_HOVER } else { style::ACCENT },
                style::TEXT_INVERTED,
                Stroke::NONE,
            ),
            Variant::Secondary => (style::glass(0.6), style::TEXT_1, Stroke::new(1.0, style::tint(0.09))),
            Variant::DangerOutline => (
                style::rgba(0xD62B26, if hover { 0.12 } else { 0.07 }),
                style::CRITICAL,
                Stroke::new(1.0, style::rgba(0xD62B26, 0.30)),
            ),
        };
        let alpha = if self.enabled { 1.0 } else { 0.45 };
        let painter = ui.painter();
        painter.rect(
            rect,
            style::RADIUS_BTN,
            fill.gamma_multiply(alpha),
            stroke,
            egui::StrokeKind::Inside,
        );
        let g = self.text(ui, text.gamma_multiply(alpha));
        let icon_w = if self.icon.is_some() { self.icon_size() } else { 0.0 };
        let gap = if self.icon.is_some() && !self.label.is_empty() {
            7.0
        } else {
            0.0
        };
        let total = icon_w + gap + g.size().x;
        let x = rect.center().x - total / 2.0;
        if let Some(icon) = self.icon {
            let r = Rect::from_min_size(Pos2::new(x, rect.center().y - icon_w / 2.0), Vec2::splat(icon_w));
            icons::paint(ui, icon, r, text.gamma_multiply(alpha));
        }
        painter.galley(Pos2::new(x + icon_w + gap, rect.center().y - g.size().y / 2.0), g, text);
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
        .on_hover_text(title);
    let (fill, color) = if active {
        (style::ACCENT_DARK, style::ACCENT_BRIGHT)
    } else if resp.hovered() {
        (style::SURFACE_HOVER, style::TEXT_1)
    } else {
        (Color32::TRANSPARENT, style::TEXT_3)
    };
    ui.painter().rect_filled(rect, style::RADIUS_BTN, fill);
    let s = if rect.width() < 24.0 { 13.0 } else { 15.0 };
    icons::paint(ui, icon, Rect::from_center_size(rect.center(), Vec2::splat(s)), color);
    resp.clicked()
}

/// A `.field-input`: the recessed well, 1 px border, a focus ring in the
/// accent. `mono` sets it in JetBrains Mono like `.field-input-mono`.
pub fn text_field(
    ui: &mut egui::Ui,
    rect: Rect,
    text: &mut String,
    placeholder: &str,
    mono: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> egui::Response {
    let id = ui.scope_id().with(id);
    let focused = ui.memory(|m| m.has_focus(id));
    let painter = ui.painter().clone();
    if focused {
        painter.rect_stroke(
            rect.expand(2.0),
            style::RADIUS_BTN + 2,
            Stroke::new(2.0, style::rgba(0x1B1F26, 0.10)),
            egui::StrokeKind::Inside,
        );
    }
    painter.rect(
        rect,
        style::RADIUS_BTN,
        style::glass(0.6),
        Stroke::new(1.0, if focused { style::ACCENT } else { style::BORDER }),
        egui::StrokeKind::Inside,
    );
    let font = if mono {
        style::mono(12.0)
    } else {
        style::font(12.0, Weight::W400)
    };
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(11.0, 0.0))));
    let edit = egui::TextEdit::singleline(text)
        .id(id)
        .frame(egui::Frame::NONE)
        .font(font)
        .text_color(style::TEXT_1)
        .hint_text(egui::RichText::new(placeholder).color(style::TEXT_3))
        .align(egui::Align2::LEFT_CENTER)
        .desired_width(rect.width() - 22.0)
        .min_size(Vec2::new(0.0, rect.height()));
    child.add(edit)
}

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
            Stroke::new(2.0, style::rgba(0x1B1F26, 0.10)),
            egui::StrokeKind::Inside,
        );
    }
    painter.rect(
        rect,
        style::RADIUS_BTN,
        style::glass(0.6),
        Stroke::new(1.0, if open { style::ACCENT } else { style::BORDER }),
        egui::StrokeKind::Inside,
    );
    let current = choices.iter().find(|c| c.value == selected);
    let (text, color) = match current {
        Some(c) => (c.label.as_str(), style::TEXT_1),
        None => (placeholder, style::TEXT_3),
    };
    let g = if mono {
        style::galley_mono(ui, text, 12.0, color)
    } else {
        style::galley(ui, text, 12.0, Weight::W400, color, 0.0)
    };
    let clip = Rect::from_min_max(rect.min, Pos2::new(rect.right() - 26.0, rect.bottom()));
    ui.painter_at(clip).galley(
        Pos2::new(rect.left() + 11.0, rect.center().y - g.size().y / 2.0),
        g,
        color,
    );
    icons::paint(
        ui,
        "chevron-down",
        Rect::from_center_size(Pos2::new(rect.right() - 14.0, rect.center().y), Vec2::splat(12.0)),
        style::TEXT_3,
    );
    if resp.clicked() {
        open = !open;
    }
    let mut picked = None;
    if open {
        let row_h = 7.0 * 2.0 + 12.0 * 1.21;
        let menu = Rect::from_min_size(
            rect.left_bottom() + Vec2::new(0.0, 4.0),
            Vec2::new(rect.width(), 10.0 + row_h * choices.len() as f32),
        );
        egui::Area::new(id.with("menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.min)
            .show(ui.ctx(), |ui| {
                let painter = ui.painter().clone();
                style::glass_panel(&painter, "selectmenu", menu, style::RADIUS, 0.6);
                ui.allocate_rect(menu, egui::Sense::hover());
                for (i, c) in choices.iter().enumerate() {
                    let r = Rect::from_min_size(
                        menu.min + Vec2::new(5.0, 5.0 + i as f32 * row_h),
                        Vec2::new(menu.width() - 10.0, row_h),
                    );
                    let sense = if c.enabled {
                        egui::Sense::click()
                    } else {
                        egui::Sense::hover()
                    };
                    let row = ui.interact(r, id.with(("row", i)), sense);
                    let active = c.value == selected;
                    if active {
                        painter.rect_filled(r, style::RADIUS_BTN, style::ACCENT_DARK);
                    } else if row.hovered() && c.enabled {
                        painter.rect_filled(r, style::RADIUS_BTN, style::tint(0.06));
                    }
                    let color = if !c.enabled {
                        style::TEXT_3
                    } else if active || row.hovered() {
                        style::TEXT_1
                    } else {
                        style::TEXT_2
                    };
                    let g = if mono {
                        style::galley_mono(ui, &c.label, 12.0, color)
                    } else {
                        style::galley(ui, &c.label, 12.0, Weight::W400, color, 0.0)
                    };
                    ui.painter_at(r)
                        .galley(Pos2::new(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, color);
                    if row.clicked() && c.enabled {
                        picked = Some(c.value.clone());
                    }
                }
                let outside = ui.input(|i| i.pointer.any_pressed())
                    && !ui
                        .input(|i| i.pointer.interact_pos())
                        .is_some_and(|p| menu.contains(p) || rect.contains(p));
                if outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    open = false;
                }
            });
        if picked.is_some() {
            open = false;
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(open_id, open));
    picked
}

/// A `.link-toggle`: a 14 px checkbox in the accent (or the warning colour)
/// and its 12 px label. Returns true when flipped.
pub fn checkbox(
    ui: &mut egui::Ui,
    at: Pos2,
    width: f32,
    label: &str,
    on: &mut bool,
    warn: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    let g = style::galley(ui, label, 12.0, Weight::W400, style::TEXT_1, 0.0);
    let h = g.size().y.max(14.0);
    let rect = Rect::from_min_size(at, Vec2::new(width, h));
    let resp = ui.interact(rect, ui.scope_id().with(id), egui::Sense::click());
    let painter = ui.painter();
    let bx = Rect::from_min_size(Pos2::new(at.x, at.y + (h - 14.0) / 2.0), Vec2::splat(14.0));
    let accent = if warn { style::WARNING } else { style::ACCENT };
    if *on {
        painter.rect_filled(bx, 3, accent);
        let a = bx.left_top() + Vec2::new(3.2, 7.2);
        let b = bx.left_top() + Vec2::new(6.0, 10.0);
        let c = bx.left_top() + Vec2::new(11.0, 4.2);
        painter.line(vec![a, b, c], Stroke::new(1.8, Color32::WHITE));
    } else {
        painter.rect(
            bx,
            3,
            style::SURFACE_1,
            Stroke::new(1.0, style::BORDER_STRONG),
            egui::StrokeKind::Inside,
        );
    }
    painter.galley(
        Pos2::new(bx.right() + 8.0, at.y + (h - g.size().y) / 2.0),
        g,
        style::TEXT_1,
    );
    if resp.clicked() {
        *on = !*on;
        return true;
    }
    false
}

/// Text wrapped to `width`, for hints and notes.
pub fn wrapped(ui: &egui::Ui, text: &str, size: f32, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat {
            font_id: style::font(size, Weight::W400),
            color,
            line_height: Some(size * 1.4),
            ..Default::default()
        },
    );
    job.wrap.max_width = width;
    ui.painter().layout_job(job)
}

/// A dot as `.link-status-dot`: 7 px, a halo when lit, a ring when off.
pub fn dot(painter: &egui::Painter, c: Pos2, color: Color32, lit: bool) {
    let ring = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 51);
    painter.circle_filled(c, 3.5 + 2.0, ring);
    if lit {
        painter.circle_filled(
            c,
            3.5 + 4.0,
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 30),
        );
    }
    painter.circle_filled(c, 3.5, color);
}
