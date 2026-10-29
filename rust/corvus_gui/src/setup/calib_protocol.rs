//! The calibration protocol as a pure state machine (`src/js/calib-protocol.js`).
//!
//! Both flight stacks drive a calibration over STATUSTEXT: they say which side
//! they want next, when to hold still, how far along they are, and whether
//! they finished. `parse_line` turns one line into an event and a `Session`
//! turns those events into what the wizard shows. Every pattern is matched
//! loosely, so a wording change across firmwares degrades to "message shown
//! verbatim, no state change", never to a wrong instruction.
//!
//! ArduPilot does not detect an accelerometer position the way PX4 does: it
//! asks, then waits for `MAV_CMD_ACCELCAL_VEHICLE_POS`. The `Place` event
//! carries the pose to draw and the token the confirmation is sent with.

use std::sync::OnceLock;

use regex::Regex;

/// The six attitudes the autopilot asks for, in the web figure's own names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pose {
    Level,
    NoseDown,
    TailDown,
    Left,
    Right,
    UpsideDown,
}

impl Pose {
    /// The figure's drawable attitudes, in its order.
    #[cfg(test)]
    pub const ALL: [Pose; 6] = [
        Pose::Level,
        Pose::NoseDown,
        Pose::TailDown,
        Pose::Left,
        Pose::Right,
        Pose::UpsideDown,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Pose::Level => "level",
            Pose::NoseDown => "nose_down",
            Pose::TailDown => "tail_down",
            Pose::Left => "left",
            Pose::Right => "right",
            Pose::UpsideDown => "upside_down",
        }
    }

    /// `poseLabel`.
    pub fn label(self) -> &'static str {
        match self {
            Pose::Level => "Level",
            Pose::NoseDown => "Nose down",
            Pose::TailDown => "Tail down",
            Pose::Left => "Left side down",
            Pose::Right => "Right side down",
            Pose::UpsideDown => "Upside down",
        }
    }

    /// `poseHint`.
    pub fn hint(self) -> &'static str {
        match self {
            Pose::Level => "Flat, nose forward.",
            Pose::NoseDown => "On its nose, tail up.",
            Pose::TailDown => "On its tail, nose up.",
            Pose::Left => "Roll onto the left wing.",
            Pose::Right => "Roll onto the right wing.",
            Pose::UpsideDown => "Belly up, nose forward.",
        }
    }

    /// The token `POST /api/calibrate/position` takes.
    pub fn position(self) -> &'static str {
        match self {
            Pose::Level => "level",
            Pose::Left => "left",
            Pose::Right => "right",
            Pose::NoseDown => "nosedown",
            Pose::TailDown => "noseup",
            Pose::UpsideDown => "back",
        }
    }
}

/// `poseLabel` of a pose that may be missing: the web falls back to level.
pub fn label_of(p: Option<Pose>) -> &'static str {
    p.unwrap_or(Pose::Level).label()
}

/// `poseHint` of a pose that may be missing.
pub fn hint_of(p: Option<Pose>) -> &'static str {
    p.unwrap_or(Pose::Level).hint()
}

/// PX4's orientation vocabulary names the side that faces down, which is why
/// "up" means upside down and "down" means level.
pub fn pose_for(word: &str) -> Option<Pose> {
    match word.trim().to_lowercase().as_str() {
        "down" => Some(Pose::Level),
        "up" => Some(Pose::UpsideDown),
        "front" => Some(Pose::NoseDown),
        "back" => Some(Pose::TailDown),
        "left" => Some(Pose::Left),
        "right" => Some(Pose::Right),
        _ => None,
    }
}

const SIDE_WORDS: &str = "down|up|front|back|left|right";

/// One STATUSTEXT line as an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Cancelled,
    Done { sensor: String },
    Failed { sensor: String },
    Started,
    Progress(f64),
    Place { pose: Pose },
    SideCompleted(Option<Pose>),
    SideFailed(Option<Pose>),
    SideDone(Option<Pose>),
    Measure(Option<Pose>),
    RotateAround { pose: Option<Pose>, seconds: Option<u32> },
    Detected(Option<Pose>),
    Pending(Vec<Pose>),
    RotateTo(Option<Pose>),
    Hold,
    Prompt(&'static str),
    Note,
}

