//! The rules of manual control (`python/src/js/joystick.js`): how the sticks,
//! the keys and a gamepad sum into one MANUAL_CONTROL frame, the lock the
//! keyboard starts in, and the gesture that arms from a gamepad.
//!
//! Axis mapping is the transmitter's, because that is what PX4 receives:
//! left stick X is yaw, Y is thrust (0.5 the hover detent); right stick X is
//! roll, Y is pitch. Arrow keys move pitch and roll, W A S D thrust and yaw.

use serde_json::Value;

pub const ACTIVE_HZ: f64 = 20.0;
pub const IDLE_HZ: f64 = 5.0;
/// Between the two Shift + W presses that unlock the keyboard.
pub const UNLOCK_WINDOW_MS: f64 = 800.0;
/// Between mode requests from key presses.
pub const MODE_RETRY_MS: f64 = 2000.0;
/// The share of stick travel that still reads as centre.
pub const DEADZONE: f64 = 0.06;
/// A thumbstick rests further off centre than a pointer.
pub const GAMEPAD_DEADZONE: f64 = 0.12;
/// How far each stick must lean toward the other to arm.
pub const ARM_INWARD: f64 = 0.7;
/// How long both must stay there.
pub const ARM_HOLD_MS: f64 = 1000.0;
/// What one held key is worth, as a share of full travel.
pub const GAIN_STEPS: [f64; 4] = [0.25, 0.5, 0.75, 1.0];
pub const DEFAULT_GAIN: f64 = 0.5;
/// Shift multiplies the gain.
pub const BOOST_FACTOR: f64 = 2.0;

/// A stick's position, each axis in [-1, 1], y positive up.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stick {
    pub x: f64,
    pub y: f64,
}

impl Stick {
    pub const CENTRE: Stick = Stick { x: 0.0, y: 0.0 };

    pub fn is_centre(self) -> bool {
        self.x == 0.0 && self.y == 0.0
    }
}

/// Clamp to the circle, not the square: a stick has no corners, and without
/// this a diagonal reads as 1.41 of travel.
pub fn clamp_to_circle(x: f64, y: f64) -> Stick {
    let mag = x.hypot(y);
    if mag > 1.0 {
        Stick { x: x / mag, y: y / mag }
    } else {
        Stick { x, y }
    }
}

fn dead(v: f64, zone: f64) -> f64 {
    if v.abs() <= zone {
        return 0.0;
    }
    let scaled = (v.abs() - zone) / (1.0 - zone);
    if v < 0.0 {
        -scaled
    } else {
        scaled
    }
}

/// A pointer's deadzone, rescaled so the stick still reaches full travel.
pub fn apply_deadzone(v: f64) -> f64 {
    dead(v, DEADZONE)
}

/// Axes go on the wire as int16 ticks at 1000 per unit, so anything past the
/// third decimal is noise, and rounding here lets `is_neutral` be an equality.
pub fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// A gamepad stick from its raw axes: axes grow down like the screen, so y is
/// inverted once here.
pub fn gamepad_stick(ax: f64, ay: f64) -> Stick {
    let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
    let c = clamp_to_circle(finite(ax), -finite(ay));
    Stick {
        x: round3(dead(c.x, GAMEPAD_DEADZONE)),
        y: round3(dead(c.y, GAMEPAD_DEADZONE)),
    }
}

/// Which surfaces are switched on, `controls` of the config.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub sticks: bool,
    pub gamepad: bool,
    pub keys: bool,
    pub wasd: bool,
    pub gain: f64,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            sticks: false,
            gamepad: false,
            keys: false,
            wasd: false,
            gain: DEFAULT_GAIN,
        }
    }
}

impl Config {
    /// All off by default: nothing flies the aircraft unless asked to.
    pub fn from_config(cfg: &Value) -> Config {
        let c = cfg.get("controls");
        let flag = |k: &str| c.and_then(|c| c.get(k)).and_then(Value::as_bool).unwrap_or(false);
        let gain = c
            .and_then(|c| c.get("key_gain"))
            .and_then(Value::as_f64)
            .filter(|g| g.is_finite())
            .map_or(DEFAULT_GAIN, |g| g.clamp(GAIN_STEPS[0], GAIN_STEPS[3]));
        Config {
            sticks: flag("virtual_joystick"),
            gamepad: flag("gamepad"),
            keys: flag("arrow_keys"),
            wasd: flag("wasd_keys"),
            gain,
        }
    }

