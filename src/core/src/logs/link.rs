//! The log service's view of the bridge.

use std::sync::Arc;

use corvus_link::plain::{Message, Value};

use super::service::{LogLink, LogMsg, LogSink};
use crate::bridge::Bridge;

/// A bridge, as a [`LogLink`].
pub struct BridgeLink {
    bridge: Bridge,
}

impl BridgeLink {
    pub fn new(bridge: Bridge) -> BridgeLink {
        BridgeLink { bridge }
    }
}

fn int(msg: &Message, field: &str) -> Option<i64> {
    msg.get(field).and_then(Value::as_i64)
}

/// The LOG_ENTRY or LOG_DATA the service reads, or `None` for a frame whose
/// fields are not numbers (the Python sink returned on a `TypeError`).
fn read(msg: &Message) -> Option<LogMsg> {
    match msg.name() {
        "LOG_ENTRY" => Some(LogMsg::Entry {
            id: int(msg, "id")?,
            size: int(msg, "size")?,
            utc: int(msg, "time_utc").unwrap_or(0),
            num_logs: int(msg, "num_logs").unwrap_or(0),
        }),
        "LOG_DATA" => {
            let data = match msg.get("data") {
                Some(Value::Bytes(b)) => b.clone(),
                Some(Value::Array(items)) => items.iter().filter_map(Value::as_i64).map(|v| v as u8).collect(),
                _ => Vec::new(),
            };
            Some(LogMsg::Data {
                id: int(msg, "id")?,
                ofs: int(msg, "ofs")?,
                count: int(msg, "count")?,
                data,
            })
        }
        _ => None,
    }
}

impl LogLink for BridgeLink {
    fn is_connected(&self) -> bool {
        self.bridge.is_connected()
    }

    fn set_log_sink(&self, sink: Option<LogSink>) {
        self.bridge.set_log_sink(sink.map(|sink| {
            Arc::new(move |msg: &Message| {
                if let Some(parsed) = read(msg) {
                    sink(&parsed);
                }
            }) as crate::bridge::LogSink
        }));
    }

    fn request_log_list(&self, start: i64, end: i64) -> bool {
        self.bridge.log_request_list(start, end)
    }

    fn request_log_data(&self, id: i64, offset: i64, count: i64) -> bool {
        self.bridge.log_request_data(id, offset, count)
    }

    fn log_request_end(&self) -> bool {
        self.bridge.log_request_end()
    }

    fn erase_logs(&self) -> bool {
        self.bridge.log_erase()
    }

    fn last_command_error(&self) -> String {
        self.bridge.last_error()
    }

    fn log_suffix(&self) -> &'static str {
        match self.bridge.capabilities().get("log_suffix").and_then(|v| v.as_str()) {
            Some(".bin") => ".bin",
            _ => ".ulg",
        }
    }
}
