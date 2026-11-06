//! The dropdown list of the web interface (`.ui-menu` of `.option-item` rows,
//! components.css, `Corvus.ui.menu` and `menuItem` of ui.js) on a glass
//! surface. Every list in the app is made here: a select's choices, the
//! flight mode picker, the layers and camera lists of the map rail, the
//! context menu of a map point, the power menu of Schwalby, the elevation
//! models of the 3D panel and the answers of the place search. They open,
//! close, scroll, hover, mark the active row and take the keyboard alike.
//!
//! # Which look a kind of menu gets
//!
//! The web has one row (`.option-item`: 12 px at weight 500, 7 px above and
//! below, 8 px in, 8 px between the lead and the text) and one surface
//! (`.ui-menu` on `.glass`: 5 px padding, the radius, a 150 ms entry). A menu
//! differs from another in three things only, and each of them is decided in
//! the CSS, not here:
//!
//! * **The lead.** A chooser (a select, the flight modes, the layer list, the
//!   camera list, the elevation models) leads with the state dot and marks the
//!   row in use with the accent fill and the 2 px ring (`.option-item.active`
//!   inside a `.ui-menu`). An action list (the map's context menu, the power
//!   menu, Open all) leads with a 14 px icon that takes the accent on hover
//!   and marks nothing. So the flight mode picker is a chooser: the web builds
//!   it with `enhanceSelect`, and there is no plain variant of the row. The
//!   `PLAIN` and `PANEL` looks this module once had (weight 400, no dot, a
//!   fill without the ring) were copies that no CSS rule backs.
//! * **The gap between rows.** A list built with `.option-list` (a select's
//!   menu, the layers, the elevation models) has `gap: 2px`; rows laid
//!   straight into a `.ui-menu` (the cameras, the context menu, the power
//!   menu) touch.
//! * **The glass.** `.glass` is 0.6. Two surfaces say otherwise in their own
//!   rules and keep it: `.map-context-menu.glass` is 0.9, "at 0.6 the imagery
//!   behind shows through the text, and a disabled row over a green field was
//!   barely readable", and `.map-search-results` is 0.86, a panel of its own
//!   that is not a `.ui-menu` at all (4 px padding, `.map-search-row`
//!   rows of an icon, a name over its address and a distance, and the field
//!   above it owns the keyboard). The context menu also lays its rows on
//!   imagery, so its label is `--text-1`, its note 10.5 px at `--text-2`, and
//!   a disabled row `--text-3` throughout (`Ink::Map`); the search list is
//!   `Ink::Place`.
//!
//! Where a menu goes is the anchor: below an element (a select; flips above
//! when the screen has no room), beside an element (the rail's lists, which
//! open to the left of it and to the right when the left has no room), or at a
//! point (the context menu, 12 px from the pin and flipped on either axis).

use std::sync::Arc;

use eframe::egui::{self, Color32, Galley, Id, Pos2, Rect, Stroke, Vec2};

use crate::ui::kit;
use crate::ui::style::{self, Weight};

/// `body` line height, which every row's text block is made of.
const LINE: f32 = 1.21;
/// `.ui-menu` padding.
pub const PAD: f32 = 5.0;
/// `.option-list { gap }`.
pub const LIST_GAP: f32 = 2.0;
/// How far a menu keeps from the edge of the screen (`EDGE` of `ui.js`).
const EDGE: f32 = 8.0;
/// A list is never squeezed below this height (`Math.max(120, …)`).
const MIN_MAX_H: f32 = 120.0;
/// The seconds of `corvus-enter`.
const ENTER: f64 = 0.15;

/// The family of a menu: what its rows are made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// `.option-item` on `.glass`.
    Menu,
    /// `.option-item` on imagery (`.map-context-item`).
    Map,
    /// `.map-search-row`.
    Place,
}

impl Ink {
    fn weight(self) -> Weight {
        match self {
            Ink::Place => Weight::W400,
            _ => Weight::W500,
        }
    }

    /// The row's padding left and right, which is also the right edge.
    fn row_x(self) -> f32 {
        match self {
            Ink::Place => 9.0,
            _ => 8.0,
        }
    }

    /// The gap between the lead, the text and the trailing text.
    fn row_gap(self) -> f32 {
        self.row_x()
    }

    /// The menu's own padding.
    fn pad(self) -> f32 {
        match self {
            Ink::Place => 4.0,
            _ => PAD,
        }
    }

    fn note_px(self) -> f32 {
        match self {
            Ink::Menu => 9.5,
            _ => 10.5,
        }
    }

    /// The width of the lead: the dot, the icon, the search row's column.
    fn lead_w(self, has_icon: bool, dot: bool) -> f32 {
        match (self, has_icon, dot) {
            (Ink::Place, true, _) => 16.0,
            (_, true, _) => 14.0,
            (_, false, true) => 7.0,
            _ => 0.0,
        }
    }

    fn lead_h(self, has_icon: bool, dot: bool) -> f32 {
        match (self, has_icon, dot) {
            (Ink::Place, true, _) => 15.0,
            (_, true, _) => 14.0,
            (_, false, true) => 7.0,
            _ => 0.0,
        }
    }
}

/// How the rows look and how the menu behaves.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub ink: Ink,
    pub mono: bool,
    /// A 7 px state dot before the label of a row that has no icon.
    pub dots: bool,
    /// Pixels between rows: `.option-list` has 2, a bare `.ui-menu` none.
    pub gap: f32,
    /// The glass alpha.
    pub glass: f32,
    /// The list is at least this wide; it is as wide as its widest row
    /// otherwise.
    pub min_width: f32,
    /// ... and at most this wide (`max-width`).
    pub max_width: f32,
    /// As wide as the element it hangs from, no more, no less.
    pub exact_width: bool,
    /// The list may be at most this share of the screen's height (46vh of
    /// the search list), scrolling beyond.
    pub max_height: Option<f32>,
    /// The keyboard starts on the row in use (or the first one), as `open`
    /// of ui.js does unless the menu was revealed by pointing.
    pub autofocus: bool,
    /// Arrows, Home, End and Enter move through the rows. Off where another
    /// widget owns the keyboard (the search field).
    pub keys: bool,
    /// The arrow keys and WASD close the menu and go on (`dismissKeys` of
    /// the map's context menu, which gives the flight keys back).
    pub flight_keys_close: bool,
}

