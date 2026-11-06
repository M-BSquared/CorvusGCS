//! The module that is always there: the routes the foundation owns.

use std::sync::Arc;

use serde_json::Value;

use crate::core::Core;
use crate::events::TelemetryStream;
use crate::hub::StoreHub;
use crate::module::{Module, Registry};
use crate::request::Request;
use crate::response::Response;
use crate::store::VehicleStateStore;

pub(crate) struct Foundation;

impl Module for Foundation {
    fn name(&self) -> &'static str {
        "foundation"
    }

    fn register(&self, reg: &mut Registry) {
        reg.service(Arc::new(StoreHub::new(Arc::new(VehicleStateStore::new()))));
        reg.get("/api/state", api_state);
        reg.get("/api/telemetry", api_telemetry);
        reg.get("/api/events", api_events);
    }
}

fn api_state(core: &Core, req: &Request) -> Response {
    let hub = core.service::<StoreHub>().expect("store hub");
    match hub.for_request(core, req) {
        Ok(store) => Response::json(Value::Object((*store.snapshot()).clone())),
        Err(refusal) => refusal,
    }
}

fn api_telemetry(core: &Core, _req: &Request) -> Response {
    Response::sse(Box::new(TelemetryStream::attach(core.store())))
}

fn api_events(core: &Core, req: &Request) -> Response {
    let asked: Vec<String> = req
        .query
        .get("topics")
        .unwrap_or("")
        .split(',')
        .map(|t| t.trim().to_string())
        .collect();
    let wanted: Vec<(&str, &str)> = asked
        .iter()
        .filter_map(|t| core.topic_names().into_iter().find(|n| n == t))
        .map(|n| (n, n))
        .collect();
    if wanted.is_empty() {
        return Response::error(400, "no known topics requested");
    }
    Response::sse(core.topic_stream(&wanted, req.query.get("job"), &[]))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::buffer::Sink;
    use crate::core::{CoreBuilder, Options};
    use crate::events::{collect, Binding, Topic};
    use crate::request::Method;
    use crate::response::Body;

    type Slot = Arc<std::sync::Mutex<Option<Sink<Value>>>>;

    struct Feed(Arc<AtomicBool>, Slot);

    impl Module for Feed {
        fn name(&self) -> &'static str {
            "feed"
        }

        fn register(&self, reg: &mut Registry) {
            let (unbound, slot) = (self.0.clone(), self.1.clone());
            reg.topic(
                Topic::new("fleet", 1, move |_, sink, _job| {
                    *slot.lock().unwrap() = Some(sink);
                    let unbound = unbound.clone();
                    Binding {
                        unbind: Box::new(move || unbound.store(true, Ordering::SeqCst)),
                        initial: Some(json!({"first": true})),
                    }
                })
                .shaped(|v| json!({"shaped": v})),
            );
        }
    }

    fn core() -> (Core, Arc<AtomicBool>, Slot) {
        let unbound = Arc::new(AtomicBool::new(false));
        let slot = Arc::new(std::sync::Mutex::new(None));
        let c = CoreBuilder::new(Options::default())
            .module(Feed(unbound.clone(), slot.clone()))
            .build()
            .unwrap();
        (c, unbound, slot)
    }

    fn stream_of(r: Response) -> Box<dyn crate::response::SseStream> {
        match r.body {
            Body::Sse(s) => s,
            _ => panic!("not a stream"),
        }
    }

    #[test]
    fn state_is_the_snapshot() {
        let (c, _, _) = core();
        c.store().set("heading", json!(12.5));
        let (status, body) = c.internal_call("GET", "/api/state", None);
        assert_eq!(status, 200);
        assert_eq!(body["heading"], json!(12.5));
        assert_eq!(body["autopilot_stack"], json!(""));
        assert_eq!(c.internal_call("GET", "/api/state?vehicle=main%3A1", None).0, 200);
    }

    #[test]
    fn telemetry_sends_the_snapshot_then_each_change_under_state() {
        let (c, _, _) = core();
        c.store().set("heading", json!(1.0));
        let mut s = stream_of(c.dispatch(Request::new(Method::Get, "/api/telemetry")));
        let first = s.open();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].name, "state");
        let v: Value = serde_json::from_slice(&first[0].data).unwrap();
        assert_eq!(v["heading"], json!(1.0));
        c.store().set("armed", json!(true));
        let events = collect(&mut *s, Duration::from_secs(2), 1);
        let v: Value = serde_json::from_slice(&events[0].data).unwrap();
        assert_eq!(v["armed"], json!(true));
        s.close();
        c.store().set("armed", json!(false));
        assert!(matches!(
            s.next(Duration::from_millis(20)),
            crate::response::SseNext::Idle
        ));
    }

    #[test]
    fn serialisation_is_shared_per_version() {
        let (c, _, _) = core();
        let store = c.store();
        let snap = store.snapshot();
        let a = store.serialize_snapshot(&snap);
        assert!(Arc::ptr_eq(&a, &store.serialize_snapshot(&snap)));
        store.set("heading", json!(3.0));
        let stale = store.serialize_snapshot(&snap);
        assert!(!Arc::ptr_eq(&a, &stale) || store.version() == 0);
        let fresh = store.snapshot();
        let b = store.serialize_snapshot(&fresh);
        assert!(Arc::ptr_eq(&b, &store.serialize_snapshot(&fresh)));
        let v: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["heading"], json!(3.0));
    }

    #[test]
    fn events_need_a_known_topic() {
        let (c, _, _) = core();
        let r = c.dispatch(Request::new(Method::Get, "/api/events?topics=nothing,console"));
        assert_eq!(r.status, 400);
        assert_eq!(r.json_body(), Some(json!({"error": "no known topics requested"})));
        assert_eq!(c.dispatch(Request::new(Method::Get, "/api/events")).status, 400);
    }

    #[test]
    fn events_carry_the_topic_under_its_name_and_unbind_on_close() {
        let (c, unbound, slot) = core();
        let r = c.dispatch(Request::new(Method::Get, "/api/events?topics=bogus,fleet"));
        assert_eq!(r.status, 200);
        let mut s = stream_of(r);
        let first = s.open();
        assert_eq!(first[0].name, "fleet");
        assert_eq!(
            serde_json::from_slice::<Value>(&first[0].data).unwrap(),
            json!({"first": true})
        );
        slot.lock().unwrap().as_ref().unwrap().put_latest(json!(7));
        let events = collect(&mut *s, Duration::from_secs(2), 1);
        assert_eq!(
            serde_json::from_slice::<Value>(&events[0].data).unwrap(),
            json!({"shaped": 7})
        );
        assert!(!unbound.load(Ordering::SeqCst));
        s.close();
        assert!(unbound.load(Ordering::SeqCst));
    }

    #[test]
    fn a_stream_cannot_be_read_by_internal_call() {
        let (c, unbound, _) = core();
        let (status, _) = c.internal_call("GET", "/api/events?topics=fleet", None);
        assert_eq!(status, 400);
        assert!(unbound.load(Ordering::SeqCst));
    }

    #[test]
    fn a_job_stream_ends_on_a_terminal_state() {
        struct Job;
        impl Module for Job {
            fn name(&self) -> &'static str {
                "job"
            }
            fn register(&self, reg: &mut Registry) {
                reg.topic(Topic::new("tiles", 16, |_, sink, job| {
                    assert_eq!(job, Some("j1"));
                    sink.put_latest(json!({"state": "done"}));
                    Binding::none()
                }));
            }
        }
        let c = CoreBuilder::new(Options::default()).module(Job).build().unwrap();
        let mut s = c.topic_stream(&[("tiles", "progress")], Some("j1"), &["tiles"]);
        let events = collect(&mut *s, Duration::from_secs(2), 1);
        assert_eq!(events[0].name, "progress");
        assert!(matches!(
            s.next(Duration::from_millis(10)),
            crate::response::SseNext::End
        ));
    }
}
