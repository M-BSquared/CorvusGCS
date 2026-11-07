//! The module that is always there: the routes the foundation owns.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::config::is_valid_language_code;
use crate::core::{Core, Options};
use crate::events::TelemetryStream;
use crate::hub::StoreHub;
use crate::languages::{self, Sources};
use crate::lock::{describe_peer, InstanceLock};
use crate::module::{Module, Registry};
use crate::request::Request;
use crate::response::Response;
use crate::settings::Settings;
use crate::store::VehicleStateStore;

pub(crate) struct Foundation {
    options: Options,
}

impl Foundation {
    pub(crate) fn new(options: Options) -> Foundation {
        Foundation { options }
    }
}

/// Holds the instance lock for the life of the core.
pub struct Instance {
    lock: Mutex<InstanceLock>,
}

impl Module for Foundation {
    fn name(&self) -> &'static str {
        "foundation"
    }

    fn register(&self, reg: &mut Registry) {
        let o = &self.options;
        let config_path = o.config_path.clone().unwrap_or_else(|| o.paths.config());
        reg.service(Arc::new(StoreHub::new(Arc::new(VehicleStateStore::new()))));
        reg.service(Arc::new(Settings::new(config_path)));
        reg.service(Arc::new(Instance {
            lock: Mutex::new(InstanceLock::new(o.paths.lock())),
        }));
        reg.get("/api/version", api_version);
        reg.get("/api/state", api_state);
        reg.get("/api/telemetry", api_telemetry);
        reg.get("/api/events", api_events);
        reg.get("/api/config", api_config);
        reg.post("/api/config", api_config_update);
        reg.get("/api/welcome", api_welcome);
        reg.post("/api/welcome", api_welcome_done);
        reg.get("/api/languages", api_languages);
        reg.get("/api/languages/strings", api_language_strings);
        let plugins = o.paths.join("plugins");
        reg.language_roots(move |_| vec![plugins.clone()]);
    }

    fn start(&self, core: &Core) -> std::io::Result<()> {
        core.settings().load();
        Ok(())
    }

    fn stop(&self, core: &Core) {
        if let Some(i) = core.service::<Instance>() {
            i.lock.lock().unwrap_or_else(|e| e.into_inner()).release();
        }
        core.store().shutdown();
    }
}

impl Core {
    /// Takes the single instance lock. `Err` is the sentence to tell the
    /// operator when another Corvus holds it and `CORVUS_ALLOW_MULTI` is not
    /// set; with it set, a second instance runs without holding the lock.
    pub fn claim_instance(&self) -> Result<(), String> {
        let inst = self
            .service::<Instance>()
            .expect("the foundation registers the instance lock");
        let mut lock = inst.lock.lock().unwrap_or_else(|e| e.into_inner());
        if lock.acquire(None) || crate::lock::allow_multi() {
            return Ok(());
        }
        Err(format!(
            "CORVUS GCS is {}. Open that one, or set {} = 1 to run a second instance.",
            describe_peer(&lock.peer()),
            crate::lock::ALLOW_MULTI_ENV
        ))
    }

    /// Writes the port the HTTP server listens on into the lock file, for the
    /// next launch to tell the operator.
    pub fn advertise_port(&self, port: u16) {
        if let Some(inst) = self.service::<Instance>() {
            inst.lock
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .write_port(Some(port));
        }
    }
}

fn api_version(core: &Core, _req: &Request) -> Response {
    let profile = core.store().get("px4_version");
    let profile = profile
        .as_ref()
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("undetected")
        .to_string();
    Response::json(json!({
        "product": crate::version::product(),
        "version": crate::version::version(),
        "px4_profile": profile,
    }))
}

fn api_config(core: &Core, _req: &Request) -> Response {
    Response::json(json!({"config": core.settings().get().to_public()}))
}

fn api_config_update(core: &Core, req: &Request) -> Response {
    match core.settings().apply_partial(core, req.payload()) {
        Ok(public) => Response::json(json!({"ok": true, "config": public})),
        Err(e) => Response::error(400, &e),
    }
}

