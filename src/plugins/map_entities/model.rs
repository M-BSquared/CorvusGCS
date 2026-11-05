//! Map Entities' rules, apart from any window
//! (`python/plugins/map-entities/map-entities.js`): what can be placed, how an
//! entity is read back from the saved settings, and how the aircraft's track is
//! held against gates and zones. Geometry is metres on a flat plane around each
//! entity, as the web version does it. Held to what the web functions answered.

use serde_json::{json, Map, Value};

pub const MAX_ENTITIES: usize = 500;
pub const MAX_RESULTS: usize = 50;
pub const NAME_MAX: usize = 60;
pub const NOTE_MAX: usize = 300;
/// A jump this long between two samples is a lost link or a new fix.
pub const MAX_STEP_M: f64 = 500.0;
pub const MIN_STEP_M: f64 = 0.3;
const METRES_PER_DEGREE: f64 = std::f64::consts::PI / 180.0 * 6_378_137.0;

pub const PASS_COLOR: &str = "#7BD389";
pub const MISS_COLOR: &str = "#FF9F43";
pub const HIT_COLOR: &str = "#FF5C5C";

/// `(label, colour)` of the colour select.
pub const COLORS: [(&str, &str); 9] = [
    ("Green", "#7BD389"),
    ("Dark green", "#1E8E3E"),
    ("Cyan", "#2BC4E4"),
    ("Blue", "#5B8DEF"),
    ("Pink", "#C77DFF"),
    ("Yellow", "#FFD60A"),
    ("Orange", "#FF9F43"),
    ("Red", "#FF5C5C"),
    ("White", "#FFFFFF"),
];

/// What can be placed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Kind {
    pub id: &'static str,
    pub label: &'static str,
    pub gate: bool,
    pub color: &'static str,
    pub radius: f64,
    pub height: f64,
    pub base: f64,
    pub alert: bool,
}

const fn point(id: &'static str, label: &'static str, color: &'static str, radius: f64, alert: bool) -> Kind {
    Kind {
        id,
        label,
        gate: false,
        color,
        radius,
        height: 0.0,
        base: 0.0,
        alert,
    }
}

pub const KINDS: [Kind; 14] = [
    Kind {
        id: "gate",
        label: "Gate",
        gate: true,
        color: "#FFD60A",
        radius: 0.0,
        height: 5.0,
        base: 0.0,
        alert: false,
    },
    Kind {
        height: 12.0,
        ..point("tree", "Tree", "#7BD389", 3.0, true)
    },
    point("radar", "Radar", "#5B8DEF", 500.0, true),
    point("camera", "Camera", "#2BC4E4", 150.0, true),
    point("antenna", "Antenna", "#C77DFF", 300.0, false),
    point("person", "Person", "#1E8E3E", 0.0, false),
    point("vehicle", "Vehicle", "#FFFFFF", 0.0, false),
    point("building", "Building", "#FFFFFF", 0.0, false),
    point("explosion", "Explosion", "#FF5C5C", 50.0, true),
    point("fire", "Fire", "#FF9F43", 30.0, true),
    point("hazard", "Hazard", "#FFD60A", 0.0, false),
    point("target", "Target", "#FF5C5C", 0.0, false),
    point("pad", "Landing pad", "#5B8DEF", 0.0, false),
    point("flag", "Flag", "#2BC4E4", 0.0, false),
];

pub fn kind_of(id: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.id == id)
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.max(lo).min(hi)
}

/// `finite(value, fallback)`: a number from a JSON number or numeric text.
pub fn finite(v: Option<&Value>, fallback: f64) -> f64 {
    let n = match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(Value::Bool(b)) => Some(f64::from(u8::from(*b))),
        _ => None,
    };
    n.filter(|n| n.is_finite()).unwrap_or(fallback)
}

pub fn valid_point(lng: f64, lat: f64) -> bool {
    lng.is_finite() && lat.is_finite() && lat.abs() <= 90.0 && lng.abs() <= 180.0
}

/// Plain one line text of at most `max` characters.
pub fn plain_name(text: &str, max: usize) -> String {
    plain(text, max)
}

fn plain(text: &str, max: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

/// One angle in decimal degrees from what an operator typed; a decimal comma
/// is fine. `None` for anything that is not a number inside +-`limit`.
pub fn parse_degrees(text: &str, limit: f64) -> Option<f64> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let n: f64 = t.replace(',', ".").parse().ok()?;
    (n.is_finite() && n.abs() <= limit).then_some(n)
}

/// `48.1234, 11.5678` as pasted from a map: latitude first, then longitude,
/// separated by a comma, a semicolon or white space; after a semicolon the
/// numbers may use a decimal comma. `None` unless there are exactly two.
pub fn parse_coordinate_pair(text: &str) -> Option<(f64, f64)> {
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }
    let tokens: Vec<&str> = if raw.contains(';') {
        raw.split(';').map(str::trim).filter(|t| !t.is_empty()).collect()
    } else {
        raw.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .collect()
    };
    if tokens.len() != 2 {
        return None;
    }
    Some((parse_degrees(tokens[0], 90.0)?, parse_degrees(tokens[1], 180.0)?))
}

