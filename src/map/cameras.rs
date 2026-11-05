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

use eframe::egui::{self, Pos2, Rect, Vec2};
use serde_json::Value;

use crate::net::bg::Pending;
use crate::net::http::{self, Server};
use crate::ui::icons;
use crate::ui::kit;
use crate::ui::menu;
use crate::ui::style::{self, Weight};

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
        let head = style::galley(ui, "CAMERAS", 10.0, Weight::W600, style::text_3(), 0.8);
        let any = !open_ids.is_empty();
        let rows: Vec<(String, String, bool)> = self
            .list
            .iter()
            .map(|c| {
                let note = if !c.name.is_empty() && !c.address.is_empty() {
                    c.address.clone()
                } else {
                    String::new()
                };
                (label(c), note, open_ids.contains(&c.id))
            })
            .collect();
        let galleys: Vec<_> = rows
            .iter()
            .map(|(l, n, _)| {
                (
                    style::galley(ui, l, 12.0, Weight::W500, style::text_1(), 0.0),
                    style::galley(ui, n, 10.5, Weight::W400, style::text_3(), 0.0),
                )
            })
            .collect();
        let all_label = if any { "Close all" } else { "Open all" };
        let all_g = style::galley(ui, all_label, 12.0, Weight::W500, style::text_1(), 0.0);
        let widest = galleys
            .iter()
            .map(|(l, n)| l.size().x.max(n.size().x))
            .fold(all_g.size().x + 22.0, f32::max);
        let width = (menu::PAD * 2.0 + 8.0 + 15.0 + widest + 8.0).clamp(150.0, 260.0);
        let row_h = |note: &str, l: &egui::Galley, n: &egui::Galley| {
            7.0 * 2.0 + l.size().y + if note.is_empty() { 0.0 } else { n.size().y + 1.0 }
        };
        let heights: Vec<f32> = rows
            .iter()
            .zip(&galleys)
            .map(|((_, note, _), (l, n))| row_h(note, l, n))
            .collect();
        let all_h = 7.0 * 2.0 + all_g.size().y;
        let height =
            menu::PAD * 2.0 + head.size().y + 10.0 + heights.iter().sum::<f32>() + 2.0 * heights.len() as f32 + all_h;
        let rect = Rect::from_min_size(
            Pos2::new(anchor.left() - 14.0 - width, anchor.center().y - height / 2.0),
            Vec2::new(width, height),
        );
        let screen = ui.ctx().content_rect();
        let rect = rect.translate(Vec2::new(0.0, (screen.top() + 8.0 - rect.top()).max(0.0)));
        let mut picked: Option<Cmd> = None;
        let mut remember: Option<String> = None;
        egui::Area::new(ui.scope_id().with("camera-list"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ui.ctx(), |ui| {
                let painter = ui.painter().clone();
                style::glass_panel(&painter, "cameras", rect, style::RADIUS, 0.6);
                ui.allocate_rect(rect, egui::Sense::hover());
                let inner = rect.shrink(menu::PAD);
                painter.galley(
                    Pos2::new(inner.left() + 6.0, inner.top() + 4.0),
                    head.clone(),
                    style::text_3(),
                );
                let mut y = inner.top() + head.size().y + 10.0;
                for (i, ((label, _, on), (l, n))) in rows.iter().zip(&galleys).enumerate() {
                    let r = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), heights[i]));
                    let resp = ui.interact(r, ui.scope_id().with(("cam", i)), egui::Sense::click());
                    if *on {
                        painter.rect_filled(r, style::RADIUS_BTN, style::accent_dark());
                    } else if resp.hovered() {
                        painter.rect_filled(r, style::RADIUS_BTN, style::tint(0.06));
                    }
                    let dot = if *on { style::accent() } else { style::text_3() };
                    kit::dot(&painter, Pos2::new(r.left() + 8.0 + 3.5, r.center().y), dot, *on);
                    let tx = r.left() + 8.0 + 7.0 + 8.0;
                    let block = l.size().y + if rows[i].1.is_empty() { 0.0 } else { n.size().y + 1.0 };
                    let top = r.center().y - block / 2.0;
                    let color = if *on || resp.hovered() {
                        style::text_1()
                    } else {
                        style::text_2()
                    };
                    let clip = painter.with_clip_rect(Rect::from_min_max(
                        Pos2::new(tx, r.top()),
                        Pos2::new(r.right() - 8.0, r.bottom()),
                    ));
                    clip.galley(Pos2::new(tx, top), l.clone(), color);
                    if !rows[i].1.is_empty() {
                        clip.galley(Pos2::new(tx, top + l.size().y + 1.0), n.clone(), style::text_3());
                    }
                    let _ = label;
                    if resp.clicked() {
                        remember = Some(self.list[i].id.clone());
                        if let Some(raw) = self.raw_of(&self.list[i].id) {
                            picked = Some(Cmd::Toggle(raw));
                        }
                    }
                    y += heights[i] + 2.0;
                }
                let r = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), all_h));
                let resp = ui.interact(r, ui.scope_id().with("camall"), egui::Sense::click());
                if resp.hovered() {
                    painter.rect_filled(r, style::RADIUS_BTN, style::tint(0.06));
                }
                icons::paint(
                    ui,
                    if any { "x" } else { "layout-grid" },
                    Rect::from_center_size(Pos2::new(r.left() + 8.0 + 7.0, r.center().y), Vec2::splat(14.0)),
                    if resp.hovered() {
                        style::accent()
                    } else {
                        style::text_2()
                    },
                );
                painter.galley(
                    Pos2::new(r.left() + 8.0 + 14.0 + 8.0, r.center().y - all_g.size().y / 2.0),
                    all_g.clone(),
                    if resp.hovered() {
                        style::text_1()
                    } else {
                        style::text_2()
                    },
                );
                if resp.clicked() {
                    picked = Some(if any { Cmd::CloseAll } else { Cmd::OpenAll });
                }
            });
        if let Some(id) = remember {
            self.remember(&id);
        }
        if let Some(c) = picked {
            self.cmds.push(c);
        }
        Some(rect)
    }
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
}