struct Patterns {
    cancelled: Regex,
    ok: Regex,
    fail: Regex,
    done: Regex,
    failed: Regex,
    started: Regex,
    progress: Regex,
    place: Vec<(Regex, Pose)>,
    completed: Regex,
    not_enough: Regex,
    side_done: Regex,
    measure: Regex,
    continue_rotation: Regex,
    detected: Regex,
    pending: Regex,
    rotate_to: Regex,
    rotate_around: Regex,
    rotate_other: Regex,
    hold: Regex,
    battery_off: Regex,
    battery_on: Regex,
    blow: Regex,
    shield: Regex,
    short_started: Regex,
    short_done: Regex,
    rotate_vehicle: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| {
        let r = |s: &str| Regex::new(s).expect("a pattern of the protocol");
        let sides = SIDE_WORDS;
        Patterns {
            cancelled: r(r"calibration\s+(?:was\s+)?cancell?ed"),
            ok: r(r"calibration successful"),
            fail: r(r"calibration (?:failed|unsuccessful)"),
            done: r(r"calibration\s+(?:done|finished|successful)\b[: ]*\s*(\w+)?"),
            failed: r(r"calibration\s+(?:failed|aborted)\b[: ]*\s*(\w+)?"),
            started: r(r"calibration\s+started[: ]*\s*(?:\d+\s+)?(\w+)?"),
            progress: r(r"progress\s*<?\s*(\d{1,3})"),
            place: [
                (r"place vehicle level", Pose::Level),
                (r"place vehicle on its left side", Pose::Left),
                (r"place vehicle on its right side", Pose::Right),
                (r"place vehicle nose down", Pose::NoseDown),
                (r"place vehicle nose up", Pose::TailDown),
                (r"place vehicle on its back", Pose::UpsideDown),
            ]
            .into_iter()
            .map(|(p, pose)| (r(p), pose))
            .collect(),
            completed: r(&format!(r"({sides})\s+side\s+already\s+completed")),
            not_enough: r(&format!(r"not enough measurements for\s+({sides})\s+side")),
            side_done: r(&format!(r"({sides})\s+side\s+done")),
            measure: r(&format!(r"hold still,?\s+measuring\s+({sides})\s+side")),
            continue_rotation: r(&format!(r"continue rotation for\s+({sides})(?:\s+side)?\s*(\d+)?")),
            detected: r(&format!(r"({sides})\s+orientation detected")),
            pending: r(r"rotate to a pending side[: ]+(.+)$"),
            rotate_to: r(&format!(r"rotate to\s+(?:the\s+)?({sides})\s+side")),
            rotate_around: r(r"rotate vehicle around the detected orientation"),
            rotate_other: r(r"rotate to a different side|rotate the vehicle"),
            hold: r(r"hold still|hold the vehicle still|keep the vehicle still"),
            battery_off: r(r"disconnect (?:the )?battery"),
            battery_on: r(r"\bconnect (?:the )?battery"),
            blow: r(r"blow into"),
            shield: r(r"(keep wind away|not measuring wind|shield.*wind)"),
            short_started: r(r"calibrating (?:gyros|barometer|compass)"),
            short_done: r(r"(?:gyro|barometer|compass)[^.]*calibration complete"),
            rotate_vehicle: r(r"(?:rotate|turn) (?:the )?vehicle (?:around|about)"),
        }
    })
}

fn sensor_of(c: &regex::Captures) -> String {
    c.get(1).map_or(String::new(), |m| m.as_str().to_string())
}

