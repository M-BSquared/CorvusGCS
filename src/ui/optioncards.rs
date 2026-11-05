//! `Corvus.ui.optionCards` (`.option-cards`, `.option-card` of
//! components.css): a grid of selectable cards, each a label with a quiet
//! description under it. The chosen one wears the accent border with a ring
//! outside it.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::ui::icons;
use crate::ui::style::{self, Weight};

/// One card.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub id: String,
    pub label: String,
    pub desc: String,
    pub icon: Option<&'static str>,
    pub swatch: Option<[u32; 3]>,
}

impl Card {
    pub fn new(id: impl Into<String>, label: impl Into<String>, desc: impl Into<String>) -> Card {
        Card {
            id: id.into(),
            label: label.into(),
            desc: desc.into(),
            icon: None,
            swatch: None,
        }
    }

    /// A 16 px icon before the words (`.option-card-icon`).
    pub fn icon(mut self, icon: &'static str) -> Card {
        self.icon = Some(icon);
        self
    }

    /// Three colours butted into one 34 by 22 pill (`.option-card-swatch`).
    pub fn swatch(mut self, colors: [u32; 3]) -> Card {
        self.swatch = Some(colors);
        self
    }

    fn lead_w(&self) -> f32 {
        if self.swatch.is_some() {
            SWATCH_W + LEAD_GAP
        } else if self.icon.is_some() {
            ICON + LEAD_GAP
        } else {
            0.0
        }
    }

    fn lead_h(&self) -> f32 {
        if self.swatch.is_some() {
            SWATCH_H
        } else if self.icon.is_some() {
            ICON
        } else {
            0.0
        }
    }
}

/// `.option-cards { gap: 8px }`.
const GAP: f32 = 8.0;
/// `.option-card { padding: 10px 12px }`, with its 1 px border.
const PAD_X: f32 = 12.0;
const PAD_Y: f32 = 10.0;

const SWATCH_W: f32 = 34.0;
const SWATCH_H: f32 = 22.0;
const ICON: f32 = 16.0;
/// `.option-card { gap: 10px }` between the lead and the words.
const LEAD_GAP: f32 = 10.0;

fn text_height(has_desc: bool) -> f32 {
    // what the web fonts measure: a 12 px label and a 10.5 px description
    let label = 14.5;
    let desc = if has_desc { 2.0 + 12.5 } else { 0.0 };
    label + desc
}

#[cfg(test)]
fn card_height(has_desc: bool) -> f32 {
    2.0 + 2.0 * PAD_Y + text_height(has_desc)
}

fn row_height(row: &[Card]) -> f32 {
    let text = text_height(row.iter().any(|c| !c.desc.is_empty()));
    let lead = row.iter().map(Card::lead_h).fold(0.0, f32::max);
    2.0 + 2.0 * PAD_Y + text.max(lead)
}

fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// The text trimmed with an ellipsis to fit `width` (`text-overflow`).
pub fn ellipsize(ui: &egui::Ui, text: &str, size: f32, weight: Weight, width: f32) -> String {
    let fits = |t: &str| style::galley(ui, t, size, weight, style::text_1(), 0.0).size().x <= width;
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut n = chars.len();
    while n > 0 {
        n -= 1;
        let cut: String = chars[..n].iter().collect::<String>().trim_end().to_string() + "\u{2026}";
        if fits(&cut) {
            return cut;
        }
    }
    "\u{2026}".into()
}

/// How high the grid is at `columns` and width `w`.
#[cfg(test)]
pub fn height(cards: &[Card], columns: usize, w: f32) -> f32 {
    let _ = w;
    let columns = columns.max(1);
    let rows: Vec<&[Card]> = cards.chunks(columns).collect();
    let h: f32 = rows.iter().map(|r| row_height(r)).sum();
    h + GAP * rows.len().saturating_sub(1) as f32
}

