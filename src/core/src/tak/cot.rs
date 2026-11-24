//! Cursor on Target: the event ATAK, CivTAK, WinTAK, iTAK and a TAK server
//! exchange, as plain XML. Parsing is bounded and never panics on what a
//! stranger sends; building makes the handful of events Corvus itself puts on
//! the network (its own position, a marker, a chat line, a delete).

use serde_json::{json, Value};

use super::xml::{self, Node};

/// CoT's "unknown" for height and error fields.
pub const UNKNOWN: f64 = 9_999_999.0;
/// How far ahead a stale time is believed, seconds.
pub const MAX_STALE_AHEAD_S: f64 = 86_400.0;
pub const DEFAULT_STALE_S: f64 = 300.0;

const UID_MAX: usize = 128;
const TYPE_MAX: usize = 64;
const TEXT_MAX: usize = 512;
const NAME_MAX: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub lat: f64,
    pub lon: f64,
    pub hae: f64,
    pub ce: f64,
    pub le: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub uid: String,
    pub kind: String,
    pub how: String,
    pub time: f64,
    pub start: f64,
    pub stale: f64,
    pub point: Point,
    /// The children of `<detail>`, in order.
    pub detail: Vec<Node>,
}

fn clip(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control() || *c == '\n').take(max).collect()
}

fn number(s: Option<&str>) -> Option<f64> {
    s.and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite())
}

// ---- time ---------------------------------------------------------------------

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Seconds since the epoch of `2026-11-17T10:20:30.123Z`; `None` when it is
/// not a time. An offset such as `+01:00` is honoured.
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', 't'])?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    if d.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) || !(1970..=9999).contains(&y) {
        return None;
    }
    let (clock, offset) = match rest.find(['Z', 'z', '+', '-']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "Z"),
    };
    let mut c = clock.split(':');
    let h: f64 = c.next()?.parse().ok()?;
    let mi: f64 = c.next()?.parse().ok()?;
    let sec: f64 = c.next()?.parse().ok()?;
    if c.next().is_some() || !(0.0..24.0).contains(&h) || !(0.0..60.0).contains(&mi) || !(0.0..61.0).contains(&sec) {
        return None;
    }
    let shift = match offset {
        "Z" | "z" => 0.0,
        o => {
            let sign = if o.starts_with('-') { -1.0 } else { 1.0 };
            let (oh, om) = o[1..].split_once(':')?;
            sign * (oh.parse::<f64>().ok()? * 3600.0 + om.parse::<f64>().ok()? * 60.0)
        }
    };
    let t = days_from_civil(y, m, day) as f64 * 86_400.0 + h * 3600.0 + mi * 60.0 + sec - shift;
    t.is_finite().then_some(t)
}

