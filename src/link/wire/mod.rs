//! MAVLink itself: the dialect tables, message objects, payload codec, CRC,
//! framing with signing and the stream parser, and the flight mode tables.
//! The tables and the CRC are plain Rust; the rest is the Python object model
//! (`python` feature). The plain codec and framing are in `crate::plain`.

#[cfg(feature = "python")]
pub mod codec;
#[cfg(feature = "python")]
pub mod crc;
pub mod defs;
#[cfg(feature = "python")]
pub mod dialect;
#[cfg(feature = "python")]
pub mod frame;
#[cfg(feature = "python")]
pub mod message;
#[cfg(feature = "python")]
pub mod modes;
