//! Page furniture of the web interface (components.css `.page-header`,
//! `.page-section-title`, `.page-card`, `.tile`, `.setup-back`), which every
//! page that is not the map is built from.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::ui::card::{self, Look};
use crate::ui::icons;
use crate::ui::kit::{Button, Variant};
use crate::ui::segmented::{self, Opts, Segment};
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
        card::paint(&painter, r, Look::custom(fill, stroke, style::RADIUS));
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
    let t = crate::ui::text::clamped(ui, title, 14.0, Weight::W600, style::text_1(), 0.2, tw, 1, None);
    let d = crate::ui::text::clamped(ui, desc, 11.5, Weight::W400, style::text_3(), 0.0, tw, 2, Some(1.45));
    let desc_h = 2.0 * 11.5 * 1.45;
    let total = t.size().y + 3.0 + desc_h;
    let top = rect.center().y - total / 2.0;
    let clip = Rect::from_min_size(Pos2::new(tx, top), Vec2::new(tw, total));
    let p = ui.painter_at(clip);
    p.galley(Pos2::new(tx, top), t.clone(), style::text_1());
    // Two lines at most, the second ending in an ellipsis as `line-clamp` does.
    ui.painter_at(Rect::from_min_size(
        Pos2::new(tx, top + t.size().y + 3.0),
        Vec2::new(tw, desc_h),
    ))
    .galley(Pos2::new(tx, top + t.size().y + 3.0), d, style::text_3());
    resp.clicked()
}

fn segments(items: &[(String, String, bool)]) -> Vec<Segment> {
    items
        .iter()
        .map(|(label, count, enabled)| Segment::new(label.as_str()).count(count.as_str()).enabled(*enabled))
        .collect()
}

/// How wide `segmented` draws these items.
pub fn segmented_width(ui: &egui::Ui, items: &[(String, String, bool)]) -> f32 {
    segmented::width(ui, &segments(items), &Opts::FILTER)
}

/// A `.params-filter` segmented control: returns the index clicked.
pub fn segmented(
    ui: &mut egui::Ui,
    at: Pos2,
    items: &[(String, String, bool)],
    selected: usize,
    id: &str,
) -> (Rect, Option<usize>) {
    segmented::show(ui, at, &segments(items), selected, id, Opts::FILTER)
}

/// A line of text in a colour, at a size and weight; returns its height.
pub fn text(ui: &egui::Ui, at: Pos2, s: &str, size: f32, w: Weight, color: Color32) -> f32 {
    let g = style::galley(ui, s, size, w, color, 0.0);
    let h = g.size().y;
    ui.painter().galley(at, g, color);
    h
}

static LINK_UP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Whether the vehicle link is up as far as telemetry knows; unknown is up
/// (`linkUp` of setup-shared.js). The window sets it every frame.
pub fn set_link_up(up: bool) {
    LINK_UP.store(up, std::sync::atomic::Ordering::Relaxed);
}

