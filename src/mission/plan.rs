//! The plan: the item table, the bounds the backend validates against
//! (`corvus/mission.py`), the plan's JSON and the rules that edit it
//! (`python/src/js/mission.js`, "The plan, as the backend sees it" and "Editing").
//!
//! `Plan::items` is the mission, in order, and the one source of truth: the
//! map, the list and the profile are all drawn from it.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::mission::geometry::distance_m;

pub const ALT_MIN_M: f64 = -100.0;
pub const ALT_MAX_M: f64 = 1000.0;
pub const RADIUS_MIN_M: f64 = 1.0;
pub const RADIUS_MAX_M: f64 = 10000.0;
pub const HOLD_MAX_S: f64 = 3600.0;
pub const TURNS_MAX: f64 = 100.0;
pub const SPEED_MIN_MS: f64 = 0.5;
pub const SPEED_MAX_MS: f64 = 100.0;
pub const MAX_ITEMS: usize = 255;
pub const POINT_NAME_MAX: usize = 40;
pub const DEFAULT_ALT_M: f64 = 30.0;
pub const DEFAULT_RADIUS_M: f64 = 50.0;
pub const APPROACH_MIN_M: f64 = 3.0;
pub const START_OFFSET_WARN_M: f64 = 25.0;
pub const START_FOLLOW_M: f64 = 3.0;
/// How close a takeoff has to stand to the start to be the start's takeoff.
pub const TIED_M: f64 = 0.5;

/// The item types, by the names `corvus/mission.py` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Takeoff,
    Waypoint,
    LoiterTurns,
    LoiterTime,
    Land,
    Rtl,
}

pub const KINDS: [Kind; 6] = [
    Kind::Takeoff,
    Kind::Waypoint,
    Kind::LoiterTurns,
    Kind::LoiterTime,
    Kind::Land,
    Kind::Rtl,
];

/// The editable fields of an item beyond its place, with `ITEM_SPECS`' names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Param {
    Pitch,
    Hold,
    AcceptRadius,
    Turns,
    Radius,
    Direction,
    Seconds,
}

/// One field's label, unit, bounds, step and default.
pub struct Rule {
    pub key: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub min: f64,
    pub max: f64,
    pub def: f64,
}

impl Param {
    pub fn rule(self) -> Rule {
        let r = |key, label, unit, min, max, _step, def| Rule {
            key,
            label,
            unit,
            min,
            max,
            def,
        };
        match self {
            Param::Pitch => r("pitch", "Climb pitch", "°", 0.0, 45.0, 1.0, 0.0),
            Param::Hold => r("hold", "Hold", "s", 0.0, HOLD_MAX_S, 1.0, 0.0),
            Param::AcceptRadius => r("accept_radius", "Accept radius", "m", 0.0, 1000.0, 1.0, 0.0),
            Param::Turns => r("turns", "Turns", "", 0.0, TURNS_MAX, 0.5, 1.0),
            Param::Radius => r(
                "radius",
                "Radius",
                "m",
                RADIUS_MIN_M,
                RADIUS_MAX_M,
                1.0,
                DEFAULT_RADIUS_M,
            ),
            Param::Direction => r("direction", "Direction", "", -1.0, 1.0, 1.0, 1.0),
            Param::Seconds => r("seconds", "Time", "s", 0.0, HOLD_MAX_S, 1.0, 30.0),
        }
    }
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Takeoff => "takeoff",
            Kind::Waypoint => "waypoint",
            Kind::LoiterTurns => "loiter_turns",
            Kind::LoiterTime => "loiter_time",
            Kind::Land => "land",
            Kind::Rtl => "rtl",
        }
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        KINDS.iter().copied().find(|k| k.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Takeoff => "Takeoff",
            Kind::Waypoint => "Waypoint",
            Kind::LoiterTurns => "Circle",
            Kind::LoiterTime => "Hold",
            Kind::Land => "Land",
            Kind::Rtl => "Return",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Kind::Takeoff => "plane-takeoff",
            Kind::Waypoint => "map-pin",
            Kind::LoiterTurns => "rotate-cw",
            Kind::LoiterTime => "timer",
            Kind::Land => "plane-landing",
            Kind::Rtl => "house",
        }
    }

    /// Whether the item names a place on the map.
    pub fn position(self) -> bool {
        self != Kind::Rtl
    }

    pub fn is_orbit(self) -> bool {
        matches!(self, Kind::LoiterTurns | Kind::LoiterTime)
    }

    /// An item that ends the mission.
    pub fn ends(self) -> bool {
        matches!(self, Kind::Land | Kind::Rtl)
    }

    pub fn end_name(self) -> &'static str {
        match self {
            Kind::Land => "a landing",
            _ => "a return",
        }
    }

    pub fn params(self) -> &'static [Param] {
        match self {
            Kind::Takeoff => &[Param::Pitch],
            Kind::Waypoint => &[Param::Hold, Param::AcceptRadius],
            Kind::LoiterTurns => &[Param::Turns, Param::Radius, Param::Direction],
            Kind::LoiterTime => &[Param::Seconds, Param::Radius, Param::Direction],
            Kind::Land | Kind::Rtl => &[],
        }
    }

    /// What to use on a stack that will not fly this type.
    pub fn instead(self) -> &'static str {
        match self {
            Kind::LoiterTurns => "Use HOLD to stay at a point for a set time instead.",
            _ => "",
        }
    }

    /// The sentence under the heading: what the item does on this aircraft.
    pub fn hint(self, aircraft: Option<Aircraft>, drives: bool) -> &'static str {
        use Aircraft::*;
        let by = |multirotor: &'static str,
                  fixed: &'static str,
                  vtol: &'static str,
                  rover: &'static str,
                  any: &'static str| {
            match aircraft {
                Some(Multirotor) if !multirotor.is_empty() => multirotor,
                Some(FixedWing) if !fixed.is_empty() => fixed,
                Some(Vtol) if !vtol.is_empty() => vtol,
                Some(Rover) if !rover.is_empty() => rover,
                _ => any,
            }
        };
        let _ = drives;
        match self {
            Kind::Takeoff => by(
                "Climbs straight up from the start point to this height, then flies on.",
                "Climbs out from the start point in the direction it is launched, at the climb pitch or steeper, until it is at this height, then flies on. The profile assumes it is launched towards the next point.",
                "",
                "",
                "Takes off from the start point and climbs to this height before flying on.",
            ),
            Kind::Waypoint => by("", "", "", "Drive to this point.", "Fly to this point at this height."),
            Kind::LoiterTurns => by(
                "",
                "",
                "",
                "Drive a circle of this radius around the point, a set number of times, then continue.",
                "Fly a circle of this radius around the point, a set number of times, then continue. ArduPilot flies it as one circle; PX4 has no circle in a mission, so Corvus sends it as waypoints around the rim.",
            ),
            Kind::LoiterTime => by(
                "Hover over this point for a set time, then continue. A multicopter holds the point in place, so the radius and direction stay in the plan but are not flown.",
                "Circle this point for a set time, then continue.",
                "Circle this point for a set time, then continue.",
                "Stop at this point for a set time, then continue. The radius and direction stay in the plan but are not driven.",
                "Stay at this point for a set time, then continue. A fixed wing circles it, a multicopter hovers over it. Pick the aircraft over the altitude profile to see only what it flies.",
            ),
            Kind::Land => by(
                "Fly here, then descend straight down and touch down. Ends the mission. Set where the descent starts below, or drag the point above the landing in the altitude profile. All the way down, it dives straight in.",
                "Glide down from the point before and touch down here. Ends the mission. The approach has to be long and flat: the altitude profile shows its angle.",
                "",
                "",
                "Descend and touch down here. Ends the mission.",
            ),
            Kind::Rtl => by(
                "Fly back to where the aircraft was armed, or to the nearest rally point, and land there. Ends the mission. It returns at the height set on the vehicle, which the profile shows while it is connected.",
                "Fly back to where the aircraft was armed, or to the nearest rally point, and circle there. Ends the mission but not the flight: a fixed wing does not land after a return. End with LAND to bring it down.",
                "",
                "Drive back to where the vehicle was armed, or to the nearest rally point, and stop there. Ends the mission.",
                "Fly back to where the aircraft was armed, or to the nearest rally point. Ends the mission. How high it flies and what it does there is set on the vehicle.",
            ),
        }
    }
}

