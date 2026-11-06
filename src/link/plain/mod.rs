//! The plain Rust MAVLink layer: no PyO3, no Python object model.
//!
//! `corvus_core` and the native interface use this; the Python extension
//! (`python` feature) keeps its own pymavlink shaped surface beside it, and
//! `python/tests/test_corvus_link_*.py` hold that one to pymavlink. This layer
//! is held to pymavlink too, through frames it produced
//! (`python/tools/dump_link_cases.py`).
//!
//! * [`Message`] and [`Value`]: a message as data, from the generated tables.
//! * [`codec`]: payload packing and unpacking.
//! * [`frame`]: [`frame::encode`], the stream [`frame::Parser`] and
//!   MAVLink 2 [`frame::Signing`].
//! * [`connection`]: [`connection::open`] a link from a connection string,
//!   `recv` frames, `send` messages, the systems heard.
//! * [`transport`]: the UDP, TCP and serial links, as bytes in and out.

pub mod codec;
pub mod connection;
pub mod crc;
pub mod frame;
pub mod message;
pub mod time;

pub use crate::link::{transport, websocket};
pub use crate::wire::defs;
pub use message::{Message, Value};
