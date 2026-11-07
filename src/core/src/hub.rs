//! Which vehicle state store the routes talk to.

use std::sync::{Arc, RwLock};

use crate::core::Core;
use crate::request::Request;
use crate::response::Response;
use crate::store::VehicleStateStore;

/// Finds the store of `vehicle` (the id `GET /api/fleet` lists), or the
/// response that refuses it: 400 for an id that is not one, 409 for a vehicle
/// that is not there. The fleet module sets it.
pub type Resolver = Arc<dyn Fn(&Core, &str) -> Result<Arc<VehicleStateStore>, Response> + Send + Sync>;

/// The stores of the station: the main one, the one being flown and, for a
/// request naming `vehicle`, the one that vehicle has.
pub struct StoreHub {
    main: Arc<VehicleStateStore>,
    current: RwLock<Arc<VehicleStateStore>>,
    resolver: RwLock<Option<Resolver>>,
}

impl StoreHub {
    pub(crate) fn new(main: Arc<VehicleStateStore>) -> StoreHub {
        StoreHub {
            current: RwLock::new(main.clone()),
            main,
            resolver: RwLock::new(None),
        }
    }

    /// The store of the first link, which never moves.
    pub fn main(&self) -> Arc<VehicleStateStore> {
        self.main.clone()
    }

    /// The store of the vehicle being flown.
    pub fn current(&self) -> Arc<VehicleStateStore> {
        self.current.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_current(&self, store: Arc<VehicleStateStore>) {
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = store;
    }

    pub fn set_resolver(&self, resolver: Resolver) {
        *self.resolver.write().unwrap_or_else(|e| e.into_inner()) = Some(resolver);
    }

    /// The store a request acts on: the one named by `?vehicle=` (query of a
    /// GET) or by `vehicle` in a JSON body, else the one being flown. This is
    /// `targets_vehicle` of `routes/fleet.py`; without a fleet module the key
    /// names nothing and the current store answers.
    pub fn for_request(&self, core: &Core, req: &Request) -> Result<Arc<VehicleStateStore>, Response> {
        let key = req
            .payload()
            .get("vehicle")
            .and_then(|v| v.as_str())
            .or_else(|| req.query.get("vehicle"))
            .unwrap_or("");
        if key.is_empty() {
            return Ok(self.current());
        }
        match self.resolver.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(resolve) => resolve(core, key),
            None => Ok(self.current()),
        }
    }
}
