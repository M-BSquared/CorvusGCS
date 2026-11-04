//! The preflight checklist's window on the Home map (the window half of
//! python/src/js/checklist.js, the `.checklist-*` rules of map.css).
//!
//! It rests under the flight bar until it is dragged by its bar; double click
//! on the bar sends it home. Position and fold are kept in
//! `~/.corvus/checklist-window.json`, what has been ticked in
//! `~/.corvus/checklist-ticks.json`: both belong to this station, not to the
//! setup the backend keeps. An item is ticked by its text and which
//! occurrence of that text it is, so reordering a list keeps the ticks and
//! rewording an item clears only that item.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::setup::checklist::{Item, List};
use crate::ui::icons;
use crate::ui::kit::{self, Choice};
use crate::ui::style::{self, Weight};

const MIN_VISIBLE: f32 = 64.0;
const EDGE: f32 = 8.0;
const WIDTH: f32 = 272.0;

/// `itemKeys`: the tick key of every item, `None` for a heading. The text
/// plus which occurrence of it this is, so two items that read the same stay
/// two.
pub fn item_keys(items: &[Item]) -> Vec<Option<String>> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    items
        .iter()
        .map(|it| {
            if it.heading {
                return None;
            }
            let n = seen.entry(it.text.as_str()).or_insert(0);
            *n += 1;
            Some(format!("{}\u{0}{}", it.text, n))
        })
        .collect()
}

/// `progress`: how far through a list the operator is.
pub fn progress(list: &List, ticked: &[String]) -> (usize, usize) {
    let keys: Vec<String> = item_keys(&list.items).into_iter().flatten().collect();
    let done = keys.iter().filter(|k| ticked.contains(k)).count();
    (done, keys.len())
}

/// The station's own memory of the window.
#[derive(Debug, Default)]
pub struct WindowState {
    pub ticks: HashMap<String, Vec<String>>,
    pub pos: Option<(f32, f32)>,
    pub collapsed: bool,
    drag: Option<Vec2>,
    last_host: (f32, f32),
    pub was_armed: Option<bool>,
    ticks_path: Option<PathBuf>,
    pos_path: Option<PathBuf>,
}

fn corvus_file(name: &str) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".corvus").join(name))
}

fn write(path: &Option<PathBuf>, v: &Value) {
    let Some(p) = path else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, v.to_string());
}

impl WindowState {
    /// What the station kept: the ticks and the window's place and fold. An
    /// unreadable file is an empty memory.
    pub fn load_default() -> WindowState {
        WindowState::load(
            corvus_file("checklist-ticks.json"),
            corvus_file("checklist-window.json"),
        )
    }

