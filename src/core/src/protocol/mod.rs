//! The MAVLink protocols, in plain Rust on `corvus_link::plain`. Each one is
//! the Rust twin of an engine in `corvus_link/src/protocol` (kept in Python
//! until `python/` is deleted) and is held to it by scenarios the Python one
//! plays (`python/tools/dump_protocol_cases.py`).
//!
//! An engine owns its protocol's state and decisions; what belongs to the
//! bridge it runs on (the link and its send lock, whether it is ready, the
//! armed state, the clock, the listeners) is asked of a host trait. Engines
//! never hold their own lock across a call into the host.

pub mod link_host;
pub mod params;
