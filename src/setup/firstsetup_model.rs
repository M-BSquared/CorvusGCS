//! What First Setup says and decides (`python/src/js/setup-firstsetup.js` and
//! `first-setup.js`): the eight steps, which of them the vehicle shows as done,
//! the words of the aircraft and finish steps, what the operator has been
//! through, and the detector that notices a controller nobody has set up.
//!
//! The rules are held to `firstsetup_cases.json`, what the web helpers answered
//! for the same payloads.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde_json::{json, Value};

/// A step's state as the stepper shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Done,
    Todo,
    Pending,
}

/// The page a step hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPage {
    Firmware,
    Motors,
    Calibration,
    Radio,
    Safety,
    Battery,
}

/// What a step shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Page(HostPage),
    Aircraft,
    Finish,
}

/// One step of the wizard.
#[derive(Debug, Clone, Copy)]
pub struct Step {
    pub id: &'static str,
    pub label: &'static str,
    pub title: &'static str,
    pub text: &'static str,
    pub kind: Kind,
}

/// The steps, in the order a new controller needs them.
pub const STEPS: [Step; 8] = [
    Step {
        id: "firmware",
        label: "Firmware",
        title: "Install the firmware",
        text: "A new controller usually has no firmware, or not the one you want. Pick PX4 or ArduPilot below and flash it over the USB cable. If it already runs the firmware you want, continue.",
        kind: Kind::Page(HostPage::Firmware),
    },
    Step {
        id: "aircraft",
        label: "Aircraft",
        title: "Choose the aircraft type",
        text: "Tell the controller what it is flying. This decides which controllers run and how many motors it expects.",
        kind: Kind::Aircraft,
    },
    Step {
        id: "frame",
        label: "Frame and motors",
        title: "Frame, motors and positions",
        text: "Check the motor order and spin direction, and set where the flight controller and the GPS sit on the drawing of the aircraft. Test the motors with the propellers removed.",
        kind: Kind::Page(HostPage::Motors),
    },
    Step {
        id: "calibration",
        label: "Calibration",
        title: "Orientation and calibration",
        text: "First set how the controller is turned on the aircraft. Then calibrate the gyro, the accelerometer and the compass, and level the horizon. A changed rotation needs a reboot and a new calibration.",
        kind: Kind::Page(HostPage::Calibration),
    },
    Step {
        id: "radio",
        label: "Radio",
        title: "Radio control",
        text: "Bind the transmitter, calibrate the sticks and assign the flight mode switch. Skip this if you fly without a transmitter.",
        kind: Kind::Page(HostPage::Radio),
    },
    Step {
        id: "safety",
        label: "Safety",
        title: "Safety and failsafes",
        text: "Set what the aircraft does when the radio or the battery fails, and which checks must pass before it arms.",
        kind: Kind::Page(HostPage::Safety),
    },
    Step {
        id: "battery",
        label: "Battery",
        title: "Battery and power",
        text: "Enter the cell count and the capacity so the remaining charge is honest.",
        kind: Kind::Page(HostPage::Battery),
    },
    Step {
        id: "finish",
        label: "Finish",
        title: "Ready to fly?",
        text: "Here is what is done and what is still open.",
        kind: Kind::Finish,
    },
];

/// `indexOf`: an unknown id is the first step.
pub fn index_of(id: &str) -> usize {
    STEPS.iter().position(|s| s.id == id).unwrap_or(0)
}

/// The step `delta` away, when there is one.
pub fn step_at(current: &str, delta: i32) -> Option<usize> {
    let next = index_of(current) as i32 + delta;
    (0..STEPS.len() as i32).contains(&next).then_some(next as usize)
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

/// What the vehicle says is set up.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    pub airframe: String,
    pub accel: String,
    pub gyro: String,
    pub mag: String,
}

/// One aircraft type on offer.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub id: i64,
    pub label: String,
    pub group: String,
}

/// `GET /api/first-setup`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Payload {
    pub connected: bool,
    pub stack: String,
    pub facts: Facts,
    pub reasons: Vec<String>,
    pub prompt: bool,
    pub presets: Vec<Preset>,
    pub preset_current: Option<i64>,
    pub fingerprint: String,
}

