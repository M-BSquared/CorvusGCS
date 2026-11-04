//! The form vocabulary of the pages that are a column of cards (Settings,
//! Logs): `.page-section`, `.page-card`, `.field` with its caption and hint,
//! `.field-switch`, `.page-row`, `.ui-msg` and `.settings-tab`.
//!
//! A `Form` is a cursor: where the next block starts and how wide the column
//! is. Every function paints one block at the cursor and moves it down, so a
//! page reads top to bottom the way its markup does.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::ui::card::{self, Look};
use crate::ui::icons;
use crate::ui::kit;
use crate::ui::pagekit;
use crate::ui::popover;
use crate::ui::style::{self, Weight};

/// `.field` and `.page-card` spacing.
pub const FIELD_GAP: f32 = 5.0;
/// A `.field-input` / `.field-select` at 12 px with 8 px padding.
pub const CONTROL_H: f32 = 32.5;
const CARD_PAD: f32 = 19.0;

/// Where the next block goes.
#[derive(Clone, Copy, Debug)]
pub struct Form {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    stacked_switch: bool,
}

impl Form {
    pub fn new(x: f32, y: f32, w: f32) -> Form {
        Form {
            x,
            y,
            w,
            stacked_switch: false,
        }
    }

    pub fn skip(&mut self, h: f32) {
        self.y += h;
        self.stacked_switch = false;
    }

    pub fn rect(&self, h: f32) -> Rect {
        Rect::from_min_size(Pos2::new(self.x, self.y), Vec2::new(self.w, h))
    }
}

/// A titled `.page-section`; the cards in `body` end 24 px above whatever
/// follows (a card's own 12 px margin collapses into the section's).
pub fn section(ui: &mut egui::Ui, f: &mut Form, title: &str, body: impl FnOnce(&mut egui::Ui, &mut Form)) {
    f.y = pagekit::section_title(ui, f.x, f.y, f.w, title);
    f.stacked_switch = false;
    body(ui, f);
    f.y += 12.0;
}

/// A `.page-card`: 18 px padding, an optional 14 px title, then the fields.
pub fn card(ui: &mut egui::Ui, f: &mut Form, title: &str, body: impl FnOnce(&mut egui::Ui, &mut Form)) {
    let slot = card::reserve(ui.painter());
    let top = f.y;
    let mut inner = Form::new(f.x + CARD_PAD, top + CARD_PAD, f.w - 2.0 * CARD_PAD);
    if !title.is_empty() {
        let g = style::galley(ui, title, 14.0, Weight::W600, style::text_1(), 0.0);
        let h = g.size().y;
        ui.painter().galley(inner.pos(), g, style::text_1());
        inner.y += h + 6.0;
    }
    body(ui, &mut inner);
    let rect = Rect::from_min_max(Pos2::new(f.x, top), Pos2::new(f.x + f.w, inner.y + CARD_PAD));
    card::fill(ui.painter(), slot, rect, Look::Page);
    f.y = rect.bottom() + 12.0;
    f.stacked_switch = false;
}

impl Form {
    fn pos(&self) -> Pos2 {
        Pos2::new(self.x, self.y)
    }
}

fn paint_hint(ui: &egui::Ui, at: Pos2, text: &str, w: f32) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let g = kit::wrapped_weight(ui, text, 10.5, Weight::W400, style::text_3(), w, 1.4);
    let h = g.size().y;
    ui.painter().galley(at, g, style::text_3());
    h
}

/// A muted note under a card's fields (`.field-hint` as a block).
pub fn note(ui: &egui::Ui, f: &mut Form, text: &str) {
    let h = paint_hint(ui, f.pos(), text, f.w);
    f.y += h;
    f.stacked_switch = false;
}

/// `.page-card-desc`: 12 px muted text, the empty and loading lines.
pub fn desc(ui: &egui::Ui, f: &mut Form, text: &str) {
    let g = kit::wrapped_weight(ui, text, 12.0, Weight::W400, style::text_3(), f.w, 1.5);
    let h = g.size().y;
    ui.painter().galley(f.pos(), g, style::text_3());
    f.y += h;
    f.stacked_switch = false;
}