    pub fn load(ticks_path: Option<PathBuf>, pos_path: Option<PathBuf>) -> WindowState {
        let read = |p: &Option<PathBuf>| -> Value {
            p.as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null)
        };
        let mut st = WindowState {
            ticks_path: ticks_path.clone(),
            pos_path: pos_path.clone(),
            ..Default::default()
        };
        if let Some(o) = read(&ticks_path).as_object() {
            for (id, v) in o {
                if let Some(a) = v.as_array() {
                    st.ticks.insert(
                        id.clone(),
                        a.iter().filter_map(|k| k.as_str().map(String::from)).collect(),
                    );
                }
            }
        }
        let p = read(&pos_path);
        let (x, y) = (p.get("x").and_then(Value::as_f64), p.get("y").and_then(Value::as_f64));
        if let (Some(x), Some(y)) = (x, y) {
            st.pos = Some((x as f32, y as f32));
        }
        st.collapsed = p.get("collapsed") == Some(&Value::Bool(true));
        st
    }

    fn save_ticks(&self) {
        let o: serde_json::Map<String, Value> = self
            .ticks
            .iter()
            .map(|(k, v)| (k.clone(), Value::from(v.clone())))
            .collect();
        write(&self.ticks_path, &Value::Object(o));
    }

    pub fn save_pos(&self) {
        write(
            &self.pos_path,
            &json!({"x": self.pos.map(|p| p.0), "y": self.pos.map(|p| p.1), "collapsed": self.collapsed}),
        );
    }

    pub fn ticked(&self, list_id: &str) -> &[String] {
        self.ticks.get(list_id).map_or(&[], Vec::as_slice)
    }

    /// `toggleItem`.
    pub fn toggle(&mut self, list_id: &str, key: &str) {
        let v = self.ticks.entry(list_id.to_string()).or_default();
        if let Some(i) = v.iter().position(|k| k == key) {
            v.remove(i);
        } else {
            v.push(key.to_string());
        }
        self.save_ticks();
    }

    /// `resetList`: unticks every item, for the next flight.
    pub fn reset(&mut self, list_id: &str) {
        self.ticks.remove(list_id);
        self.save_ticks();
    }

    /// `pruneTicks`: ticks of lists and items that no longer exist go, so the
    /// file does not grow with every list ever written.
    pub fn prune(&mut self, lists: &[List]) {
        let mut next: HashMap<String, Vec<String>> = HashMap::new();
        for l in lists {
            let keys: Vec<String> = item_keys(&l.items).into_iter().flatten().collect();
            let kept: Vec<String> = self
                .ticked(&l.id)
                .iter()
                .filter(|k| keys.contains(k))
                .cloned()
                .collect();
            if !kept.is_empty() {
                next.insert(l.id.clone(), kept);
            }
        }
        if next != self.ticks {
            self.ticks = next;
            self.save_ticks();
        }
    }

    /// `onTelemetry`: arming with the list unfinished is reported once per
    /// arming, never prevented. Only the transition counts, so a station
    /// opened on an aircraft already in the air says nothing.
    pub fn arming_warning(&mut self, state: &Value, enabled: bool, active: Option<&List>) -> Option<String> {
        let armed = state.get("armed") == Some(&Value::Bool(true));
        let connected = state.get("connected") == Some(&Value::Bool(true));
        let mut out = None;
        if self.was_armed == Some(false) && armed && enabled {
            if let Some(l) = active {
                let (done, total) = progress(l, self.ticked(&l.id));
                if total > 0 && done < total {
                    out = Some(format!(
                        "Armed with the preflight checklist open: {done} of {total} items checked."
                    ));
                }
            }
        }
        self.was_armed = if connected { Some(armed) } else { None };
        out
    }

    fn clamp_loose(&self, x: f32, y: f32, host: Vec2, panel: Vec2) -> (f32, f32) {
        let max_x = (host.x - MIN_VISIBLE).max(0.0);
        let max_y = (host.y - MIN_VISIBLE).max(0.0);
        (
            x.clamp(MIN_VISIBLE - panel.x, max_x.max(MIN_VISIBLE - panel.x)),
            y.clamp(0.0, max_y),
        )
    }

    /// `contain`: kept inside the map with an edge of 8 px when it fits.
    fn contain(&self, x: f32, y: f32, host: Vec2, panel: Vec2) -> (f32, f32) {
        let loose = self.clamp_loose(x, y, host, panel);
        let (max_x, max_y) = (host.x - panel.x - EDGE, host.y - panel.y - EDGE);
        (
            if max_x >= EDGE { x.clamp(EDGE, max_x) } else { loose.0 },
            if max_y >= EDGE { y.clamp(EDGE, max_y) } else { loose.1 },
        )
    }

    /// `onHostResize`: the map changed size, usually the right panel sliding.
    /// A window nearer the right edge travels with it.
    fn follow_host(&mut self, host: Vec2, panel: Vec2) {
        let size = (host.x, host.y);
        if let Some((x, y)) = self.pos {
            if self.last_host.0 > 0.0 && size != self.last_host {
                let (mut nx, mut ny) = (x, y);
                if size.0 != self.last_host.0 && (self.last_host.0 - (x + panel.x)) < x {
                    nx += size.0 - self.last_host.0;
                }
                if size.1 != self.last_host.1 && (self.last_host.1 - (y + panel.y)) < y {
                    ny += size.1 - self.last_host.1;
                }
                self.pos = Some(self.contain(nx, ny, host, panel));
            }
        }
        self.last_host = size;
    }
}

/// What the window shows.
pub struct View<'a> {
    pub list: Option<&'a List>,
    pub all: &'a [List],
}

/// What the operator did in it.
#[derive(Debug, Default, PartialEq)]
pub struct Actions {
    pub toggle: Option<String>,
    pub reset: bool,
    pub close: bool,
    pub pick: Option<String>,
}

fn row_text(ui: &egui::Ui, text: &str, width: f32, on: bool) -> std::sync::Arc<egui::Galley> {
    let color = if on { style::text_3() } else { style::text_1() };
    kit::wrapped_weight(ui, text, 12.5, Weight::W400, color, width, 1.35)
}

