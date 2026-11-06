//! The core: the route table, the services modules share, the lifecycle.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::events::Topic;
use crate::json::{self, Map};
use crate::module::{Guard, Module, Registry};
use crate::request::{Method, Origin, Request};
use crate::response::{Body as ResponseBody, Response};
use crate::router::{self, Body, Router};
use crate::unported;

/// How a core is built.
#[derive(Debug, Clone)]
pub struct Options {
    /// The state folder (`~/.corvus`).
    pub paths: crate::paths::Paths,
    /// The config file; `config.json` in the state folder when `None`.
    pub config_path: Option<std::path::PathBuf>,
    /// Where languages are read from; the compiled-in ones and
    /// `~/.corvus/languages` when `None`.
    pub languages: Option<crate::languages::Sources>,
    /// The folder the static files of the web interface are served from, if any.
    pub web_root: Option<std::path::PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            paths: crate::paths::Paths::system(),
            config_path: None,
            languages: None,
            web_root: None,
        }
    }
}

impl Options {
    /// Everything under `dir`, for tests and a second instance.
    pub fn in_dir(dir: impl Into<std::path::PathBuf>) -> Options {
        Options {
            paths: crate::paths::Paths::at(dir),
            config_path: None,
            languages: None,
            web_root: None,
        }
    }
}

/// Collects the modules, then builds the core.
pub struct CoreBuilder {
    options: Options,
    modules: Vec<Arc<dyn Module>>,
}

impl CoreBuilder {
    pub fn new(options: Options) -> CoreBuilder {
        CoreBuilder {
            options,
            modules: Vec::new(),
        }
    }

    /// Adds an area. Modules start in the order they are added.
    pub fn module<M: Module + 'static>(mut self, module: M) -> CoreBuilder {
        self.modules.push(Arc::new(module));
        self
    }

    /// Registers everything. Fails, naming the module, when two modules claim
    /// the same route or service.
    pub fn build(self) -> Result<Core, String> {
        let mut reg = Registry::new();
        let mut modules: Vec<Arc<dyn Module>> =
            vec![Arc::new(crate::foundation::Foundation::new(self.options.clone()))];
        modules.extend(self.modules);
        for m in &modules {
            reg.enter(m.name());
            m.register(&mut reg);
        }
        if !reg.errors.is_empty() {
            return Err(reg.errors.join("; "));
        }
        unported::add_stubs(&mut reg.router)?;
        let Registry {
            router,
            guards,
            services,
            topics,
            coercers,
            config_listeners,
            language_roots,
            ..
        } = reg;
        let core = Core(Arc::new(Inner {
            options: self.options,
            router,
            guards,
            topics,
            modules,
            services: RwLock::new(services),
            started: Mutex::new(0),
            stop: Stop::default(),
            language_roots,
        }));
        let settings = core.settings();
        settings.set_coercers(coercers);
        settings.set_listeners(config_listeners);
        Ok(core)
    }
}

#[derive(Default)]
struct Stop {
    stopping: Mutex<bool>,
    changed: Condvar,
}

struct Inner {
    #[allow(dead_code)]
    options: Options,
    router: Router,
    guards: Vec<Guard>,
    topics: Vec<Topic>,
    modules: Vec<Arc<dyn Module>>,
    services: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    started: Mutex<usize>,
    stop: Stop,
    language_roots: Vec<crate::module::LanguageRoots>,
}

/// The backend. Cheap to clone; every clone is the same core.
#[derive(Clone)]
pub struct Core(Arc<Inner>);

impl Core {
    /// Starts the modules in order. When one fails to start, those started
    /// before it are stopped again and the error is returned.
    pub fn start(&self) -> std::io::Result<()> {
        let mut started = self.0.started.lock().unwrap_or_else(|e| e.into_inner());
        for module in &self.0.modules[*started..] {
            if let Err(e) = module.start(self) {
                let n = *started;
                drop(started);
                self.stop_modules(n);
                return Err(std::io::Error::new(e.kind(), format!("{}: {e}", module.name())));
            }
            *started += 1;
        }
        Ok(())
    }

    /// Ends the core: streams and waits return, then every module that
    /// started is stopped in reverse order, which joins its threads. Safe to
    /// call more than once.
    pub fn stop(&self) {
        {
            let mut stopping = self.0.stop.stopping.lock().unwrap_or_else(|e| e.into_inner());
            *stopping = true;
            self.0.stop.changed.notify_all();
        }
        let n = std::mem::take(&mut *self.0.started.lock().unwrap_or_else(|e| e.into_inner()));
        self.stop_modules(n);
    }

