//! Page furniture of the web interface (components.css `.page-header`,
//! `.page-section-title`, `.page-card`, `.tile`, `.setup-back`), which every
//! page that is not the map is built from.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::ui::icons;
use crate::ui::kit::{Button, Variant};
use crate::ui::style::{self, Weight};

/// `.page-view { padding: 32px 40px }`.
pub const PAD_X: f32 = 40.0;
pub const PAD_Y: f32 = 32.0;

/// `.page-header`: a 22 px title and a 13 px subtitle; returns the bottom.
pub fn header(ui: &egui::Ui, x: f32, y: f32, title: &str, subtitle: &str) -> f32 {
    let t = style::galley(ui, title, 22.0, Weight::W700, style::text_1(), 0.5);
    let th = t.size().y;
    ui.painter().galley(Pos2::new(x, y), t, style::text_1());
    let mut bottom = y + th;
    if !subtitle.is_empty() {
        let s = style::galley(ui, subtitle, 13.0, Weight::W400, style::text_3(), 0.0);
        let sh = s.size().y;
        ui.painter().galley(Pos2::new(x, bottom + 4.0), s, style::text_3());
        bottom += 4.0 + sh;
    }
    bottom
}

/// `.page-header` in a column `w` wide, the subtitle wrapping when it is
/// longer than the column (the `.setup-head` row gives it what the buttons
/// leave). Returns the bottom.
pub fn header_wrapped(ui: &egui::Ui, x: f32, y: f32, w: f32, title: &str, subtitle: &str) -> f32 {
    let t = style::galley(ui, title, 22.0, Weight::W700, style::text_1(), 0.5);
    let th = t.size().y;
    ui.painter().galley(Pos2::new(x, y), t, style::text_1());
    let mut bottom = y + th;
    if !subtitle.is_empty() {
        let s = crate::ui::kit::wrapped_weight(ui, subtitle, 13.0, Weight::W400, style::text_3(), w, 1.21);
        let sh = s.size().y;
        ui.painter().galley(Pos2::new(x, bottom + 4.0), s, style::text_3());
        bottom += 4.0 + sh;
    }
    bottom
}

/// `.page-section-title`: 13 px uppercase, a rule under it; returns the
/// y the content starts at.
pub fn section_title(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    let g = style::galley(ui, &text.to_uppercase(), 13.0, Weight::W600, style::text_2(), 0.6);
    let h = g.size().y;
    ui.painter().galley(Pos2::new(x, y + 1.0), g, style::text_2());
    let rule = y + h + 9.0;
    ui.painter().hline(
        egui::Rangef::new(x, x + w),
        rule + 0.5,
        Stroke::new(1.0, style::border_soft()),
    );
    rule + 1.0 + 12.0
}

/// `.setup-back`: a small ghost button; returns true when clicked.
pub fn back_button(ui: &mut egui::Ui, at: Pos2, label: &str) -> (f32, bool) {
    let b = Button::new(label, Variant::Ghost).icon("chevron-left").small();
    let (w, h) = (b.width(ui), b.height());
    let clicked = b.show(ui, Rect::from_min_size(at, Vec2::new(w, h)), "setupback");
    (h, clicked)
}

