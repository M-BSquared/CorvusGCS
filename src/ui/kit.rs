//! The web interface's shared controls (python/src/css/components.css), painted to
//! its measures: `.btn` and its variants, `.icon-btn`, `.field-input`,
//! `.field-select` with its `.ui-menu` list, and a checkbox row.

use std::sync::Arc;

use eframe::egui::{self, Color32, Galley, Pos2, Rect, Stroke, Vec2};

use crate::ui::field::TextField;
use crate::ui::icons;
use crate::ui::menu;
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
            resp = resp.on_hover_text(self.title);
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
        .on_hover_text(title);
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
    text_field_padded(ui, rect, text, placeholder, mono, id, 11.0)
}

/// `text_field` with the control's own side padding: 10 px by default, 6 in
/// a table cell.
pub fn text_field_padded(
    ui: &mut egui::Ui,
    rect: Rect,
    text: &mut String,
    placeholder: &str,
    mono: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
    pad: f32,
) -> egui::Response {
    TextField::new(text)
        .hint(placeholder)
        .mono(mono)
        .pad(pad, pad)
        .show(ui, rect, id)
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
        painter.rect_filled(r, style::RADIUS_BTN, style::input(0.6));
        painter.rect_stroke(
            r,
            style::RADIUS_BTN,
            Stroke::new(1.0, if open { style::accent() } else { style::border() }),
            egui::StrokeKind::Inside,
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
        style::galley_mono(ui, text, 12.0, color)
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
    let g = style::galley(ui, label, 12.0, Weight::W400, style::text_1(), 0.0);
    let h = g.size().y.max(14.0);
    let rect = Rect::from_min_size(at, Vec2::new(width, h));
    let resp = ui.interact(rect, ui.scope_id().with(id), egui::Sense::click());
    let painter = ui.painter();
    let bx = Rect::from_min_size(Pos2::new(at.x, at.y + (h - 14.0) / 2.0), Vec2::splat(14.0));
    let accent = if warn { style::warning() } else { style::accent() };
    if *on {
        painter.rect_filled(bx, 3, accent);
        let a = bx.left_top() + Vec2::new(3.2, 7.2);
        let b = bx.left_top() + Vec2::new(6.0, 10.0);
        let c = bx.left_top() + Vec2::new(11.0, 4.2);
        painter.line(vec![a, b, c], Stroke::new(1.8, Color32::WHITE));
    } else {
        {
            let r = bx;
            painter.rect_filled(r, 3, style::surface_1());
            painter.rect_stroke(r, 3, Stroke::new(1.0, style::border_strong()), egui::StrokeKind::Inside);
        }
    }
    painter.galley(
        Pos2::new(bx.right() + 8.0, at.y + (h - g.size().y) / 2.0),
        g,
        style::text_1(),
    );
    if resp.clicked() {
        *on = !*on;
        return true;
    }
    false
}

/// A `.ui-toggle`: a 38 by 22 pill, the accent when on, with a 16 px knob.
/// Drawn at `at`; returns true when pressed (the caller flips what it holds).
pub fn switch(
    ui: &mut egui::Ui,
    at: Pos2,
    on: bool,
    enabled: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> bool {
    let rect = Rect::from_min_size(at, Vec2::new(38.0, 22.0));
    let resp = ui.interact(
        rect,
        ui.scope_id().with(id),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let fade = if enabled { 1.0 } else { 0.5 };
    let (fill, line) = if on {
        (style::accent(), style::accent())
    } else if resp.hovered() && enabled {
        (style::input(0.6), style::border_strong())
    } else {
        (style::input(0.6), style::border())
    };
    let painter = ui.painter();
    {
        let r = rect;
        painter.rect_filled(r, 255, fill.gamma_multiply(fade));
        painter.rect_stroke(
            r,
            255,
            Stroke::new(1.0, line.gamma_multiply(fade)),
            egui::StrokeKind::Inside,
        );
    }
    let knob = if on { style::text_inverted() } else { style::text_3() };
    painter.circle_filled(
        Pos2::new(rect.left() + 11.0 + if on { 16.0 } else { 0.0 }, rect.center().y),
        8.0,
        knob.gamma_multiply(fade),
    );
    enabled && resp.clicked()
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

/// Text wrapped to `width` in a given weight and line height (a factor of
/// the size, as CSS `line-height`).
pub fn wrapped_weight(
    ui: &egui::Ui,
    text: &str,
    size: f32,
    weight: Weight,
    color: Color32,
    width: f32,
    line: f32,
) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat {
            font_id: style::font(size, weight),
            color,
            line_height: Some(size * line),
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
            3.5 + 3.0,
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 24),
        );
    }
    painter.circle_filled(c, 3.5, color);
}
