//! Where a movable window sits on the Home map: the flight HUD, the manual
//! control pad (`hud-panel.js`, `joystick.js`). Dragged by a grip, kept on the
//! map when the map changes size, sent home by a double click, remembered in
//! a file of `~/.corvus` because it belongs to this station.

use std::path::PathBuf;

use eframe::egui::{self, Rect, Vec2};
use serde_json::{json, Value};

/// A window may be dragged until only this much of it is left on the map.
const MIN_VISIBLE: f32 = 64.0;
/// The margin kept when the map moves under a window nobody is touching.
const EDGE: f32 = 8.0;

/// Where one window is, and what else it remembers.
#[derive(Debug, Default)]
pub struct Placement {
    /// From the map's top left; `None` until moved, which is the default
    /// corner.
    pub pos: Option<(f32, f32)>,
    /// Further things the window keeps (pinned, compact, folded).
    pub extra: serde_json::Map<String, Value>,
    drag: Option<Vec2>,
    last_host: (f32, f32),
    last_size: (f32, f32),
    path: Option<PathBuf>,
}

impl Placement {
    /// What the station kept in `~/.corvus/<file>`; an unreadable file is an
    /// empty memory.
    pub fn load(file: &str) -> Placement {
        let path = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".corvus").join(file));
        Placement::from_path(path)
    }

    pub fn from_path(path: Option<PathBuf>) -> Placement {
        let saved: Value = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let mut st = Placement {
            path,
            ..Default::default()
        };
        if let Some(o) = saved.as_object() {
            if let (Some(x), Some(y)) = (o.get("x").and_then(Value::as_f64), o.get("y").and_then(Value::as_f64)) {
                st.pos = Some((x as f32, y as f32));
            }
            // The map the position was measured on, so a window that opens at
            // another size is carried to the same edge instead of floating.
            if let (Some(w), Some(h)) = (o.get("hw").and_then(Value::as_f64), o.get("hh").and_then(Value::as_f64)) {
                if w > 0.0 && h > 0.0 && w < 1.0e5 && h < 1.0e5 {
                    st.last_host = (w as f32, h as f32);
                }
            }
            if let (Some(w), Some(h)) = (o.get("ww").and_then(Value::as_f64), o.get("wh").and_then(Value::as_f64)) {
                if w > 0.0 && h > 0.0 && w < 1.0e5 && h < 1.0e5 {
                    st.last_size = (w as f32, h as f32);
                }
            }
            st.extra = o
                .iter()
                .filter(|(k, _)| !matches!(k.as_str(), "x" | "y" | "hw" | "hh" | "ww" | "wh"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
        }
        st
    }

    pub fn flag(&self, key: &str) -> bool {
        self.extra.get(key) == Some(&Value::Bool(true))
    }

    /// A flag with a default for a window nobody has changed yet.
    pub fn flag_or(&self, key: &str, default: bool) -> bool {
        self.extra.get(key).and_then(Value::as_bool).unwrap_or(default)
    }

    pub fn set_flag(&mut self, key: &str, on: bool) {
        self.extra.insert(key.to_string(), Value::Bool(on));
        self.save();
    }

    pub fn save(&self) {
        let Some(p) = &self.path else { return };
        let mut o = self.extra.clone();
        o.insert("x".into(), json!(self.pos.map(|p| p.0)));
        o.insert("y".into(), json!(self.pos.map(|p| p.1)));
        if self.last_host.0 > 0.0 && self.last_host.1 > 0.0 {
            o.insert("hw".into(), json!(self.last_host.0));
            o.insert("hh".into(), json!(self.last_host.1));
        }
        if self.last_size.0 > 0.0 && self.last_size.1 > 0.0 {
            o.insert("ww".into(), json!(self.last_size.0));
            o.insert("wh".into(), json!(self.last_size.1));
        }
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, Value::Object(o).to_string());
    }

    /// A drag of the window is under way.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Back to the default corner.
    pub fn reset(&mut self) {
        self.pos = None;
        self.save();
    }

    /// `clampPosition`: a deliberate drag may leave only a strip showing, and
    /// never goes above the top edge.
    pub fn clamp_loose(x: f32, y: f32, host: Vec2, size: Vec2) -> (f32, f32) {
        let max_x = (host.x - MIN_VISIBLE).max(0.0);
        let max_y = (host.y - MIN_VISIBLE).max(0.0);
        (
            x.clamp(MIN_VISIBLE - size.x, max_x.max(MIN_VISIBLE - size.x)),
            y.clamp(0.0, max_y),
        )
    }

    /// `contain`: the whole window on the map with an edge of 8 px, where it
    /// fits; an axis it is too large for falls back to the loose clamp.
    pub fn contain(x: f32, y: f32, host: Vec2, size: Vec2) -> (f32, f32) {
        let loose = Placement::clamp_loose(x, y, host, size);
        let (max_x, max_y) = (host.x - size.x - EDGE, host.y - size.y - EDGE);
        (
            if max_x >= EDGE { x.clamp(EDGE, max_x) } else { loose.0 },
            if max_y >= EDGE { y.clamp(EDGE, max_y) } else { loose.1 },
        )
    }

    /// The window itself changed size (the readouts were hidden, the panel was
    /// folded). It keeps the edge it is nearer to, so a window resting on the
    /// bottom stays on the bottom instead of shrinking away from it. The size
    /// is remembered with the position, so a window saved at one size and
    /// opened at another is carried the same way.
    pub fn follow_size(&mut self, host: Vec2, size: Vec2) {
        if host.x <= 0.0 || host.y <= 0.0 {
            return;
        }
        let old = self.last_size;
        self.last_size = (size.x, size.y);
        let Some((x, y)) = self.pos else { return };
        if old.0 <= 0.0 || old.1 <= 0.0 || old == self.last_size {
            return;
        }
        let (mut nx, mut ny) = (x, y);
        if (host.x - (x + old.0)) < x {
            nx += old.0 - size.x;
        }
        if (host.y - (y + old.1)) < y {
            ny += old.1 - size.y;
        }
        self.pos = Some(Placement::contain(nx, ny, host, size));
        self.save();
    }

    /// `onHostResize`: the map changed size, usually because the right panel
    /// slid. A window nearer the right (or bottom) edge travels with it, then
    /// the whole window is contained. A map with no size says nothing.
    pub fn follow_host(&mut self, host: Vec2, size: Vec2) {
        let now = (host.x, host.y);
        if now.0 <= 0.0 || now.1 <= 0.0 {
            return;
        }
        if let Some((x, y)) = self.pos {
            if self.last_host.0 > 0.0 && now != self.last_host {
                let (mut nx, mut ny) = (x, y);
                if now.0 != self.last_host.0 && (self.last_host.0 - (x + size.x)) < x {
                    nx += now.0 - self.last_host.0;
                }
                if now.1 != self.last_host.1 && (self.last_host.1 - (y + size.y)) < y {
                    ny += now.1 - self.last_host.1;
                }
                self.pos = Some(Placement::contain(nx, ny, host, size));
            }
        }
        self.last_host = now;
    }

    /// The window's rectangle on screen: where it was put, or `default`
    /// (from the map's top left) until it was.
    pub fn rect(&mut self, host: Rect, size: Vec2, default: Vec2) -> Rect {
        self.follow_host(host.size(), size);
        self.follow_size(host.size(), size);
        let (x, y) = match self.pos {
            Some((x, y)) => Placement::clamp_loose(x, y, host.size(), size),
            None => (default.x, default.y),
        };
        Rect::from_min_size(host.min + Vec2::new(x, y), size)
    }

    /// Drags the window by `grip`: a press and move on it moves the window,
    /// a double click sends it home. `button` is a rectangle inside the grip
    /// that is not the grip (a collapse button): it is left alone. Returns
    /// true when the position changed or was reset.
    pub fn drag(&mut self, ui: &egui::Ui, id: egui::Id, grip: Rect, window: Rect, host: Rect) -> bool {
        let resp = ui.interact(grip, id, egui::Sense::click_and_drag());
        let mut changed = false;
        if resp.hovered() || self.drag.is_some() {
            ui.ctx().set_cursor_icon(if self.drag.is_some() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
        }
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.drag = Some(p - window.min);
            }
        }
        if let (Some(grab), true) = (self.drag, resp.dragged()) {
            if let Some(p) = resp.interact_pointer_pos() {
                let at = p - grab - host.min;
                self.pos = Some(Placement::clamp_loose(at.x, at.y, host.size(), window.size()));
                changed = true;
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
            self.save();
        }
        if resp.double_clicked() {
            self.reset();
            changed = true;
        }
        changed
    }
}

