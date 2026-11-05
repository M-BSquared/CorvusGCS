//! What the Radio Control page knows apart from drawing it
//! (`python/src/js/setup-control.js`): the description the backend sends, which tab a
//! section lands on, the flight mode slot arithmetic of both stacks, the
//! double booked channels, and the calibration wizard's measurement: what the
//! sweep saw, which channel each stick moved and which way, and the rows that
//! get written. Every rule has the web page's expected value in a test.

use std::collections::BTreeMap;

/// `.rc-cal-row { min-width: 580px }`: below this the table scrolls sideways.
pub const CAL_TABLE_MIN_W: f32 = 580.0;

/// The height of the calibration table for `rows` channels: the head, then
/// each row's padding and field, with a hairline between rows.
pub fn cal_table_height(rows: usize) -> f32 {
    let head = 6.0 + 12.1 + 6.0 + 1.0;
    let row = 6.0 + 29.5 + 6.0;
    head + rows as f32 * row + rows.saturating_sub(1) as f32
}

use serde_json::Value;

use crate::data::units::js_round;
use crate::setup::rc_transmitter::pwm_fraction;
use crate::setup::schema_form::{parse_fields, Field};

/// A channel that moved less than this was not swept (`MIN_TRAVEL_US`, which
/// the backend checks again on its own).
pub const MIN_TRAVEL_US: f64 = 200.0;
/// How far a channel must move for Detect and the stick steps to call it.
pub const DETECT_TRAVEL_US: f64 = 250.0;
pub const DETECT_TIMEOUT_MS: f64 = 8000.0;
/// RC_CHANNELS rate asked for while the page is open: a stick swept in half a
/// second is three samples at 5 Hz, and the endpoint written would be whatever
/// those three caught.
pub const STREAM_HZ: u32 = 20;

/// ArduPilot fixes the PWM band of each of the six mode positions: the upper
/// bound (exclusive) of positions 1 to 5.
pub const ARDUPILOT_MODE_EDGES: [f64; 5] = [1231.0, 1361.0, 1491.0, 1621.0, 1750.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Radio,
    Modes,
}

pub const TABS: [(Tab, &str); 2] = [(Tab::Radio, "Radio"), (Tab::Modes, "Flight modes")];

/// A section the backend sends under an id not listed under "modes" lands on
/// the Radio tab, so one a newer backend adds is shown rather than lost.
pub fn tab_of(section_id: &str) -> Tab {
    if ["modes", "switches", "aux"].contains(&section_id) {
        Tab::Modes
    } else {
        Tab::Radio
    }
}

/// One row of the channel calibration table: the indices of its fields in
/// `Doc::table`, for min, trim, max and direction.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelRow {
    pub channel: u32,
    pub calibrated: bool,
    pub min: f64,
    pub max: f64,
    pub reversed: bool,
    pub fields: [Option<usize>; 4],
}

#[derive(Debug, Clone, Default)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub hint: String,
    pub channels: bool,
    pub fields: Vec<Field>,
    /// The role of each field, in order: `channel`, `mode` or empty.
    pub roles: Vec<String>,
    pub rows: Vec<ChannelRow>,
}

#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub connected: bool,
    pub error: String,
    pub received: i64,
    pub sections: Vec<Section>,
    pub assignments: BTreeMap<String, u32>,
    /// The calibration table's fields, which no section lists as fields.
    pub table: Vec<Field>,
}

