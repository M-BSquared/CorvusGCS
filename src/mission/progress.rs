//! What the vehicle is flying, as far as it is this plan (`syncWith`,
//! `onVehicleMission`, `currentProgress`, `renderVehicle` of `mission.js`).
//!
//! `Synced` is the backend's mission revision when this plan was put on (or
//! read off) the aircraft, with the plan as it was then. An edit, or another
//! station's upload, ends the claim: a leg is never highlighted on a route
//! the aircraft is not flying.

use serde_json::Value;

use crate::mission::plan::{Kind, Plan};

#[derive(Debug, Clone, PartialEq)]
pub struct Synced {
    pub revision: i64,
    pub key: String,
    pub vehicle: String,
}

/// The mission fields of a telemetry frame.
#[derive(Debug, Clone, PartialEq)]
pub struct VehicleMission {
    pub connected: bool,
    pub state: String,
    pub total: i64,
    pub known: bool,
    pub revision: i64,
    pub vehicle: String,
    pub item: i64,
    pub reached: i64,
}

impl VehicleMission {
    pub fn from_state(frame: &Value) -> VehicleMission {
        let num = |k: &str, d: i64| frame.get(k).and_then(Value::as_f64).map_or(d, |v| v as i64);
        let text = |k: &str| frame.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        VehicleMission {
            connected: frame.get("connected").and_then(Value::as_bool).unwrap_or(false),
            state: text("mission_state"),
            total: num("mission_total", -1),
            known: frame.get("mission_known").and_then(Value::as_bool).unwrap_or(false),
            revision: num("mission_revision", 0),
            vehicle: text("vehicle_id"),
            item: num("mission_item", -1),
            reached: num("mission_reached_item", -1),
        }
    }
}

/// Where the flight is, once the vehicle is known to fly this plan.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub item: i64,
    pub reached: i64,
    pub state: String,
}

#[derive(Default)]
pub struct Tracker {
    pub synced: Option<Synced>,
    pub vehicle: Option<VehicleMission>,
    pub progress: Option<Progress>,
}

impl Tracker {
    /// This plan is now what the vehicle holds, at the backend's `revision`.
    pub fn sync_with(&mut self, plan: &Plan, revision: Option<i64>, vehicle_id: &str) {
        self.synced = revision.map(|revision| Synced {
            revision,
            key: plan.key(),
            vehicle: vehicle_id.to_string(),
        });
        self.apply(plan);
    }

    pub fn is_synced(&self) -> bool {
        match (&self.vehicle, &self.synced) {
            (Some(v), Some(s)) => {
                v.known
                    && v.revision == s.revision
                    && (s.vehicle.is_empty() || v.vehicle.is_empty() || v.vehicle == s.vehicle)
            }
            _ => false,
        }
    }

    /// Takes in a telemetry frame; true when anything about the mission changed.
    pub fn on_frame(&mut self, plan: &Plan, frame: &Value) -> bool {
        let next = VehicleMission::from_state(frame);
        if self.vehicle.as_ref() == Some(&next) {
            return false;
        }
        self.vehicle = Some(next);
        self.apply(plan);
        true
    }

    /// Recomputes the claim, after an edit as well.
    pub fn apply(&mut self, plan: &Plan) {
        self.progress = if self.is_synced() && self.synced.as_ref().is_some_and(|s| s.key == plan.key()) {
            self.vehicle.as_ref().map(|v| Progress {
                item: v.item,
                reached: v.reached,
                state: v.state.clone(),
            })
        } else {
            None
        };
    }

    /// The plan item being flown to, or None when nothing is.
    pub fn current_index(&self) -> Option<usize> {
        let p = self.progress.as_ref()?;
        if matches!(p.state.as_str(), "not_started" | "complete" | "no_mission") {
            return None;
        }
        usize::try_from(p.item).ok()
    }

    /// How many items are behind the aircraft.
    pub fn done_before(&self, len: usize) -> usize {
        match &self.progress {
            Some(p) if p.state == "complete" => len,
            Some(p) if p.state != "not_started" => usize::try_from(p.item).unwrap_or(0),
            _ => 0,
        }
    }
}

