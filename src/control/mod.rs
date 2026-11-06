//! Manual control: the pad over the Home map (`python/src/js/joystick.js`,
//! `.joystick-pad` of `map.css`) with the stick pair, the gamepad block and
//! the two key clusters, all feeding one MANUAL_CONTROL stream.
//!
//! Every surface is off until Settings, Controls turns it on, and nothing
//! streams without one. The stream is a thread of its own that posts the
//! frame at 20 Hz while an axis is off centre and 5 Hz at neutral (a gap in
//! the stream is what PX4 reads as RC loss), one frame at a time. The
//! keyboard starts locked: Shift + W twice opens it, Escape locks it again.

pub mod model;

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
            .on_hover_text(if collapsed {
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

    /// One stick: a base, a knob that follows the pointer, a caption.
    fn stick(&self, ui: &egui::Ui, at: Pos2, index: usize, caption: &str) -> Stick {
        let base = Rect::from_min_size(at, Vec2::splat(SIZE));
        let resp = ui.interact(base, ui.scope_id().with(("stick", index)), egui::Sense::drag());
        let active = resp.is_pointer_button_down_on() || resp.dragged();
        let mut value = Stick::CENTRE;
        if active {
            if let Some(p) = resp.interact_pointer_pos() {
                let radius = f64::from(SIZE) / 2.0;
                let nx = f64::from(p.x - base.center().x) / radius;
                let ny = f64::from(base.center().y - p.y) / radius;
                let c = model::clamp_to_circle(nx, ny);
                value = Stick {
                    x: model::apply_deadzone(c.x),
                    y: model::apply_deadzone(c.y),
                };
            }
        }
        let painter = ui.painter();
        painter.circle_filled(base.center(), SIZE / 2.0, style::input(0.5));
        painter.circle_stroke(
            base.center(),
            SIZE / 2.0 - 0.5,
            Stroke::new(1.0, if active { style::accent() } else { style::border() }),
        );
        if active {
            painter.circle_stroke(base.center(), SIZE / 2.0 + 1.0, Stroke::new(2.0, style::accent_dim()));
        }
        let hair = Stroke::new(1.0, style::border_soft());
        painter.hline((base.left() + 1.0)..=(base.right() - 1.0), base.center().y, hair);
        painter.vline(base.center().x, (base.top() + 1.0)..=(base.bottom() - 1.0), hair);
        let travel = (SIZE - KNOB) / 2.0;
        let knob = base.center() + Vec2::new(value.x as f32 * travel, -value.y as f32 * travel);
        painter.circle_filled(knob + Vec2::new(0.0, 2.0), KNOB / 2.0, Color32::from_black_alpha(46));
        painter.circle_filled(
            knob,
            KNOB / 2.0,
            if active { style::accent() } else { style::surface_3() },
        );
        painter.circle_stroke(
            knob,
            KNOB / 2.0 - 0.5,
            Stroke::new(
                1.0,
                if active {
                    style::accent()
                } else {
                    style::border_strong()
                },
            ),
        );
        let g = style::galley(ui, &caption.to_uppercase(), 8.5, Weight::W600, style::text_3(), 0.7);
        painter.galley(
            Pos2::new(base.center().x - g.size().x / 2.0, base.bottom() + 4.0),
            g,
            style::text_3(),
        );
        value
    }

    /// The gamepad block, its bottom edge at `at.y`: two rings, the status
    /// line, the arm bar.
    fn gamepad(&self, ui: &egui::Ui, at: Pos2, now_ms: f64) {
        let present = self.pad.is_some();
        let painter = ui.painter();
        let dot = if present { style::accent() } else { style::text_3() };
        let rings_w = GP_RING * 2.0 + 10.0;
        let x0 = at.x + (GP_W - rings_w) / 2.0;
        let (caption, status) = self.gamepad_text(ui);
        let mut y = at.y - self.gamepad_height(ui);
        let at = Pos2::new(at.x, y);
        for (i, v) in [self.shown_left, self.shown_right].iter().enumerate() {
            let c = Pos2::new(x0 + GP_RING / 2.0 + i as f32 * (GP_RING + 10.0), y + GP_RING / 2.0);
            painter.circle_filled(c, GP_RING / 2.0, style::input(0.5));
            painter.circle_stroke(c, GP_RING / 2.0 - 0.5, Stroke::new(1.0, style::border()));
            let travel = (GP_RING - GP_DOT) / 2.0;
            painter.circle_filled(
                c + Vec2::new(v.x as f32 * travel, -v.y as f32 * travel),
                GP_DOT / 2.0,
                dot,
            );
        }
        y += GP_RING + 4.0;
        painter.galley(
            Pos2::new(at.x + (GP_W - caption.size().x) / 2.0, y),
            caption.clone(),
            style::text_3(),
        );
        y += caption.size().y + 4.0;
        let sc = if !present { style::text_3() } else { style::text_2() };
        painter.galley(Pos2::new(at.x + (GP_W - status.size().x) / 2.0, y), status.clone(), sc);
        y += status.size().y + 4.0;
        let bar = Rect::from_min_size(Pos2::new(at.x, y), Vec2::new(GP_W, 3.0));
        painter.rect_filled(bar, 2, style::border_soft());
        let p = self.gesture.progress(now_ms, self.armed) as f32;
        if p > 0.0 {
            painter.rect_filled(
                Rect::from_min_size(bar.min, Vec2::new(GP_W * p, 3.0)),
                2,
                if self.armed { style::accent() } else { style::warning() },
            );
        }
    }

    /// The key clusters, bottom aligned to the right of what is before them.
    fn keys(&mut self, ui: &egui::Ui, x: f32, bottom: f32, right: f32) {
        let cluster_w = KEY * 3.0 + 8.0;
        let n = usize::from(self.cfg.keys) + usize::from(self.cfg.wasd);
        let total = cluster_w * n as f32 + 8.0 * (n.saturating_sub(1)) as f32;
        let mut cx = (right - total).max(x);
        let top = bottom - Control::keys_height();
        let mut clusters: Vec<Cluster> = Vec::new();
        if self.cfg.wasd {
            clusters.push((
                "THR \u{b7} YAW",
                [
                    ("thrust_up", "W", None),
                    ("yaw_left", "A", None),
                    ("thrust_down", "S", None),
                    ("yaw_right", "D", None),
                ],
            ));
        }
        if self.cfg.keys {
            clusters.push((
                "ARROW KEYS",
                [
                    ("up", "", Some("arrow-up")),
                    ("left", "", Some("arrow-left")),
                    ("down", "", Some("arrow-down")),
                    ("right", "", Some("arrow-right")),
                ],
            ));
        }
        let mut pointer_held = Held::default();
        for (caption, keys) in clusters {
            // The inverted T: up in the middle of the top row, then left,
            // down, right along the bottom.
            let cells = [(1, 0), (0, 1), (1, 1), (2, 1)];
            for ((dir, letter, icon), (col, row)) in keys.iter().zip(cells) {
                let r = Rect::from_min_size(
                    Pos2::new(cx + col as f32 * (KEY + 4.0), top + row as f32 * (KEY + 4.0)),
                    Vec2::splat(KEY),
                );
                let resp = ui.interact(r, ui.scope_id().with(("key", *dir)), egui::Sense::click_and_drag());
                let down = resp.is_pointer_button_down_on();
                if down {
                    match *dir {
                        "up" => pointer_held.up = true,
                        "down" => pointer_held.down = true,
                        "left" => pointer_held.left = true,
                        "right" => pointer_held.right = true,
                        "thrust_up" => pointer_held.thrust_up = true,
                        "thrust_down" => pointer_held.thrust_down = true,
                        "yaw_left" => pointer_held.yaw_left = true,
                        _ => pointer_held.yaw_right = true,
                    }
                }
                let lit = down || self.is_held(dir);
                let (fill, color, stroke) = if lit {
                    let c = if self.boost { style::warning() } else { style::accent() };
                    (c, style::text_inverted(), c)
                } else {
                    (
                        style::input(0.5),
                        if resp.hovered() {
                            style::text_1()
                        } else {
                            style::text_2()
                        },
                        if resp.hovered() {
                            style::border_strong()
                        } else {
                            style::border()
                        },
                    )
                };
                let r = if lit { r.translate(Vec2::new(0.0, 1.0)) } else { r };
                let painter = ui.painter();
                painter.rect_filled(r, style::RADIUS_BTN, fill);
                painter.rect_stroke(r, style::RADIUS_BTN, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
                if let Some(icon) = icon {
                    icons::paint(ui, icon, Rect::from_center_size(r.center(), Vec2::splat(13.0)), color);
                } else {
                    let g = style::galley_mono(ui, letter, 11.0, color);
                    painter.galley(r.center() - g.size() / 2.0, g, color);
                }
            }
            let c = style::galley(
                ui,
                caption,
                8.5,
                Weight::W600,
                if self.boost { style::warning() } else { style::text_3() },
                0.7,
            );
            ui.painter().galley(
                Pos2::new(cx + cluster_w / 2.0 - c.size().x / 2.0, top + KEY * 2.0 + 4.0 + 4.0),
                c,
                if self.boost { style::warning() } else { style::text_3() },
            );
            cx += cluster_w + 8.0;
        }
        // A key held by the pointer flies like one held by the keyboard.
        self.pointer = if self.collapsed() {
            Held::default()
        } else {
            pointer_held
        };
    }

    fn is_held(&self, dir: &str) -> bool {
        let h = or_held(self.kbd, Held::default());
        match dir {
            "up" => h.up,
            "down" => h.down,
            "left" => h.left,
            "right" => h.right,
            "thrust_up" => h.thrust_up,
            "thrust_down" => h.thrust_down,
            "yaw_left" => h.yaw_left,
            _ => h.yaw_right,
        }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.shut_down();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A stand-in backend that counts the frames it is sent.
    fn backend() -> (Server, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let got: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = got.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let mut text = String::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = s.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    text.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let want = head
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if body.len() >= want {
                            break;
                        }
                    }
                }
                if let Some(body) = text.split("\r\n\r\n").nth(1) {
                    seen.relock().push(body.to_string());
                }
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}");
            }
        });
        (Server::local(port), got)
    }

    #[test]
    fn a_pad_with_no_surface_streams_nothing() {
        let (server, got) = backend();
        let mut c = Control::new(server, Config::default());
        let ctx = egui::Context::default();
        c.update(&ctx, &json!({"connected": true}), &Default::default(), true, 0.0);
        std::thread::sleep(Duration::from_millis(500));
        c.shut_down();
        assert!(got.relock().is_empty());
    }

    #[test]
    fn the_stream_sends_neutral_frames_while_the_link_is_up_and_stops_on_shutdown() {
        let (server, got) = backend();
        let cfg = Config {
            sticks: true,
            ..Config::default()
        };
        let mut c = Control::new(server, cfg);
        let ctx = egui::Context::default();
        c.update(&ctx, &json!({"connected": true}), &Default::default(), true, 0.0);
        std::thread::sleep(Duration::from_millis(700));
        c.shut_down();
        let frames = got.relock().clone();
        assert!(frames.len() >= 2, "{frames:?}");
        assert!(frames.iter().all(|b| {
            let v: Value = serde_json::from_str(b).expect("json");
            v == json!({"x": 0.0, "y": 0.0, "z": 0.5, "r": 0.0})
        }));
        // Nothing more once it is down.
        let n = got.relock().len();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(got.relock().len(), n);
    }

    #[test]
    fn no_link_sends_nothing() {
        let (server, got) = backend();
        let mut c = Control::new(
            server,
            Config {
                sticks: true,
                ..Config::default()
            },
        );
        let ctx = egui::Context::default();
        c.update(&ctx, &json!({"connected": false}), &Default::default(), true, 0.0);
        std::thread::sleep(Duration::from_millis(500));
        c.shut_down();
        assert!(got.relock().is_empty());
    }

    #[test]
    fn a_gamepad_pushed_forward_flies_and_the_arm_gesture_asks_once() {
        let (server, got) = backend();
        let mut c = Control::new(
            server,
            Config {
                gamepad: true,
                ..Config::default()
            },
        );
        let ctx = egui::Context::default();
        let state = json!({"connected": true, "armed": true});
        // Left stick down on the raw axes is thrust up.
        c.feed_gamepad(Some([0.0, -1.0, 0.0, 0.0]));
        c.update(&ctx, &state, &Default::default(), true, 0.0);
        assert_eq!(c.frame.z, 1.0);
        c.shut_down();
        let _ = got;
        // Disarmed, both sticks inward for a second: one arm request.
        let (server, got) = backend();
        let mut c = Control::new(
            server,
            Config {
                gamepad: true,
                ..Config::default()
            },
        );
        let parked = json!({"connected": true, "armed": false});
        c.feed_gamepad(Some([1.0, 0.0, -1.0, 0.0]));
        c.update(&ctx, &parked, &Default::default(), true, 0.0);
        assert!(c.frame.is_neutral(), "silent while the gesture is held");
        c.update(&ctx, &parked, &Default::default(), true, 1000.0);
        assert!(c.gesture.sent && c.stick_mode_after_arm);
        std::thread::sleep(Duration::from_millis(400));
        c.shut_down();
        let bodies = got.relock().clone();
        assert!(bodies.iter().any(|b| b.contains("\"arm\":true")), "{bodies:?}");
    }

    fn key(key: egui::Key, pressed: bool, shift: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: egui::Modifiers {
                shift,
                ..Default::default()
            },
        }
    }

    /// One frame of the window with these events, then the pad's step.
    fn frame_with(c: &mut Control, ctx: &egui::Context, events: Vec<egui::Event>, now: f64) {
        let shift = events
            .iter()
            .any(|e| matches!(e, egui::Event::Key { modifiers, .. } if modifiers.shift));
        let mut all = vec![egui::Event::ModifiersChanged(egui::Modifiers {
            shift,
            ..Default::default()
        })];
        all.extend(events);
        let input = egui::RawInput {
            events: all,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            c.update(
                ui.ctx(),
                &json!({"connected": true, "armed": false}),
                &Default::default(),
                true,
                now,
            );
        });
        out.textures_delta.clear();
    }

    #[test]
    fn the_keyboard_is_locked_until_shift_w_twice_and_escape_locks_it_again() {
        let (server, _) = backend();
        let mut c = Control::new(
            server,
            Config {
                keys: true,
                wasd: true,
                ..Config::default()
            },
        );
        let ctx = egui::Context::default();
        // Locked: an arrow key flies nothing.
        frame_with(&mut c, &ctx, vec![key(egui::Key::ArrowUp, true, false)], 0.0);
        assert!(c.frame.is_neutral());
        frame_with(&mut c, &ctx, vec![key(egui::Key::ArrowUp, false, false)], 10.0);
        // Shift + W, released, Shift + W again inside the window.
        frame_with(&mut c, &ctx, vec![key(egui::Key::W, true, true)], 100.0);
        assert!(c.lock.locked);
        frame_with(&mut c, &ctx, vec![key(egui::Key::W, false, true)], 200.0);
        frame_with(&mut c, &ctx, vec![key(egui::Key::W, true, true)], 500.0);
        assert!(!c.lock.locked);
        frame_with(&mut c, &ctx, vec![key(egui::Key::W, false, true)], 600.0);
        // Unlocked: the arrow flies half a stick, Shift doubles it.
        frame_with(&mut c, &ctx, vec![key(egui::Key::ArrowUp, true, false)], 700.0);
        assert_eq!(c.frame.x, 0.5);
        frame_with(&mut c, &ctx, vec![key(egui::Key::ArrowUp, true, true)], 750.0);
        assert_eq!(c.frame.x, 1.0);
        frame_with(&mut c, &ctx, vec![key(egui::Key::ArrowUp, false, false)], 800.0);
        frame_with(&mut c, &ctx, vec![key(egui::Key::D, true, false)], 850.0);
        assert_eq!(c.frame.r, 0.5);
        frame_with(&mut c, &ctx, vec![key(egui::Key::D, false, false)], 900.0);
        // Escape locks it again and lets go of everything.
        frame_with(&mut c, &ctx, vec![key(egui::Key::Escape, true, false)], 950.0);
        assert!(c.lock.locked);
        assert!(c.frame.is_neutral());
        c.shut_down();
    }

    #[test]
    fn a_second_press_too_late_does_not_unlock() {
        let (server, _) = backend();
        let mut c = Control::new(
            server,
            Config {
                wasd: true,
                ..Config::default()
            },
        );
        let ctx = egui::Context::default();
        frame_with(&mut c, &ctx, vec![key(egui::Key::W, true, true)], 0.0);
        frame_with(&mut c, &ctx, vec![key(egui::Key::W, false, true)], 100.0);
        frame_with(&mut c, &ctx, vec![key(egui::Key::W, true, true)], 1500.0);
        assert!(c.lock.locked);
        c.shut_down();
    }

    #[test]
    fn a_new_config_locks_the_keyboard_and_lets_go_of_everything() {
        let (server, _) = backend();
        let mut c = Control::new(
            server,
            Config {
                keys: true,
                ..Config::default()
            },
        );
        c.lock.set(false);
        c.kbd.up = true;
        c.set_config(Config {
            keys: true,
            wasd: true,
            ..Config::default()
        });
        assert!(c.lock.locked);
        assert_eq!(c.kbd, Held::default());
        c.shut_down();
    }
}
