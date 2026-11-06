//! How an area of the backend joins the core.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use crate::config::{Coercer, Coercers};
use crate::core::Core;
use crate::events::Topic;
use crate::request::{Method, Request};
use crate::response::Response;
use crate::router::{Body, Handler, Route, RouteOptions, Router};
use crate::settings::ConfigListener;

/// A refusal before a handler runs, for both HTTP and in-process calls
/// (`_fleet_refuses` in Python): `Some(response)` stops the request.
/// The plugin folders whose `lang/` files are merged into a language.
pub type LanguageRoots = Arc<dyn Fn(&Core) -> Vec<std::path::PathBuf> + Send + Sync>;

pub type Guard = Arc<dyn Fn(&Core, &Request) -> Option<Response> + Send + Sync>;

/// An area of the backend: the vehicle, the map data, the logs, the
/// peripherals, the AI client and the plugins each implement it in their own
/// folder and touch nothing else in the core.
///
/// `register` runs once while the core is built and only declares: routes,
/// guards, services. Threads and sockets belong in `start`, which runs in
/// registration order once everything is registered, and every one of them is
/// joined in `stop`, which runs in the reverse order on the way out. `stop`
/// must return promptly and may run when `start` did not.
pub trait Module: Send + Sync {
    fn name(&self) -> &'static str;

    fn register(&self, reg: &mut Registry);

    fn start(&self, _core: &Core) -> std::io::Result<()> {
        Ok(())
    }

    fn stop(&self, _core: &Core) {}
}

/// What a module declares while it registers.
pub struct Registry {
    pub(crate) router: Router,
    pub(crate) guards: Vec<Guard>,
    pub(crate) services: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
    pub(crate) topics: Vec<Topic>,
    pub(crate) coercers: Coercers,
    pub(crate) config_listeners: Vec<ConfigListener>,
    pub(crate) language_roots: Vec<LanguageRoots>,
    pub(crate) errors: Vec<String>,
    module: &'static str,
}

impl Registry {
    pub(crate) fn new() -> Registry {
        Registry {
            router: Router::default(),
            guards: Vec::new(),
            services: HashMap::new(),
            topics: Vec::new(),
            coercers: Coercers::default(),
            config_listeners: Vec::new(),
            language_roots: Vec::new(),
            errors: Vec::new(),
            module: "core",
        }
    }

    pub(crate) fn enter(&mut self, module: &'static str) {
        self.module = module;
    }

    fn add(&mut self, method: Method, path: &str, options: RouteOptions, handler: Handler) {
        if let Err(e) = self.router.add(method, path, Route { handler, options }) {
            self.errors.push(format!("{}: {e}", self.module));
        }
    }

    /// `GET path`.
    pub fn get<F>(&mut self, path: &str, f: F)
    where
        F: Fn(&Core, &Request) -> Response + Send + Sync + 'static,
    {
        self.add(Method::Get, path, RouteOptions::GET, Arc::new(f));
    }

    /// `POST path` with a JSON object body: `req.payload()` is that object.
    pub fn post<F>(&mut self, path: &str, f: F)
    where
        F: Fn(&Core, &Request) -> Response + Send + Sync + 'static,
    {
        self.add(Method::Post, path, RouteOptions::JSON, Arc::new(f));
    }

    /// A POST with its own cap or body kind (`RouteOptions::raw(n)` for an
    /// upload, `RouteOptions::json(n)` for a larger JSON body).
    pub fn post_with<F>(&mut self, path: &str, options: RouteOptions, f: F)
    where
        F: Fn(&Core, &Request) -> Response + Send + Sync + 'static,
    {
        debug_assert!(options.body != Body::None);
        self.add(Method::Post, path, options, Arc::new(f));
    }

    /// `DELETE path`, which only `/mcp` has.
    pub fn delete<F>(&mut self, path: &str, f: F)
    where
        F: Fn(&Core, &Request) -> Response + Send + Sync + 'static,
    {
        self.add(Method::Delete, path, RouteOptions::GET, Arc::new(f));
    }

    /// A GET on a pattern (`/api/tiles/{source}/{z:d}/{x:d}/{y:d}.png`); the
    /// captures are `req.param("source")` and so on.
    pub fn get_pattern<F>(&mut self, pattern: &str, f: F)
    where
        F: Fn(&Core, &Request) -> Response + Send + Sync + 'static,
    {
        let route = Route {
            handler: Arc::new(f),
            options: RouteOptions::GET,
        };
        if let Err(e) = self.router.add_pattern(Method::Get, pattern, route) {
            self.errors.push(format!("{}: {e}", self.module));
        }
    }

    /// A check that runs before every non-GET handler, over HTTP and in
    /// process alike. `Some(response)` answers instead of the handler.
    pub fn guard<F>(&mut self, f: F)
    where
        F: Fn(&Core, &Request) -> Option<Response> + Send + Sync + 'static,
    {
        self.guards.push(Arc::new(f));
    }

    /// An event topic for `/api/events?topics=NAME` (see `events::Topic`).
    pub fn topic(&mut self, topic: Topic) {
        if self.topics.iter().any(|t| t.name == topic.name) {
            self.errors
                .push(format!("{}: topic {} is registered twice", self.module, topic.name));
            return;
        }
        self.topics.push(topic);
    }

    /// The coercer of a block of the config file that belongs to this area
    /// (`battery`, `remote_id`, `rtk`, `video`, `checklists`, `fleet`,
    /// `mcp`): what `_coerce_battery` and its kin do. Until one is
    /// registered the block passes through as an object.
    pub fn config_block<F>(&mut self, key: &str, f: F)
    where
        F: Fn(&serde_json::Value) -> Option<crate::json::Map> + Send + Sync + 'static,
    {
        let c: Coercer = Arc::new(f);
        if !self.coercers.insert(key, c) {
            self.errors
                .push(format!("{}: config block {key} is registered twice", self.module));
        }
    }

    /// Called with the config after every change (see `Settings::update`).
    pub fn on_config<F>(&mut self, f: F)
    where
        F: Fn(&Core, &crate::config::CorvusConfig) + Send + Sync + 'static,
    {
        self.config_listeners.push(Arc::new(f));
    }

    /// Plugin folders whose `lang/<code>.json` files join a language, asked
    /// on each request.
    pub fn language_roots<F>(&mut self, f: F)
    where
        F: Fn(&Core) -> Vec<std::path::PathBuf> + Send + Sync + 'static,
    {
        self.language_roots.push(Arc::new(f));
    }

    /// Makes `value` reachable from every module as `core.service::<T>()`.
    pub fn service<T: Any + Send + Sync>(&mut self, value: Arc<T>) {
        if self.services.insert(TypeId::of::<T>(), value).is_some() {
            self.errors.push(format!(
                "{}: service {} is registered twice",
                self.module,
                std::any::type_name::<T>()
            ));
        }
    }
}
