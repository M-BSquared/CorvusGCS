//! The bridge's own requests to the vehicle and MAVLink signing: the stream
//! rate tables and the legacy REQUEST_DATA_STREAM fallback, the per message
//! interval requests, the GCS authority check, the home and version requests,
//! and applying a signing key.
//!
//! The twin of `corvus_link`'s requests engine (`bridge/requests.rs`). The
//! rates are for a radio link or a cable, because a 57 kbps SiK radio cannot
//! carry what a USB link can; which mechanism the autopilot speaks is read from
//! the ACKs of the interval requests, since the firmware version arrives too
//! late to decide and not at all on some builds.

use corvus_link::plain::defs::{constant_or, message_by_name, Wire};
use corvus_link::plain::{Message, Value as Wv};
use serde_json::{json, Map, Value};

use super::common::result_text;
use super::targeted;
use crate::dialect::Dialect;
use crate::priority_lock::PriorityLock;

/// The ground station's own system id, 254 and not the conventional 255, so
/// it can share a link with QGroundControl without the two fighting over one id.
pub const GCS_SYSTEM_ID: i64 = 254;
/// How many interval requests in a row may go unanswered before the rest are left alone.
pub const INTERVAL_REQUEST_SILENT_LIMIT: i64 = 3;
const MSG_ID_HOME_POSITION: i64 = 242;
const MSG_ID_AUTOPILOT_VERSION: i64 = 148;
/// ArduPilot takes MANUAL_CONTROL only from this system id (`MAV_GCS_SYSID`
/// from 4.5) and silently drops everything else.
pub const GCS_AUTHORITY_PARAMS: [&str; 2] = ["SYSID_MYGCS", "MAV_GCS_SYSID"];

/// What the requests ask of the bridge they run on.
pub trait RequestsHost: Send + Sync {
    fn is_slow_link(&self) -> bool;
    fn has_connection(&self) -> bool;
    /// Sends one message under the link's send lock. `Ok(false)` is "the link
    /// is no longer this bridge's".
    fn send(&self, msg: Message) -> Result<bool, String>;
    fn target(&self) -> (u8, u8);
    fn request_sent(&self) -> bool;
    fn set_request_sent(&self, sent: bool);
    fn dialect(&self) -> Dialect;
    fn connection_ready(&self) -> bool;
    fn lock(&self) -> &PriorityLock;
    fn set_error(&self, text: &str);
    fn console(&self, source: &str, text: &str, level: &str);
    fn log(&self, level: &str, text: &str);
    fn store_warning(&self, text: &str, level: &str);
    fn command_failure(&self, action: &str, result: i64) -> bool;
    /// COMMAND_LONG and its ACK.
    fn send_command(&self, command: i64, params: &[f64], timeout: f64, retries: i64) -> i64;
    /// Reads parameters by name, up to `timeout`; the names the vehicle gave.
    fn fetch_params(&self, names: &[&str], timeout: f64) -> Vec<(String, f64)>;
    /// Arranges the AUTOPILOT_VERSION request to be repeated if nothing came.
    fn schedule_version_retry(&self);
    fn store_update(&self, fields: Map<String, Value>);
    // Signing.
    /// The operator's key, `None` when signing is off, `Err` when it cannot be used.
    fn load_signing_key(&self) -> Result<Option<Vec<u8>>, String>;
    fn wire_is_mavlink2(&self) -> bool;
    fn switch_to_mavlink2(&self) -> Result<(), String>;
    fn setup_signing(&self, key: &[u8]) -> Result<(), String>;
    fn conn_string(&self) -> String;
}

fn data_stream(name: &str, fallback: i64) -> i64 {
    constant_or(name, fallback)
}

fn msg_id(name: &str) -> i64 {
    message_by_name(name, Wire::V2).map_or(0, |m| m.id as i64)
}

