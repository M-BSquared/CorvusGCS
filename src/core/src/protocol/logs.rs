//! On-board log transfer, the request side: list the vehicle's logs, ask for
//! a chunk of one, erase them all, and end the session. The LOG_ENTRY and
//! LOG_DATA replies go to whatever consumes the link's frames.
//!
//! The twin of `LogEngine` in `corvus_link` (`protocol/logs.rs`). Every method
//! expects the caller to hold the bridge's operation lock.

use corvus_link::plain::Value;

use super::{targeted, RequestHost};

#[derive(Debug, Default, Clone, Copy)]
pub struct LogEngine;

fn send(host: &dyn RequestHost, mut msg: corvus_link::plain::Message, fields: &[(&str, i128)]) -> Result<(), String> {
    for (name, value) in fields {
        let _ = msg.set(name, Value::Int(*value));
    }
    match host.send(msg) {
        Ok(true) => Ok(()),
        Ok(false) => Err("link closed".to_string()),
        Err(e) => Err(e),
    }
}

impl LogEngine {
    pub fn new() -> LogEngine {
        LogEngine
    }

    /// LOG_REQUEST_LIST: the vehicle answers with one LOG_ENTRY per log.
    pub fn request_list(&self, host: &dyn RequestHost, start: i64, end: i64) -> bool {
        if !host.connection_ready() {
            host.set_error("not connected");
            return false;
        }
        let msg = targeted("LOG_REQUEST_LIST", host.target());
        if let Err(e) = send(host, msg, &[("start", start as i128), ("end", end as i128)]) {
            host.set_error(&format!("log list request failed: {e}"));
            return false;
        }
        true
    }

    /// LOG_REQUEST_DATA for one chunk; answered with LOG_DATA.
    pub fn request_data(&self, host: &dyn RequestHost, log_id: i64, offset: i64, count: i64) -> bool {
        if !host.connection_ready() {
            host.set_error("not connected");
            return false;
        }
        let msg = targeted("LOG_REQUEST_DATA", host.target());
        if let Err(e) = send(
            host,
            msg,
            &[
                ("id", log_id as i128),
                ("ofs", offset as i128),
                ("count", count as i128),
            ],
        ) {
            host.set_error(&format!("log data request failed: {e}"));
            return false;
        }
        true
    }

    /// LOG_ERASE: every log on the vehicle. Refused while armed.
    pub fn erase(&self, host: &dyn RequestHost) -> bool {
        host.set_error("");
        if !host.connection_ready() {
            host.set_error("not connected");
            return false;
        }
        if host.armed() {
            host.set_error("cannot erase logs while armed");
            return false;
        }
        if let Err(e) = send(host, targeted("LOG_ERASE", host.target()), &[]) {
            host.set_error(&format!("log erase failed: {e}"));
            return false;
        }
        host.console("LOGS", "erasing all on-board logs", "warning");
        true
    }

    /// LOG_REQUEST_END, without which PX4 keeps the session and blocks the
    /// next flight's logging. Best effort.
    pub fn request_end(&self, host: &dyn RequestHost) -> bool {
        if !host.connection_ready() {
            return false;
        }
        send(host, targeted("LOG_REQUEST_END", host.target()), &[]).is_ok()
    }
}