/// `parseLine`: one STATUSTEXT line as an event, `None` when it is not
/// calibration traffic.
pub fn parse_line(raw: &str) -> Option<Event> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let low = text.to_lowercase();
    let p = patterns();
    let pose_at = |c: &regex::Captures, i: usize| c.get(i).and_then(|m| pose_for(m.as_str()));
    // terminal outcomes first: they win over position wording on the same line
    if p.cancelled.is_match(&low) {
        return Some(Event::Cancelled);
    }
    if p.ok.is_match(&low) {
        return Some(Event::Done { sensor: String::new() });
    }
    if p.fail.is_match(&low) {
        return Some(Event::Failed { sensor: String::new() });
    }
    if low.contains("esc calibration finished") {
        return Some(Event::Done { sensor: "motor".into() });
    }
    if let Some(c) = p.done.captures(&low) {
        return Some(Event::Done { sensor: sensor_of(&c) });
    }
    if let Some(c) = p.failed.captures(&low) {
        return Some(Event::Failed { sensor: sensor_of(&c) });
    }
    if p.started.is_match(&low) {
        return Some(Event::Started);
    }
    if let Some(c) = p.progress.captures(&low) {
        let v: f64 = c[1].parse().unwrap_or(0.0);
        return Some(Event::Progress(v.clamp(0.0, 100.0)));
    }
    for (re, pose) in &p.place {
        if re.is_match(&low) {
            return Some(Event::Place { pose: *pose });
        }
    }
    if let Some(c) = p.completed.captures(&low) {
        return Some(Event::SideCompleted(pose_at(&c, 1)));
    }
    if let Some(c) = p.not_enough.captures(&low) {
        return Some(Event::SideFailed(pose_at(&c, 1)));
    }
    if let Some(c) = p.side_done.captures(&low) {
        return Some(Event::SideDone(pose_at(&c, 1)));
    }
    if let Some(c) = p.measure.captures(&low) {
        return Some(Event::Measure(pose_at(&c, 1)));
    }
    if let Some(c) = p.continue_rotation.captures(&low) {
        return Some(Event::RotateAround {
            pose: pose_at(&c, 1),
            seconds: c.get(2).and_then(|m| m.as_str().parse().ok()),
        });
    }
    if let Some(c) = p.detected.captures(&low) {
        return Some(Event::Detected(pose_at(&c, 1)));
    }
    if let Some(c) = p.pending.captures(&low) {
        let poses = c[1].split([',', ';']).filter_map(|w| pose_for(w.trim())).collect();
        return Some(Event::Pending(poses));
    }
    if let Some(c) = p.rotate_to.captures(&low) {
        return Some(Event::RotateTo(pose_at(&c, 1)));
    }
    if p.rotate_around.is_match(&low) {
        return Some(Event::RotateAround {
            pose: None,
            seconds: None,
        });
    }
    if p.rotate_other.is_match(&low) {
        return Some(Event::Pending(Vec::new()));
    }
    if p.hold.is_match(&low) {
        return Some(Event::Hold);
    }
    if p.battery_off.is_match(&low) {
        return Some(Event::Prompt("battery_off"));
    }
    if p.battery_on.is_match(&low) {
        return Some(Event::Prompt("battery_on"));
    }
    if p.blow.is_match(&low) {
        return Some(Event::Prompt("blow"));
    }
    if p.shield.is_match(&low) {
        return Some(Event::Prompt("shield"));
    }
    if p.short_started.is_match(&low) {
        return Some(Event::Started);
    }
    if p.short_done.is_match(&low) {
        return Some(Event::Done { sensor: String::new() });
    }
    if p.rotate_vehicle.is_match(&low) {
        return Some(Event::RotateAround {
            pose: None,
            seconds: None,
        });
    }
    if low.starts_with("[cal]") {
        return Some(Event::Note);
    }
    None
}

/// One calibration the page offers.
pub struct Procedure {
    pub kind: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    pub summary: &'static str,
    pub duration: &'static str,
    pub danger: bool,
    pub reboot: bool,
    pub poses: &'static [Pose],
    pub start_pose: Pose,
    pub spin: bool,
    pub marker: Option<&'static str>,
    pub brief: &'static str,
    pub prep: &'static [(&'static str, &'static str)],
}

const SIX: [Pose; 6] = [
    Pose::Level,
    Pose::Left,
    Pose::Right,
    Pose::NoseDown,
    Pose::TailDown,
    Pose::UpsideDown,
];