impl Look {
    /// The list of a `.field-select`, the flight modes, the elevation models.
    pub const SELECT: Look = Look {
        ink: Ink::Menu,
        mono: false,
        dots: true,
        gap: LIST_GAP,
        glass: 0.6,
        min_width: 0.0,
        max_width: f32::INFINITY,
        exact_width: false,
        max_height: None,
        autofocus: true,
        keys: true,
        flight_keys_close: false,
    };
    /// The same set in the monospace of a mono field.
    pub const SELECT_MONO: Look = Look {
        mono: true,
        ..Look::SELECT
    };
    /// The map's layer list (`.layers-popover`: 168 px at least).
    pub const LAYERS: Look = Look {
        min_width: 168.0,
        ..Look::SELECT
    };
    /// The camera list: a chooser whose rows lie straight in the surface, at
    /// most 260 px, revealed by pointing so it does not take the keyboard.
    pub const CAMERAS: Look = Look {
        gap: 0.0,
        min_width: 168.0,
        max_width: 260.0,
        autofocus: false,
        ..Look::SELECT
    };
    /// A list of actions with an icon each: the power menu, Open all.
    pub const ACTIONS: Look = Look {
        dots: false,
        gap: 0.0,
        ..Look::SELECT
    };
    /// The context menu of a map point (`.map-context-menu`).
    pub const CONTEXT: Look = Look {
        ink: Ink::Map,
        glass: 0.9,
        max_width: 220.0,
        flight_keys_close: true,
        ..Look::ACTIONS
    };
    /// The answers of the place search (`.map-search-results`).
    pub const SEARCH: Look = Look {
        ink: Ink::Place,
        dots: false,
        gap: 0.0,
        glass: 0.86,
        exact_width: true,
        max_height: Some(0.46),
        autofocus: false,
        keys: false,
        ..Look::SELECT
    };
}

/// One row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub value: String,
    pub label: String,
    /// The quieter second line (`.option-item-note`).
    pub note: String,
    /// A Lucide name: the lead of an action row.
    pub icon: Option<&'static str>,
    /// Text at the right edge (the distance of a search answer).
    pub trail: String,
    /// In use whatever the menu's selected value is (a camera whose window is
    /// open; several rows can be).
    pub on: bool,
    pub enabled: bool,
}

impl Row {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Row {
        Row {
            value: value.into(),
            label: label.into(),
            note: String::new(),
            icon: None,
            trail: String::new(),
            on: false,
            enabled: true,
        }
    }

    pub fn disabled(mut self) -> Row {
        self.enabled = false;
        self
    }

    pub fn enabled(mut self, on: bool) -> Row {
        self.enabled = on;
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Row {
        self.note = note.into();
        self
    }

    pub fn icon(mut self, icon: &'static str) -> Row {
        self.icon = Some(icon);
        self
    }

    pub fn on(mut self, on: bool) -> Row {
        self.on = on;
        self
    }

    pub fn trail(mut self, text: impl Into<String>) -> Row {
        self.trail = text.into();
        self
    }
}

/// What a menu is made of, top to bottom.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// `.ui-menu-head`: what the list is about, before the rows.
    Head(String),
    /// `.layer-group`: a heading between groups of rows.
    Section(String),
    /// A sentence where there is nothing to pick (`.map-search-note`).
    Note(String),
    Row(Row),
}

impl From<Row> for Item {
    fn from(r: Row) -> Item {
        Item::Row(r)
    }
}

/// Where the menu goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anchor {
    /// Dropped out of an element: below it, above when the room below runs out.
    Below { rect: Rect, gap: f32 },
    /// Beside an element: left of it, right when the left has no room.
    Beside { rect: Rect, gap: f32 },
    /// At a point inside `bounds`: 12 px down and right, flipped on the axis
    /// that would overflow.
    Point { at: Pos2, bounds: Rect },
}

impl Anchor {
    /// `GAP` of `ui.js`: 6 px under a control.
    pub fn below(rect: Rect) -> Anchor {
        Anchor::Below { rect, gap: 6.0 }
    }

    pub fn below_by(rect: Rect, gap: f32) -> Anchor {
        Anchor::Below { rect, gap }
    }

    /// Beside a rail button: `MAP_RAIL_MENU_GAP` (8) plus the rail's padding
    /// (7) from the button.
    pub fn beside_rail(rect: Rect) -> Anchor {
        Anchor::Beside { rect, gap: 8.0 + 7.0 }
    }

    pub fn point(at: Pos2, bounds: Rect) -> Anchor {
        Anchor::Point { at, bounds }
    }

    /// The element the menu hangs from, which a press does not dismiss.
    fn element(&self) -> Option<Rect> {
        match self {
            Anchor::Below { rect, .. } | Anchor::Beside { rect, .. } => Some(*rect),
            Anchor::Point { .. } => None,
        }
    }

    /// `matchAnchorWidth`: a list dropped under a control is as wide as it.
    fn matches_width(&self) -> bool {
        matches!(self, Anchor::Below { .. })
    }
}

/// Where a menu of `size` (its natural size) lands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub rect: Rect,
    /// Above the element, to the right of it, or left of the point.
    pub flipped: bool,
}

