//! MAVLink FTP, the read side: open a file, burst read it with the gaps asked
//! for again, reset a stuck session, take its CRC32, terminate. One transfer
//! at a time, cancellable.
//!
//! The twin of `FtpEngine` in `corvus_link` (`protocol/ftp.rs`). The wire
//! helpers below are a copy of that file's, which stays in Python's shape
//! until `python/` is deleted; the scenarios hold the two to each other.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub const FTP_OP_TERMINATE_SESSION: u8 = 1;
pub const FTP_OP_RESET_SESSIONS: u8 = 2;
pub const FTP_OP_OPEN_FILE_RO: u8 = 4;
pub const FTP_OP_READ_FILE: u8 = 5;
pub const FTP_OP_CALC_FILE_CRC32: u8 = 14;
pub const FTP_OP_BURST_READ_FILE: u8 = 15;
pub const FTP_OP_ACK: u8 = 128;
pub const FTP_OP_NAK: u8 = 129;

pub const FTP_ERR_FAIL: u8 = 1;
pub const FTP_ERR_FAIL_ERRNO: u8 = 2;
pub const FTP_ERR_INVALID_DATA_SIZE: u8 = 3;
pub const FTP_ERR_INVALID_SESSION: u8 = 4;
pub const FTP_ERR_NO_SESSIONS: u8 = 5;
pub const FTP_ERR_EOF: u8 = 6;
pub const FTP_ERR_UNKNOWN_COMMAND: u8 = 7;
pub const FTP_ERR_FILE_EXISTS: u8 = 8;
pub const FTP_ERR_FILE_PROTECTED: u8 = 9;
pub const FTP_ERR_FILE_NOT_FOUND: u8 = 10;

pub const FTP_PAYLOAD_LEN: usize = 251;
const HEADER_LEN: usize = 12;
pub const FTP_DATA_MAX: usize = FTP_PAYLOAD_LEN - HEADER_LEN;
const INBOX_MAX: usize = 1024;

/// What an operator is told for an error code of a NAK.
pub fn nak_text(code: u8) -> String {
    match code {
        FTP_ERR_FAIL => "the vehicle reported a failure".into(),
        FTP_ERR_FAIL_ERRNO => "the vehicle could not read the file".into(),
        FTP_ERR_INVALID_DATA_SIZE => "the vehicle rejected the request size".into(),
        FTP_ERR_INVALID_SESSION => "the file session was lost".into(),
        FTP_ERR_NO_SESSIONS => "the vehicle has no free file session".into(),
        FTP_ERR_EOF => "unexpected end of file".into(),
        FTP_ERR_UNKNOWN_COMMAND => "the vehicle does not support MAVLink FTP".into(),
        FTP_ERR_FILE_EXISTS => "the file already exists".into(),
        FTP_ERR_FILE_PROTECTED => "the file is protected".into(),
        FTP_ERR_FILE_NOT_FOUND => "the file does not exist on the vehicle".into(),
        other => format!("the vehicle refused the request (error {other})"),
    }
}

/// One decoded FILE_TRANSFER_PROTOCOL reply.
#[derive(Debug, Clone, PartialEq)]
pub struct FtpReply {
    pub seq: u16,
    pub session: u8,
    pub opcode: u8,
    pub size: u8,
    pub req_opcode: u8,
    pub burst_complete: bool,
    pub offset: u32,
    pub data: Vec<u8>,
}

impl FtpReply {
    /// The error code of a NAK, 0 for an ACK.
    pub fn code(&self) -> u8 {
        if self.opcode != FTP_OP_NAK {
            return 0;
        }
        if self.size != 0 && !self.data.is_empty() {
            self.data[0]
        } else {
            FTP_ERR_FAIL
        }
    }
}

/// The 251 byte payload of a request.
pub fn encode(seq: u16, session: u8, opcode: u8, offset: u32, data: &[u8], size: Option<i64>) -> Vec<u8> {
    let body = &data[..data.len().min(FTP_DATA_MAX)];
    let length = size.unwrap_or(body.len() as i64);
    let mut out = Vec::with_capacity(FTP_PAYLOAD_LEN);
    out.extend_from_slice(&seq.to_le_bytes());
    out.push(session);
    out.push(opcode);
    out.push((length & 0xFF) as u8);
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(body);
    out.resize(FTP_PAYLOAD_LEN, 0);
    out
}

