//! RTK corrections (`rtk.py`, `rtk_service.py`, `routes/rtk.py`): the wire
//! formats, the settings, the correction sources and the base station session.

pub mod config;
pub mod io;
pub mod service;
pub mod wire;

mod routes;

pub use routes::register;
pub use service::{RtkHost, RtkIo, RtkService, Timing};

use std::sync::Arc;

use serde_json::Value;

use crate::fleet::MAIN_LINK;
use crate::vehicle::VehicleService;

/// The session's view of the vehicle side: the main link's bridge and store,
/// and the vehicle list that fans corrections out to every link.
pub struct VehicleRtkHost {
    pub vehicle: Arc<VehicleService>,
}

impl RtkHost for VehicleRtkHost {
    fn inject_rtcm(&self, frame: &[u8]) -> bool {
        self.vehicle.fleet.inject_rtcm(frame)
    }

    fn rtcm_stats(&self) -> Value {
        match self.vehicle.handles(MAIN_LINK) {
            Some(h) => h.bridge.rtcm_stats(),
            None => serde_json::json!({"bytes": 0, "messages": 0, "dropped": 0, "age": null}),
        }
    }

    fn forget_rtcm_session(&self) {
        if let Some(h) = self.vehicle.handles(MAIN_LINK) {
            h.bridge.forget_rtcm_session();
        }
    }

    fn connection_string(&self) -> String {
        self.vehicle
            .handles(MAIN_LINK)
            .map(|h| h.bridge.connection_string().to_string())
            .unwrap_or_default()
    }

    fn is_connected(&self) -> bool {
        self.vehicle.fleet.is_connected()
    }

    fn store_update(&self, state: &str, source: &str, accuracy: f64, progress: i64) {
        if let Some(h) = self.vehicle.handles(MAIN_LINK) {
            h.store.update([
                ("rtk_state", Value::from(state)),
                ("rtk_source", Value::from(source)),
                ("rtk_accuracy", Value::from(accuracy)),
                ("rtk_progress", Value::from(progress)),
            ]);
        }
    }

    fn vehicle(&self) -> (Value, Value, Value) {
        match self.vehicle.handles(MAIN_LINK) {
            Some(h) => {
                let s = h.store.snapshot();
                (
                    s.get("gps_fix").cloned().unwrap_or(Value::from("")),
                    s.get("gps_satellites").cloned().unwrap_or(Value::from(0)),
                    s.get("gps_hdop").cloned().unwrap_or(Value::from(0)),
                )
            }
            None => (Value::from(""), Value::from(0), Value::from(0)),
        }
    }
}
