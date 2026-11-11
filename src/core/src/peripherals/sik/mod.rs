//! SiK telemetry radios (`sik_config.py`, `sik_service.py`, and the radio
//! endpoints of `server.py`): the schema and the AT dialect, and the sessions
//! that borrow a serial port.

pub mod config;
mod routes;
pub mod service;

pub use routes::register;
pub use service::{SikFailure, SikHost, SikService, SikTiming};

use std::sync::Arc;

use crate::fleet::MAIN_LINK;
use crate::peripherals::ports::{list_serial_ports, PortRow};
use crate::protocol::common::py_truthy;
use crate::vehicle::VehicleService;

/// The service's view of the vehicle side: the main link's bridge and store.
pub struct VehicleSikHost {
    pub vehicle: Arc<VehicleService>,
}

impl SikHost for VehicleSikHost {
    fn armed(&self) -> bool {
        self.vehicle
            .handles(MAIN_LINK)
            .is_some_and(|h| h.store.snapshot().get("armed").is_some_and(py_truthy))
    }

    fn connection_string(&self) -> String {
        self.vehicle
            .handles(MAIN_LINK)
            .map(|h| h.bridge.connection_string().to_string())
            .unwrap_or_default()
    }

    /// The bridge does not say whether its cycle is live, only whether it is
    /// connected: a link that is still connecting is left as it is.
    fn link_running(&self) -> bool {
        self.vehicle.handles(MAIN_LINK).is_some_and(|h| h.bridge.is_connected())
    }

    fn stop_link(&self) {
        if let Some(h) = self.vehicle.handles(MAIN_LINK) {
            h.bridge.stop();
        }
    }

    /// The bridge keeps the connection string it was made with, so it can come
    /// back on the same one only; a new baud needs `Bridge::set_connection`.
    fn restart_link(&self, conn: &str) -> Result<(), String> {
        let Some(h) = self.vehicle.handles(MAIN_LINK) else {
            return Err("there is no link".into());
        };
        if h.bridge.connection_string() != conn {
            return Err(format!("the link cannot be moved to {conn} yet"));
        }
        h.bridge.start();
        Ok(())
    }

    fn list_ports(&self) -> Vec<PortRow> {
        list_serial_ports()
    }
}
