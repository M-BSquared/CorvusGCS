//! The bridge's protocols, moved from Python one at a time.

#[cfg(feature = "python")]
pub mod flight;
#[cfg(feature = "python")]
pub mod ftp;
#[cfg(feature = "python")]
pub mod host;
#[cfg(feature = "python")]
pub mod logs;
#[cfg(feature = "python")]
pub mod missions;
#[cfg(feature = "python")]
pub mod params;
#[cfg(feature = "python")]
pub mod remote_id;
#[cfg(feature = "python")]
pub mod setup;
#[cfg(feature = "python")]
pub mod shell;
#[cfg(feature = "python")]
pub mod telemetry;