/// Per stream REQUEST_DATA_STREAM rates, throttled on radio links: a SiK radio
/// tunnels MAVLink over about 57 kbps, so the default 50 Hz streams would
/// saturate it. These are what ArduPilot honours; PX4 ignores the message.
/// EXTENDED_STATUS carries SYS_STATUS, GPS_RAW_INT and MISSION_CURRENT on
/// ArduPilot; without it a stack that needs this fallback had no battery, no
/// GPS fix and no mission progress at all.
pub fn stream_rates(slow: bool) -> Vec<(i64, i64)> {
    let s = |name: &str, fallback: i64, rate: i64| (data_stream(name, fallback), rate);
    if slow {
        vec![
            s("MAV_DATA_STREAM_EXTENDED_STATUS", 2, 2),
            s("MAV_DATA_STREAM_POSITION", 6, 5),
            s("MAV_DATA_STREAM_EXTRA1", 10, 10),
            s("MAV_DATA_STREAM_EXTRA2", 11, 5),
            s("MAV_DATA_STREAM_EXTRA3", 12, 2),
            s("MAV_DATA_STREAM_RAW_SENSORS", 1, 0),
            s("MAV_DATA_STREAM_RC_CHANNELS", 3, 0),
        ]
    } else {
        vec![
            s("MAV_DATA_STREAM_EXTENDED_STATUS", 2, 2),
            s("MAV_DATA_STREAM_POSITION", 6, 10),
            s("MAV_DATA_STREAM_EXTRA1", 10, 50),
            s("MAV_DATA_STREAM_EXTRA2", 11, 10),
            s("MAV_DATA_STREAM_EXTRA3", 12, 5),
            s("MAV_DATA_STREAM_RAW_SENSORS", 1, 5),
            s("MAV_DATA_STREAM_RC_CHANNELS", 3, 5),
        ]
    }
}

/// Per message MAV_CMD_SET_MESSAGE_INTERVAL targets in microseconds: PX4's
/// rate control and the only mechanism it has ever answered. VIBRATION is on
/// demand only. HEARTBEAT is absent on purpose: it is 1 Hz on every stack by
/// definition, and PX4 answers an interval request for it with FAILED.
pub fn message_intervals(slow: bool) -> Vec<(i64, i64)> {
    let m = |name: &str, us: i64| (msg_id(name), us);
    if slow {
        vec![
            m("ATTITUDE", 100_000),
            m("GLOBAL_POSITION_INT", 200_000),
            m("VFR_HUD", 200_000),
            m("SYS_STATUS", 1_000_000),
            m("GPS_RAW_INT", 1_000_000),
            // 0.2 Hz, because home is where RTL ends and a stale one on the map
            // is a lie about where the aircraft will come back to.
            m("HOME_POSITION", 5_000_000),
            m("EXTENDED_SYS_STATE", 1_000_000),
            // The pack's own detail: per cell volts, consumed mAh, temperature.
            m("BATTERY_STATUS", 2_000_000),
            // Mission progress, and the uptime that tells a reboot from a
            // dropped link. ArduPilot streams neither unless asked.
            m("MISSION_CURRENT", 1_000_000),
            m("SYSTEM_TIME", 2_000_000),
        ]
    } else {
        vec![
            m("ATTITUDE", 20_000),
            m("GLOBAL_POSITION_INT", 100_000),
            m("VFR_HUD", 100_000),
            m("SYS_STATUS", 1_000_000),
            m("GPS_RAW_INT", 1_000_000),
            m("HOME_POSITION", 1_000_000),
            m("EXTENDED_SYS_STATE", 1_000_000),
            // 5 Hz, matching what MAV_DATA_STREAM_RC_CHANNELS used to ask for.
            m("RC_CHANNELS", 200_000),
            m("BATTERY_STATUS", 1_000_000),
            m("MISSION_CURRENT", 1_000_000),
            m("SYSTEM_TIME", 1_000_000),
        ]
    }
}

fn command_long(host: &dyn RequestsHost, command: i64, params: &[f64]) -> Message {
    let mut msg = targeted("COMMAND_LONG", host.target())
        .with("command", Wv::Int(i128::from(command)))
        .with("confirmation", Wv::Int(0));
    for (i, value) in params.iter().enumerate() {
        msg = msg.with(&format!("param{}", i + 1), Wv::Float(*value));
    }
    msg
}

/// The bridge's own requests.
#[derive(Debug, Default, Clone, Copy)]
pub struct RequestsEngine;

impl RequestsEngine {
    pub fn new() -> RequestsEngine {
        RequestsEngine
    }

    /// Turns on MAVLink 2 signing when the operator has configured a key. With
    /// it on, every frame sent is signed and unsigned ones are rejected, the
    /// only thing that stops somebody within radio range of a SiK link from
    /// injecting a disarm. RADIO_STATUS, ADSB_VEHICLE and COLLISION stay exempt:
    /// none can command anything. A key that cannot be loaded fails the
    /// connection rather than falling back to an unsigned link, so a typo in a
    /// path never silently downgrades the security the operator asked for.
    /// `Err` carries the message of the failed connection.
    pub fn apply_signing(&self, host: &dyn RequestsHost) -> Result<(), String> {
        let key = match host.load_signing_key() {
            Ok(key) => key,
            Err(e) => {
                let message = format!("MAVLink signing key unusable: {e}");
                host.log("error", &message);
                host.store_update(link_error(&message));
                return Err(message);
            }
        };
        let Some(key) = key else { return Ok(()) };
        // Signing only exists in MAVLink 2, and the key lives on the link's
        // dialect object. A connection that starts as MAVLink 1 swaps it for a
        // fresh one when the first MAVLink 2 frame arrives, and the key goes
        // with the old one: the link then says "signing enabled" and sends
        // every frame unsigned. Switching first leaves nothing for that swap to do.
        let enabled = (|| {
            if !host.wire_is_mavlink2() {
                host.switch_to_mavlink2()?;
            }
            host.setup_signing(&key)
        })();
        if let Err(e) = enabled {
            let message = format!("MAVLink signing could not be enabled: {e}");
            host.log("error", &message);
            host.store_update(link_error(&message));
            return Err(message);
        }
        host.log("info", &format!("MAVLink 2 signing enabled for {}", host.conn_string()));
        host.console("LINK", "MAVLink signing enabled", "success");
        Ok(())
    }

