//! The plan of the operator's mission planner, as an AI client reads and changes it.
//!
//! The window reports the plan it shows (`/api/mission/planner/mirror`) and takes in what a
//! client put there from the `planner` topic. Nothing here reaches the vehicle: uploading is
//! `/api/mission/upload`, as it is for the planner's own button. Every plan a client sets gets
//! the next number, and a report made before the window knew of it is not taken, so a plan on
//! its way to the window is never overwritten by the one it replaces.

use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};

use crate::core::Core;
use crate::events::{Binding, Topic};
use crate::mission::validate_plan;
use crate::module::{Module, Registry};
use crate::request::Request;
use crate::response::Response;

const TOPIC_CAPACITY: usize = 16;
/// The most a window may say about a plan, in bytes.
const CHECKS_MAX: usize = 16_384;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

type Listener = Arc<dyn Fn(&Value) + Send + Sync>;

#[derive(Default)]
struct State {
    /// The plan shown, or the one on its way to the window.
    plan: Option<Value>,
    seq: u64,
    /// The last number a window reported back.
    shown: u64,
    /// What the window says about the plan it shows: its warnings and its summary.
    checks: Option<Value>,
    /// Whether a window reported, and has its planner page switched on.
    page: Option<bool>,
    listeners: Vec<(u64, Listener)>,
    next_listener: u64,
}

impl State {
    fn tell(&self, event: &Value) {
        for (_, f) in &self.listeners {
            f(event);
        }
    }

    fn snapshot(&self) -> Value {
        json!({"type": "plan", "seq": self.seq, "plan": self.plan})
    }
}

#[derive(Default)]
pub struct PlannerService {
    state: Mutex<State>,
}

impl PlannerService {
    /// The plan of the planner, its number, whether the planner's page is on, and what the window
    /// says about this plan (null until it shows it).
    pub fn read(&self) -> Value {
        let st = lock(&self.state);
        json!({"ok": true, "plan": st.plan, "seq": st.seq, "page": st.page, "checks": st.checks})
    }

    /// Puts a plan into the planner: the cleaned plan and its number.
    pub fn set(&self, raw: &Value) -> Result<(Value, u64), String> {
        let plan = validate_plan(raw)?;
        let mut st = lock(&self.state);
        st.plan = Some(plan.clone());
        st.checks = None;
        st.seq += 1;
        st.tell(&st.snapshot());
        Ok((plan, st.seq))
    }

    /// What the window shows. False when a newer plan is on its way to it.
    pub fn mirror(&self, raw: &Value, seq: u64, page: bool, checks: Option<&Value>) -> Result<bool, String> {
        let plan = validate_plan(raw)?;
        let mut st = lock(&self.state);
        st.page = Some(page);
        if seq < st.seq {
            return Ok(false);
        }
        st.shown = st.seq;
        st.checks = checks
            .filter(|c| c.is_object() && c.to_string().len() <= CHECKS_MAX)
            .cloned();
        st.plan = Some(plan);
        Ok(true)
    }

    /// The vehicle took plan `seq`, as its mission `revision`.
    pub fn sent(&self, seq: u64, revision: &Value) {
        let st = lock(&self.state);
        st.tell(&json!({"type": "sent", "seq": seq, "revision": revision}));
    }

    fn add_listener(&self, f: Listener) -> (u64, Option<Value>) {
        let mut st = lock(&self.state);
        st.next_listener += 1;
        let id = st.next_listener;
        st.listeners.push((id, f));
        (id, (st.seq > st.shown).then(|| st.snapshot()))
    }

    fn remove_listener(&self, id: u64) {
        lock(&self.state).listeners.retain(|(i, _)| *i != id);
    }
}

#[derive(Default)]
pub struct PlannerModule {
    service: Arc<PlannerService>,
}

impl PlannerModule {
    pub fn new() -> PlannerModule {
        PlannerModule::default()
    }
}

