//! Firmware catalogue, downloads and flashing (`firmware_catalog.py`,
//! `ardupilot_firmware.py`, `firmware_uploader.py`, `flash_service.py`,
//! `routes/firmware.py`).
//!
//! [`FirmwareModule`] publishes [`Firmware`] as a service and registers the
//! routes and the `firmware` event topic. The flash worker is joined in `stop`.

pub mod ardupilot;
pub mod catalog;
pub mod flash;
pub mod host;
pub mod image;
pub mod net;
pub mod pyjson;
mod routes;
pub mod tables;
pub mod uploader;
pub mod util;

use std::sync::{Arc, OnceLock};

use crate::core::Core;
use crate::module::{Module, Registry};

pub use catalog::FirmwareCatalog;
pub use flash::FlashService;

/// What the module owns, reachable from every route as
/// `core.service::<Firmware>()`.
#[derive(Default)]
pub struct Firmware {
    flash: OnceLock<Arc<FlashService>>,
    catalog: OnceLock<Arc<FirmwareCatalog>>,
}

impl Firmware {
    /// The flash service, once the module has started with a vehicle to flash.
    pub fn flash(&self) -> Option<Arc<FlashService>> {
        self.flash.get().cloned()
    }

    /// The firmware catalogue, with its image cache.
    pub fn catalog(&self) -> Option<Arc<FirmwareCatalog>> {
        self.catalog.get().cloned()
    }

    /// Holds services built elsewhere (a test with a fake link).
    pub fn set(&self, flash: Arc<FlashService>, catalog: Arc<FirmwareCatalog>) -> bool {
        self.flash.set(flash).is_ok() && self.catalog.set(catalog).is_ok()
    }
}

pub struct FirmwareModule {
    inner: Arc<Firmware>,
    net: Arc<dyn net::Net>,
}

impl FirmwareModule {
    pub fn new() -> FirmwareModule {
        FirmwareModule::with_net(Arc::new(net::HttpNet::new()))
    }

    /// The module with another network (a test points it at a stub server).
    pub fn with_net(net: Arc<dyn net::Net>) -> FirmwareModule {
        FirmwareModule {
            inner: Arc::new(Firmware::default()),
            net,
        }
    }
}

impl Default for FirmwareModule {
    fn default() -> Self {
        FirmwareModule::new()
    }
}

impl Module for FirmwareModule {
    fn name(&self) -> &'static str {
        "firmware"
    }

    fn register(&self, reg: &mut Registry) {
        reg.service(Arc::clone(&self.inner));
        routes::register(reg);
    }

    fn start(&self, core: &Core) -> std::io::Result<()> {
        if self.inner.flash.get().is_some() {
            return Ok(());
        }
        let Some(vehicle) = core.service::<crate::vehicle::VehicleService>() else {
            return Ok(());
        };
        let Some(main) = vehicle.handles(crate::fleet::MAIN_LINK) else {
            return Ok(());
        };
        let configured = core.settings().get().str_field("firmware_dir").trim().to_string();
        let dir = if configured.is_empty() {
            core.paths().join("firmware").to_string_lossy().into_owned()
        } else {
            crate::mapdata::util::expand_user(&configured)
        };
        let catalog = Arc::new(FirmwareCatalog::new(dir, Arc::clone(&self.net)));
        let host = Arc::new(host::BridgeFlashHost {
            bridge: main.bridge,
            store: main.store,
        });
        let flash = FlashService::new(host, Some(catalog.clone()), Arc::new(uploader::SystemEnv));
        let _ = self.inner.set(flash, catalog);
        Ok(())
    }

    fn stop(&self, _core: &Core) {
        if let Some(flash) = self.inner.flash() {
            flash.shutdown();
        }
    }
}