/// What the plan is flown by (`AIRCRAFT`); the names are the contract with
/// `PLAN_AIRCRAFT` in `corvus/mission.py`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aircraft {
    Multirotor,
    FixedWing,
    Vtol,
    Rover,
}

pub const AIRCRAFT: [Aircraft; 4] = [
    Aircraft::Multirotor,
    Aircraft::FixedWing,
    Aircraft::Vtol,
    Aircraft::Rover,
];

impl Aircraft {
    pub fn value(self) -> &'static str {
        match self {
            Aircraft::Multirotor => "multirotor",
            Aircraft::FixedWing => "fixed_wing",
            Aircraft::Vtol => "vtol",
            Aircraft::Rover => "rover",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Aircraft::Multirotor => "Multicopter",
            Aircraft::FixedWing => "Fixed wing",
            Aircraft::Vtol => "VTOL",
            Aircraft::Rover => "Rover or boat",
        }
    }

    pub fn from_value(v: &str) -> Option<Aircraft> {
        AIRCRAFT.iter().copied().find(|a| a.value() == v)
    }

    /// The MAV_TYPE names the bridge puts into the state store.
    pub fn types(self) -> &'static [&'static str] {
        match self {
            Aircraft::Multirotor => &[
                "QUADROTOR",
                "COAXIAL",
                "HELICOPTER",
                "HEXAROTOR",
                "OCTOROTOR",
                "TRIROTOR",
            ],
            Aircraft::FixedWing => &["FIXED_WING"],
            Aircraft::Vtol => &["VTOL_DUOROTOR", "VTOL_QUADROTOR", "VTOL_TILTROTOR"],
            Aircraft::Rover => &["GROUNDED_ROVER", "SURFACE_BOAT"],
        }
    }

    pub fn of_vehicle(vehicle_type: &str) -> Option<Aircraft> {
        AIRCRAFT.iter().copied().find(|a| a.types().contains(&vehicle_type))
    }
}

/// One planned item. Every field exists on every item; only those the kind's
/// table names are sent.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: u32,
    pub kind: Kind,
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
    pub pitch: f64,
    pub hold: f64,
    pub accept_radius: f64,
    pub turns: f64,
    pub radius: f64,
    pub direction: f64,
    pub seconds: f64,
    pub speed: Option<f64>,
    pub name: String,
    pub approach_alt: Option<f64>,
    /// The group a pattern's waypoints were placed in.
    pub pattern: Option<u32>,
}

impl Item {
    pub fn new(id: u32, kind: Kind) -> Item {
        let d = |p: Param| p.rule().def;
        Item {
            id,
            kind,
            lat: 0.0,
            lon: 0.0,
            alt: 0.0,
            pitch: d(Param::Pitch),
            hold: d(Param::Hold),
            accept_radius: d(Param::AcceptRadius),
            turns: d(Param::Turns),
            radius: d(Param::Radius),
            direction: d(Param::Direction),
            seconds: d(Param::Seconds),
            speed: None,
            name: String::new(),
            approach_alt: None,
            pattern: None,
        }
    }

    pub fn get(&self, p: Param) -> f64 {
        match p {
            Param::Pitch => self.pitch,
            Param::Hold => self.hold,
            Param::AcceptRadius => self.accept_radius,
            Param::Turns => self.turns,
            Param::Radius => self.radius,
            Param::Direction => self.direction,
            Param::Seconds => self.seconds,
        }
    }

    pub fn set(&mut self, p: Param, v: f64) {
        match p {
            Param::Pitch => self.pitch = v,
            Param::Hold => self.hold = v,
            Param::AcceptRadius => self.accept_radius = v,
            Param::Turns => self.turns = v,
            Param::Radius => self.radius = v,
            Param::Direction => self.direction = v,
            Param::Seconds => self.seconds = v,
        }
    }

    /// Direction is one of two things, not a range.
    pub fn snap_direction(&mut self) {
        if self.kind.is_orbit() {
            self.direction = if self.direction >= 0.0 { 1.0 } else { -1.0 };
        }
    }

    /// What a point is called on screen: its own name, else its kind.
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            self.kind.label()
        } else {
            &self.name
        }
    }
}

/// Clamps into [min, max], falling back to `fallback` for junk.
pub fn clamp_number(v: f64, min: f64, max: f64, fallback: f64) -> f64 {
    if !v.is_finite() {
        return fallback;
    }
    v.clamp(min, max)
}