/// The line about the vehicle's side of it: (text, level, offer to read).
pub fn vehicle_line(t: &Tracker, plan: &Plan) -> (String, &'static str, bool) {
    let Some(v) = t.vehicle.as_ref().filter(|v| v.connected) else {
        return (String::new(), "", false);
    };
    if let Some(p) = &t.progress {
        let index = t.current_index();
        let text = match p.state.as_str() {
            "complete" => "The vehicle has flown this mission to the end.".to_string(),
            "not_started" => "This plan is on the vehicle, not started yet.".to_string(),
            "paused" => format!(
                "Paused. Next: {}.",
                plan.item_label(usize::try_from(p.item).unwrap_or(usize::MAX))
            ),
            _ => match index {
                Some(i) if plan.items.get(i).is_some_and(|it| !it.kind.position()) => {
                    "Returning to launch.".to_string()
                }
                Some(i) => format!("Flying to {}.", plan.item_label(i)),
                None => "This plan is on the vehicle.".to_string(),
            },
        };
        return (text, "live", false);
    }
    if t.is_synced() {
        return (
            "Changed since it was put on the vehicle. Upload to fly this version.".into(),
            "warn",
            false,
        );
    }
    if v.total > 0 {
        return (
            format!(
                "The vehicle holds a mission of {} item{} that is not this plan.",
                v.total,
                if v.total == 1 { "" } else { "s" }
            ),
            "warn",
            true,
        );
    }
    (String::new(), "", false)
}

/// The leg being flown: from the position before the current item (or the
/// start) to the current item (or the start, for a return), as `[lon, lat]`.
pub fn current_leg(t: &Tracker, plan: &Plan) -> Option<([f64; 2], [f64; 2])> {
    let index = t.current_index()?;
    let target = plan.items.get(index)?;
    let from = plan.items[..index]
        .iter()
        .rev()
        .find(|i| i.kind.position())
        .map(|i| [i.lon, i.lat])
        .or_else(|| plan.home.map(|h| [h.lon, h.lat]))?;
    let to = if target.kind != Kind::Rtl {
        [target.lon, target.lat]
    } else {
        let h = plan.home?;
        [h.lon, h.lat]
    };
    Some((from, to))
}