/// Draws the grid with its top left at `(x, y)`; returns its bottom and the
/// id of the card pressed this frame.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    cards: &[Card],
    selected: Option<&str>,
    columns: usize,
    id: &str,
) -> (f32, Option<String>) {
    let columns = columns.max(1);
    let cw = (w - GAP * (columns as f32 - 1.0)) / columns as f32;
    let mut pressed = None;
    let mut cy = y;
    for (ri, row) in cards.chunks(columns).enumerate() {
        let h = row_height(row);
        for (ci, card) in row.iter().enumerate() {
            let rect = Rect::from_min_size(Pos2::new(x + ci as f32 * (cw + GAP), cy), Vec2::new(cw, h));
            let resp = ui.interact(rect, ui.scope_id().with((id, ri, ci)), egui::Sense::click());
            let on = selected == Some(card.id.as_str());
            let hover = resp.hovered();
            let p = ui.painter();
            if on {
                p.rect_filled(rect, style::RADIUS_BTN, style::accent_dark());
                p.rect_stroke(
                    rect.expand(1.0),
                    style::RADIUS_BTN + 1,
                    Stroke::new(1.0, style::accent()),
                    egui::StrokeKind::Inside,
                );
                p.rect_stroke(
                    rect,
                    style::RADIUS_BTN,
                    Stroke::new(1.0, style::accent()),
                    egui::StrokeKind::Inside,
                );
            } else {
                p.rect_filled(
                    rect,
                    style::RADIUS_BTN,
                    if hover { style::glass(0.95) } else { style::glass(0.82) },
                );
                p.rect_stroke(
                    rect,
                    style::RADIUS_BTN,
                    Stroke::new(1.0, if hover { style::tint(0.15) } else { style::tint(0.09) }),
                    egui::StrokeKind::Inside,
                );
            }
            let label_c = if on || hover { style::text_1() } else { style::text_2() };
            let mut tx = rect.left() + 1.0 + PAD_X;
            if let Some(sw) = card.swatch {
                let s = Rect::from_min_size(
                    Pos2::new(tx, rect.center().y - SWATCH_H / 2.0),
                    Vec2::new(SWATCH_W, SWATCH_H),
                );
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
            } else if let Some(icon) = card.icon {
                icons::paint(
                    ui,
                    icon,
                    Rect::from_center_size(Pos2::new(tx + ICON / 2.0, rect.center().y), Vec2::splat(ICON)),
                    label_c,
                );
            }
            tx += card.lead_w();
            let avail = (rect.right() - 1.0 - PAD_X - tx).max(10.0);
            let label = ellipsize(ui, &card.label, 12.0, Weight::W600, avail);
            let lg = style::galley(ui, &label, 12.0, Weight::W600, label_c, 0.0);
            let lh = lg.size().y;
            let content = lh + if card.desc.is_empty() { 0.0 } else { 2.0 + 12.5 };
            let top = rect.center().y - content / 2.0;
            ui.painter().galley(Pos2::new(tx, top), lg, label_c);
            if !card.desc.is_empty() {
                let desc = ellipsize(ui, &card.desc, 10.5, Weight::W400, avail);
                let dc = if on { style::text_2() } else { style::text_3() };
                let dg = style::galley(ui, &desc, 10.5, Weight::W400, dc, 0.0);
                ui.painter().galley(Pos2::new(tx, top + lh + 2.0), dg, dc);
            }
            if resp.clicked() {
                pressed = Some(card.id.clone());
            }
        }
        cy += h + GAP;
    }
    (cy - if cards.is_empty() { 0.0 } else { GAP }, pressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cards(n: usize, desc: &str) -> Vec<Card> {
        (0..n)
            .map(|i| Card::new(i.to_string(), format!("Type {i}"), desc))
            .collect()
    }

    #[test]
    fn a_card_with_a_description_is_two_lines_tall() {
        assert!((card_height(true) - (2.0 + 20.0 + 14.5 + 2.0 + 12.5)).abs() < 0.01);
        assert!((card_height(false) - (2.0 + 20.0 + 14.5)).abs() < 0.01);
    }

    #[test]
    fn the_grid_is_as_many_rows_as_the_columns_need_with_eight_between() {
        let one = card_height(true);
        assert!((height(&cards(3, "g"), 3, 600.0) - one).abs() < 0.01);
        assert!((height(&cards(4, "g"), 3, 600.0) - (2.0 * one + 8.0)).abs() < 0.01);
        assert!((height(&cards(7, "g"), 3, 600.0) - (3.0 * one + 16.0)).abs() < 0.01);
        assert_eq!(height(&[], 3, 600.0), 0.0);
    }

    #[test]
    fn a_row_is_as_tall_as_its_tallest_card() {
        let mut c = cards(3, "");
        c[1].desc = "group".into();
        assert!((height(&c, 3, 600.0) - card_height(true)).abs() < 0.01);
    }
}
