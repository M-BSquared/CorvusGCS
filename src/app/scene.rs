//! The staging of a picture (`corvus-scene`, `src/scene`): `CORVUS_GUI_SCENE`
//! names a file with the frame the interface is laid out in, how the window
//! stands, and steps at their times. It is the interface's half of the scene
//! kit the README and the website are photographed with; a program started
//! without the variable never looks at any of this.
//!
//! ```json
//! {"frame": [1600, 1000], "panel": "collapsed", "hud_controls": true,
//!  "steps": [{"at": 8, "click": [120, 40]}, {"at": 50, "fit": [11.64, 48.07, 11.65, 48.08]}]}
//! ```
//!
//! A step is one of `click`, `rclick` and `move` (a point), `drag` (`[x, y, to x, to y]`), `gone`
//! (the pointer leaves), `scroll` (`[x, y, dy]`), `text`,
//! `key`, `open` (a page, as an AI client names it), `panel` (a tab of the side panel),
//! `fit` (`[west, south, east, north]`), `look` (`[lng, lat, zoom]`) and `three_d`
//! (`{"bearing": 20, "zoom": 0.5}`, from where the map looks).

use std::sync::OnceLock;

use eframe::egui::{self, Pos2, Vec2};
use serde_json::Value;

/// The padding a fitted flight keeps from the edges of the map.
const FIT_PAD: f32 = 90.0;

pub struct Scene {
    frame: Option<Vec2>,
    collapsed: bool,
    hud_controls: bool,
    steps: Vec<(f64, Value)>,
    next: usize,
    /// What the frames after a step get, one at a time.
    pressed: std::collections::VecDeque<egui::Event>,
}

/// What a step asks of the window.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Open(String),
    Panel(String),
    Fit([f64; 4]),
    Look([f64; 3]),
    ThreeD { bearing: f64, zoom: f64 },
}

static HUD_CONTROLS: OnceLock<bool> = OnceLock::new();

/// Whether the flight HUD shows its buttons with no pointer over it.
pub fn hud_controls() -> bool {
    HUD_CONTROLS.get().copied().unwrap_or(false)
}

fn numbers<const N: usize>(v: &Value) -> Option<[f64; N]> {
    let list: Vec<f64> = v.as_array()?.iter().filter_map(Value::as_f64).collect();
    list.try_into().ok()
}