impl Module for PlannerModule {
    fn name(&self) -> &'static str {
        "planner"
    }

    fn register(&self, reg: &mut Registry) {
        reg.service(Arc::clone(&self.service));
        reg.topic(Topic::new("planner", TOPIC_CAPACITY, |core, sink, _job| {
            let Some(svc) = core.service::<PlannerService>() else {
                return Binding::none();
            };
            let (id, initial) = svc.add_listener(Arc::new(move |event: &Value| sink.put_fifo(event.clone())));
            let svc2 = svc.clone();
            Binding {
                unbind: Box::new(move || svc2.remove_listener(id)),
                initial,
            }
        }));
        reg.get("/api/mission/planner", api_read);
        reg.post("/api/mission/planner", api_set);
        reg.post("/api/mission/planner/mirror", api_mirror);
        reg.post("/api/mission/planner/sent", api_sent);
    }
}

// ---------------------------------------------------------------- routes

macro_rules! svc {
    ($core:expr) => {
        match $core.service::<PlannerService>() {
            Some(s) => s,
            None => return Response::not_ok(503, "The mission planner is not available"),
        }
    };
}

fn api_read(core: &Core, _req: &Request) -> Response {
    Response::json(svc!(core).read())
}

fn api_set(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    match svc.set(req.payload().get("plan").unwrap_or(&Value::Null)) {
        Ok((plan, seq)) => Response::json(json!({"ok": true, "plan": plan, "seq": seq, "page": svc.read()["page"]})),
        Err(why) => Response::not_ok(400, &why),
    }
}

fn api_mirror(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let p = req.payload();
    let seq = p.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let page = p.get("page") == Some(&Value::Bool(true));
    match svc.mirror(p.get("plan").unwrap_or(&Value::Null), seq, page, p.get("checks")) {
        Ok(taken) => Response::json(json!({"ok": true, "taken": taken})),
        Err(why) => Response::not_ok(400, &why),
    }
}

fn api_sent(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let p = req.payload();
    match p.get("seq").and_then(Value::as_u64) {
        Some(seq) => {
            svc.sent(seq, p.get("revision").unwrap_or(&Value::Null));
            Response::json(json!({"ok": true}))
        }
        None => Response::not_ok(400, "seq must be a number"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(n: usize) -> Value {
        let items: Vec<Value> = (0..n)
            .map(|i| json!({"type": "waypoint", "lat": 48.0 + i as f64 * 0.001, "lon": 11.0, "alt": 30}))
            .collect();
        json!({"name": "T", "items": items})
    }

    fn count(svc: &PlannerService) -> usize {
        svc.read()["plan"]["items"].as_array().map_or(0, Vec::len)
    }

    #[test]
    fn a_report_made_before_a_plan_was_set_does_not_take_it_away() {
        let svc = PlannerService::default();
        assert_eq!(svc.mirror(&plan(2), 0, true, None), Ok(true));
        assert_eq!(svc.set(&plan(3)).unwrap().1, 1);
        assert_eq!(
            svc.mirror(&plan(2), 0, true, None),
            Ok(false),
            "the window has not seen plan 1 yet"
        );
        assert_eq!(count(&svc), 3);
        assert_eq!(
            svc.mirror(&plan(4), 1, true, None),
            Ok(true),
            "the operator went on from plan 1"
        );
        assert_eq!(count(&svc), 4);
        let checks = json!({"problems": ["Too low"]});
        assert_eq!(svc.mirror(&plan(4), 1, true, Some(&checks)), Ok(true));
        assert_eq!(svc.read()["checks"], checks);
        assert!(svc.set(&json!({"items": [{"type": "teleport"}]})).is_err());
        assert_eq!(count(&svc), 4, "a plan that is refused changes nothing");
    }

    #[test]
    fn a_window_that_comes_late_still_gets_the_plan() {
        let svc = PlannerService::default();
        assert!(svc.add_listener(Arc::new(|_| {})).1.is_none());
        svc.set(&plan(1)).unwrap();
        let heard: Arc<Mutex<Vec<Value>>> = Arc::default();
        let sink = heard.clone();
        let (id, first) = svc.add_listener(Arc::new(move |e| lock(&sink).push(e.clone())));
        assert_eq!(first.unwrap()["seq"], 1);
        svc.sent(1, &json!(7));
        svc.remove_listener(id);
        svc.set(&plan(2)).unwrap();
        assert_eq!(*lock(&heard), vec![json!({"type": "sent", "seq": 1, "revision": 7})]);
    }
}
