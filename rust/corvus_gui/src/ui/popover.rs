//! The hint icon and the sheet it opens, and the cards the top bar opens
//! (`Corvus.ui.popover` and `infoHint` of src/js/ui.js, `.ui-info` and
//! `.ui-popover` of components.css, `.tb-pop*` of main.css).
//!
//! Hovering an anchor opens its sheet after the pointer has rested on it for
//! 120 ms, and a hover-opened sheet goes 80 ms after the pointer leaves it,
//! without taking the pointer. A press pins the sheet: it stays until Escape,
//! a press elsewhere, a scroll, another sheet opening, or a second press on
//! its anchor. One sheet is open at a time.

use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Color32, Galley, Id, Pos2, Rect, Vec2};

use crate::ui::icons;
use crate::ui::kit;
use crate::ui::style::{self, Weight};

/// `openDelay`: how long the pointer must rest before a hover opens a sheet.
const OPEN_DELAY: f64 = 0.12;
/// `delay`: how long a hover-opened sheet lingers after the pointer leaves.
const GRACE: f64 = 0.08;

/// One row of a card: label, value and the value's tone (`""`, `healthy`,
/// `warning`, `critical` or `muted`).
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub key: String,
    pub value: String,
    pub tone: &'static str,
}

/// What a sheet says: a title, paragraphs (`\n` splits them), a two column
/// list of rows and small print under it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Content {
    pub title: String,
    pub text: String,
    pub rows: Vec<Row>,
    pub notes: Vec<String>,
}

impl Content {
    /// A hint: a title and a sentence.
    pub fn hint(title: &str, text: &str) -> Content {
        Content {
            title: title.into(),
            text: text.into(),
            ..Default::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.title.is_empty() && self.text.is_empty() && self.rows.is_empty() && self.notes.is_empty()
    }
}

/// The sheet's width rules: `.ui-popover`, `.tb-pop-narrow` (a card of the
/// bar with a 150 px floor) and `.tb-pop-tight` (no floor).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Look {
    Hint,
    Narrow,
    Tight,
}

impl Look {
    fn max_w(self, screen_w: f32) -> f32 {
        let cap: f32 = match self {
            Look::Hint => 260.0,
            _ => 300.0,
        };
        cap.min(screen_w - 16.0)
    }

    fn min_w(self) -> f32 {
        match self {
            Look::Narrow => 150.0,
            _ => 0.0,
        }
    }