/// The seven calibrations, in the order the page lists them (`ORDER`).
pub static PROCEDURES: [Procedure; 7] = [
    Procedure {
        kind: "accel",
        label: "Accelerometer",
        icon: "move-3d",
        summary: "Teaches the autopilot where level is, on all three axes.",
        duration: "2 to 3 min",
        danger: false,
        reboot: true,
        poses: &SIX,
        start_pose: Pose::Level,
        spin: false,
        marker: None,
        brief: "Six positions. Hold each one still until it is ticked off.",
        prep: &[
            ("separator-horizontal", "Flat, stable surface"),
            ("hand", "Hold each side still"),
            ("zap", "Turn briskly between sides"),
        ],
    },
    Procedure {
        kind: "compass",
        label: "Compass",
        icon: "compass",
        summary: "Maps the magnetometer's iron distortion.",
        duration: "3 to 5 min",
        danger: false,
        reboot: true,
        poses: &SIX,
        start_pose: Pose::Level,
        spin: true,
        marker: None,
        brief: "Six positions. Turn slowly in each one until it is ticked off.",
        prep: &[
            ("trees", "Outside, away from metal"),
            ("magnet", "No watch or phone on you"),
            ("refresh-cw", "One turn every 4 s"),
        ],
    },
    Procedure {
        kind: "level",
        label: "Level Horizon",
        icon: "separator-horizontal",
        summary: "Trims the attitude so a level aircraft reads zero.",
        duration: "< 30 s",
        danger: false,
        reboot: false,
        poses: &[],
        start_pose: Pose::Level,
        spin: false,
        marker: None,
        brief: "Set it down level and let go.",
        prep: &[("separator-horizontal", "Level flight attitude"), ("hand", "Hands off")],
    },
    Procedure {
        kind: "gyro",
        label: "Gyroscope",
        icon: "rotate-3d",
        summary: "Zeroes the rate gyro bias.",
        duration: "< 30 s",
        danger: false,
        reboot: false,
        poses: &[],
        start_pose: Pose::Level,
        spin: false,
        marker: None,
        brief: "Keep it perfectly still.",
        prep: &[("vibrate", "No vibration"), ("hand", "Hands off")],
    },
    Procedure {
        kind: "baro",
        label: "Barometer",
        icon: "gauge",
        summary: "Re-zeroes the pressure sensor.",
        duration: "< 30 s",
        danger: false,
        reboot: false,
        poses: &[],
        start_pose: Pose::Level,
        spin: false,
        marker: None,
        brief: "Keep it still, out of wind.",
        prep: &[("wind", "Still air, no propwash"), ("hand", "Sensor port uncovered")],
    },
    Procedure {
        kind: "airspeed",
        label: "Airspeed",
        icon: "wind",
        summary: "Zeroes the differential pressure sensor.",
        duration: "< 1 min",
        danger: false,
        reboot: false,
        poses: &[],
        start_pose: Pose::Level,
        spin: false,
        marker: Some("nose"),
        brief: "Shield the pitot. Blow into it when asked.",
        prep: &[
            ("shield", "Shield the pitot from wind"),
            ("wind", "Blow into it when asked"),
            ("ban", "Never touch the tube"),
        ],
    },
    Procedure {
        kind: "motor",
        label: "Motors / ESC",
        icon: "fan",
        summary: "Teaches the ESCs the throttle end points.",
        duration: "1 to 2 min",
        danger: true,
        reboot: false,
        poses: &[],
        start_pose: Pose::Level,
        spin: false,
        marker: None,
        brief: "Props off. Motors will spin.",
        prep: &[
            ("fan", "ALL propellers removed"),
            ("unplug", "Battery disconnected"),
            ("plug-zap", "Connect only when asked"),
        ],
    },
];

/// The procedure of a type.
#[cfg(test)]
pub fn procedure(kind: &str) -> Option<&'static Procedure> {
    PROCEDURES.iter().find(|p| p.kind == kind)
}