pub fn parse_doc(v: &Value) -> Doc {
    let s = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut table: Vec<Field> = Vec::new();
    let sections = v
        .get("sections")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|sec| {
                    let channels = sec.get("kind").and_then(Value::as_str) == Some("channels");
                    let raw = sec.get("fields").cloned().unwrap_or(Value::Null);
                    let fields = parse_fields(&raw);
                    // fields without a parameter are dropped by parse_fields, so
                    // the roles are read from the same survivors
                    let roles = raw
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter(|f| {
                                    f.get("param").and_then(Value::as_str).is_some_and(|p| !p.is_empty())
                                        && f.get("kind").and_then(Value::as_str).is_some()
                                })
                                .map(|f| s(f, "role"))
                                .collect()
                        })
                        .unwrap_or_default();
                    let rows = sec
                        .get("rows")
                        .and_then(Value::as_array)
                        .map(|rows| {
                            rows.iter()
                                .map(|r| {
                                    let channel = r.get("channel").and_then(Value::as_u64).unwrap_or(0) as u32;
                                    let params = r.get("params").cloned().unwrap_or(Value::Null);
                                    let has_rev = r.get("has_rev") == Some(&Value::Bool(true));
                                    let mut fields = [None; 4];
                                    for (i, key) in ["min", "trim", "max", "rev"].iter().enumerate() {
                                        let Some(param) =
                                            params.get(*key).and_then(Value::as_str).filter(|p| !p.is_empty())
                                        else {
                                            continue;
                                        };
                                        if *key == "rev" && !has_rev {
                                            continue;
                                        }
                                        let f = if *key == "rev" {
                                            Field {
                                                param: param.into(),
                                                label: format!("Channel {channel} direction"),
                                                kind: crate::setup::schema_form::Kind::Sign(vec![
                                                    (1, "Normal".into()),
                                                    (-1, "Reversed".into()),
                                                ]),
                                                value: if r.get("reversed") == Some(&Value::Bool(true)) {
                                                    -1.0
                                                } else {
                                                    1.0
                                                },
                                                unit: String::new(),
                                                hint: String::new(),
                                                min: None,
                                                max: None,
                                                also: Vec::new(),
                                            }
                                        } else {
                                            Field {
                                                param: param.into(),
                                                label: format!("Channel {channel} {key}"),
                                                kind: crate::setup::schema_form::Kind::Number,
                                                value: r.get(*key).and_then(Value::as_f64).unwrap_or(0.0),
                                                unit: String::new(),
                                                hint: String::new(),
                                                min: Some(500.0),
                                                max: Some(2500.0),
                                                also: Vec::new(),
                                            }
                                        };
                                        fields[i] = Some(table.len());
                                        table.push(f);
                                    }
                                    ChannelRow {
                                        channel,
                                        calibrated: r.get("calibrated") == Some(&Value::Bool(true)),
                                        min: r.get("min").and_then(Value::as_f64).unwrap_or(0.0),
                                        max: r.get("max").and_then(Value::as_f64).unwrap_or(0.0),
                                        reversed: r.get("reversed") == Some(&Value::Bool(true)),
                                        fields,
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Section {
                        id: s(sec, "id"),
                        title: s(sec, "title"),
                        hint: s(sec, "hint"),
                        channels,
                        fields,
                        roles,
                        rows,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Doc {
        connected: v.get("connected") == Some(&Value::Bool(true)),
        error: s(v, "error"),
        received: v.get("received").and_then(Value::as_i64).unwrap_or(0),
        sections,
        assignments: v
            .get("assignments")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_f64().unwrap_or(0.0) as u32))
                    .collect()
            })
            .unwrap_or_default(),
        table,
    }
}

impl Doc {
    /// Every `RC_MAP_*` field the vehicle answered for, across all sections.
    pub fn channel_fields(&self) -> Vec<&Field> {
        self.sections
            .iter()
            .flat_map(|s| s.fields.iter().zip(&s.roles))
            .filter(|(_, r)| r.as_str() == "channel")
            .map(|(f, _)| f)
            .collect()
    }

    /// Every parameter a check reads back: the fields and the table.
    pub fn check_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for f in self.sections.iter().flat_map(|s| &s.fields).chain(&self.table) {
            if !names.contains(&f.param) {
                names.push(f.param.clone());
            }
        }
        names
    }
}

/// Channels carrying more than one `RC_MAP_*` assignment, ascending.
pub fn duplicate_channels(assignments: &BTreeMap<String, u32>) -> Vec<u32> {
    let mut counts: BTreeMap<u32, u32> = BTreeMap::new();
    for ch in assignments.values().filter(|c| **c > 0) {
        *counts.entry(*ch).or_default() += 1;
    }
    counts.into_iter().filter(|(_, n)| *n > 1).map(|(c, _)| c).collect()
}