/// The plan as the Home map draws it, when the vehicle is known to fly it.
pub fn flown_route(t: &Tracker, plan: &Plan) -> Option<crate::map::flown::FlownRoute> {
    use crate::map::flown::{FlownRoute, Mark, Stage};
    t.progress.as_ref()?;
    let route: Vec<[f64; 2]> = crate::mission::geometry::stations(&plan.items, plan.home.as_ref())
        .iter()
        .map(|s| [s.lon, s.lat])
        .collect();
    if route.len() < 2 {
        return None;
    }
    let current = t.current_index();
    let done = t.done_before(plan.items.len());
    let marks = plan
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| i.kind.position())
        .map(|(index, item)| Mark {
            at: [item.lon, item.lat],
            number: plan.position_number(index),
            stage: if current == Some(index) {
                Stage::Current
            } else if index < done {
                Stage::Done
            } else {
                Stage::Ahead
            },
        })
        .collect();
    Some(FlownRoute {
        route,
        marks,
        leg: current_leg(t, plan),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plan() -> Plan {
        let mut p = Plan::default();
        p.place_start(11.0, 48.0, false);
        p.add_item(Kind::Waypoint, Some((11.001, 48.0)), None);
        p
    }

    fn frame(revision: i64, item: i64, state: &str) -> Value {
        json!({"connected": true, "mission_state": state, "mission_total": 2, "mission_known": true,
               "mission_revision": revision, "vehicle_id": "v1", "mission_item": item, "mission_reached_item": 0})
    }

    #[test]
    fn the_home_map_draws_only_a_plan_the_vehicle_flies_and_marks_the_leg() {
        use crate::map::flown::Stage;
        let mut p = plan();
        p.add_item(Kind::Waypoint, Some((11.002, 48.0)), None);
        let mut t = Tracker::default();
        assert_eq!(
            flown_route(&t, &p),
            None,
            "nothing is drawn before the plan is on the vehicle"
        );
        t.on_frame(&p, &frame(5, 1, "running"));
        assert_eq!(flown_route(&t, &p), None, "a plan nobody uploaded is not the vehicle's");
        t.sync_with(&p, Some(5), "v1");
        let f = flown_route(&t, &p).expect("the vehicle flies this plan");
        assert!(f.route.len() >= 3, "the start and its points");
        let stages: Vec<Stage> = f.marks.iter().map(|m| m.stage).collect();
        assert_eq!(stages, [Stage::Done, Stage::Current, Stage::Ahead]);
        assert_eq!(f.marks[1].number, 2);
        assert!(f.leg.is_some());
        p.add_item(Kind::Waypoint, Some((11.003, 48.0)), None);
        t.apply(&p);
        assert_eq!(flown_route(&t, &p), None, "an edit ends the claim");
    }

    #[test]
    fn nothing_is_claimed_until_the_plan_is_on_the_vehicle() {
        let p = plan();
        let mut t = Tracker::default();
        t.on_frame(&p, &frame(3, 1, "active"));
        assert_eq!(t.progress, None);
        t.sync_with(&p, Some(3), "v1");
        assert_eq!(
            t.progress,
            Some(Progress {
                item: 1,
                reached: 0,
                state: "active".into()
            })
        );
        assert_eq!(t.current_index(), Some(1));
        t.on_frame(&p, &frame(3, 2, "complete"));
        assert_eq!(t.current_index(), None, "a finished mission has no leg being flown");
        assert_eq!(t.done_before(2), 2);
    }

    #[test]
    fn another_station_or_an_edit_ends_the_claim() {
        let mut p = plan();
        let mut t = Tracker::default();
        t.sync_with(&p, Some(3), "v1");
        t.on_frame(&p, &frame(3, 1, "active"));
        assert!(t.progress.is_some());
        t.on_frame(&p, &frame(4, 1, "active"));
        assert_eq!(t.progress, None, "another station replaced the mission");
        t.on_frame(&p, &frame(3, 1, "active"));
        assert!(t.progress.is_some(), "synced again");
        p.items[1].alt += 1.0;
        t.apply(&p);
        assert_eq!(t.progress, None, "an edited plan is no longer the one being flown");
        p.items[1].alt -= 1.0;
        p.items[1].name = "x".into();
        t.apply(&p);
        assert!(t.progress.is_some(), "a name never reaches the vehicle");
    }

    #[test]
    fn a_revision_is_only_the_same_on_the_same_vehicle() {
        let p = plan();
        let mut t = Tracker::default();
        t.sync_with(&p, Some(3), "v1");
        let mut other = frame(3, 1, "active");
        other["vehicle_id"] = json!("v2");
        t.on_frame(&p, &other);
        assert_eq!(t.progress, None, "another vehicle with the same revision");
    }

    #[test]
    fn the_line_names_the_leg_or_offers_to_read() {
        let p = plan();
        let mut t = Tracker::default();
        assert_eq!(vehicle_line(&t, &p).0, "");
        t.on_frame(&p, &frame(9, 1, "active"));
        let (text, level, offer) = vehicle_line(&t, &p);
        assert_eq!(
            (text.as_str(), level, offer),
            (
                "The vehicle holds a mission of 2 items that is not this plan.",
                "warn",
                true
            )
        );
        t.sync_with(&p, Some(9), "v1");
        assert_eq!(
            vehicle_line(&t, &p),
            ("Flying to 2. Waypoint.".to_string(), "live", false)
        );
        t.on_frame(&p, &frame(9, 0, "not_started"));
        assert_eq!(vehicle_line(&t, &p).0, "This plan is on the vehicle, not started yet.");
        t.on_frame(&p, &frame(9, 1, "paused"));
        assert_eq!(vehicle_line(&t, &p).0, "Paused. Next: 2. Waypoint.");
        let mut edited = p;
        edited.items[1].alt = 99.0;
        t.apply(&edited);
        assert_eq!(
            vehicle_line(&t, &edited),
            (
                "Changed since it was put on the vehicle. Upload to fly this version.".to_string(),
                "warn",
                false
            )
        );
    }

    #[test]
    fn the_leg_runs_from_the_position_before_to_the_current_item() {
        let mut p = plan();
        p.add_item(Kind::Rtl, None, None);
        let mut t = Tracker::default();
        t.sync_with(&p, Some(1), "v1");
        t.on_frame(&p, &frame(1, 1, "active"));
        assert_eq!(current_leg(&t, &p), Some(([11.0, 48.0], [11.001, 48.0])));
        t.on_frame(&p, &frame(1, 2, "active"));
        assert_eq!(
            current_leg(&t, &p),
            Some(([11.001, 48.0], [11.0, 48.0])),
            "a return heads for the start"
        );
        assert_eq!(vehicle_line(&t, &p).0, "Returning to launch.");
    }
}