/// `placeByElement` and `placeByPoint` of ui.js. `screen` is the room there
/// is, `max_h` an own cap on the height (a share of the screen).
pub fn place(anchor: &Anchor, size: Vec2, screen: Rect, max_h: Option<f32>) -> Placed {
    let clamp = |v: f32, lo: f32, hi: f32| v.min(hi).max(lo);
    match *anchor {
        Anchor::Below { rect: r, gap } => {
            let below = screen.bottom() - r.bottom() - gap - EDGE;
            let above = r.top() - screen.top() - gap - EDGE;
            let up = below < size.y.min(180.0) && above > below;
            let mut cap = MIN_MAX_H.max(if up { above } else { below });
            if let Some(m) = max_h {
                cap = cap.min(m);
            }
            let h = size.y.min(cap);
            let top = if up {
                (r.top() - gap - h).max(screen.top() + EDGE)
            } else {
                r.bottom() + gap
            };
            let left = clamp(r.left(), screen.left() + EDGE, screen.right() - size.x - EDGE);
            Placed {
                rect: Rect::from_min_size(Pos2::new(left, top), Vec2::new(size.x, h)),
                flipped: up,
            }
        }
        Anchor::Beside { rect: r, gap } => {
            let cap = MIN_MAX_H.max(screen.height() - 2.0 * EDGE);
            let h = size.y.min(cap);
            let flip =
                r.left() - gap - size.x < screen.left() + EDGE && screen.right() - r.right() - gap - size.x >= EDGE;
            let left = if flip { r.right() + gap } else { r.left() - gap - size.x };
            let left = clamp(left, screen.left() + EDGE, screen.right() - size.x - EDGE);
            let top = clamp(r.top(), screen.top() + EDGE, screen.bottom() - h - EDGE);
            Placed {
                rect: Rect::from_min_size(Pos2::new(left, top), Vec2::new(size.x, h)),
                flipped: flip,
            }
        }
        Anchor::Point { at, bounds } => {
            const GAP: f32 = 12.0;
            const EDGE_P: f32 = 4.0;
            let (w, h) = (size.x, size.y);
            let rel = at - bounds.min;
            let flip_x = rel.x + GAP + w > bounds.width() && rel.x - GAP - w >= 0.0;
            let flip_y = rel.y + GAP + h > bounds.height() && rel.y - GAP - h >= 0.0;
            let x = if flip_x { rel.x - GAP - w } else { rel.x + GAP };
            let y = if flip_y { rel.y - GAP - h } else { rel.y + GAP };
            let x = clamp(x, EDGE_P, bounds.width() - w - EDGE_P);
            let y = clamp(y, EDGE_P, bounds.height() - h - EDGE_P);
            Placed {
                rect: Rect::from_min_size(bounds.min + Vec2::new(x, y), Vec2::new(w, h)),
                flipped: flip_x || flip_y,
            }
        }
    }
}

/// The height of a `.ui-menu-head`: 4 above, 6 below a 10 px line.
pub const HEAD_H: f32 = 4.0 + 10.0 * LINE + 6.0;

/// The height of a `.layer-group`; the first of a list has no rule above it.
pub fn section_h(first: bool) -> f32 {
    let line = 9.5 * LINE;
    if first {
        2.0 + line + 3.0
    } else {
        8.0 + 1.0 + 6.0 + line + 3.0
    }
}

/// The height of a row of a given shape: the padding around the taller of the
/// lead and the text block, the note under the label with 1 px between.
pub fn row_height(ink: Ink, has_note: bool, has_icon: bool, dot: bool) -> f32 {
    let text = 12.0 * LINE + if has_note { 1.0 + ink.note_px() * LINE } else { 0.0 };
    7.0 * 2.0 + text.max(ink.lead_h(has_icon, dot))
}

/// `.map-search-note`: 8 px around the sentence.
pub fn note_h(text_h: f32) -> f32 {
    8.0 * 2.0 + text_h
}

/// Where each item sits in the menu's content (below the padding), and how
/// tall the whole is. `lead_head` is the height of a custom head before the
/// items; `note_text_h` is the wrapped sentence of a `Note`.
#[derive(Debug, Clone, PartialEq)]
pub struct Stack {
    pub ys: Vec<f32>,
    pub hs: Vec<f32>,
    pub content: f32,
}

pub fn stack(items: &[Item], look: &Look, lead_head: f32, note_text_h: f32) -> Stack {
    let mut ys = Vec::with_capacity(items.len());
    let mut hs = Vec::with_capacity(items.len());
    let mut y = lead_head;
    let mut seen_section = false;
    let mut prev_head = true;
    for it in items {
        if !prev_head {
            y += look.gap;
        }
        let h = match it {
            Item::Head(_) => HEAD_H,
            Item::Section(_) => {
                let h = section_h(!seen_section);
                seen_section = true;
                h
            }
            Item::Note(_) => note_h(note_text_h),
            Item::Row(r) => row_height(
                look.ink,
                !r.note.is_empty(),
                r.icon.is_some(),
                look.dots && r.icon.is_none(),
            ),
        };
        ys.push(y);
        hs.push(h);
        y += h;
        prev_head = matches!(it, Item::Head(_));
    }
    Stack { ys, hs, content: y }
}

/// The width of a menu: its widest row inside the padding, no narrower than
/// `min_width` and than the element it drops out of, no wider than
/// `max_width` (`min-width` outranks `max-width`).
pub fn width_for(look: &Look, anchor: &Anchor, widest: f32) -> f32 {
    let pad = look.ink.pad();
    if look.exact_width {
        if let Some(r) = anchor.element() {
            return r.width();
        }
    }
    let natural = pad * 2.0 + widest;
    let min = look.min_width.max(if anchor.matches_width() {
        anchor.element().map_or(0.0, |r| r.width())
    } else {
        0.0
    });
    natural.min(look.max_width).max(min)
}

/// The next row the keyboard can stop on, wrapping round; `from` is the row
/// it is on now (`None` before the first). `step` is 1 or -1.
pub fn next_enabled(items: &[Item], from: Option<usize>, step: i32) -> Option<usize> {
    let n = items.len() as i32;
    if n == 0 {
        return None;
    }
    let mut i = from.map_or(if step > 0 { -1 } else { n }, |f| f as i32);
    for _ in 0..n {
        i = (i + step).rem_euclid(n);
        if matches!(&items[i as usize], Item::Row(r) if r.enabled) {
            return Some(i as usize);
        }
    }
    None
}