/// `2026-11-17T10:20:30.123Z`.
pub fn format_time(t: f64) -> String {
    let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
    let ms_total = (t * 1000.0).round() as i64;
    let (secs, ms) = (ms_total.div_euclid(1000), ms_total.rem_euclid(1000));
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

// ---- types --------------------------------------------------------------------

/// What an event's type says, in words the interface and an AI client can use.
#[derive(Debug, Clone, PartialEq)]
pub struct Kind {
    /// `unit`, `point`, `route`, `drawing`, `chat`, `alert`, `control` or `other`.
    pub class: &'static str,
    /// `friend`, `hostile`, `neutral`, `unknown` or `other` (atoms only).
    pub affiliation: &'static str,
    /// `air`, `ground`, `sea`, `subsurface`, `space`, `special` or `other` (atoms only).
    pub dimension: &'static str,
}

pub fn classify(kind: &str) -> Kind {
    let mut parts = kind.split('-');
    let (class, mut affiliation, mut dimension) = match (parts.next(), parts.next(), parts.next()) {
        (Some("a"), aff, dim) => {
            let affiliation = match aff {
                Some("f" | "a") => "friend",
                Some("h" | "s" | "j" | "k") => "hostile",
                Some("n") => "neutral",
                Some("u" | "p") => "unknown",
                _ => "other",
            };
            let dimension = match dim {
                Some("A") => "air",
                Some("G") => "ground",
                Some("S") => "sea",
                Some("U") => "subsurface",
                Some("P") => "space",
                Some("F") => "special",
                _ => "other",
            };
            ("unit", affiliation, dimension)
        }
        (Some("b"), Some("m"), Some("p")) => ("point", "other", "other"),
        (Some("b"), Some("m"), Some("r")) => ("route", "other", "other"),
        (Some("b"), Some("t"), _) => ("chat", "other", "other"),
        (Some("b"), Some("a"), _) => ("alert", "other", "other"),
        (Some("u"), Some("d"), _) => ("drawing", "other", "other"),
        (Some("t"), ..) => ("control", "other", "other"),
        _ => ("other", "other", "other"),
    };
    if class != "unit" {
        affiliation = "other";
        dimension = "other";
    }
    Kind {
        class,
        affiliation,
        dimension,
    }
}

/// Whether `kind` is a type Corvus lets an operator or an AI client send as a
/// marker: an atom (`a-f-G-U-C`) or a spot point (`b-m-p-...`).
pub fn marker_type_ok(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= TYPE_MAX
        && kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && (kind.starts_with("a-") || kind.starts_with("b-m-p"))
}

// ---- reading ------------------------------------------------------------------

impl Event {
    /// The event in `text`, with `now` standing in for a time it does not have.
    pub fn parse(text: &str, now: f64) -> Result<Event, String> {
        let root = xml::parse(text)?;
        if root.name != "event" {
            return Err(format!("<{}> is not an event", root.name));
        }
        let uid = clip(root.get("uid").unwrap_or(""), UID_MAX);
        let kind = clip(root.get("type").unwrap_or(""), TYPE_MAX);
        if uid.is_empty() || kind.is_empty() {
            return Err("an event needs a uid and a type".into());
        }
        let p = root.find("point").ok_or("an event needs a point")?;
        let lat = number(p.get("lat")).ok_or("the point has no latitude")?;
        let lon = number(p.get("lon")).ok_or("the point has no longitude")?;
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return Err("the point is off the globe".into());
        }
        let time = root.get("time").and_then(parse_time).unwrap_or(now);
        let start = root.get("start").and_then(parse_time).unwrap_or(time);
        let stale = root
            .get("stale")
            .and_then(parse_time)
            .unwrap_or(now + DEFAULT_STALE_S)
            .min(now + MAX_STALE_AHEAD_S);
        Ok(Event {
            uid,
            kind,
            how: clip(root.get("how").unwrap_or(""), 16),
            time,
            start,
            stale,
            point: Point {
                lat,
                lon,
                hae: number(p.get("hae")).unwrap_or(UNKNOWN),
                ce: number(p.get("ce")).unwrap_or(UNKNOWN),
                le: number(p.get("le")).unwrap_or(UNKNOWN),
            },
            detail: root.find("detail").map(|d| d.children.clone()).unwrap_or_default(),
        })
    }

    fn part(&self, name: &str) -> Option<&Node> {
        self.detail.iter().find(|n| n.name == name)
    }

    pub fn callsign(&self) -> Option<String> {
        self.part("contact")
            .and_then(|c| c.get("callsign"))
            .map(|s| clip(s, NAME_MAX))
            .filter(|s| !s.is_empty())
    }

    pub fn endpoint(&self) -> Option<String> {
        self.part("contact")
            .and_then(|c| c.get("endpoint"))
            .map(|s| clip(s, NAME_MAX))
            .filter(|s| !s.is_empty())
    }

    /// `(team colour, role)` of `<__group>`.
    pub fn group(&self) -> Option<(String, String)> {
        let g = self.part("__group")?;
        Some((
            clip(g.get("name").unwrap_or(""), NAME_MAX),
            clip(g.get("role").unwrap_or(""), NAME_MAX),
        ))
    }

    /// `(course in degrees, speed in m/s)` of `<track>`.
    pub fn track(&self) -> Option<(f64, f64)> {
        let t = self.part("track")?;
        let course = number(t.get("course")).unwrap_or(0.0).rem_euclid(360.0);
        let speed = number(t.get("speed")).unwrap_or(0.0).max(0.0);
        Some((course, speed))
    }

    pub fn battery(&self) -> Option<f64> {
        number(self.part("status")?.get("battery")).filter(|b| (0.0..=100.0).contains(b))
    }

    pub fn remarks(&self) -> String {
        self.part("remarks")
            .map(|r| clip(&r.text, TEXT_MAX))
            .unwrap_or_default()
    }

    /// `#AARRGGBB` of `<color argb>` (a signed integer in CoT), when set.
    pub fn color(&self) -> Option<String> {
        let argb = number(self.part("color")?.get("argb"))? as i64 as u32;
        Some(format!("#{:06X}", argb & 0x00FF_FFFF))
    }

    pub fn classify(&self) -> Kind {
        classify(&self.kind)
    }

    /// Whether the event is a delete order (`t-x-d-d`) and for which uids.
    pub fn deletes(&self) -> Vec<String> {
        if !self.kind.starts_with("t-x-d-d") {
            return Vec::new();
        }
        self.detail
            .iter()
            .filter(|n| n.name == "link")
            .filter_map(|n| n.get("uid"))
            .map(|u| clip(u, UID_MAX))
            .collect()
    }

    /// The chat line of a GeoChat event: `(room, sender callsign, text)`.
    pub fn chat(&self) -> Option<(String, String, String)> {
        if !self.kind.starts_with("b-t-f") {
            return None;
        }
        let chat = self.part("__chat");
        let room = chat
            .and_then(|c| c.get("chatroom").or(c.get("id")))
            .unwrap_or("All Chat Rooms");
        let from = chat
            .and_then(|c| c.get("senderCallsign"))
            .map(str::to_string)
            .or_else(|| self.callsign())
            .unwrap_or_default();
        let text = self.remarks();
        (!text.is_empty()).then(|| (clip(room, NAME_MAX), clip(&from, NAME_MAX), text))
    }

    /// The event as the interface and an AI client see a contact.
    pub fn to_json(&self, now: f64) -> Value {
        let k = self.classify();
        let known = |v: f64| (v.is_finite() && v < UNKNOWN).then_some(v);
        let track = self.track();
        let group = self.group();
        json!({
            "uid": self.uid,
            "type": self.kind,
            "how": self.how,
            "class": k.class,
            "affiliation": k.affiliation,
            "dimension": k.dimension,
            "callsign": self.callsign().unwrap_or_else(|| self.uid.clone()),
            "lat": self.point.lat,
            "lon": self.point.lon,
            "hae": known(self.point.hae),
            "ce": known(self.point.ce),
            "course": track.map(|t| t.0),
            "speed": track.map(|t| t.1),
            "battery": self.battery(),
            "team": group.as_ref().map(|g| g.0.clone()).filter(|s| !s.is_empty()),
            "role": group.as_ref().map(|g| g.1.clone()).filter(|s| !s.is_empty()),
            "color": self.color(),
            "remarks": self.remarks(),
            "time": format_time(self.time),
            "stale_in_s": (self.stale - now).round(),
            "stale": self.stale <= now,
        })
    }

    pub fn to_xml(&self) -> String {
        let mut e = Node::new("event")
            .attr("version", "2.0")
            .attr("uid", self.uid.clone())
            .attr("type", self.kind.clone())
            .attr("how", self.how.clone())
            .attr("time", format_time(self.time))
            .attr("start", format_time(self.start))
            .attr("stale", format_time(self.stale))
            .child(
                Node::new("point")
                    .attr("lat", format!("{:.7}", self.point.lat))
                    .attr("lon", format!("{:.7}", self.point.lon))
                    .attr("hae", format!("{:.1}", self.point.hae))
                    .attr("ce", format!("{:.1}", self.point.ce))
                    .attr("le", format!("{:.1}", self.point.le)),
            );
        let mut detail = Node::new("detail");
        detail.children = self.detail.clone();
        e.children.push(detail);
        e.to_xml()
    }
}

