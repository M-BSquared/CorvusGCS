//! The planner's calls to the backend (`/api/mission/...`,
//! `/api/mavlink/capabilities`), each on a thread of its own with its answer
//! picked up by the frame that finds it there.

use eframe::egui;
use serde_json::{json, Value};

use crate::net::bg::Pending;
use crate::net::http::{self, Server};

/// One request, with what its answer is for.
#[derive(Debug, Clone, PartialEq)]
pub enum Job {
    /// Write the plan to the vehicle; `fly` also starts it.
    Upload {
        fly: bool,
    },
    /// Read the vehicle's mission as a plan.
    Read,
    Save,
    ListPlans,
    LoadPlan {
        name: String,
        quiet: bool,
    },
    RemovePlan {
        name: String,
    },
    /// How the connected vehicle flies a Return; `token` drops a stale answer.
    Return {
        token: u32,
    },
    /// What the connected stack will not fly; `stack` drops a stale answer.
    Capabilities {
        stack: String,
    },
}

impl Job {
    /// The path, and the body for a POST (None for a GET).
    pub fn request(&self, plan: &Value, name: &str) -> (&'static str, Option<Value>) {
        match self {
            Job::Upload { fly } => ("/api/mission/upload", Some(json!({"plan": plan, "start": fly}))),
            Job::Read => ("/api/mission/download", Some(json!({}))),
            Job::Save => ("/api/mission/plans/save", Some(json!({"name": name, "plan": plan}))),
            Job::ListPlans => ("/api/mission/plans", None),
            Job::LoadPlan { name, .. } => ("/api/mission/plans/load", Some(json!({"name": name}))),
            Job::RemovePlan { name } => ("/api/mission/plans/remove", Some(json!({"name": name}))),
            Job::Return { .. } => ("/api/mission/return", None),
            Job::Capabilities { .. } => ("/api/mavlink/capabilities", None),
        }
    }
}

/// The requests on their way.
pub struct Calls {
    server: Server,
    running: Vec<(Job, Pending<Result<Value, String>>)>,
}

impl Calls {
    pub fn new(server: Server) -> Calls {
        Calls {
            server,
            running: Vec::new(),
        }
    }

    pub fn start(&mut self, ctx: &egui::Context, job: Job, plan: &Value, name: &str) {
        let (path, body) = job.request(plan, name);
        let server = self.server.clone();
        let pending = Pending::spawn(ctx, move || match body {
            Some(b) => http::post_json(&server, path, &b),
            None => http::get_json(&server, path),
        });
        self.running.push((job, pending));
    }

    /// Whether a request of this kind is in flight.
    pub fn busy(&self, matches: impl Fn(&Job) -> bool) -> bool {
        self.running.iter().any(|(j, _)| matches(j))
    }

    /// The answers that have arrived.
    pub fn poll(&mut self) -> Vec<(Job, Result<Value, String>)> {
        let mut done = Vec::new();
        let mut i = 0;
        while i < self.running.len() {
            match self.running[i].1.take() {
                Some(r) => done.push((self.running.remove(i).0, r)),
                None => i += 1,
            }
        }
        done
    }
}

/// A saved plan in the Open list.
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub name: String,
    pub title: String,
    pub items: i64,
    pub distance_m: f64,
}