impl Payload {
    pub fn from_json(v: &Value) -> Payload {
        let f = v.get("facts").cloned().unwrap_or(Value::Null);
        Payload {
            connected: v.get("connected") == Some(&Value::Bool(true)),
            stack: text(v, "stack"),
            facts: Facts {
                airframe: text(&f, "airframe"),
                accel: text(&f, "accel"),
                gyro: text(&f, "gyro"),
                mag: text(&f, "mag"),
            },
            reasons: v
                .get("reasons")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
                .unwrap_or_default(),
            prompt: v.get("prompt") == Some(&Value::Bool(true)),
            presets: v
                .get("presets")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|p| {
                            Some(Preset {
                                id: p.get("id").and_then(Value::as_i64)?,
                                label: text(p, "label"),
                                group: text(p, "group"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            preset_current: v.get("preset_current").and_then(Value::as_i64),
            fingerprint: text(v, "fingerprint"),
        }
    }
}

/// `calibrationState`: one word for the three sensor facts.
pub fn calibration_state(f: &Facts) -> SensorState {
    let list: Vec<&str> = [&f.accel, &f.gyro, &f.mag]
        .into_iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    if list.contains(&"todo") {
        SensorState::Todo
    } else if !list.is_empty() && list.iter().all(|s| *s == "done" || *s == "not_required") {
        SensorState::Done
    } else {
        SensorState::Unknown
    }
}

/// `calibrationState`'s three answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorState {
    Done,
    Todo,
    Unknown,
}

/// `stepStates`: the state of every step. What the vehicle knows (the
/// airframe, the calibrations, a link at all) wins over the operator having
/// been through a step.
pub fn step_states(payload: Option<&Payload>, visited: &BTreeSet<&'static str>, bootloader: bool) -> [State; 8] {
    let empty = Payload::default();
    let p = payload.unwrap_or(&empty);
    let mut out = [State::Pending; 8];
    for (i, s) in STEPS.iter().enumerate() {
        if visited.contains(s.id) {
            out[i] = State::Done;
        }
    }
    out[index_of("firmware")] = if !p.connected || bootloader {
        State::Todo
    } else {
        State::Done
    };
    match p.facts.airframe.as_str() {
        "done" => out[index_of("aircraft")] = State::Done,
        "todo" => out[index_of("aircraft")] = State::Todo,
        _ => {}
    }
    match calibration_state(&p.facts) {
        SensorState::Done => out[index_of("calibration")] = State::Done,
        SensorState::Todo => out[index_of("calibration")] = State::Todo,
        SensorState::Unknown => {}
    }
    out[index_of("finish")] = State::Pending;
    out
}

/// `initialStep`: the step to start on when nobody named one.
pub fn initial_step(payload: Option<&Payload>, bootloader: bool) -> &'static str {
    let empty = Payload::default();
    let p = payload.unwrap_or(&empty);
    if bootloader || !p.connected {
        return "firmware";
    }
    let states = step_states(Some(p), &BTreeSet::new(), false);
    if states[index_of("aircraft")] == State::Todo {
        "aircraft"
    } else if states[index_of("calibration")] == State::Todo {
        "calibration"
    } else {
        "aircraft"
    }
}

/// `presetLabel`: what was written, in words the operator recognises.
pub fn preset_label(payload: Option<&Payload>, id: i64) -> String {
    payload
        .and_then(|p| p.presets.iter().find(|x| x.id == id))
        .map(|x| x.label.clone())
        .unwrap_or_default()
}

/// What the operator has been through survives leaving the page and coming
/// back within the session, so a trip to Parameters in the middle of the setup
/// costs nothing. It is not kept past the program: a fresh start is a fresh
/// look at the controller.
#[derive(Debug, Default)]
pub struct Memory {
    pub visited: BTreeSet<&'static str>,
    pub current: String,
}

impl Memory {
    pub fn forget(&mut self) {
        self.visited.clear();
        self.current.clear();
    }
}

static MEMORY: OnceLock<Mutex<Memory>> = OnceLock::new();

/// The session's memory.
pub fn memory() -> MutexGuard<'static, Memory> {
    MEMORY
        .get_or_init(|| Mutex::new(Memory::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

// ----- the aircraft step -------------------------------------------------

/// What the aircraft step shows for the payload, and which buttons it has.
#[derive(Debug, Clone, PartialEq)]
pub struct AircraftView {
    pub status: String,
    pub note: String,
    pub show_apply: bool,
    pub apply_enabled: bool,
    pub show_reboot: bool,
    pub reboot_enabled: bool,
}

/// `paintAircraft` and `paintButtons`.
pub fn aircraft_view(
    payload: Option<&Payload>,
    armed: bool,
    written: bool,
    rebooting: bool,
    chosen: Option<i64>,
) -> AircraftView {
    let empty = Payload::default();
    let p = payload.unwrap_or(&empty);
    let status = if !p.connected {
        "No controller is connected yet. Install the firmware in the first step, then come back here.".to_string()
    } else if rebooting {
        "Waiting for the controller to restart. This takes a few seconds.".to_string()
    } else if let Some(current) = p.preset_current {
        let label = preset_label(Some(p), current);
        format!(
            "This controller is set to {}.",
            if label.is_empty() {
                format!("airframe {current}")
            } else {
                label
            }
        )
    } else if p.facts.airframe == "todo" {
        "No aircraft type is selected on this controller.".to_string()
    } else {
        "Pick the type that matches your aircraft.".to_string()
    };
    let ardupilot = p.stack == "ardupilot";
    let note = if p.connected && p.presets.is_empty() {
        if ardupilot {
            "ArduPilot Plane, Rover and Sub pick their type with the firmware you flashed. There is nothing to choose here. Continue with the frame and motors."
        } else {
            "This firmware offers no presets here. Continue with the next step."
        }
    } else if armed {
        "The aircraft is armed. Disarm it before changing the aircraft type."
    } else if written {
        "Written. The controller applies the new type after a reboot."
    } else if ardupilot {
        "Sets the frame class and type. Reboot the controller afterwards."
    } else {
        "Sets the airframe and resets the controller's parameters to the defaults of that airframe on the next boot. Reboot the controller afterwards."
    }
    .to_string();
    AircraftView {
        status,
        note,
        show_apply: !p.presets.is_empty(),
        apply_enabled: !(armed
            || !p.connected
            || chosen.is_none()
            || written
            || rebooting
            || chosen == p.preset_current),
        show_reboot: written,
        reboot_enabled: !(armed || rebooting),
    }
}

/// The question asked before an existing aircraft type is changed.
pub fn change_question(payload: Option<&Payload>, chosen: i64) -> String {
    format!(
        "This controller already has an aircraft type. Changing it to {} can reset settings you made before. Continue?",
        preset_label(payload, chosen)
    )
}

/// The body of `POST /api/first-setup/airframe`: a preset id from the table
/// the GET returned; the values written come from that table, never from here.
pub fn airframe_body(preset: i64) -> Value {
    json!({"preset": preset})
}

/// What the notification board is told when the type was written: the level
/// and the words.
pub fn written_note(answer: &Value) -> (&'static str, String) {
    match answer.get("warning").and_then(Value::as_str).filter(|w| !w.is_empty()) {
        Some(w) => ("warning", format!("The aircraft type is set, but: {w}")),
        None => (
            "info",
            "Aircraft type written. Reboot the controller to apply it.".into(),
        ),
    }
}

/// How the reboot is followed: the link has to go away and come back, and the
/// parameters are read again a moment after it does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reboot {
    Idle,
    /// Waiting for the link to drop (`went_down` false) or to return.
    Waiting {
        went_down: bool,
    },
    /// The link is back; read again at this time (ms).
    Settling {
        at: f64,
    },
}

/// The delay after the link returns before the controller is read again.
pub const REBOOT_SETTLE_MS: f64 = 2500.0;

impl Reboot {
    /// One telemetry frame. Returns true when the controller is to be read
    /// again now.
    pub fn follow(&mut self, connected: bool, now: f64) -> bool {
        match *self {
            Reboot::Idle => false,
            Reboot::Waiting { went_down } => {
                if !connected {
                    *self = Reboot::Waiting { went_down: true };
                } else if went_down {
                    *self = Reboot::Settling {
                        at: now + REBOOT_SETTLE_MS,
                    };
                }
                false
            }
            Reboot::Settling { at } => {
                if now >= at {
                    *self = Reboot::Idle;
                    true
                } else {
                    false
                }
            }
        }
    }

    pub fn active(&self) -> bool {
        *self != Reboot::Idle
    }
}

// ----- the finish step ---------------------------------------------------

/// One line of the summary.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryRow {
    pub id: &'static str,
    pub label: &'static str,
    pub state: State,
    pub word: &'static str,
    pub action: &'static str,
}

/// `paintFinish`: every step but the last, and how many are not done.
pub fn summary(states: &[State; 8]) -> (Vec<SummaryRow>, usize) {
    let mut open = 0;
    let rows = STEPS[..STEPS.len() - 1]
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let st = states[i];
            if st != State::Done {
                open += 1;
            }
            SummaryRow {
                id: s.id,
                label: s.label,
                state: st,
                word: match st {
                    State::Done => "Done",
                    State::Todo => "Needs attention",
                    State::Pending => "Not visited",
                },
                action: if st == State::Done { "Review" } else { "Open" },
            }
        })
        .collect();
    (rows, open)
}