/// The caption of a field: 10 px capitals, with the hint icon beside it when
/// there is `info`. Returns the height.
fn caption(ui: &mut egui::Ui, f: &Form, label: &str, info: &str, id: &str) -> f32 {
    if label.is_empty() {
        return 0.0;
    }
    let g = style::galley(ui, &label.to_uppercase(), 10.0, Weight::W600, style::text_3(), 0.7);
    let h = g.size().y.max(if info.is_empty() { 0.0 } else { 16.0 });
    let gw = g.size().x;
    ui.painter()
        .galley(Pos2::new(f.x, f.y + (h - g.size().y) / 2.0), g, style::text_3());
    if !info.is_empty() {
        popover::info(ui, Pos2::new(f.x + gw + 6.0 + 8.0, f.y + h / 2.0), label, info, id);
    }
    h
}

/// A `.field`: caption, the control `draw` paints at the given `x, y, w` and
/// returns its height of, then the hint.
pub fn field(
    ui: &mut egui::Ui,
    f: &mut Form,
    label: &str,
    info: &str,
    hint: &str,
    draw: impl FnOnce(&mut egui::Ui, f32, f32, f32) -> f32,
) {
    let mut y = f.y;
    let cap = caption(ui, f, label, info, label);
    if cap > 0.0 {
        y += cap + FIELD_GAP;
    }
    y += draw(ui, f.x, y, f.w);
    if !hint.is_empty() {
        y += FIELD_GAP;
        y += paint_hint(ui, Pos2::new(f.x, y), hint, f.w);
    }
    f.y = y;
    f.stacked_switch = false;
}

/// A `.field` holding a text box. Returns the box's response.
#[allow(clippy::too_many_arguments)]
pub fn input(
    ui: &mut egui::Ui,
    f: &mut Form,
    label: &str,
    info: &str,
    hint: &str,
    buf: &mut String,
    placeholder: &str,
    mono: bool,
    secret: bool,
    id: &str,
) -> egui::Response {
    let mut out = None;
    field(ui, f, label, info, hint, |ui, x, y, w| {
        let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, CONTROL_H));
        let mut t = crate::ui::field::TextField::new(buf)
            .hint(placeholder)
            .mono(mono)
            .pad(11.0, 11.0);
        if secret {
            t = t.password();
        }
        out = Some(t.show(ui, r, id));
        CONTROL_H
    });
    out.expect("the field is drawn")
}

/// What a switch row says and shows.
pub struct Switch<'a> {
    pub label: &'a str,
    pub info: &'a str,
    pub hint: &'a str,
    pub on: bool,
    pub enabled: bool,
    pub title: &'a str,
}

impl<'a> Switch<'a> {
    pub fn new(label: &'a str, hint: &'a str, on: bool) -> Switch<'a> {
        Switch {
            label,
            info: "",
            hint,
            on,
            enabled: true,
            title: "",
        }
    }
    pub fn info(mut self, info: &'a str) -> Self {
        self.info = info;
        self
    }
    pub fn enabled(mut self, on: bool, title: &'a str) -> Self {
        self.enabled = on;
        self.title = title;
        self
    }
}