    fn stop_modules(&self, n: usize) {
        for module in self.0.modules[..n].iter().rev() {
            let _ = catch_unwind(AssertUnwindSafe(|| module.stop(self)));
        }
    }

    /// True once `stop` was called. Every loop that waits checks it.
    pub fn is_stopping(&self) -> bool {
        *self.0.stop.stopping.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits up to `timeout`, or until the core stops. Returns whether it is
    /// stopping: a thread's sleep that must end on shutdown.
    pub fn wait_stopping(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut stopping = self.0.stop.stopping.lock().unwrap_or_else(|e| e.into_inner());
        while !*stopping {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            stopping = self
                .0
                .stop
                .changed
                .wait_timeout(stopping, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *stopping
    }

    /// A service a module registered.
    pub fn service<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        let map = self.0.services.read().unwrap_or_else(|e| e.into_inner());
        map.get(&TypeId::of::<T>())
            .cloned()
            .and_then(|a| a.downcast::<T>().ok())
    }

    /// Registers a service after the build, for one made in `start`.
    /// Returns false when there already is one.
    pub fn insert_service<T: Any + Send + Sync>(&self, value: Arc<T>) -> bool {
        let mut map = self.0.services.write().unwrap_or_else(|e| e.into_inner());
        map.insert(TypeId::of::<T>(), value).is_none()
    }

    /// The event topic named `name`.
    pub fn topic(&self, name: &str) -> Option<&Topic> {
        self.0.topics.iter().find(|t| t.name == name)
    }

    pub fn topic_names(&self) -> Vec<&'static str> {
        self.0.topics.iter().map(|t| t.name).collect()
    }

    /// The state store of the vehicle being flown.
    pub fn store(&self) -> Arc<crate::store::VehicleStateStore> {
        self.service::<crate::hub::StoreHub>()
            .expect("the foundation registers the store hub")
            .current()
    }

    /// The configuration in force and the one place that changes it.
    pub fn settings(&self) -> Arc<crate::settings::Settings> {
        self.service::<crate::settings::Settings>()
            .expect("the foundation registers the settings")
    }

    pub fn paths(&self) -> &crate::paths::Paths {
        &self.0.options.paths
    }

    /// The plugin folders whose `lang/` files are merged into a language.
    pub fn language_roots(&self) -> Vec<std::path::PathBuf> {
        self.0.language_roots.iter().flat_map(|f| f(self)).collect()
    }

    pub fn options(&self) -> &Options {
        &self.0.options
    }

    pub fn router(&self) -> &Router {
        &self.0.router
    }

    /// Runs one request: finds the route, reads a JSON body, applies the
    /// guards, calls the handler. A panic in the handler is a 500. Both the
    /// HTTP server and `internal_call` go through here.
    pub fn dispatch(&self, mut req: Request) -> Response {
        let method = req.method;
        let Some(found) = self.0.router.find(method, &req.path) else {
            return self.not_found(&req);
        };
        let route = found.route.clone();
        router::apply_params(&mut req, found.params);
        if route.options.body == Body::Json && req.origin == Origin::Http {
            match json::object_from_body(&req.body) {
                Ok(map) => req.set_payload(map),
                Err(e) => return Response::error(400, e.message()),
            }
        }
        if method != Method::Get {
            for guard in &self.0.guards {
                if let Some(refusal) = guard(self, &req) {
                    return refusal;
                }
            }
        }
        match catch_unwind(AssertUnwindSafe(|| (route.handler)(self, &req))) {
            Ok(response) => response,
            Err(_) => Response::error(500, "internal server error"),
        }
    }

    fn not_found(&self, req: &Request) -> Response {
        if self.0.router.is_unavailable(req.method, &req.path) {
            return Response::unavailable();
        }
        match req.method {
            Method::Delete => Response::error(405, "method not allowed"),
            _ => Response::error(404, "not found"),
        }
    }

    /// Runs an endpoint in this process and returns `(status, body)`, the way
    /// `internal_call` in `routes/mcp.py` does: through the very handler a
    /// button calls, with its validation, its refusals and the guards, never
    /// around it. A tool of an AI client and the interface both use this.
    ///
    /// `method` is `GET`, `POST` or `DELETE`; `path` may carry a query. A body
    /// that is not an object comes back as `{"value": ...}`.
    pub fn internal_call(&self, method: &str, path: &str, payload: Option<Map>) -> (u16, Value) {
        let target_path = path.split(['?', '#']).next().unwrap_or("");
        let Some(m) = Method::parse(method) else {
            return (404, json::not_ok(&format!("no endpoint {target_path}")));
        };
        if self.0.router.find(m, target_path).is_none() && !self.0.router.is_unavailable(m, target_path) {
            return (404, json::not_ok(&format!("no endpoint {target_path}")));
        }
        let response = self.dispatch(Request::internal(m, path, payload.unwrap_or_default()));
        let status = response.status;
        match response.body {
            ResponseBody::Bytes(bytes) => {
                let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
                match value {
                    Value::Object(_) => (status, value),
                    other => {
                        let mut m = Map::new();
                        m.insert("value".into(), other);
                        (status, Value::Object(m))
                    }
                }
            }
            ResponseBody::Sse(mut stream) => {
                stream.close();
                (400, json::not_ok("a stream cannot be read in process"))
            }
            ResponseBody::Empty => (500, json::not_ok("the endpoint gave no answer")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::Registry;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Echo;

    impl Module for Echo {
        fn name(&self) -> &'static str {
            "echo"
        }

        fn register(&self, reg: &mut Registry) {
            reg.get("/api/echo", |_, req| Response::json(json!({"q": req.query.get("q")})));
            reg.post("/api/echo", |_, req| {
                Response::json(Value::Object(req.payload().clone()))
            });
            reg.post("/api/boom", |_, _| panic!("a handler bug"));
            reg.post("/api/list", |_, _| Response::json(json!([1, 2])));
            reg.get_pattern("/api/item/{id:d}", |_, req| {
                Response::json(json!({"id": req.param("id")}))
            });
            reg.guard(|_, req| (req.path == "/api/locked").then(|| Response::not_ok(409, "locked")));
            reg.post("/api/locked", |_, _| Response::json(json!({"ok": true})));
            reg.service(Arc::new(AtomicUsize::new(7)));
        }
    }

    fn core() -> Core {
        CoreBuilder::new(Options::in_dir(std::env::temp_dir().join("corvus-core-test-state")))
            .module(Echo)
            .build()
            .unwrap()
    }

    #[test]
    fn internal_call_runs_the_handler_with_a_payload() {
        let c = core();
        let mut p = Map::new();
        p.insert("a".into(), json!(1));
        let (status, body) = c.internal_call("POST", "/api/echo", Some(p));
        assert_eq!((status, body), (200, json!({"a": 1})));
        let (status, body) = c.internal_call("GET", "/api/echo?q=x", None);
        assert_eq!((status, body), (200, json!({"q": "x"})));
    }

    #[test]
    fn internal_call_wraps_non_objects_and_reports_missing_endpoints() {
        let c = core();
        assert_eq!(
            c.internal_call("POST", "/api/list", None),
            (200, json!({"value": [1, 2]}))
        );
        let (status, body) = c.internal_call("GET", "/api/nope", None);
        assert_eq!(status, 404);
        assert_eq!(body, json!({"ok": false, "error": "no endpoint /api/nope"}));
        assert_eq!(c.internal_call("PUT", "/api/echo", None).0, 404);
    }

    #[test]
    fn a_panic_is_a_500_and_the_core_lives_on() {
        let c = core();
        assert_eq!(
            c.internal_call("POST", "/api/boom", None),
            (500, json!({"error": "internal server error"}))
        );
        assert_eq!(c.internal_call("GET", "/api/echo", None).0, 200);
    }

    #[test]
    fn guards_refuse_in_process_calls_too() {
        let c = core();
        assert_eq!(
            c.internal_call("POST", "/api/locked", None),
            (409, json!({"ok": false, "error": "locked"}))
        );
    }

    #[test]
    fn unported_routes_are_501_not_404() {
        let c = core();
        let (status, body) = c.internal_call("POST", "/api/mavlink/arm", None);
        assert_eq!(status, 501);
        assert_eq!(body, json!({"error": "Not available in this build"}));
        assert_eq!(c.internal_call("GET", "/api/tiles/esri/1/2/3.png", None).0, 501);
        assert_eq!(c.dispatch(Request::new(Method::Get, "/api/nowhere")).status, 404);
        assert_eq!(c.dispatch(Request::new(Method::Delete, "/api/x")).status, 405);
    }

    #[test]
    fn http_bodies_are_parsed_by_the_router() {
        let c = core();
        let mut req = Request::new(Method::Post, "/api/echo");
        req.body = br#"{"k": "v"}"#.to_vec();
        assert_eq!(c.dispatch(req).json_body(), Some(json!({"k": "v"})));
        let mut bad = Request::new(Method::Post, "/api/echo");
        bad.body = b"[1]".to_vec();
        let r = bad.clone();
        assert_eq!(c.dispatch(r).status, 400);
        assert_eq!(
            c.dispatch(bad).json_body(),
            Some(json!({"error": "json payload must be an object"}))
        );
        let mut garbage = Request::new(Method::Post, "/api/echo");
        garbage.body = b"{".to_vec();
        assert_eq!(c.dispatch(garbage).json_body(), Some(json!({"error": "invalid json"})));
    }

    #[test]
    fn patterns_hand_out_their_captures() {
        let c = core();
        assert_eq!(c.internal_call("GET", "/api/item/42", None), (200, json!({"id": "42"})));
    }

    #[test]
    fn services_are_shared() {
        let c = core();
        assert_eq!(c.service::<AtomicUsize>().unwrap().load(Ordering::SeqCst), 7);
        assert!(c.service::<String>().is_none());
        assert!(c.insert_service(Arc::new(String::from("late"))));
        assert!(!c.insert_service(Arc::new(String::from("again"))));
    }

    #[test]
    fn two_modules_cannot_claim_a_route() {
        struct Twin;
        impl Module for Twin {
            fn name(&self) -> &'static str {
                "twin"
            }
            fn register(&self, reg: &mut Registry) {
                reg.get("/api/echo", |_, _| Response::empty(200));
            }
        }
        let err = CoreBuilder::new(Options::in_dir(std::env::temp_dir().join("corvus-core-test-state")))
            .module(Echo)
            .module(Twin)
            .build()
            .err()
            .unwrap();
        assert!(err.contains("twin: GET /api/echo is registered twice"), "{err}");
    }

    struct Order(&'static str, Arc<Mutex<Vec<String>>>, bool);

    impl Module for Order {
        fn name(&self) -> &'static str {
            self.0
        }
        fn register(&self, _: &mut Registry) {}
        fn start(&self, _: &Core) -> std::io::Result<()> {
            self.1.lock().unwrap().push(format!("start {}", self.0));
            if self.2 {
                return Err(std::io::Error::other("no port"));
            }
            Ok(())
        }
        fn stop(&self, _: &Core) {
            self.1.lock().unwrap().push(format!("stop {}", self.0));
        }
    }

    #[test]
    fn modules_start_in_order_and_stop_in_reverse() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let c = CoreBuilder::new(Options::in_dir(std::env::temp_dir().join("corvus-core-test-state")))
            .module(Order("a", log.clone(), false))
            .module(Order("b", log.clone(), false))
            .build()
            .unwrap();
        c.start().unwrap();
        assert!(!c.is_stopping());
        c.stop();
        c.stop();
        assert!(c.is_stopping());
        assert_eq!(*log.lock().unwrap(), ["start a", "start b", "stop b", "stop a"]);
    }

    #[test]
    fn a_failed_start_unwinds_the_ones_before_it() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let c = CoreBuilder::new(Options::in_dir(std::env::temp_dir().join("corvus-core-test-state")))
            .module(Order("a", log.clone(), false))
            .module(Order("b", log.clone(), true))
            .build()
            .unwrap();
        let err = c.start().unwrap_err();
        assert!(err.to_string().contains("b: no port"));
        assert_eq!(*log.lock().unwrap(), ["start a", "start b", "stop a"]);
    }

    #[test]
    fn wait_stopping_ends_on_stop() {
        let c = core();
        let c2 = c.clone();
        let t = std::thread::spawn(move || c2.wait_stopping(Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(50));
        let began = Instant::now();
        c.stop();
        assert!(t.join().unwrap());
        assert!(began.elapsed() < Duration::from_secs(5));
        assert!(!core().wait_stopping(Duration::from_millis(10)));
    }
}
