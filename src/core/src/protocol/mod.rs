//! The MAVLink protocols, in plain Rust on `corvus_link::plain`. Each one is
//! the Rust twin of an engine in `corvus_link/src/protocol` (kept in Python
//! until `python/` is deleted) and is held to it by scenarios the Python one
//! plays (`python/tools/dump_protocol_cases.py`).
//!
//! An engine owns its protocol's state and decisions; what belongs to the
//! bridge it runs on (the link and its send lock, whether it is ready, the
//! armed state, the clock, the listeners) is asked of a host trait. Engines
//! never hold their own lock across a call into the host.

use corvus_link::plain::{defs::Wire, Message, Value};

pub mod link_host;
pub mod logs;
pub mod params;

/// What every protocol engine asks of the bridge it runs on. A protocol that
/// needs more (a clock, listeners) extends it with its own host trait.
pub trait RequestHost: Send + Sync {
    fn connection_ready(&self) -> bool;
    fn armed(&self) -> bool;
    /// Sends one message on the bridge's link, under its send lock.
    /// `Ok(false)` is "the link is no longer this bridge's", `Err` a failed write.
    fn send(&self, msg: Message) -> Result<bool, String>;
    /// The vehicle's system and component id.
    fn target(&self) -> (u8, u8);
    fn set_error(&self, text: &str);
    fn last_error(&self) -> String;
    fn console(&self, source: &str, text: &str, level: &str);
    fn log(&self, _level: &str, _text: &str) {}
}

/// A message addressed to the vehicle: its `target_system` and `target_component` set.
pub(crate) fn targeted(name: &str, target: (u8, u8)) -> Message {
    let mut m = Message::named(name, Wire::V2).expect("the all dialect has this message");
    let _ = m.set("target_system", Value::Int(target.0 as i128));
    let _ = m.set("target_component", Value::Int(target.1 as i128));
    m
}
