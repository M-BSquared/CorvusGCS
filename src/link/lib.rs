//! corvus_link: MAVLink for Corvus GCS.
//!
//! A Rust implementation of what Corvus used pymavlink for: the message set of
//! the `all` dialect in MAVLink 1 and 2 (`wire::defs`, generated from
//! `message_definitions/` at build time), framing with CRC_EXTRA, signing, the
//! byte-stream parser and the serial, UDP, TCP, Unix socket and WebSocket
//! links, all under [`plain`]. `tests/pymavlink_frozen.rs` holds it to
//! pymavlink's answers, frozen once.

use std::sync::{Mutex, MutexGuard};

mod link;
pub mod plain;
mod wire;

/// A lock that a panicked holder does not poison for everyone after it.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