/// `Number(value).toFixed(7)` without trailing zeros (`formatDegrees`).
pub fn format_degrees(v: f64) -> String {
    let s = format!("{v:.7}");
    let s = s.trim_end_matches('0');
    if s.ends_with('.') {
        format!("{s}0")
    } else {
        s.to_string()
    }
}

/// `[lng, lat]` as east and north metres around `origin`.
pub fn to_local(origin: [f64; 2], p: [f64; 2]) -> [f64; 2] {
    [
        (p[0] - origin[0]) * METRES_PER_DEGREE * (origin[1] * std::f64::consts::PI / 180.0).cos(),
        (p[1] - origin[1]) * METRES_PER_DEGREE,
    ]
}

pub fn distance_m(a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = to_local(a, b);
    d[0].hypot(d[1])
}

/// Compass bearing from `a` to `b`, 0 to 360.
pub fn bearing_deg(a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = to_local(a, b);
    (d[0].atan2(d[1]).to_degrees() + 360.0) % 360.0
}

fn cross(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}

/// Where the aircraft was.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub lng: f64,
    pub lat: f64,
    pub alt: f64,
}

/// What an entity is, with the fields of its shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub id: String,
    pub kind: &'static Kind,
    pub name: String,
    pub color: String,
    pub lng: f64,
    pub lat: f64,
    pub note: String,
    pub lng2: f64,
    pub lat2: f64,
    pub base: f64,
    pub height: f64,
    pub radius: f64,
    pub alert: bool,
    pub angle: f64,
    pub bearing: f64,
}

impl Entity {
    pub fn is_gate(&self) -> bool {
        self.kind.gate
    }

    pub fn is_tree(&self) -> bool {
        self.kind.id == "tree"
    }

    /// The angle of a zone's area in degrees: 360 for a full circle. A tree's
    /// crown is always round.
    pub fn sector_angle(&self) -> f64 {
        if self.is_tree() {
            360.0
        } else {
            clamp(self.angle, 1.0, 360.0)
        }
    }

    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        for (k, v) in [
            ("id", json!(self.id)),
            ("kind", json!(self.kind.id)),
            ("name", json!(self.name)),
            ("color", json!(self.color)),
            ("lng", json!(self.lng)),
            ("lat", json!(self.lat)),
            ("note", json!(self.note)),
        ] {
            m.insert(k.into(), v);
        }
        if self.is_gate() {
            for (k, v) in [
                ("lng2", self.lng2),
                ("lat2", self.lat2),
                ("base", self.base),
                ("height", self.height),
            ] {
                m.insert(k.into(), json!(v));
            }
        } else {
            m.insert("radius".into(), json!(self.radius));
            m.insert("alert".into(), json!(self.alert));
            if self.is_tree() {
                m.insert("height".into(), json!(self.height));
            } else {
                m.insert("angle".into(), json!(self.angle));
                m.insert("bearing".into(), json!(self.bearing));
            }
        }
        Value::Object(m)
    }

    fn base_of(kind: &'static Kind, id: String, name: String, lng: f64, lat: f64) -> Entity {
        Entity {
            id,
            kind,
            name,
            color: kind.color.into(),
            lng,
            lat,
            note: String::new(),
            lng2: 0.0,
            lat2: 0.0,
            base: 0.0,
            height: 0.0,
            radius: 0.0,
            alert: false,
            angle: 360.0,
            bearing: 0.0,
        }
    }
}

/// `Gate 3`, `Tree 4`: the next free number of the kind.
pub fn next_name(entities: &[Entity], kind: &Kind) -> String {
    let mut n = 0;
    for e in entities.iter().filter(|e| e.kind.id == kind.id) {
        if let Some(rest) = e.name.strip_prefix(&format!("{} ", kind.label)) {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                n = n.max(rest.parse::<u64>().unwrap_or(0));
            }
        }
    }
    format!("{} {}", kind.label, n + 1)
}

pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("e{ms:x}{:x}", N.fetch_add(1, Ordering::Relaxed))
}

/// A new entity with its kind's defaults; a gate needs its second post.
pub fn make_entity(kind_id: &str, p: [f64; 2], second: Option<[f64; 2]>, existing: &[Entity]) -> Option<Entity> {
    let kind = kind_of(kind_id)?;
    if !valid_point(p[0], p[1]) {
        return None;
    }
    let mut e = Entity::base_of(kind, new_id(), next_name(existing, kind), p[0], p[1]);
    if kind.gate {
        let s = second.filter(|s| valid_point(s[0], s[1]))?;
        e.lng2 = s[0];
        e.lat2 = s[1];
        e.base = kind.base;
        e.height = kind.height;
    } else {
        e.radius = kind.radius;
        e.alert = kind.alert;
        if kind.id == "tree" {
            e.height = kind.height;
        }
    }
    Some(e)
}