/// A `.setup-tile`: icon box, title, a two line description and a chevron.
pub fn tile(ui: &mut egui::Ui, rect: Rect, icon: &'static str, title: &str, desc: &str) -> bool {
    let resp = ui.interact(rect, ui.scope_id().with(("tile", title)), egui::Sense::click());
    let (fill, stroke) = if resp.hovered() {
        (style::glass(0.95), style::tint(0.15))
    } else {
        (style::glass(0.82), style::tint(0.09))
    };
    let painter = ui.painter().clone();
    {
        let r = rect;
        painter.rect_filled(r, style::RADIUS, fill);
        painter.rect_stroke(r, style::RADIUS, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    }
    let inner = rect.shrink2(Vec2::new(20.0, 18.0));
    let box_ = Rect::from_min_size(Pos2::new(inner.left(), rect.center().y - 20.0), Vec2::splat(40.0));
    painter.rect_filled(box_, style::RADIUS_BTN, style::accent_dark());
    icons::paint(
        ui,
        icon,
        Rect::from_center_size(box_.center(), Vec2::splat(20.0)),
        style::accent_bright(),
    );
    let chevron = Rect::from_center_size(Pos2::new(inner.right() - 8.0, rect.center().y), Vec2::splat(16.0));
    icons::paint(ui, "chevron-right", chevron, style::text_3());
    let tx = box_.right() + 14.0;
    let tw = chevron.left() - 14.0 - tx;
    let t = style::galley(ui, title, 14.0, Weight::W600, style::text_1(), 0.2);
    let d = crate::ui::kit::wrapped_weight(ui, desc, 11.5, Weight::W400, style::text_3(), tw, 1.45);
    let desc_h = 2.0 * 11.5 * 1.45;
    let total = t.size().y + 3.0 + desc_h;
    let top = rect.center().y - total / 2.0;
    let clip = Rect::from_min_size(Pos2::new(tx, top), Vec2::new(tw, total));
    let p = ui.painter_at(clip);
    p.galley(Pos2::new(tx, top), t.clone(), style::text_1());
    // Two lines at most: whatever the third would show is cut by the clip.
    ui.painter_at(Rect::from_min_size(
        Pos2::new(tx, top + t.size().y + 3.0),
        Vec2::new(tw, desc_h),
    ))
    .galley(Pos2::new(tx, top + t.size().y + 3.0), d, style::text_3());
    resp.clicked()
}

/// How wide `segmented` draws these items.
pub fn segmented_width(ui: &egui::Ui, items: &[(String, String, bool)]) -> f32 {
    let widths: f32 = items
        .iter()
        .map(|(label, count, _)| {
            let l = style::galley(ui, label, 11.0, Weight::W600, style::text_3(), 0.3)
                .size()
                .x;
            let c = style::galley(ui, count, 11.0, Weight::W500, style::text_3(), 0.0);
            20.0 + l + if c.text().is_empty() { 0.0 } else { 6.0 + c.size().x }
        })
        .sum();
    2.0 + 6.0 + widths + 3.0 * (items.len() as f32 - 1.0)
}

/// A `.params-filter` segmented control: returns the index clicked.
pub fn segmented(
    ui: &mut egui::Ui,
    at: Pos2,
    items: &[(String, String, bool)],
    selected: usize,
    id: &str,
) -> (Rect, Option<usize>) {
    let painter = ui.painter().clone();
    let gals: Vec<_> = items
        .iter()
        .map(|(label, count, _)| {
            let l = style::galley(ui, label, 11.0, Weight::W600, style::text_3(), 0.3);
            let c = style::galley(ui, count, 11.0, Weight::W500, style::text_3(), 0.0);
            (l, c)
        })
        .collect();
    let widths: Vec<f32> = gals
        .iter()
        .map(|(l, c)| 20.0 + l.size().x + if c.text().is_empty() { 0.0 } else { 6.0 + c.size().x })
        .collect();
    let h = 3.0 * 2.0 + 2.0 + 5.0 * 2.0 + 11.0 * 1.21;
    let w = 2.0 + 6.0 + widths.iter().sum::<f32>() + 3.0 * (items.len() as f32 - 1.0);
    let rect = Rect::from_min_size(at, Vec2::new(w, h));
    {
        let r = rect;
        painter.rect_filled(r, style::RADIUS_BTN, style::glass(0.82));
        painter.rect_stroke(
            r,
            style::RADIUS_BTN,
            Stroke::new(1.0, style::tint(0.09)),
            egui::StrokeKind::Inside,
        );
    }
    let mut x = rect.left() + 4.0;
    let mut clicked = None;
    for (i, ((label, _count, enabled), (lg, cg))) in items.iter().zip(gals).enumerate() {
        let r = Rect::from_min_size(Pos2::new(x, rect.top() + 4.0), Vec2::new(widths[i], h - 8.0));
        let sense = if *enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let resp = ui.interact(r, ui.scope_id().with((id, i)), sense);
        let sel = i == selected;
        if sel {
            painter.rect_filled(r, style::RADIUS_BTN - 2, style::surface_1());
        }
        let color = if sel || (resp.hovered() && *enabled) {
            style::text_1()
        } else {
            style::text_3()
        };
        let alpha = if *enabled { 1.0 } else { 0.45 };
        let mut lx = r.left() + 10.0;
        let l2 = style::galley(ui, label, 11.0, Weight::W600, color.gamma_multiply(alpha), 0.3);
        painter.galley(Pos2::new(lx, r.center().y - l2.size().y / 2.0), l2, color);
        lx += lg.size().x + 6.0;
        if !cg.text().is_empty() {
            let cc = if sel { style::text_2() } else { style::text_3() };
            let c2 = style::galley(ui, cg.text(), 11.0, Weight::W500, cc.gamma_multiply(alpha), 0.0);
            painter.galley(Pos2::new(lx, r.center().y - c2.size().y / 2.0), c2, cc);
        }
        if resp.clicked() {
            clicked = Some(i);
        }
        x += widths[i] + 3.0;
    }
    (rect, clicked)
}

/// A line of text in a colour, at a size and weight; returns its height.
pub fn text(ui: &egui::Ui, at: Pos2, s: &str, size: f32, w: Weight, color: Color32) -> f32 {
    let g = style::galley(ui, s, size, w, color, 0.0);
    let h = g.size().y;
    ui.painter().galley(at, g, color);
    h
}
