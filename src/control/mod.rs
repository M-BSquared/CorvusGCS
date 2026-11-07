//! Manual control: the pad over the Home map (`python/src/js/joystick.js`,
//! `.joystick-pad` of `map.css`) with the stick pair, the gamepad block and
//! the two key clusters, all feeding one MANUAL_CONTROL stream.
//!
//! Every surface is off until Settings, Controls turns it on, and nothing
//! streams without one. The stream is a thread of its own that posts the
//! frame at 20 Hz while an axis is off centre and 5 Hz at neutral (a gap in
//! the stream is what PX4 reads as RC loss), one frame at a time. The
//! keyboard starts locked: Shift + W twice opens it, Escape locks it again.

mod draw;
pub mod model;

use crate::ui::tip::HoverTr;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::home::mapwindow::{self, Placement};
use crate::net::http::{self, Server};
use crate::net::lock::Relock;
use crate::ui::icons;
use crate::ui::style::{self, Weight};
use model::{ArmGesture, Config, Frame, Held, Inputs, Lock, PadRead, Stick};

const SIZE: f32 = 76.0;
const KNOB: f32 = 28.0;
const KEY: f32 = 26.0;
const GP_RING: f32 = 30.0;
const GP_DOT: f32 = 10.0;
const GP_W: f32 = 104.0;

/// A key cluster: its caption and its four keys (direction, letter, icon).
type Cluster = (&'static str, [(&'static str, &'static str, Option<&'static str>); 4]);

/// What the stream thread shares with the window.
#[derive(Default)]
struct Shared {
    frame: Option<Frame>,
    streaming: bool,
    connected: bool,
    stop: bool,
}

type Link = Arc<(Mutex<Shared>, Condvar)>;

/// Posts the frame as long as there is one; a failure is silent, because a
/// stream that raised a toast per dropped frame would bury every other note.
fn run_stream(server: Server, link: Link) {
    let (lock, cv) = &*link;
    let mut guard = lock.relock();
    loop {
        if guard.stop {
            return;
        }
        let period = if guard.frame.is_some_and(|f| f.is_neutral()) {
            1.0 / model::IDLE_HZ
        } else {
            1.0 / model::ACTIVE_HZ
        };
        let (g, _) = cv
            .wait_timeout(guard, Duration::from_secs_f64(period))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard = g;
        if guard.stop {
            return;
        }
        if let (true, true, Some(f)) = (guard.streaming, guard.connected, guard.frame) {
            drop(guard);
            let _ = http::post_json(&server, "/api/mavlink/manual", &f.body());
            guard = lock.relock();
        }
    }
}

fn or_held(a: Held, b: Held) -> Held {
    Held {
        up: a.up || b.up,
        down: a.down || b.down,
        left: a.left || b.left,
        right: a.right || b.right,
        thrust_up: a.thrust_up || b.thrust_up,
        thrust_down: a.thrust_down || b.thrust_down,
        yaw_left: a.yaw_left || b.yaw_left,
        yaw_right: a.yaw_right || b.yaw_right,
    }
}

/// The pad and what drives it.
pub struct Control {
    server: Server,
    pub cfg: Config,
    link: Link,
    thread: Option<std::thread::JoinHandle<()>>,
    lock: Lock,
    left: Stick,
    right: Stick,
    kbd: Held,
    pointer: Held,
    boost: bool,
    gesture: ArmGesture,
    pad: Option<[f64; 4]>,
    pad_left: Stick,
    pad_right: Stick,
    shown_left: Stick,
    shown_right: Stick,
    stick_mode_after_arm: bool,
    mode_requested_at: f64,
    place: Placement,
    connected: bool,
    armed: bool,
    mode: String,
    caps: crate::data::capabilities::Capabilities,
    frame: Frame,
    notes: Arc<Mutex<Vec<String>>>,
}