/// A `.field-switch`: the caption at the left, the switch at the right, the
/// hint under both. Returns the new value when the switch was pressed. Two in
/// a row are ruled apart.
pub fn switch_field(
    ui: &mut egui::Ui,
    f: &mut Form,
    id: impl std::hash::Hash + std::fmt::Debug,
    s: Switch<'_>,
    caption_color: Color32,
    caption_size: f32,
) -> Option<bool> {
    if f.stacked_switch {
        f.y += 12.0;
        ui.painter().hline(
            egui::Rangef::new(f.x, f.x + f.w),
            f.y + 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        f.y += 1.0 + 12.0;
    }
    let info_w = if s.info.is_empty() { 0.0 } else { 6.0 + 16.0 };
    let g = kit::wrapped_weight(
        ui,
        s.label,
        caption_size,
        Weight::W400,
        caption_color,
        f.w - 38.0 - 12.0 - info_w,
        1.21,
    );
    let row_h = g.size().y.max(22.0);
    let gw = g.size().x;
    ui.painter()
        .galley(Pos2::new(f.x, f.y + (row_h - g.size().y) / 2.0), g, caption_color);
    if !s.info.is_empty() {
        popover::info(
            ui,
            Pos2::new(f.x + gw + 6.0 + 8.0, f.y + row_h / 2.0),
            s.label,
            s.info,
            ("switchinfo", s.label),
        );
    }
    let at = Pos2::new(f.x + f.w - 38.0, f.y + (row_h - 22.0) / 2.0);
    let mut pressed = kit::switch(ui, at, s.on, s.enabled, id);
    if !s.title.is_empty() && !s.enabled {
        ui.interact(
            Rect::from_min_size(at, Vec2::new(38.0, 22.0)),
            ui.scope_id().with(("switchtip", s.label)),
            egui::Sense::hover(),
        )
        .on_hover_text(s.title);
        pressed = false;
    }
    let mut y = f.y + row_h;
    if !s.hint.is_empty() {
        y += 8.0;
        y += paint_hint(ui, Pos2::new(f.x, y), s.hint, f.w - 50.0);
    }
    f.y = y;
    f.stacked_switch = true;
    pressed.then_some(!s.on)
}

/// The usual switch: 12 px caption in the main colour.
pub fn switch(
    ui: &mut egui::Ui,
    f: &mut Form,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    hint: &str,
    on: bool,
) -> Option<bool> {
    switch_field(ui, f, id, Switch::new(label, hint, on), style::text_1(), 12.0)
}

/// `.field-ruled`: a hairline and 12 px above the next field, 4 px margin.
pub fn ruled(ui: &egui::Ui, f: &mut Form) {
    f.y += 4.0;
    ui.painter().hline(
        egui::Rangef::new(f.x, f.x + f.w),
        f.y + 0.5,
        Stroke::new(1.0, style::border()),
    );
    f.y += 1.0 + 12.0;
    f.stacked_switch = false;
}

/// `.page-row`: a label and a mono value, 10 px above and below, a hairline
/// under it unless it is the last.
pub fn row(ui: &egui::Ui, f: &mut Form, label: &str, value: &str, last: bool) {
    let l = style::galley(ui, label, 13.0, Weight::W400, style::text_1(), 0.0);
    let v = style::galley_mono(ui, value, 13.0, style::text_2());
    let h = l.size().y.max(v.size().y);
    let top = f.y + 10.0;
    ui.painter()
        .galley(Pos2::new(f.x, top + (h - l.size().y) / 2.0), l, style::text_1());
    ui.painter().galley(
        Pos2::new(f.x + f.w - v.size().x, top + (h - v.size().y) / 2.0),
        v,
        style::text_2(),
    );
    f.y = top + h + 10.0;
    if !last {
        ui.painter().hline(
            egui::Rangef::new(f.x, f.x + f.w),
            f.y + 0.5,
            Stroke::new(1.0, style::border_soft()),
        );
        f.y += 1.0;
    }
    f.stacked_switch = false;
}

/// How a `.ui-msg` is coloured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Plain,
    Ok,
    Err,
    Warn,
}