fn id_ok(s: &str) -> bool {
    (1..=24).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// One saved entity read back, or `None` for what cannot be one.
pub fn normalize_entity(raw: &Value) -> Option<Entity> {
    let o = raw.as_object()?;
    let kind = kind_of(o.get("kind")?.as_str()?)?;
    let lng = finite(o.get("lng"), f64::NAN);
    let lat = finite(o.get("lat"), f64::NAN);
    if !valid_point(lng, lat) {
        return None;
    }
    let id = o
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| id_ok(s))
        .map_or_else(new_id, str::to_string);
    let name = plain(o.get("name").and_then(Value::as_str).unwrap_or(""), NAME_MAX);
    let mut e = Entity::base_of(
        kind,
        id,
        if name.is_empty() { kind.label.into() } else { name },
        lng,
        lat,
    );
    if let Some(c) = o
        .get("color")
        .and_then(Value::as_str)
        .filter(|c| crate::plugins::shapes::hex(c).is_some())
    {
        e.color = c.into();
    }
    e.note = plain(o.get("note").and_then(Value::as_str).unwrap_or(""), NOTE_MAX);
    if kind.gate {
        let lng2 = finite(o.get("lng2"), f64::NAN);
        let lat2 = finite(o.get("lat2"), f64::NAN);
        if !valid_point(lng2, lat2) {
            return None;
        }
        e.lng2 = lng2;
        e.lat2 = lat2;
        e.base = clamp(finite(o.get("base"), kind.base), -1000.0, 10000.0);
        e.height = clamp(finite(o.get("height"), kind.height), 0.0, 10000.0);
    } else {
        e.radius = clamp(finite(o.get("radius"), kind.radius), 0.0, 100_000.0);
        e.alert = match o.get("alert") {
            None | Some(Value::Null) => kind.alert,
            Some(v) => truthy(v),
        };
        if kind.id == "tree" {
            e.height = clamp(finite(o.get("height"), kind.height), 0.0, 1000.0);
        } else {
            e.angle = clamp(finite(o.get("angle"), 360.0), 1.0, 360.0);
            e.bearing = ((finite(o.get("bearing"), 0.0) % 360.0) + 360.0) % 360.0;
        }
    }
    Some(e)
}

/// JavaScript's `!!value`.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// The saved list: at most 500, every id once.
pub fn normalize_list(list: &Value) -> Vec<Entity> {
    let Some(items) = list.as_array() else {
        return Vec::new();
    };
    let mut out: Vec<Entity> = Vec::new();
    for raw in items.iter().take(MAX_ENTITIES) {
        let Some(mut e) = normalize_entity(raw) else { continue };
        while out.iter().any(|o| o.id == e.id) {
            e.id = new_id();
        }
        out.push(e);
    }
    out
}

/// Where a gate was crossed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crossing {
    pub u: f64,
    pub t: f64,
    pub offset: f64,
    pub alt: f64,
}

/// Where the step from `prev` to `cur` crosses a gate's line; `None` when it
/// does not. A step that ends exactly on the line belongs to this step, so a
/// crossing is never counted twice.
pub fn gate_crossing(g: &Entity, prev: Sample, cur: Sample) -> Option<Crossing> {
    let origin = [g.lng, g.lat];
    let r = to_local(origin, [g.lng2, g.lat2]);
    let p = to_local(origin, [prev.lng, prev.lat]);
    let q = to_local(origin, [cur.lng, cur.lat]);
    let d = [q[0] - p[0], q[1] - p[1]];
    let denom = cross(r, d);
    if denom.abs() < 1e-9 {
        return None;
    }
    let u = cross(p, d) / denom;
    let t = cross(p, r) / denom;
    if !(0.0..=1.0).contains(&u) || t <= 0.0 || t > 1.0 {
        return None;
    }
    let width = r[0].hypot(r[1]);
    Some(Crossing {
        u,
        t,
        offset: (u - 0.5) * width,
        alt: prev.alt + t * (cur.alt - prev.alt),
    })
}

/// Whether a crossing went through the opening: `""`, `"above"` or `"below"`.
pub fn gate_verdict(g: &Entity, c: &Crossing) -> (bool, &'static str) {
    let top = g.base + g.height;
    if c.alt > top {
        (false, "above")
    } else if c.alt < g.base {
        (false, "below")
    } else {
        (true, "")
    }
}

/// How close a step came to a zone's centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    pub touched: bool,
    pub dist: f64,
    pub alt: f64,
    pub inside: bool,
}

