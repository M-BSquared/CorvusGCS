//! How the board looks: the NOTIFICATIONS block with its pill in the top
//! bar (main.css `.tb-warn-pill`), the board itself (flight.css
//! `.warnings-popover`, `.wp-*`) and the toast stack (components.css
//! `.ui-toast`), on the notification anatomy of themes.css: a 9 px gutter,
//! a 20 px icon threaded by a 3 px severity mark, 14 px to the text.

use crate::ui::tip::HoverTr;
use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Stroke, Vec2};

use crate::notify::{Board, Notification, Summary};
use crate::ui::icons;
use crate::ui::kit;
use crate::ui::listrow::{self, Mark};
use crate::ui::style::{self, Weight};

const GUTTER: f32 = 9.0;
const ICON: f32 = 20.0;
const ICON_SVG: f32 = 16.0;
const GAP: f32 = 14.0;
const PAD_Y: f32 = 10.0;
const PAD_RIGHT: f32 = 13.0;
const MARK: f32 = 3.0;
const MARK_GAP: f32 = 4.0;
const MARK_FILL: f32 = 0.38;
const DISMISS: f32 = 24.0;
const WIDTH: f32 = 320.0;

fn level_color(level: &str) -> Color32 {
    match level {
        "critical" => style::critical(),
        "warning" => style::warning(),
        _ => style::nav(),
    }
}

fn level_icon(level: &str) -> &'static str {
    match level {
        "critical" => "octagon-alert",
        "warning" => "triangle-alert",
        _ => "info",
    }
}

/// The pill's colour for the summary level (`.tb-warn-pill[data-level]`).
pub fn pill_color(level: &str) -> Color32 {
    match level {
        "critical" => style::critical(),
        "warning" => style::warning(),
        "info" => style::nav(),
        "read" => style::text_3(),
        _ => style::healthy(),
    }
}

/// The NOTIFICATIONS block's content: the pill, centred under the label.
/// Returns the block's width; `x` is its left edge.
/// The block's width when nothing squeezes it.
pub fn block_width(ui: &egui::Ui, summary: &Summary) -> f32 {
    let label = style::galley(ui, "NOTIFICATIONS", 9.5, Weight::W600, style::text_3(), 0.9);
    let count = style::galley(
        ui,
        &summary.count.to_string(),
        12.0,
        Weight::W700,
        style::text_inverted(),
        0.0,
    );
    let two = style::galley(ui, "00", 12.0, Weight::W700, style::text_inverted(), 0.0)
        .size()
        .x;
    let pill_w = 7.0 + count.size().x.max(two) + 5.0 + 12.0 + 7.0;
    label.size().x.max(pill_w) + 24.0
}

pub fn block(ui: &mut egui::Ui, bar: Rect, x: f32, summary: &Summary, open: bool, force_w: Option<f32>) -> (f32, bool) {
    let label = style::galley(ui, "NOTIFICATIONS", 9.5, Weight::W600, style::text_3(), 0.9);
    let count = style::galley(
        ui,
        &summary.count.to_string(),
        12.0,
        Weight::W700,
        style::text_inverted(),
        0.0,
    );
    // .tb-warn-count { min-width: 2ch }
    let two = style::galley(ui, "00", 12.0, Weight::W700, style::text_inverted(), 0.0)
        .size()
        .x;
    let pill_w = 7.0 + count.size().x.max(two) + 5.0 + 12.0 + 7.0;
    let w = force_w.unwrap_or(label.size().x.max(pill_w) + 24.0);
    let r = Rect::from_min_max(Pos2::new(x, bar.top()), Pos2::new(x + w, bar.bottom() - 1.0));
    let painter = ui.painter_at(r);
    let resp = ui.interact(r, ui.scope_id().with("tbwarnings"), egui::Sense::click());
    if resp.hovered() || open {
        listrow::paint(&painter, r, 0, Mark::Hover);
    }
    let value_h = 15.0 * 1.1;
    let top = r.center().y - (label.size().y + 2.0 + value_h) / 2.0;
    painter.galley(
        Pos2::new(r.center().x - label.size().x / 2.0, top),
        label.clone(),
        style::text_3(),
    );
    let pill = Rect::from_center_size(
        Pos2::new(r.center().x, top + label.size().y + 2.0 + value_h / 2.0),
        Vec2::new(pill_w, 18.0),
    );
    let color = pill_color(summary.level);
    if matches!(summary.level, "warning" | "critical") {
        // box-shadow: 0 0 7px of the level; critical pulses to 13px.
        let mut blur = 7.0;
        if summary.level == "critical" {
            let t = ui.input(|i| i.time) as f32;
            blur = 7.0 + 6.0 * (0.5 - 0.5 * (t / 1.8 * std::f32::consts::TAU).cos());
            ui.ctx().request_repaint();
        }
        let glow = egui::epaint::Shadow {
            offset: [0, 0],
            blur: blur as u8,
            spread: 0,
            color: color.gamma_multiply(0.6),
        };
        painter.add(glow.as_shape(pill, CornerRadius::same(9)));
    }
    painter.rect_filled(pill, 9, color);
    let cx = pill.left() + 7.0 + count.size().x.max(two) / 2.0;
    painter.galley(
        Pos2::new(cx - count.size().x / 2.0, pill.center().y - count.size().y / 2.0),
        count,
        style::text_inverted(),
    );
    let icon = Rect::from_min_size(
        Pos2::new(pill.right() - 7.0 - 12.0, pill.center().y - 6.0),
        Vec2::splat(12.0),
    );
    icons::paint(ui, "message-square", icon, style::text_inverted());
    (w, resp.clicked())
}