fn js_round(v: f64) -> f64 {
    crate::data::units::js_round(v)
}

/// A point's name as `clean_point_name` stores it: control characters and
/// line separators become spaces, runs of space collapse, 40 code points.
pub fn clean_point_name(raw: &str) -> String {
    let spaced: String = raw
        .chars()
        .map(|c| {
            let u = c as u32;
            if u <= 0x1f || (0x7f..=0x9f).contains(&u) || u == 0x2028 || u == 0x2029 {
                ' '
            } else {
                c
            }
        })
        .collect();
    let joined = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    joined
        .chars()
        .take(POINT_NAME_MAX)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The planned start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Home {
    pub lat: f64,
    pub lon: f64,
    pub elevation: Option<f64>,
}

/// How the connected vehicle flies a Return (`GET /api/mission/return`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ReturnProfile {
    pub known: bool,
    pub hold: bool,
    pub climb_to: Option<f64>,
    pub arrive_at: Option<f64>,
}

impl ReturnProfile {
    pub fn from_json(v: &Value) -> Option<ReturnProfile> {
        if v.get("connected").and_then(Value::as_bool) != Some(true)
            || v.get("known").and_then(Value::as_bool) != Some(true)
        {
            return None;
        }
        Some(ReturnProfile {
            known: true,
            hold: v.get("hold").and_then(Value::as_bool).unwrap_or(false),
            climb_to: v.get("climb_to").and_then(Value::as_f64),
            arrive_at: v.get("arrive_at").and_then(Value::as_f64),
        })
    }
}

/// The tools of the bar, and the three pattern shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Circle,
    Corridor,
    Area,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Home,
    VehicleStart,
    Item(Kind),
    /// The PATTERN button itself, which opens its card and arms nothing.
    Pattern,
    Shape(Shape),
}

/// A GPS fix of the connected aircraft, as the plan needs it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Link {
    pub connected: bool,
    pub start: Option<(f64, f64)>,
}

pub struct Plan {
    pub items: Vec<Item>,
    pub home: Option<Home>,
    pub name: String,
    pub aircraft: Option<Aircraft>,
    pub aircraft_locked: bool,
    pub drives: bool,
    pub start_at_vehicle: bool,
    pub selected: Option<u32>,
    next_id: u32,
    pub next_pattern: u32,
    /// Item types the connected stack will not fly, with the reason.
    pub refusals: HashMap<String, String>,
    pub return_profile: Option<ReturnProfile>,
    /// Warnings for the notification board.
    pub notices: Vec<String>,
}

impl Default for Plan {
    fn default() -> Self {
        Plan {
            items: Vec::new(),
            home: None,
            name: "Mission".into(),
            aircraft: None,
            aircraft_locked: false,
            drives: false,
            start_at_vehicle: false,
            selected: None,
            next_id: 1,
            next_pattern: 1,
            refusals: HashMap::new(),
            return_profile: None,
            notices: Vec::new(),
        }
    }
}

impl Plan {
    pub fn selected_item(&self) -> Option<&Item> {
        self.items.iter().find(|i| Some(i.id) == self.selected)
    }

    pub fn index_of(&self, id: u32) -> Option<usize> {
        self.items.iter().position(|i| i.id == id)
    }

    pub fn item_mut(&mut self, id: u32) -> Option<&mut Item> {
        self.items.iter_mut().find(|i| i.id == id)
    }

    /// Where the mission ends: the index of its first landing or return.
    pub fn end_index(&self) -> Option<usize> {
        self.items.iter().position(|i| i.kind.ends())
    }

    /// The takeoff the start carries: the first item, standing on the start.
    pub fn start_takeoff(&self) -> Option<&Item> {
        let home = self.home?;
        let first = self.items.first()?;
        (first.kind == Kind::Takeoff && distance_m(home.lat, home.lon, first.lat, first.lon) <= TIED_M).then_some(first)
    }

    /// The altitude a new point gets: the one before it.
    pub fn suggested_altitude(&self, before: Option<usize>) -> f64 {
        let from = before.unwrap_or(self.items.len());
        self.items[..from.min(self.items.len())]
            .iter()
            .rev()
            .find(|i| i.kind.position() && i.kind != Kind::Land)
            .map_or(DEFAULT_ALT_M, |i| i.alt)
    }

    /// Whether the ring, radius and direction of an orbit are flown.
    pub fn ring_flown(&self, item: &Item) -> bool {
        match item.kind {
            Kind::LoiterTime => !matches!(self.aircraft, Some(Aircraft::Multirotor) | Some(Aircraft::Rover)),
            Kind::LoiterTurns => true,
            _ => false,
        }
    }

    /// Is `p` a field this airframe would actually fly?
    pub fn param_applies(&self, item: &Item, p: Param) -> bool {
        match p {
            Param::Pitch => self.aircraft != Some(Aircraft::Multirotor),
            Param::Radius | Param::Direction => !item.kind.is_orbit() || self.ring_flown(item),
            _ => true,
        }
    }