pub fn link_up() -> bool {
    LINK_UP.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether a page status is shown as the "Not connected" chip instead of its
/// text (`setActionsStatus`): a read that failed with no link is a state that
/// ends when the vehicle appears, not something wrong with what was read. A
/// page with a live link chip of its own gets no second one.
pub fn shows_offline_chip(class: &str, has_link_chip: bool, link_up: bool) -> bool {
    class == "err" && !link_up && !has_link_chip
}

/// The colour of a status line (`.params-actions-status`, `.ok`, `.err`,
/// `.pending`).
pub fn status_color(cls: &str) -> Color32 {
    match cls {
        "ok" => style::healthy(),
        "err" => style::critical(),
        _ => style::nav(),
    }
}

/// `.setup-head` with its `.params-actions`: the title and subtitle, and at
/// the right the status line then the buttons. The buttons wrap under the
/// title when it would be left under 280 px. Returns the y under the head
/// (with its 26 px) and the index of the button pressed.
#[allow(clippy::too_many_arguments)]
pub fn head_actions(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    title: &str,
    subtitle: &str,
    status: &(&'static str, String),
    buttons: Vec<Button<'_>>,
    id: &str,
) -> (f32, Option<usize>) {
    head_actions_with(ui, x, y, w, title, subtitle, status, buttons, Vec::new(), 0.0, id)
}

/// `head_actions` with chips at the left of the actions (the readiness strip
/// of Calibration and PID Tuning, `.setup-head .params-actions > .calib-ready`)
/// and the actions standing `drop` lower than the title's bottom.
#[allow(clippy::too_many_arguments)]
pub fn head_actions_with(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    title: &str,
    subtitle: &str,
    status: &(&'static str, String),
    buttons: Vec<Button<'_>>,
    lead: Vec<crate::ui::chip::Chip>,
    drop: f32,
    id: &str,
) -> (f32, Option<usize>) {
    let sizes: Vec<(f32, f32)> = buttons.iter().map(|b| (b.width(ui), b.height())).collect();
    let chip_sizes: Vec<Vec2> = lead.iter().map(|c| c.measure(ui)).collect();
    let rh = sizes.iter().map(|s| s.1).fold(0.0, f32::max);
    let color = status_color(status.0);
    let sg = style::galley(ui, &status.1, 11.5, Weight::W400, color, 0.0);
    let offline = shows_offline_chip(status.0, !lead.is_empty(), link_up()).then(crate::ui::chip::Chip::offline);
    let offline_size = offline.as_ref().map(|c| c.measure(ui));
    let status_w = offline_size.map_or(sg.size().x.min(420.0), |s| s.x);
    let gap = if status_w > 0.0 { 10.0 } else { 0.0 };
    let buttons_w: f32 = sizes.iter().map(|s| s.0).sum::<f32>() + 10.0 * sizes.len().saturating_sub(1) as f32;
    let chips_w: f32 = if chip_sizes.is_empty() {
        0.0
    } else {
        chip_sizes.iter().map(|s| s.x).sum::<f32>() + 8.0 * (chip_sizes.len() - 1) as f32 + 10.0
    };
    let actions_w = chips_w + status_w + gap + buttons_w;
    let wrap = w - actions_w - 24.0 < 280.0;
    let head_w = if wrap { w } else { (w - actions_w - 24.0).min(w) };
    let head_bottom = header_wrapped(ui, x, y, head_w, title, subtitle);
    let (ay, bottom) = if wrap {
        (head_bottom + 10.0, head_bottom + 12.0 + rh)
    } else {
        (head_bottom - rh + drop, head_bottom + drop)
    };
    let (mut cx, status_x, mut bx) = if wrap {
        (x, x + chips_w + 0.0, x + chips_w + status_w + gap)
    } else {
        let first = x + w - buttons_w;
        let status_x = first - gap - offline_size.map_or(sg.size().x, |s| s.x);
        (status_x - chips_w, status_x, first)
    };
    for (c, s) in lead.iter().zip(&chip_sizes) {
        c.paint(ui, Pos2::new(cx, ay + (rh - s.y) / 2.0));
        cx += s.x + 8.0;
    }
    let mut pressed = None;
    for (i, b) in buttons.into_iter().enumerate() {
        let (bw, bh) = sizes[i];
        let rect = Rect::from_min_size(Pos2::new(bx, ay + (rh - bh) / 2.0), Vec2::new(bw, bh));
        if b.show(ui, rect, (id, i)) {
            pressed = Some(i);
        }
        bx += bw + 10.0;
    }
    if let (Some(chip), Some(size)) = (offline, offline_size) {
        chip.paint_tip(
            ui,
            Pos2::new(status_x, ay + (rh - size.y) / 2.0),
            (id, "offline"),
            &status.1,
        );
    } else {
        ui.painter()
            .galley(Pos2::new(status_x, ay + rh / 2.0 - sg.size().y / 2.0), sg, color);
    }
    (bottom + 26.0, pressed)
}

/// The page's own ground, under everything the page draws.
pub fn background(ui: &egui::Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 0, style::bg());
}

/// `.params-banner`: the amber notice that gates a card; returns its bottom.
pub fn warning_banner(ui: &egui::Ui, x: f32, y: f32, w: f32, text: &str) -> f32 {
    crate::ui::message::boxed(ui, x, y, w, text, crate::ui::message::Tone::Warn)
}

/// `.tune-tabs`: a row of text tabs on a rule, the active one in the accent
/// with an underline. Returns the y under it (with its 14 px margin) and the
/// tab pressed.
pub fn tabs(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    labels: &[&str],
    active: usize,
    id: &str,
) -> (f32, Option<usize>) {
    let mut pressed = None;
    let mut tx = x;
    let h = style::galley(ui, "Ag", 12.0, Weight::W600, style::text_3(), 0.2)
        .size()
        .y
        + 20.0;
    // the rule runs under the tabs; the active tab's underline covers it
    ui.painter().hline(
        egui::Rangef::new(x, x + w),
        y + h - 0.5,
        Stroke::new(1.0, style::border()),
    );
    for (i, label) in labels.iter().enumerate() {
        let on = i == active;
        let g = style::galley(ui, label, 12.0, Weight::W600, style::text_3(), 0.2);
        let (tw, th) = (g.size().x + 24.0, g.size().y + 18.0 + 2.0);
        let rect = Rect::from_min_size(Pos2::new(tx, y), Vec2::new(tw, th));
        let resp = ui.interact(rect, ui.scope_id().with((id, i)), egui::Sense::click());
        let color = if on {
            style::accent()
        } else if resp.hovered() {
            style::text_1()
        } else {
            style::text_3()
        };
        if resp.hovered() && !on {
            ui.painter()
                .rect_filled(rect.with_max_y(rect.bottom() - 2.0), 0, style::surface_hover());
        }
        let g = style::galley(ui, label, 12.0, Weight::W600, color, 0.2);
        ui.painter()
            .galley(Pos2::new(rect.left() + 12.0, rect.top() + 9.0), g, color);
        if on {
            ui.painter().rect_filled(
                Rect::from_min_size(Pos2::new(rect.left(), rect.bottom() - 2.0), Vec2::new(tw, 2.0)),
                0,
                style::accent(),
            );
        }
        if resp.clicked() {
            pressed = Some(i);
        }
        tx += tw + 2.0;
    }
    (y + h + 14.0, pressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_read_with_no_link_is_the_offline_chip_unless_the_page_has_its_own() {
        assert!(shows_offline_chip("err", false, false));
        assert!(
            !shows_offline_chip("err", false, true),
            "the link is up: the error is the news"
        );
        assert!(!shows_offline_chip("ok", false, false));
        assert!(
            !shows_offline_chip("err", true, false),
            "a page with a link chip of its own"
        );
    }
}