/// Whether a point, in metres east and north of the zone's centre, lies in
/// the zone's area: inside the radius and, for a sector, within half its angle
/// either side of the direction it faces. The centre always counts.
pub fn in_sector(z: &Entity, local: [f64; 2]) -> bool {
    let dist = local[0].hypot(local[1]);
    if dist > z.radius {
        return false;
    }
    let angle = z.sector_angle();
    if angle >= 360.0 || dist == 0.0 {
        return true;
    }
    let to_point = (local[0].atan2(local[1]).to_degrees() + 360.0) % 360.0;
    let off = (((to_point - z.bearing + 540.0) % 360.0) - 180.0).abs();
    off <= angle / 2.0
}

fn sector_contact(z: &Entity, prev: Sample, cur: Sample) -> Contact {
    let origin = [z.lng, z.lat];
    let p = to_local(origin, [prev.lng, prev.lat]);
    let q = to_local(origin, [cur.lng, cur.lat]);
    let d = [q[0] - p[0], q[1] - p[1]];
    let len = d[0].hypot(d[1]);
    let pace = (z.radius / 30.0).clamp(0.5, 5.0);
    let n = clamp((len / pace).ceil(), 1.0, 400.0) as usize;
    let (mut touched, mut alt, mut dist) = (false, cur.alt, f64::INFINITY);
    for i in 0..=n {
        let t = i as f64 / n as f64;
        let at = [p[0] + t * d[0], p[1] + t * d[1]];
        dist = dist.min(at[0].hypot(at[1]));
        if !touched && in_sector(z, at) {
            touched = true;
            alt = prev.alt + t * (cur.alt - prev.alt);
        }
    }
    Contact {
        touched,
        dist,
        alt,
        inside: in_sector(z, q),
    }
}

/// How close the step from `prev` to `cur` came to a zone's centre.
pub fn zone_contact(z: &Entity, prev: Sample, cur: Sample) -> Contact {
    if z.sector_angle() < 360.0 {
        return sector_contact(z, prev, cur);
    }
    let origin = [z.lng, z.lat];
    let p = to_local(origin, [prev.lng, prev.lat]);
    let q = to_local(origin, [cur.lng, cur.lat]);
    let d = [q[0] - p[0], q[1] - p[1]];
    let len2 = d[0] * d[0] + d[1] * d[1];
    let t = if len2 > 0.0 {
        clamp(-(p[0] * d[0] + p[1] * d[1]) / len2, 0.0, 1.0)
    } else {
        1.0
    };
    let dist = (p[0] + t * d[0]).hypot(p[1] + t * d[1]);
    Contact {
        touched: dist <= z.radius,
        dist,
        alt: prev.alt + t * (cur.alt - prev.alt),
        inside: q[0].hypot(q[1]) <= z.radius,
    }
}

/// The corners of a sector: the centre, then the arc. Not closed.
pub fn sector_ring(z: &Entity) -> Vec<[f64; 2]> {
    let angle = z.sector_angle();
    let start = z.bearing - angle / 2.0;
    let steps = ((angle / 5.0).ceil() as usize).max(2);
    let cos = z.lat.to_radians().cos().max(0.01);
    let mut ring = vec![[z.lng, z.lat]];
    for i in 0..=steps {
        let theta = (start + angle * i as f64 / steps as f64).to_radians();
        ring.push([
            z.lng + z.radius * theta.sin() / (METRES_PER_DEGREE * cos),
            z.lat + z.radius * theta.cos() / METRES_PER_DEGREE,
        ]);
    }
    ring
}

/// The aircraft's position from telemetry, or `None` when there is nothing to
/// check: no link, no fix, not armed (when only armed flight counts).
pub fn sample_of(state: &Value, armed_only: bool) -> Option<Sample> {
    if state.get("connected") != Some(&Value::Bool(true)) {
        return None;
    }
    let pos = state.get("position")?.as_array()?;
    if armed_only && state.get("armed").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let lng = pos.first().and_then(Value::as_f64)?;
    let lat = pos.get(1).and_then(Value::as_f64)?;
    if !valid_point(lng, lat) || (lng == 0.0 && lat == 0.0) {
        return None;
    }
    Some(Sample {
        lng,
        lat,
        alt: finite(state.get("altitude_agl"), 0.0),
    })
}

/// One line of an entity's results.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub t: f64,
    pub ok: bool,
    pub reason: &'static str,
    pub alt: f64,
    pub offset: f64,
}

/// What the aircraft did to one entity this session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Results {
    pub passes: u32,
    pub misses: u32,
    pub entries: u32,
    pub hits: u32,
    pub inside: bool,
    pub log: Vec<Entry>,
}

impl Results {
    fn push(&mut self, e: Entry) {
        self.log.push(e);
        if self.log.len() > MAX_RESULTS {
            self.log.remove(0);
        }
    }
}

/// Something worth telling the operator.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub entity: String,
    pub level: &'static str,
    pub text: String,
}