/// A payload read as a reply; `None` when it is too short to be one.
pub fn decode(raw: &[u8]) -> Option<FtpReply> {
    if raw.len() < HEADER_LEN {
        return None;
    }
    let size = (raw[4] as usize).min(FTP_DATA_MAX).min(raw.len() - HEADER_LEN);
    Some(FtpReply {
        seq: u16::from_le_bytes([raw[0], raw[1]]),
        session: raw[2],
        opcode: raw[3],
        size: size as u8,
        req_opcode: raw[5],
        burst_complete: raw[6] != 0,
        offset: u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]),
        data: raw[HEADER_LEN..HEADER_LEN + size].to_vec(),
    })
}

/// The first offset not covered by `chunks`.
pub fn first_gap(chunks: &BTreeMap<u32, Vec<u8>>) -> u64 {
    let mut pos: u64 = 0;
    for (&offset, data) in chunks {
        let offset = offset as u64;
        if offset > pos {
            return pos;
        }
        pos = pos.max(offset + data.len() as u64);
    }
    pos
}

/// Why a transfer failed: a sentence for the operator, and the vehicle's
/// error code (0 when it was not the vehicle's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FtpError {
    pub message: String,
    pub code: u8,
}

impl std::fmt::Display for FtpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FtpError {}

fn error(message: &str, code: u8) -> FtpError {
    FtpError {
        message: message.to_string(),
        code,
    }
}

/// The limits `corvus.mavlink_ftp` sets.
#[derive(Debug, Clone, Copy)]
pub struct FtpLimits {
    pub max_file_bytes: u64,
    pub burst_rounds: i64,
    pub request_retries: i64,
    pub reply_timeout_s: f64,
    pub reply_timeout_slow_s: f64,
}

impl Default for FtpLimits {
    fn default() -> Self {
        FtpLimits {
            max_file_bytes: 8 * 1024 * 1024,
            burst_rounds: 25,
            request_retries: 3,
            reply_timeout_s: 1.0,
            reply_timeout_slow_s: 3.0,
        }
    }
}

/// What the engine asks of its bridge.
pub trait FtpHost: Send + Sync {
    fn now(&self) -> f64;
    fn slow_link(&self) -> bool;
    /// Whether the bridge is stopping.
    fn stopping(&self) -> bool;
    /// Whether there is a link at all.
    fn connected(&self) -> bool;
    /// Sends a FILE_TRANSFER_PROTOCOL payload to the vehicle under the send
    /// lock. `Ok(false)` is "the link is no longer this bridge's".
    fn send_ftp(&self, payload: &[u8]) -> Result<bool, String>;
    /// This station's system and component id, to check a reply's address.
    fn source(&self) -> (u8, u8);
    fn log(&self, _level: &str, _text: &str) {}
}

/// A cancel flag a caller may hold to stop a transfer.
pub type Cancel = Arc<AtomicBool>;