    pub fn item_hint(&self, item: &Item) -> &'static str {
        item.kind.hint(self.aircraft, self.drives)
    }

    /// Why a plan that drives, or the connected stack, will not fly `kind`.
    pub fn refusal_of(&self, kind: Kind) -> String {
        if self.drives {
            match kind {
                Kind::Takeoff => return "A rover drives, it does not take off".into(),
                Kind::Land => return "A rover drives, it does not land".into(),
                _ => {}
            }
        }
        self.refusals.get(kind.name()).cloned().unwrap_or_default()
    }

    pub fn craft(&self) -> &'static str {
        if self.drives {
            "vehicle"
        } else {
            "aircraft"
        }
    }

    /// Why `tool` cannot be used right now, or None.
    pub fn tool_blocked(&self, tool: Tool, link: &Link) -> Option<String> {
        match tool {
            Tool::Select | Tool::Home => None,
            Tool::VehicleStart => {
                if self.start_at_vehicle {
                    return None;
                }
                if !link.connected {
                    let who = if self.drives { "a vehicle" } else { "an aircraft" };
                    return Some(format!("Connect {who} first. The start is then put where it stands."));
                }
                if link.start.is_none() {
                    return Some(format!("The {} has not reported a position yet.", self.craft()));
                }
                None
            }
            Tool::Pattern | Tool::Shape(_) => self
                .home
                .is_none()
                .then(|| "Set the start first. The mission begins there.".to_string()),
            Tool::Item(kind) => {
                let refused = self.refusal_of(kind);
                if !refused.is_empty() {
                    return Some(format!("{refused}. {}", kind.instead()).trim().to_string());
                }
                if self.home.is_none() {
                    return Some("Set the start first. The mission begins there.".into());
                }
                if kind.ends() {
                    if let Some(end) = self.end_index() {
                        return Some(format!(
                            "The mission already ends with {}. Remove it to end the mission differently.",
                            self.items[end].kind.end_name()
                        ));
                    }
                }
                None
            }
        }
    }

    /// Adds one item. It goes in before the mission's ending when there is
    /// one; `at` overrides that.
    pub fn add_item(&mut self, kind: Kind, at_point: Option<(f64, f64)>, at: Option<usize>) -> Option<usize> {
        if self.items.len() >= MAX_ITEMS {
            self.notices.push(format!("A mission can hold {MAX_ITEMS} items."));
            return None;
        }
        let index = at.unwrap_or_else(|| self.end_index().unwrap_or(self.items.len()));
        let mut item = Item::new(self.next_id, kind);
        self.next_id += 1;
        if kind.position() {
            let (lon, lat) = at_point?;
            item.lat = lat;
            item.lon = lon;
            item.alt = if kind == Kind::Land || self.drives {
                0.0
            } else {
                self.suggested_altitude(Some(index))
            };
        }
        item.snap_direction();
        self.selected = Some(item.id);
        self.items.insert(index, item);
        Some(index)
    }

    /// Puts the start at `(lon, lat)`, and the takeoff with it.
    pub fn place_start(&mut self, lon: f64, lat: f64, keep_selection: bool) {
        let tied = self.start_takeoff().map(|t| t.id);
        self.home = Some(Home {
            lat,
            lon,
            elevation: None,
        });
        if let Some(id) = tied {
            if let Some(t) = self.item_mut(id) {
                t.lat = lat;
                t.lon = lon;
            }
            if !keep_selection {
                self.selected = Some(id);
            }
            return;
        }
        if self.drives || self.items.iter().any(|i| i.kind == Kind::Takeoff) {
            return;
        }
        if let Some(i) = self.add_item(Kind::Takeoff, Some((lon, lat)), Some(0)) {
            self.items[i].alt = DEFAULT_ALT_M;
        }
    }

    /// Takes the start away, and the takeoff that stands on it.
    pub fn remove_start(&mut self) {
        if let Some(id) = self.start_takeoff().map(|t| t.id) {
            self.remove_item(id);
        }
        self.home = None;
        self.start_at_vehicle = false;
    }

    /// The slots a moved item may land in.
    pub fn slot_range(&self) -> (usize, usize) {
        let min = usize::from(self.start_takeoff().is_some());
        let max = match self.items.last() {
            Some(last) if last.kind.ends() && self.end_index() == Some(self.items.len() - 1) => {
                self.items.len().saturating_sub(2)
            }
            _ => self.items.len().saturating_sub(1),
        };
        (min, max)
    }

    /// Whether the item at `index` is held in place.
    pub fn pinned(&self, index: isize) -> bool {
        let (min, max) = self.slot_range();
        index < min as isize || index > max as isize
    }

    pub fn remove_item(&mut self, id: u32) {
        let Some(index) = self.index_of(id) else { return };
        self.items.remove(index);
        if self.selected == Some(id) {
            self.selected = self
                .items
                .get(index.min(self.items.len().saturating_sub(1)))
                .map(|i| i.id);
        }
    }

    pub fn move_item(&mut self, id: u32, delta: isize) {
        if let Some(index) = self.index_of(id) {
            self.move_item_to(id, index as isize + delta);
        }
    }

    /// Lifts an item out and drops it at `target`, an index in the list as it
    /// will be afterwards. A target off either end is refused.
    pub fn move_item_to(&mut self, id: u32, target: isize) {
        let Some(index) = self.index_of(id) else { return };
        if target == index as isize || target < 0 || target >= self.items.len() as isize {
            return;
        }
        if self.pinned(index as isize) || self.pinned(target) {
            return;
        }
        let moved = self.items.remove(index);
        self.items.insert(target as usize, moved);
    }

    /// Sets one item's altitude, honouring the shared bounds.
    pub fn set_altitude(&mut self, id: u32, metres: f64) -> Option<f64> {
        let item = self.item_mut(id)?;
        if item.kind == Kind::Land || !item.kind.position() {
            return None;
        }
        item.alt = js_round(clamp_number(metres, ALT_MIN_M, ALT_MAX_M, item.alt) * 10.0) / 10.0;
        Some(item.alt)
    }

    /// Sets the height a landing's descent starts from; None clears it.
    pub fn set_approach(&mut self, id: u32, metres: Option<f64>) {
        let before = self.height_before(id);
        if let Some(item) = self.item_mut(id) {
            if item.kind != Kind::Land {
                return;
            }
            item.approach_alt =
                metres.map(|m| js_round(clamp_number(m, APPROACH_MIN_M, ALT_MAX_M, before) * 10.0) / 10.0);
        }
    }

    /// The height of the station before the item: where it comes in from.
    pub fn height_before(&self, id: u32) -> f64 {
        let points = crate::mission::geometry::stations(&self.items, self.home.as_ref());
        match points.iter().position(|s| s.id == id) {
            Some(i) if i > 0 => points[i - 1].alt,
            _ => DEFAULT_ALT_M,
        }
    }

    /// The speed a point is reached at, or None when nothing ahead pins one.
    pub fn speed_into(&self, id: u32) -> Option<f64> {
        let mut current = None;
        for i in &self.items {
            if i.speed.is_some() {
                current = i.speed;
            }
            if i.id == id {
                return current;
            }
        }
        current
    }

    /// The number the list and the map give the item at `index`: its place
    /// among the items that have a position.
    pub fn position_number(&self, index: usize) -> usize {
        self.items.iter().take(index + 1).filter(|i| i.kind.position()).count()
    }

    pub fn item_label(&self, index: usize) -> String {
        let Some(item) = self.items.get(index) else {
            return String::new();
        };
        if !item.kind.position() {
            return item.display_name().to_string();
        }
        format!("{}. {}", self.position_number(index), item.display_name())
    }

    /// Takes the plan's aircraft from the connected one. True on a change.
    pub fn adopt_vehicle_aircraft(&mut self, vehicle_type: Option<&str>) -> bool {
        let vehicle = vehicle_type.and_then(Aircraft::of_vehicle);
        let locked = vehicle.is_some();
        let changed = (vehicle.is_some() && vehicle != self.aircraft) || locked != self.aircraft_locked;
        if let Some(v) = vehicle {
            self.aircraft = Some(v);
        }
        self.aircraft_locked = locked;
        changed
    }

    /// Follows the plan's kind after it may have changed. `edit` adapts the
    /// plan too. True on a change.
    pub fn sync_drives(&mut self, edit: bool) -> bool {
        let next = self.aircraft == Some(Aircraft::Rover);
        if next == self.drives {
            return false;
        }
        self.drives = next;
        if edit {
            self.adapt_to_kind();
        }
        true
    }

    fn adapt_to_kind(&mut self) {
        if self.drives {
            if let Some(id) = self.start_takeoff().map(|t| t.id) {
                self.remove_item(id);
            }
            return;
        }
        let mut lifted = 0;
        for item in &mut self.items {
            if !item.kind.position() || item.kind == Kind::Land || item.alt != 0.0 {
                continue;
            }
            item.alt = DEFAULT_ALT_M;
            lifted += 1;
        }
        if let Some(h) = self.home {
            if !self.items.iter().any(|i| i.kind == Kind::Takeoff) {
                let keep = self.selected;
                if let Some(i) = self.add_item(Kind::Takeoff, Some((h.lon, h.lat)), Some(0)) {
                    self.items[i].alt = DEFAULT_ALT_M;
                }
                self.selected = keep;
            }
        }
        if lifted > 0 {
            self.notices.push(format!(
                "{} at ground level now {} at {} m. Check the heights in the altitude profile.",
                if lifted == 1 {
                    "A point".to_string()
                } else {
                    format!("{lifted} points")
                },
                if lifted == 1 { "flies" } else { "fly" },
                DEFAULT_ALT_M
            ));
        }
    }

    /// The operator picked what the plan is for. False while a connected
    /// vehicle decides that instead.
    pub fn choose_aircraft(&mut self, value: &str) -> bool {
        if self.aircraft_locked {
            return false;
        }
        self.aircraft = Aircraft::from_value(value);
        self.sync_drives(true);
        true
    }

    /// How far the drawn start is from the aircraft, or None when there is
    /// nothing to compare (or the start follows it).
    pub fn start_offset_m(&self, vehicle: Option<(f64, f64)>) -> Option<f64> {
        let home = self.home?;
        if self.start_at_vehicle {
            return None;
        }
        let (lon, lat) = vehicle?;
        Some(distance_m(home.lat, home.lon, lat, lon))
    }

    /// Moves a start that follows the aircraft to where it is now. True when
    /// it moved; `force` moves it however little that is.
    pub fn follow_vehicle_start(&mut self, vehicle: Option<(f64, f64)>, force: bool) -> bool {
        if !self.start_at_vehicle {
            return false;
        }
        let Some((lon, lat)) = vehicle else { return false };
        if let Some(h) = self.home {
            if !force && distance_m(h.lat, h.lon, lat, lon) < START_FOLLOW_M {
                return false;
            }
        }
        self.place_start(lon, lat, true);
        true
    }

    /// Ties the start to the connected aircraft, or lets go of it.
    pub fn set_start_at_vehicle(&mut self, on: bool, vehicle: Option<(f64, f64)>) {
        self.start_at_vehicle = on;
        if on {
            self.follow_vehicle_start(vehicle, true);
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.home = None;
        self.start_at_vehicle = false;
        self.selected = None;
        self.name = "Mission".into();
    }

    /// The plan in the shape `corvus/mission.py` validates.
    pub fn to_json(&self) -> Value {
        let items: Vec<Value> = self
            .items
            .iter()
            .map(|item| {
                let mut o = serde_json::Map::new();
                o.insert("type".into(), json!(item.kind.name()));
                if item.kind.position() {
                    o.insert("lat".into(), json!(item.lat));
                    o.insert("lon".into(), json!(item.lon));
                    o.insert("alt".into(), json!(item.alt));
                }
                for p in item.kind.params() {
                    o.insert(p.rule().key.into(), json!(item.get(*p)));
                }
                if let Some(s) = item.speed {
                    o.insert("speed".into(), json!(s));
                }
                if item.kind == Kind::Land {
                    if let Some(a) = item.approach_alt {
                        o.insert("approach_alt".into(), json!(a));
                    }
                }
                if !item.name.is_empty() {
                    o.insert("name".into(), json!(item.name));
                }
                Value::Object(o)
            })
            .collect();
        let mut plan = serde_json::Map::new();
        plan.insert("version".into(), json!(1));
        plan.insert(
            "name".into(),
            json!(if self.name.is_empty() { "Mission" } else { &self.name }),
        );
        plan.insert("items".into(), Value::Array(items));
        if let Some(h) = self.home {
            let mut home = json!({"lat": h.lat, "lon": h.lon});
            if let Some(e) = h.elevation {
                home["elevation"] = json!(e);
            }
            plan.insert("home".into(), home);
        }
        if let Some(a) = self.aircraft {
            plan.insert("aircraft".into(), json!(a.value()));
        }
        if self.start_at_vehicle {
            plan.insert("start_at_vehicle".into(), json!(true));
        }
        Value::Object(plan)
    }

    /// Replaces the whole plan from a validated backend payload.
    pub fn load_json(&mut self, plan: &Value, vehicle_type: Option<&str>, vehicle: Option<(f64, f64)>) {
        self.items.clear();
        self.next_id = 1;
        self.selected = None;
        self.name = plan
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("Mission")
            .to_string();
        self.aircraft = plan
            .get("aircraft")
            .and_then(Value::as_str)
            .and_then(Aircraft::from_value);
        self.adopt_vehicle_aircraft(vehicle_type);
        self.sync_drives(false);
        self.home = plan.get("home").filter(|h| h.is_object()).map(|h| Home {
            lat: h.get("lat").and_then(Value::as_f64).unwrap_or(0.0),
            lon: h.get("lon").and_then(Value::as_f64).unwrap_or(0.0),
            elevation: h.get("elevation").and_then(Value::as_f64),
        });
        let num = |raw: &Value, key: &str| raw.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN);
        for raw in plan.get("items").and_then(Value::as_array).into_iter().flatten() {
            let Some(kind) = raw.get("type").and_then(Value::as_str).and_then(Kind::from_name) else {
                continue;
            };
            let mut item = Item::new(self.next_id, kind);
            self.next_id += 1;
            if kind.position() {
                item.lat = num(raw, "lat");
                item.lon = num(raw, "lon");
                item.alt = if kind == Kind::Land {
                    0.0
                } else {
                    clamp_number(num(raw, "alt"), ALT_MIN_M, ALT_MAX_M, DEFAULT_ALT_M)
                };
            }
            for p in kind.params() {
                let r = p.rule();
                item.set(*p, clamp_number(num(raw, r.key), r.min, r.max, r.def));
            }
            item.speed = raw
                .get("speed")
                .filter(|v| !v.is_null())
                .map(|_| clamp_number(num(raw, "speed"), SPEED_MIN_MS, SPEED_MAX_MS, SPEED_MIN_MS));
            item.name = clean_point_name(raw.get("name").and_then(Value::as_str).unwrap_or(""));
            if kind == Kind::Land {
                item.approach_alt = raw
                    .get("approach_alt")
                    .filter(|v| !v.is_null())
                    .map(|_| clamp_number(num(raw, "approach_alt"), APPROACH_MIN_M, ALT_MAX_M, DEFAULT_ALT_M));
            }
            item.snap_direction();
            self.items.push(item);
        }
        if let (Some(speed), Some(first)) = (plan.get("speed").and_then(Value::as_f64), self.items.first_mut()) {
            if first.speed.is_none() {
                first.speed = Some(clamp_number(speed, SPEED_MIN_MS, SPEED_MAX_MS, SPEED_MIN_MS));
            }
        }
        self.start_at_vehicle = plan.get("start_at_vehicle").and_then(Value::as_bool) == Some(true);
        self.follow_vehicle_start(vehicle, true);
    }

    /// The plan as it goes on the wire, for "is this still what was synced":
    /// without the names, which never reach the vehicle.
    pub fn key(&self) -> String {
        let plan = self.to_json();
        let items: Vec<Value> = plan["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| {
                        let mut i = i.clone();
                        if let Some(o) = i.as_object_mut() {
                            o.remove("name");
                        }
                        i
                    })
                    .collect()
            })
            .unwrap_or_default();
        json!({"items": items, "speed": Value::Null, "aircraft": plan.get("aircraft").cloned().unwrap_or(Value::Null)})
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(lon: f64, lat: f64) -> Option<(f64, f64)> {
        Some((lon, lat))
    }

    fn started() -> Plan {
        let mut p = Plan::default();
        p.place_start(11.0, 48.0, false);
        p
    }

    #[test]
    fn the_bounds_are_the_backends() {
        assert_eq!((ALT_MIN_M, ALT_MAX_M), (-100.0, 1000.0));
        assert_eq!((RADIUS_MIN_M, RADIUS_MAX_M), (1.0, 10000.0));
        assert_eq!(
            (HOLD_MAX_S, TURNS_MAX, MAX_ITEMS, POINT_NAME_MAX),
            (3600.0, 100.0, 255, 40)
        );
        assert_eq!((SPEED_MIN_MS, SPEED_MAX_MS, APPROACH_MIN_M), (0.5, 100.0, 3.0));
    }

    #[test]
    fn the_item_table_matches_the_backends_specs() {
        let names: Vec<&str> = KINDS.iter().map(|k| k.name()).collect();
        assert_eq!(
            names,
            ["takeoff", "waypoint", "loiter_turns", "loiter_time", "land", "rtl"]
        );
        let keys = |k: Kind| k.params().iter().map(|p| p.rule().key).collect::<Vec<_>>();
        assert_eq!(keys(Kind::Takeoff), ["pitch"]);
        assert_eq!(keys(Kind::Waypoint), ["hold", "accept_radius"]);
        assert_eq!(keys(Kind::LoiterTurns), ["turns", "radius", "direction"]);
        assert_eq!(keys(Kind::LoiterTime), ["seconds", "radius", "direction"]);
        assert!(keys(Kind::Land).is_empty() && keys(Kind::Rtl).is_empty());
        let positionless: Vec<Kind> = KINDS.into_iter().filter(|k| !k.position()).collect();
        assert_eq!(positionless, [Kind::Rtl]);
        assert_eq!(Param::Direction.rule().def, 1.0, "clockwise unless asked otherwise");
        let aircraft: Vec<&str> = AIRCRAFT.iter().map(|a| a.value()).collect();
        assert_eq!(aircraft, ["multirotor", "fixed_wing", "vtol", "rover"]);
    }

    #[test]
    fn a_vehicle_type_picks_the_plans_aircraft() {
        assert_eq!(Aircraft::of_vehicle("QUADROTOR"), Some(Aircraft::Multirotor));
        assert_eq!(Aircraft::of_vehicle("HELICOPTER"), Some(Aircraft::Multirotor));
        assert_eq!(Aircraft::of_vehicle("FIXED_WING"), Some(Aircraft::FixedWing));
        assert_eq!(Aircraft::of_vehicle("VTOL_QUADROTOR"), Some(Aircraft::Vtol));
        assert_eq!(Aircraft::of_vehicle("SURFACE_BOAT"), Some(Aircraft::Rover));
        assert_eq!(Aircraft::of_vehicle("SUBMARINE"), None);
        assert_eq!(Aircraft::of_vehicle(""), None);
    }

    #[test]
    fn the_start_carries_the_takeoff_and_moving_it_moves_both() {
        let mut p = started();
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.items[0].kind, Kind::Takeoff);
        assert_eq!(p.items[0].alt, DEFAULT_ALT_M);
        assert!(p.start_takeoff().is_some());
        p.place_start(11.5, 48.5, false);
        assert_eq!(p.items.len(), 1, "no second takeoff");
        assert_eq!((p.items[0].lon, p.items[0].lat), (11.5, 48.5));
        p.remove_start();
        assert!(p.items.is_empty() && p.home.is_none());
    }

    #[test]
    fn new_points_go_in_before_the_ending_at_the_height_before() {
        let mut p = started();
        p.add_item(Kind::Waypoint, at(11.001, 48.0), None);
        p.items[1].alt = 55.0;
        p.add_item(Kind::Land, at(11.002, 48.0), None);
        assert_eq!(p.items[2].alt, 0.0, "a touchdown is on the ground");
        p.add_item(Kind::Waypoint, at(11.003, 48.0), None);
        let kinds: Vec<Kind> = p.items.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, [Kind::Takeoff, Kind::Waypoint, Kind::Waypoint, Kind::Land]);
        assert_eq!(p.items[2].alt, 55.0);
        assert_eq!(p.end_index(), Some(3));
    }

    #[test]
    fn a_second_ending_and_a_start_less_plan_are_refused_with_a_reason() {
        let mut p = Plan::default();
        let link = Link::default();
        assert_eq!(
            p.tool_blocked(Tool::Item(Kind::Waypoint), &link).as_deref(),
            Some("Set the start first. The mission begins there.")
        );
        assert_eq!(p.tool_blocked(Tool::Home, &link), None);
        p.place_start(11.0, 48.0, false);
        assert_eq!(p.tool_blocked(Tool::Item(Kind::Waypoint), &link), None);
        p.add_item(Kind::Land, at(11.001, 48.0), None);
        let why = p.tool_blocked(Tool::Item(Kind::Rtl), &link).expect("blocked");
        assert_eq!(
            why,
            "The mission already ends with a landing. Remove it to end the mission differently."
        );
        assert!(p.tool_blocked(Tool::Shape(Shape::Area), &link).is_none());
    }

    #[test]
    fn the_aircraft_button_needs_a_connected_aircraft_with_a_position() {
        let mut p = started();
        let off = Link::default();
        assert_eq!(
            p.tool_blocked(Tool::VehicleStart, &off).as_deref(),
            Some("Connect an aircraft first. The start is then put where it stands.")
        );
        let no_fix = Link {
            connected: true,
            start: None,
        };
        assert_eq!(
            p.tool_blocked(Tool::VehicleStart, &no_fix).as_deref(),
            Some("The aircraft has not reported a position yet.")
        );
        p.start_at_vehicle = true;
        assert_eq!(p.tool_blocked(Tool::VehicleStart, &off), None, "free to switch off");
    }

    #[test]
    fn a_stack_that_refuses_a_type_blocks_its_tool_with_what_to_use_instead() {
        let mut p = started();
        p.refusals
            .insert("loiter_turns".into(), "PX4 does not fly an orbit".into());
        let why = p
            .tool_blocked(Tool::Item(Kind::LoiterTurns), &Link::default())
            .expect("refused");
        assert!(why.starts_with("PX4 does not fly an orbit. Use HOLD"), "{why}");
        assert_eq!(p.tool_blocked(Tool::Item(Kind::LoiterTime), &Link::default()), None);
    }

    #[test]
    fn rows_move_between_the_takeoff_and_the_ending_only() {
        let mut p = started();
        for lon in [11.001, 11.002, 11.003] {
            p.add_item(Kind::Waypoint, at(lon, 48.0), None);
        }
        p.add_item(Kind::Rtl, None, None);
        assert_eq!(p.slot_range(), (1, 3));
        assert!(p.pinned(0) && p.pinned(4) && !p.pinned(2));
        let second = p.items[2].id;
        p.move_item(second, -1);
        assert_eq!(p.items[1].id, second);
        p.move_item(second, -1);
        assert_eq!(p.items[1].id, second, "never before the climb");
        let last = p.items[3].id;
        p.move_item_to(last, 4);
        assert_eq!(p.items[3].id, last, "never past the return");
        p.move_item_to(last, 1);
        assert_eq!(p.items[1].id, last);
    }

    #[test]
    fn deleting_the_selected_item_selects_its_neighbour() {
        let mut p = started();
        p.add_item(Kind::Waypoint, at(11.001, 48.0), None);
        p.add_item(Kind::Waypoint, at(11.002, 48.0), None);
        let (a, b, c) = (p.items[0].id, p.items[1].id, p.items[2].id);
        p.selected = Some(b);
        p.remove_item(b);
        assert_eq!(p.selected, Some(c));
        p.remove_item(c);
        assert_eq!(p.selected, Some(a));
    }

    #[test]
    fn altitude_is_clamped_rounded_and_never_set_on_a_landing() {
        let mut p = started();
        p.add_item(Kind::Waypoint, at(11.001, 48.0), None);
        p.add_item(Kind::Land, at(11.002, 48.0), None);
        let (w, l) = (p.items[1].id, p.items[2].id);
        assert_eq!(p.set_altitude(w, 33.37), Some(33.4));
        assert_eq!(p.set_altitude(w, 5000.0), Some(1000.0));
        assert_eq!(p.set_altitude(w, -500.0), Some(-100.0));
        assert_eq!(p.set_altitude(l, 20.0), None);
        p.set_approach(l, Some(1.0));
        assert_eq!(
            p.items[2].approach_alt,
            Some(3.0),
            "dragged into the ground it stops at the floor"
        );
        p.set_approach(l, None);
        assert_eq!(p.items[2].approach_alt, None);
    }

    #[test]
    fn driving_takes_the_takeoff_away_and_flying_puts_it_back_with_heights() {
        let mut p = started();
        p.add_item(Kind::Waypoint, at(11.001, 48.0), None);
        assert!(p.choose_aircraft("rover"));
        assert!(p.drives);
        assert!(p.items.iter().all(|i| i.kind != Kind::Takeoff));
        p.add_item(Kind::Waypoint, at(11.002, 48.0), None);
        assert_eq!(
            p.items.last().map(|i| i.alt),
            Some(0.0),
            "a rover's points sit on the ground"
        );
        assert!(p
            .tool_blocked(Tool::Item(Kind::Land), &Link::default())
            .unwrap()
            .contains("does not land"));
        assert!(p.choose_aircraft("multirotor"));
        assert_eq!(p.items[0].kind, Kind::Takeoff);
        assert!(p.items[1..].iter().all(|i| i.alt == DEFAULT_ALT_M));
        assert!(
            p.notices
                .iter()
                .any(|n| n.starts_with("A point at ground level now flies at 30 m")),
            "{:?}",
            p.notices
        );
    }

    #[test]
    fn a_connected_vehicle_decides_the_kind_and_locks_the_choice() {
        let mut p = started();
        assert!(p.adopt_vehicle_aircraft(Some("QUADROTOR")));
        assert_eq!(p.aircraft, Some(Aircraft::Multirotor));
        assert!(p.aircraft_locked);
        assert!(!p.choose_aircraft("fixed_wing"));
        assert!(!p.adopt_vehicle_aircraft(Some("QUADROTOR")), "nothing changed");
        assert!(p.adopt_vehicle_aircraft(None), "the lock comes off");
        assert_eq!(p.aircraft, Some(Aircraft::Multirotor), "the plan keeps the kind it set");
        assert!(p.choose_aircraft(""));
        assert_eq!(p.aircraft, None);
    }

    #[test]
    fn a_start_that_follows_the_aircraft_moves_only_past_three_metres() {
        let mut p = started();
        p.set_start_at_vehicle(true, at(11.0001, 48.0001));
        let h = p.home.expect("home");
        assert_eq!((h.lon, h.lat), (11.0001, 48.0001));
        assert_eq!(p.items[0].lat, 48.0001, "and the takeoff");
        assert!(
            !p.follow_vehicle_start(at(11.0001, 48.00011), false),
            "the fix wandering"
        );
        assert!(p.follow_vehicle_start(at(11.0001, 48.0002), false));
        assert_eq!(p.start_offset_m(at(11.0, 48.0)), None, "tied: nothing to compare");
        p.start_at_vehicle = false;
        let off = p.start_offset_m(at(11.0, 48.0)).expect("offset");
        assert!((off - 23.45).abs() < 0.1, "{off}");
    }

    #[test]
    fn the_wire_form_has_only_the_fields_a_kind_names() {
        let mut p = started();
        p.add_item(Kind::Waypoint, at(11.001, 48.0), None);
        p.items[1].speed = Some(6.0);
        p.items[1].name = "Ridge".into();
        p.add_item(Kind::Land, at(11.002, 48.0), None);
        p.items[2].approach_alt = Some(15.0);
        p.aircraft = Some(Aircraft::Multirotor);
        let j = p.to_json();
        assert_eq!(j["version"], 1);
        assert_eq!(j["name"], "Mission");
        assert_eq!(j["aircraft"], "multirotor");
        assert_eq!(j["home"], serde_json::json!({"lat": 48.0, "lon": 11.0}));
        assert!(j.get("start_at_vehicle").is_none());
        let items = j["items"].as_array().expect("items");
        assert_eq!(
            items[0],
            serde_json::json!({"type": "takeoff", "lat": 48.0, "lon": 11.0, "alt": 30.0, "pitch": 0.0})
        );
        assert_eq!(items[1]["speed"], 6.0);
        assert_eq!(items[1]["name"], "Ridge");
        assert_eq!(items[1]["hold"], 0.0);
        assert_eq!(items[2]["approach_alt"], 15.0);
        assert!(items[2].get("hold").is_none() && items[2].get("speed").is_none());
        p.add_item(Kind::LoiterTime, at(11.0015, 48.0), Some(2));
        assert_eq!(p.to_json()["items"][2]["direction"], 1.0);
    }

    #[test]
    fn a_plan_read_back_is_clamped_and_keeps_an_absent_speed_absent() {
        let mut p = Plan::default();
        let raw = serde_json::json!({
            "name": "Survey",
            "home": {"lat": 48.0, "lon": 11.0, "elevation": 520.0},
            "aircraft": "fixed_wing",
            "speed": 12,
            "start_at_vehicle": true,
            "items": [
                {"type": "waypoint", "lat": 48.0, "lon": 11.0, "alt": 5000, "hold": -3, "name": "A\n  B"},
                {"type": "loiter_time", "lat": 48.0, "lon": 11.0, "alt": 40, "seconds": 20, "radius": 0, "direction": 0, "speed": 0.1},
                {"type": "land", "lat": 48.0, "lon": 11.0, "alt": 9, "approach_alt": 1},
                {"type": "nonsense"}
            ]
        });
        p.load_json(&raw, None, None);
        assert_eq!(p.name, "Survey");
        assert_eq!(p.items.len(), 3);
        assert_eq!(
            (p.items[0].alt, p.items[0].hold, p.items[0].name.as_str()),
            (1000.0, 0.0, "A B")
        );
        assert_eq!(
            p.items[0].speed,
            Some(12.0),
            "the plan wide speed goes to the first item"
        );
        assert_eq!(
            (p.items[1].radius, p.items[1].direction, p.items[1].speed),
            (1.0, 1.0, Some(0.5))
        );
        assert_eq!((p.items[2].alt, p.items[2].approach_alt), (0.0, Some(3.0)));
        assert!(p.start_at_vehicle);
        assert_eq!(p.home.and_then(|h| h.elevation), Some(520.0));
        assert_eq!(p.aircraft, Some(Aircraft::FixedWing));
    }

    #[test]
    fn point_names_are_cleaned_as_the_backend_cleans_them() {
        assert_eq!(clean_point_name("  a\tb\u{2028}c  "), "a b c");
        assert_eq!(clean_point_name(&"x".repeat(60)).chars().count(), 40);
        assert_eq!(clean_point_name(&format!("{}😀😀", "y".repeat(39))).chars().count(), 40);
    }

    #[test]
    fn the_sync_key_ignores_names() {
        let mut p = started();
        p.add_item(Kind::Waypoint, at(11.001, 48.0), None);
        let before = p.key();
        p.items[1].name = "Renamed".into();
        assert_eq!(p.key(), before, "names never reach the vehicle");
        p.items[1].alt += 1.0;
        assert_ne!(p.key(), before);
    }

    #[test]
    fn the_item_limit_is_said_not_silently_hit() {
        let mut p = started();
        for i in 0..MAX_ITEMS - 1 {
            p.add_item(Kind::Waypoint, at(11.0 + i as f64 * 1e-5, 48.0), None);
        }
        assert_eq!(p.items.len(), MAX_ITEMS);
        assert!(p.add_item(Kind::Waypoint, at(11.5, 48.0), None).is_none());
        assert_eq!(p.notices, ["A mission can hold 255 items."]);
    }

    #[test]
    fn a_return_names_no_place_and_is_numbered_by_name() {
        let mut p = started();
        p.add_item(Kind::Waypoint, at(11.001, 48.0), None);
        p.add_item(Kind::Rtl, None, None);
        assert_eq!(p.item_label(1), "2. Waypoint");
        assert_eq!(p.item_label(2), "Return");
        assert_eq!(p.position_number(2), 2);
    }
}