/// The sentence under the page when channels are double booked.
pub fn conflict_text(dups: &[u32]) -> String {
    let many = dups.len() > 1;
    let list = if many {
        let head: Vec<String> = dups[..dups.len() - 1].iter().map(u32::to_string).collect();
        format!("{} and {}", head.join(", "), dups[dups.len() - 1])
    } else {
        dups.first().map_or(String::new(), u32::to_string)
    };
    format!(
        "{} to more than one action. PX4 allows it; in the air it means one control moves two things.",
        if many {
            format!("Channels {list} are each bound")
        } else {
            format!("Channel {list} is bound")
        }
    )
}

/// What every channel is bound to, as `{channel: "Roll, AUX 1"}`: joined, so a
/// channel bound to two things reads as both (a kill switch sharing a channel
/// with the mode switch fires on a mode change).
pub fn assignment_labels(doc: &Doc) -> BTreeMap<u32, String> {
    let titles: BTreeMap<&str, &str> = doc
        .channel_fields()
        .into_iter()
        .map(|f| (f.param.as_str(), f.label.as_str()))
        .collect();
    let mut out: BTreeMap<u32, String> = BTreeMap::new();
    for (param, ch) in &doc.assignments {
        if *ch == 0 {
            continue;
        }
        let name = titles.get(param.as_str()).copied().unwrap_or(param.as_str());
        out.entry(*ch)
            .and_modify(|t| {
                t.push_str(", ");
                t.push_str(name);
            })
            .or_insert_with(|| name.to_string());
    }
    out
}

/// PX4's own mode slot arithmetic (`RCUpdate`): the mode channel's normalised
/// -1 to +1 value in six equal bands with a small margin at each end.
pub fn mode_slot_index(normalized: f64) -> usize {
    let (lo, hi) = (-1.0 - 0.05, 1.0 + 0.05);
    let slot = ((normalized - lo) / (hi - lo) * 6.0).floor();
    slot.clamp(0.0, 5.0) as usize
}

/// ArduPilot reads the raw pulse against fixed bands.
pub fn ardupilot_slot_index(pulse: f64) -> usize {
    ARDUPILOT_MODE_EDGES
        .iter()
        .position(|e| pulse < *e)
        .unwrap_or(ARDUPILOT_MODE_EDGES.len())
}

/// The mode slot a pulse is in, normalised against the display scale rather
/// than the channel's own endpoints (an uncalibrated channel has none worth
/// trusting).
pub fn active_slot(pulse: f64, ardupilot: bool) -> Option<usize> {
    if pulse <= 0.0 {
        return None;
    }
    Some(if ardupilot {
        ardupilot_slot_index(pulse)
    } else {
        mode_slot_index(pwm_fraction(pulse) * 2.0 - 1.0)
    })
}

/// A flight mode slot's name: the label of the option the field holds.
pub fn slot_label(f: &Field) -> String {
    if let crate::setup::schema_form::Kind::Enum(o) = &f.kind {
        if let Some((_, l)) = o.iter().find(|(v, _)| *v as f64 == f.value) {
            return l.clone();
        }
    }
    crate::setup::schema_form::format_number(f.value)
}

/// The four sticks, in the order the wizard asks for them.
pub struct Stick {
    pub id: &'static str,
    pub param: &'static str,
    pub label: &'static str,
    pub action: &'static str,
    pub direction: &'static str,
}

/// Each stick with the direction PX4 expects the pulse to rise in, and which
/// way on the drawing the prompt points (forward on a gimbal is up).
pub const STICKS: [Stick; 4] = [
    Stick {
        id: "throttle",
        param: "RC_MAP_THROTTLE",
        label: "Throttle",
        action: "Push the throttle stick all the way UP and hold it.",
        direction: "up",
    },
    Stick {
        id: "roll",
        param: "RC_MAP_ROLL",
        label: "Roll",
        action: "Move the roll stick all the way RIGHT and hold it.",
        direction: "right",
    },
    Stick {
        id: "pitch",
        param: "RC_MAP_PITCH",
        label: "Pitch",
        action: "Push the pitch stick all the way FORWARD (nose down) and hold it.",
        direction: "up",
    },
    Stick {
        id: "yaw",
        param: "RC_MAP_YAW",
        label: "Yaw",
        action: "Move the yaw stick all the way RIGHT and hold it.",
        direction: "right",
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Intro,
    Centre,
    Sweep,
    Sticks,
    Review,
}

pub const STEPS: [Step; 5] = [Step::Intro, Step::Centre, Step::Sweep, Step::Sticks, Step::Review];

/// A stick's measured channel and whether it moved the wrong way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    pub channel: u32,
    pub reversed: bool,
}