    /// Sends the deprecated REQUEST_DATA_STREAM set, at most once per link. On
    /// serial it runs at connect so telemetry starts without a command round
    /// trip; on UDP only if the interval requests find the modern mechanism
    /// missing. The latch is what lets that second caller retry per message
    /// without sending the whole set again.
    pub fn request_streams(&self, host: &dyn RequestsHost) {
        if host.request_sent() || !host.has_connection() {
            return;
        }
        for (stream, rate) in stream_rates(host.is_slow_link()) {
            if rate == 0 {
                continue;
            }
            let (system, component) = host.target();
            let msg = targeted("REQUEST_DATA_STREAM", (system, component))
                .with("req_stream_id", Wv::Int(i128::from(stream)))
                .with("req_message_rate", Wv::Int(i128::from(rate)))
                .with("start_stop", Wv::Int(1));
            match host.send(msg) {
                Ok(true) => {}
                Ok(false) => return,
                // A fallback request is best effort.
                Err(e) => host.log("debug", &format!("REQUEST_DATA_STREAM {stream} failed: {e}")),
            }
        }
        host.set_request_sent(true);
    }

    /// Warns when this ArduPilot vehicle will ignore our stick input: it
    /// accepts MANUAL_CONTROL only from the system id in `SYSID_MYGCS`, and
    /// silently drops everything else, with no NAK and no STATUSTEXT. Corvus is
    /// 254, so on a stock vehicle the joystick does nothing and says nothing.
    /// Checked once per connection and reported with the fix in it; the
    /// parameter is the operator's to write. PX4 accepts MANUAL_CONTROL from
    /// any station, so this runs on ArduPilot only.
    pub fn check_gcs_authority(&self, host: &dyn RequestsHost) {
        if host.dialect().stack() != crate::dialect::Stack::ArduPilot {
            return;
        }
        if !host.connection_ready() {
            return;
        }
        let values = host.fetch_params(&GCS_AUTHORITY_PARAMS, 3.0);
        // An ArduPilot that answers for neither is old enough or odd enough
        // that guessing would be worse than saying nothing.
        let Some((name, value)) = GCS_AUTHORITY_PARAMS
            .iter()
            .find_map(|n| values.iter().find(|(k, _)| k == n).map(|(k, v)| (k.clone(), *v)))
        else {
            return;
        };
        let current = value.round_ties_even() as i64;
        if current == GCS_SYSTEM_ID {
            return;
        }
        let text = format!(
            "This vehicle only accepts stick input from ground station {current} ({name}), and Corvus is {GCS_SYSTEM_ID}, so the joystick will be ignored until {name} is set to {GCS_SYSTEM_ID} in Parameters. Flight commands and mode changes are unaffected."
        );
        host.log("info", &text);
        host.console("JOYSTICK", &text, "warning");
        host.store_warning(&text, "warning");
    }

