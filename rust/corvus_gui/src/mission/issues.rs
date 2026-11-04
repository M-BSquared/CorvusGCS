//! What the planner says about the plan: the warnings under the summary, a
//! row's one line, and the display units a number is read in
//! (`problems`, `describeItem`, `lengthText`, `speedText` of `mission.js`).

use crate::data::units::{js_number, js_round, js_to_fixed, Units};
use crate::mission::geometry::{
    clearances, interpolate_at, landing_approach, profile_marks, route_duration, route_length, stations, takeoff_climb,
    Ground, MarkKind, StationKind,
};
use crate::mission::plan::{Aircraft, Item, Kind, Plan, APPROACH_MIN_M, START_OFFSET_WARN_M};

/// The highest a plan may reach before the open category's ceiling is said.
pub const CEILING_HINT_M: f64 = 120.0;
/// Ground clearance under which a leg is called out.
pub const CLEARANCE_WARN_M: f64 = 15.0;
/// The steepest landing approach a fixed wing is expected to glide down.
pub const GLIDE_MAX_DEG: f64 = 5.0;
/// What a route that drives is timed at when no point pins a speed.
pub const DRIVE_SPEED_MS: f64 = 2.0;
pub const EMPTY_PLAN: &str = "The mission is empty.";

/// One warning: a short line and, on demand, the reason and the fix.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub text: String,
    pub info: String,
}

/// A length as the editor box shows it: untouched in metres, else rounded.
pub fn shown_length(units: &Units, si: f64) -> f64 {
    if units.length.id == "m" {
        si
    } else {
        js_to_fixed(si * units.length.per_si, 0).parse().unwrap_or(0.0)
    }
}

pub fn shown_speed(units: &Units, si: f64) -> f64 {
    if units.speed.id == "ms" {
        si
    } else {
        js_to_fixed(si * units.speed.per_si, 1).parse().unwrap_or(0.0)
    }
}

pub fn length_from_shown(units: &Units, v: f64) -> f64 {
    if units.length.id == "m" {
        v
    } else {
        v / units.length.per_si
    }
}

pub fn speed_from_shown(units: &Units, v: f64) -> f64 {
    if units.speed.id == "ms" {
        v
    } else {
        v / units.speed.per_si
    }
}

/// `formatAlt`: whole numbers bare, others to one decimal.
pub fn format_alt(v: f64) -> String {
    if v.fract() == 0.0 {
        js_number(v)
    } else {
        js_to_fixed(v, 1)
    }
}

pub fn length_text(units: &Units, m: f64) -> String {
    format!("{} {}", format_alt(shown_length(units, m)), units.length.symbol)
}

pub fn speed_text(units: &Units, ms: f64) -> String {
    format!("{} {}", format_alt(shown_speed(units, ms)), units.speed.symbol)
}

/// `formatDistance` of the summary: a dash for nothing.
pub fn distance_text(units: &Units, m: f64) -> String {
    if m > 0.0 {
        units.format_distance(m)
    } else {
        "—".into()
    }
}

pub fn duration_text(seconds: f64) -> String {
    if seconds <= 0.0 || seconds.is_nan() {
        return "—".into();
    }
    let total = js_round(seconds) as i64;
    let minutes = total / 60;
    if minutes > 0 {
        format!("{minutes}:{:02}", total % 60)
    } else {
        format!("{total} s")
    }
}

/// `formatDegrees`: whole degrees from 10, else one decimal.
pub fn degrees_text(v: f64) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let n = if v >= 10.0 {
        js_round(v)
    } else {
        js_round(v * 10.0) / 10.0
    };
    format!("{}°", js_number(n))
}