/// A row to be written.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewRow {
    pub channel: u32,
    pub min: f64,
    pub max: f64,
    pub trim: f64,
    pub reversed: bool,
    pub set_reverse: bool,
    pub role: String,
}

/// What the wizard produces: per channel the lowest and highest pulse seen
/// during the sweep, the pulse seen at rest, and for the four sticks which
/// channel moved when a named direction was asked for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Measurement {
    pub centre: BTreeMap<u32, f64>,
    pub min: BTreeMap<u32, f64>,
    pub max: BTreeMap<u32, f64>,
    pub sticks: BTreeMap<&'static str, Measured>,
}

/// The channels delivering a pulse, 1 based.
pub fn active_channels(latest: &[f64]) -> Vec<(u32, f64)> {
    latest
        .iter()
        .enumerate()
        .filter(|(_, v)| **v > 0.0)
        .map(|(i, v)| (i as u32 + 1, *v))
        .collect()
}

impl Measurement {
    /// Snapshots the resting position of every channel the receiver delivers.
    pub fn capture_centre(&mut self, latest: &[f64]) {
        self.centre.clear();
        self.min.clear();
        self.max.clear();
        for (ch, v) in active_channels(latest) {
            self.centre.insert(ch, v);
            self.min.insert(ch, v);
            self.max.insert(ch, v);
        }
    }

    /// Widens each channel's measured travel with the frame that just arrived.
    pub fn accumulate(&mut self, latest: &[f64]) {
        for (ch, v) in active_channels(latest) {
            match (self.min.get_mut(&ch), self.max.get_mut(&ch)) {
                (Some(lo), Some(hi)) => {
                    *lo = lo.min(v);
                    *hi = hi.max(v);
                }
                _ => {
                    self.min.insert(ch, v);
                    self.max.insert(ch, v);
                }
            }
        }
    }

    /// Channels whose sweep is wide enough to be a real measurement.
    pub fn swept(&self) -> Vec<u32> {
        self.max
            .iter()
            .filter(|(ch, hi)| **hi - self.min.get(ch).copied().unwrap_or(**hi) >= MIN_TRAVEL_US)
            .map(|(ch, _)| *ch)
            .collect()
    }

    /// The channel the current stick prompt moved, and which way: measured
    /// against the centre captured in step 2, because the operator holds the
    /// stick, so the frame to frame delta is zero exactly when the answer is
    /// there.
    pub fn detect_stick(&self, latest: &[f64]) -> Option<Measured> {
        let mut best: Option<Measured> = None;
        let mut travel = 0.0;
        for (i, v) in latest.iter().enumerate() {
            let ch = i as u32 + 1;
            let Some(centre) = self.centre.get(&ch) else { continue };
            if *v <= 0.0 {
                continue;
            }
            let t = (v - centre).abs();
            if t > travel {
                travel = t;
                best = Some(Measured {
                    channel: ch,
                    reversed: v < centre,
                });
            }
        }
        if travel >= DETECT_TRAVEL_US {
            best
        } else {
            None
        }
    }