/// `GET /api/mission/plans`: the saved plans and the folder they are in.
pub fn read_plans(v: &Value) -> (Vec<Saved>, String) {
    let list = v
        .get("plans")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let name = e.get("name")?.as_str()?.to_string();
                    Some(Saved {
                        title: e.get("title").and_then(Value::as_str).unwrap_or(&name).to_string(),
                        name,
                        items: e.get("items").and_then(Value::as_i64).unwrap_or(0),
                        distance_m: e.get("distance_m").and_then(Value::as_f64).unwrap_or(0.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let dir = v
        .get("dir")
        .and_then(Value::as_str)
        .unwrap_or("~/.corvus/missions")
        .to_string();
    (list, dir)
}

/// What a download answer says, in the planner's words.
pub fn read_report(res: &Value) -> (String, bool) {
    let count = res.get("count").and_then(Value::as_i64).unwrap_or(0);
    let skipped = res
        .get("skipped")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let adjusted = res.get("adjusted").and_then(Value::as_array).map_or(0, Vec::len);
    let mut text = if count > 0 {
        format!(
            "Read {count} item{} from the vehicle.",
            if count == 1 { "" } else { "s" }
        )
    } else {
        "The vehicle holds no mission.".into()
    };
    if let Some(first) = skipped.first() {
        let reason = first.get("reason").and_then(Value::as_str).unwrap_or("");
        text += &format!(
            " {} cannot be drawn here ({reason}{}). Uploading this plan would take them off the vehicle.",
            skipped.len(),
            if skipped.len() > 1 { ", and more" } else { "" }
        );
    }
    if adjusted > 0 {
        text += &format!(
            " {adjusted} value{} changed to fit the planner.",
            if adjusted == 1 { " was" } else { "s were" }
        );
    }
    (text, !skipped.is_empty() || adjusted > 0)
}

/// The upload's report.
pub fn upload_report(res: &Value, fly: bool) -> String {
    let count = res.get("items").and_then(Value::as_i64).unwrap_or(0);
    if fly {
        format!("Mission started: {count} items on the vehicle.")
    } else {
        format!("{count} items on the vehicle. Start it from the flight modes when ready.")
    }
}

/// `mission_unsupported` of the capabilities, as (type, reason).
pub fn read_refusals(caps: &Value) -> Vec<(String, String)> {
    caps.get("mission_unsupported")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_job_asks_for_the_path_the_web_page_does() {
        let plan = json!({"items": []});
        let (path, body) = Job::Upload { fly: true }.request(&plan, "M");
        assert_eq!(path, "/api/mission/upload");
        assert_eq!(body, Some(json!({"plan": {"items": []}, "start": true})));
        assert_eq!(
            Job::Save.request(&plan, "Field").1,
            Some(json!({"name": "Field", "plan": {"items": []}}))
        );
        assert_eq!(Job::Read.request(&plan, ""), ("/api/mission/download", Some(json!({}))));
        assert_eq!(Job::ListPlans.request(&plan, ""), ("/api/mission/plans", None));
        assert_eq!(
            Job::LoadPlan {
                name: "a".into(),
                quiet: true
            }
            .request(&plan, ""),
            ("/api/mission/plans/load", Some(json!({"name": "a"})))
        );
        assert_eq!(
            Job::RemovePlan { name: "a".into() }.request(&plan, "").0,
            "/api/mission/plans/remove"
        );
        assert_eq!(
            Job::Return { token: 1 }.request(&plan, ""),
            ("/api/mission/return", None)
        );
        assert_eq!(
            Job::Capabilities { stack: "px4".into() }.request(&plan, "").0,
            "/api/mavlink/capabilities"
        );
    }

    #[test]
    fn the_saved_list_reads_names_titles_and_the_folder() {
        let v = json!({"plans": [{"name": "a", "title": "Alpha", "items": 4, "distance_m": 1200.0}, {"name": "b"}], "dir": "/m"});
        let (list, dir) = read_plans(&v);
        assert_eq!(dir, "/m");
        assert_eq!(
            list[0],
            Saved {
                name: "a".into(),
                title: "Alpha".into(),
                items: 4,
                distance_m: 1200.0
            }
        );
        assert_eq!(list[1].title, "b");
        assert_eq!(read_plans(&json!({})).1, "~/.corvus/missions");
    }

    #[test]
    fn reading_the_vehicle_says_what_was_lost() {
        assert_eq!(
            read_report(&json!({"count": 1})),
            ("Read 1 item from the vehicle.".to_string(), false)
        );
        assert_eq!(read_report(&json!({"count": 0})).0, "The vehicle holds no mission.");
        let (text, warn) = read_report(&json!({
            "count": 5,
            "skipped": [{"reason": "a camera trigger"}, {"reason": "x"}],
            "adjusted": [1, 2]
        }));
        assert!(warn);
        assert_eq!(
            text,
            "Read 5 items from the vehicle. 2 cannot be drawn here (a camera trigger, and more). Uploading this plan would take them off the vehicle. 2 values were changed to fit the planner."
        );
    }

    #[test]
    fn the_upload_report_counts_the_items() {
        assert_eq!(
            upload_report(&json!({"items": 7}), false),
            "7 items on the vehicle. Start it from the flight modes when ready."
        );
        assert_eq!(
            upload_report(&json!({"items": 7}), true),
            "Mission started: 7 items on the vehicle."
        );
    }

    #[test]
    fn the_refusals_come_from_the_capabilities() {
        let caps = json!({"mission_unsupported": {"loiter_turns": "PX4 does not fly an orbit"}});
        assert_eq!(
            read_refusals(&caps),
            [("loiter_turns".to_string(), "PX4 does not fly an orbit".to_string())]
        );
        assert!(read_refusals(&json!({})).is_empty());
    }
}