pub struct FtpEngine {
    limits: FtpLimits,
    inbox: Mutex<VecDeque<FtpReply>>,
    arrived: Condvar,
    listening: AtomicBool,
    busy: AtomicBool,
    seq: Mutex<u16>,
    cancel: Mutex<Option<Cancel>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl FtpEngine {
    pub fn new(limits: FtpLimits) -> FtpEngine {
        FtpEngine {
            limits,
            inbox: Mutex::new(VecDeque::new()),
            arrived: Condvar::new(),
            listening: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            seq: Mutex::new(0),
            cancel: Mutex::new(None),
        }
    }

    pub fn listening(&self) -> bool {
        self.listening.load(Ordering::SeqCst)
    }

    pub fn inbox_len(&self) -> usize {
        lock(&self.inbox).len()
    }

    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// Cancels the transfer in progress, if any.
    pub fn abort(&self) {
        if let Some(c) = lock(&self.cancel).as_ref() {
            c.store(true, Ordering::SeqCst);
        }
        let _guard = lock(&self.inbox);
        self.arrived.notify_all();
    }

    /// A FILE_TRANSFER_PROTOCOL message from the receive thread. Only what is
    /// addressed to this station, while a transfer listens, and an ACK or NAK
    /// is kept.
    pub fn handle_message(&self, host: &dyn FtpHost, target_system: u8, target_component: u8, payload: &[u8]) {
        if !self.listening.load(Ordering::SeqCst) || !host.connected() {
            return;
        }
        let ours = host.source();
        for (target, expected) in [(target_system, ours.0), (target_component, ours.1)] {
            if target != 0 && expected != 0 && target != expected {
                return;
            }
        }
        let Some(reply) = decode(payload) else { return };
        if reply.opcode != FTP_OP_ACK && reply.opcode != FTP_OP_NAK {
            return;
        }
        let mut inbox = lock(&self.inbox);
        if !self.listening.load(Ordering::SeqCst) {
            return;
        }
        if inbox.len() >= INBOX_MAX {
            inbox.pop_front();
        }
        inbox.push_back(reply);
        self.arrived.notify_all();
    }

    fn timeout(&self, host: &dyn FtpHost) -> f64 {
        if host.slow_link() {
            self.limits.reply_timeout_slow_s
        } else {
            self.limits.reply_timeout_s
        }
    }

    fn check_alive(&self, host: &dyn FtpHost) -> Result<(), FtpError> {
        let cancelled = lock(&self.cancel).as_ref().is_some_and(|c| c.load(Ordering::SeqCst));
        if cancelled || host.stopping() {
            return Err(error("cancelled", 0));
        }
        if !host.connected() {
            return Err(error("not connected", 0));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn send(
        &self,
        host: &dyn FtpHost,
        opcode: u8,
        seq: Option<u16>,
        session: u8,
        offset: u32,
        data: &[u8],
        size: Option<i64>,
    ) -> Result<u16, FtpError> {
        self.check_alive(host)?;
        let seq = match seq {
            Some(s) => s,
            None => {
                let mut counter = lock(&self.seq);
                *counter = counter.wrapping_add(1);
                *counter
            }
        };
        let payload = encode(seq, session, opcode, offset, data, size);
        match host.send_ftp(&payload) {
            Ok(true) => Ok(seq),
            Ok(false) => Err(error("not connected", 0)),
            Err(e) => Err(error(&format!("could not send to the vehicle: {e}"), 0)),
        }
    }

    fn next_reply(&self, host: &dyn FtpHost, timeout: f64) -> Result<Option<FtpReply>, FtpError> {
        let deadline = host.now() + timeout;
        loop {
            self.check_alive(host)?;
            if let Some(reply) = lock(&self.inbox).pop_front() {
                return Ok(Some(reply));
            }
            let remaining = deadline - host.now();
            if remaining <= 0.0 {
                return Ok(None);
            }
            let wait = Duration::from_secs_f64(remaining.min(0.1));
            let inbox = lock(&self.inbox);
            if inbox.is_empty() {
                let _ = self.arrived.wait_timeout(inbox, wait);
            }
        }
    }

    /// One request, resent with the same sequence number up to `retries`
    /// times, answered by the reply carrying that number plus one.
    #[allow(clippy::too_many_arguments)]
    fn request(
        &self,
        host: &dyn FtpHost,
        opcode: u8,
        session: u8,
        offset: u32,
        data: &[u8],
        size: Option<i64>,
        retries: i64,
    ) -> Result<FtpReply, FtpError> {
        let mut seq: Option<u16> = None;
        for _attempt in 0..retries.max(1) {
            let sent = self.send(host, opcode, seq, session, offset, data, size)?;
            seq = Some(sent);
            let deadline = host.now() + self.timeout(host);
            loop {
                let remaining = deadline - host.now();
                let Some(reply) = self.next_reply(host, remaining.max(0.0))? else {
                    break;
                };
                if reply.req_opcode == opcode && reply.seq == sent.wrapping_add(1) {
                    return Ok(reply);
                }
            }
        }
        Err(error("no answer from the vehicle", 0))
    }

    /// Opens a file read only; a session that is stuck is reset once.
    fn open(&self, host: &dyn FtpHost, path: &str) -> Result<(u8, u64), FtpError> {
        let encoded = path.as_bytes();
        if encoded.len() >= FTP_DATA_MAX {
            return Err(error("path too long", 0));
        }
        let retries = self.limits.request_retries;
        let mut reply = self.request(host, FTP_OP_OPEN_FILE_RO, 0, 0, encoded, None, retries)?;
        if reply.code() == FTP_ERR_NO_SESSIONS {
            host.log("info", "FTP: no free session on the vehicle, resetting sessions");
            let _ = self.request(host, FTP_OP_RESET_SESSIONS, 0, 0, &[], None, retries);
            reply = self.request(host, FTP_OP_OPEN_FILE_RO, 0, 0, encoded, None, retries)?;
        }
        if reply.opcode == FTP_OP_NAK {
            return Err(error(&nak_text(reply.code()), reply.code()));
        }
        let size = if reply.size >= 4 {
            u32::from_le_bytes([reply.data[0], reply.data[1], reply.data[2], reply.data[3]]) as u64
        } else {
            0
        };
        if size > self.limits.max_file_bytes {
            self.terminate(host, reply.session);
            return Err(error("the file is too large", 0));
        }
        Ok((reply.session, size))
    }

    /// Ends a session; a vehicle that does not answer is let go.
    fn terminate(&self, host: &dyn FtpHost, session: u8) {
        let _ = self.request(host, FTP_OP_TERMINATE_SESSION, session, 0, &[], None, 1);
    }

    /// BURST_READ_FILE from the first gap, until every byte is there.
    fn burst_read(
        &self,
        host: &dyn FtpHost,
        session: u8,
        size: u64,
        progress: Option<&(dyn Fn(u64, u64) + Sync)>,
    ) -> Result<Vec<u8>, FtpError> {
        let mut chunks: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
        let mut received: u64 = 0;
        let mut end: Option<u64> = if size > 0 { Some(size) } else { None };
        let idle = self.timeout(host);
        let rounds = self.limits.burst_rounds;
        let max_bytes = self.limits.max_file_bytes;
        for burst in 0..=rounds {
            let start = first_gap(&chunks);
            if end.is_some_and(|e| start >= e) {
                break;
            }
            if burst == rounds {
                return Err(error("the transfer did not complete", 0));
            }
            self.send(
                host,
                FTP_OP_BURST_READ_FILE,
                None,
                session,
                start as u32,
                &[],
                Some(FTP_DATA_MAX as i64),
            )?;
            while let Some(reply) = self.next_reply(host, idle)? {
                if reply.req_opcode != FTP_OP_BURST_READ_FILE {
                    continue;
                }
                if reply.opcode == FTP_OP_NAK {
                    if reply.code() != FTP_ERR_EOF {
                        return Err(error(&nak_text(reply.code()), reply.code()));
                    }
                    if end.is_none() {
                        let top = chunks
                            .iter()
                            .map(|(&o, d)| o as u64 + d.len() as u64)
                            .max()
                            .unwrap_or(0);
                        end = Some((reply.offset as u64).max(top));
                    }
                    break;
                }
                if reply.size != 0 {
                    if reply.offset as u64 + reply.size as u64 > max_bytes {
                        return Err(error("the file is too large", 0));
                    }
                    if !chunks.contains_key(&reply.offset) {
                        received += reply.size as u64;
                    }
                    chunks.insert(reply.offset, reply.data.clone());
                    if let Some(progress) = progress {
                        let total = end.unwrap_or(0);
                        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| progress(received, total))).is_err()
                        {
                            host.log("debug", "FTP progress callback raised");
                        }
                    }
                }
                if let Some(e) = end {
                    if received >= e && first_gap(&chunks) >= e {
                        break;
                    }
                }
                if reply.burst_complete {
                    break;
                }
            }
        }
        let total = end.unwrap_or(0) as usize;
        let mut out = vec![0u8; total];
        for (&offset, data) in &chunks {
            let offset = offset as usize;
            if offset < total {
                let take = data.len().min(total - offset);
                out[offset..offset + take].copy_from_slice(&data[..take]);
            }
        }
        Ok(out)
    }

    /// Runs one transfer at a time. The flag a caller passes cancels it.
    fn exchange<T>(&self, cancel: Option<Cancel>, f: impl FnOnce() -> Result<T, FtpError>) -> Result<T, FtpError> {
        if self
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(error("another file transfer is in progress", 0));
        }
        *lock(&self.cancel) = Some(cancel.unwrap_or_default());
        lock(&self.inbox).clear();
        self.listening.store(true, Ordering::SeqCst);
        let result = f();
        self.listening.store(false, Ordering::SeqCst);
        lock(&self.inbox).clear();
        *lock(&self.cancel) = None;
        self.busy.store(false, Ordering::SeqCst);
        result
    }