fn row_galley(ui: &egui::Ui, text: &str, color: Color32, look: &Look) -> Arc<Galley> {
    if look.mono {
        style::galley_mono(ui, &crate::i18n::tr(text), 12.0, color)
    } else {
        style::galley(ui, text, 12.0, look.ink.weight(), color, 0.0)
    }
}

fn caps(ui: &egui::Ui, text: &str, px: f32, letter: f32) -> Arc<Galley> {
    let text = crate::i18n::tr(text).to_uppercase();
    style::galley(ui, &text, px, Weight::W600, style::text_3(), letter)
}

/// The colours a row is painted in.
struct Ink3 {
    label: Color32,
    note: Color32,
    icon: Color32,
}

fn colours(look: &Look, active: bool, hovered: bool, enabled: bool) -> Ink3 {
    let lit = active || hovered;
    match look.ink {
        Ink::Menu => Ink3 {
            label: if !enabled {
                style::text_3()
            } else if lit {
                style::text_1()
            } else {
                style::text_2()
            },
            note: if enabled {
                style::text_3()
            } else {
                style::text_disabled()
            },
            icon: if !enabled {
                style::text_disabled()
            } else if hovered {
                style::accent()
            } else {
                style::text_2()
            },
        },
        Ink::Map => Ink3 {
            label: if enabled { style::text_1() } else { style::text_3() },
            note: if enabled { style::text_2() } else { style::text_3() },
            icon: if !enabled {
                style::text_3()
            } else if hovered {
                style::accent()
            } else {
                style::text_2()
            },
        },
        Ink::Place => Ink3 {
            label: style::text_1(),
            note: style::text_3(),
            icon: if active {
                style::accent_bright()
            } else {
                style::text_3()
            },
        },
    }
}

/// One row painted in `r`; true when pressed. A disabled row only hovers.
/// `active` marks the row in use; `focused` the one the keyboard is on.
#[allow(clippy::too_many_arguments)]
fn paint_row(ui: &mut egui::Ui, r: Rect, id: Id, row: &Row, active: bool, focused: bool, look: &Look) -> bool {
    let sense = if row.enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let resp = ui.interact(r, id, sense);
    let hovered = resp.hovered();
    let pressed = row.enabled && resp.is_pointer_button_down_on();
    let radius = style::RADIUS_BTN;
    let painter = ui.painter().clone();
    let ring = |on: bool| {
        if on {
            painter.rect_stroke(
                r,
                radius,
                Stroke::new(2.0, style::accent_bright()),
                egui::StrokeKind::Inside,
            );
        }
    };
    match look.ink {
        Ink::Place => {
            if active || (hovered && row.enabled) {
                painter.rect_filled(r, radius, style::surface_hover());
            }
        }
        _ => {
            if active {
                painter.rect_filled(r, radius, style::accent_dark());
            } else if pressed {
                painter.rect_filled(r, radius, style::accent_dim());
            } else if hovered && row.enabled {
                painter.rect_filled(r, radius, style::tint(0.06));
            }
            ring(active || focused);
        }
    }
    let c = colours(look, active, hovered && row.enabled, row.enabled);
    let ink = look.ink;
    let x0 = r.left() + ink.row_x();
    let dot = look.dots && row.icon.is_none();
    let lead = ink.lead_w(row.icon.is_some(), dot);
    if let Some(name) = row.icon {
        let side = if ink == Ink::Place { 15.0 } else { 14.0 };
        crate::ui::icons::paint(
            ui,
            name,
            Rect::from_center_size(Pos2::new(x0 + lead / 2.0, r.center().y), Vec2::splat(side)),
            c.icon,
        );
    } else if dot {
        let col = if active { style::accent() } else { style::text_3() };
        kit::dot(&painter, Pos2::new(x0 + 3.5, r.center().y), col, active);
    }
    let text_x = x0 + if lead > 0.0 { lead + ink.row_gap() } else { 0.0 };
    let mut right = r.right() - ink.row_x();
    if !row.trail.is_empty() {
        let tc = if active { style::text_2() } else { style::text_3() };
        let g = style::galley(ui, &row.trail, 10.5, Weight::W400, tc, 0.0);
        painter.galley(
            Pos2::new(right - g.size().x, r.center().y - g.size().y / 2.0),
            g.clone(),
            tc,
        );
        right -= g.size().x + ink.row_gap();
    }
    let room = (right - text_x).max(0.0);
    let clip = Rect::from_min_max(Pos2::new(text_x, r.top()), Pos2::new(right.max(text_x), r.bottom()));
    let tp = painter.with_clip_rect(clip);
    let lg = if look.mono {
        row_galley(ui, &row.label, c.label, look)
    } else {
        crate::ui::text::clamped(ui, &row.label, 12.0, ink.weight(), c.label, 0.0, room, 1, None)
    };
    if row.note.is_empty() {
        tp.galley(Pos2::new(text_x, r.center().y - lg.size().y / 2.0), lg, c.label);
    } else {
        let ng = crate::ui::text::clamped(ui, &row.note, ink.note_px(), Weight::W400, c.note, 0.0, room, 1, None);
        let block = lg.size().y + 1.0 + ng.size().y;
        let top = r.center().y - block / 2.0;
        let ly = lg.size().y;
        tp.galley(Pos2::new(text_x, top), lg, c.label);
        tp.galley(Pos2::new(text_x, top + ly + 1.0), ng, c.note);
    }
    row.enabled && resp.clicked()
}

/// One row in a rect of the caller's own, for a panel that lays out its own
/// list (the elevation models of the 3D panel); true when pressed.
pub fn row(
    ui: &mut egui::Ui,
    r: Rect,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    active: bool,
    enabled: bool,
    look: &Look,
) -> bool {
    let row = Row::new("", label).enabled(enabled);
    let id = ui.scope_id().with(id);
    paint_row(ui, r, id, &row, active, false, look)
}