/// What the interface keeps for this station in `~/.corvus`: window places,
/// the checklist ticks, the tablet mode and the sensor list. A factory reset
/// removes them with the settings.
pub const STATION_FILES: [&str; 6] = [
    "hud.json",
    "joystick.json",
    "checklist-ticks.json",
    "checklist-window.json",
    "gui-touch.json",
    "safety-extra.json",
];

/// Removes [`STATION_FILES`]; a missing file is already gone. Returns the
/// names that could not be removed.
pub fn remove_station_files() -> Vec<&'static str> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let dir = PathBuf::from(home).join(".corvus");
    STATION_FILES
        .iter()
        .copied()
        .filter(|name| match std::fs::remove_file(dir.join(name)) {
            Ok(()) => false,
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        })
        .collect()
}

/// Where the window rests until moved: `from_right` and `from_bottom` are
/// the gaps to the map's right and bottom edge.
pub fn corner(
    host: Rect,
    size: Vec2,
    from_left: Option<f32>,
    from_right: Option<f32>,
    top: Option<f32>,
    from_bottom: Option<f32>,
) -> Vec2 {
    let x = match (from_left, from_right) {
        (Some(l), _) => l,
        (None, Some(r)) => host.width() - size.x - r,
        _ => 0.0,
    };
    let y = match (top, from_bottom) {
        (Some(t), _) => t,
        (None, Some(b)) => host.height() - size.y - b,
        _ => 0.0,
    };
    Vec2::new(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Pos2;

    const HOST: Vec2 = Vec2::new(1000.0, 700.0);
    const WIN: Vec2 = Vec2::new(200.0, 100.0);

    #[test]
    fn a_drag_may_leave_a_strip_showing_but_never_the_top() {
        assert_eq!(Placement::clamp_loose(-500.0, -20.0, HOST, WIN), (64.0 - 200.0, 0.0));
        assert_eq!(Placement::clamp_loose(5000.0, 5000.0, HOST, WIN), (936.0, 636.0));
        assert_eq!(Placement::clamp_loose(300.0, 300.0, HOST, WIN), (300.0, 300.0));
    }

    #[test]
    fn containing_keeps_the_whole_window_with_an_edge() {
        assert_eq!(Placement::contain(-50.0, -50.0, HOST, WIN), (8.0, 8.0));
        assert_eq!(Placement::contain(5000.0, 5000.0, HOST, WIN), (792.0, 592.0));
        // Too large for the map: the loose rule.
        let small = Vec2::new(150.0, 90.0);
        assert_eq!(
            Placement::contain(100.0, 40.0, small, WIN).0,
            Placement::clamp_loose(100.0, 40.0, small, WIN).0
        );
    }

    #[test]
    fn a_window_near_the_right_edge_travels_with_it() {
        let mut p = Placement {
            pos: Some((780.0, 100.0)),
            ..Default::default()
        };
        p.follow_host(HOST, WIN);
        assert_eq!(p.pos, Some((780.0, 100.0)));
        // The right panel slides open: the map is 300 narrower.
        p.follow_host(Vec2::new(700.0, 700.0), WIN);
        assert_eq!(p.pos, Some((480.0, 100.0)));
    }

    #[test]
    fn a_window_near_the_left_edge_stays_put() {
        let mut p = Placement {
            pos: Some((40.0, 100.0)),
            ..Default::default()
        };
        p.follow_host(HOST, WIN);
        p.follow_host(Vec2::new(700.0, 700.0), WIN);
        assert_eq!(p.pos, Some((40.0, 100.0)));
    }

    #[test]
    fn a_map_with_no_size_changes_nothing() {
        let mut p = Placement {
            pos: Some((780.0, 100.0)),
            ..Default::default()
        };
        p.follow_host(HOST, WIN);
        p.follow_host(Vec2::ZERO, WIN);
        p.follow_host(HOST, WIN);
        assert_eq!(p.pos, Some((780.0, 100.0)));
    }

    #[test]
    fn it_is_remembered_in_a_file() {
        let dir = std::env::temp_dir().join(format!("corvus-mapwindow-{}", std::process::id()));
        let path = dir.join("pad.json");
        let mut p = Placement::from_path(Some(path.clone()));
        assert_eq!(p.pos, None);
        p.pos = Some((12.5, 40.0));
        p.set_flag("collapsed", true);
        let q = Placement::from_path(Some(path));
        assert_eq!(q.pos, Some((12.5, 40.0)));
        assert!(q.flag("collapsed") && !q.flag("pinned"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_window_saved_on_a_tall_map_follows_the_bottom_edge_on_a_short_one() {
        let dir = std::env::temp_dir().join(format!("corvus-mapwindow-host-{}", std::process::id()));
        let path = dir.join("hud.json");
        let mut p = Placement::from_path(Some(path.clone()));
        p.pos = Some((8.0, 500.0));
        p.follow_host(Vec2::new(1000.0, 700.0), WIN);
        p.save();
        let mut q = Placement::from_path(Some(path));
        assert_eq!(q.pos, Some((8.0, 500.0)));
        q.follow_host(Vec2::new(1000.0, 400.0), WIN);
        assert_eq!(q.pos, Some((8.0, 200.0)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_window_resting_on_the_bottom_stays_there_when_it_gets_shorter() {
        let mut p = Placement::from_path(None);
        p.pos = Some((8.0, 492.0));
        p.follow_size(HOST, Vec2::new(200.0, 200.0));
        assert_eq!(p.pos, Some((8.0, 492.0)));
        p.follow_size(HOST, Vec2::new(200.0, 100.0));
        assert_eq!(p.pos, Some((8.0, 592.0)), "the bottom edge stays");
        p.follow_size(HOST, Vec2::new(200.0, 200.0));
        assert_eq!(p.pos, Some((8.0, 492.0)), "and again when it grows");
        let mut top = Placement::from_path(None);
        top.pos = Some((8.0, 20.0));
        top.follow_size(HOST, Vec2::new(200.0, 200.0));
        top.follow_size(HOST, Vec2::new(200.0, 100.0));
        assert_eq!(top.pos, Some((8.0, 20.0)), "a window near the top keeps its top");
    }

    #[test]
    fn a_saved_size_carries_the_window_to_the_edge_at_the_next_start() {
        let dir = std::env::temp_dir().join(format!("corvus-mapwindow-size-{}", std::process::id()));
        let path = dir.join("hud.json");
        let mut p = Placement::from_path(Some(path.clone()));
        p.pos = Some((8.0, 492.0));
        p.follow_size(HOST, Vec2::new(200.0, 200.0));
        p.save();
        let mut q = Placement::from_path(Some(path));
        q.follow_size(HOST, Vec2::new(200.0, 100.0));
        assert_eq!(q.pos, Some((8.0, 592.0)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_default_corner_is_measured_from_the_edges() {
        let host = Rect::from_min_size(Pos2::new(70.0, 80.0), HOST);
        assert_eq!(
            corner(host, WIN, Some(16.0), None, None, Some(16.0)),
            Vec2::new(16.0, 584.0)
        );
        assert_eq!(
            corner(host, WIN, None, Some(16.0), None, Some(16.0)),
            Vec2::new(784.0, 584.0)
        );
    }
}