    /// Whether the pad exists and the stream runs.
    pub fn streaming(&self) -> bool {
        self.sticks || self.gamepad || self.keys || self.wasd
    }

    /// Whether the keyboard has anything to fly.
    pub fn keyboard(&self) -> bool {
        self.keys || self.wasd
    }
}

/// The eight keys of the two clusters, held or not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub thrust_up: bool,
    pub thrust_down: bool,
    pub yaw_left: bool,
    pub yaw_right: bool,
}

/// One MANUAL_CONTROL frame as `POST /api/mavlink/manual` takes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Pitch, forward positive.
    pub x: f64,
    /// Roll, right positive.
    pub y: f64,
    /// Thrust 0 to 1, 0.5 the hover detent.
    pub z: f64,
    /// Yaw, clockwise positive.
    pub r: f64,
}

impl Frame {
    pub const NEUTRAL: Frame = Frame {
        x: 0.0,
        y: 0.0,
        z: 0.5,
        r: 0.0,
    };

    pub fn is_neutral(&self) -> bool {
        *self == Frame::NEUTRAL
    }

    pub fn body(&self) -> Value {
        serde_json::json!({"x": self.x, "y": self.y, "z": self.z, "r": self.r})
    }
}

/// What every input contributes this instant.
#[derive(Debug, Clone, Copy, Default)]
pub struct Inputs {
    pub left: Stick,
    pub right: Stick,
    /// What the gamepad adds (zero while the arm gesture holds it silent).
    pub pad_left: Stick,
    pub pad_right: Stick,
    pub held: Held,
    pub boost: bool,
}

/// How much stick one held key is worth, boost included.
pub fn deflection(cfg: &Config, boost: bool) -> f64 {
    (cfg.gain * if boost { BOOST_FACTOR } else { 1.0 }).min(GAIN_STEPS[3])
}

/// The four axes for the current state of every surface. Keys and sticks sum
/// into the same virtual stick, clamped to the circle.
pub fn frame(cfg: &Config, i: &Inputs) -> Frame {
    let mut left = if cfg.sticks { i.left } else { Stick::CENTRE };
    let mut right = if cfg.sticks { i.right } else { Stick::CENTRE };
    if cfg.gamepad {
        left = clamp_to_circle(left.x + i.pad_left.x, left.y + i.pad_left.y);
        right = clamp_to_circle(right.x + i.pad_right.x, right.y + i.pad_right.y);
    }
    let step = deflection(cfg, i.boost);
    let (mut pitch, mut roll) = (right.y, right.x);
    if cfg.keys {
        if i.held.up {
            pitch += step;
        }
        if i.held.down {
            pitch -= step;
        }
        if i.held.right {
            roll += step;
        }
        if i.held.left {
            roll -= step;
        }
        let c = clamp_to_circle(roll, pitch);
        (roll, pitch) = (c.x, c.y);
    }
    let (mut thrust, mut yaw) = (left.y, left.x);
    if cfg.wasd {
        if i.held.thrust_up {
            thrust += step;
        }
        if i.held.thrust_down {
            thrust -= step;
        }
        if i.held.yaw_right {
            yaw += step;
        }
        if i.held.yaw_left {
            yaw -= step;
        }
        let c = clamp_to_circle(yaw, thrust);
        (yaw, thrust) = (c.x, c.y);
    }
    Frame {
        x: round3(pitch),
        y: round3(roll),
        z: round3(0.5 + thrust * 0.5),
        r: round3(yaw),
    }
}

fn signed(v: f64) -> String {
    format!("{}{:.2}", if v >= 0.0 { "+" } else { "\u{2212}" }, v.abs())
}

/// The axes in the pad's bar: only the half the visible surfaces can move.
pub fn status_text(cfg: &Config, f: &Frame, connected: bool, boost: bool) -> String {
    if !connected {
        return "NO LINK".into();
    }
    let mut parts: Vec<String> = Vec::new();
    let all = cfg.sticks || cfg.gamepad;
    if all || cfg.keys {
        parts.push(format!("P {}", signed(f.x)));
        parts.push(format!("R {}", signed(f.y)));
    }
    if all || cfg.wasd {
        parts.push(format!("T {:.2}", f.z));
        parts.push(format!("Y {}", signed(f.r)));
    }
    if boost {
        parts.push("BOOST".into());
    }
    parts.join("  ")
}

