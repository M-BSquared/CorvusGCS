//! The camera button of the map rail and its list (`wireCameraButton` and
//! `renderCameraRows` of `python/src/js/map.js`).
//!
//! The button is there once a camera is set up. A press opens or closes the
//! camera picked last, or all of them when none was picked; with more than
//! one camera, pointing at the button reveals the list, where each camera is
//! a row that opens or closes its window, with Open all or Close all under
//! them. Which camera the press opens is this operator's habit, not the
//! vehicle's, so it is kept beside the station (`~/.corvus/gui-last-camera`)
//! and not in the config.

use std::path::PathBuf;

use eframe::egui::{self, Rect};
use serde_json::Value;

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::ui::menu;

/// Seconds between reads of the camera list.
const REFRESH: f64 = 5.0;
/// `THREE_D_HOVER_OPEN_MS` and `THREE_D_HOVER_CLOSE_MS`.
const OPEN_AFTER: f64 = 0.22;
const CLOSE_AFTER: f64 = 0.32;

/// One camera of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct Camera {
    pub id: String,
    pub name: String,
    pub address: String,
}

/// The cameras of a `GET /api/video/status` answer.
pub fn parse(status: &Value) -> Vec<Camera> {
    status
        .get("streams")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|s| {
                    let id = s.get("id").and_then(Value::as_str).filter(|i| !i.is_empty())?;
                    let t = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    Some(Camera {
                        id: id.to_string(),
                        name: t("name"),
                        address: t("address"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What a camera is called: its name, else its address, else "Camera".
pub fn label(c: &Camera) -> String {
    if !c.name.is_empty() {
        c.name.clone()
    } else if !c.address.is_empty() {
        c.address.clone()
    } else {
        "Camera".to_string()
    }
}

/// The camera picked last, or the first one when that one is gone.
pub fn last_camera<'a>(list: &'a [Camera], remembered: &str) -> Option<&'a Camera> {
    list.iter().find(|c| c.id == remembered).or_else(|| list.first())
}

/// The button's tooltip: "Camera: Front", or the hint with none set up.
pub fn button_title(list: &[Camera], remembered: &str) -> String {
    match last_camera(list, remembered) {
        Some(c) => format!("Camera: {}", label(c)),
        None => "Camera windows (set up under Setup, Video)".to_string(),
    }
}

/// What the operator asked of the windows.
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    /// Open this camera's window, or close it when it is open.
    Toggle(Value),
    OpenAll,
    CloseAll,
}

/// The list as the rail keeps it.
#[derive(Default)]
pub struct Cameras {
    pub list: Vec<Camera>,
    raw: Vec<Value>,
    load: Option<Pending<Result<Value, String>>>,
    asked: Option<f64>,
    remembered: String,
    path: Option<PathBuf>,
    loaded_memory: bool,
    hover: Option<f64>,
    leave: Option<f64>,
    pub list_open: bool,
    cmds: Vec<Cmd>,
}

impl Cameras {
    fn memory_path() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".corvus").join("gui-last-camera"))
    }

    /// Reads the list now and then; follows what the Video page changes.
    pub fn poll(&mut self, ctx: &egui::Context, server: &Server, now: f64) {
        if !self.loaded_memory {
            self.loaded_memory = true;
            self.path = Cameras::memory_path();
            self.remembered = self
                .path
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default()
                .trim()
                .to_string();
        }
        if let Some(Ok(v)) = self.load.as_ref().and_then(Pending::take) {
            self.load = None;
            self.raw = v.get("streams").and_then(Value::as_array).cloned().unwrap_or_default();
            self.list = parse(&v);
        }
        if self.load.is_none() && self.asked.is_none_or(|t| now - t >= REFRESH) {
            self.asked = Some(now);
            let s = server.clone();
            self.load = Some(Pending::spawn(ctx, move || http::get_json(&s, "/api/video/status")));
        }
    }

    /// The camera's raw record, for opening its window.
    fn raw_of(&self, id: &str) -> Option<Value> {
        self.raw
            .iter()
            .find(|s| s.get("id").and_then(Value::as_str) == Some(id))
            .cloned()
    }

    fn remember(&mut self, id: &str) {
        self.remembered = id.to_string();
        if let Some(p) = &self.path {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, id);
        }
    }

    pub fn title(&self) -> String {
        button_title(&self.list, &self.remembered)
    }

    /// A press of the button: the camera picked last, or all of them.
    pub fn press(&mut self) {
        let cmd = match last_camera(&self.list, &self.remembered).and_then(|c| self.raw_of(&c.id)) {
            Some(raw) => Cmd::Toggle(raw),
            None => Cmd::OpenAll,
        };
        self.cmds.push(cmd);
    }

    /// Opens one window per camera.
    pub fn open_all(
        &self,
        ctx: &egui::Context,
        server: &Server,
        windows: &mut crate::setup::video_window::VideoWindows,
    ) {
        for raw in &self.raw {
            windows.open(ctx, server, raw);
        }
    }

    /// What the operator asked since the last call.
    pub fn take_cmds(&mut self) -> Vec<Cmd> {
        std::mem::take(&mut self.cmds)
    }

    /// Pointing at the button reveals the list; leaving it, and the list,
    /// hides it a moment later so the way across the gap is not lost.
    pub fn hover(&mut self, over_button: bool, over_list: bool, now: f64, ctx: &egui::Context) {
        if self.list.len() < 2 {
            self.list_open = false;
            return;
        }
        if over_button || over_list {
            self.leave = None;
            let since = *self.hover.get_or_insert(now);
            if !self.list_open {
                if now - since >= OPEN_AFTER {
                    self.list_open = true;
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(60));
                }
            }
        } else {
            self.hover = None;
            if self.list_open {
                let since = *self.leave.get_or_insert(now);
                if now - since >= CLOSE_AFTER {
                    self.list_open = false;
                    self.leave = None;
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(60));
                }
            }
        }
    }

    /// The list to the left of `anchor`, the rail's button. Returns where it
    /// is, so the pointer on it keeps it open.
    pub fn menu(&mut self, ui: &egui::Ui, anchor: Rect, open_ids: &[String]) -> Option<Rect> {
        if !self.list_open || self.list.len() < 2 {
            return None;
        }
        let items = camera_items(&self.list, open_ids);
        let id = ui.scope_id().with("camera-list");
        let out = menu::Menu::new(id, menu::Anchor::beside_rail(anchor), &menu::Look::CAMERAS, &items).show(ui.ctx());
        if let Some(value) = out.picked {
            if value == ALL {
                self.cmds.push(if open_ids.is_empty() {
                    Cmd::OpenAll
                } else {
                    Cmd::CloseAll
                });
            } else if let Some(raw) = self.raw_of(&value) {
                self.remember(&value);
                self.cmds.push(Cmd::Toggle(raw));
            }
        }
        out.rect
    }
}