/// The two halves of the severity mark, threaded through the icon.
fn marks(painter: &egui::Painter, row: Rect, icon_center: Pos2, color: Color32) {
    let half = row.height() / 2.0;
    let len = (half - ICON / 2.0 - MARK_GAP) * MARK_FILL;
    if len <= 0.0 {
        return;
    }
    let x = icon_center.x - MARK / 2.0;
    let up = Rect::from_min_size(
        Pos2::new(x, icon_center.y - ICON / 2.0 - MARK_GAP - len),
        Vec2::new(MARK, len),
    );
    let down = Rect::from_min_size(
        Pos2::new(x, icon_center.y + ICON / 2.0 + MARK_GAP),
        Vec2::new(MARK, len),
    );
    painter.rect_filled(up, 255, color);
    painter.rect_filled(down, 255, color);
}

/// One row as the board and a toast share it: mark, icon, two lines of text
/// and the dismiss button. Returns (height, dismissed, text clicked).
#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    at: Pos2,
    width: f32,
    level: &str,
    title: (&str, f32, Weight, Color32),
    second: (&str, f32, Color32),
    dim: bool,
    marked: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
    measure_only: bool,
) -> (f32, bool, bool) {
    let text_w = width - GUTTER - ICON - GAP - DISMISS + 4.0 - PAD_RIGHT;
    let t = kit::wrapped_weight(ui, title.0, title.1, title.2, title.3, text_w, 1.3);
    let s = (!second.0.is_empty())
        .then(|| kit::wrapped_weight(ui, second.0, second.1, Weight::W400, second.2, text_w, 1.35));
    let text_h = t.size().y + s.as_ref().map_or(0.0, |g| 1.0 + g.size().y);
    let h = PAD_Y * 2.0 + text_h.max(ICON).max(DISMISS - 4.0);
    if measure_only {
        return (h, false, false);
    }
    let r = Rect::from_min_size(at, Vec2::new(width, h));
    let hover = ui.rect_contains_pointer(r);
    let alpha = if dim && !hover { 0.55 } else { 1.0 };
    let icon_c = Pos2::new(r.left() + GUTTER + ICON / 2.0, r.center().y);
    if marked {
        marks(painter, r, icon_c, level_color(level).gamma_multiply(alpha));
    }
    let ic = if dim && !marked && level.is_empty() {
        style::text_3()
    } else {
        level_color(level)
    };
    icons::paint(
        ui,
        level_icon(level),
        Rect::from_center_size(icon_c, Vec2::splat(ICON_SVG)),
        ic.gamma_multiply(alpha),
    );
    let tx = r.left() + GUTTER + ICON + GAP;
    let ty = r.top() + PAD_Y + ((h - PAD_Y * 2.0) - text_h).max(0.0) / 2.0;
    painter.galley(Pos2::new(tx, ty), t.clone(), title.3.gamma_multiply(alpha));
    let text_rect = Rect::from_min_size(Pos2::new(tx, ty), Vec2::new(text_w, text_h));
    if let Some(s) = s {
        painter.galley(Pos2::new(tx, ty + t.size().y + 1.0), s, second.2.gamma_multiply(alpha));
    }
    // margin: -2px -4px -2px auto
    let d = Rect::from_min_size(
        Pos2::new(r.right() - PAD_RIGHT + 4.0 - DISMISS, r.top() + PAD_Y - 2.0),
        Vec2::splat(DISMISS),
    );
    let dr = ui
        .interact(d, ui.scope_id().with(("dismiss", &id)), egui::Sense::click())
        .on_hover_tr("Dismiss");
    if dr.hovered() {
        listrow::paint(painter, d, style::RADIUS_BTN, Mark::Hover);
    }
    icons::paint(
        ui,
        "x",
        Rect::from_center_size(d.center(), Vec2::splat(13.0)),
        if dr.hovered() { style::text_1() } else { style::text_3() },
    );
    let tr = ui.interact(text_rect, ui.scope_id().with(("rowtext", &id)), egui::Sense::click());
    (h, dr.clicked(), tr.clicked())
}