/// A row's one-line summary.
pub fn describe_item(plan: &Plan, item: &Item, units: &Units) -> String {
    if !item.kind.position() {
        return if plan.aircraft == Some(Aircraft::FixedWing) {
            "circles at start".into()
        } else {
            "to start".into()
        };
    }
    let count = |item: &Item| {
        if item.kind == Kind::LoiterTurns {
            format!("{}×", js_number(item.turns))
        } else {
            format!("{} s", js_round(item.seconds) as i64)
        }
    };
    let ring = |item: &Item| {
        format!(
            "{}{}",
            if item.direction < 0.0 { "↺" } else { "↻" },
            units.format_length(item.radius, 0, false, false)
        )
    };
    let hold = |item: &Item, parts: &mut Vec<String>| {
        if item.kind == Kind::Waypoint && item.hold > 0.0 {
            parts.push(format!("{} s", js_round(item.hold) as i64));
        }
    };
    if plan.drives && item.kind != Kind::Takeoff && item.kind != Kind::Land {
        let mut parts = Vec::new();
        if item.kind.is_orbit() {
            if plan.ring_flown(item) {
                parts.push(ring(item));
            }
            parts.push(count(item));
        }
        hold(item, &mut parts);
        if let Some(s) = item.speed {
            parts.push(speed_text(units, s));
        }
        return parts.join(" · ");
    }
    if item.kind == Kind::Land {
        return match item.approach_alt {
            Some(a) if plan.aircraft == Some(Aircraft::Multirotor) => {
                if a <= APPROACH_MIN_M {
                    "straight in".into()
                } else {
                    format!("down from {}", length_text(units, a))
                }
            }
            _ => "ground".into(),
        };
    }
    if item.kind == Kind::Takeoff {
        return format!("climb to {}", length_text(units, item.alt));
    }
    let mut parts = vec![length_text(units, item.alt)];
    if item.kind.is_orbit() {
        if plan.ring_flown(item) {
            parts.push(ring(item));
        }
        parts.push(count(item));
    }
    hold(item, &mut parts);
    if let Some(s) = item.speed {
        parts.push(speed_text(units, s));
    }
    parts.join(" · ")
}

/// The summary cells: (label, value, warn).
pub fn summary(plan: &Plan, units: &Units, ground: Option<&Ground>) -> Vec<(&'static str, String, bool)> {
    let points = stations(&plan.items, plan.home.as_ref());
    let length = route_length(&plan.items, plan.home.as_ref());
    let seconds = route_duration(
        &plan.items,
        plan.home.as_ref(),
        if plan.drives { Some(DRIVE_SPEED_MS) } else { None },
    );
    let mut cells = vec![
        ("Items", plan.items.len().to_string(), false),
        ("Distance", distance_text(units, length), false),
        ("Duration", duration_text(seconds), false),
    ];
    if !plan.drives {
        let lowest = clearances(&points, ground)
            .into_iter()
            .flatten()
            .fold(f64::INFINITY, f64::min);
        if lowest.is_finite() {
            cells.push(("Clearance", length_text(units, lowest), lowest < CLEARANCE_WARN_M));
        }
    }
    cells
}

