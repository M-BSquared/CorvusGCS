//! MAVLink itself: the dialect tables, message objects, payload codec, CRC,
//! framing with signing and the stream parser, and the flight mode tables.

pub mod codec;
pub mod crc;
pub mod defs;
pub mod dialect;
pub mod frame;
pub mod message;
pub mod modes;
