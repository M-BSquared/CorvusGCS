//! Tablet mode (`python/src/js/touch.js`, `python/src/css/touch.css`).
//!
//! One switch (Settings, Interface, Tablet mode) turns the same app into one a
//! finger can fly. It is a state the chrome reads (`on()`), not a second
//! layout: the controls a flight is flown with grow to a target a finger can
//! hit, the map's zoom buttons go (two fingers zoom the map), what a pointer
//! finds by hovering stays shown, the parts that need a mouse or a keyboard
//! are taken away (the SSH tab, the panel's resize edge), and a held finger is
//! the right click a tablet does not have.
//!
//! Kept on THIS device only (`~/.corvus/gui-touch.json`), never in the
//! backend config: a tablet reaches Corvus over the network and shares the
//! laptop's config, so a config key would put the laptop into tablet mode the
//! moment the tablet switched it on. Until the operator has chosen, a device
//! that has been touched starts in it and one that has not does not.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Pos2};
use serde_json::{json, Value};

/// `--touch-target`.
pub const TARGET: f32 = crate::ui::style::TOUCH_TARGET;
/// Just after the map's own 500 ms, as the web has it.
pub const LONG_PRESS_S: f64 = 0.55;
/// How far a finger may drift and still be a press.
pub const MOVE_TOLERANCE: f32 = 10.0;
/// A finger held this long on a pinned window picks it up to move it.
pub const HOLD_TO_MOVE_S: f64 = 0.4;

/// Whether a press that began at `origin` at `start` has been held still long
/// enough at `pos` to pick a window up.
pub fn held_to_move(start: f64, origin: Pos2, pos: Pos2, now: f64) -> bool {
    now - start >= HOLD_TO_MOVE_S && origin.distance(pos) <= MOVE_TOLERANCE
}

static ON: AtomicBool = AtomicBool::new(false);

/// Whether tablet mode is in force.
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// One finger, as the long press sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ev {
    Down(Pos2),
    Move(Pos2),
    Up,
}

/// A held finger as a right click.
#[derive(Debug, Default)]
pub struct LongPress {
    start: Option<(Pos2, f64)>,
    fired: bool,
}

impl LongPress {
    pub fn event(&mut self, ev: Ev, now: f64) {
        match ev {
            Ev::Down(p) => {
                self.start = Some((p, now));
                self.fired = false;
            }
            Ev::Move(p) => {
                if let Some((s, _)) = self.start {
                    if s.distance(p) > MOVE_TOLERANCE {
                        self.start = None;
                    }
                }
            }
            Ev::Up => {
                self.start = None;
                self.fired = false;
            }
        }
    }

    /// The place the right click lands, once the finger has been held there
    /// long enough; once per press.
    pub fn tick(&mut self, now: f64) -> Option<Pos2> {
        let (p, t) = self.start?;
        if self.fired || now - t < LONG_PRESS_S {
            return None;
        }
        self.fired = true;
        Some(p)
    }

    /// A press that fired is not also a tap: its release is swallowed.
    pub fn swallowing(&self) -> bool {
        self.fired
    }

    /// Time until the press fires, for waking the frame loop.
    pub fn pending(&self) -> bool {
        self.start.is_some() && !self.fired
    }
}

/// The mode and what moves it.
#[derive(Debug, Default)]
pub struct Touch {
    /// The operator's choice, once made.
    setting: Option<bool>,
    /// A finger has touched this window.
    seen: bool,
    press: LongPress,
    path: Option<PathBuf>,
}

