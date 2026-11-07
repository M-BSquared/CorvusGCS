//! RTCM injection: putting one RTCM 3 correction on the link as
//! GPS_RTCM_DATA, and counting what actually reached the aircraft.
//!
//! The twin of `corvus_link`'s injection (`bridge/rtcm.rs`). Called from the
//! RTK service's reader thread at a few hertz for as long as a base station is
//! streaming. It does not take the operation lock: every other send path
//! serialises behind it because it is a transaction, while a correction is
//! fire and forget and queueing it behind a mission upload would stall the
//! corrections for the length of the upload. It never raises: the caller's
//! other job is the serial port. It drops rather than truncates: a receiver
//! that reassembles three quarters of a message gets a CRC failure.

use std::sync::Mutex;

use corvus_link::plain::{Message, Value as Wv};
use serde_json::{json, Value};

use super::targeted_none;

/// GPS_RTCM_DATA carries up to 180 bytes per message, in at most four fragments.
pub const FRAGMENT_BYTES: usize = 180;
pub const MAX_FRAGMENTS: usize = 4;
pub const MAX_MESSAGE_BYTES: usize = FRAGMENT_BYTES * MAX_FRAGMENTS - 1;

/// One RTCM frame as `(flags, data)` pairs for GPS_RTCM_DATA. Empty for an
/// empty frame and for one too long to express: the caller counts those rather
/// than send a sequence the far end waits on forever. `sequence` increments
/// per correction modulo 32 and tells a receiver the fragments of one message
/// from those of the next after a dropped packet. The unfragmented case keeps
/// bit 0 clear and still carries the sequence, as QGroundControl does.
pub fn fragments(frame: &[u8], sequence: u32) -> Vec<(u8, Vec<u8>)> {
    if frame.is_empty() || frame.len() > MAX_MESSAGE_BYTES {
        return Vec::new();
    }
    let seq_bits = ((sequence & 0x1F) << 3) as u8;
    if frame.len() < FRAGMENT_BYTES {
        return vec![(seq_bits, frame.to_vec())];
    }
    let mut out: Vec<(u8, Vec<u8>)> = frame
        .chunks(FRAGMENT_BYTES)
        .enumerate()
        .map(|(index, chunk)| (1 | ((index as u8) << 1) | seq_bits, chunk.to_vec()))
        .collect();
    if frame.len().is_multiple_of(FRAGMENT_BYTES) {
        // The last fragment was a full 180 bytes, so nothing in it said "end".
        out.push((1 | ((out.len() as u8) << 1) | seq_bits, Vec::new()));
    }
    out
}

/// What the injection asks of the bridge.
pub trait RtcmHost: Send + Sync {
    fn connection_ready(&self) -> bool;
    /// Sends one message under the link's send lock, without the command lock.
    fn send(&self, msg: Message) -> Result<bool, String>;
    fn now(&self) -> f64;
    fn log(&self, level: &str, text: &str);
}

#[derive(Default)]
struct State {
    sequence: u32,
    bytes: i64,
    messages: i64,
    dropped: i64,
    last_send: f64,
}

/// The RTCM counters of one bridge.
#[derive(Default)]
pub struct RtcmInjector {
    state: Mutex<State>,
}

impl RtcmInjector {
    pub fn new() -> RtcmInjector {
        RtcmInjector::default()
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Puts one correction on the link; true when every fragment was written.
    pub fn inject_rtcm(&self, host: &dyn RtcmHost, frame: &[u8]) -> bool {
        if frame.is_empty() {
            return false;
        }
        if !host.connection_ready() {
            self.st().dropped += 1;
            return false;
        }
        let sequence = {
            let mut st = self.st();
            let s = st.sequence;
            st.sequence = (s + 1) & 0x1F;
            s
        };
        let parts = fragments(frame, sequence);
        if parts.is_empty() {
            self.st().dropped += 1;
            host.log(
                "warning",
                &format!(
                    "RTCM message of {} bytes is too long for GPS_RTCM_DATA; dropped",
                    frame.len()
                ),
            );
            return false;
        }
        for (flags, chunk) in parts {
            // The field is a fixed 180 bytes; the length is carried separately,
            // so the tail is padding and not data.
            let mut data: Vec<Wv> = chunk.iter().map(|b| Wv::Int(i128::from(*b))).collect();
            data.resize(FRAGMENT_BYTES, Wv::Int(0));
            let msg = targeted_none("GPS_RTCM_DATA")
                .with("flags", Wv::Int(i128::from(flags)))
                .with("len", Wv::Int(chunk.len() as i128))
                .with("data", Wv::Array(data));
            match host.send(msg) {
                Ok(true) => {}
                outcome => {
                    self.st().dropped += 1;
                    let why = match outcome {
                        Err(e) => e,
                        _ => "link closed".to_string(),
                    };
                    host.log("debug", &format!("RTCM injection failed: {why}"));
                    return false;
                }
            }
        }
        let mut st = self.st();
        st.bytes += frame.len() as i64;
        st.messages += 1;
        st.last_send = host.now();
        true
    }

    /// What has actually reached the aircraft, for the RTK page. Separate from
    /// what the base produced: a base streaming into a link that is down is the
    /// failure this pair of numbers exists to make visible.
    pub fn rtcm_stats(&self, host: &dyn RtcmHost) -> Value {
        let st = self.st();
        let age = (st.last_send != 0.0).then(|| host.now() - st.last_send);
        json!({
            "bytes": st.bytes,
            "messages": st.messages,
            "dropped": st.dropped,
            "age": age,
        })
    }

    /// Resets the counters when the correction source changes.
    pub fn forget_rtcm_session(&self) {
        let mut st = self.st();
        st.bytes = 0;
        st.messages = 0;
        st.dropped = 0;
        st.last_send = 0.0;
    }
}