// ---- building -----------------------------------------------------------------

/// Eight random hex digits, for uids.
pub fn random_id() -> String {
    let mut b = [0u8; 4];
    if getrandom::fill(&mut b).is_err() {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        b = t.to_be_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What Corvus says about itself in `<takv>`.
fn takv() -> Node {
    Node::new("takv")
        .attr("device", "Corvus GCS")
        .attr("platform", "Corvus GCS")
        .attr("os", std::env::consts::OS)
        .attr("version", crate::version::version())
}

pub struct Own<'a> {
    pub uid: &'a str,
    pub callsign: &'a str,
    pub team: &'a str,
    pub role: &'a str,
    pub lat: f64,
    pub lon: f64,
    pub hae: Option<f64>,
    pub course: Option<f64>,
    pub speed: Option<f64>,
    pub battery: Option<f64>,
    pub stale_s: f64,
}

/// The situational awareness event of the aircraft: a friendly unmanned
/// aircraft (`a-f-A-M-F-Q`), which ATAK draws as a drone.
pub fn position(o: &Own, now: f64) -> Event {
    let mut detail = vec![
        takv(),
        Node::new("contact")
            .attr("callsign", clip(o.callsign, NAME_MAX))
            .attr("endpoint", "*:-1:stcp"),
        Node::new("uid").attr("Droid", clip(o.callsign, NAME_MAX)),
        Node::new("__group")
            .attr("name", clip(o.team, NAME_MAX))
            .attr("role", clip(o.role, NAME_MAX)),
        Node::new("track")
            .attr("course", format!("{:.1}", o.course.unwrap_or(0.0).rem_euclid(360.0)))
            .attr("speed", format!("{:.2}", o.speed.unwrap_or(0.0).max(0.0))),
    ];
    if let Some(b) = o.battery.filter(|b| (0.0..=100.0).contains(b)) {
        detail.push(Node::new("status").attr("battery", format!("{b:.0}")));
    }
    Event {
        uid: o.uid.to_string(),
        kind: "a-f-A-M-F-Q".into(),
        how: "m-g".into(),
        time: now,
        start: now,
        stale: now + o.stale_s,
        point: Point {
            lat: o.lat,
            lon: o.lon,
            hae: o.hae.unwrap_or(UNKNOWN),
            ce: UNKNOWN,
            le: UNKNOWN,
        },
        detail,
    }
}

/// A marker placed by hand (or by an AI client): a spot point by default.
#[allow(clippy::too_many_arguments)]
pub fn marker(
    uid: &str,
    kind: &str,
    callsign: &str,
    lat: f64,
    lon: f64,
    hae: Option<f64>,
    remarks: &str,
    stale_s: f64,
    now: f64,
) -> Event {
    let mut detail = vec![Node::new("contact").attr("callsign", clip(callsign, NAME_MAX))];
    if !remarks.is_empty() {
        detail.push(Node::new("remarks").with_text(&clip(remarks, TEXT_MAX)));
    }
    Event {
        uid: uid.to_string(),
        kind: kind.to_string(),
        how: "h-g-i-g-o".into(),
        time: now,
        start: now,
        stale: now + stale_s,
        point: Point {
            lat,
            lon,
            hae: hae.unwrap_or(UNKNOWN),
            ce: UNKNOWN,
            le: UNKNOWN,
        },
        detail,
    }
}

/// A GeoChat line to everybody (`All Chat Rooms`) or to one room.
pub fn chat(sender_uid: &str, sender: &str, room: &str, text: &str, lat: f64, lon: f64, now: f64) -> Event {
    let room = clip(room, NAME_MAX);
    let room = if room.is_empty() {
        "All Chat Rooms".to_string()
    } else {
        room
    };
    let message_id = random_id();
    let uid = format!("GeoChat.{sender_uid}.{room}.{message_id}");
    Event {
        uid,
        kind: "b-t-f".into(),
        how: "h-g-i-g-o".into(),
        time: now,
        start: now,
        stale: now + 60.0,
        point: Point {
            lat,
            lon,
            hae: UNKNOWN,
            ce: UNKNOWN,
            le: UNKNOWN,
        },
        detail: vec![
            Node::new("__chat")
                .attr("parent", "RootContactGroup")
                .attr("groupOwner", "false")
                .attr("messageId", message_id)
                .attr("chatroom", room.clone())
                .attr("id", room.clone())
                .attr("senderCallsign", clip(sender, NAME_MAX))
                .child(
                    Node::new("chatgrp")
                        .attr("uid0", sender_uid)
                        .attr("uid1", room.clone())
                        .attr("id", room.clone()),
                ),
            Node::new("link")
                .attr("uid", sender_uid)
                .attr("type", "a-f-A-M-F-Q")
                .attr("relation", "p-p"),
            Node::new("remarks")
                .attr("source", format!("Corvus.{sender_uid}"))
                .attr("to", room)
                .attr("time", format_time(now))
                .with_text(&clip(text, TEXT_MAX)),
        ],
    }
}

/// A forced delete of `target`, which every TAK client honours.
pub fn delete(target: &str, sender_uid: &str, now: f64) -> Event {
    Event {
        uid: format!("{sender_uid}.del.{}", random_id()),
        kind: "t-x-d-d".into(),
        how: "m-g".into(),
        time: now,
        start: now,
        stale: now + 60.0,
        point: Point {
            lat: 0.0,
            lon: 0.0,
            hae: UNKNOWN,
            ce: UNKNOWN,
            le: UNKNOWN,
        },
        detail: vec![
            Node::new("link")
                .attr("uid", target)
                .attr("type", "none")
                .attr("relation", "none"),
            Node::new("__forcedelete"),
        ],
    }
}

// ---- framing ------------------------------------------------------------------

const END: &[u8] = b"</event>";

/// Cuts a byte stream (or a datagram) into whole events: TAK sends them one
/// after another with nothing between.
#[derive(Default)]
pub struct Framer {
    buf: Vec<u8>,
}

impl Framer {
    pub fn push(&mut self, data: &[u8]) -> Framed {
        let mut out = Framed::default();
        self.buf.extend_from_slice(data);
        while let Some(at) = self.buf.windows(END.len()).position(|w| w == END) {
            let end = at + END.len();
            let chunk: Vec<u8> = self.buf.drain(..end).collect();
            if chunk.len() > xml::MAX_XML_BYTES {
                out.dropped += 1;
                continue;
            }
            let text = String::from_utf8_lossy(&chunk);
            match text.find("<event") {
                Some(start) => out.events.push(text[start..].to_string()),
                None => out.dropped += 1,
            }
        }
        if self.buf.len() > xml::MAX_XML_BYTES {
            self.buf.clear();
            out.dropped += 1;
        }
        out
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

#[derive(Default, Debug)]
pub struct Framed {
    pub events: Vec<String>,
    /// Pieces that were not an event or were too large.
    pub dropped: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SA: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<event version="2.0" uid="ANDROID-abc" type="a-f-G-U-C" how="m-g" time="2026-11-17T10:00:00.000Z"
 start="2026-11-17T10:00:00.000Z" stale="2026-11-17T10:01:30.000Z">
<point lat="48.0795" lon="11.6385" hae="540.5" ce="9.5" le="9999999.0"/>
<detail><takv device="Pixel" platform="ATAK-CIV" os="34" version="5.2.0"/>
<contact callsign="ALPHA-1" endpoint="*:-1:stcp"/><__group name="Cyan" role="Team Lead"/>
<status battery="87"/><track course="123.4" speed="1.5"/><remarks>at the gate</remarks></detail></event>"#;

    #[test]
    fn reads_an_atak_position_report() {
        let now = parse_time("2026-11-17T10:00:10Z").unwrap();
        let e = Event::parse(SA, now).unwrap();
        assert_eq!(e.uid, "ANDROID-abc");
        assert_eq!(e.callsign().as_deref(), Some("ALPHA-1"));
        assert_eq!(e.group(), Some(("Cyan".into(), "Team Lead".into())));
        assert_eq!(e.track(), Some((123.4, 1.5)));
        assert_eq!(e.battery(), Some(87.0));
        assert_eq!(e.remarks(), "at the gate");
        assert!((e.point.lat - 48.0795).abs() < 1e-9 && e.point.hae == 540.5 && e.point.le == UNKNOWN);
        let j = e.to_json(now);
        assert_eq!(j["affiliation"], "friend");
        assert_eq!(j["dimension"], "ground");
        assert_eq!(j["class"], "unit");
        assert_eq!(j["le"], Value::Null);
        assert_eq!(j["stale"], false);
        assert_eq!(j["stale_in_s"], 80.0);
    }

    #[test]
    fn time_round_trips_and_honours_offsets() {
        let t = parse_time("2026-11-17T10:20:30.123Z").unwrap();
        assert_eq!(format_time(t), "2026-11-17T10:20:30.123Z");
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(parse_time("2000-02-29T12:00:00Z"), Some(951_825_600.0));
        assert_eq!(
            parse_time("2026-11-17T11:00:00+01:00"),
            parse_time("2026-11-17T10:00:00Z")
        );
        for bad in [
            "",
            "yesterday",
            "2026-13-01T00:00:00Z",
            "2026-11-17T25:00:00Z",
            "2026-11-17",
        ] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn refuses_events_that_cannot_be_placed() {
        let now = 0.0;
        for bad in [
            r#"<event uid="a" type="a-f-G"><point lat="91" lon="0"/></event>"#,
            r#"<event uid="a" type="a-f-G"><point lat="NaN" lon="0"/></event>"#,
            r#"<event uid="a" type="a-f-G"><point lat="0" lon="inf"/></event>"#,
            r#"<event uid="a" type="a-f-G"/>"#,
            r#"<event type="a-f-G"><point lat="0" lon="0"/></event>"#,
            r#"<event uid="a"><point lat="0" lon="0"/></event>"#,
            r#"<other uid="a" type="x"><point lat="0" lon="0"/></other>"#,
        ] {
            assert!(Event::parse(bad, now).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_missing_or_far_stale_time_is_bounded() {
        let now = 1_000_000.0;
        let e = Event::parse(
            r#"<event uid="a" type="a-f-G" stale="2999-01-01T00:00:00Z"><point lat="1" lon="2"/></event>"#,
            now,
        )
        .unwrap();
        assert_eq!(e.stale, now + MAX_STALE_AHEAD_S);
        let e = Event::parse(r#"<event uid="a" type="a-f-G"><point lat="1" lon="2"/></event>"#, now).unwrap();
        assert_eq!(e.stale, now + DEFAULT_STALE_S);
    }

    #[test]
    fn what_corvus_builds_it_can_read_back() {
        let now = 1_800_000_000.0;
        let own = position(
            &Own {
                uid: "CORVUS-1",
                callsign: "Raven <1>",
                team: "Cyan",
                role: "Team Member",
                lat: 48.1,
                lon: 11.5,
                hae: Some(560.0),
                course: Some(370.0),
                speed: Some(12.0),
                battery: Some(76.4),
                stale_s: 30.0,
            },
            now,
        );
        let back = Event::parse(&own.to_xml(), now).unwrap();
        assert_eq!(back.kind, "a-f-A-M-F-Q");
        assert_eq!(back.callsign().as_deref(), Some("Raven <1>"));
        assert_eq!(back.track(), Some((10.0, 12.0)));
        assert_eq!(back.battery(), Some(76.0));
        assert_eq!(back.stale, now + 30.0);
        assert_eq!(back.classify().dimension, "air");

        let m = marker(
            "M1",
            "b-m-p-s-p-i",
            "Rally",
            48.2,
            11.6,
            None,
            "meet here & wait",
            600.0,
            now,
        );
        let back = Event::parse(&m.to_xml(), now).unwrap();
        assert_eq!(back.remarks(), "meet here & wait");
        assert_eq!(back.classify().class, "point");
        assert_eq!(back.point.hae, UNKNOWN);
    }

    #[test]
    fn chat_and_delete_carry_what_atak_looks_for() {
        let now = 1_800_000_000.0;
        let c = chat("CORVUS-1", "Raven", "", "landing now", 48.0, 11.0, now);
        let back = Event::parse(&c.to_xml(), now).unwrap();
        assert_eq!(
            back.chat(),
            Some(("All Chat Rooms".into(), "Raven".into(), "landing now".into()))
        );
        assert!(back.uid.starts_with("GeoChat.CORVUS-1.All Chat Rooms."));
        let d = delete("M1", "CORVUS-1", now);
        assert_eq!(
            Event::parse(&d.to_xml(), now).unwrap().deletes(),
            vec!["M1".to_string()]
        );
        assert!(back.deletes().is_empty());
    }

    #[test]
    fn types_are_classified_for_the_map() {
        let k = |t: &str| {
            let c = classify(t);
            (c.class, c.affiliation, c.dimension)
        };
        assert_eq!(k("a-h-A-M-F"), ("unit", "hostile", "air"));
        assert_eq!(k("a-n-S"), ("unit", "neutral", "sea"));
        assert_eq!(k("a-u-G"), ("unit", "unknown", "ground"));
        assert_eq!(k("b-m-p-s-p-i"), ("point", "other", "other"));
        assert_eq!(k("b-t-f"), ("chat", "other", "other"));
        assert_eq!(k("t-x-d-d"), ("control", "other", "other"));
        assert_eq!(k("u-d-f"), ("drawing", "other", "other"));
        assert_eq!(k(""), ("other", "other", "other"));
        assert!(marker_type_ok("a-f-G-U-C") && marker_type_ok("b-m-p-s-p-i"));
        assert!(!marker_type_ok("t-x-d-d") && !marker_type_ok("a-f-G\"><x") && !marker_type_ok(""));
    }

    #[test]
    fn the_framer_splits_a_stream_and_survives_garbage() {
        let a = r#"<event uid="a" type="a-f-G"><point lat="1" lon="2"/></event>"#;
        let b = r#"<?xml version="1.0"?><event uid="b" type="a-f-G"><point lat="1" lon="2"/></event>"#;
        let mut f = Framer::default();
        let whole = format!("{a}{b}");
        let (head, tail) = whole.as_bytes().split_at(30);
        let first = f.push(head);
        assert!(first.events.is_empty());
        let second = f.push(tail);
        assert_eq!(second.events.len(), 2);
        assert!(second.events[1].starts_with("<event"));
        assert!(Event::parse(&second.events[1], 0.0).is_ok());
        let junk = f.push(b"\xbf\x01\xbf\xff\xfe not xml </event>");
        assert_eq!(junk.dropped, 1);
        let big = vec![b'x'; xml::MAX_XML_BYTES + 10];
        assert_eq!(f.push(&big).dropped, 1);
        assert_eq!(f.push(a.as_bytes()).events.len(), 1);
    }
}
