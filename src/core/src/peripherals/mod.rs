//! The peripherals area of the core: serial port classification, the radios
//! (RTK base, SiK, ExpressLRS), the local shell, the SSH bridge and video.
//!
//! The module publishes [`Peripherals`] as a service and registers the routes
//! of `routes/rtk.py`, `routes/video.py`, `routes/console.py` and the parts of
//! `server.py` that belong to these programs (`/api/local/*`, `/api/ssh/*`,
//! `/api/sik/*`). Every thread, socket, pty and child it starts is ended in
//! `stop`, which the core calls on its shutdown path.

pub mod child_env;
pub mod net_probe;
pub mod ports;
pub mod rtk;
pub mod util;

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};

use crate::core::Core;
use crate::module::{Module, Registry};
use crate::request::Request;
use crate::response::Response;

use net_probe::Pinger;

/// What the module owns, reachable from every route as
/// `core.service::<Peripherals>()`.
pub struct Peripherals {
    pub pinger: Pinger,
    rtk: OnceLock<Arc<rtk::RtkService>>,
}

impl Peripherals {
    /// The RTK base station service, once the module has started.
    pub fn rtk(&self) -> Option<Arc<rtk::RtkService>> {
        self.rtk.get().cloned()
    }
}

pub struct PeripheralsModule {
    inner: Arc<Peripherals>,
}

impl PeripheralsModule {
    pub fn new() -> PeripheralsModule {
        PeripheralsModule {
            inner: Arc::new(Peripherals {
                pinger: Pinger::new(None),
                rtk: OnceLock::new(),
            }),
        }
    }
}

impl Default for PeripheralsModule {
    fn default() -> Self {
        PeripheralsModule::new()
    }
}

impl Module for PeripheralsModule {
    fn name(&self) -> &'static str {
        "peripherals"
    }

    fn register(&self, reg: &mut Registry) {
        reg.service(Arc::clone(&self.inner));
        reg.post("/api/local/ping", api_local_ping);
        rtk::register(reg);
    }

    fn start(&self, core: &Core) -> std::io::Result<()> {
        // RTK is the one service whose default is to act: it is built and, when
        // switched on in the config, started here, so a base plugged in before
        // the ground station opened is found without anybody asking.
        if let Some(vehicle) = core.service::<crate::vehicle::VehicleService>() {
            let settings = core
                .settings()
                .get()
                .get("rtk")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let timing = rtk::Timing::default();
            let service = Arc::new(rtk::RtkService::new(
                Arc::new(rtk::VehicleRtkHost { vehicle }),
                &settings,
                rtk::RtkIo::system(timing),
                timing,
            ));
            if service
                .settings()
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                service.start();
            }
            let _ = self.inner.rtk.set(service);
        }
        Ok(())
    }

    fn stop(&self, _core: &Core) {
        if let Some(rtk) = self.inner.rtk() {
            rtk.stop(3.0);
        }
        self.inner.pinger.shutdown();
    }
}

/// `_local_only`: the program runs on this computer or not at all.
fn local_only(req: &Request) -> Option<Response> {
    if req.is_loopback() {
        return None;
    }
    Some(Response::not_ok(
        403,
        "Programs on this computer can only be started from this computer.",
    ))
}

fn api_local_ping(core: &Core, req: &Request) -> Response {
    if let Some(refused) = local_only(req) {
        return refused;
    }
    let payload = req.payload();
    let host = payload.get("host");
    let wait_ms = payload.get("wait_ms").cloned().unwrap_or(json!(2000));
    let wait = match &wait_ms {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        _ => None,
    };
    let (Some(Value::String(host)), Some(wait)) = (host, wait) else {
        return Response::not_ok(400, "host must be a string and wait_ms a number");
    };
    let (_, problem) = net_probe::clean_host(host);
    if !problem.is_empty() {
        return Response::not_ok(400, &problem);
    }
    let Some(svc) = core.service::<Peripherals>() else {
        return Response::not_ok(503, "Pinging is unavailable in this build.");
    };
    Response::json(svc.pinger.ping(&Value::String(host.clone()), wait / 1000.0))
}