/// The board, under the bar at the right, when open; returns its bottom
/// edge so the toasts can stack below it.
pub fn board(ctx: &egui::Context, screen: Rect, top: f32, board: &mut Board, now: f64) -> Option<f32> {
    if !board.open {
        return None;
    }
    let board_marks = board.marks;
    let summary = board.summary(now);
    let unread: std::collections::HashSet<&String> = summary.unread.iter().collect();
    let pos = Pos2::new(screen.right() - 12.0 - WIDTH, top + 10.0);
    let mut bottom = None;
    let mut dismiss: Option<String> = None;
    let mut close = false;
    let mut clear = false;
    let rise = kit::enter(ctx, "warningsboard", 0.18);
    egui::Area::new(egui::Id::NULL.with("warningsboard-area"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            kit::enter_layer(ui.ctx(), ui.layer_id(), rise);
            ui.set_opacity(rise);
            let painter = ui.painter().clone();
            let total = summary.items.len();
            let title = format!("{total} Notification{}", if total == 1 { "" } else { "s" });
            let title = if summary.unread.is_empty() {
                title
            } else {
                format!("{title} \u{00b7} {} new", summary.unread.len())
            };
            let title = crate::i18n::tr_joined(&title);
            let tg = style::galley(ui, &title, 12.0, Weight::W600, style::text_1(), 0.5);
            let header_h = 11.0 * 2.0 + tg.size().y.max(28.0) + 1.0;
            let rows: Vec<(f32, &Notification)> = summary
                .items
                .iter()
                .map(|n| {
                    let (h, _, _) = row(
                        ui,
                        &painter,
                        Pos2::ZERO,
                        WIDTH,
                        n.level(),
                        (&n.msg, 12.0, Weight::W400, style::text_1()),
                        (&n.meta, 10.0, style::text_3()),
                        false,
                        false,
                        n.key(),
                        true,
                    );
                    (h + 1.0, n)
                })
                .collect();
            let list_h: f32 = if rows.is_empty() {
                22.0 * 2.0 + 15.0
            } else {
                rows.iter().map(|(h, _)| *h).sum::<f32>() - 1.0
            };
            let list_shown = list_h.min(280.0);
            let rect = Rect::from_min_size(pos, Vec2::new(WIDTH, header_h + list_shown));
            style::glass_panel(&painter, "warningsboard", rect, style::RADIUS, 0.6);
            ui.allocate_rect(rect, egui::Sense::click_and_drag());
            painter.galley(
                Pos2::new(rect.left() + 13.0, rect.top() + header_h / 2.0 - tg.size().y / 2.0),
                tg,
                style::text_1(),
            );
            painter.hline(
                rect.x_range(),
                rect.top() + header_h - 0.5,
                Stroke::new(1.0, style::border_soft()),
            );
            let close_r = Rect::from_min_size(
                Pos2::new(rect.right() - 13.0 - 28.0, rect.top() + header_h / 2.0 - 14.0),
                Vec2::splat(28.0),
            );
            let clear_r = close_r.translate(Vec2::new(-32.0, 0.0));
            if kit::icon_btn(ui, close_r, "x", "Close", false, "wpclose") {
                close = true;
            }
            if total == 0 {
                icons::paint(
                    ui,
                    "trash-2",
                    Rect::from_center_size(clear_r.center(), Vec2::splat(15.0)),
                    style::text_3().gamma_multiply(0.4),
                );
            } else if kit::icon_btn(ui, clear_r, "trash-2", "Clear all", false, "wpclear") {
                clear = true;
            }
            let list = Rect::from_min_size(
                Pos2::new(rect.left(), rect.top() + header_h),
                Vec2::new(WIDTH, list_shown),
            );
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list));
            child.set_clip_rect(list);
            egui::ScrollArea::vertical()
                .id_salt("wplist")
                .max_height(280.0)
                .auto_shrink([false, false])
                .show(&mut child, |ui| {
                    let top = ui.max_rect().top();
                    let painter = ui.painter().clone();
                    if rows.is_empty() {
                        let g = style::galley(ui, "No notifications.", 12.0, Weight::W400, style::text_3(), 0.0);
                        painter.galley(
                            Pos2::new(list.center().x - g.size().x / 2.0, top + 22.0),
                            g,
                            style::text_3(),
                        );
                        ui.allocate_space(Vec2::new(WIDTH, list_h));
                        return;
                    }
                    let mut y = top;
                    let last = rows.len() - 1;
                    for (i, (h, n)) in rows.iter().enumerate() {
                        let key = n.key();
                        let is_read = !unread.contains(&key);
                        let meta = if n.resolved {
                            [n.meta.as_str(), "resolved"]
                                .iter()
                                .filter(|s| !s.is_empty())
                                .copied()
                                .collect::<Vec<_>>()
                                .join(" \u{00b7} ")
                        } else {
                            n.meta.clone()
                        };
                        let msg = if n.msg.is_empty() {
                            "Unknown notification"
                        } else {
                            n.msg.as_str()
                        };
                        let msg_c = if n.resolved { style::text_2() } else { style::text_1() };
                        let level = if n.resolved { "" } else { n.level() };
                        let (_, dismissed, _) = row(
                            ui,
                            &painter,
                            Pos2::new(list.left(), y),
                            WIDTH,
                            level,
                            (msg, 12.0, Weight::W400, msg_c),
                            (&meta, 10.0, style::text_3()),
                            is_read || n.resolved,
                            board_marks && !is_read && !n.resolved,
                            key.clone(),
                            false,
                        );
                        if dismissed {
                            dismiss = Some(key);
                        }
                        y += h;
                        if i != last {
                            painter.hline(list.x_range(), y - 0.5, Stroke::new(1.0, style::border_soft()));
                        }
                    }
                    ui.allocate_space(Vec2::new(WIDTH, y - top));
                });
            bottom = Some(rect.bottom());
        });
    if let Some(k) = dismiss {
        board.dismiss(&k);
    }
    if clear {
        board.clear_all();
    }
    if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        board.set_open(false, now);
    }
    bottom
}