    /// The stick names bound to a channel.
    pub fn stick_label(&self, channel: u32) -> String {
        STICKS
            .iter()
            .filter(|s| self.sticks.get(s.id).is_some_and(|m| m.channel == channel))
            .map(|s| s.label)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The rows that will be written: every channel swept far enough, with its
    /// centre clamped into its own measured travel (a centre a microsecond
    /// outside the travel measured afterwards is refused by the backend).
    pub fn review_rows(&self) -> Vec<ReviewRow> {
        let by_channel: BTreeMap<u32, Measured> = self.sticks.values().map(|m| (m.channel, *m)).collect();
        self.swept()
            .into_iter()
            .map(|channel| {
                let (min, max) = (self.min[&channel], self.max[&channel]);
                let trim = match self.centre.get(&channel) {
                    None => js_round((min + max) / 2.0),
                    Some(c) => c.clamp(min, max),
                };
                let stick = by_channel.get(&channel);
                ReviewRow {
                    channel,
                    min,
                    max,
                    trim,
                    reversed: stick.is_some_and(|s| s.reversed),
                    set_reverse: stick.is_some(),
                    role: if stick.is_some() {
                        self.stick_label(channel)
                    } else {
                        String::new()
                    },
                }
            })
            .collect()
    }

    /// A stick bound to two axes is a measurement mistake (a stick that did
    /// not move when it was asked to); saying so beats flying it.
    pub fn clash(&self) -> String {
        let mut bound: BTreeMap<u32, &str> = BTreeMap::new();
        let mut clash = String::new();
        for s in &STICKS {
            let Some(m) = self.sticks.get(s.id) else { continue };
            if let Some(first) = bound.get(&m.channel) {
                clash = format!(
                    "Channel {} was measured for both {first} and {}. One of those sticks did not move when it was asked to.",
                    m.channel, s.label
                );
            }
            bound.insert(m.channel, s.label);
        }
        clash
    }

    /// The axes nothing was measured for.
    pub fn missing(&self) -> Vec<&'static str> {
        STICKS
            .iter()
            .filter(|s| !self.sticks.contains_key(s.id))
            .map(|s| s.label)
            .collect()
    }

    /// The body of `POST /api/rc/calibrate`.
    pub fn request(&self) -> Value {
        let rows = self.review_rows();
        let mut mapping = serde_json::Map::new();
        for s in &STICKS {
            if let Some(m) = self.sticks.get(s.id) {
                mapping.insert(s.param.into(), m.channel.into());
            }
        }
        serde_json::json!({
            "channels": rows.iter().map(|r| serde_json::json!({
                "channel": r.channel, "min": r.min, "max": r.max, "trim": r.trim,
                "reversed": r.reversed, "set_reverse": r.set_reverse,
            })).collect::<Vec<_>>(),
            "count": rows.iter().map(|r| r.channel).max().unwrap_or(0),
            "mapping": mapping,
        })
    }
}

/// The text under the sweep step.
pub fn sweep_hint(swept: &[u32]) -> String {
    if swept.is_empty() {
        "No channel has moved far enough yet.".into()
    } else {
        format!(
            "{} channel{} swept far enough so far: CH {}",
            swept.len(),
            if swept.len() == 1 { "" } else { "s" },
            swept.iter().map(u32::to_string).collect::<Vec<_>>().join(", CH ")
        )
    }
}

