//! The links to the vehicle: `mavlink_connection`, the per-system state on
//! top of a link, and the transports under it. The transports are plain
//! Rust; the connection object is the Python surface (`python` feature).

#[cfg(feature = "python")]
pub mod connection;
#[cfg(feature = "python")]
pub mod csv_log;
#[cfg(feature = "python")]
pub mod dataflash;
#[cfg(unix)]
pub mod serial_posix;
pub mod transport;
pub mod websocket;