/// The sentence under the summary.
pub fn finish_hint(open: usize) -> String {
    if open > 0 {
        format!(
            "{open}{} not marked done. You can finish now and come back from the Setup page at any time.",
            if open == 1 { " step is" } else { " steps are" }
        )
    } else {
        "Everything is done. Check the aircraft with the preflight checklist before the first flight.".into()
    }
}

// ----- the detector (first-setup.js) --------------------------------------

/// A fresh board's parameters are still arriving for a moment after the
/// heartbeat.
pub const CONNECT_SETTLE_MS: f64 = 3500.0;
/// A flash briefly shows the bootloader port too.
pub const BOOTLOADER_SETTLE_MS: f64 = 4000.0;
pub const RETRY_MS: f64 = 4000.0;
pub const MODAL_RETRY_MS: f64 = 5000.0;

/// `shouldPrompt`: whether a payload earns an unprompted dialog.
pub fn should_prompt(payload: Option<&Payload>, silenced: bool) -> bool {
    payload.is_some_and(|p| p.connected && p.prompt) && !silenced
}

/// `bootloaderDevice`: the device of a published `link_bootloader`, or "".
pub fn bootloader_device(state: &Value) -> String {
    state
        .get("link_bootloader")
        .and_then(|b| b.get("device"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// `freshReasons`: the dialog's reasons for a fresh controller.
pub fn fresh_reasons(payload: Option<&Payload>) -> Vec<String> {
    match payload {
        Some(p) if !p.reasons.is_empty() => p.reasons.clone(),
        _ => vec!["This controller looks like it has not been set up.".into()],
    }
}

/// The controllers this station was told to stop asking about, kept in
/// `~/.corvus/gui-first-setup.json` (the web page keeps them in localStorage:
/// losing them costs one more dialog, never a wrong answer).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Quiet {
    list: Vec<String>,
    path: Option<PathBuf>,
}

impl Quiet {
    pub fn default_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
        Some(PathBuf::from(home).join(".corvus").join("gui-first-setup.json"))
    }

    pub fn load(path: Option<PathBuf>) -> Quiet {
        let list = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| {
                v.get("quiet")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>())
            })
            .unwrap_or_default();
        Quiet { list, path }
    }

    pub fn is_silenced(&self, fingerprint: &str) -> bool {
        !fingerprint.is_empty() && self.list.iter().any(|f| f == fingerprint)
    }

    /// `silence`: stop offering the wizard for this controller; the last 50
    /// are kept.
    pub fn silence(&mut self, fingerprint: &str) {
        if fingerprint.is_empty() {
            return;
        }
        if !self.is_silenced(fingerprint) {
            self.list.push(fingerprint.to_string());
        }
        let keep = self.list.len().saturating_sub(50);
        self.list.drain(..keep);
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, json!({"quiet": self.list}).to_string());
    }
}

