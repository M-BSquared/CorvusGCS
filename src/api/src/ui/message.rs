//! `Corvus.ui.message` (`.ui-msg`) and the notice boxes (`.params-desc`,
//! `.params-banner`): the one painter and the one `Tone` every page uses for
//! a line of feedback or a note that stands in a box.

use std::sync::Arc;

use egui::{self, Color32, Galley, Painter, Pos2, Rect, Stroke, Vec2};

use crate::ui::card::{self, Look};
use crate::ui::icons;
use crate::ui::kit;
use crate::ui::style::{self, Weight};
use crate::ui::tip::HoverTr as _;

/// How a message is dressed: plain text, or `Ok`, `Err` or `Warn` in a tinted
/// box with a bar at its left edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Ok,
    Err,
    Warn,
}

/// The text of a notice starts this far right of its box, behind the bar.
pub const NOTICE_LEFT: f32 = 22.0;
/// The room kept right of the text of a notice.
pub const NOTICE_RIGHT: f32 = 14.0;
/// The room above and under the text of a notice, inside the border.
pub const NOTICE_PAD_Y: f32 = 8.0;
/// A notice's corner.
pub const NOTICE_RADIUS: u8 = 10;

/// The four colours of one tone's notice.
#[derive(Debug, Clone, Copy)]
pub struct Notice {
    pub fill: Color32,
    pub line: Color32,
    pub bar: Color32,
    pub text: Color32,
}

/// The colours of a notice. A warning is a peach orange, not the amber of
/// the status chips, so a box of it does not read as every other app's.
pub fn notice(tone: Tone) -> Notice {
    let c = |fill, line, bar, text| Notice {
        fill: style::hex_color(fill),
        line: style::hex_color(line),
        bar: style::hex_color(bar),
        text: style::hex_color(text),
    };
    match (tone, style::is_dark()) {
        (Tone::Plain, _) => Notice {
            fill: style::surface_2(),
            line: style::border_soft(),
            bar: style::text_3(),
            text: style::text_2(),
        },
        (Tone::Ok, false) => c(0xE8F5EE, 0x9FD4B5, 0x2F9461, 0x0E4B2D),
        (Tone::Err, false) => c(0xFBEBEB, 0xE7A3A3, 0xB34A4A, 0x5E1519),
        (Tone::Warn, false) => c(0xFEF1E8, 0xF4C4A3, 0xE08A4F, 0x7A3F1A),
        (Tone::Ok, true) => c(0x14301F, 0x2C6246, 0x3FB878, 0xC3EBD4),
        (Tone::Err, true) => c(0x3A1A1C, 0x79363A, 0xE0605F, 0xF8C9C7),
        (Tone::Warn, true) => c(0x3A2A1E, 0x7C5638, 0xEFA06A, 0xF8DCC4),
    }
}

/// The bar at the left edge of a notice.
pub fn paint_bar(painter: &Painter, rect: Rect, tone: Tone) {
    if tone == Tone::Plain {
        return;
    }
    bar(painter, rect, notice(tone).bar);
}

fn bar(painter: &Painter, rect: Rect, color: Color32) {
    let inset = 7.0f32.min(((rect.height() - 8.0) / 2.0).max(3.0));
    let bar = Rect::from_min_max(
        Pos2::new(rect.left() + 8.0, rect.top() + inset),
        Pos2::new(rect.left() + 11.5, rect.bottom() - inset),
    );
    painter.rect_filled(bar, 2, color);
}

/// A notice's box, border and bar at `rect`, for a view or a plugin that lays
/// out its own content in it (from `NOTICE_LEFT` on).
pub fn paint_notice(painter: &Painter, rect: Rect, tone: Tone) {
    surface(painter, rect, notice(tone));
    paint_bar(painter, rect, tone);
}

fn surface(painter: &Painter, rect: Rect, n: Notice) {
    painter.rect_filled(rect, NOTICE_RADIUS, n.fill);
    painter.rect_stroke(rect, NOTICE_RADIUS, Stroke::new(1.0, n.line), egui::StrokeKind::Inside);
}

const ICON: f32 = 13.0;
const ICON_GAP: f32 = 7.0;

/// The one notice box, for the program and its plugins: the caller says what
/// it holds and how tight it is. It is as tall as its content, so a long text
/// never leaves it. A `Tone::Plain` box is grey and has no bar.
#[derive(Debug, Clone, Copy)]
#[must_use]
pub struct NoticeBox<'a> {
    text: &'a str,
    tone: Tone,
    icon: Option<&'static str>,
    one_line: bool,
    size: f32,
    line: f32,
    pad_y: f32,
    colors: Option<Notice>,
}