/// Whether the current step has enough evidence to move on. Every step but the
/// review gates on the live frame; the centre step captures its snapshot on
/// the way out, so it is gated on what arrives now.
pub fn can_advance(step: Step, rc_live: bool, latest: &[f64], m: &Measurement) -> bool {
    if !rc_live {
        return false;
    }
    match step {
        Step::Intro | Step::Centre => !active_channels(latest).is_empty(),
        Step::Sweep => m.swept().len() >= 4,
        Step::Sticks => m.detect_stick(latest).is_some(),
        Step::Review => true,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_calibration_table_is_as_tall_as_its_rows() {
        assert_eq!(super::cal_table_height(0), 25.1);
        assert_eq!(super::cal_table_height(1), 25.1 + 41.5);
        assert_eq!(super::cal_table_height(8), 25.1 + 8.0 * 41.5 + 7.0);
        assert_eq!(super::CAL_TABLE_MIN_W, 580.0);
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn the_flight_mode_slots_follow_px4_and_ardupilot() {
        // six equal bands from -1.05 to +1.05
        assert_eq!(mode_slot_index(-1.0), 0);
        assert_eq!(mode_slot_index(-0.6), 1);
        assert_eq!(mode_slot_index(-0.2), 2);
        assert_eq!(mode_slot_index(0.2), 3);
        assert_eq!(mode_slot_index(0.6), 4);
        assert_eq!(mode_slot_index(1.0), 5);
        assert_eq!(mode_slot_index(-3.0), 0);
        assert_eq!(mode_slot_index(3.0), 5);
        // ArduPilot reads the pulse against fixed bands
        assert_eq!(ardupilot_slot_index(1000.0), 0);
        assert_eq!(ardupilot_slot_index(1231.0), 1);
        assert_eq!(ardupilot_slot_index(1490.0), 2);
        assert_eq!(ardupilot_slot_index(1491.0), 3);
        assert_eq!(ardupilot_slot_index(1750.0), 5);
        assert_eq!(ardupilot_slot_index(2000.0), 5);
        // the strip uses the display scale: 1000 us is the bottom slot, 2000 the top
        assert_eq!(active_slot(1000.0, false), Some(0));
        assert_eq!(active_slot(2000.0, false), Some(5));
        assert_eq!(active_slot(1500.0, true), Some(3));
        assert_eq!(active_slot(0.0, true), None);
    }

    fn rc_doc() -> Doc {
        parse_doc(&json!({
        "connected": true, "received": 103,
        "assignments": {"RC_MAP_ROLL": 1, "RC_MAP_PITCH": 2, "RC_MAP_KILL_SW": 8, "RC_MAP_ARM_SW": 8, "RC_MAP_AUX1": 0},
        "sections": [
            {"id": "sticks", "title": "Stick channels", "kind": "fields", "fields": [
                {"param": "RC_MAP_ROLL", "label": "Roll", "kind": "enum", "value": 1, "role": "channel", "options": [{"value": 0, "label": "Unassigned"}, {"value": 1, "label": "Channel 1"}]},
                {"param": "RC_MAP_PITCH", "label": "Pitch", "kind": "enum", "value": 2, "role": "channel", "options": []}]},
            {"id": "modes", "title": "Flight mode settings", "kind": "fields", "fields": [
                {"param": "COM_FLTMODE1", "label": "Position 1", "kind": "enum", "value": 2, "role": "mode", "options": [{"value": 0, "label": "Manual"}, {"value": 2, "label": "Position"}]}]},
            {"id": "channels", "title": "Channel calibration", "kind": "channels", "rows": [
                {"channel": 1, "min": 1000, "max": 2000, "trim": 1500, "reversed": false, "has_rev": true, "calibrated": true,
                 "params": {"min": "RC1_MIN", "max": "RC1_MAX", "trim": "RC1_TRIM", "rev": "RC1_REV"}},
                {"channel": 2, "min": 1003, "max": 2003, "trim": 1503, "reversed": true, "has_rev": false, "calibrated": false,
                 "params": {"min": "RC2_MIN", "max": "RC2_MAX", "trim": "RC2_TRIM", "rev": "RC2_REV"}}]}
        ]}))
    }

    #[test]
    fn the_description_is_read_into_sections_roles_and_a_table() {
        let d = rc_doc();
        assert!(d.connected);
        assert_eq!(d.sections.len(), 3);
        assert_eq!(d.sections[0].roles, ["channel", "channel"]);
        assert_eq!(d.sections[1].roles, ["mode"]);
        assert!(d.sections[2].channels);
        // CH 1 has four fields, CH 2 three (no reverse parameter was reported for it)
        assert_eq!(d.table.len(), 7);
        assert_eq!(d.sections[2].rows[0].fields, [Some(0), Some(1), Some(2), Some(3)]);
        assert_eq!(d.sections[2].rows[1].fields, [Some(4), Some(5), Some(6), None]);
        assert_eq!(d.table[3].value, 1.0, "normal is +1");
        assert_eq!((d.table[0].min, d.table[0].max), (Some(500.0), Some(2500.0)));
        assert_eq!(d.channel_fields().len(), 2);
        let names = d.check_names();
        assert!(names.contains(&"RC1_REV".to_string()) && names.contains(&"COM_FLTMODE1".to_string()));
        assert!(
            !names.contains(&"RC2_REV".to_string()),
            "a reverse the vehicle lacks is not asked for"
        );
    }

    #[test]
    fn a_section_lands_on_the_tab_the_web_page_puts_it_on() {
        assert_eq!(tab_of("modes"), Tab::Modes);
        assert_eq!(tab_of("switches"), Tab::Modes);
        assert_eq!(tab_of("aux"), Tab::Modes);
        assert_eq!(tab_of("input"), Tab::Radio);
        assert_eq!(tab_of("something_new"), Tab::Radio);
    }

    #[test]
    fn a_doubled_channel_is_named_and_a_channel_bound_twice_reads_as_both() {
        let d = rc_doc();
        assert_eq!(duplicate_channels(&d.assignments), [8]);
        assert_eq!(
            conflict_text(&[8]),
            "Channel 8 is bound to more than one action. PX4 allows it; in the air it means one control moves two things."
        );
        assert_eq!(
            conflict_text(&[2, 5, 8]),
            "Channels 2, 5 and 8 are each bound to more than one action. PX4 allows it; in the air it means one control moves two things."
        );
        let labels = assignment_labels(&d);
        assert_eq!(labels[&1], "Roll");
        assert_eq!(
            labels[&8], "RC_MAP_ARM_SW, RC_MAP_KILL_SW",
            "no field title falls back to the parameter"
        );
        assert!(!labels.contains_key(&0));
    }

    #[test]
    fn a_slot_is_named_by_the_option_it_holds() {
        let d = rc_doc();
        assert_eq!(slot_label(&d.sections[1].fields[0]), "Position");
    }

    fn sweep(m: &mut Measurement, frames: &[&[f64]]) {
        for f in frames {
            m.accumulate(f);
        }
    }

    #[test]
    fn a_sweep_widens_the_travel_and_only_wide_travel_counts() {
        let mut m = Measurement::default();
        m.capture_centre(&[1500.0, 1500.0, 1000.0, 1500.0, 1500.0]);
        sweep(
            &mut m,
            &[
                &[1000.0, 1500.0, 1000.0, 1500.0, 1500.0],
                &[2000.0, 1599.0, 1900.0, 1100.0, 1500.0],
                &[1500.0, 1500.0, 1000.0, 1900.0, 1600.0],
            ],
        );
        assert_eq!(m.min[&1], 1000.0);
        assert_eq!(m.max[&1], 2000.0);
        // channel 2 moved 99 us, channel 5 only 100: neither is a sweep
        assert_eq!(m.swept(), [1, 3, 4]);
        assert!(!can_advance(Step::Sweep, true, &[], &m), "three channels are not four");
        sweep(
            &mut m,
            &[
                &[1500.0, 1500.0, 1000.0, 1500.0, 1500.0, 1000.0],
                &[0.0, 0.0, 0.0, 0.0, 0.0, 1900.0],
            ],
        );
        assert_eq!(m.swept(), [1, 3, 4, 6]);
        assert!(can_advance(Step::Sweep, true, &[], &m));
        assert!(!can_advance(Step::Sweep, false, &[], &m), "no signal, no step");
        assert_eq!(
            sweep_hint(&m.swept()),
            "4 channels swept far enough so far: CH 1, CH 3, CH 4, CH 6"
        );
        assert_eq!(sweep_hint(&[3]), "1 channel swept far enough so far: CH 3");
        assert_eq!(sweep_hint(&[]), "No channel has moved far enough yet.");
    }

    #[test]
    fn a_stick_is_the_channel_that_left_its_centre_and_a_fall_means_reversed() {
        let mut m = Measurement::default();
        m.capture_centre(&[1500.0, 1500.0, 1000.0, 1500.0]);
        assert_eq!(
            m.detect_stick(&[1500.0, 1510.0, 1000.0, 1500.0]),
            None,
            "jitter is not a stick"
        );
        assert_eq!(
            m.detect_stick(&[1500.0, 1500.0, 1900.0, 1500.0]),
            Some(Measured {
                channel: 3,
                reversed: false
            })
        );
        assert_eq!(
            m.detect_stick(&[1100.0, 1500.0, 1000.0, 1500.0]),
            Some(Measured {
                channel: 1,
                reversed: true
            })
        );
        assert_eq!(m.detect_stick(&[0.0, 0.0, 0.0, 0.0]), None);
        assert!(can_advance(Step::Sticks, true, &[1100.0, 1500.0, 1000.0, 1500.0], &m));
        assert!(can_advance(Step::Centre, true, &[1500.0], &m));
        assert!(!can_advance(Step::Centre, true, &[0.0, 0.0], &m));
        assert!(can_advance(Step::Review, true, &[], &m));
    }

    #[test]
    fn the_rows_to_write_clamp_the_centre_into_the_travel_and_carry_the_directions() {
        let mut m = Measurement::default();
        m.centre.extend([(1, 1500.0), (2, 1990.0), (3, 900.0)]);
        m.min.extend([(1, 1000.0), (2, 1000.0), (3, 1000.0), (5, 1000.0)]);
        m.max.extend([(1, 2000.0), (2, 1950.0), (3, 2000.0), (5, 1400.0)]);
        m.sticks.insert(
            "throttle",
            Measured {
                channel: 3,
                reversed: true,
            },
        );
        m.sticks.insert(
            "roll",
            Measured {
                channel: 1,
                reversed: false,
            },
        );
        let rows = m.review_rows();
        let ch: Vec<u32> = rows.iter().map(|r| r.channel).collect();
        assert_eq!(ch, [1, 2, 3, 5]);
        // the centre 1990 is above channel 2's own top of 1950, and 900 below channel 3's 1000
        assert_eq!((rows[1].trim, rows[2].trim), (1950.0, 1000.0));
        assert_eq!(rows[0].trim, 1500.0);
        // channel 5 never had a centre captured: the middle of its travel
        assert_eq!(rows[3].trim, 1200.0);
        assert_eq!(
            (rows[2].reversed, rows[2].set_reverse, rows[2].role.as_str()),
            (true, true, "Throttle")
        );
        assert_eq!((rows[0].reversed, rows[0].role.as_str()), (false, "Roll"));
        assert_eq!(
            (rows[1].set_reverse, rows[3].set_reverse),
            (false, false),
            "an unmeasured channel keeps its direction"
        );
    }

    #[test]
    fn a_channel_without_a_centre_is_centred_between_its_ends() {
        let mut m = Measurement::default();
        m.min.insert(5, 1000.0);
        m.max.insert(5, 1401.0);
        let rows = m.review_rows();
        assert_eq!(rows[0].trim, 1201.0, "Math.round of 1200.5");
        assert_eq!((rows[0].set_reverse, rows[0].reversed), (false, false));
        assert_eq!(rows[0].role, "");
    }

    #[test]
    fn the_request_names_the_channels_the_count_and_the_stick_mapping() {
        let mut m = Measurement::default();
        m.centre.extend([(1, 1500.0), (3, 1000.0)]);
        m.min.extend([(1, 1000.0), (3, 1000.0)]);
        m.max.extend([(1, 2000.0), (3, 1990.0)]);
        m.sticks.insert(
            "throttle",
            Measured {
                channel: 3,
                reversed: false,
            },
        );
        m.sticks.insert(
            "roll",
            Measured {
                channel: 1,
                reversed: true,
            },
        );
        let r = m.request();
        assert_eq!(r["count"], 3);
        assert_eq!(r["mapping"], json!({"RC_MAP_THROTTLE": 3, "RC_MAP_ROLL": 1}));
        assert_eq!(
            r["channels"][1],
            json!({"channel": 3, "min": 1000.0, "max": 1990.0, "trim": 1000.0, "reversed": false, "set_reverse": true})
        );
        assert_eq!(r["channels"][0]["reversed"], true);
        assert_eq!(m.stick_label(3), "Throttle");
    }

    #[test]
    fn two_axes_on_one_channel_and_axes_never_measured_are_said_before_writing() {
        let mut m = Measurement::default();
        m.sticks.insert(
            "throttle",
            Measured {
                channel: 3,
                reversed: false,
            },
        );
        m.sticks.insert(
            "pitch",
            Measured {
                channel: 3,
                reversed: false,
            },
        );
        assert_eq!(
            m.clash(),
            "Channel 3 was measured for both Throttle and Pitch. One of those sticks did not move when it was asked to."
        );
        assert_eq!(m.missing(), ["Roll", "Yaw"]);
        assert_eq!(Measurement::default().clash(), "");
    }

    #[test]
    fn the_stick_prompts_point_the_way_the_web_page_points() {
        let dirs: Vec<(&str, &str)> = STICKS.iter().map(|s| (s.id, s.direction)).collect();
        assert_eq!(
            dirs,
            [("throttle", "up"), ("roll", "right"), ("pitch", "up"), ("yaw", "right")]
        );
        assert_eq!(
            STICKS[2].action,
            "Push the pitch stick all the way FORWARD (nose down) and hold it."
        );
        assert_eq!(STICKS[0].param, "RC_MAP_THROTTLE");
    }
}