/// What a menu did this frame.
#[derive(Debug, Default)]
pub struct Outcome {
    pub picked: Option<String>,
    /// True when a press outside, or Escape, asks to close it.
    pub dismissed: bool,
    /// Where the menu is, so the pointer on it can keep it open.
    pub rect: Option<Rect>,
}

#[derive(Clone, Copy)]
struct State {
    pass: u64,
    since: f64,
    cursor: Option<usize>,
}

type HeadPainter<'a> = Box<dyn FnMut(&mut egui::Ui, Rect) + 'a>;

/// A menu, built with the pieces it has and then `show`n every frame it is
/// open.
pub struct Menu<'a> {
    id: Id,
    anchor: Anchor,
    look: &'a Look,
    items: &'a [Item],
    selected: Option<&'a str>,
    head: Option<(f32, f32, HeadPainter<'a>)>,
    opacity: f32,
}

impl<'a> Menu<'a> {
    pub fn new(id: Id, anchor: Anchor, look: &'a Look, items: &'a [Item]) -> Menu<'a> {
        Menu {
            id,
            anchor,
            look,
            items,
            selected: None,
            head: None,
            opacity: 1.0,
        }
    }

    /// The value of the row in use.
    pub fn selected(mut self, value: &'a str) -> Menu<'a> {
        self.selected = Some(value);
        self
    }

    /// A head the caller paints (the pin and the coordinates of the context
    /// menu): `height` tall, at least `min_width` wide inside the padding.
    pub fn head(mut self, height: f32, min_width: f32, paint: impl FnMut(&mut egui::Ui, Rect) + 'a) -> Menu<'a> {
        self.head = Some((height, min_width, Box::new(paint)));
        self
    }

    /// `opacity: 0.55` while an answer is on its way.
    pub fn opacity(mut self, o: f32) -> Menu<'a> {
        self.opacity = o;
        self
    }

    pub fn show(mut self, ctx: &egui::Context) -> Outcome {
        let look = *self.look;
        let items = self.items;
        let layer = egui::LayerId::new(egui::Order::Foreground, self.id.with("probe"));
        let probe = egui::Ui::new(
            ctx.clone(),
            self.id.with("probe"),
            egui::UiBuilder::new().layer_id(layer).max_rect(ctx.content_rect()),
        );
        let ink = look.ink;
        let pad = ink.pad();
        let lead_head = self.head.as_ref().map_or(0.0, |h| h.0);
        let mut widest = self.head.as_ref().map_or(0.0, |h| h.1);
        for it in items {
            let w = match it {
                Item::Head(t) | Item::Section(t) => {
                    let px = if matches!(it, Item::Head(_)) { 10.0 } else { 9.5 };
                    let letter = if matches!(it, Item::Head(_)) { 0.8 } else { 0.9 };
                    caps(&probe, t, px, letter).size().x + 12.0
                }
                Item::Note(_) => 0.0,
                Item::Row(r) => {
                    let dot = look.dots && r.icon.is_none();
                    let lead = ink.lead_w(r.icon.is_some(), dot);
                    let label = row_galley(&probe, &r.label, style::text_2(), &look).size().x;
                    let note = if r.note.is_empty() {
                        0.0
                    } else {
                        style::galley(&probe, &r.note, ink.note_px(), Weight::W400, style::text_3(), 0.0)
                            .size()
                            .x
                    };
                    let trail = if r.trail.is_empty() {
                        0.0
                    } else {
                        ink.row_gap()
                            + style::galley(&probe, &r.trail, 10.5, Weight::W400, style::text_3(), 0.0)
                                .size()
                                .x
                    };
                    ink.row_x() * 2.0 + if lead > 0.0 { lead + ink.row_gap() } else { 0.0 } + label.max(note) + trail
                }
            };
            widest = widest.max(w);
        }
        let width = width_for(&look, &self.anchor, widest);
        let inner_w = width - pad * 2.0;
        let note_text_h = items
            .iter()
            .find_map(|it| match it {
                Item::Note(t) => Some(
                    crate::ui::kit::wrapped_weight(
                        &probe,
                        t,
                        11.5,
                        Weight::W400,
                        style::text_2(),
                        inner_w - 18.0,
                        LINE,
                    )
                    .size()
                    .y,
                ),
                _ => None,
            })
            .unwrap_or(0.0);
        let st = stack(items, &look, lead_head, note_text_h);
        let full = pad * 2.0 + st.content;
        let screen = ctx.content_rect();
        let max_h = look.max_height.map(|f| screen.height() * f);
        let placed = place(&self.anchor, Vec2::new(width, full), screen, max_h);

        let pass = ctx.cumulative_pass_nr();
        let now = ctx.input(|i| i.time);
        let prev = ctx
            .data(|d| d.get_temp::<State>(self.id))
            .filter(|s| pass <= s.pass + 2);
        let fresh = prev.is_none();
        let mut state = prev.unwrap_or(State {
            pass,
            since: now,
            cursor: None,
        });
        if fresh && look.autofocus && look.keys {
            let active = self.selected.and_then(|s| {
                items
                    .iter()
                    .position(|it| matches!(it, Item::Row(r) if r.enabled && r.value == s))
            });
            state.cursor = active.or_else(|| next_enabled(items, None, 1));
        }
        let before = if fresh { None } else { state.cursor };
        state.pass = pass;
        let t = ((now - state.since) / ENTER).clamp(0.0, 1.0) as f32;
        let rise = (1.0 - (1.0 - t).powi(3)).clamp(0.0, 1.0);
        if t < 1.0 {
            ctx.request_repaint();
        }

        let menu = placed.rect.translate(Vec2::new(0.0, 4.0 * (1.0 - rise)));
        let mut out = Outcome {
            rect: Some(menu),
            ..Default::default()
        };
        let mut cursor = state.cursor;
        let id = self.id;
        let selected = self.selected;
        let opacity = self.opacity * rise;
        let mut head = self.head.take();
        egui::Area::new(id.with("area"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.min)
            .show(ctx, |ui| {
                ui.set_opacity(opacity);
                let painter = ui.painter().clone();
                style::glass_panel(&painter, ("menu", id), menu, style::RADIUS, look.glass);
                ui.allocate_rect(menu, egui::Sense::hover());
                if look.keys {
                    let moved = ui.input_mut(|i| {
                        let mut to = None;
                        if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                            to = Some(next_enabled(items, cursor, 1));
                        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                            to = Some(next_enabled(items, cursor, -1));
                        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                            to = Some(next_enabled(items, None, 1));
                        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                            to = Some(next_enabled(items, None, -1));
                        }
                        to
                    });
                    if let Some(to) = moved {
                        cursor = to;
                    }
                    let enter = ui.input_mut(|i| {
                        i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                            || i.consume_key(egui::Modifiers::NONE, egui::Key::Space)
                    });
                    if enter {
                        if let Some(Item::Row(r)) = cursor.and_then(|c| items.get(c)) {
                            out.picked = Some(r.value.clone());
                        }
                    }
                }
                let inner = menu.shrink(pad);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                child.set_clip_rect(inner);
                egui::ScrollArea::vertical()
                    .id_salt(id.with("scroll"))
                    .auto_shrink([false, false])
                    .show(&mut child, |ui| {
                        let origin = ui.max_rect().min;
                        let w = inner.width();
                        if let Some((h, _, paint)) = head.as_mut() {
                            paint(ui, Rect::from_min_size(origin, Vec2::new(w, *h)));
                        }
                        for (i, it) in items.iter().enumerate() {
                            let r = Rect::from_min_size(origin + Vec2::new(0.0, st.ys[i]), Vec2::new(w, st.hs[i]));
                            match it {
                                Item::Head(t) => {
                                    let g = caps(ui, t, 10.0, 0.8);
                                    ui.painter().galley(r.min + Vec2::new(6.0, 4.0), g, style::text_3());
                                }
                                Item::Section(t) => {
                                    let first = items[..i].iter().all(|p| !matches!(p, Item::Section(_)));
                                    let g = caps(ui, t, 9.5, 0.9);
                                    let top = if first { 2.0 } else { 8.0 + 1.0 + 6.0 };
                                    if !first {
                                        ui.painter().hline(
                                            (r.left() + 6.0)..=(r.right() - 6.0),
                                            r.top() + 8.0 + 0.5,
                                            Stroke::new(1.0, style::border_soft()),
                                        );
                                    }
                                    ui.painter().galley(r.min + Vec2::new(6.0, top), g, style::text_3());
                                }
                                Item::Note(t) => {
                                    let g = crate::ui::kit::wrapped_weight(
                                        ui,
                                        t,
                                        11.5,
                                        Weight::W400,
                                        style::text_2(),
                                        w - 18.0,
                                        LINE,
                                    );
                                    ui.painter().galley(r.min + Vec2::new(9.0, 8.0), g, style::text_2());
                                }
                                Item::Row(row) => {
                                    let active = row.on || selected.is_some_and(|s| row.value == s);
                                    let focused = cursor == Some(i) && look.keys;
                                    if focused && cursor != before {
                                        ui.scroll_to_rect(r, None);
                                    }
                                    if paint_row(ui, r, id.with(("row", i)), row, active, focused, &look) {
                                        out.picked = Some(row.value.clone());
                                    }
                                }
                            }
                        }
                        ui.allocate_space(Vec2::new(w, st.content));
                    });
                let mut inside = vec![menu];
                inside.extend(self.anchor.element());
                out.dismissed =
                    dismissed(ui, &inside) || (look.keys && tab(ui)) || (look.flight_keys_close && flight_key(ui));
            });
        state.cursor = cursor;
        ctx.data_mut(|d| d.insert_temp(self.id, state));
        out
    }
}