/// The toast stack in the same corner, under the board while it is open.
pub fn toasts(ctx: &egui::Context, screen: Rect, top: f32, board: &mut Board) {
    if board.toasts.is_empty() {
        return;
    }
    let toasts = board.toasts.clone();
    let marks = board.marks;
    let mut close = Vec::new();
    let mut open_board = false;
    let pos = Pos2::new(screen.right() - 12.0 - WIDTH, top);
    let rise = kit::enter(ctx, "toaststack", 0.18);
    egui::Area::new(egui::Id::NULL.with("toaststack-area"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            kit::enter_layer(ui.ctx(), ui.layer_id(), rise);
            ui.set_opacity(rise);
            let painter = ui.painter().clone();
            let mut y = pos.y;
            for t in &toasts {
                let (h, _, _) = row(
                    ui,
                    &painter,
                    Pos2::ZERO,
                    WIDTH,
                    t.level,
                    (&t.title, 12.0, Weight::W600, style::text_1()),
                    (&t.text, 12.0, style::text_2()),
                    false,
                    marks,
                    ("toast", t.id),
                    true,
                );
                let r = Rect::from_min_size(Pos2::new(pos.x, y), Vec2::new(WIDTH, h));
                style::glass_panel(&painter, ("toast", t.id), r, style::RADIUS, 0.6);
                ui.allocate_rect(r, egui::Sense::hover());
                let (_, dismissed, clicked) = row(
                    ui,
                    &painter,
                    r.min,
                    WIDTH,
                    t.level,
                    (&t.title, 12.0, Weight::W600, style::text_1()),
                    (&t.text, 12.0, style::text_2()),
                    false,
                    marks,
                    ("toast", t.id),
                    false,
                );
                if dismissed {
                    close.push(t.id);
                }
                if clicked {
                    close.push(t.id);
                    open_board = true;
                }
                y += h + 8.0;
            }
        });
    for id in close {
        board.close_toast(id);
    }
    if open_board {
        board.open = true;
    }
}