/// Holds one step of the track against every entity.
pub fn process_step(
    entities: &[Entity],
    results: &mut std::collections::HashMap<String, Results>,
    prev: Sample,
    cur: Sample,
    now: f64,
) -> Vec<Event> {
    let mut events = Vec::new();
    for e in entities {
        let r = results.entry(e.id.clone()).or_default();
        if e.is_gate() {
            let Some(c) = gate_crossing(e, prev, cur) else { continue };
            let (ok, reason) = gate_verdict(e, &c);
            r.push(Entry {
                t: now,
                ok,
                reason,
                alt: c.alt,
                offset: c.offset,
            });
            if ok {
                r.passes += 1;
                events.push(Event {
                    entity: e.id.clone(),
                    level: "info",
                    text: format!("{}: flown through", e.name),
                });
            } else {
                r.misses += 1;
                events.push(Event {
                    entity: e.id.clone(),
                    level: "warning",
                    text: format!(
                        "{}: crossed {} the opening",
                        e.name,
                        if reason == "above" { "above" } else { "below" }
                    ),
                });
            }
            continue;
        }
        if e.radius <= 0.0 {
            continue;
        }
        let c = zone_contact(e, prev, cur);
        let tree = e.is_tree();
        let low = !tree || c.alt <= e.height;
        let touching = c.touched && low;
        if touching && !r.inside {
            r.entries += 1;
            if tree {
                r.hits += 1;
            }
            r.push(Entry {
                t: now,
                ok: !tree,
                reason: "",
                alt: c.alt,
                offset: c.dist,
            });
            if e.alert {
                events.push(Event {
                    entity: e.id.clone(),
                    level: if tree { "warning" } else { "info" },
                    text: if tree {
                        format!("{}: aircraft is inside the crown, below the top", e.name)
                    } else {
                        format!("{}: aircraft entered the area", e.name)
                    },
                });
            }
        }
        r.inside = c.inside && (!tree || cur.alt <= e.height);
    }
    events
}

/// The colour an entity is drawn in: its own until the aircraft has done
/// something to it.
pub fn status_color<'a>(e: &'a Entity, r: Option<&Results>) -> &'a str {
    let Some(r) = r else { return &e.color };
    if e.is_gate() {
        if r.passes > 0 {
            return PASS_COLOR;
        }
        return if r.misses > 0 { MISS_COLOR } else { &e.color };
    }
    if e.is_tree() && r.hits > 0 {
        return HIT_COLOR;
    }
    if r.inside {
        MISS_COLOR
    } else {
        &e.color
    }
}

/// `Math.round(v * 10) / 10 + " m"`.
pub fn metres(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    format!("{} m", num(r))
}

/// A number as JavaScript prints it: no trailing `.0`.
pub fn num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// The value on the right of a list row and its level.
pub fn row_detail(e: &Entity, r: Option<&Results>) -> (String, &'static str) {
    if e.is_gate() {
        return match r {
            Some(r) if r.passes > 0 => (format!("{}\u{d7} through", r.passes), "good"),
            Some(r) if r.misses > 0 => (format!("{}\u{d7} missed", r.misses), "warn"),
            _ => (metres(distance_m([e.lng, e.lat], [e.lng2, e.lat2])), ""),
        };
    }
    if e.is_tree() {
        if let Some(r) = r.filter(|r| r.hits > 0) {
            return (format!("hit \u{d7}{}", r.hits), "bad");
        }
        return (format!("{} \u{b7} r {}", metres(e.height), metres(e.radius)), "");
    }
    if r.is_some_and(|r| r.inside) {
        return ("inside".into(), "warn");
    }
    let angle = e.sector_angle();
    if e.radius > 0.0 {
        let tail = if angle < 360.0 {
            format!(" \u{b7} {}\u{b0}", angle.round() as i64)
        } else {
            String::new()
        };
        (format!("r {}{tail}", metres(e.radius)), "")
    } else {
        (String::new(), "")
    }
}

/// The corners to frame an entity on the map (`pointsOf`).
pub fn points_of(e: &Entity) -> Vec<[f64; 2]> {
    if e.is_gate() {
        return vec![[e.lng, e.lat], [e.lng2, e.lat2]];
    }
    let reach = e.radius.max(20.0);
    let d_lat = reach / METRES_PER_DEGREE;
    let d_lng = reach / (METRES_PER_DEGREE * (e.lat.to_radians().cos()).max(0.01));
    vec![[e.lng - d_lng, e.lat - d_lat], [e.lng + d_lng, e.lat + d_lat]]
}