fn tab(ui: &egui::Ui) -> bool {
    ui.input(|i| i.key_pressed(egui::Key::Tab))
}

/// An arrow or W, A, S, D with no modifier that makes it a shortcut
/// (`isFlightKey` of map.js).
fn flight_key(ui: &egui::Ui) -> bool {
    use egui::Key::*;
    ui.input(|i| {
        !i.modifiers.command
            && !i.modifiers.alt
            && [ArrowUp, ArrowDown, ArrowLeft, ArrowRight, W, A, S, D]
                .into_iter()
                .any(|k| i.key_pressed(k))
    })
}

/// The list hanging under `anchor`, as wide as `look` asks, scrolling when
/// the screen has no room for all its rows. `selected` is the active value.
/// Pressing outside the list and the anchor, or Escape, dismisses it.
pub fn dropdown(
    ui: &egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    anchor: Rect,
    rows: &[Row],
    selected: &str,
    look: &Look,
) -> Outcome {
    let id = ui.scope_id().with(id).with("menu");
    let items: Vec<Item> = rows.iter().cloned().map(Item::Row).collect();
    Menu::new(id, Anchor::below(anchor), look, &items)
        .selected(selected)
        .show(ui.ctx())
}

/// True when the pointer pressed outside every one of `inside` this frame,
/// or Escape was pressed.
pub fn dismissed(ui: &egui::Ui, inside: &[Rect]) -> bool {
    let outside = ui.input(|i| i.pointer.any_pressed())
        && !ui
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| inside.iter().any(|r| r.contains(p)));
    outside || ui.input(|i| i.key_pressed(egui::Key::Escape))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))
    }

    fn row_h() -> f32 {
        row_height(Ink::Menu, false, false, true)
    }

    fn rows(n: usize) -> Vec<Item> {
        (0..n)
            .map(|i| Item::Row(Row::new(i.to_string(), format!("Row {i}"))))
            .collect()
    }

    #[test]
    fn a_row_is_seven_padding_and_a_line_of_12() {
        let row_h = row_height(Ink::Menu, false, false, true);
        assert!((row_h - 28.52).abs() < 0.01);
        assert!((row_height(Ink::Menu, true, true, false) - (14.0 + 14.52 + 1.0 + 9.5 * 1.21)).abs() < 0.01);
        assert!((row_height(Ink::Map, true, true, false) - (14.0 + 14.52 + 1.0 + 10.5 * 1.21)).abs() < 0.01);
        // the search row: the 15 px icon is taller than a name alone
        assert!((row_height(Ink::Place, false, true, false) - 29.0).abs() < 0.01);
        assert!((row_height(Ink::Place, true, true, false) - (14.0 + 14.52 + 1.0 + 10.5 * 1.21)).abs() < 0.01);
    }

    #[test]
    fn the_head_and_the_section_heights() {
        assert!((HEAD_H - 22.1).abs() < 0.01);
        assert!((section_h(true) - (2.0 + 9.5 * 1.21 + 3.0)).abs() < 0.01);
        assert!((section_h(false) - (15.0 + 9.5 * 1.21 + 3.0)).abs() < 0.01);
        assert!(section_h(false) > section_h(true));
    }

    #[test]
    fn rows_of_an_option_list_are_two_apart_and_a_bare_menu_touches() {
        let items = rows(3);
        let a = stack(&items, &Look::SELECT, 0.0, 0.0);
        assert_eq!(a.ys[1] - a.ys[0], row_h() + 2.0);
        assert!((a.content - (3.0 * row_h() + 4.0)).abs() < 1e-3);
        let b = stack(&items, &Look::ACTIONS, 0.0, 0.0);
        assert!((b.ys[1] - b.ys[0] - row_h()).abs() < 1e-3);
        assert!((b.content - 3.0 * row_h()).abs() < 1e-3);
    }

    #[test]
    fn nothing_is_a_gap_after_the_head_and_sections_sit_between_groups() {
        let items = vec![
            Item::Head("Map layers".into()),
            Item::Section("Esri".into()),
            Item::Row(Row::new("a", "A")),
            Item::Section("Google".into()),
            Item::Row(Row::new("b", "B")),
        ];
        let s = stack(&items, &Look::LAYERS, 0.0, 0.0);
        assert_eq!(s.ys[0], 0.0);
        assert_eq!(s.ys[1], HEAD_H, "no gap under the head");
        assert!((s.ys[2] - (HEAD_H + section_h(true) + 2.0)).abs() < 1e-3);
        assert!((s.ys[3] - (s.ys[2] + row_h() + 2.0)).abs() < 1e-3);
        assert!((s.hs[3] - section_h(false)).abs() < 1e-3, "only the first has no rule");
    }

    #[test]
    fn a_custom_head_pushes_the_rows_down_without_a_gap() {
        let items = rows(2);
        let s = stack(&items, &Look::CONTEXT, 34.0, 0.0);
        assert_eq!(s.ys[0], 34.0);
        assert!((s.content - (34.0 + 2.0 * row_height(Ink::Map, false, true, false))).abs() < 1e-3);
    }

    #[test]
    fn a_list_under_a_control_opens_6_px_below_and_as_wide_as_the_control() {
        let a = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::new(200.0, 30.0));
        let w = width_for(&Look::SELECT, &Anchor::below(a), 120.0);
        assert_eq!(w, 200.0);
        let p = place(&Anchor::below(a), Vec2::new(w, 90.0), screen(), None);
        assert_eq!(p.rect.min, Pos2::new(100.0, 136.0));
        assert!(!p.flipped);
        // wider than the control: as wide as its widest row
        assert_eq!(width_for(&Look::SELECT, &Anchor::below(a), 300.0), 310.0);
    }

    #[test]
    fn a_list_flips_above_when_the_room_below_runs_out() {
        let a = Rect::from_min_size(Pos2::new(100.0, 700.0), Vec2::new(200.0, 30.0));
        let p = place(&Anchor::below(a), Vec2::new(200.0, 150.0), screen(), None);
        assert!(p.flipped);
        assert_eq!(p.rect.bottom(), 700.0 - 6.0);
        // room below for a short list is a list below
        let b = Rect::from_min_size(Pos2::new(100.0, 400.0), Vec2::new(200.0, 30.0));
        assert!(!place(&Anchor::below(b), Vec2::new(200.0, 150.0), screen(), None).flipped);
    }

    #[test]
    fn a_long_list_scrolls_in_the_room_it_has() {
        let a = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::new(200.0, 30.0));
        let p = place(&Anchor::below(a), Vec2::new(200.0, 5000.0), screen(), None);
        assert!((p.rect.bottom() - (800.0 - 8.0)).abs() < 1e-3, "{:?}", p.rect);
        let capped = place(
            &Anchor::below(a),
            Vec2::new(200.0, 5000.0),
            screen(),
            Some(0.46 * 800.0),
        );
        assert!((capped.rect.height() - 368.0).abs() < 1e-3);
        // never squeezed below 120
        let low = Rect::from_min_size(Pos2::new(100.0, 780.0), Vec2::new(200.0, 10.0));
        assert!(
            place(&Anchor::below(low), Vec2::new(200.0, 500.0), screen(), None)
                .rect
                .height()
                >= 120.0 - 1e-3
        );
    }

    #[test]
    fn the_rail_lists_open_to_the_left_level_with_the_button() {
        let b = Rect::from_min_size(Pos2::new(1150.0, 120.0), Vec2::splat(34.0));
        let w = width_for(&Look::LAYERS, &Anchor::beside_rail(b), 100.0);
        assert_eq!(w, 168.0, "168 px at least, the button's 34 is not the floor");
        let p = place(&Anchor::beside_rail(b), Vec2::new(w, 300.0), screen(), None);
        assert_eq!(p.rect.right(), 1150.0 - 15.0);
        assert_eq!(p.rect.top(), 120.0);
        assert!(!p.flipped);
    }

    #[test]
    fn a_rail_list_goes_right_when_the_left_has_no_room_and_stays_on_screen() {
        let b = Rect::from_min_size(Pos2::new(20.0, 700.0), Vec2::splat(34.0));
        let p = place(&Anchor::beside_rail(b), Vec2::new(168.0, 300.0), screen(), None);
        assert!(p.flipped);
        assert_eq!(p.rect.left(), 54.0 + 15.0);
        assert_eq!(p.rect.bottom(), 800.0 - 8.0, "pulled up to fit");
    }

    #[test]
    fn the_camera_list_is_168_to_260_wide() {
        let b = Rect::from_min_size(Pos2::new(1150.0, 120.0), Vec2::splat(34.0));
        let a = Anchor::beside_rail(b);
        assert_eq!(width_for(&Look::CAMERAS, &a, 40.0), 168.0);
        assert_eq!(width_for(&Look::CAMERAS, &a, 900.0), 260.0);
    }

    #[test]
    fn a_menu_at_a_point_sits_12_px_off_it_and_flips_to_the_room() {
        let bounds = Rect::from_min_size(Pos2::new(0.0, 60.0), Vec2::new(1000.0, 600.0));
        let p = place(
            &Anchor::point(Pos2::new(100.0, 160.0), bounds),
            Vec2::new(180.0, 100.0),
            bounds,
            None,
        );
        assert_eq!(p.rect.min, Pos2::new(112.0, 172.0));
        assert!(!p.flipped);
        let q = place(
            &Anchor::point(Pos2::new(950.0, 600.0), bounds),
            Vec2::new(180.0, 100.0),
            bounds,
            None,
        );
        assert!(q.flipped);
        assert_eq!(q.rect.max, Pos2::new(950.0 - 12.0, 600.0 - 12.0));
        // too big for either side: clamped back inside by 4 px
        let r = place(
            &Anchor::point(Pos2::new(500.0, 360.0), bounds),
            Vec2::new(1000.0, 100.0),
            bounds,
            None,
        );
        assert_eq!(r.rect.left(), 4.0);
    }

    #[test]
    fn the_context_menu_is_at_most_220_wide_and_dense() {
        let a = Anchor::point(Pos2::ZERO, screen());
        assert_eq!(width_for(&Look::CONTEXT, &a, 400.0), 220.0);
        assert_eq!(width_for(&Look::CONTEXT, &a, 90.0), 100.0);
        assert_eq!(Look::CONTEXT.glass, 0.9);
        assert!(std::hint::black_box(Look::CONTEXT).flight_keys_close);
    }

    #[test]
    fn the_search_list_is_as_wide_as_its_field_and_a_panel_of_its_own() {
        let field = Rect::from_min_size(Pos2::new(800.0, 14.0), Vec2::new(272.0, 34.0));
        let a = Anchor::below_by(field, 8.0);
        assert_eq!(width_for(&Look::SEARCH, &a, 50.0), 272.0);
        assert_eq!(width_for(&Look::SEARCH, &a, 900.0), 272.0);
        assert_eq!(Look::SEARCH.glass, 0.86);
        assert_eq!(Look::SEARCH.ink.pad(), 4.0);
        assert!(!std::hint::black_box(Look::SEARCH).keys, "the field owns the keyboard");
        let p = place(&a, Vec2::new(272.0, 200.0), screen(), None);
        assert_eq!(p.rect.min, Pos2::new(800.0, 14.0 + 34.0 + 8.0));
    }

    #[test]
    fn which_look_a_kind_of_menu_gets() {
        // choosers: dot, ring, the glass
        for l in [Look::SELECT, Look::LAYERS, Look::CAMERAS] {
            assert!(l.dots);
            assert_eq!(l.ink, Ink::Menu);
            assert_eq!(l.glass, 0.6);
        }
        // actions: an icon
        assert!(!std::hint::black_box(Look::ACTIONS).dots && !std::hint::black_box(Look::CONTEXT).dots);
        // the gap is the option-list's
        assert_eq!(Look::SELECT.gap, 2.0);
        assert_eq!(Look::LAYERS.gap, 2.0);
        assert_eq!(Look::CAMERAS.gap, 0.0);
        assert_eq!(Look::ACTIONS.gap, 0.0);
        assert_eq!(Ink::Menu.weight(), Weight::W500);
        assert_eq!(Ink::Place.weight(), Weight::W400);
        // the camera list is revealed by pointing and keeps the keyboard
        assert!(!std::hint::black_box(Look::CAMERAS).autofocus);
        assert!(std::hint::black_box(Look::SELECT).autofocus);
    }

    #[test]
    fn a_row_with_a_lead_is_wider_by_the_lead_and_its_gap() {
        assert_eq!(Ink::Menu.lead_w(false, true), 7.0);
        assert_eq!(Ink::Menu.lead_w(true, false), 14.0);
        assert_eq!(Ink::Place.lead_w(true, false), 16.0);
        assert_eq!(Ink::Menu.lead_w(false, false), 0.0);
    }

    #[test]
    fn the_keyboard_skips_what_cannot_be_picked_and_wraps() {
        let items = vec![
            Item::Head("h".into()),
            Item::Row(Row::new("a", "A")),
            Item::Row(Row::new("b", "B").disabled()),
            Item::Section("s".into()),
            Item::Row(Row::new("c", "C")),
        ];
        assert_eq!(next_enabled(&items, None, 1), Some(1));
        assert_eq!(next_enabled(&items, Some(1), 1), Some(4));
        assert_eq!(next_enabled(&items, Some(4), 1), Some(1), "round the end");
        assert_eq!(next_enabled(&items, Some(1), -1), Some(4));
        assert_eq!(next_enabled(&items, None, -1), Some(4), "End");
        assert_eq!(next_enabled(&[], None, 1), None);
        assert_eq!(next_enabled(&[Item::Head("h".into())], None, 1), None);
    }

    #[test]
    fn a_note_is_8_px_around_its_sentence() {
        assert_eq!(note_h(14.0), 30.0);
        let items = vec![Item::Note("Nothing there".into())];
        assert_eq!(stack(&items, &Look::SEARCH, 0.0, 14.0).content, 30.0);
    }
}