/// What the dialog says and offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub title: &'static str,
    pub lede: String,
    pub reasons: Vec<String>,
    pub note: &'static str,
    pub fingerprint: String,
    /// The step to start on, "" to let the wizard pick.
    pub step: &'static str,
}

/// `offerFresh`.
pub fn offer_fresh(p: &Payload) -> Offer {
    Offer {
        title: "New controller detected",
        lede: "This flight controller has not been set up yet.".into(),
        reasons: fresh_reasons(Some(p)),
        note: "The first setup takes you through the aircraft type, the frame and motors, the sensor calibration and the safety settings, one page after the other.",
        fingerprint: p.fingerprint.clone(),
        step: "",
    }
}

/// `offerBlank`.
pub fn offer_blank(device: &str) -> Offer {
    Offer {
        title: "Controller without firmware",
        lede: format!("A flight controller is waiting in its bootloader on {device}."),
        reasons: vec!["It has no firmware to talk to, so Corvus cannot connect to it yet.".into()],
        note: "The first setup starts by flashing PX4 or ArduPilot onto it, then goes on with the aircraft type and the calibration.",
        fingerprint: String::new(),
        step: "firmware",
    }
}

/// What the detector wants done next.
#[derive(Debug, Clone, PartialEq)]
pub enum Job {
    /// Read `GET /api/first-setup`; `may_retry` when an empty answer may be
    /// asked once more.
    Fresh { may_retry: bool },
    /// Read `GET /api/firmware/status` for this bootloader port.
    Bootloader(String),
}

/// `Corvus.firstSetup`: notices a controller nobody has set up, from the
/// telemetry transitions, and asks for the two answers that decide whether to
/// offer the wizard. Nothing polls: each check hangs off a transition (the
/// link, the bootloader port) and runs once, after a settle delay.
#[derive(Debug, Default)]
pub struct Watcher {
    last_connected: bool,
    last_bootloader: String,
    connect_due: Option<(f64, bool)>,
    boot_due: Option<(f64, String)>,
    shown_for: String,
    shown_bootloader: String,
    armed: bool,
    pub quiet: Quiet,
}

impl Watcher {
    pub fn new(quiet: Quiet) -> Watcher {
        Watcher {
            quiet,
            ..Watcher::default()
        }
    }

    /// `onState`: a telemetry frame.
    pub fn on_state(&mut self, state: &Value, now: f64) {
        let connected = state.get("connected") == Some(&Value::Bool(true));
        self.armed = state.get("armed") == Some(&Value::Bool(true));
        let waiting = bootloader_device(state);
        if connected && !self.last_connected {
            self.connect_due = Some((now + CONNECT_SETTLE_MS, true));
        } else if !connected && self.last_connected {
            self.connect_due = None;
            self.shown_for.clear();
        }
        self.last_connected = connected;
        if !waiting.is_empty() && waiting != self.last_bootloader {
            self.boot_due = Some((now + BOOTLOADER_SETTLE_MS, waiting.clone()));
        } else if waiting.is_empty() {
            self.boot_due = None;
            self.shown_bootloader.clear();
        }
        self.last_bootloader = waiting;
    }