fn api_welcome(core: &Core, _req: &Request) -> Response {
    Response::json(json!({"pending": core.settings().first_start_pending()}))
}

fn api_welcome_done(core: &Core, _req: &Request) -> Response {
    let settings = core.settings();
    settings.finish_first_start();
    let saved = settings.save().is_ok();
    Response::json(json!({"ok": true, "saved": saved}))
}

fn language_sources(core: &Core) -> Sources {
    match &core.options().languages {
        Some(s) => s.clone(),
        None => Sources {
            user_dir: Some(core.paths().languages()),
            bundled_dir: None,
        },
    }
}

fn api_languages(core: &Core, _req: &Request) -> Response {
    let sources = language_sources(core);
    if core.options().languages.is_none() {
        languages::ensure_user_dir(&core.paths().languages());
    }
    Response::json(json!({
        "languages": languages::discover(&sources),
        "default": languages::DEFAULT_CODE,
        "user_dir": sources.user_dir.map(|p| p.to_string_lossy().into_owned()),
    }))
}

fn api_language_strings(core: &Core, req: &Request) -> Response {
    let code = req.query.get("code").unwrap_or("");
    if !is_valid_language_code(code) {
        return Response::error(400, "code must be a language code such as de or pt-BR");
    }
    match languages::load(code, &language_sources(core), &core.language_roots()) {
        None => Response::error(404, &format!("language {code} is not installed")),
        Some(l) => Response::json(json!({
            "code": l.code, "name": l.name, "native_name": l.native_name,
            "source": l.source, "strings": l.strings,
        })),
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
        let c = CoreBuilder::new(Options::in_dir(std::env::temp_dir().join("corvus-core-test-state")))
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
        let c = CoreBuilder::new(Options::in_dir(std::env::temp_dir().join("corvus-core-test-state")))
            .module(Job)
            .build()
            .unwrap();
        let mut s = c.topic_stream(&[("tiles", "progress")], Some("j1"), &["tiles"]);
        let events = collect(&mut *s, Duration::from_secs(2), 1);
        assert_eq!(events[0].name, "progress");
        assert!(matches!(
            s.next(Duration::from_millis(10)),
            crate::response::SseNext::End
        ));
    }

    fn state_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("corvus-core-found-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn started(dir: &std::path::Path) -> Core {
        let c = CoreBuilder::new(Options::in_dir(dir)).build().unwrap();
        c.start().unwrap();
        c
    }

    #[test]
    fn version_is_the_version_file_and_the_firmware() {
        let c = started(&state_dir("version"));
        let (status, body) = c.internal_call("GET", "/api/version", None);
        assert_eq!(status, 200);
        assert_eq!(body["product"], json!("Corvus GCS"));
        assert_eq!(body["version"], json!(include_str!("../../../VERSION").trim()));
        assert_eq!(body["px4_profile"], json!("undetected"));
        c.store().set("px4_version", json!("1.16.0"));
        assert_eq!(
            c.internal_call("GET", "/api/version", None).1["px4_profile"],
            json!("1.16.0")
        );
        c.stop();
    }

    #[test]
    fn config_is_read_changed_saved_and_read_again() {
        let dir = state_dir("config");
        let c = started(&dir);
        let (_, body) = c.internal_call("GET", "/api/config", None);
        assert_eq!(body["config"]["http_port"], json!(8000));
        let mut p = crate::json::Map::new();
        p.insert("http_port".into(), json!("9001"));
        p.insert("ui".into(), json!({"scale": 2}));
        let (status, body) = c.internal_call("POST", "/api/config", Some(p));
        assert_eq!(status, 200);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["config"]["http_port"], json!(9001));
        let mut bad = crate::json::Map::new();
        bad.insert("http_port".into(), json!(70000));
        let (status, body) = c.internal_call("POST", "/api/config", Some(bad));
        assert_eq!(
            (status, &body),
            (400, &json!({"error": "http_port must be between 0 and 65535"}))
        );
        c.stop();
        let again = started(&dir);
        let (_, body) = again.internal_call("GET", "/api/config", None);
        assert_eq!(body["config"]["http_port"], json!(9001));
        assert_eq!(body["config"]["ui"]["scale"], json!(2.0));
        again.stop();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_config_listener_hears_every_change() {
        struct Watch(Arc<Mutex<Vec<i64>>>);
        impl Module for Watch {
            fn name(&self) -> &'static str {
                "watch"
            }
            fn register(&self, reg: &mut Registry) {
                let seen = self.0.clone();
                reg.on_config(move |_, cfg| seen.lock().unwrap().push(cfg.http_port()));
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let dir = state_dir("listener");
        let c = CoreBuilder::new(Options::in_dir(&dir))
            .module(Watch(seen.clone()))
            .build()
            .unwrap();
        c.start().unwrap();
        c.settings()
            .update(&c, |cfg| cfg.set("http_port", Some(json!(8100))))
            .unwrap();
        let mut p = crate::json::Map::new();
        p.insert("http_port".into(), json!(8200));
        c.internal_call("POST", "/api/config", Some(p));
        assert_eq!(*seen.lock().unwrap(), [8100, 8200]);
        c.stop();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_first_start_is_pending_until_it_is_finished() {
        let dir = state_dir("welcome");
        let c = started(&dir);
        assert_eq!(c.internal_call("GET", "/api/welcome", None).1, json!({"pending": true}));
        assert_eq!(
            c.internal_call("POST", "/api/welcome", None).1,
            json!({"ok": true, "saved": true})
        );
        assert_eq!(
            c.internal_call("GET", "/api/welcome", None).1,
            json!({"pending": false})
        );
        c.stop();
        let again = started(&dir);
        assert_eq!(
            again.internal_call("GET", "/api/welcome", None).1,
            json!({"pending": false})
        );
        again.stop();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn languages_list_and_serve_their_strings() {
        let dir = state_dir("languages");
        std::fs::create_dir_all(dir.join("languages")).unwrap();
        std::fs::write(
            dir.join("languages/es.json"),
            r#"{"meta": {"name": "Spanish"}, "strings": {"Settings": "Ajustes"}}"#,
        )
        .unwrap();
        let c = started(&dir);
        let (status, body) = c.internal_call("GET", "/api/languages", None);
        assert_eq!(status, 200);
        let codes: Vec<&str> = body["languages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|l| l["code"].as_str())
            .collect();
        assert_eq!(codes[0], "en");
        assert!(codes.contains(&"es") && codes.contains(&"de"));
        assert_eq!(body["default"], json!("en"));
        assert_eq!(body["user_dir"], json!(dir.join("languages").to_string_lossy()));
        assert!(dir.join("languages/README.md").is_file());
        let (_, es) = c.internal_call("GET", "/api/languages/strings?code=es", None);
        assert_eq!(es["strings"]["Settings"], json!("Ajustes"));
        assert_eq!(es["source"], json!("user"));
        assert_eq!(c.internal_call("GET", "/api/languages/strings?code=de", None).0, 200);
        assert_eq!(c.internal_call("GET", "/api/languages/strings?code=xx", None).0, 404);
        let (status, body) = c.internal_call("GET", "/api/languages/strings?code=../x", None);
        assert_eq!(
            (status, &body["error"]),
            (400, &json!("code must be a language code such as de or pt-BR"))
        );
        c.stop();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_second_station_on_the_same_folder_is_turned_away() {
        let dir = state_dir("instance");
        let first = started(&dir);
        first.claim_instance().unwrap();
        first.advertise_port(8765);
        let second = started(&dir);
        let err = second.claim_instance().unwrap_err();
        assert!(err.starts_with("CORVUS GCS is already running"), "{err}");
        assert!(
            err.contains("http://localhost:8765/") && err.contains("CORVUS_ALLOW_MULTI"),
            "{err}"
        );
        first.stop();
        second.claim_instance().unwrap();
        second.stop();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