impl Control {
    pub fn new(server: Server, cfg: Config) -> Control {
        let link: Link = Arc::default();
        let thread = {
            let (server, link) = (server.clone(), link.clone());
            std::thread::Builder::new()
                .name("manual-control".into())
                .spawn(move || run_stream(server, link))
                .ok()
        };
        Control {
            server,
            cfg,
            link,
            thread,
            lock: Lock::default(),
            left: Stick::CENTRE,
            right: Stick::CENTRE,
            kbd: Held::default(),
            pointer: Held::default(),
            boost: false,
            gesture: ArmGesture::default(),
            pad: None,
            pad_left: Stick::CENTRE,
            pad_right: Stick::CENTRE,
            shown_left: Stick::CENTRE,
            shown_right: Stick::CENTRE,
            stick_mode_after_arm: false,
            mode_requested_at: 0.0,
            place: Placement::load("joystick.json"),
            connected: false,
            armed: false,
            mode: String::new(),
            caps: Default::default(),
            frame: Frame::NEUTRAL,
            notes: Arc::default(),
        }
    }

    /// The config changed. A fresh switch is a fresh start: the keyboard
    /// locks and nothing stays pinned.
    pub fn set_config(&mut self, cfg: Config) {
        if cfg == self.cfg {
            return;
        }
        let keys_changed = cfg.keys != self.cfg.keys || cfg.wasd != self.cfg.wasd;
        if cfg.gamepad != self.cfg.gamepad {
            self.gesture = ArmGesture::default();
            self.stick_mode_after_arm = false;
            self.pad_left = Stick::CENTRE;
            self.pad_right = Stick::CENTRE;
        }
        self.cfg = cfg;
        if keys_changed {
            self.lock.set(true);
        }
        self.release_all();
    }

    fn release_all(&mut self) {
        self.left = Stick::CENTRE;
        self.right = Stick::CENTRE;
        self.kbd = Held::default();
        self.pointer = Held::default();
        self.boost = false;
    }