    /// Reads a whole file: open, burst read, terminate.
    pub fn read_file(
        &self,
        host: &dyn FtpHost,
        path: &str,
        progress: Option<&(dyn Fn(u64, u64) + Sync)>,
        cancel: Option<Cancel>,
    ) -> Result<Vec<u8>, FtpError> {
        self.exchange(cancel, || {
            let (session, size) = self.open(host, path)?;
            let read = self.burst_read(host, session, size, progress);
            self.terminate(host, session);
            read
        })
    }

    /// The CRC32 the vehicle computes of a file.
    pub fn file_crc32(&self, host: &dyn FtpHost, path: &str, cancel: Option<Cancel>) -> Result<u32, FtpError> {
        let encoded = path.as_bytes();
        if encoded.len() >= FTP_DATA_MAX {
            return Err(error("path too long", 0));
        }
        let retries = self.limits.request_retries;
        let reply = self.exchange(cancel, || {
            self.request(host, FTP_OP_CALC_FILE_CRC32, 0, 0, encoded, None, retries)
        })?;
        if reply.opcode == FTP_OP_NAK {
            return Err(error(&nak_text(reply.code()), reply.code()));
        }
        if reply.size < 4 {
            return Err(error("the vehicle sent no checksum", 0));
        }
        Ok(u32::from_le_bytes([
            reply.data[0],
            reply.data[1],
            reply.data[2],
            reply.data[3],
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_round_trips() {
        let raw = encode(513, 2, FTP_OP_OPEN_FILE_RO, 70000, b"/etc/x", None);
        assert_eq!(raw.len(), FTP_PAYLOAD_LEN);
        let reply = decode(&raw).unwrap();
        assert_eq!(
            (reply.seq, reply.session, reply.opcode, reply.size, reply.offset),
            (513, 2, FTP_OP_OPEN_FILE_RO, 6, 70000)
        );
        assert_eq!(reply.data, b"/etc/x");
    }

    #[test]
    fn short_and_oversized_payloads() {
        assert!(decode(&[0u8; 11]).is_none());
        let mut raw = encode(1, 0, FTP_OP_ACK, 0, b"abc", None);
        raw[4] = 255;
        raw.truncate(HEADER_LEN + 3);
        assert_eq!(decode(&raw).unwrap().data, b"abc");
    }

    #[test]
    fn the_first_gap_is_where_coverage_stops() {
        let mut chunks = BTreeMap::new();
        assert_eq!(first_gap(&chunks), 0);
        chunks.insert(0, vec![0; 10]);
        chunks.insert(10, vec![0; 10]);
        chunks.insert(30, vec![0; 10]);
        assert_eq!(first_gap(&chunks), 20);
        chunks.insert(20, vec![0; 10]);
        assert_eq!(first_gap(&chunks), 40);
    }

    #[test]
    fn a_nak_without_data_is_a_failure_and_codes_have_words() {
        let reply = FtpReply {
            seq: 0,
            session: 0,
            opcode: FTP_OP_NAK,
            size: 0,
            req_opcode: 4,
            burst_complete: false,
            offset: 0,
            data: vec![],
        };
        assert_eq!(reply.code(), FTP_ERR_FAIL);
        assert_eq!(
            nak_text(FTP_ERR_FILE_NOT_FOUND),
            "the file does not exist on the vehicle"
        );
        assert_eq!(nak_text(42), "the vehicle refused the request (error 42)");
    }
}