/// `.ui-msg`: a tinted line with a status dot for a severity.
pub fn message(ui: &egui::Ui, f: &mut Form, text: &str, tone: Tone) {
    if text.is_empty() {
        return;
    }
    let (color, fill) = match tone {
        Tone::Plain => (style::text_2(), Color32::TRANSPARENT),
        Tone::Ok => (style::healthy(), style::healthy_dark()),
        Tone::Err => (style::critical(), style::critical_dark()),
        Tone::Warn => (style::warning(), style::warning_dark()),
    };
    let left = if tone == Tone::Plain { 10.0 } else { 25.0 };
    let g = kit::wrapped_weight(ui, text, 11.5, Weight::W400, color, f.w - left - 10.0, 1.4);
    let rect = f.rect(g.size().y + 16.0);
    if tone != Tone::Plain {
        ui.painter().rect_filled(rect, style::RADIUS_BTN, fill);
        ui.painter().rect_stroke(
            rect,
            style::RADIUS_BTN,
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
        ui.painter()
            .circle_filled(Pos2::new(rect.left() + 14.0, rect.top() + 8.0 + 11.5 * 0.7), 3.5, color);
    }
    ui.painter()
        .galley(Pos2::new(rect.left() + left, rect.top() + 8.0), g, color);
    f.y = rect.bottom();
    f.stacked_switch = false;
}

/// `.ui-actions`: buttons in a row, 8 px apart, wrapping at the column.
/// Returns the index pressed.
pub fn actions(ui: &mut egui::Ui, f: &mut Form, buttons: Vec<kit::Button<'_>>) -> Option<usize> {
    let sizes: Vec<(f32, f32)> = buttons.iter().map(|b| (b.width(ui), b.height())).collect();
    let (mut x, mut y, mut line_h) = (f.x, f.y, 0.0f32);
    let mut hit = None;
    for (i, b) in buttons.into_iter().enumerate() {
        let (bw, bh) = sizes[i];
        if x > f.x && x + bw > f.x + f.w {
            x = f.x;
            y += line_h + 8.0;
            line_h = 0.0;
        }
        if b.show(
            ui,
            Rect::from_min_size(Pos2::new(x, y), Vec2::new(bw, bh)),
            ("formbtn", i, y as i32, x as i32),
        ) {
            hit = Some(i);
        }
        x += bw + 8.0;
        line_h = line_h.max(bh);
    }
    f.y = y + line_h;
    f.stacked_switch = false;
    hit
}

/// A button as wide as the column (`data-shape="block"`).
pub fn block_button(ui: &mut egui::Ui, f: &mut Form, button: kit::Button<'_>, id: &str) -> bool {
    let h = button.height();
    let pressed = button.show(ui, f.rect(h), ("block", id));
    f.y += h;
    f.stacked_switch = false;
    pressed
}

/// `.params-desc` (an inset box of explaining text) or, with a tone, the amber
/// `.params-banner`.
pub fn box_note(ui: &egui::Ui, f: &mut Form, text: &str, tone: Tone) {
    let (fill, line, color, size, lh) = match tone {
        Tone::Warn => (style::warning_dark(), style::warning(), style::warning(), 11.5, 1.4),
        _ => (style::surface_2(), style::border_soft(), style::text_2(), 12.0, 1.55),
    };
    let g = kit::wrapped_weight(ui, text, size, Weight::W400, color, f.w - 28.0, lh);
    let rect = f.rect(g.size().y + 24.0);
    ui.painter().rect_filled(rect, style::RADIUS_BTN, fill);
    ui.painter().rect_stroke(
        rect,
        style::RADIUS_BTN,
        Stroke::new(1.0, line),
        egui::StrokeKind::Inside,
    );
    ui.painter().galley(rect.min + Vec2::new(14.0, 12.0), g, color);
    f.y = rect.bottom();
    f.stacked_switch = false;
}

/// A title as `.logs-head .page-section-title`: 13 px capitals with the rule
/// under the words only. Returns the box's width and height.
pub fn head_title(ui: &egui::Ui, at: Pos2, text: &str) -> (f32, f32) {
    let g = style::galley(ui, &text.to_uppercase(), 13.0, Weight::W600, style::text_2(), 0.6);
    let (w, h) = (g.size().x, g.size().y);
    ui.painter().galley(at, g, style::text_2());
    ui.painter().hline(
        egui::Rangef::new(at.x, at.x + w),
        at.y + h + 8.5,
        Stroke::new(1.0, style::border_soft()),
    );
    (w, h + 9.0)
}

/// One option of `option_cards`.
#[derive(Clone, Debug)]
pub struct Opt {
    pub id: String,
    pub label: String,
    pub desc: String,
    pub icon: Option<&'static str>,
    pub swatch: Option<[u32; 3]>,
}

impl Opt {
    pub fn new(id: &str, label: &str, desc: &str) -> Opt {
        Opt {
            id: id.into(),
            label: label.into(),
            desc: desc.into(),
            icon: None,
            swatch: None,
        }
    }
}

/// `.option-cards`: a grid of selectable cards. Returns the id picked.
pub fn option_cards(
    ui: &mut egui::Ui,
    f: &mut Form,
    id: &str,
    opts: &[Opt],
    selected: &str,
    columns: usize,
) -> Option<String> {
    let cols = columns.clamp(1, opts.len().max(1));
    let card_w = (f.w - 8.0 * (cols as f32 - 1.0)) / cols as f32;
    let mut picked = None;
    let mut bottom = f.y;
    let mut row_top = f.y;
    let mut row_h = 0.0f32;
    for (row_start, chunk) in opts.chunks(cols).enumerate().map(|(r, c)| (r * cols, c)) {
        let text_w = |o: &Opt| {
            let lead = if o.swatch.is_some() {
                34.0 + 10.0
            } else if o.icon.is_some() {
                16.0 + 10.0
            } else {
                0.0
            };
            card_w - 2.0 - 24.0 - lead
        };
        let heights: Vec<f32> = chunk
            .iter()
            .map(|o| {
                let l = style::galley(ui, &o.label, 12.0, Weight::W600, style::text_1(), 0.0);
                let d = style::galley(ui, &o.desc, 10.5, Weight::W400, style::text_3(), 0.0);
                let text = l.size().y + if o.desc.is_empty() { 0.0 } else { 2.0 + d.size().y };
                let lead = if o.swatch.is_some() {
                    22.0
                } else if o.icon.is_some() {
                    16.0
                } else {
                    0.0
                };
                2.0 + 20.0 + text.max(lead)
            })
            .collect();
        row_h = heights.iter().copied().fold(0.0, f32::max);
        for (i, o) in chunk.iter().enumerate() {
            let col = i as f32;
            let r = Rect::from_min_size(Pos2::new(f.x + col * (card_w + 8.0), row_top), Vec2::new(card_w, row_h));
            let resp = ui.interact(r, ui.scope_id().with((id, row_start + i)), egui::Sense::click());
            let on = o.id == selected;
            let p = ui.painter();
            if on {
                p.rect_stroke(
                    r.expand(1.0),
                    style::RADIUS_BTN + 1,
                    Stroke::new(1.0, style::accent()),
                    egui::StrokeKind::Inside,
                );
                p.rect_filled(r, style::RADIUS_BTN, style::accent_dark());
                p.rect_stroke(
                    r,
                    style::RADIUS_BTN,
                    Stroke::new(1.0, style::accent()),
                    egui::StrokeKind::Inside,
                );
            } else {
                let (fill, line) = if resp.hovered() {
                    (style::glass(0.95), style::tint(0.15))
                } else {
                    (style::glass(0.82), style::tint(0.09))
                };
                p.rect_filled(r, style::RADIUS_BTN, fill);
                p.rect_stroke(r, style::RADIUS_BTN, Stroke::new(1.0, line), egui::StrokeKind::Inside);
            }
            let text_color = if on || resp.hovered() {
                style::text_1()
            } else {
                style::text_2()
            };
            let mut tx = r.left() + 13.0;
            if let Some(sw) = o.swatch {
                let s = Rect::from_min_size(Pos2::new(tx, r.center().y - 11.0), Vec2::new(34.0, 22.0));
                let third = s.width() / 3.0;
                for (k, c) in sw.iter().enumerate() {
                    let cell = Rect::from_min_size(
                        Pos2::new(s.left() + third * k as f32, s.top()),
                        Vec2::new(third, s.height()),
                    );
                    ui.painter().rect_filled(cell, 0, rgb(*c));
                }
                ui.painter()
                    .rect_stroke(s, 5, Stroke::new(1.0, style::tint(0.12)), egui::StrokeKind::Inside);
                tx += 34.0 + 10.0;
            } else if let Some(icon) = o.icon {
                icons::paint(
                    ui,
                    icon,
                    Rect::from_center_size(Pos2::new(tx + 8.0, r.center().y), Vec2::splat(16.0)),
                    text_color,
                );
                tx += 16.0 + 10.0;
            }
            let l = style::galley(ui, &o.label, 12.0, Weight::W600, text_color, 0.0);
            let d_color = if on { style::text_2() } else { style::text_3() };
            let d = style::galley(ui, &o.desc, 10.5, Weight::W400, d_color, 0.0);
            let total = l.size().y + if o.desc.is_empty() { 0.0 } else { 2.0 + d.size().y };
            let clip = Rect::from_min_size(
                Pos2::new(tx, r.top() + 1.0),
                Vec2::new(text_w(o).max(10.0), r.height() - 2.0),
            );
            let top = r.center().y - total / 2.0;
            let lh = l.size().y;
            let pa = ui.painter_at(clip);
            pa.galley(Pos2::new(tx, top), l, text_color);
            if !o.desc.is_empty() {
                pa.galley(Pos2::new(tx, top + lh + 2.0), d, d_color);
            }
            if resp.clicked() && !on {
                picked = Some(o.id.clone());
            }
        }
        bottom = row_top + row_h;
        row_top = bottom + 8.0;
    }
    let _ = row_h;
    f.y = bottom;
    f.stacked_switch = false;
    picked
}

fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// `.ui-slider`: a stepped slider with a mark and a caption per step. Returns
/// the step index while it is dragged or clicked, and whether the pointer was
/// released this frame.
pub fn slider(ui: &mut egui::Ui, f: &mut Form, id: &str, steps: &[String], current: usize) -> Option<(usize, bool)> {
    let thumb = 16.0;
    let row = Rect::from_min_size(Pos2::new(f.x, f.y + 4.0), Vec2::new(f.w, thumb));
    let n = steps.len().max(2) as f32 - 1.0;
    let pos_of = |i: usize| row.left() + thumb / 2.0 + (f.w - thumb) * i as f32 / n;
    let resp = ui.interact(row, ui.scope_id().with(("slider", id)), egui::Sense::click_and_drag());
    let mut out = None;
    if resp.dragged() || resp.clicked() || resp.drag_stopped() {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = ((p.x - row.left() - thumb / 2.0) / (f.w - thumb)).clamp(0.0, 1.0);
            let i = (t * n).round() as usize;
            out = Some((i, resp.drag_stopped() || resp.clicked()));
        }
    }
    let shown = out.map_or(current, |o| o.0);
    let p = ui.painter();
    let cy = row.center().y;
    p.rect_filled(
        Rect::from_min_max(Pos2::new(row.left(), cy - 2.0), Pos2::new(row.right(), cy + 2.0)),
        2,
        style::surface_3(),
    );
    let fill_to = pos_of(shown);
    p.rect_filled(
        Rect::from_min_max(Pos2::new(row.left(), cy - 2.0), Pos2::new(fill_to, cy + 2.0)),
        2,
        style::accent(),
    );
    for i in 0..steps.len() {
        let x = pos_of(i);
        let color = if i <= shown {
            style::text_inverted().gamma_multiply(0.6)
        } else {
            style::border_strong()
        };
        p.rect_filled(Rect::from_center_size(Pos2::new(x, cy), Vec2::new(2.0, 9.0)), 1, color);
    }
    p.circle_filled(Pos2::new(fill_to, cy + 1.0), thumb / 2.0, style::tint(0.12));
    p.circle_filled(Pos2::new(fill_to, cy), thumb / 2.0, style::surface_1());
    p.circle_filled(Pos2::new(fill_to, cy), thumb / 2.0 - 2.0, style::accent());
    let label_y = row.bottom() + 4.0 + 2.0 - 4.0;
    for (i, s) in steps.iter().enumerate() {
        let c = if i == shown { style::accent() } else { style::text_3() };
        let g = style::galley(ui, s, 9.5, Weight::W600, c, 0.3);
        let w = g.size().x + 4.0;
        let lr = Rect::from_min_size(Pos2::new(pos_of(i) - w / 2.0, label_y), Vec2::new(w, 15.0));
        let r = ui.interact(lr, ui.scope_id().with(("sliderstep", id, i)), egui::Sense::click());
        let c = if r.hovered() && i != shown { style::text_2() } else { c };
        let g = style::galley(ui, s, 9.5, Weight::W600, c, 0.3);
        ui.painter()
            .galley(Pos2::new(lr.left() + 2.0, lr.center().y - g.size().y / 2.0), g, c);
        if r.clicked() {
            out = Some((i, true));
        }
    }
    f.y = label_y + 15.0;
    f.stacked_switch = false;
    out
}

