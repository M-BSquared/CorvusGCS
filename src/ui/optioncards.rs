//! `Corvus.ui.optionCards` (`.option-cards`, `.option-card` of
//! components.css): a grid of selectable cards, each a label with a quiet
//! description under it. The chosen one wears the accent border with a ring
//! outside it.

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};

use crate::ui::style::{self, Weight};

/// One card.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub id: String,
    pub label: String,
    pub desc: String,
}

impl Card {
    pub fn new(id: impl Into<String>, label: impl Into<String>, desc: impl Into<String>) -> Card {
        Card {
            id: id.into(),
            label: label.into(),
            desc: desc.into(),
        }
    }
}

/// `.option-cards { gap: 8px }`.
const GAP: f32 = 8.0;
/// `.option-card { padding: 10px 12px }`, with its 1 px border.
const PAD_X: f32 = 12.0;
const PAD_Y: f32 = 10.0;

fn card_height(has_desc: bool) -> f32 {
    // what the web fonts measure: a 12 px label and a 10.5 px description
    let label = 14.5;
    let desc = if has_desc { 2.0 + 12.5 } else { 0.0 };
    2.0 + 2.0 * PAD_Y + label + desc
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
    let h: f32 = rows
        .iter()
        .map(|r| card_height(r.iter().any(|c| !c.desc.is_empty())))
        .sum();
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
        let h = card_height(row.iter().any(|c| !c.desc.is_empty()));
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
                    Stroke::new(1.0, if hover { style::tint(0.14) } else { style::tint(0.09) }),
                    egui::StrokeKind::Inside,
                );
            }
            let label_c = if on || hover { style::text_1() } else { style::text_2() };
            let avail = cw - 2.0 - 2.0 * PAD_X;
            let label = ellipsize(ui, &card.label, 12.0, Weight::W600, avail);
            let lg = style::galley(ui, &label, 12.0, Weight::W600, label_c, 0.0);
            let lh = lg.size().y;
            let content = lh + if card.desc.is_empty() { 0.0 } else { 2.0 + 12.5 };
            let top = rect.center().y - content / 2.0;
            ui.painter()
                .galley(Pos2::new(rect.left() + 1.0 + PAD_X, top), lg, label_c);
            if !card.desc.is_empty() {
                let desc = ellipsize(ui, &card.desc, 10.5, Weight::W400, avail);
                let dc = if on { style::text_2() } else { style::text_3() };
                let dg = style::galley(ui, &desc, 10.5, Weight::W400, dc, 0.0);
                ui.painter()
                    .galley(Pos2::new(rect.left() + 1.0 + PAD_X, top + lh + 2.0), dg, dc);
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