    /// The timers: the check that is due now, if one is. `wizard_open` and
    /// `modal_open` hold a check back (a modal for five seconds, the wizard
    /// altogether).
    pub fn due(&mut self, now: f64, wizard_open: bool, modal_open: bool) -> Option<Job> {
        if let Some((at, may_retry)) = self.connect_due {
            if now >= at {
                self.connect_due = None;
                if !self.last_connected || self.armed || wizard_open {
                    return None;
                }
                if modal_open {
                    self.connect_due = Some((now + MODAL_RETRY_MS, may_retry));
                    return None;
                }
                return Some(Job::Fresh { may_retry });
            }
        }
        if let Some((at, device)) = self.boot_due.clone() {
            if now >= at {
                self.boot_due = None;
                if self.last_bootloader != device || self.last_connected || wizard_open {
                    return None;
                }
                if device == self.shown_bootloader {
                    return None;
                }
                if modal_open {
                    self.boot_due = Some((now + MODAL_RETRY_MS, device));
                    return None;
                }
                return Some(Job::Bootloader(device));
            }
        }
        None
    }

    /// The answer of `GET /api/first-setup`.
    pub fn fresh_answer(&mut self, payload: Option<&Payload>, may_retry: bool, now: f64) -> Option<Offer> {
        if !self.last_connected {
            return None;
        }
        let Some(p) = payload.filter(|p| p.connected) else {
            // the parameters were not there yet: once more, then quietly give up
            if may_retry {
                self.connect_due = Some((now + RETRY_MS, false));
            }
            return None;
        };
        if !p.fingerprint.is_empty() && p.fingerprint == self.shown_for {
            return None;
        }
        if !should_prompt(Some(p), self.quiet.is_silenced(&p.fingerprint)) {
            return None;
        }
        self.shown_for = p.fingerprint.clone();
        Some(offer_fresh(p))
    }

    /// The answer of `GET /api/firmware/status`: a flash in progress reboots
    /// its own board into the bootloader, and that is not a new controller.
    pub fn bootloader_answer(&mut self, device: &str, flash_state: &str) -> Option<Offer> {
        if self.last_bootloader != device {
            return None;
        }
        if flash_state == "downloading" || flash_state == "flashing" {
            return None;
        }
        self.shown_bootloader = device.to_string();
        Some(offer_blank(device))
    }
}