/// `ACTION_TEXT`.
fn action_text(action: &str) -> Option<&'static str> {
    match action {
        "battery_on" => Some("Connect the flight battery now."),
        "battery_off" => Some("Disconnect the flight battery."),
        "blow" => Some("Blow into the pitot tube."),
        "shield" => Some("Shield the pitot tube."),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Starting,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl Phase {
    pub fn key(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Starting => "starting",
            Phase::Running => "running",
            Phase::Done => "done",
            Phase::Failed => "failed",
            Phase::Cancelled => "cancelled",
        }
    }

    pub fn terminal(self) -> bool {
        matches!(self, Phase::Done | Phase::Failed | Phase::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Pending,
    Active,
    Done,
    Failed,
}

/// The placement the autopilot waits to be told about.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Confirm {
    pub pose: Pose,
    pub position: &'static str,
}

/// What the wizard renders.
#[derive(Debug, Clone)]
pub struct State {
    pub phase: Phase,
    pub headline: String,
    pub detail: String,
    pub progress: Option<f64>,
    pub pose: Pose,
    pub from: Pose,
    /// A cue word: `idle`, `wait`, `rotate`, `hold`, `spin`, `warn`,
    /// `battery_on`, `battery_off`, `blow`, `shield`, `done`, `failed`,
    /// `cancelled`.
    pub cue: String,
    pub spin: bool,
    pub marker: Option<&'static str>,
    pub confirm: Option<Confirm>,
    pub sides: Vec<(Pose, Side)>,
    pub last_event_at: f64,
    pub seen_vehicle_message: bool,
}

impl State {
    pub fn side(&self, pose: Pose) -> Side {
        self.sides.iter().find(|s| s.0 == pose).map_or(Side::Pending, |s| s.1)
    }
}

/// A running calibration, fed one STATUSTEXT line at a time.
pub struct Session {
    pub procedure: &'static Procedure,
    state: State,
}

impl Session {
    pub fn new(proc: &'static Procedure) -> Session {
        Session {
            procedure: proc,
            state: Self::fresh(proc),
        }
    }

    fn fresh(proc: &'static Procedure) -> State {
        State {
            phase: Phase::Idle,
            headline: proc.brief.into(),
            detail: String::new(),
            progress: None,
            pose: proc.start_pose,
            from: proc.start_pose,
            cue: "idle".into(),
            spin: false,
            marker: proc.marker,
            confirm: None,
            sides: proc.poses.iter().map(|p| (*p, Side::Pending)).collect(),
            last_event_at: 0.0,
            seen_vehicle_message: false,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn reset(&mut self) {
        self.state = Self::fresh(self.procedure);
    }

    /// Called as the start command goes out, and again once the vehicle has
    /// acknowledged it: a session the autopilot has already spoken in is left
    /// where it is.
    pub fn begin(&mut self, now: f64) -> bool {
        let s = &mut self.state;
        if s.seen_vehicle_message || !matches!(s.phase, Phase::Idle | Phase::Starting) {
            return false;
        }
        s.phase = Phase::Starting;
        s.cue = "wait".into();
        s.headline = "Waiting for the autopilot\u{2026}".into();
        s.detail.clear();
        s.progress = None;
        s.confirm = None;
        s.last_event_at = now;
        true
    }

    fn mark_side(&mut self, pose: Option<Pose>, value: Side) {
        if let Some(p) = pose {
            if let Some(s) = self.state.sides.iter_mut().find(|s| s.0 == p) {
                s.1 = value;
            }
        }
    }

    /// Anything not yet done becomes the active one; keeps the strip honest.
    fn activate(&mut self, pose: Option<Pose>) {
        let Some(p) = pose else { return };
        for s in &mut self.state.sides {
            if s.1 == Side::Active {
                s.1 = Side::Pending;
            }
        }
        if let Some(s) = self.state.sides.iter_mut().find(|s| s.0 == p) {
            if s.1 != Side::Done {
                s.1 = Side::Active;
            }
        }
        self.state.pose = p;
    }

    fn rotate_to(pose: Option<Pose>) -> String {
        match pose {
            Some(p) => format!("Rotate to {}", p.label().to_lowercase()),
            None => "Rotate to an open side".into(),
        }
    }

    /// Feeds one STATUSTEXT line; true when the visible state changed.
    pub fn ingest(&mut self, text: &str, now: f64) -> bool {
        let Some(ev) = parse_line(text) else {
            return false;
        };
        let proc = self.procedure;
        {
            let s = &mut self.state;
            s.last_event_at = now;
            s.seen_vehicle_message = true;
            if matches!(s.phase, Phase::Idle | Phase::Starting) {
                s.phase = Phase::Running;
            }
            // any new word supersedes an outstanding placement prompt
            s.confirm = None;
        }
        match ev {
            Event::Started => {
                let s = &mut self.state;
                s.cue = "wait".into();
                s.headline = "Calibration running".into();
                s.detail.clear();
                s.progress = Some(0.0);
            }
            Event::Progress(v) => self.state.progress = Some(v),
            Event::Measure(pose) => {
                self.activate(pose);
                let s = &mut self.state;
                if let Some(p) = pose {
                    s.from = p;
                }
                s.spin = false;
                s.cue = "hold".into();
                s.headline = "Hold still".into();
                s.detail = label_of(pose).into();
            }
            Event::RotateAround { pose, seconds } => {
                if let Some(p) = pose {
                    self.activate(Some(p));
                    self.state.from = p;
                }
                let s = &mut self.state;
                s.spin = true;
                s.cue = "spin".into();
                s.headline = "Keep rotating".into();
                s.detail = match seconds {
                    Some(n) if n > 0 => format!("About {n} s left"),
                    _ => String::new(),
                };
            }
            Event::Detected(pose) => {
                self.activate(pose);
                let s = &mut self.state;
                if let Some(p) = pose {
                    s.from = p;
                }
                s.spin = proc.spin;
                s.cue = if proc.spin { "spin" } else { "hold" }.into();
                s.headline = if proc.spin { "Keep rotating" } else { "Hold still" }.into();
                s.detail = label_of(pose).into();
            }
            Event::RotateTo(pose) => {
                self.activate(pose);
                let s = &mut self.state;
                s.spin = false;
                s.cue = "rotate".into();
                s.headline = Self::rotate_to(pose);
                s.detail = hint_of(pose).into();
            }
            Event::SideDone(pose) => {
                self.mark_side(pose, Side::Done);
                {
                    let s = &mut self.state;
                    if let Some(p) = pose {
                        s.from = p;
                    }
                    s.spin = false;
                    s.cue = "rotate".into();
                }
                // on to the next position still outstanding, without waiting
                // for the autopilot's own "rotate to a pending side"
                let next = proc
                    .poses
                    .iter()
                    .copied()
                    .find(|p| self.state.side(*p) == Side::Pending);
                if let Some(n) = next {
                    self.activate(Some(n));
                    self.state.headline = Self::rotate_to(Some(n));
                    self.state.detail = n.hint().into();
                } else {
                    self.state.headline = Self::rotate_to(None);
                    self.state.detail.clear();
                }
            }
            Event::Place { pose } => {
                self.activate(Some(pose));
                let s = &mut self.state;
                s.spin = false;
                s.cue = "rotate".into();
                s.headline = Self::rotate_to(Some(pose));
                s.detail = format!("{} Then confirm.", pose.hint());
                s.confirm = Some(Confirm {
                    pose,
                    position: pose.position(),
                });
            }
            Event::SideCompleted(pose) => {
                self.mark_side(pose, Side::Done);
                self.state.detail = format!("{} already done.", label_of(pose));
            }
            Event::SideFailed(pose) => {
                self.mark_side(pose, Side::Failed);
                let s = &mut self.state;
                s.cue = "warn".into();
                s.headline = "Not enough measurements".into();
                s.detail = format!("Hold {} more steadily.", label_of(pose).to_lowercase());
            }
            Event::Pending(poses) => {
                self.state.spin = false;
                self.state.cue = "rotate".into();
                let next = poses.iter().copied().find(|p| self.state.side(*p) != Side::Done);
                if next.is_some() {
                    self.activate(next);
                }
                self.state.headline = Self::rotate_to(next);
                self.state.detail = next.map_or(String::new(), |p| p.hint().into());
            }
            Event::Hold => {
                let s = &mut self.state;
                s.spin = false;
                s.cue = "hold".into();
                s.headline = "Hold still".into();
                s.detail.clear();
            }
            Event::Prompt(action) => {
                let s = &mut self.state;
                s.cue = action.into();
                s.headline = action_text(action).map_or_else(|| text.trim().to_string(), String::from);
                s.detail.clear();
            }
            Event::Done { .. } => {
                let s = &mut self.state;
                s.phase = Phase::Done;
                s.spin = false;
                s.cue = "done".into();
                s.progress = Some(100.0);
                for side in &mut s.sides {
                    side.1 = Side::Done;
                }
                s.headline = "Calibration complete".into();
                s.detail = if proc.reboot {
                    "Reboot to apply."
                } else {
                    "New values active."
                }
                .into();
            }
            Event::Failed { .. } => {
                let s = &mut self.state;
                s.phase = Phase::Failed;
                s.spin = false;
                s.cue = "failed".into();
                s.headline = "Calibration failed".into();
                s.detail = text.trim().to_string();
            }
            Event::Cancelled => {
                let s = &mut self.state;
                s.phase = Phase::Cancelled;
                s.spin = false;
                s.cue = "cancelled".into();
                s.headline = "Calibration cancelled".into();
                s.detail.clear();
            }
            Event::Note => self.state.detail = text.trim().to_string(),
        }
        true
    }

    /// A terminal state the operator, not the autopilot, caused.
    pub fn finish(&mut self, phase: Phase, headline: &str, detail: &str) {
        let s = &mut self.state;
        s.phase = phase;
        s.cue = phase.key().into();
        s.spin = false;
        s.confirm = None;
        s.headline = headline.into();
        s.detail = detail.into();
    }

    /// The operator answered a placement prompt: the side is marked measured
    /// and the prompt cleared at once, so the confirm button cannot be pressed
    /// twice while the autopilot measures.
    pub fn confirm_placement(&mut self) -> bool {
        let Some(c) = self.state.confirm else {
            return false;
        };
        self.mark_side(Some(c.pose), Side::Done);
        let s = &mut self.state;
        s.from = c.pose;
        s.confirm = None;
        s.cue = "hold".into();
        s.headline = "Hold still".into();
        s.detail.clear();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// What the web page's own parser and session answered for the same
    /// lines (`calib_protocol_cases.json`, made by running calib-protocol.js).
    const CASES: &str = include_str!("calib_protocol_cases.json");

    fn pose_key(p: Option<Pose>) -> Value {
        p.map_or(Value::Null, |p| json!(p.key()))
    }

    fn summary(e: &Event) -> Value {
        match e {
            Event::Cancelled => json!({"kind": "cancelled"}),
            Event::Done { sensor } => json!({"kind": "done", "sensor": sensor}),
            Event::Failed { sensor } => json!({"kind": "failed", "sensor": sensor}),
            Event::Started => json!({"kind": "started"}),
            Event::Progress(v) => json!({"kind": "progress", "value": v}),
            Event::Place { pose } => json!({"kind": "place", "pose": pose.key(), "position": pose.position()}),
            Event::SideCompleted(p) => json!({"kind": "side_completed", "pose": pose_key(*p)}),
            Event::SideFailed(p) => json!({"kind": "side_failed", "pose": pose_key(*p)}),
            Event::SideDone(p) => json!({"kind": "side_done", "pose": pose_key(*p)}),
            Event::Measure(p) => json!({"kind": "measure", "pose": pose_key(*p)}),
            Event::RotateAround { pose, seconds } => {
                json!({"kind": "rotate_around", "pose": pose_key(*pose), "seconds": seconds})
            }
            Event::Detected(p) => json!({"kind": "detected", "pose": pose_key(*p)}),
            Event::Pending(ps) => json!({"kind": "pending", "poses": ps.iter().map(|p| p.key()).collect::<Vec<_>>()}),
            Event::RotateTo(p) => json!({"kind": "rotate_to", "pose": pose_key(*p)}),
            Event::Hold => json!({"kind": "hold"}),
            Event::Prompt(a) => json!({"kind": "prompt", "action": a}),
            Event::Note => json!({"kind": "note"}),
        }
    }

    /// Whether the Rust event says what the web event said: every field the
    /// web event has, except the sensor of a start, which nothing reads.
    fn same(web: &Value, ours: &Value) -> bool {
        let Some(map) = web.as_object() else {
            return false;
        };
        map.iter().all(|(k, v)| {
            if k == "sensor" && web["kind"] == "started" {
                return true;
            }
            if *v == Value::Null {
                return ours.get(k).is_none_or(|o| *o == Value::Null);
            }
            match (v.as_f64(), ours.get(k).and_then(Value::as_f64)) {
                (Some(a), Some(b)) => a == b,
                _ => ours.get(k) == Some(v),
            }
        })
    }

    #[test]
    fn every_line_is_read_as_the_web_page_reads_it() {
        let cases: Value = serde_json::from_str(CASES).unwrap();
        let lines = cases["lines"].as_array().unwrap();
        assert!(lines.len() > 150);
        for case in lines {
            let line = case["line"].as_str().unwrap();
            let got = parse_line(line);
            match (&case["event"], got) {
                (Value::Null, None) => {}
                (web, Some(ev)) => {
                    assert!(same(web, &summary(&ev)), "{line:?}: web {web}, here {ev:?}");
                }
                (web, None) => panic!("{line:?}: web {web}, here nothing"),
            }
            if case["event"] == Value::Null {
                assert!(parse_line(line).is_none(), "{line:?} should be ignored");
            }
        }
    }

    fn pose_of(v: &Value) -> Pose {
        Pose::ALL.into_iter().find(|p| p.key() == v.as_str().unwrap()).unwrap()
    }

    #[test]
    fn a_session_moves_as_the_web_session_moves() {
        let cases: Value = serde_json::from_str(CASES).unwrap();
        for sc in cases["sessions"].as_array().unwrap() {
            let kind = sc["type"].as_str().unwrap();
            let mut s = Session::new(procedure(kind).unwrap());
            let mut t = 1000.0;
            for step in sc["steps"].as_array().unwrap() {
                let line = step["line"].as_str().unwrap();
                let changed = match line {
                    "<begin>" => s.begin(t),
                    "<confirm>" => s.confirm_placement(),
                    l => {
                        t += 1000.0;
                        s.ingest(l, t)
                    }
                };
                let w = &step["state"];
                let st = s.state();
                let at = format!("{kind} after {line:?}");
                if line != "<begin>" {
                    assert_eq!(changed, step["changed"].as_bool().unwrap(), "{at}: changed");
                }
                assert_eq!(st.phase.key(), w["phase"], "{at}: phase");
                assert_eq!(st.headline, w["headline"].as_str().unwrap(), "{at}: headline");
                assert_eq!(st.detail, w["detail"].as_str().unwrap(), "{at}: detail");
                assert_eq!(st.progress, w["progress"].as_f64(), "{at}: progress");
                assert_eq!(st.pose, pose_of(&w["pose"]), "{at}: pose");
                assert_eq!(st.from, pose_of(&w["from"]), "{at}: from");
                assert_eq!(st.cue, w["cue"].as_str().unwrap(), "{at}: cue");
                assert_eq!(st.spin, w["spin"].as_bool().unwrap(), "{at}: spin");
                assert_eq!(st.seen_vehicle_message, w["seen"].as_bool().unwrap(), "{at}: seen");
                match &w["confirm"] {
                    Value::Null => assert!(st.confirm.is_none(), "{at}: confirm"),
                    c => {
                        let got = st.confirm.expect("a prompt to confirm");
                        assert_eq!(got.pose.key(), c["pose"].as_str().unwrap(), "{at}");
                        assert_eq!(got.position, c["position"].as_str().unwrap(), "{at}");
                    }
                }
                for (pose, value) in w["sides"].as_object().unwrap() {
                    let side = st.side(pose_of(&json!(pose)));
                    let word = match side {
                        Side::Pending => "pending",
                        Side::Active => "active",
                        Side::Done => "done",
                        Side::Failed => "failed",
                    };
                    assert_eq!(word, value.as_str().unwrap(), "{at}: side {pose}");
                }
            }
        }
    }

    #[test]
    fn every_procedure_is_complete_and_drawable() {
        let order: Vec<&str> = PROCEDURES.iter().map(|p| p.kind).collect();
        assert_eq!(
            order,
            ["accel", "compass", "level", "gyro", "baro", "airspeed", "motor"]
        );
        for p in &PROCEDURES {
            assert!(!p.label.is_empty() && !p.summary.is_empty() && !p.brief.is_empty());
            assert!(p.prep.len() >= 2, "{} states its preconditions", p.kind);
            for pose in p.poses {
                assert!(Pose::ALL.contains(pose));
            }
        }
        assert!(procedure("motor").unwrap().danger);
        assert_eq!(procedure("accel").unwrap().poses.len(), 6);
        assert!(procedure("compass").unwrap().spin);
    }

    #[test]
    fn the_side_words_mean_the_side_that_faces_down() {
        assert_eq!(pose_for("down"), Some(Pose::Level));
        assert_eq!(pose_for("up"), Some(Pose::UpsideDown));
        assert_eq!(pose_for("front"), Some(Pose::NoseDown));
        assert_eq!(pose_for("back"), Some(Pose::TailDown));
        assert_eq!(pose_for("DOWN"), Some(Pose::Level));
        assert_eq!(pose_for("sideways"), None);
    }

    #[test]
    fn a_confirm_button_cannot_outlive_its_prompt() {
        let mut s = Session::new(procedure("accel").unwrap());
        s.begin(0.0);
        assert!(s.ingest("Place vehicle on its left side and press any key.", 1.0));
        assert_eq!(s.state().confirm.map(|c| c.position), Some("left"));
        assert!(s.confirm_placement());
        assert!(s.state().confirm.is_none());
        assert_eq!(s.state().side(Pose::Left), Side::Done);
        assert!(!s.confirm_placement(), "nothing left to confirm");
    }

    #[test]
    fn the_operator_can_end_a_session() {
        let mut s = Session::new(procedure("gyro").unwrap());
        s.begin(0.0);
        s.finish(
            Phase::Cancelled,
            "Calibration cancelled",
            "The vehicle stopped the calibration.",
        );
        assert_eq!(s.state().cue, "cancelled");
        assert!(s.state().phase.terminal());
        s.reset();
        assert_eq!(s.state().phase, Phase::Idle);
    }
}
