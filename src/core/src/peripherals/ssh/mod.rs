//! The SSH transport (`ssh_bridge.py`): interactive sessions for the terminal
//! registry, one shot commands for the launcher and the power buttons, and the
//! host key rules of `~/.corvus/known_hosts`.
//!
//! `russh` is the client: pure Rust, so no libssh2 and no OpenSSL is built into
//! or shipped with any bundle. It is asynchronous; each session and each
//! command runs a private single threaded tokio runtime on a thread of its own,
//! and nothing async leaves this folder.

pub mod client;
pub mod commands;
pub mod hostkeys;
mod routes;
pub mod run;
pub mod session;

pub use routes::register;
pub use session::SshSession;