#[cfg(test)]
/// The session memory is one for the whole program, so the tests that touch
/// it take turns.
pub static MEMORY_TEST: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn cases() -> Value {
        serde_json::from_str(include_str!("firstsetup_cases.json")).unwrap()
    }

    fn payload(v: &Value) -> Option<Payload> {
        (!v.is_null()).then(|| Payload::from_json(v))
    }

    fn state_of(s: &str) -> State {
        match s {
            "done" => State::Done,
            "todo" => State::Todo,
            _ => State::Pending,
        }
    }

    fn visited(v: &Value) -> BTreeSet<&'static str> {
        v.as_array()
            .unwrap()
            .iter()
            .filter_map(|id| STEPS.iter().find(|s| Some(s.id) == id.as_str()).map(|s| s.id))
            .collect()
    }

    #[test]
    fn the_steps_and_their_words_are_the_web_pages() {
        let c = cases();
        let steps = c["steps"].as_array().unwrap();
        assert_eq!(steps.len(), STEPS.len());
        for (js, rust) in steps.iter().zip(STEPS.iter()) {
            assert_eq!(js["id"], rust.id);
            assert_eq!(js["label"], rust.label);
            assert_eq!(js["title"], rust.title);
            assert_eq!(js["text"], rust.text);
        }
    }

    #[test]
    fn the_sensor_facts_make_the_same_word_as_the_web_page() {
        let c = cases();
        for case in c["calibration"].as_array().unwrap() {
            let facts = payload(&json!({"facts": case["facts"]})).unwrap().facts;
            let want = match case["state"].as_str().unwrap() {
                "done" => SensorState::Done,
                "todo" => SensorState::Todo,
                _ => SensorState::Unknown,
            };
            assert_eq!(calibration_state(&facts), want, "{case}");
        }
    }

    #[test]
    fn every_step_has_the_state_the_web_page_gives_it() {
        let c = cases();
        let all = c["states"].as_array().unwrap();
        assert_eq!(all.len(), 90);
        for case in all {
            let got = step_states(
                payload(&case["payload"]).as_ref(),
                &visited(&case["visited"]),
                case["bootloader"] == true,
            );
            for (i, s) in STEPS.iter().enumerate() {
                assert_eq!(
                    got[i],
                    state_of(case["states"][s.id].as_str().unwrap()),
                    "{} in {case}",
                    s.id
                );
            }
        }
    }

    #[test]
    fn the_wizard_opens_where_the_work_is() {
        let c = cases();
        for case in c["initial"].as_array().unwrap() {
            assert_eq!(
                initial_step(payload(&case["payload"]).as_ref(), case["bootloader"] == true),
                case["step"].as_str().unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    fn a_dialog_is_earned_only_by_a_connected_fresh_controller_nobody_waved_away() {
        let c = cases();
        for case in c["prompt"].as_array().unwrap() {
            assert_eq!(
                should_prompt(payload(&case["payload"]).as_ref(), case["silenced"] == true),
                case["prompt"] == true,
                "{case}"
            );
        }
        for case in c["reasons"].as_array().unwrap() {
            let want: Vec<String> = case["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_str().unwrap().to_string())
                .collect();
            assert_eq!(fresh_reasons(payload(&case["payload"]).as_ref()), want, "{case}");
        }
    }

    #[test]
    fn a_preset_is_named_as_the_web_page_names_it() {
        let c = cases();
        for case in c["labels"].as_array().unwrap() {
            assert_eq!(
                preset_label(payload(&case["payload"]).as_ref(), case["id"].as_i64().unwrap()),
                case["label"].as_str().unwrap()
            );
        }
    }

    #[test]
    fn steps_move_one_at_a_time_and_stop_at_the_ends() {
        assert_eq!(step_at("firmware", -1), None);
        assert_eq!(step_at("firmware", 1), Some(1));
        assert_eq!(step_at("battery", 1), Some(7));
        assert_eq!(step_at("finish", 1), None);
        assert_eq!(index_of("no-such-step"), 0);
    }

    fn connected(v: Value) -> Payload {
        let mut base = json!({"connected": true, "stack": "px4", "facts": {}, "presets": [], "preset_current": null});
        base.as_object_mut().unwrap().extend(v.as_object().unwrap().clone());
        Payload::from_json(&base)
    }

    fn presets() -> Value {
        json!([{"id": 4001, "label": "Generic quad", "group": "Quadrotor"}, {"id": 4002, "label": "Hex", "group": "Hexarotor"}])
    }

    #[test]
    fn the_aircraft_step_says_where_it_stands() {
        let none = aircraft_view(None, false, false, false, None);
        assert_eq!(
            none.status,
            "No controller is connected yet. Install the firmware in the first step, then come back here."
        );
        assert!(!none.show_apply && !none.apply_enabled);

        let fresh = connected(json!({"presets": presets(), "facts": {"airframe": "todo"}}));
        let v = aircraft_view(Some(&fresh), false, false, false, None);
        assert_eq!(v.status, "No aircraft type is selected on this controller.");
        assert!(v.show_apply);
        assert!(!v.apply_enabled, "nothing is chosen");
        let v = aircraft_view(Some(&fresh), false, false, false, Some(4001));
        assert!(v.apply_enabled);
        assert!(v
            .note
            .starts_with("Sets the airframe and resets the controller's parameters"));

        let set = connected(json!({"presets": presets(), "preset_current": 4001}));
        let v = aircraft_view(Some(&set), false, false, false, Some(4001));
        assert_eq!(v.status, "This controller is set to Generic quad.");
        assert!(!v.apply_enabled, "the type it already has");
        let v = aircraft_view(Some(&set), false, false, false, Some(4002));
        assert!(v.apply_enabled);
        let unknown = connected(json!({"presets": presets(), "preset_current": 9}));
        assert_eq!(
            aircraft_view(Some(&unknown), false, false, false, None).status,
            "This controller is set to airframe 9."
        );
    }

    #[test]
    fn an_armed_aircraft_cannot_be_changed() {
        let p = connected(json!({"presets": presets()}));
        let v = aircraft_view(Some(&p), true, false, false, Some(4001));
        assert!(!v.apply_enabled);
        assert_eq!(
            v.note,
            "The aircraft is armed. Disarm it before changing the aircraft type."
        );
        assert!(!aircraft_view(Some(&p), true, true, false, Some(4001)).reboot_enabled);
    }

    #[test]
    fn a_written_type_waits_for_the_reboot() {
        let p = connected(json!({"presets": presets()}));
        let v = aircraft_view(Some(&p), false, true, false, Some(4001));
        assert!(v.show_reboot && v.reboot_enabled && !v.apply_enabled);
        assert_eq!(v.note, "Written. The controller applies the new type after a reboot.");
        let v = aircraft_view(Some(&p), false, true, true, Some(4001));
        assert!(!v.reboot_enabled);
        assert_eq!(
            v.status,
            "Waiting for the controller to restart. This takes a few seconds."
        );
    }

    #[test]
    fn a_stack_with_no_presets_says_there_is_nothing_to_choose() {
        let ap = connected(json!({"stack": "ardupilot"}));
        let v = aircraft_view(Some(&ap), false, false, false, None);
        assert!(!v.show_apply);
        assert!(v.note.starts_with("ArduPilot Plane, Rover and Sub pick their type"));
        let px = connected(json!({}));
        assert_eq!(
            aircraft_view(Some(&px), false, false, false, None).note,
            "This firmware offers no presets here. Continue with the next step."
        );
        let ap_presets = connected(json!({"stack": "ardupilot", "presets": presets()}));
        assert_eq!(
            aircraft_view(Some(&ap_presets), false, false, false, None).note,
            "Sets the frame class and type. Reboot the controller afterwards."
        );
    }

    #[test]
    fn the_write_names_a_preset_id_and_says_what_it_did() {
        assert_eq!(airframe_body(4001), json!({"preset": 4001}));
        assert_eq!(
            written_note(&json!({"ok": true, "written": ["SYS_AUTOSTART"], "reboot": true})),
            (
                "info",
                "Aircraft type written. Reboot the controller to apply it.".to_string()
            )
        );
        assert_eq!(
            written_note(&json!({"ok": true, "warning": "SYS_AUTOCONFIG not written"})),
            (
                "warning",
                "The aircraft type is set, but: SYS_AUTOCONFIG not written".to_string()
            )
        );
        let p = connected(json!({"presets": presets()}));
        assert_eq!(
            change_question(Some(&p), 4002),
            "This controller already has an aircraft type. Changing it to Hex can reset settings you made before. Continue?"
        );
    }

    #[test]
    fn a_reboot_is_followed_down_and_back_up_and_the_controller_read_after_a_pause() {
        let mut r = Reboot::Waiting { went_down: false };
        assert!(!r.follow(true, 0.0), "still up: not rebooted yet");
        assert_eq!(r, Reboot::Waiting { went_down: false });
        assert!(!r.follow(false, 100.0));
        assert_eq!(r, Reboot::Waiting { went_down: true });
        assert!(!r.follow(true, 5000.0));
        assert_eq!(r, Reboot::Settling { at: 7500.0 });
        assert!(!r.follow(true, 7000.0));
        assert!(r.follow(true, 7500.0));
        assert_eq!(r, Reboot::Idle);
        assert!(!r.follow(true, 9000.0));
    }

    #[test]
    fn the_finish_step_counts_what_is_not_done() {
        let mut states = [State::Done; 8];
        let (rows, open) = summary(&states);
        assert_eq!((rows.len(), open), (7, 0));
        assert_eq!(rows[0].word, "Done");
        assert_eq!(rows[0].action, "Review");
        assert!(finish_hint(0).starts_with("Everything is done."));
        states[1] = State::Todo;
        states[2] = State::Pending;
        let (rows, open) = summary(&states);
        assert_eq!(open, 2);
        assert_eq!((rows[1].word, rows[1].action), ("Needs attention", "Open"));
        assert_eq!(rows[2].word, "Not visited");
        assert_eq!(
            finish_hint(2),
            "2 steps are not marked done. You can finish now and come back from the Setup page at any time."
        );
        assert!(finish_hint(1).starts_with("1 step is not marked done."));
    }

    #[test]
    fn the_session_remembers_what_was_visited_until_the_setup_is_finished() {
        let _turn = MEMORY_TEST.lock().unwrap_or_else(|e| e.into_inner());
        let mut m = memory();
        m.forget();
        m.visited.insert("firmware");
        m.current = "radio".into();
        drop(m);
        assert!(memory().visited.contains("firmware"));
        assert_eq!(memory().current, "radio");
        memory().forget();
        assert!(memory().visited.is_empty());
    }

    // ----- the detector -----

    fn tel(connected: bool, armed: bool, boot: &str) -> Value {
        if boot.is_empty() {
            json!({"connected": connected, "armed": armed})
        } else {
            json!({"connected": connected, "armed": armed, "link_bootloader": {"device": boot}})
        }
    }

    fn fresh_payload() -> Payload {
        connected(json!({"prompt": true, "reasons": ["No aircraft type is set."], "fingerprint": "px4|1|2|abc"}))
    }

    #[test]
    fn a_new_link_is_looked_at_once_after_the_parameters_have_settled() {
        let mut w = Watcher::new(Quiet::default());
        w.on_state(&tel(true, false, ""), 1000.0);
        assert_eq!(w.due(4000.0, false, false), None);
        assert_eq!(w.due(4500.0, false, false), Some(Job::Fresh { may_retry: true }));
        assert_eq!(w.due(9000.0, false, false), None, "once");
        let offer = w.fresh_answer(Some(&fresh_payload()), true, 4500.0).unwrap();
        assert_eq!(offer.title, "New controller detected");
        assert_eq!(offer.reasons, ["No aircraft type is set."]);
        assert_eq!(offer.fingerprint, "px4|1|2|abc");
        assert_eq!(offer.step, "");
        assert_eq!(
            w.fresh_answer(Some(&fresh_payload()), true, 5000.0),
            None,
            "not twice for one controller"
        );
    }

    #[test]
    fn a_dropped_link_forgets_what_was_shown_so_the_next_connection_asks_again() {
        let mut w = Watcher::new(Quiet::default());
        w.on_state(&tel(true, false, ""), 0.0);
        w.due(4000.0, false, false);
        assert!(w.fresh_answer(Some(&fresh_payload()), true, 4000.0).is_some());
        w.on_state(&tel(false, false, ""), 5000.0);
        w.on_state(&tel(true, false, ""), 6000.0);
        assert_eq!(w.due(10000.0, false, false), Some(Job::Fresh { may_retry: true }));
        assert!(w.fresh_answer(Some(&fresh_payload()), true, 10000.0).is_some());
    }

    #[test]
    fn an_answer_with_no_parameters_is_asked_once_more_and_then_dropped() {
        let mut w = Watcher::new(Quiet::default());
        w.on_state(&tel(true, false, ""), 0.0);
        assert_eq!(w.due(3600.0, false, false), Some(Job::Fresh { may_retry: true }));
        assert_eq!(w.fresh_answer(Some(&Payload::default()), true, 3600.0), None);
        assert_eq!(w.due(7000.0, false, false), None);
        assert_eq!(w.due(7700.0, false, false), Some(Job::Fresh { may_retry: false }));
        assert_eq!(w.fresh_answer(None, false, 7700.0), None);
        assert_eq!(w.due(20000.0, false, false), None);
    }

    #[test]
    fn nothing_interrupts_an_armed_aircraft_an_open_wizard_or_a_dialog() {
        let mut w = Watcher::new(Quiet::default());
        w.on_state(&tel(true, true, ""), 0.0);
        assert_eq!(w.due(4000.0, false, false), None, "armed");
        let mut w = Watcher::new(Quiet::default());
        w.on_state(&tel(true, false, ""), 0.0);
        assert_eq!(w.due(4000.0, true, false), None, "the wizard is open");
        let mut w = Watcher::new(Quiet::default());
        w.on_state(&tel(true, false, ""), 0.0);
        assert_eq!(w.due(4000.0, false, true), None, "another dialog");
        assert_eq!(w.due(8000.0, false, true), None);
        assert_eq!(w.due(9100.0, false, false), Some(Job::Fresh { may_retry: true }));
    }

    #[test]
    fn a_controller_waved_away_is_not_offered_again_and_the_wish_is_kept_on_disk() {
        let dir = std::env::temp_dir().join(format!("corvus-first-setup-{}", std::process::id()));
        let path = dir.join("quiet.json");
        let mut q = Quiet::load(Some(path.clone()));
        assert!(!q.is_silenced("px4|1|2|abc"));
        q.silence("px4|1|2|abc");
        q.silence("px4|1|2|abc");
        q.silence("");
        assert_eq!(Quiet::load(Some(path.clone())).list, ["px4|1|2|abc"]);
        let mut w = Watcher::new(Quiet::load(Some(path.clone())));
        w.on_state(&tel(true, false, ""), 0.0);
        w.due(4000.0, false, false);
        assert_eq!(w.fresh_answer(Some(&fresh_payload()), true, 4000.0), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_the_last_fifty_controllers_are_remembered() {
        let mut q = Quiet::default();
        for i in 0..60 {
            q.silence(&format!("fp{i}"));
        }
        assert_eq!(q.list.len(), 50);
        assert!(!q.is_silenced("fp9"));
        assert!(q.is_silenced("fp10"));
    }

    #[test]
    fn a_board_waiting_in_its_bootloader_is_offered_the_firmware_step() {
        let mut w = Watcher::new(Quiet::default());
        w.on_state(&tel(false, false, "/dev/ttyACM0"), 0.0);
        assert_eq!(w.due(3000.0, false, false), None);
        assert_eq!(
            w.due(4000.0, false, false),
            Some(Job::Bootloader("/dev/ttyACM0".into()))
        );
        let offer = w.bootloader_answer("/dev/ttyACM0", "idle").unwrap();
        assert_eq!(offer.title, "Controller without firmware");
        assert_eq!(
            offer.lede,
            "A flight controller is waiting in its bootloader on /dev/ttyACM0."
        );
        assert_eq!(offer.step, "firmware");
        assert!(offer.fingerprint.is_empty(), "there is no identity to remember");
        // a second frame with the same port asks nothing more
        w.on_state(&tel(false, false, "/dev/ttyACM0"), 5000.0);
        assert_eq!(w.due(20000.0, false, false), None);
    }

    #[test]
    fn a_flash_in_progress_is_not_a_new_controller() {
        let mut w = Watcher::new(Quiet::default());
        w.on_state(&tel(false, false, "/dev/ttyACM0"), 0.0);
        w.due(4000.0, false, false);
        assert_eq!(w.bootloader_answer("/dev/ttyACM0", "flashing"), None);
        assert_eq!(w.bootloader_answer("/dev/ttyACM0", "downloading"), None);
    }

    #[test]
    fn a_bootloader_that_left_or_became_a_link_is_not_offered() {
        let mut w = Watcher::new(Quiet::default());
        w.on_state(&tel(false, false, "/dev/ttyACM0"), 0.0);
        w.on_state(&tel(true, false, "/dev/ttyACM0"), 1000.0);
        assert_ne!(
            w.due(4000.0, false, false),
            Some(Job::Bootloader("/dev/ttyACM0".into()))
        );
        let mut w = Watcher::new(Quiet::default());
        w.on_state(&tel(false, false, "/dev/ttyACM0"), 0.0);
        w.on_state(&tel(false, false, ""), 1000.0);
        assert_eq!(w.due(5000.0, false, false), None);
        assert_eq!(w.bootloader_answer("/dev/ttyACM0", "idle"), None);
    }
}