    /// Asks the autopilot for per message stream intervals, best effort: a
    /// firmware that denies one message must not block the rest. This is also
    /// where the legacy fallback is decided on UDP, because the ACKs are the
    /// only honest evidence of which mechanism the autopilot speaks.
    /// UNSUPPORTED is the firmware saying it has never heard of the command (an
    /// older ArduPilot, never an old PX4): the fallback is the whole point.
    /// A missing ACK is the link saying nothing: the fallback goes once as
    /// insurance, but the loop goes on. Anything else proves the modern path
    /// works and REQUEST_DATA_STREAM is never sent. Quiet throughout: a refusal
    /// here is an answer the loop acts on, not a failure the operator caused.
    pub fn request_message_intervals(&self, host: &dyn RequestsHost) {
        let mut silent = 0;
        for (msg_id, interval_us) in message_intervals(host.is_slow_link()) {
            let (_ok, result) = self.set_message_interval(host, msg_id, interval_us, true);
            if result == -2 {
                // Link torn down under us; the next connect starts over.
                return;
            }
            if result == constant_or("MAV_RESULT_UNSUPPORTED", 3) {
                host.log(
                    "info",
                    &format!(
                        "SET_MESSAGE_INTERVAL unsupported (msg {msg_id}), falling back to REQUEST_DATA_STREAM for this connection"
                    ),
                );
                self.request_streams(host);
                return;
            }
            if result == -1 {
                self.request_streams(host);
                silent += 1;
                // Each unanswered request costs two full ACK timeouts under the
                // command lock, so on a link that answers nothing the whole
                // list spent the better part of a minute with every operator
                // command queued behind it. A few in a row is that answer.
                if silent >= INTERVAL_REQUEST_SILENT_LIMIT {
                    host.log(
                        "info",
                        &format!(
                            "no COMMAND_ACK for {silent} interval requests, leaving the rest to REQUEST_DATA_STREAM for this connection"
                        ),
                    );
                    return;
                }
            } else {
                silent = 0;
            }
        }
    }

    /// Asks the autopilot to send HOME_POSITION now. It arrives on its own, so
    /// this only makes it appear promptly: straight after connect, and straight
    /// after set home. Fire and forget, both times; the periodic stream is the
    /// backstop.
    pub fn request_home(&self, host: &dyn RequestsHost) {
        if !host.has_connection() {
            return;
        }
        let nan = f64::NAN;
        let msg = command_long(
            host,
            constant_or("MAV_CMD_REQUEST_MESSAGE", 512),
            &[MSG_ID_HOME_POSITION as f64, nan, nan, nan, nan, nan, nan],
        );
        // A nudge that fails costs nothing.
        if let Err(e) = host.send(msg) {
            host.log("debug", &format!("HOME_POSITION request failed: {e}"));
        }
    }

    /// Asks the autopilot for AUTOPILOT_VERSION. Three requests, because no
    /// single one covers the supported firmware range: REQUEST_MESSAGE (PX4
    /// v1.16 to v1.18, the only one v1.18 accepts), the deprecated
    /// REQUEST_AUTOPILOT_CAPABILITIES for older builds, and the bare
    /// AUTOPILOT_VERSION_REQUEST message ArduPilot era firmware answers. Fire
    /// and forget: this runs before the receive loop dispatches ACKs.
    pub fn request_version(&self, host: &dyn RequestsHost) {
        if host.has_connection() {
            let nan = f64::NAN;
            let requests = [
                (
                    constant_or("MAV_CMD_REQUEST_MESSAGE", 512),
                    [MSG_ID_AUTOPILOT_VERSION as f64, nan, nan, nan, nan, nan, nan],
                ),
                (
                    constant_or("MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES", 520),
                    [1.0, nan, nan, nan, nan, nan, nan],
                ),
            ];
            for (command, params) in requests {
                let _ = host.send(command_long(host, command, &params));
            }
            let _ = host.send(targeted("AUTOPILOT_VERSION_REQUEST", host.target()));
        }
        host.schedule_version_retry();
    }

    /// One SET_MESSAGE_INTERVAL, with the raw COMMAND_ACK result beside the
    /// verdict. The connect time batch needs the result code: it separates a
    /// firmware that has never heard of the command (UNSUPPORTED) from one that
    /// refuses a single message it does not carry (DENIED), and only the first
    /// is grounds for the fallback. -1 is no ACK at all, -2 no usable link.
    /// `quiet` keeps the outcome off the console and the status bar.
    pub fn set_message_interval(
        &self,
        host: &dyn RequestsHost,
        msg_id: i64,
        interval_us: i64,
        quiet: bool,
    ) -> (bool, i64) {
        let _gate = host.lock().lock();
        host.set_error("");
        if !host.connection_ready() {
            return (false, -2);
        }
        let nan = f64::NAN;
        let result = host.send_command(
            constant_or("MAV_CMD_SET_MESSAGE_INTERVAL", 511),
            &[msg_id as f64, interval_us as f64, nan, nan, nan, nan, nan],
            3.0,
            1,
        );
        if result != constant_or("MAV_RESULT_ACCEPTED", 0) {
            if quiet {
                host.set_error(&format!("Set message interval failed: {}", result_text(result)));
                host.log("debug", &format!("SET_MESSAGE_INTERVAL msg {msg_id}: result {result}"));
                return (false, result);
            }
            return (host.command_failure("Set message interval", result), result);
        }
        if quiet {
            return (true, result);
        }
        host.console(
            "STREAM",
            &format!("Message {msg_id} interval set to {interval_us} us"),
            "success",
        );
        (true, result)
    }
}

fn link_error(message: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("link_error".into(), json!(message));
    m
}
