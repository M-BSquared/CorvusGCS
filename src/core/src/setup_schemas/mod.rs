//! The setup page schemas of the Vehicle area: for every page (safety,
//! battery, mounting, motors, radio control, tuning, first setup) one schema
//! per flight stack with the same output shape, so the interface cannot tell
//! which stack built a page. The Python originals are `python/corvus/*_config.py`
//! and `ardupilot_*.py`; the tests hold each schema to the JSON the Python
//! builds for the same parameter values.

pub mod ardupilot_battery;
pub mod ardupilot_motors;
pub mod ardupilot_mounting;
pub mod ardupilot_rc;
pub mod ardupilot_safety;
pub mod ardupilot_tuning;
pub mod battery;
pub mod fields;
pub mod first_setup;
pub mod motors;
pub mod mounting;
pub mod param_metadata;
pub mod rc;
mod routes;
pub mod safety;
mod tables;

pub use routes::SetupSchemasModule;
pub mod tuning;
