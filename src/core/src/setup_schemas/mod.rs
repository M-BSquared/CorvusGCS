//! The setup page schemas of the Vehicle area: for every page (safety,
//! battery, mounting, motors, radio control, tuning, first setup) one schema
//! per flight stack with the same output shape, so the interface cannot tell
//! which stack built a page. The Python originals are `python/corvus/*_config.py`
//! and `ardupilot_*.py`; the tests hold each schema to the JSON the Python
//! builds for the same parameter values.

pub mod fields;
pub mod param_metadata;