impl<'a> NoticeBox<'a> {
    pub fn new(tone: Tone, text: &'a str) -> Self {
        NoticeBox {
            text,
            tone,
            icon: None,
            one_line: false,
            size: 11.5,
            line: 1.4,
            pad_y: NOTICE_PAD_Y,
            colors: None,
        }
    }

    /// An icon before the text, in the colour of the bar.
    pub fn icon(mut self, name: &'static str) -> Self {
        self.icon = Some(name);
        self
    }

    /// One line, cut with an ellipsis; the whole text is then the hover tip.
    pub fn one_line(mut self) -> Self {
        self.one_line = true;
        self
    }

    /// The text's size, and its line height as a multiple of it.
    pub fn text_size(mut self, size: f32, line: f32) -> Self {
        (self.size, self.line) = (size, line);
        self
    }

    /// The room above and under the text.
    pub fn pad_y(mut self, pad: f32) -> Self {
        self.pad_y = pad;
        self
    }

    /// Colours of the caller's own in place of the tone's.
    pub fn colors(mut self, colors: Notice) -> Self {
        self.colors = Some(colors);
        self
    }

    fn lead(&self) -> f32 {
        NOTICE_LEFT + if self.icon.is_some() { ICON + ICON_GAP } else { 0.0 }
    }

    fn layout(&self, ui: &egui::Ui, w: f32, color: Color32) -> Arc<Galley> {
        let mut job = egui::text::LayoutJob::single_section(
            crate::i18n::tr(self.text).into_owned(),
            egui::TextFormat {
                font_id: style::font(self.size, Weight::W400),
                color,
                line_height: Some(self.size * self.line),
                ..Default::default()
            },
        );
        job.wrap.max_width = (w - self.lead() - NOTICE_RIGHT).max(0.0);
        if self.one_line {
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
        }
        ui.painter().layout_job(job)
    }

    fn height_of(&self, galley: &Galley) -> f32 {
        galley.size().y + 2.0 * self.pad_y + 2.0
    }

    /// How tall the box is at `w` wide.
    pub fn height(&self, ui: &egui::Ui, w: f32) -> f32 {
        self.height_of(&self.layout(ui, w, Color32::PLACEHOLDER))
    }

    /// Draws the box at `x, y`, `w` wide, and returns where it is.
    pub fn show(self, ui: &egui::Ui, x: f32, y: f32, w: f32) -> Rect {
        let n = self.colors.unwrap_or_else(|| notice(self.tone));
        let g = self.layout(ui, w, n.text);
        let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, self.height_of(&g)));
        surface(ui.painter(), rect, n);
        if self.tone != Tone::Plain {
            bar(ui.painter(), rect, n.bar);
        }
        let top = y + 1.0 + self.pad_y;
        if let Some(name) = self.icon {
            let middle = top + self.size * self.line / 2.0;
            let at = Pos2::new(x + NOTICE_LEFT + ICON / 2.0, middle);
            icons::paint(ui, name, Rect::from_center_size(at, Vec2::splat(ICON)), n.bar);
        }
        let cut = g.elided;
        ui.painter().galley(Pos2::new(x + self.lead(), top), g, n.text);
        if cut {
            ui.interact(rect, ui.scope_id().with(("notice", self.text)), egui::Sense::hover())
                .on_hover_tr(self.text);
        }
        rect
    }
}

/// The message at `x, y`, `w` wide; returns its bottom. The lines wrap inside
/// the box, so a long text makes it taller.
pub fn show(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str, tone: Tone) -> f32 {
    if tone == Tone::Plain {
        let g = kit::wrapped_weight(ui, text, 11.5, Weight::W400, style::text_2(), w - 20.0, 1.4);
        let h = g.size().y;
        ui.painter().galley(Pos2::new(x + 10.0, y + 8.0), g, style::text_2());
        return y + h + 16.0;
    }
    NoticeBox::new(tone, text).show(ui, x, y, w).bottom()
}

/// `.params-desc`, an inset box of explaining text, or with another tone than
/// `Plain` the banner that gates a card. Returns the bottom.
pub fn boxed(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str, tone: Tone) -> f32 {
    match tone {
        Tone::Plain => {
            let g = kit::wrapped_weight(ui, text, 12.0, Weight::W400, style::text_2(), w - 30.0, 1.55);
            let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 26.0));
            card::paint(ui.painter(), rect, Look::Tone(Tone::Plain));
            ui.painter()
                .galley(rect.min + Vec2::new(15.0, 13.0), g, style::text_2());
            rect.bottom()
        }
        _ => NoticeBox::new(tone, text).show(ui, x, y, w).bottom(),
    }
}