    fn column_gap(self) -> f32 {
        match self {
            Look::Narrow => 20.0,
            Look::Tight => 10.0,
            _ => 16.0,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct State {
    hover_since: Option<f64>,
    leave_since: Option<f64>,
    open: bool,
    pinned: bool,
    sheet: Option<Rect>,
}

fn tone_color(tone: &str) -> Color32 {
    match tone {
        "healthy" => style::healthy(),
        "warning" => style::warning(),
        "critical" => style::critical(),
        "muted" => style::text_2(),
        _ => style::text_1(),
    }
}

struct Laid {
    title: Option<Arc<Galley>>,
    paragraphs: Vec<Arc<Galley>>,
    rows: Vec<(Arc<Galley>, Arc<Galley>)>,
    notes: Vec<Arc<Galley>>,
    key_w: f32,
    val_w: f32,
    inner_w: f32,
    inner_h: f32,
}

const ROW_H: f32 = 11.5 * 1.4;

fn lay_out(ui: &egui::Ui, c: &Content, look: Look, screen_w: f32) -> Laid {
    let max_inner = look.max_w(screen_w) - 22.0;
    let title = (!c.title.is_empty()).then(|| style::galley(ui, &c.title, 11.0, Weight::W600, style::text_1(), 0.1));
    let paragraphs: Vec<Arc<Galley>> = c
        .text
        .split('\n')
        .filter(|l| !l.is_empty())
        .map(|l| kit::wrapped_weight(ui, l, 11.0, Weight::W400, style::text_2(), max_inner, 1.45))
        .collect();
    let rows: Vec<(Arc<Galley>, Arc<Galley>)> = c
        .rows
        .iter()
        .map(|r| {
            (
                style::galley(ui, &r.key, 11.5, Weight::W400, style::text_3(), 0.0),
                style::galley(ui, &r.value, 11.5, Weight::W600, tone_color(r.tone), 0.0),
            )
        })
        .collect();
    let notes: Vec<Arc<Galley>> = c
        .notes
        .iter()
        .map(|n| kit::wrapped_weight(ui, n, 10.5, Weight::W400, style::text_3(), max_inner, 1.4))
        .collect();
    let key_w = rows.iter().map(|r| r.0.size().x).fold(0.0, f32::max);
    let val_w = rows.iter().map(|r| r.1.size().x).fold(0.0, f32::max);
    let grid_w = if rows.is_empty() {
        0.0
    } else {
        key_w + look.column_gap() + val_w
    };
    let content_w = [
        title.as_ref().map_or(0.0, |g| g.size().x),
        paragraphs.iter().map(|g| g.size().x).fold(0.0, f32::max),
        grid_w,
        notes.iter().map(|g| g.size().x).fold(0.0, f32::max),
    ]
    .into_iter()
    .fold(0.0, f32::max);
    let inner_w = (content_w + 22.0).clamp(look.min_w(), look.max_w(screen_w)) - 22.0;
    // the flex column: 4 px between its children
    let mut parts: Vec<f32> = Vec::new();
    if let Some(t) = &title {
        parts.push(t.size().y);
    }
    parts.extend(paragraphs.iter().map(|g| g.size().y));
    if !rows.is_empty() || !notes.is_empty() {
        let mut body = 0.0;
        if !rows.is_empty() {
            body += rows.len() as f32 * ROW_H + (rows.len() as f32 - 1.0) * 3.0;
        }
        for (i, n) in notes.iter().enumerate() {
            if i == 0 {
                body += 6.0 + 1.0 + 6.0;
            } else {
                body += 6.0;
            }
            body += n.size().y;
        }
        parts.push(body);
    }
    let inner_h = parts.iter().sum::<f32>() + 4.0 * (parts.len() as f32 - 1.0).max(0.0);
    Laid {
        title,
        paragraphs,
        rows,
        notes,
        key_w,
        val_w,
        inner_w,
        inner_h,
    }
}

/// Where the sheet goes: centred under the anchor, above it when the space
/// below cannot hold it, clamped to the screen.
fn place(anchor: Rect, size: Vec2, screen: Rect) -> Rect {
    const GAP: f32 = 8.0;
    const EDGE: f32 = 8.0;
    let below = screen.bottom() - anchor.bottom() - GAP - EDGE;
    let above = anchor.top() - GAP - EDGE - screen.top();
    let up = below < size.y && above > below;
    let top = if up {
        (anchor.top() - GAP - size.y).max(screen.top() + EDGE)
    } else {
        (anchor.bottom() + GAP)
            .min(screen.bottom() - EDGE - size.y)
            .max(screen.top() + EDGE)
    };
    let centred = anchor.center().x - size.x / 2.0;
    let left = centred.min(screen.right() - size.x - EDGE).max(screen.left() + EDGE);
    Rect::from_min_size(Pos2::new(left.round(), top.round()), size)
}

/// Drives one anchor: opens, pins and closes its sheet and paints it. `resp`
/// is the anchor's response, made with `Sense::click()`.
pub fn drive(ctx: &egui::Context, id: Id, resp: &egui::Response, content: &Content, look: Look) {
    let now = ctx.input(|i| i.time);
    let state_id = id.with("popover-state");
    #[allow(deprecated)]
    let current_id = Id::new("popover-current");
    let mut st: State = ctx.data(|d| d.get_temp(state_id)).unwrap_or_default();
    let current: Option<Id> = ctx.data(|d| d.get_temp(current_id)).flatten();
    if st.open && current != Some(id) {
        st = State::default();
    }
    let anchor = resp.rect;
    let hovered = resp.hovered();
    let pointer = ctx.input(|i| i.pointer.interact_pos());
    let over_sheet = st.sheet.is_some_and(|r| pointer.is_some_and(|p| r.contains(p)));
    let empty = content.is_empty();
    let open_now = |st: &mut State, pin: bool| {
        st.open = true;
        st.pinned = pin;
        st.hover_since = None;
        st.leave_since = None;
        ctx.data_mut(|d| d.insert_temp(current_id, Some(id)));
    };
    if empty {
        st = State::default();
    } else if resp.clicked() {
        if st.open && st.pinned {
            st = State::default();
        } else {
            open_now(&mut st, true);
        }
    } else if hovered && !st.open {
        let since = *st.hover_since.get_or_insert(now);
        if now - since >= OPEN_DELAY {
            open_now(&mut st, false);
        } else {
            ctx.request_repaint_after(Duration::from_millis(
                ((OPEN_DELAY - (now - since)) * 1000.0) as u64 + 5,
            ));
        }
    } else if !hovered {
        st.hover_since = None;
    }
    if st.open {
        if hovered || (st.pinned && over_sheet) {
            st.leave_since = None;
        } else if !st.pinned {
            let since = *st.leave_since.get_or_insert(now);
            if now - since >= GRACE {
                st = State::default();
            } else {
                ctx.request_repaint_after(Duration::from_millis(((GRACE - (now - since)) * 1000.0) as u64 + 5));
            }
        }
    }
    if st.open {
        let pressed_away = ctx.input(|i| i.pointer.any_pressed()) && !over_sheet && !hovered;
        let scrolled = ctx.input(|i| i.smooth_scroll_delta != Vec2::ZERO) && !over_sheet;
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if pressed_away || scrolled || escape {
            st = State::default();
        }
    }
    if st.open {
        let screen = ctx.content_rect();
        let pinned = st.pinned;
        let mut sheet = None;
        egui::Area::new(Id::unique("popover-sheet"))
            .order(egui::Order::Tooltip)
            .fixed_pos(screen.min)
            .interactable(false)
            .show(ctx, |ui| {
                let laid = lay_out(ui, content, look, screen.width());
                let size = Vec2::new(laid.inner_w + 22.0, laid.inner_h + 18.0);
                let rect = place(anchor, size, screen);
                sheet = Some(rect);
                let painter = ui.painter().clone();
                style::glass_panel(&painter, ("popover", id), rect, style::RADIUS, 0.6);
                paint(ui, &laid, rect, look);
            });
        st.sheet = sheet;
        let _ = pinned;
        ctx.request_repaint();
    } else {
        st.sheet = None;
        if current == Some(id) {
            ctx.data_mut(|d| d.insert_temp::<Option<Id>>(current_id, None));
        }
    }
    ctx.data_mut(|d| d.insert_temp(state_id, st));
}

fn paint(ui: &egui::Ui, l: &Laid, rect: Rect, look: Look) {
    let p = ui.painter();
    let x = rect.left() + 11.0;
    let mut y = rect.top() + 9.0;
    let mut first = true;
    let mut gap = |y: &mut f32| {
        if !first {
            *y += 4.0;
        }
        first = false;
    };
    if let Some(t) = &l.title {
        gap(&mut y);
        p.galley(Pos2::new(x, y), t.clone(), style::text_1());
        y += t.size().y;
    }
    for g in &l.paragraphs {
        gap(&mut y);
        p.galley(Pos2::new(x, y), g.clone(), style::text_2());
        y += g.size().y;
    }
    if !l.rows.is_empty() || !l.notes.is_empty() {
        gap(&mut y);
        let right = rect.right() - 11.0;
        let _ = (l.key_w, l.val_w, look);
        for (i, (k, v)) in l.rows.iter().enumerate() {
            if i > 0 {
                y += 3.0;
            }
            let ky = y + (ROW_H - k.size().y) / 2.0;
            p.galley(Pos2::new(x, ky), k.clone(), style::text_3());
            p.galley(
                Pos2::new(right - v.size().x, y + (ROW_H - v.size().y) / 2.0),
                v.clone(),
                style::text_1(),
            );
            y += ROW_H;
        }
        for (i, n) in l.notes.iter().enumerate() {
            if i == 0 {
                y += 6.0;
                p.hline(
                    egui::Rangef::new(x, right),
                    y + 0.5,
                    egui::Stroke::new(1.0, style::border_soft()),
                );
                y += 1.0 + 6.0;
            } else {
                y += 6.0;
            }
            p.galley(Pos2::new(x, y), n.clone(), style::text_3());
            y += n.size().y;
        }
    }
}

/// Whether the anchor's sheet is open, for the anchor's own highlight.
pub fn is_open(ctx: &egui::Context, id: Id) -> bool {
    ctx.data(|d| d.get_temp::<State>(id.with("popover-state")))
        .is_some_and(|s| s.open)
}

/// `.ui-info`: the 16 px round hint button, centred on `center`, and the
/// sheet it opens. The icon carries the label's muted colour and takes the
/// accent, on a hover wash, while hovered or open.
pub fn info(ui: &mut egui::Ui, center: Pos2, title: &str, text: &str, id: impl std::hash::Hash + std::fmt::Debug) {
    let rect = Rect::from_center_size(center, Vec2::splat(16.0));
    let id = ui.scope_id().with(("info", id));
    let resp = ui.interact(rect, id, egui::Sense::click());
    let open = is_open(ui.ctx(), id);
    if resp.hovered() || open {
        ui.painter().circle_filled(rect.center(), 8.0, style::surface_hover());
    }
    icons::paint(
        ui,
        "info",
        Rect::from_center_size(rect.center(), Vec2::splat(13.0)),
        if resp.hovered() || open {
            style::accent()
        } else {
            style::text_3()
        },
    );
    drive(ui.ctx(), id, &resp, &Content::hint(title, text), Look::Hint);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sheet_opens_below_its_anchor_and_flips_above_when_the_room_runs_out() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let anchor = Rect::from_center_size(Pos2::new(400.0, 100.0), Vec2::splat(16.0));
        let r = place(anchor, Vec2::new(200.0, 80.0), screen);
        assert_eq!((r.left(), r.top()), (300.0, 116.0));
        let low = Rect::from_center_size(Pos2::new(400.0, 560.0), Vec2::splat(16.0));
        let r = place(low, Vec2::new(200.0, 80.0), screen);
        assert_eq!(r.bottom(), low.top() - 8.0);
    }

    #[test]
    fn a_sheet_at_the_edge_stays_on_the_screen() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let right = Rect::from_center_size(Pos2::new(795.0, 100.0), Vec2::splat(16.0));
        let r = place(right, Vec2::new(200.0, 60.0), screen);
        assert_eq!(r.right(), 792.0);
        let left = Rect::from_center_size(Pos2::new(3.0, 100.0), Vec2::splat(16.0));
        assert_eq!(place(left, Vec2::new(200.0, 60.0), screen).left(), 8.0);
    }

    #[test]
    fn the_width_rules_follow_the_stylesheet() {
        assert_eq!(Look::Hint.max_w(1200.0), 260.0);
        assert_eq!(Look::Hint.max_w(200.0), 184.0);
        assert_eq!(Look::Narrow.max_w(1200.0), 300.0);
        assert_eq!((Look::Narrow.min_w(), Look::Tight.min_w()), (150.0, 0.0));
        assert_eq!((Look::Narrow.column_gap(), Look::Tight.column_gap()), (20.0, 10.0));
    }

    #[test]
    fn a_content_with_nothing_to_say_is_empty() {
        assert!(Content::default().is_empty());
        assert!(!Content::hint("", "x").is_empty());
    }
}
