//! The links to the vehicle: the transports a `plain::connection` runs on.

#[cfg(unix)]
pub mod serial_posix;
pub mod transport;
pub mod websocket;
