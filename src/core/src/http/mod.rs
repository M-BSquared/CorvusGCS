//! The optional HTTP server (`corvus_core::http`).
//!
//! The interface does not need it: it calls the core in its own process. It is
//! there for an AI client (`POST /mcp`), for `--server` clients, for a second
//! station and for the later Android app, and it serves exactly the routes the
//! core has, with the rules of `server.py` around them.
//!
//! **Why standard library only.** A thread per connection on `std::net` is
//! what Python's `ThreadingTCPServer` is, and what this needs: a few
//! connections, long lived streams, an upgrade to WebSocket. `tiny_http`
//! would take over the socket for the SSE and WebSocket cases and make
//! "every thread joins on the shutdown path" something to work around; `axum`
//! would bring tokio, a second runtime next to a 60 fps interface that must
//! not share a thread with one, plus hyper and tower for what is a few hundred
//! lines here. Every thread (the accept loop and each connection) is joined in
//! `Http::stop`; waits are cut into 200 to 250 ms slices that look at
//! `core.is_stopping()`, so joining never waits for a quiet socket.
//!
//! Like the Python server it answers HTTP/1.0 and closes after each response.

pub mod mcp;
pub mod security;
pub mod server;
pub mod static_files;

pub use server::{Http, HttpInfo, HttpOptions};