impl Scene {
    /// The scene `CORVUS_GUI_SCENE` names, or `None`; one that does not read is said.
    pub fn from_env() -> Option<Scene> {
        let path = std::env::var_os("CORVUS_GUI_SCENE")?;
        let read = std::fs::read_to_string(&path).map_err(|e| e.to_string());
        match read.and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
            Ok(v) => {
                let s = Scene::parse(&v);
                let _ = HUD_CONTROLS.set(s.hud_controls);
                Some(s)
            }
            Err(e) => {
                eprintln!("CORVUS_GUI_SCENE: {e}");
                None
            }
        }
    }

    fn parse(v: &Value) -> Scene {
        let mut steps: Vec<(f64, Value)> = v["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| (s["at"].as_f64().unwrap_or(0.0), s.clone()))
            .collect();
        steps.sort_by(|a, b| a.0.total_cmp(&b.0));
        Scene {
            frame: numbers::<2>(&v["frame"])
                .filter(|f| f.iter().all(|n| (200.0..=10_000.0).contains(n)))
                .map(|f| Vec2::new(f[0] as f32, f[1] as f32)),
            collapsed: v["panel"] == "collapsed",
            hud_controls: v["hud_controls"] == true,
            steps,
            next: 0,
            pressed: Default::default(),
        }
    }

    pub fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// The zoom a window `width` points wide begins with.
    pub fn first_zoom(&self, width: f32) -> Option<f32> {
        Some(width / self.frame?.x)
    }

    /// Zooms so that the window, whatever the screen had room for, is laid out as the frame.
    pub fn hold_frame(&self, ctx: &egui::Context) {
        let Some(frame) = self.frame else { return };
        let now = ctx.content_rect().width();
        if now > 1.0 && (now - frame.x).abs() > 0.5 {
            ctx.set_zoom_factor(ctx.zoom_factor() * now / frame.x);
        }
    }

    /// The pointer, the keyboard and the wheel of the steps that are due.
    pub fn input(&mut self, raw: &mut egui::RawInput) -> Vec<Act> {
        raw.events.extend(self.pressed.pop_front());
        let now = raw.time.unwrap_or(0.0);
        let mut acts = Vec::new();
        if !self.pressed.is_empty() {
            return acts;
        }
        while let Some((_, step)) = self.steps.get(self.next).filter(|(at, _)| *at <= now) {
            let step = step.clone();
            self.next += 1;
            self.run(&step, raw, &mut acts);
            // One press a frame: the interface has to see the one before.
            if !self.pressed.is_empty() {
                break;
            }
        }
        acts
    }

    fn run(&mut self, step: &Value, raw: &mut egui::RawInput, acts: &mut Vec<Act>) {
        let point = |k: &str| numbers::<2>(&step[k]).map(|p| Pos2::new(p[0] as f32, p[1] as f32));
        let button = |pos, button, pressed| egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: Default::default(),
        };
        for (k, b) in [
            ("click", egui::PointerButton::Primary),
            ("rclick", egui::PointerButton::Secondary),
        ] {
            if let Some(p) = point(k) {
                raw.events.push(egui::Event::PointerMoved(p));
                raw.events.push(button(p, b, true));
                self.pressed.push_back(button(p, b, false));
            }
        }
        if let Some([x, y, tx, ty]) = numbers::<4>(&step["drag"]) {
            let at = |k: f64| Pos2::new((x + (tx - x) * k) as f32, (y + (ty - y) * k) as f32);
            raw.events.push(egui::Event::PointerMoved(at(0.0)));
            let left = egui::PointerButton::Primary;
            self.pressed.push_back(button(at(0.0), left, true));
            for i in 1..=8 {
                self.pressed
                    .push_back(egui::Event::PointerMoved(at(f64::from(i) / 8.0)));
            }
            self.pressed.push_back(button(at(1.0), left, false));
        }
        if let Some(p) = point("move") {
            raw.events.push(egui::Event::PointerMoved(p));
        }
        if step.get("gone").is_some() {
            raw.events.push(egui::Event::PointerGone);
        }
        if let Some([x, y, dy]) = numbers::<3>(&step["scroll"]) {
            raw.events
                .push(egui::Event::PointerMoved(Pos2::new(x as f32, y as f32)));
            raw.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, -dy as f32),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
                modifiers: Default::default(),
            });
        }
        if let Some(t) = step["text"].as_str() {
            raw.events.push(egui::Event::Text(t.to_string()));
        }
        if let Some(key) = step["key"].as_str().and_then(egui::Key::from_name) {
            let event = |pressed| egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers: Default::default(),
            };
            raw.events.push(event(true));
            self.pressed.push_back(event(false));
        }
        if let Some(p) = step["open"].as_str() {
            acts.push(Act::Open(p.to_string()));
        }
        if let Some(p) = step["panel"].as_str() {
            acts.push(Act::Panel(p.to_string()));
        }
        if let Some(b) = numbers::<4>(&step["fit"]) {
            acts.push(Act::Fit(b));
        }
        if let Some(l) = numbers::<3>(&step["look"]) {
            acts.push(Act::Look(l));
        }
        if let Some(t) = step.get("three_d") {
            acts.push(Act::ThreeD {
                bearing: t["bearing"].as_f64().unwrap_or(0.0),
                zoom: t["zoom"].as_f64().unwrap_or(0.0),
            });
        }
    }

    /// Where the flat map stands so that `bounds` fills a map of `size`.
    pub fn fitted([w, s, e, n]: [f64; 4], size: Vec2) -> Option<(f64, f64, f64)> {
        let (zoom, c) = crate::setup::geofence::fit(&[[w, s], [e, n]], (size.x, size.y), FIT_PAD, 19.0)?;
        Some((c[0], c[1], zoom))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(t: f64) -> egui::RawInput {
        egui::RawInput {
            time: Some(t),
            ..Default::default()
        }
    }

    #[test]
    fn steps_run_at_their_time_and_one_press_a_frame() {
        let mut s = Scene::parse(&json!({"frame": [1600, 1000], "panel": "collapsed", "steps": [
            {"at": 2, "click": [10, 20]}, {"at": 2, "click": [30, 40]}, {"at": 1, "open": "settings"},
            {"at": 3, "fit": [1, 2, 3, 4]}, {"at": 3, "three_d": {"bearing": 20}}
        ]}));
        assert!(s.collapsed() && s.frame == Some(Vec2::new(1600.0, 1000.0)));
        let mut raw = at(0.5);
        assert!(s.input(&mut raw).is_empty() && raw.events.is_empty());
        assert_eq!(s.input(&mut at(1.0)), [Act::Open("settings".into())]);
        let mut raw = at(2.0);
        s.input(&mut raw);
        assert_eq!(raw.events.len(), 2, "moved and pressed");
        let mut raw = at(2.1);
        s.input(&mut raw);
        assert_eq!(raw.events.len(), 3, "let go, then the second press");
        s.input(&mut at(2.2));
        let acts = s.input(&mut at(4.0));
        assert_eq!(acts[0], Act::Fit([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(
            acts[1],
            Act::ThreeD {
                bearing: 20.0,
                zoom: 0.0
            }
        );
    }

    #[test]
    fn what_is_no_scene_stages_nothing() {
        for v in [
            json!(null),
            json!([]),
            json!({"frame": [1], "steps": [{"at": "x", "click": ["a"]}, 3]}),
        ] {
            let mut s = Scene::parse(&v);
            let mut raw = at(100.0);
            assert!(s.input(&mut raw).is_empty() && raw.events.is_empty());
            assert!(s.frame.is_none());
        }
    }
}