impl Touch {
    pub fn load() -> Touch {
        let path = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".corvus").join("gui-touch.json"));
        let t = Touch::from_path(path);
        t.publish();
        t
    }

    pub fn from_path(path: Option<PathBuf>) -> Touch {
        let setting = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("enabled").and_then(Value::as_bool));
        Touch {
            setting,
            path,
            ..Default::default()
        }
    }

    /// What is in force: the choice, else whether a finger has been used.
    pub fn enabled(&self) -> bool {
        self.setting.unwrap_or(self.seen)
    }

    fn publish(&self) {
        ON.store(self.enabled(), Ordering::Relaxed);
        crate::ui::style::set_touch(self.enabled());
    }

    /// The Settings switch.
    pub fn set_enabled(&mut self, on: bool) {
        self.setting = Some(on);
        self.publish();
        if let Some(p) = &self.path {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, json!({"enabled": on}).to_string());
        }
    }

    /// Reads the frame's input: a first touch turns the mode on for a device
    /// that has made no choice, and a held finger becomes a right click.
    pub fn handle(&mut self, raw: &mut egui::RawInput, now: f64) {
        let mut touched = false;
        for e in &raw.events {
            if let egui::Event::Touch { phase, pos, .. } = e {
                touched = true;
                self.press.event(
                    match phase {
                        egui::TouchPhase::Start => Ev::Down(*pos),
                        egui::TouchPhase::Move => Ev::Move(*pos),
                        egui::TouchPhase::End | egui::TouchPhase::Cancel => Ev::Up,
                    },
                    now,
                );
            }
        }
        if touched && !self.seen {
            self.seen = true;
            self.publish();
        }
        if !self.enabled() {
            return;
        }
        if let Some(p) = self.press.tick(now) {
            raw.events.push(egui::Event::PointerMoved(p));
            for pressed in [true, false] {
                raw.events.push(egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: Default::default(),
                });
            }
        }
        if self.press.swallowing() {
            // The release of the finger that was held is not a tap.
            raw.events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::PointerButton {
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        ..
                    }
                )
            });
        }
    }

    /// Whether the frame loop must come back to see a press fire.
    pub fn waiting(&self) -> bool {
        self.press.pending()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Pos2 {
        Pos2::new(x, y)
    }

    #[test]
    fn a_finger_held_still_picks_a_window_up_and_one_that_drifts_does_not() {
        let o = p(10.0, 10.0);
        assert!(!held_to_move(1.0, o, o, 1.2), "too soon");
        assert!(held_to_move(1.0, o, p(14.0, 10.0), 1.5));
        assert!(
            !held_to_move(1.0, o, p(40.0, 10.0), 1.5),
            "it moved: a drag, not a hold"
        );
    }

    #[test]
    fn a_finger_held_still_fires_once() {
        let mut l = LongPress::default();
        l.event(Ev::Down(p(100.0, 100.0)), 1.0);
        assert!(l.pending());
        assert_eq!(l.tick(1.3), None);
        assert_eq!(l.tick(1.6), Some(p(100.0, 100.0)));
        assert_eq!(l.tick(2.0), None, "once per press");
        assert!(l.swallowing());
        l.event(Ev::Up, 2.1);
        assert!(!l.swallowing() && !l.pending());
    }

    #[test]
    fn a_finger_that_moves_is_a_drag_and_one_lifted_early_a_tap() {
        let mut l = LongPress::default();
        l.event(Ev::Down(p(0.0, 0.0)), 0.0);
        l.event(Ev::Move(p(4.0, 3.0)), 0.2);
        assert_eq!(l.tick(0.7), Some(p(0.0, 0.0)), "a little drift is still a press");
        let mut l = LongPress::default();
        l.event(Ev::Down(p(0.0, 0.0)), 0.0);
        l.event(Ev::Move(p(30.0, 0.0)), 0.2);
        assert_eq!(l.tick(0.7), None);
        let mut l = LongPress::default();
        l.event(Ev::Down(p(0.0, 0.0)), 0.0);
        l.event(Ev::Up, 0.3);
        assert_eq!(l.tick(0.7), None);
    }

    #[test]
    fn a_device_with_no_choice_follows_the_finger_and_a_choice_is_kept() {
        let dir = std::env::temp_dir().join(format!("corvus-touch-{}", std::process::id()));
        let path = dir.join("touch.json");
        let mut t = Touch::from_path(Some(path.clone()));
        assert!(!t.enabled());
        t.seen = true;
        assert!(t.enabled());
        t.set_enabled(false);
        assert!(!t.enabled(), "a choice beats a finger");
        let t2 = Touch::from_path(Some(path.clone()));
        assert!(!t2.enabled());
        let mut t3 = Touch::from_path(Some(path));
        t3.set_enabled(true);
        assert!(Touch::from_path(t3.path.clone()).enabled());
        let _ = std::fs::remove_dir_all(dir);
        ON.store(false, Ordering::Relaxed);
    }

    #[test]
    fn a_held_finger_becomes_a_right_click_and_its_release_is_not_a_tap() {
        let mut t = Touch::from_path(None);
        t.set_enabled(true);
        let touch = |phase| egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(0),
            phase,
            pos: p(50.0, 60.0),
            force: None,
        };
        let mut raw = egui::RawInput {
            events: vec![touch(egui::TouchPhase::Start)],
            ..Default::default()
        };
        t.handle(&mut raw, 1.0);
        assert!(t.waiting());
        let mut raw = egui::RawInput::default();
        t.handle(&mut raw, 1.6);
        let secondary: Vec<bool> = raw
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::PointerButton {
                    button: egui::PointerButton::Secondary,
                    pressed,
                    ..
                } => Some(*pressed),
                _ => None,
            })
            .collect();
        assert_eq!(secondary, vec![true, false]);
        let mut raw = egui::RawInput {
            events: vec![egui::Event::PointerButton {
                pos: p(50.0, 60.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        };
        t.handle(&mut raw, 1.7);
        assert!(raw.events.is_empty(), "the lift of a held finger is not a click");
        ON.store(false, Ordering::Relaxed);
    }
}
