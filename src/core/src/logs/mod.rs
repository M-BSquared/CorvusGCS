//! The logs area of the core: the ULog reader, the telemetry recording
//! reader, both reviews, the log service and the routes of `routes/logs.py`.
//!
//! [`LogsModule`] publishes [`Logs`] as a service and registers the routes of
//! `routes/logs.py` and the log review upload of `server.py`. The log worker
//! thread belongs to [`service::LogService`] and is joined in `stop`.

pub mod flight_review;
pub mod link;
pub mod localtime;
pub mod pytext;
mod routes;
pub mod service;
pub mod tlog_review;
pub mod ulog;

use std::sync::{Arc, OnceLock};

use crate::core::Core;
use crate::module::{Module, Registry};

pub use service::LogService;

/// What the module owns, reachable from every route as
/// `core.service::<Logs>()`.
#[derive(Default)]
pub struct Logs {
    service: OnceLock<Arc<LogService>>,
}

impl Logs {
    /// The log service, once the module has started with a vehicle to talk to.
    pub fn service(&self) -> Option<Arc<LogService>> {
        self.service.get().cloned()
    }

    /// Holds a service built elsewhere (a test with a fake link).
    pub fn set_service(&self, service: Arc<LogService>) -> bool {
        self.service.set(service).is_ok()
    }
}

pub struct LogsModule {
    inner: Arc<Logs>,
}

impl LogsModule {
    pub fn new() -> LogsModule {
        LogsModule {
            inner: Arc::new(Logs::default()),
        }
    }
}

impl Default for LogsModule {
    fn default() -> Self {
        LogsModule::new()
    }
}

impl Module for LogsModule {
    fn name(&self) -> &'static str {
        "logs"
    }

    fn register(&self, reg: &mut Registry) {
        reg.service(Arc::clone(&self.inner));
        routes::register(reg);
    }

    fn start(&self, core: &Core) -> std::io::Result<()> {
        if self.inner.service.get().is_some() {
            return Ok(());
        }
        let Some(vehicle) = core.service::<crate::vehicle::VehicleService>() else {
            return Ok(());
        };
        let Some(main) = vehicle.handles(crate::fleet::MAIN_LINK) else {
            return Ok(());
        };
        let settings = core.settings();
        let paths = core.paths().clone();
        let dir_of = move |key: &'static str, fallback: &'static str| {
            let (settings, paths) = (Arc::clone(&settings), paths.clone());
            Arc::new(move || {
                let cfg = settings.get();
                let configured = cfg.str_field(key).trim();
                if configured.is_empty() {
                    paths.join(fallback).to_string_lossy().into_owned()
                } else {
                    crate::mapdata::util::expand_user(configured)
                }
            }) as Arc<dyn Fn() -> String + Send + Sync>
        };
        let service = LogService::new(
            Arc::new(link::BridgeLink::new(main.bridge)),
            dir_of("log_download_dir", "flightlogs"),
            Some(dir_of("tlog_dir", "logs")),
        );
        let _ = self.inner.service.set(service);
        Ok(())
    }

    fn stop(&self, _core: &Core) {
        if let Some(service) = self.inner.service() {
            service.shutdown();
        }
    }
}