/// `.settings-tab`: a row of pills with an icon each, wrapping. Returns the
/// index pressed. `dots` marks a pill with the notice dot (a release waiting).
pub fn pills(
    ui: &mut egui::Ui,
    f: &mut Form,
    items: &[(&str, &'static str)],
    selected: usize,
    dots: &[bool],
) -> Option<usize> {
    let mut x = f.x;
    let mut y = f.y;
    let mut hit = None;
    let mut line_h = 0.0f32;
    for (i, (label, icon)) in items.iter().enumerate() {
        let on = i == selected;
        let color = if on { style::text_1() } else { style::text_3() };
        let g = style::galley(ui, label, 11.5, Weight::W600, color, 0.0);
        let w = 2.0 + 11.0 + 14.0 + 7.0 + g.size().x + 13.0;
        let h = 2.0 + 14.0 + g.size().y.max(14.0);
        if x > f.x && x + w > f.x + f.w {
            x = f.x;
            y += line_h + 6.0;
            line_h = 0.0;
        }
        let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
        let resp = ui.interact(r, ui.scope_id().with(("pill", i)), egui::Sense::click());
        let (fill, line) = if on {
            (style::accent_dim(), style::accent())
        } else if resp.hovered() {
            (style::surface_hover(), style::border_soft())
        } else {
            (Color32::TRANSPARENT, style::border_soft())
        };
        let color = if !on && resp.hovered() { style::text_1() } else { color };
        let g = style::galley(ui, label, 11.5, Weight::W600, color, 0.0);
        let p = ui.painter();
        p.rect_filled(r, 255, fill);
        p.rect_stroke(r, 255, Stroke::new(1.0, line), egui::StrokeKind::Inside);
        icons::paint(
            ui,
            icon,
            Rect::from_center_size(Pos2::new(r.left() + 12.0 + 7.0, r.center().y), Vec2::splat(14.0)),
            if on { style::accent_bright() } else { color },
        );
        ui.painter().galley(
            Pos2::new(r.left() + 12.0 + 14.0 + 7.0, r.center().y - g.size().y / 2.0),
            g,
            color,
        );
        if dots.get(i).copied().unwrap_or(false) {
            kit::dot(
                ui.painter(),
                Pos2::new(r.right() - 6.5, r.top() + 6.5),
                style::accent(),
                true,
            );
        }
        if resp.clicked() {
            hit = Some(i);
        }
        x += w + 6.0;
        line_h = line_h.max(h);
    }
    f.y = y + line_h;
    f.stacked_switch = false;
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_moves_down_and_forgets_the_switch_above() {
        let mut f = Form::new(10.0, 20.0, 300.0);
        f.stacked_switch = true;
        f.skip(8.0);
        assert_eq!((f.y, f.stacked_switch), (28.0, false));
        assert_eq!(f.rect(5.0).max, Pos2::new(310.0, 33.0));
    }
}