    /// Hands a gamepad's four raw axes (left x, left y, right x, right y), or
    /// None when none is plugged in.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn feed_gamepad(&mut self, axes: Option<[f64; 4]>) {
        self.pad = axes;
    }

    /// Errors for the board, from the requests the pad makes.
    pub fn take_notes(&mut self) -> Vec<String> {
        self.notes
            .lock()
            .map(|mut n| std::mem::take(&mut *n))
            .unwrap_or_default()
    }

    pub fn shut_down(&mut self) {
        {
            let (lock, cv) = &*self.link;
            if let Ok(mut g) = lock.lock() {
                g.stop = true;
            }
            cv.notify_all();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn keys_live(&self, ctx: &egui::Context, visible: bool) -> bool {
        self.cfg.keyboard() && !self.collapsed() && visible && !ctx.egui_wants_keyboard_input()
    }

    fn collapsed(&self) -> bool {
        self.place.flag("collapsed")
    }

    /// The sticks only matter in a mode that reads them. Asked for from a
    /// fresh press, never from the stream, and throttled so a refusal is one
    /// note per attempt. The target comes from the capabilities document.
    fn ensure_stick_mode(&mut self, now: f64) {
        if !self.connected || !self.armed || now - self.mode_requested_at < model::MODE_RETRY_MS {
            return;
        }
        self.mode_requested_at = now;
        let (server, notes, mode) = (self.server.clone(), self.notes.clone(), self.mode.clone());
        let caps = self.caps.clone();
        std::thread::spawn(move || {
            let target = caps.stick_mode.clone();
            if target.is_empty() || caps.is_stick_mode(&mode) {
                return;
            }
            let answer = http::post_json(&server, "/api/mavlink/mode", &json!({"mode": target}));
            let failure = match answer {
                Ok(d) if d.get("ok") == Some(&Value::Bool(true)) => None,
                Ok(d) => Some(
                    d.get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Mode change for manual flight rejected")
                        .to_string(),
                ),
                Err(e) if e.is_empty() => Some("Mode change for manual flight rejected".to_string()),
                Err(e) => Some(e),
            };
            if let (Some(f), Ok(mut n)) = (failure, notes.lock()) {
                n.push(f);
            }
        });
    }

    fn arm(&self, ctx: &egui::Context) {
        let (server, notes, ctx) = (self.server.clone(), self.notes.clone(), ctx.clone());
        std::thread::spawn(move || {
            let answer = http::post_json(&server, "/api/mavlink/arm", &json!({"arm": true}));
            let failure = match answer {
                Ok(d) if d.get("ok") == Some(&Value::Bool(true)) => None,
                Ok(d) => Some(
                    d.get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Arm failed")
                        .to_string(),
                ),
                Err(e) if e.is_empty() => Some("Arm failed".to_string()),
                Err(e) => Some(e),
            };
            if let (Some(f), Ok(mut n)) = (failure, notes.lock()) {
                n.push(f);
            }
            ctx.request_repaint();
        });
    }

    /// One step, on every page: the keyboard, the gamepad, the frame and the
    /// stream. `home` is whether the map is the page on screen.
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        state: &Value,
        caps: &crate::data::capabilities::Capabilities,
        home: bool,
        now_ms: f64,
    ) {
        self.caps = caps.clone();
        self.connected = state.get("connected") == Some(&Value::Bool(true));
        self.armed = state.get("armed") == Some(&Value::Bool(true));
        self.mode = state.get("mode").and_then(Value::as_str).unwrap_or("").to_string();
        if !self.connected {
            self.stick_mode_after_arm = false;
        }
        if self.stick_mode_after_arm && self.armed {
            self.stick_mode_after_arm = false;
            self.mode_requested_at = f64::NEG_INFINITY;
            self.ensure_stick_mode(now_ms);
        }

        let live = self.keys_live(ctx, home);
        let (events, focused, shift) = ctx.input(|i| (i.events.clone(), i.focused, i.modifiers.shift));
        if !focused || !self.cfg.streaming() {
            // A window that lost focus must not leave a key pinned with the
            // stream still sending it.
            self.kbd = Held::default();
            self.boost = false;
        } else if live {
            for e in &events {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } = e
                {
                    if self.lock.locked && *key == egui::Key::W && modifiers.shift && self.lock.shift_w(now_ms) {
                        self.mode_requested_at = f64::NEG_INFINITY;
                        self.ensure_stick_mode(now_ms);
                    } else if !self.lock.locked && *key == egui::Key::Escape {
                        self.lock.set(true);
                        self.kbd = Held::default();
                        self.boost = false;
                    } else if !self.lock.locked {
                        self.ensure_stick_mode(now_ms);
                    }
                }
            }
            if self.lock.locked {
                self.kbd = Held::default();
                self.boost = false;
            } else {
                self.boost = shift;
                let down = |k| ctx.input(|i| i.key_down(k));
                use egui::Key::*;
                self.kbd.up = self.cfg.keys && down(ArrowUp);
                self.kbd.down = self.cfg.keys && down(ArrowDown);
                self.kbd.left = self.cfg.keys && down(ArrowLeft);
                self.kbd.right = self.cfg.keys && down(ArrowRight);
                self.kbd.thrust_up = self.cfg.wasd && down(W);
                self.kbd.thrust_down = self.cfg.wasd && down(S);
                self.kbd.yaw_left = self.cfg.wasd && down(A);
                self.kbd.yaw_right = self.cfg.wasd && down(D);
            }
        } else if !self.cfg.keyboard() || self.collapsed() {
            self.kbd = Held::default();
            self.boost = false;
        }

        // The gamepad: its sticks are the on-screen ones in the operator's
        // hands, and it carries the arm gesture.
        if self.cfg.gamepad && !self.collapsed() {
            if let Some(axes) = self.pad {
                let read = PadRead::of(axes);
                let step = self.gesture.step(now_ms, &read, self.connected, self.armed);
                self.shown_left = read.left;
                self.shown_right = read.right;
                self.pad_left = step.left;
                self.pad_right = step.right;
                if step.arm {
                    self.stick_mode_after_arm = true;
                    self.arm(ctx);
                }
            } else {
                self.gesture = ArmGesture::default();
                self.shown_left = Stick::CENTRE;
                self.shown_right = Stick::CENTRE;
                self.pad_left = Stick::CENTRE;
                self.pad_right = Stick::CENTRE;
            }
        } else {
            self.pad_left = Stick::CENTRE;
            self.pad_right = Stick::CENTRE;
        }

        // Collapsed, the pad stops being an input: the stream keeps running
        // at neutral, because a gap in it is what PX4 reads as RC loss.
        let collapsed = self.collapsed();
        let inputs = Inputs {
            left: if collapsed { Stick::CENTRE } else { self.left },
            right: if collapsed { Stick::CENTRE } else { self.right },
            pad_left: self.pad_left,
            pad_right: self.pad_right,
            held: or_held(self.kbd, self.pointer),
            boost: self.boost,
        };
        self.frame = model::frame(&self.cfg, &inputs);
        {
            let (lock, cv) = &*self.link;
            if let Ok(mut g) = lock.lock() {
                let wake = g.frame != Some(self.frame) && !self.frame.is_neutral();
                g.frame = Some(self.frame);
                g.streaming = self.cfg.streaming();
                g.connected = self.connected;
                if wake {
                    cv.notify_all();
                }
            }
        }
        if self.cfg.gamepad {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    /// The pad's size at this configuration.
    fn size(&self, ui: &egui::Ui) -> Vec2 {
        let collapsed = self.collapsed();
        let grip_h = 18.0 + 4.0 + 1.0;
        if collapsed {
            return Vec2::new(self.width(ui), 4.0 + 18.0 + 4.0);
        }
        let h = self.row_height(ui);
        let lock_h = if self.cfg.keyboard() {
            5.0 + 10.0 * 1.21 + 8.0 + 2.0
        } else {
            0.0
        };
        Vec2::new(self.width(ui), 3.0 + grip_h + 5.0 + h + lock_h + 7.0)
    }

    /// The gamepad block's lines under the rings: the caption and the status.
    fn gamepad_text(&self, ui: &egui::Ui) -> (std::sync::Arc<egui::Galley>, std::sync::Arc<egui::Galley>) {
        let present = self.pad.is_some();
        let caption = style::galley(ui, "GAMEPAD", 8.5, Weight::W600, style::text_3(), 0.7);
        let color = if !present {
            style::text_3()
        } else if self.gesture.since.is_some() && !self.armed {
            style::warning()
        } else {
            style::text_2()
        };
        let status = crate::ui::kit::wrapped(
            ui,
            self.gesture.message(present, self.connected, self.armed),
            9.5,
            color,
            GP_W,
        );
        (caption, status)
    }

    fn gamepad_height(&self, ui: &egui::Ui) -> f32 {
        let (caption, status) = self.gamepad_text(ui);
        GP_RING + 4.0 + caption.size().y + 4.0 + status.size().y + 4.0 + 3.0
    }

    fn keys_height() -> f32 {
        KEY * 2.0 + 4.0 + 4.0 + 10.2
    }

    /// The tallest block of the surfaces row.
    fn row_height(&self, ui: &egui::Ui) -> f32 {
        let mut h: f32 = 0.0;
        if self.cfg.sticks {
            h = h.max(SIZE + 4.0 + 10.2);
        }
        if self.cfg.gamepad {
            h = h.max(self.gamepad_height(ui));
        }
        if self.cfg.keyboard() {
            h = h.max(Control::keys_height());
        }
        h
    }

    fn surfaces_width(&self) -> f32 {
        let mut parts: Vec<f32> = Vec::new();
        if self.cfg.sticks {
            parts.push(SIZE * 2.0 + 10.0);
        }
        if self.cfg.gamepad {
            parts.push(GP_W);
        }
        if self.cfg.keyboard() {
            let cluster = KEY * 3.0 + 8.0;
            let n = usize::from(self.cfg.keys) + usize::from(self.cfg.wasd);
            parts.push(cluster * n as f32 + 8.0 * (n.saturating_sub(1)) as f32);
        }
        parts.iter().sum::<f32>() + 10.0 * parts.len().saturating_sub(1) as f32
    }

    fn width(&self, ui: &egui::Ui) -> f32 {
        if self.collapsed() {
            let g = style::galley_mono(ui, &self.axes_text(), 9.5, style::text_2());
            return (8.0 * 2.0 + 13.0 + 6.0 + g.size().x + 6.0 + 18.0).max(120.0);
        }
        let lock = if self.cfg.keyboard() {
            style::galley(ui, self.lock.notice(), 10.0, Weight::W600, style::warning(), 0.3)
                .size()
                .x
                + 16.0
                + 2.0
        } else {
            0.0
        };
        (self.surfaces_width()).max(lock) + 16.0
    }

    fn axes_text(&self) -> String {
        model::status_text(&self.cfg, &self.frame, self.connected, self.boost)
    }

    /// Draws the pad in `host` (the map's rectangle). A pad that is not
    /// needed draws nothing.
    pub fn show(&mut self, ui: &mut egui::Ui, host: Rect, now_ms: f64) {
        if !self.cfg.streaming() {
            return;
        }
        let size = self.size(ui);
        let default = mapwindow::corner(host, size, Some(16.0), None, None, Some(16.0));
        let r = self.place.rect(host, size, default);
        let painter = ui.painter().clone();
        style::glass_panel(&painter, "joystick", r, 12, 0.6);
        ui.allocate_rect(r, egui::Sense::hover());

        let collapsed = self.collapsed();
        let pad = r.shrink2(Vec2::new(8.0, 0.0));
        let top = r.top() + if collapsed { 4.0 } else { 3.0 };
        // The grip bar: grip icon, the axes, the collapse button.
        let grip = Rect::from_min_max(Pos2::new(r.left(), r.top()), Pos2::new(r.right(), top + 18.0 + 4.0));
        let btn = Rect::from_center_size(Pos2::new(pad.right() - 9.0, top + 9.0), Vec2::splat(18.0));
        let drag_grip = Rect::from_min_max(grip.min, Pos2::new(btn.left() - 2.0, grip.max.y));
        if self.place.drag(ui, ui.scope_id().with("padgrip"), drag_grip, r, host) {
            ui.ctx().request_repaint();
        }
        icons::paint(
            ui,
            "grip-horizontal",
            Rect::from_center_size(Pos2::new(pad.left() + 6.5, top + 9.0), Vec2::splat(13.0)),
            style::text_3(),
        );
        let axes_color = if self.connected {
            style::text_2()
        } else {
            style::warning()
        };
        let g = style::galley_mono(ui, &self.axes_text(), 9.5, axes_color);
        painter.galley(
            Pos2::new(pad.left() + 13.0 + 6.0, top + 9.0 - g.size().y / 2.0),
            g,
            axes_color,
        );
        let toggle = ui
            .interact(btn, ui.scope_id().with("padcollapse"), egui::Sense::click())
            .on_hover_tr(if collapsed {
                "Expand the control pad"
            } else {
                "Collapse the control pad"
            });
        if toggle.hovered() {
            painter.rect_filled(btn, 5, style::surface_hover());
        }
        icons::paint(
            ui,
            if collapsed { "chevron-up" } else { "chevron-down" },
            Rect::from_center_size(btn.center(), Vec2::splat(13.0)),
            if toggle.hovered() {
                style::text_1()
            } else {
                style::text_3()
            },
        );
        if toggle.clicked() {
            self.place.set_flag("collapsed", !collapsed);
            self.lock.set(true);
            self.release_all();
        }
        if collapsed {
            return;
        }
        painter.hline(
            (pad.left())..=(pad.right()),
            top + 18.0 + 4.0 + 0.5,
            Stroke::new(1.0, style::border_soft()),
        );

        let mut x = pad.left();
        let y = top + 18.0 + 4.0 + 1.0 + 5.0;
        let row_bottom = y + self.row_height(ui);
        if self.cfg.sticks {
            for (i, caption) in ["THR \u{b7} YAW", "PITCH \u{b7} ROLL"].iter().enumerate() {
                let value = self.stick(ui, Pos2::new(x, y), i, caption);
                if i == 0 {
                    self.left = value;
                } else {
                    self.right = value;
                }
                x += SIZE + 10.0;
            }
        }
        if self.cfg.gamepad {
            self.gamepad(ui, Pos2::new(x, row_bottom), now_ms);
            x += GP_W + 10.0;
        }
        if self.cfg.keyboard() {
            self.keys(ui, x, row_bottom, pad.right());
        }
        if self.cfg.keyboard() {
            let lock = Rect::from_min_max(
                Pos2::new(pad.left(), r.bottom() - 7.0 - (10.0 * 1.21 + 8.0 + 2.0)),
                Pos2::new(pad.right(), r.bottom() - 7.0),
            );
            let (text, border) = if self.lock.locked {
                (style::warning(), style::warning_dark())
            } else {
                (style::text_2(), style::border())
            };
            painter.rect_stroke(
                lock,
                style::RADIUS_BTN,
                Stroke::new(1.0, border),
                egui::StrokeKind::Inside,
            );
            let g = style::galley(ui, self.lock.notice(), 10.0, Weight::W600, text, 0.3);
            painter.galley(lock.center() - g.size() / 2.0, g, text);
        }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.shut_down();
    }
}

#[cfg(test)]
mod tests;