/// The keyboard's lock: Shift + W twice in quick succession opens it, Escape
/// closes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Lock {
    pub locked: bool,
    last_press: Option<f64>,
}

impl Default for Lock {
    fn default() -> Lock {
        Lock {
            locked: true,
            last_press: None,
        }
    }
}

impl Lock {
    /// A Shift + W press (not a repeat) at `now` ms; true when it unlocked.
    pub fn shift_w(&mut self, now: f64) -> bool {
        match self.last_press {
            Some(t) if now - t <= UNLOCK_WINDOW_MS => {
                self.locked = false;
                self.last_press = None;
                true
            }
            _ => {
                self.last_press = Some(now);
                false
            }
        }
    }

    pub fn set(&mut self, locked: bool) {
        self.last_press = None;
        self.locked = locked;
    }

    /// The notice in the pad.
    pub fn notice(&self) -> &'static str {
        if self.locked {
            "Keyboard locked. Press Shift + W twice to unlock."
        } else {
            "Keyboard unlocked. Esc locks it again."
        }
    }
}

/// What a gamepad read this instant, in raw axes.
#[derive(Debug, Clone, Copy, Default)]
pub struct PadRead {
    pub left: Stick,
    pub right: Stick,
    /// Left stick leaning right and right stick leaning left, on the RAW
    /// axes, before the deadzone and the circle.
    pub inward: bool,
}

impl PadRead {
    /// From the four raw axes (left x, left y, right x, right y).
    pub fn of(axes: [f64; 4]) -> PadRead {
        PadRead {
            left: gamepad_stick(axes[0], axes[1]),
            right: gamepad_stick(axes[2], axes[3]),
            inward: axes[0] >= ARM_INWARD && axes[2] <= -ARM_INWARD,
        }
    }
}

/// The gamepad's one gesture: both sticks inward for a second arms a
/// disarmed vehicle; the pad then flies nothing until both are centred.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArmGesture {
    pub since: Option<f64>,
    pub sent: bool,
    pub gated: bool,
}

/// What a step of the gesture wants done.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    pub left: Stick,
    pub right: Stick,
    pub arm: bool,
}

impl ArmGesture {
    pub fn step(&mut self, now: f64, read: &PadRead, connected: bool, armed: bool) -> Step {
        let mut arm = false;
        if read.inward && connected && !armed {
            let since = *self.since.get_or_insert(now);
            if !self.sent && now - since >= ARM_HOLD_MS {
                self.sent = true;
                self.gated = true;
                arm = true;
            }
        } else {
            self.since = None;
            self.sent = false;
        }
        if self.gated && read.left.is_centre() && read.right.is_centre() {
            self.gated = false;
        }
        // Silent while the gesture is held on the ground and after an arm
        // until both sticks are let go; in the air inward on both is a real
        // input.
        let silent = self.gated || (read.inward && !armed);
        Step {
            left: if silent { Stick::CENTRE } else { read.left },
            right: if silent { Stick::CENTRE } else { read.right },
            arm,
        }
    }

    /// How far the hold has come, 0 to 1.
    pub fn progress(&self, now: f64, armed: bool) -> f64 {
        match self.since {
            Some(s) if !self.sent && !armed => ((now - s) / ARM_HOLD_MS).clamp(0.0, 1.0),
            _ if self.sent => 1.0,
            _ => 0.0,
        }
    }

