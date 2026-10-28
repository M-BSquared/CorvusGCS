//! The links to the vehicle: `mavlink_connection`, the per-system state on
//! top of a link, and the transports under it.

pub mod connection;
pub mod csv_log;
#[cfg(unix)]
pub mod serial_posix;
pub mod transport;
pub mod websocket;