/// The plan's warnings, in the order the web page gives them.
pub fn problems(plan: &Plan, units: &Units, ground: Option<&Ground>, vehicle: Option<(f64, f64)>) -> Vec<Issue> {
    let mut list = Vec::new();
    let mut say = |text: String, info: &str| {
        list.push(Issue {
            text,
            info: info.to_string(),
        })
    };
    let items = &plan.items;
    let drives = plan.drives;
    if items.is_empty() {
        say(EMPTY_PLAN.into(), "");
    }
    if !items.is_empty() && plan.home.is_none() {
        say(
            "No start point.".into(),
            "Place the start point on the map before uploading.",
        );
    }
    if let Some(offset) = plan.start_offset_m(vehicle).filter(|o| *o > START_OFFSET_WARN_M) {
        say(
            format!(
                "The {} is {} from the start.",
                if drives { "vehicle" } else { "aircraft" },
                distance_text(units, js_round(offset))
            ),
            if drives {
                "It drives the route from where it stands, not from the start. Press VEHICLE to start the plan where the vehicle is, or drive it to the start."
            } else {
                "It takes off where it stands, not at the start, and every height in the plan is measured from where it takes off. Press AIRCRAFT to start the plan where the aircraft is, or carry the aircraft to the start."
            },
        );
    }
    for kind in crate::mission::plan::KINDS {
        let reason = plan.refusal_of(kind);
        let count = items.iter().filter(|i| i.kind == kind).count();
        if reason.is_empty() || count == 0 {
            continue;
        }
        let label = kind.label();
        let instead = if kind.instead().is_empty() {
            "Remove it before uploading."
        } else {
            kind.instead()
        };
        say(
            format!(
                "{} {}{} {}.",
                if count == 1 { "A".to_string() } else { count.to_string() },
                label,
                if count == 1 { "" } else { "s" },
                if drives {
                    "this vehicle will not drive"
                } else {
                    "this aircraft will not fly"
                }
            ),
            &format!("{reason}, so the upload is refused. {instead}"),
        );
    }
    let takeoffs = items.iter().filter(|i| i.kind == Kind::Takeoff).count();
    if takeoffs > 1 {
        say(
            format!("{takeoffs} takeoffs in the plan."),
            "Only the first one climbs. The others are flown as points.",
        );
    }
    if takeoffs == 1 && items[0].kind != Kind::Takeoff {
        say(
            "The takeoff is not the first item.".into(),
            "The aircraft flies to it before climbing.",
        );
    }
    let ends = plan.end_index();
    if ends.is_none() && items.iter().any(|i| i.kind.position() && i.kind != Kind::Takeoff) {
        say(
            "The mission has no ending.".into(),
            if drives {
                "Add a return, or the vehicle stops at the last point when it is done."
            } else {
                "Add a landing or a return, or the aircraft stays at the last point when it is done."
            },
        );
    }
    if let Some(e) = ends.filter(|e| *e < items.len() - 1) {
        let left = items.len() - 1 - e;
        let end = items[e].kind.label().to_lowercase();
        say(
            format!(
                "{left} item{} after the {end} will not be flown.",
                if left == 1 { "" } else { "s" }
            ),
            &format!("The mission ends at the {end}. Move the items before it or delete them."),
        );
    }
    if drives {
        return list;
    }
    let level = items
        .iter()
        .filter(|i| i.kind.position() && i.kind != Kind::Land && i.alt == 0.0)
        .count();
    if level > 0 {
        say(
            format!(
                "{} at ground level.",
                if level == 1 {
                    "A point".to_string()
                } else {
                    format!("{level} points")
                }
            ),
            "At the height of the start, the aircraft flies along the ground. Raise them in the altitude profile.",
        );
    }
    let highest = items
        .iter()
        .filter(|i| i.kind.position())
        .fold(0.0f64, |best, i| best.max(i.alt));
    if highest > CEILING_HINT_M {
        say(
            format!("The plan reaches {}.", length_text(units, highest)),
            &format!(
                "That is above the {} ceiling of the open category.",
                length_text(units, CEILING_HINT_M)
            ),
        );
    }
    let points = stations(items, plan.home.as_ref());
    let approach = if plan.aircraft == Some(Aircraft::FixedWing) {
        landing_approach(&points)
    } else {
        None
    };
    if let Some(a) = approach.filter(|a| a.degrees > GLIDE_MAX_DEG) {
        let slope = GLIDE_MAX_DEG.to_radians().tan();
        let unit = units.length.symbol;
        let away = shown_length(units, a.drop / slope).ceil();
        let lower = shown_length(units, a.run * slope).floor();
        say(
            format!("The landing approach is {} steep.", degrees_text(a.degrees)),
            &format!(
                "A fixed wing glides in at {GLIDE_MAX_DEG}° or flatter, and PX4 refuses a steeper approach on upload unless FW_LND_ANG allows it. Move the point before the landing at least {away} {unit} away from it, or lower it to {lower} {unit}."
            ),
        );
    }
    let gaps: Vec<f64> = clearances(&points, ground).into_iter().flatten().collect();
    if let Some(low) = gaps.iter().copied().reduce(f64::min).filter(|l| *l < CLEARANCE_WARN_M) {
        say(
            format!("Only {} above the ground.", length_text(units, low)),
            "The route passes this close to the terrain. Raise the points near the lowest part of the profile.",
        );
    }
    if let Some(g) = ground {
        let low = profile_marks(&points, plan.aircraft, plan.return_profile.as_ref())
            .iter()
            .filter(|m| m.kind == MarkKind::Descent)
            .filter_map(|m| interpolate_at(g, m.distance).map(|under| m.alt - under))
            .reduce(f64::min);
        if let Some(low) = low.filter(|l| *l < APPROACH_MIN_M) {
            say(
                format!("The descent starts only {} above the ground.", length_text(units, low)),
                "The ground at the landing is higher than the start, and the height the descent starts from is measured from the start. Raise it in the profile.",
            );
        }
    }
    if plan.aircraft == Some(Aircraft::FixedWing) {
        let index = points.iter().position(|s| s.kind == StationKind::Item(Kind::Takeoff));
        let climb = index
            .filter(|i| *i > 0)
            .and_then(|i| takeoff_climb(&points[i - 1], &points[i], points.get(i + 1)));
        if let Some(c) = climb.filter(|c| c.run > c.room) {
            say(
                "The first point comes before the climb is done.".into(),
                &format!(
                    "At {} a fixed wing needs about {} to climb to {}, and the first point is {} out. Move it further away or lower the takeoff.",
                    degrees_text(c.pitch),
                    length_text(units, c.run.ceil()),
                    length_text(units, c.height),
                    length_text(units, c.room.floor())
                ),
            );
        }
    }
    let ending = plan.end_index().map(|i| &items[i]);
    if let Some(e) = ending {
        if e.kind == Kind::Rtl && plan.aircraft == Some(Aircraft::FixedWing) {
            say(
                "The mission ends circling above the start.".into(),
                "A fixed wing does not land after a return. PX4 and ArduPlane both fly back to where it was armed and circle there until you take over. End with LAND to bring it down.",
            );
        } else if e.kind == Kind::Rtl
            && plan.aircraft == Some(Aircraft::Multirotor)
            && plan.return_profile.is_some_and(|r| r.known && r.hold)
        {
            say(
                "The mission ends hovering above the start.".into(),
                "The vehicle is set to wait there after a return rather than land. Take over, or change its Return settings on the Safety page.",
            );
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::plan::{Home, Item};

    fn plan_with(items: Vec<Item>) -> Plan {
        let mut p = Plan::default();
        p.home = Some(Home {
            lat: 48.0,
            lon: 11.0,
            elevation: None,
        });
        p.items = items;
        p
    }

    fn wp(id: u32, lat: f64, alt: f64) -> Item {
        let mut i = Item::new(id, Kind::Waypoint);
        i.lat = lat;
        i.lon = 11.0;
        i.alt = alt;
        i
    }

    fn texts(p: &Plan, g: Option<&Ground>) -> Vec<String> {
        problems(p, &Units::default(), g, None)
            .into_iter()
            .map(|i| i.text)
            .collect()
    }

    #[test]
    fn durations_read_as_the_web_page_reads_them() {
        assert_eq!(duration_text(0.0), "—");
        assert_eq!(duration_text(42.4), "42 s");
        assert_eq!(duration_text(59.6), "1:00");
        assert_eq!(duration_text(125.0), "2:05");
    }

    #[test]
    fn degrees_keep_a_decimal_below_ten() {
        assert_eq!(degrees_text(11.31), "11°");
        assert_eq!(degrees_text(5.04), "5°");
        assert_eq!(degrees_text(5.26), "5.3°");
    }

    #[test]
    fn lengths_follow_the_display_unit() {
        let metric = Units::default();
        assert_eq!(length_text(&metric, 30.0), "30 m");
        assert_eq!(length_text(&metric, 12.5), "12.5 m");
        let feet = Units::from_value(&serde_json::json!({"length": "ft", "speed": "kmh"}));
        assert_eq!(length_text(&feet, 30.0), "98 ft");
        assert_eq!(shown_speed(&feet, 10.0), 36.0);
        assert!((length_from_shown(&feet, 98.0) - 29.8704).abs() < 1e-3);
        assert_eq!(distance_text(&metric, 0.0), "—");
    }

    #[test]
    fn an_empty_plan_says_so_and_a_plan_without_a_start_asks_for_one() {
        let mut p = Plan::default();
        assert_eq!(texts(&p, None), vec![EMPTY_PLAN.to_string()]);
        p.items.push(wp(1, 48.0, 30.0));
        assert!(texts(&p, None).contains(&"No start point.".to_string()));
    }

    #[test]
    fn a_plan_without_an_ending_and_items_after_one_are_called_out() {
        let mut p = plan_with(vec![wp(1, 48.001, 30.0)]);
        assert!(texts(&p, None).contains(&"The mission has no ending.".to_string()));
        let mut rtl = Item::new(2, Kind::Rtl);
        rtl.alt = 0.0;
        p.items.push(rtl);
        p.items.push(wp(3, 48.002, 30.0));
        let t = texts(&p, None);
        assert!(
            t.contains(&"1 item after the return will not be flown.".to_string()),
            "{t:?}"
        );
    }

    #[test]
    fn height_warnings_follow_the_web_rules() {
        let p = plan_with(vec![wp(1, 48.001, 0.0), wp(2, 48.002, 130.0)]);
        let t = texts(&p, None);
        assert!(t.contains(&"A point at ground level.".to_string()));
        assert!(t.contains(&"The plan reaches 130 m.".to_string()));
        let ground = Ground {
            distances: vec![0.0, 1000.0],
            elevations: vec![0.0, 0.0],
        };
        let q = plan_with(vec![wp(1, 48.001, 10.0)]);
        assert!(texts(&q, Some(&ground)).contains(&"Only 10 m above the ground.".to_string()));
    }

    #[test]
    fn a_steep_fixed_wing_landing_gets_the_two_ways_out() {
        let mut p = plan_with(vec![wp(1, 48.0009, 40.0), {
            let mut l = Item::new(2, Kind::Land);
            l.lat = 48.0027;
            l.lon = 11.0;
            l
        }]);
        p.aircraft = Some(Aircraft::FixedWing);
        let issues = problems(&p, &Units::default(), None, None);
        let steep = issues
            .iter()
            .find(|i| i.text.starts_with("The landing approach"))
            .expect("steep");
        assert_eq!(steep.text, "The landing approach is 11° steep.");
        assert!(steep.info.contains("at least 458 m away"), "{}", steep.info);
        assert!(steep.info.contains("lower it to 17 m"), "{}", steep.info);
        p.aircraft = Some(Aircraft::Multirotor);
        assert!(!texts(&p, None).iter().any(|t| t.contains("landing approach")));
    }

    #[test]
    fn an_aircraft_away_from_the_start_is_said_in_the_plans_words() {
        let p = plan_with(vec![wp(1, 48.001, 30.0)]);
        let t: Vec<String> = problems(&p, &Units::default(), None, Some((11.0, 48.0018)))
            .into_iter()
            .map(|i| i.text)
            .collect();
        assert!(
            t.contains(&"The aircraft is 200 m from the start.".to_string()),
            "{t:?}"
        );
        let near: Vec<String> = problems(&p, &Units::default(), None, Some((11.0, 48.0001)))
            .into_iter()
            .map(|i| i.text)
            .collect();
        assert!(!near.iter().any(|t| t.contains("from the start")));
    }

    #[test]
    fn rows_are_short_and_say_only_what_changes() {
        let units = Units::default();
        let mut p = plan_with(Vec::new());
        let mut w = wp(1, 48.0, 40.0);
        w.hold = 5.0;
        w.speed = Some(8.0);
        assert_eq!(describe_item(&p, &w, &units), "40 m · 5 s · 8 m/s");
        let mut c = Item::new(2, Kind::LoiterTurns);
        c.alt = 30.0;
        c.radius = 80.0;
        c.turns = 2.5;
        c.direction = -1.0;
        assert_eq!(describe_item(&p, &c, &units), "30 m · ↺80 m · 2.5×");
        let mut h = Item::new(3, Kind::LoiterTime);
        h.alt = 30.0;
        h.seconds = 45.0;
        p.aircraft = Some(Aircraft::Multirotor);
        assert_eq!(describe_item(&p, &h, &units), "30 m · 45 s", "a hover has no ring");
        p.aircraft = Some(Aircraft::FixedWing);
        assert_eq!(describe_item(&p, &h, &units), "30 m · ↻50 m · 45 s");
        let mut l = Item::new(4, Kind::Land);
        assert_eq!(describe_item(&p, &l, &units), "ground");
        p.aircraft = Some(Aircraft::Multirotor);
        l.approach_alt = Some(3.0);
        assert_eq!(describe_item(&p, &l, &units), "straight in");
        l.approach_alt = Some(12.0);
        assert_eq!(describe_item(&p, &l, &units), "down from 12 m");
        let mut t = Item::new(5, Kind::Takeoff);
        t.alt = 30.0;
        assert_eq!(describe_item(&p, &t, &units), "climb to 30 m");
        assert_eq!(describe_item(&p, &Item::new(6, Kind::Rtl), &units), "to start");
    }

    #[test]
    fn the_summary_has_a_clearance_cell_only_with_ground() {
        let p = plan_with(vec![wp(1, 48.001, 30.0)]);
        let s = summary(&p, &Units::default(), None);
        assert_eq!(
            s.iter().map(|c| c.0).collect::<Vec<_>>(),
            ["Items", "Distance", "Duration"]
        );
        assert_eq!(s[0].1, "1");
        let g = Ground {
            distances: vec![0.0, 200.0],
            elevations: vec![20.0, 20.0],
        };
        let s = summary(&p, &Units::default(), Some(&g));
        assert_eq!(s[3], ("Clearance", "10 m".to_string(), true));
    }
}