/// Draws the window in `host` (the map's rectangle). `below` is where the
/// flight bar ends, which is where an unmoved window starts.
pub fn show(ui: &mut egui::Ui, st: &mut WindowState, host: Rect, below: f32, view: &View) -> Actions {
    let mut act = Actions::default();
    let list = view.list;
    let (done, total) = list.map_or((0, 0), |l| progress(l, st.ticked(&l.id)));
    let complete = total > 0 && done == total;
    let inner_w = WIDTH - 16.0;

    // the rows, measured
    let keys = list.map(|l| item_keys(&l.items)).unwrap_or_default();
    let mut body_h = 0.0;
    let picker = view.all.len() > 1;
    let mut rows: Vec<(f32, bool)> = Vec::new();
    if !st.collapsed {
        if picker {
            body_h += 28.52 + 6.0;
        }
        match list {
            Some(l) if !l.items.is_empty() => {
                let mut first = true;
                for (i, it) in l.items.iter().enumerate() {
                    let h = if it.heading {
                        let g = style::galley(ui, &it.text.to_uppercase(), 9.5, Weight::W600, style::text_3(), 0.7);
                        (if first { 2.0 } else { 8.0 }) + g.size().y + 3.0
                    } else {
                        let on = keys[i].as_ref().is_some_and(|k| st.ticked(&l.id).contains(k));
                        let g = row_text(ui, &it.text, inner_w - 8.0 - 16.0 - 8.0, on);
                        5.0 + g.size().y.max(17.0) + 5.0
                    };
                    rows.push((h, it.heading));
                    first = false;
                }
                body_h += rows.iter().map(|r| r.0).sum::<f32>() + (rows.len() as f32 - 1.0).max(0.0);
                if complete {
                    body_h += 6.0 + 14.0;
                }
            }
            _ => {
                let msg = if list.is_some() {
                    "This checklist has no items yet. Add them under Setup, Safety & Sensors."
                } else {
                    "No checklist yet. Write one under Setup, Safety & Sensors."
                };
                body_h += kit::wrapped_weight(ui, msg, 12.0, Weight::W400, style::text_3(), inner_w - 8.0, 1.45)
                    .size()
                    .y
                    + 8.0;
            }
        }
    }
    let bar_h = 20.0;
    let chrome = 6.0 + bar_h + if st.collapsed { 6.0 } else { 6.0 + 1.0 + 6.0 + 8.0 };
    let max_h = if st.pos.is_some() {
        host.height() - 92.0
    } else {
        host.height() - below - 16.0
    };
    let height = (chrome + body_h).min(max_h.max(chrome));
    let panel_size = Vec2::new(WIDTH, height);

    st.follow_host(host.size(), panel_size);
    let origin = match st.pos {
        None => host.min + Vec2::new(14.0, below),
        Some((x, y)) => {
            let c = st.clamp_loose(x, y, host.size(), panel_size);
            st.pos = Some(c);
            host.min + Vec2::new(c.0, c.1)
        }
    };
    let rect = Rect::from_min_size(origin, panel_size);
    let painter = ui.painter().clone();
    style::glass_panel(&painter, "checklist", rect, 14, 0.6);
    // The window takes the pointer, so the map under it does not pan.
    ui.interact(rect, ui.scope_id().with("clpanel"), egui::Sense::click_and_drag());

    // the bar
    let bar = Rect::from_min_size(
        Pos2::new(rect.left() + 8.0, rect.top() + 6.0),
        Vec2::new(inner_w, bar_h),
    );
    let grip = ui.interact(bar, ui.scope_id().with("clgrip"), egui::Sense::click_and_drag());
    if grip.hovered() || st.drag.is_some() {
        ui.ctx().set_cursor_icon(if st.drag.is_some() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::Grab
        });
    }
    let grip = grip.on_hover_text("Drag to move, double-click to reset");
    let icon_c = if complete { style::healthy() } else { style::text_3() };
    icons::paint(
        ui,
        "clipboard-check",
        Rect::from_center_size(Pos2::new(bar.left() + 2.0 + 7.0, bar.center().y), Vec2::splat(14.0)),
        icon_c,
    );
    // three 20 px buttons at the right, 6 px apart
    let mut bx = bar.right();
    let mut button = |ui: &mut egui::Ui, icon: &'static str, title: &str, id: &str| -> bool {
        bx -= 20.0;
        let r = Rect::from_min_size(Pos2::new(bx, bar.top()), Vec2::splat(20.0));
        let resp = ui
            .interact(r, ui.scope_id().with(("clbtn", id)), egui::Sense::click())
            .on_hover_text(title);
        if resp.hovered() {
            painter.rect_filled(r, 5, style::tint(0.04));
        }
        icons::paint(
            ui,
            icon,
            Rect::from_center_size(r.center(), Vec2::splat(13.0)),
            if resp.hovered() {
                style::text_1()
            } else {
                style::text_3()
            },
        );
        bx -= 6.0;
        resp.clicked()
    };
    let close = button(
        ui,
        "x",
        "Hide the checklist window. Setup, Safety & Sensors shows it again.",
        "close",
    );
    let fold = button(
        ui,
        if st.collapsed { "chevron-down" } else { "chevron-up" },
        if st.collapsed {
            "Expand the checklist"
        } else {
            "Collapse the checklist"
        },
        "fold",
    );
    let reset = button(ui, "rotate-ccw", "Uncheck every item for the next flight", "reset");
    let count = if total > 0 {
        format!("{done}/{total}")
    } else {
        String::new()
    };
    let cg = (!count.is_empty()).then(|| {
        style::galley_mono(
            ui,
            &count,
            11.0,
            if complete { style::healthy() } else { style::text_2() },
        )
    });
    let count_w = cg.as_ref().map_or(0.0, |g| g.size().x + 6.0);
    let title_text = list.map_or("PREFLIGHT CHECKLIST".to_string(), |l| l.name.to_uppercase());
    let title_x = bar.left() + 2.0 + 14.0 + 6.0;
    let title_w = (bx - count_w - title_x).max(0.0);
    let tg = style::galley(ui, &title_text, 11.0, Weight::W600, style::text_1(), 0.4);
    ui.painter_at(Rect::from_min_size(
        Pos2::new(title_x, bar.top()),
        Vec2::new(title_w, bar_h),
    ))
    .galley(
        Pos2::new(title_x, bar.center().y - tg.size().y / 2.0),
        tg,
        style::text_1(),
    );
    if let Some(g) = cg {
        painter.galley(
            Pos2::new(bx - g.size().x, bar.center().y - g.size().y / 2.0),
            g,
            style::text_2(),
        );
    }
    act.close = close;
    act.reset = reset;
    if fold {
        st.collapsed = !st.collapsed;
        st.save_pos();
    }

    // drag by the bar, double click sends it home
    if grip.drag_started() {
        if let Some(p) = grip.interact_pointer_pos() {
            st.drag = Some(p - rect.min);
        }
    }
    if let (Some(off), true) = (st.drag, grip.dragged()) {
        if let Some(p) = grip.interact_pointer_pos() {
            let at = p - host.min - off;
            st.pos = Some((at.x, at.y));
        }
    }
    if grip.drag_stopped() {
        st.drag = None;
        st.save_pos();
    }
    if grip.double_clicked() {
        st.pos = None;
        st.save_pos();
    }

    if st.collapsed {
        return act;
    }
    // the body
    let rule_y = bar.bottom() + 6.0;
    painter.hline(
        egui::Rangef::new(bar.left(), bar.right()),
        rule_y + 0.5,
        Stroke::new(1.0, style::border_soft()),
    );
    let body = Rect::from_min_max(
        Pos2::new(bar.left(), rule_y + 1.0 + 6.0),
        Pos2::new(bar.right(), rect.bottom() - 8.0),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
    child.set_clip_rect(body);
    egui::ScrollArea::vertical()
        .id_salt("clscroll")
        .auto_shrink([false, false])
        .show(&mut child, |ui| {
            let origin = ui.max_rect().min;
            let mut y = origin.y;
            if picker {
                let choices: Vec<Choice> = view
                    .all
                    .iter()
                    .map(|l| Choice::new(l.id.clone(), l.name.clone()))
                    .collect();
                let r = Rect::from_min_size(Pos2::new(origin.x, y), Vec2::new(inner_w, 28.52));
                let current = list.map(|l| l.id.clone()).unwrap_or_default();
                if let Some(v) = kit::select(ui, r, &choices, &current, "", false, "Checklist", "clpicker") {
                    act.pick = Some(v);
                }
                y += 28.52 + 6.0;
            }
            match list {
                Some(l) if !l.items.is_empty() => {
                    let have = st.ticked(&l.id).to_vec();
                    for (i, it) in l.items.iter().enumerate() {
                        let h = rows[i].0;
                        if it.heading {
                            let g = style::galley(ui, &it.text.to_uppercase(), 9.5, Weight::W600, style::text_3(), 0.7);
                            let top = y + if i == 0 { 2.0 } else { 8.0 };
                            ui.painter().galley(Pos2::new(origin.x + 4.0, top), g, style::text_3());
                        } else {
                            let key = keys[i].clone().unwrap_or_default();
                            let on = have.contains(&key);
                            let r = Rect::from_min_size(Pos2::new(origin.x, y), Vec2::new(inner_w, h));
                            let resp = ui.interact(r, ui.scope_id().with(("clitem", i)), egui::Sense::click());
                            if resp.hovered() {
                                ui.painter().rect_filled(r, 6, style::tint(0.04));
                            }
                            let bxr =
                                Rect::from_min_size(Pos2::new(r.left() + 4.0, r.top() + 5.0 + 1.0), Vec2::splat(16.0));
                            if on {
                                ui.painter().rect_filled(bxr, 4, style::healthy());
                                icons::paint(
                                    ui,
                                    "check",
                                    Rect::from_center_size(bxr.center(), Vec2::splat(12.0)),
                                    style::text_inverted(),
                                );
                            } else {
                                ui.painter().rect_stroke(
                                    bxr,
                                    4,
                                    Stroke::new(1.5, style::border()),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            let g = row_text(ui, &it.text, inner_w - 8.0 - 16.0 - 8.0, on);
                            let tp = Pos2::new(bxr.right() + 8.0, r.top() + 5.0);
                            ui.painter().galley(tp, g.clone(), style::text_1());
                            if on {
                                // text-decoration: line-through
                                for row in &g.rows {
                                    let ly = tp.y + row.rect().center().y;
                                    ui.painter().hline(
                                        egui::Rangef::new(tp.x + row.rect().left(), tp.x + row.rect().right()),
                                        ly,
                                        Stroke::new(1.0, style::text_3()),
                                    );
                                }
                            }
                            if resp.clicked() {
                                act.toggle = Some(key);
                            }
                        }
                        y += h + 1.0;
                    }
                    if complete {
                        y += 5.0;
                        icons::paint(
                            ui,
                            "circle-check",
                            Rect::from_min_size(Pos2::new(origin.x + 4.0, y + 6.0), Vec2::splat(14.0)),
                            style::healthy(),
                        );
                        let g = style::galley(ui, "All items checked", 12.0, Weight::W600, style::healthy(), 0.0);
                        ui.painter()
                            .galley(Pos2::new(origin.x + 4.0 + 14.0 + 6.0, y + 6.0), g, style::healthy());
                    }
                }
                _ => {
                    let msg = if list.is_some() {
                        "This checklist has no items yet. Add them under Setup, Safety & Sensors."
                    } else {
                        "No checklist yet. Write one under Setup, Safety & Sensors."
                    };
                    let g = kit::wrapped_weight(ui, msg, 12.0, Weight::W400, style::text_3(), inner_w - 8.0, 1.45);
                    ui.painter()
                        .galley(Pos2::new(origin.x + 4.0, y + 4.0), g, style::text_3());
                }
            }
            ui.allocate_space(Vec2::new(inner_w, body_h.max(1.0)));
        });
    act
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(t: &str, h: bool) -> Item {
        Item {
            text: t.into(),
            heading: h,
        }
    }

    fn list(items: Vec<Item>) -> List {
        List {
            id: "a".into(),
            name: "A".into(),
            items,
        }
    }

    #[test]
    fn two_items_that_read_the_same_stay_two_and_a_heading_has_no_key() {
        let k = item_keys(&[item("Section", true), item("Props", false), item("Props", false)]);
        assert_eq!(k[0], None);
        assert_ne!(k[1], k[2]);
        assert_eq!(k[1].as_deref(), Some("Props\u{0}1"));
    }

    #[test]
    fn the_progress_counts_items_ticked_and_ignores_headings_and_strangers() {
        let l = list(vec![item("H", true), item("a", false), item("b", false)]);
        assert_eq!(progress(&l, &[]), (0, 2));
        assert_eq!(progress(&l, &["a\u{0}1".into(), "gone\u{0}1".into()]), (1, 2));
        assert_eq!(progress(&l, &["a\u{0}1".into(), "b\u{0}1".into()]), (2, 2));
    }

    #[test]
    fn a_tick_toggles_and_a_reset_clears_the_list() {
        let mut st = WindowState::default();
        st.toggle("a", "x\u{0}1");
        st.toggle("a", "y\u{0}1");
        assert_eq!(st.ticked("a").len(), 2);
        st.toggle("a", "x\u{0}1");
        assert_eq!(st.ticked("a"), ["y\u{0}1".to_string()]);
        st.reset("a");
        assert!(st.ticked("a").is_empty());
    }

    #[test]
    fn ticks_of_a_reworded_item_or_a_deleted_list_go_with_a_prune() {
        let mut st = WindowState::default();
        st.toggle("a", "old\u{0}1");
        st.toggle("a", "kept\u{0}1");
        st.toggle("gone", "x\u{0}1");
        st.prune(&[list(vec![item("kept", false), item("new", false)])]);
        assert_eq!(st.ticked("a"), ["kept\u{0}1".to_string()]);
        assert!(st.ticked("gone").is_empty());
    }

    #[test]
    fn arming_with_items_open_warns_once_and_only_on_the_transition() {
        let mut st = WindowState::default();
        let l = list(vec![item("a", false), item("b", false)]);
        let s = |armed: bool, connected: bool| json!({"armed": armed, "connected": connected});
        assert_eq!(
            st.arming_warning(&s(true, true), true, Some(&l)),
            None,
            "already armed when first heard"
        );
        assert_eq!(st.arming_warning(&s(false, true), true, Some(&l)), None);
        let w = st.arming_warning(&s(true, true), true, Some(&l));
        assert_eq!(
            w.as_deref(),
            Some("Armed with the preflight checklist open: 0 of 2 items checked.")
        );
        assert_eq!(
            st.arming_warning(&s(true, true), true, Some(&l)),
            None,
            "once per arming"
        );
        st.arming_warning(&s(false, true), true, Some(&l));
        st.toggle("a", "a\u{0}1");
        st.toggle("a", "b\u{0}1");
        assert_eq!(
            st.arming_warning(&s(true, true), true, Some(&l)),
            None,
            "all checked says nothing"
        );
        st.arming_warning(&s(false, true), true, Some(&l));
        assert_eq!(
            st.arming_warning(&s(true, true), false, Some(&l)),
            None,
            "switched off says nothing"
        );
    }

    #[test]
    fn the_window_stays_on_the_map_and_travels_with_the_right_edge() {
        let mut st = WindowState::default();
        let panel = Vec2::new(272.0, 200.0);
        assert_eq!(st.contain(-50.0, 900.0, Vec2::new(800.0, 600.0), panel), (8.0, 392.0));
        assert_eq!(
            st.clamp_loose(5000.0, 5000.0, Vec2::new(800.0, 600.0), panel),
            (736.0, 536.0)
        );
        st.pos = Some((500.0, 100.0));
        st.last_host = (800.0, 600.0);
        st.follow_host(Vec2::new(700.0, 600.0), panel);
        assert_eq!(
            st.pos,
            Some((400.0, 100.0)),
            "nearer the right edge, so it moved with it"
        );
        st.pos = Some((20.0, 100.0));
        st.last_host = (800.0, 600.0);
        st.follow_host(Vec2::new(700.0, 600.0), panel);
        assert_eq!(st.pos, Some((20.0, 100.0)), "nearer the left edge, so it stayed");
    }

    #[test]
    fn what_was_kept_comes_back_from_the_files() {
        let dir = std::env::temp_dir().join(format!("corvus-clw-{}", std::process::id()));
        let (t, p) = (dir.join("t.json"), dir.join("p.json"));
        let _ = std::fs::remove_dir_all(&dir);
        let mut st = WindowState::load(Some(t.clone()), Some(p.clone()));
        st.toggle("a", "x\u{0}1");
        st.pos = Some((12.0, 34.0));
        st.collapsed = true;
        st.save_pos();
        let back = WindowState::load(Some(t), Some(p));
        assert_eq!(back.ticked("a"), ["x\u{0}1".to_string()]);
        assert_eq!((back.pos, back.collapsed), (Some((12.0, 34.0)), true));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