/// A log line's text (`logLine`).
pub fn log_text(e: &Entity, entry: &Entry) -> String {
    if e.is_gate() {
        let what = if entry.ok {
            "Through"
        } else if entry.reason == "above" {
            "Above"
        } else {
            "Below"
        };
        format!(
            "{what}, {} up, {} off centre",
            metres(entry.alt),
            metres(entry.offset.abs())
        )
    } else {
        format!("Entered, {} up", metres(entry.alt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn gate() -> Entity {
        let mut e = make_entity("gate", [11.0, 48.0], Some([11.0002, 48.0]), &[]).unwrap();
        e.base = 2.0;
        e.height = 5.0;
        e
    }

    fn zone(kind: &str, radius: f64, angle: f64, bearing: f64) -> Entity {
        let mut e = make_entity(kind, [11.0, 48.0], None, &[]).unwrap();
        e.radius = radius;
        e.angle = angle;
        e.bearing = bearing;
        e
    }

    fn s(lng: f64, lat: f64, alt: f64) -> Sample {
        Sample { lng, lat, alt }
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    }

    #[test]
    fn a_step_across_the_gate_crosses_it_where_the_web_says() {
        let c = gate_crossing(&gate(), s(11.0001, 47.9999, 3.0), s(11.0001, 48.0001, 4.0)).unwrap();
        near(c.u, 0.5);
        near(c.t, 0.5);
        near(c.offset, 0.0);
        near(c.alt, 3.5);
        assert_eq!(
            gate_crossing(&gate(), s(11.0003, 47.9999, 3.0), s(11.0003, 48.0001, 4.0)),
            None
        );
    }

    #[test]
    fn the_opening_lets_through_what_flies_between_its_base_and_its_top() {
        let g = gate();
        let at = |alt| Crossing {
            u: 0.5,
            t: 0.5,
            offset: 0.0,
            alt,
        };
        assert_eq!(gate_verdict(&g, &at(9.0)), (false, "above"));
        assert_eq!(gate_verdict(&g, &at(1.0)), (false, "below"));
        assert_eq!(gate_verdict(&g, &at(3.0)), (true, ""));
        assert_eq!(gate_verdict(&g, &at(2.0)), (true, ""));
        assert_eq!(gate_verdict(&g, &at(7.0)), (true, ""));
    }

    #[test]
    fn a_round_zone_is_touched_by_the_nearest_point_of_the_step() {
        let z = zone("radar", 100.0, 360.0, 0.0);
        let c = zone_contact(&z, s(11.001, 48.0, 10.0), s(11.0, 48.0004, 20.0));
        assert!(c.touched && c.inside);
        assert!((c.dist - 38.21947339039461).abs() < 1e-6);
        assert!((c.alt - 17.367276893546638).abs() < 1e-6);
        let far = zone_contact(&z, s(11.01, 48.0, 10.0), s(11.011, 48.0004, 20.0));
        assert!(!far.touched && !far.inside);
        assert!((far.dist - 744.8727837406088).abs() < 1e-6);
        assert_eq!(far.alt, 10.0);
    }

    #[test]
    fn a_sector_is_walked_in_short_pieces() {
        let z = zone("radar", 100.0, 90.0, 90.0);
        let c = zone_contact(&z, s(11.002, 48.0, 10.0), s(11.0, 48.0, 20.0));
        assert!(c.touched && c.inside);
        assert_eq!(c.dist, 0.0);
        assert!((c.alt - 13.333333333333332).abs() < 1e-9);
        let off = zone_contact(&z, s(11.0, 48.002, 10.0), s(11.0, 48.0006, 20.0));
        assert!(!off.touched && !off.inside);
        assert!((off.dist - 66.79169447580847).abs() < 1e-6);
        assert_eq!(off.alt, 20.0);
    }

    #[test]
    fn a_sector_covers_half_its_angle_either_side_and_always_its_centre() {
        let z = zone("radar", 100.0, 90.0, 90.0);
        assert!(in_sector(&z, [50.0, 0.0]));
        assert!(!in_sector(&z, [0.0, 50.0]));
        assert!(in_sector(&z, [0.0, 0.0]));
        assert!(!in_sector(&z, [200.0, 0.0]));
        let mut tree = zone("tree", 3.0, 30.0, 0.0);
        assert_eq!(tree.sector_angle(), 360.0);
        tree.kind = kind_of("radar").unwrap();
        tree.angle = 0.0;
        assert_eq!(tree.sector_angle(), 1.0);
    }

    #[test]
    fn a_sector_ring_starts_at_the_centre_and_runs_along_the_arc() {
        let mut z = zone("radar", 100.0, 90.0, 0.0);
        z.radius = 100.0;
        let ring = sector_ring(&z);
        assert_eq!(ring.len(), 20);
        assert_eq!(ring[0], [11.0, 48.0]);
        near(ring[1][0], 10.999050701278632);
        near(ring[1][1], 48.000635204829045);
        near(ring[10][0], 11.0);
        near(ring[10][1], 48.00089831528412);
        near(ring[19][0], 11.000949298721368);
    }

    #[test]
    fn degrees_and_pasted_coordinates_read_as_the_web_reads_them() {
        assert_eq!(parse_degrees("48,5", 90.0), Some(48.5));
        assert_eq!(parse_degrees("", 90.0), None);
        assert_eq!(parse_degrees("91", 90.0), None);
        assert_eq!(parse_degrees(" -3.2 ", 90.0), Some(-3.2));
        assert_eq!(parse_coordinate_pair("48.1234, 11.5678"), Some((48.1234, 11.5678)));
        assert_eq!(parse_coordinate_pair("48,5; 11,5"), Some((48.5, 11.5)));
        assert_eq!(parse_coordinate_pair("48.1 11.5"), Some((48.1, 11.5)));
        assert_eq!(parse_coordinate_pair("1,2,3"), None);
        assert_eq!(parse_coordinate_pair("a b"), None);
        assert_eq!(parse_coordinate_pair(""), None);
        assert_eq!(format_degrees(48.1234), "48.1234");
        assert_eq!(format_degrees(11.0), "11.0");
    }

    #[test]
    fn distances_and_bearings_are_flat_metres() {
        near(distance_m([11.0, 48.0], [11.001, 48.0]), 74.4872783740212);
        near(bearing_deg([11.0, 48.0], [11.001, 48.001]), 33.78769180575489);
    }

    #[test]
    fn a_new_entity_has_its_kinds_defaults_and_the_next_number() {
        let r = make_entity("radar", [11.0, 48.0], None, &[]).unwrap();
        assert_eq!(
            (r.name.as_str(), r.color.as_str(), r.radius, r.alert, r.angle, r.bearing),
            ("Radar 1", "#5B8DEF", 500.0, true, 360.0, 0.0)
        );
        let mut old = make_entity("tree", [11.0, 48.0], None, &[]).unwrap();
        old.name = "Tree 3".into();
        let t = make_entity("tree", [11.0, 48.0], None, &[old]).unwrap();
        assert_eq!(
            (t.name.as_str(), t.radius, t.height, t.alert),
            ("Tree 4", 3.0, 12.0, true)
        );
        let g = make_entity("gate", [11.0, 48.0], Some([11.1, 48.0]), &[]).unwrap();
        assert_eq!((g.name.as_str(), g.base, g.height, g.lng2), ("Gate 1", 0.0, 5.0, 11.1));
        assert!(make_entity("gate", [11.0, 48.0], None, &[]).is_none());
        assert!(make_entity("nope", [11.0, 48.0], None, &[]).is_none());
        assert!(make_entity("tree", [200.0, 48.0], None, &[]).is_none());
    }

    #[test]
    fn a_saved_entity_is_clamped_and_cleaned_as_the_web_does() {
        let e = normalize_entity(
            &json!({"kind": "radar", "lng": "11", "lat": 48, "radius": -5, "angle": 1000,
            "bearing": -90, "name": "  A   B ", "color": "red", "id": "x y"}),
        )
        .unwrap();
        assert_eq!(
            (e.name.as_str(), e.color.as_str(), e.radius, e.angle, e.bearing, e.alert),
            ("A B", "#5B8DEF", 0.0, 360.0, 270.0, true)
        );
        assert_ne!(e.id, "x y");
        let t = normalize_entity(&json!({"kind": "tree", "lng": 11, "lat": 48, "height": 5000})).unwrap();
        assert_eq!((t.name.as_str(), t.radius, t.height), ("Tree", 3.0, 1000.0));
        let g = normalize_entity(
            &json!({"kind": "gate", "lng": 11, "lat": 48, "lng2": 12, "lat2": 48, "base": -5000, "height": -1}),
        )
        .unwrap();
        assert_eq!((g.base, g.height, g.lng2), (-1000.0, 0.0, 12.0));
        assert!(normalize_entity(&json!({"kind": "nope", "lng": 1, "lat": 1})).is_none());
        assert!(normalize_entity(&json!({"kind": "gate", "lng": 11, "lat": 48})).is_none());
        assert!(normalize_entity(&json!("x")).is_none());
    }

    #[test]
    fn the_saved_list_keeps_five_hundred_and_every_id_once() {
        let many: Vec<Value> = (0..520)
            .map(|i| json!({"kind": "flag", "lng": 11, "lat": 48, "id": format!("a{}", i % 3)}))
            .collect();
        let list = normalize_list(&Value::Array(many));
        assert_eq!(list.len(), MAX_ENTITIES);
        let mut ids: Vec<_> = list.iter().map(|e| e.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), MAX_ENTITIES);
        assert!(normalize_list(&json!({})).is_empty());
    }

    #[test]
    fn a_sample_needs_a_link_a_fix_and_a_reason_to_check() {
        let armed = json!({"connected": true, "armed": true, "position": [11, 48], "altitude_agl": 3});
        assert_eq!(sample_of(&armed, true), Some(s(11.0, 48.0, 3.0)));
        let idle = json!({"connected": true, "armed": false, "position": [11, 48]});
        assert_eq!(sample_of(&idle, true), None);
        assert_eq!(sample_of(&idle, false), Some(s(11.0, 48.0, 0.0)));
        assert_eq!(sample_of(&json!({"connected": true, "position": [0, 0]}), false), None);
        assert_eq!(
            sample_of(&json!({"connected": false, "armed": true, "position": [11, 48]}), true),
            None
        );
    }

    #[test]
    fn flying_through_a_gate_is_counted_once_and_told() {
        let gates = [gate()];
        let mut results = HashMap::new();
        let ev = process_step(
            &gates,
            &mut results,
            s(11.0001, 47.9999, 3.0),
            s(11.0001, 48.0001, 4.0),
            1.0,
        );
        assert_eq!(
            ev,
            vec![Event {
                entity: gates[0].id.clone(),
                level: "info",
                text: "Gate 1: flown through".into()
            }]
        );
        let r = &results[&gates[0].id];
        assert_eq!((r.passes, r.misses, r.log.len()), (1, 0, 1));
        let ev = process_step(
            &gates,
            &mut results,
            s(11.0001, 48.0001, 9.0),
            s(11.0001, 47.9999, 9.0),
            2.0,
        );
        assert_eq!(ev[0].text, "Gate 1: crossed above the opening");
        assert_eq!(ev[0].level, "warning");
        assert_eq!(status_color(&gates[0], results.get(&gates[0].id)), PASS_COLOR);
    }

    #[test]
    fn a_zone_counts_an_entry_once_until_the_aircraft_leaves() {
        let zones = [zone("radar", 100.0, 360.0, 0.0)];
        let mut results = HashMap::new();
        let inside = s(11.0, 48.0, 5.0);
        let ev = process_step(&zones, &mut results, s(11.01, 48.0, 5.0), inside, 1.0);
        assert_eq!(ev[0].text, "Radar 1: aircraft entered the area");
        assert!(results[&zones[0].id].inside);
        assert!(process_step(&zones, &mut results, inside, s(11.0001, 48.0, 5.0), 2.0).is_empty());
        assert_eq!(results[&zones[0].id].entries, 1);
        process_step(&zones, &mut results, s(11.0001, 48.0, 5.0), s(11.01, 48.0, 5.0), 3.0);
        assert!(!results[&zones[0].id].inside);
        assert_eq!(status_color(&zones[0], results.get(&zones[0].id)), "#5B8DEF");
    }

    #[test]
    fn a_tree_is_hit_only_below_its_top() {
        let mut tree = zone("tree", 3.0, 360.0, 0.0);
        tree.height = 12.0;
        let trees = [tree];
        let mut results = HashMap::new();
        let ev = process_step(&trees, &mut results, s(11.0001, 48.0, 30.0), s(11.0, 48.0, 30.0), 1.0);
        assert!(ev.is_empty(), "above the crown is no hit");
        let ev = process_step(&trees, &mut results, s(11.0001, 48.0, 5.0), s(11.0, 48.0, 5.0), 2.0);
        assert_eq!(ev[0].text, "Tree 1: aircraft is inside the crown, below the top");
        assert_eq!(results[&trees[0].id].hits, 1);
        assert_eq!(status_color(&trees[0], results.get(&trees[0].id)), HIT_COLOR);
    }

    #[test]
    fn the_list_rows_read_as_the_web_rows_do() {
        let g = gate();
        assert_eq!(row_detail(&g, None).0, "14.9 m");
        let r = Results {
            passes: 2,
            ..Default::default()
        };
        assert_eq!(row_detail(&g, Some(&r)), ("2\u{d7} through".into(), "good"));
        let mut t = zone("tree", 3.0, 360.0, 0.0);
        t.height = 12.0;
        assert_eq!(row_detail(&t, None).0, "12 m \u{b7} r 3 m");
        let z = zone("radar", 500.0, 90.0, 0.0);
        assert_eq!(row_detail(&z, None).0, "r 500 m \u{b7} 90\u{b0}");
        assert_eq!(row_detail(&zone("flag", 0.0, 360.0, 0.0), None).0, "");
        assert_eq!(metres(3.15159), "3.2 m");
        assert_eq!(metres(10.0), "10 m");
        assert_eq!(
            log_text(
                &g,
                &Entry {
                    t: 0.0,
                    ok: false,
                    reason: "above",
                    alt: 9.04,
                    offset: -1.26
                }
            ),
            "Above, 9 m up, 1.3 m off centre"
        );
        assert_eq!(
            log_text(
                &z,
                &Entry {
                    t: 0.0,
                    ok: true,
                    reason: "",
                    alt: 12.5,
                    offset: 0.0
                }
            ),
            "Entered, 12.5 m up"
        );
    }

    #[test]
    fn an_entity_survives_its_own_json() {
        for k in KINDS {
            let e = if k.gate {
                make_entity(k.id, [11.0, 48.0], Some([11.1, 48.0]), &[]).unwrap()
            } else {
                make_entity(k.id, [11.0, 48.0], None, &[]).unwrap()
            };
            assert_eq!(normalize_entity(&e.to_json()).unwrap(), e, "{}", k.id);
        }
    }

    #[test]
    fn the_frame_of_a_point_is_at_least_twenty_metres_wide() {
        let p = points_of(&zone("flag", 0.0, 360.0, 0.0));
        assert!(distance_m(p[0], p[1]) > 40.0);
    }
}