    /// The line under the gamepad's dots.
    pub fn message(&self, present: bool, connected: bool, armed: bool) -> &'static str {
        if !present {
            return "Plug in a gamepad and press any button on it.";
        }
        if !connected {
            return "Gamepad ready. No link.";
        }
        if armed {
            return if self.gated {
                "Armed. Let go of both sticks to fly."
            } else {
                "Armed."
            };
        }
        if self.sent {
            "Arming."
        } else if self.since.is_some() {
            "Keep holding to arm."
        } else {
            "Push both sticks inward to arm."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg(sticks: bool, keys: bool, wasd: bool) -> Config {
        Config {
            sticks,
            keys,
            wasd,
            ..Config::default()
        }
    }

    #[test]
    fn everything_is_off_until_the_config_says_otherwise() {
        let c = Config::from_config(&json!({}));
        assert!(!c.streaming() && !c.keyboard());
        assert_eq!(c.gain, 0.5);
        let c = Config::from_config(&json!({"controls": {"virtual_joystick": true, "wasd_keys": true, "key_gain": 3}}));
        assert!(c.sticks && c.wasd && !c.keys && !c.gamepad);
        assert_eq!(c.gain, 1.0);
        assert!(c.streaming() && c.keyboard());
        assert_eq!(Config::from_config(&json!({"controls": {"key_gain": 0.1}})).gain, 0.25);
        assert_eq!(Config::from_config(&json!({"controls": {"key_gain": "x"}})).gain, 0.5);
    }

    #[test]
    fn nothing_held_is_the_neutral_frame() {
        let f = frame(&cfg(true, true, true), &Inputs::default());
        assert!(f.is_neutral());
        assert_eq!(f, Frame::NEUTRAL);
    }

    #[test]
    fn a_key_is_worth_half_a_stick_and_shift_doubles_it() {
        let mut i = Inputs::default();
        i.held.up = true;
        let c = cfg(false, true, false);
        assert_eq!(frame(&c, &i).x, 0.5);
        i.boost = true;
        assert_eq!(frame(&c, &i).x, 1.0);
        let quarter = Config { gain: 0.25, ..c };
        assert_eq!(frame(&quarter, &i).x, 0.5);
        assert_eq!(deflection(&Config { gain: 1.0, ..c }, true), 1.0);
    }

    #[test]
    fn the_axes_are_the_transmitters() {
        let c = cfg(true, false, false);
        let i = Inputs {
            left: Stick { x: 0.4, y: 1.0 },
            right: Stick { x: -0.5, y: 0.25 },
            ..Inputs::default()
        };
        let f = frame(&c, &i);
        assert_eq!((f.r, f.z, f.y, f.x), (0.4, 1.0, -0.5, 0.25));
        // Down on the left stick is no thrust.
        let down = Inputs {
            left: Stick { x: 0.0, y: -1.0 },
            ..Inputs::default()
        };
        assert_eq!(frame(&c, &down).z, 0.0);
    }

    #[test]
    fn a_key_only_counts_while_its_surface_is_on() {
        let mut i = Inputs::default();
        i.held.thrust_up = true;
        i.held.up = true;
        assert!(frame(&cfg(false, false, false), &i).is_neutral());
        let f = frame(&cfg(false, true, false), &i);
        assert_eq!((f.x, f.z), (0.5, 0.5));
        let f = frame(&cfg(false, false, true), &i);
        assert_eq!((f.x, f.z), (0.0, 0.75));
    }

    #[test]
    fn keys_and_sticks_sum_into_one_stick_clamped_to_the_circle() {
        let mut i = Inputs {
            right: Stick { x: 1.0, y: 0.0 },
            ..Inputs::default()
        };
        i.held.up = true;
        i.boost = true;
        let f = frame(&cfg(true, true, false), &i);
        let mag = f.x.hypot(f.y);
        assert!((mag - 1.0).abs() < 0.002, "{mag}");
        assert!(f.x > 0.6 && f.y < 0.8);
    }

    #[test]
    fn a_gamepad_sums_into_its_own_side() {
        let c = Config {
            gamepad: true,
            ..Config::default()
        };
        let i = Inputs {
            pad_left: Stick { x: 0.0, y: -0.5 },
            pad_right: Stick { x: 0.3, y: 0.2 },
            ..Inputs::default()
        };
        let f = frame(&c, &i);
        assert_eq!((f.z, f.y, f.x), (0.25, 0.3, 0.2));
    }

    #[test]
    fn the_pointer_deadzone_is_rescaled() {
        assert_eq!(apply_deadzone(0.05), 0.0);
        assert_eq!(apply_deadzone(1.0), 1.0);
        assert!((apply_deadzone(0.53) - 0.5).abs() < 1e-9);
        assert!((apply_deadzone(-0.53) + 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_gamepad_stick_is_inverted_and_has_a_wider_deadzone() {
        assert_eq!(gamepad_stick(0.1, -0.1), Stick::CENTRE);
        assert_eq!(gamepad_stick(0.0, -1.0), Stick { x: 0.0, y: 1.0 });
        assert_eq!(gamepad_stick(1.0, 1.0), Stick { x: 0.667, y: -0.667 });
        assert_eq!(gamepad_stick(f64::NAN, 0.0), Stick::CENTRE);
    }

    #[test]
    fn the_bar_names_only_the_axes_the_surfaces_move() {
        let f = Frame {
            x: 0.5,
            y: -0.25,
            z: 0.75,
            r: 0.0,
        };
        assert_eq!(
            status_text(&cfg(true, false, false), &f, true, false),
            "P +0.50  R \u{2212}0.25  T 0.75  Y +0.00"
        );
        assert_eq!(
            status_text(&cfg(false, true, false), &f, true, true),
            "P +0.50  R \u{2212}0.25  BOOST"
        );
        assert_eq!(
            status_text(&cfg(false, false, true), &f, true, false),
            "T 0.75  Y +0.00"
        );
        assert_eq!(status_text(&cfg(true, false, false), &f, false, false), "NO LINK");
    }

    #[test]
    fn two_shift_w_presses_in_time_unlock_and_escape_locks() {
        let mut l = Lock::default();
        assert!(l.locked);
        assert!(!l.shift_w(1000.0));
        assert!(l.locked);
        assert!(l.shift_w(1700.0));
        assert!(!l.locked);
        assert_eq!(l.notice(), "Keyboard unlocked. Esc locks it again.");
        l.set(true);
        assert!(l.locked);
        assert!(!l.shift_w(5000.0));
        assert!(!l.shift_w(6000.0), "the second press came too late");
        assert!(l.locked);
        assert!(l.shift_w(6500.0));
    }

    #[test]
    fn both_sticks_inward_for_a_second_arms_once() {
        let read = PadRead::of([1.0, 0.0, -1.0, 0.0]);
        assert!(read.inward);
        let mut g = ArmGesture::default();
        let s = g.step(0.0, &read, true, false);
        assert!(!s.arm);
        assert!(s.left.is_centre() && s.right.is_centre(), "silent while held");
        assert_eq!(g.message(true, true, false), "Keep holding to arm.");
        assert!((g.progress(500.0, false) - 0.5).abs() < 1e-9);
        assert!(!g.step(900.0, &read, true, false).arm);
        assert!(g.step(1000.0, &read, true, false).arm);
        assert!(!g.step(1200.0, &read, true, false).arm, "asked once");
        assert_eq!(g.message(true, true, false), "Arming.");
        assert_eq!(g.progress(1200.0, false), 1.0);
    }

    #[test]
    fn after_the_arm_the_pad_flies_nothing_until_both_sticks_are_let_go() {
        let hold = PadRead::of([1.0, 0.0, -1.0, 0.0]);
        let mut g = ArmGesture::default();
        g.step(0.0, &hold, true, false);
        g.step(1000.0, &hold, true, false);
        // Armed now, the sticks still full inward: gated.
        let s = g.step(1100.0, &hold, true, true);
        assert!(g.gated && s.left.is_centre() && s.right.is_centre());
        assert_eq!(g.message(true, true, true), "Armed. Let go of both sticks to fly.");
        let centre = PadRead::of([0.0, 0.0, 0.0, 0.0]);
        g.step(1200.0, &centre, true, true);
        assert!(!g.gated);
        assert_eq!(g.message(true, true, true), "Armed.");
        let push = PadRead::of([0.0, -0.8, 0.0, -0.8]);
        let s = g.step(1300.0, &push, true, true);
        assert!(s.left.y > 0.0 && s.right.y > 0.0);
    }

    #[test]
    fn inward_sticks_in_the_air_are_a_real_input_and_never_disarm() {
        let read = PadRead::of([1.0, 0.0, -1.0, 0.0]);
        let mut g = ArmGesture::default();
        let s = g.step(0.0, &read, true, true);
        assert_eq!(s.left, Stick { x: 1.0, y: 0.0 });
        assert_eq!(s.right, Stick { x: -1.0, y: 0.0 });
        assert!(!s.arm && g.since.is_none());
    }

    #[test]
    fn letting_go_early_or_losing_the_link_forgets_the_hold() {
        let hold = PadRead::of([1.0, 0.0, -1.0, 0.0]);
        let mut g = ArmGesture::default();
        g.step(0.0, &hold, true, false);
        g.step(500.0, &PadRead::default(), true, false);
        assert!(g.since.is_none());
        g.step(600.0, &hold, true, false);
        assert!(
            !g.step(1500.0, &hold, true, false).arm,
            "the clock started again at 600"
        );
        g.step(1600.0, &hold, false, false);
        assert!(g.since.is_none());
        assert_eq!(g.message(true, false, false), "Gamepad ready. No link.");
        assert_eq!(
            g.message(false, true, false),
            "Plug in a gamepad and press any button on it."
        );
    }
}