/// The value of the Open all or Close all row.
const ALL: &str = "\u{0}all";

/// `renderCameraRows`: the caption, a dot row per camera (lit while its
/// window is open, the address under a name), then Open all or Close all.
fn camera_items(list: &[Camera], open_ids: &[String]) -> Vec<menu::Item> {
    let mut items = vec![menu::Item::Head("Cameras".into())];
    for c in list {
        let note = if !c.name.is_empty() && !c.address.is_empty() {
            c.address.clone()
        } else {
            String::new()
        };
        items.push(
            menu::Row::new(c.id.clone(), label(c))
                .note(note)
                .on(open_ids.contains(&c.id))
                .into(),
        );
    }
    let any = !open_ids.is_empty();
    items.push(
        menu::Row::new(ALL, if any { "Close all" } else { "Open all" })
            .icon(if any { "x" } else { "layout-grid" })
            .into(),
    );
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn list() -> Vec<Camera> {
        parse(&json!({"streams": [
            {"id": "a", "name": "Front", "address": "rtsp://10.0.0.2/live"},
            {"id": "b", "name": "", "address": "rtsp://10.0.0.3/live"},
            {"id": "", "name": "no id"},
            {"name": "no id either"}
        ]}))
    }

    #[test]
    fn only_cameras_with_an_id_are_listed() {
        let l = list();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].name, "Front");
        assert!(parse(&json!({})).is_empty());
    }

    #[test]
    fn a_camera_is_called_by_name_then_address() {
        let l = list();
        assert_eq!(label(&l[0]), "Front");
        assert_eq!(label(&l[1]), "rtsp://10.0.0.3/live");
        assert_eq!(
            label(&Camera {
                id: "c".into(),
                name: String::new(),
                address: String::new()
            }),
            "Camera"
        );
    }

    #[test]
    fn the_press_opens_the_camera_picked_last_or_the_first() {
        let l = list();
        assert_eq!(last_camera(&l, "b").map(|c| c.id.as_str()), Some("b"));
        assert_eq!(last_camera(&l, "gone").map(|c| c.id.as_str()), Some("a"));
        assert_eq!(last_camera(&l, "").map(|c| c.id.as_str()), Some("a"));
        assert!(last_camera(&[], "a").is_none());
        assert_eq!(button_title(&l, "b"), "Camera: rtsp://10.0.0.3/live");
        assert_eq!(button_title(&[], ""), "Camera windows (set up under Setup, Video)");
    }

    #[test]
    fn a_press_with_a_camera_toggles_it_and_with_none_asks_for_all() {
        let mut c = Cameras::default();
        c.press();
        assert_eq!(c.take_cmds(), vec![Cmd::OpenAll]);
        c.raw = vec![json!({"id": "a", "name": "Front"})];
        c.list = parse(&json!({"streams": c.raw}));
        c.press();
        assert_eq!(c.take_cmds(), vec![Cmd::Toggle(json!({"id": "a", "name": "Front"}))]);
        assert!(c.take_cmds().is_empty());
    }

    #[test]
    fn the_list_opens_after_a_pause_and_closes_after_a_pause() {
        let ctx = egui::Context::default();
        let mut c = Cameras {
            list: list(),
            ..Default::default()
        };
        c.hover(true, false, 1.0, &ctx);
        assert!(!c.list_open);
        c.hover(true, false, 1.3, &ctx);
        assert!(c.list_open);
        c.hover(false, false, 2.0, &ctx);
        assert!(c.list_open, "not at once");
        c.hover(false, true, 2.1, &ctx);
        c.hover(false, false, 2.2, &ctx);
        c.hover(false, false, 2.6, &ctx);
        assert!(!c.list_open);
        // One camera has no list to reveal.
        c.list.truncate(1);
        c.hover(true, false, 5.0, &ctx);
        c.hover(true, false, 5.5, &ctx);
        assert!(!c.list_open);
    }

    #[test]
    fn the_camera_list_is_a_caption_dot_rows_and_open_or_close_all() {
        let l = list();
        let none = camera_items(&l, &[]);
        assert_eq!(none[0], menu::Item::Head("Cameras".into()));
        assert!(matches!(&none[1], menu::Item::Row(r) if r.note == "rtsp://10.0.0.2/live" && !r.on));
        assert!(
            matches!(&none[2], menu::Item::Row(r) if r.note.is_empty()),
            "an address is not its own note"
        );
        assert!(matches!(&none[3], menu::Item::Row(r) if r.label == "Open all" && r.icon == Some("layout-grid")));
        let some = camera_items(&l, &["b".to_string()]);
        assert!(matches!(&some[2], menu::Item::Row(r) if r.on));
        assert!(matches!(&some[3], menu::Item::Row(r) if r.label == "Close all" && r.icon == Some("x")));
    }
}
