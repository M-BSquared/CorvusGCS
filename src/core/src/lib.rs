//! `corvus_core`: the Corvus GCS backend as a library.
//!
//! The native interface runs this in its own process. An optional HTTP
//! server serves the same routes to an AI client (`POST /mcp`), to `--server`
//! clients and to a second station. The Python program in `python/` is the
//! reference: a route here answers what the Python route answers for the same
//! input, and the tests hold it to values dumped from the Python.
//!
//! # The contract for an area
//!
//! An area (vehicle, map data, logs and firmware, peripherals, AI and plugins)
//! is a [`Module`]. It lives in its own folder of this crate, and the only
//! shared files it edits are the one line that adds it to the core and its own
//! lines of `lib.rs`.
//!
//! ```ignore
//! struct Vehicle;
//!
//! impl Module for Vehicle {
//!     fn name(&self) -> &'static str { "vehicle" }
//!
//!     fn register(&self, reg: &mut Registry) {
//!         reg.get("/api/battery", |core, req| Response::json(json!({})));
//!         reg.post("/api/mavlink/arm", |core, req| {
//!             let armed = req.payload().get("armed");
//!             Response::json(json!({"ok": true}))
//!         });
//!         reg.get_pattern("/api/buildings/{z:d}/{x:d}/{y:d}.json", |core, req| {
//!             Response::bytes(200, "application/json", vec![])
//!         });
//!         reg.guard(|core, req| None);
//!         reg.service(Arc::new(Bridge::new()));
//!     }
//!
//!     fn start(&self, core: &Core) -> io::Result<()> { /* threads */ Ok(()) }
//!     fn stop(&self, core: &Core) { /* join them */ }
//! }
//! ```
//!
//! * **Routes.** A handler is `Fn(&Core, &Request) -> Response`. A POST route
//!   reads its JSON object with `req.payload()`; the router has already
//!   answered 400 for a body that is not one, and 413 beyond the cap
//!   (`Registry::post_with` takes another cap, or a raw body for an upload).
//!   `req.query.get("name")` is `parse_qs(...)[name][0]`, and `req.param("z")`
//!   a capture of a pattern. A path nothing registered but the Python server
//!   answers is a 501 "Not available in this build" ([`unported`]); registering
//!   it takes it over, with nothing else to edit.
//! * **Calling a route.** [`Core::internal_call`] runs `(method, path,
//!   payload)` through the same handler and the same guards as an HTTP request
//!   and returns `(status, JSON)`. A tool of an AI client and the interface use
//!   it; nothing reaches the link around a route.
//! * **Services.** `reg.service(Arc<T>)` (or `core.insert_service` in `start`)
//!   publishes a value that other areas fetch with `core.service::<T>()`. The
//!   state store and the settings are services of the foundation.
//! * **Threads.** A module starts its threads in `start` and joins every one in
//!   `stop` (reverse order, once). A loop that sleeps uses
//!   `core.wait_stopping(duration)` so shutdown does not wait for it.
//! * **JSON.** `serde_json::Value` with `preserve_order`; [`json::num`] turns
//!   NaN and infinity into `0.0` as `_sanitize` does; [`json::error`] is
//!   `{"error": ...}`. A response equals the Python one as parsed JSON.

pub mod json;
pub mod request;
pub mod response;

mod core;
mod foundation;
mod module;
mod router;
mod unported;

pub use crate::core::{Core, CoreBuilder, Options};
pub use crate::module::{Guard, Module, Registry};
pub use crate::request::{Headers, Method, Origin, Query, Request};
pub use crate::response::{Response, SseEvent, SseNext, SseStream, UNAVAILABLE};
pub use crate::router::{Body, Handler, Pattern, Route, RouteOptions, Router, MAX_JSON_BODY, MAX_SETTINGS_BODY};
