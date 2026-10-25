//! MAVLink FTP, the reading half: open a file on the autopilot, read it in
//! bursts with the gaps a lossy link leaves asked for again, ask for a file's
//! CRC32, and close the session. One transfer at a time.
//!
//! The 251 byte FILE_TRANSFER_PROTOCOL payload:
//!
//! ```text
//! seq u16 | session u8 | opcode u8 | size u8 | req_opcode u8 |
//! burst_complete u8 | padding u8 | offset u32 | data[239]
//! ```
//!
//! Replies arrive on the receive thread and wait in an inbox the transfer
//! reads from. The tunables, the clock and the error type are read from the
//! module the engine was made for (`corvus.mavlink_ftp`), so a test that
//! changes them there changes them here.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList, PyModule};

use crate::lock;
use crate::protocol::host::{logger, monotonic_of, obj, with_send_lock};

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

fn nak_text(code: u8) -> String {
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

/// One decoded FILE_TRANSFER_PROTOCOL payload.
#[pyclass(name = "FtpReply", module = "corvus_link.ftp", frozen, eq)]
#[derive(Clone, PartialEq, Debug)]
pub struct FtpReply {
    #[pyo3(get)]
    seq: u16,
    #[pyo3(get)]
    session: u8,
    #[pyo3(get)]
    opcode: u8,
    #[pyo3(get)]
    size: u8,
    #[pyo3(get)]
    req_opcode: u8,
    #[pyo3(get)]
    burst_complete: bool,
    #[pyo3(get)]
    offset: u32,
    data: Vec<u8>,
}

impl FtpReply {
    fn code(&self) -> u8 {
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

#[pymethods]
impl FtpReply {
    #[new]
    #[allow(clippy::too_many_arguments)]
    fn new(
        seq: u16,
        session: u8,
        opcode: u8,
        size: u8,
        req_opcode: u8,
        burst_complete: bool,
        offset: u32,
        data: Vec<u8>,
    ) -> Self {
        FtpReply {
            seq,
            session,
            opcode,
            size,
            req_opcode,
            burst_complete,
            offset,
            data,
        }
    }

    #[getter]
    fn data<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.data)
    }

    /// The NAK error code, or 0 for an ACK.
    #[getter]
    fn error(&self) -> u8 {
        self.code()
    }

    fn __repr__(&self) -> String {
        format!(
            "FtpReply(seq={}, session={}, opcode={}, size={}, req_opcode={}, burst_complete={}, offset={}, data={} bytes)",
            self.seq,
            self.session,
            self.opcode,
            self.size,
            self.req_opcode,
            if self.burst_complete { "True" } else { "False" },
            self.offset,
            self.data.len()
        )
    }
}

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

/// The first byte offset no received chunk covers.
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

/// `encode_ftp_payload`: the payload as the list of ints the generated
/// sender expects.
#[pyfunction]
#[pyo3(signature = (seq, session, opcode, *, offset=0, data=None, size=None))]
pub fn encode_ftp_payload(
    py: Python<'_>,
    seq: i64,
    session: i64,
    opcode: i64,
    offset: i64,
    data: Option<Vec<u8>>,
    size: Option<i64>,
) -> PyResult<Bound<'_, PyList>> {
    let raw = encode(
        (seq & 0xFFFF) as u16,
        (session & 0xFF) as u8,
        (opcode & 0xFF) as u8,
        (offset & 0xFFFF_FFFF) as u32,
        &data.unwrap_or_default(),
        size,
    );
    PyList::new(py, raw.iter().map(|&b| b as i64))
}

/// `decode_ftp_payload`: a reply, or None when the payload is malformed.
#[pyfunction]
pub fn decode_ftp_payload(py: Python<'_>, payload: &Bound<'_, PyAny>) -> PyResult<Option<FtpReply>> {
    let raw = match py.get_type::<PyBytes>().call1((payload,)) {
        Ok(b) => b.downcast_into::<PyBytes>()?.as_bytes().to_vec(),
        Err(err) if crate::protocol::host::caught_by(py, &err, &["TypeError", "ValueError"]) => return Ok(None),
        Err(err) => return Err(err),
    };
    Ok(decode(&raw))
}

/// The read side of MAVLink FTP for one bridge.
#[pyclass(name = "FtpEngine", module = "corvus_link.ftp", frozen)]
pub struct FtpEngine {
    env: PyObject,
    inbox: Mutex<VecDeque<FtpReply>>,
    arrived: Condvar,
    listening: AtomicBool,
    seq: Mutex<u16>,
    cancel: Mutex<Option<PyObject>>,
}

impl FtpEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn error(&self, py: Python<'_>, message: &str, code: u8) -> PyErr {
        match self.env(py).getattr("FtpError").and_then(|t| t.call1((message, code))) {
            Ok(instance) => PyErr::from_value(instance),
            Err(err) => err,
        }
    }

    fn monotonic(&self, py: Python<'_>) -> PyResult<f64> {
        monotonic_of(&self.env(py).getattr("time")?)
    }

    fn timeout(&self, bridge: &Bound<'_, PyAny>) -> PyResult<f64> {
        let slow = bridge.call_method0("_is_slow_link")?.is_truthy()?;
        self.env(bridge.py())
            .getattr(if slow {
                "FTP_REPLY_TIMEOUT_S_SLOW"
            } else {
                "FTP_REPLY_TIMEOUT_S"
            })?
            .extract()
    }

    fn tunable_i(&self, py: Python<'_>, name: &str) -> PyResult<i64> {
        self.env(py).getattr(name)?.extract()
    }

    fn check_alive(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let cancel = lock(&self.cancel).as_ref().map(|c| c.clone_ref(py));
        let cancelled = match cancel {
            Some(c) => c.bind(py).call_method0("is_set")?.is_truthy()?,
            None => false,
        };
        if cancelled || bridge.getattr("_stop_event")?.call_method0("is_set")?.is_truthy()? {
            return Err(self.error(py, "cancelled", 0));
        }
        if bridge.getattr("_conn")?.is_none() {
            return Err(self.error(py, "not connected", 0));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    /// Send one request; returns its sequence number.
    fn send(
        &self,
        bridge: &Bound<'_, PyAny>,
        opcode: u8,
        seq: Option<u16>,
        session: u8,
        offset: u32,
        data: &[u8],
        size: Option<i64>,
    ) -> PyResult<u16> {
        let py = bridge.py();
        self.check_alive(bridge)?;
        let seq = match seq {
            Some(s) => s,
            None => {
                let mut counter = lock(&self.seq);
                *counter = counter.wrapping_add(1);
                *counter
            }
        };
        let payload = encode(seq, session, opcode, offset, data, size);
        let conn = bridge.getattr("_conn")?;
        let sent = with_send_lock(bridge, || -> PyResult<bool> {
            let current = bridge.getattr("_conn")?;
            if conn.is_none() || !conn.is(&current) {
                return Ok(false);
            }
            let payload = PyList::new(py, payload.iter().map(|&b| b as i64))?;
            conn.getattr("mav")?.call_method1(
                "file_transfer_protocol_send",
                (
                    0,
                    bridge.getattr("_target_system")?,
                    bridge.getattr("_target_component")?,
                    payload,
                ),
            )?;
            Ok(true)
        });
        match sent {
            Ok(true) => Ok(seq),
            Ok(false) => Err(self.error(py, "not connected", 0)),
            Err(err) => Err(self.error(py, &format!("could not send to the vehicle: {}", err.value(py)), 0)),
        }
    }

    /// The next queued reply, or None once `timeout` passes without one.
    fn next_reply(&self, bridge: &Bound<'_, PyAny>, timeout: f64) -> PyResult<Option<FtpReply>> {
        let py = bridge.py();
        let deadline = self.monotonic(py)? + timeout;
        loop {
            self.check_alive(bridge)?;
            if let Some(reply) = lock(&self.inbox).pop_front() {
                return Ok(Some(reply));
            }
            let remaining = deadline - self.monotonic(py)?;
            if remaining <= 0.0 {
                return Ok(None);
            }
            let wait = Duration::from_secs_f64(remaining.min(0.1));
            py.allow_threads(|| {
                let inbox = lock(&self.inbox);
                if inbox.is_empty() {
                    let _ = self.arrived.wait_timeout(inbox, wait);
                }
            });
        }
    }

    /// Send a request and wait for its answer, resending (same sequence
    /// number) on a timeout.
    #[allow(clippy::too_many_arguments)]
    fn request(
        &self,
        bridge: &Bound<'_, PyAny>,
        opcode: u8,
        session: u8,
        offset: u32,
        data: &[u8],
        size: Option<i64>,
        retries: i64,
    ) -> PyResult<FtpReply> {
        let py = bridge.py();
        let mut seq: Option<u16> = None;
        for _attempt in 0..retries.max(1) {
            let sent = self.send(bridge, opcode, seq, session, offset, data, size)?;
            seq = Some(sent);
            let deadline = self.monotonic(py)? + self.timeout(bridge)?;
            loop {
                let remaining = deadline - self.monotonic(py)?;
                let Some(reply) = self.next_reply(bridge, remaining.max(0.0))? else {
                    break;
                };
                if reply.req_opcode == opcode && reply.seq == sent.wrapping_add(1) {
                    return Ok(reply);
                }
            }
        }
        Err(self.error(py, "no answer from the vehicle", 0))
    }

    fn retries(&self, py: Python<'_>) -> PyResult<i64> {
        self.tunable_i(py, "FTP_REQUEST_RETRIES")
    }

    fn open(&self, bridge: &Bound<'_, PyAny>, path: &str) -> PyResult<(u8, u64)> {
        let py = bridge.py();
        let encoded = path.as_bytes();
        if encoded.len() >= FTP_DATA_MAX {
            return Err(self.error(py, "path too long", 0));
        }
        let retries = self.retries(py)?;
        let mut reply = self.request(bridge, FTP_OP_OPEN_FILE_RO, 0, 0, encoded, None, retries)?;
        if reply.code() == FTP_ERR_NO_SESSIONS {
            logger(py)?.call_method1("info", ("FTP: no free session on the vehicle, resetting sessions",))?;
            if let Err(err) = self.request(bridge, FTP_OP_RESET_SESSIONS, 0, 0, &[], None, retries) {
                if !self.is_ftp_error(py, &err) {
                    return Err(err);
                }
            }
            reply = self.request(bridge, FTP_OP_OPEN_FILE_RO, 0, 0, encoded, None, retries)?;
        }
        if reply.opcode == FTP_OP_NAK {
            return Err(self.error(py, &nak_text(reply.code()), reply.code()));
        }
        let size = if reply.size >= 4 {
            u32::from_le_bytes([reply.data[0], reply.data[1], reply.data[2], reply.data[3]]) as u64
        } else {
            0
        };
        let max = self.tunable_i(py, "FTP_MAX_FILE_BYTES")? as u64;
        if size > max {
            self.terminate(bridge, reply.session)?;
            return Err(self.error(py, "the file is too large", 0));
        }
        Ok((reply.session, size))
    }

    fn is_ftp_error(&self, py: Python<'_>, err: &PyErr) -> bool {
        self.env(py)
            .getattr("FtpError")
            .and_then(|t| Ok(err.is_instance(py, t.downcast::<pyo3::types::PyType>()?)))
            .unwrap_or(false)
    }

    /// Close `session`; best effort, a vehicle that misses it frees it on reset.
    fn terminate(&self, bridge: &Bound<'_, PyAny>, session: u8) -> PyResult<()> {
        match self.request(bridge, FTP_OP_TERMINATE_SESSION, session, 0, &[], None, 1) {
            Ok(_) => Ok(()),
            Err(err) if self.is_ftp_error(bridge.py(), &err) => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Read the open file in bursts, each resuming at the first gap.
    fn burst_read(
        &self,
        bridge: &Bound<'_, PyAny>,
        session: u8,
        size: u64,
        progress: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<u8>> {
        let py = bridge.py();
        let mut chunks: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
        let mut received: u64 = 0;
        let mut end: Option<u64> = if size > 0 { Some(size) } else { None };
        let idle = self.timeout(bridge)?;
        let rounds = self.tunable_i(py, "FTP_BURST_ROUNDS")?;
        let max_bytes = self.tunable_i(py, "FTP_MAX_FILE_BYTES")? as u64;
        for burst in 0..=rounds {
            let start = first_gap(&chunks);
            if end.is_some_and(|e| start >= e) {
                break;
            }
            if burst == rounds {
                return Err(self.error(py, "the transfer did not complete", 0));
            }
            self.send(
                bridge,
                FTP_OP_BURST_READ_FILE,
                None,
                session,
                start as u32,
                &[],
                Some(FTP_DATA_MAX as i64),
            )?;
            loop {
                let Some(reply) = self.next_reply(bridge, idle)? else {
                    break;
                };
                if reply.req_opcode != FTP_OP_BURST_READ_FILE {
                    continue;
                }
                if reply.opcode == FTP_OP_NAK {
                    if reply.code() != FTP_ERR_EOF {
                        return Err(self.error(py, &nak_text(reply.code()), reply.code()));
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
                        return Err(self.error(py, "the file is too large", 0));
                    }
                    if !chunks.contains_key(&reply.offset) {
                        received += reply.size as u64;
                    }
                    chunks.insert(reply.offset, reply.data.clone());
                    if let Some(progress) = progress {
                        if let Err(err) = progress.call1((received, end.unwrap_or(0))) {
                            let kwargs = pyo3::types::PyDict::new(py);
                            kwargs.set_item("exc_info", (err.get_type(py), err.value(py), err.traceback(py)))?;
                            logger(py)?.call_method("debug", ("FTP progress callback raised",), Some(&kwargs))?;
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

    /// Hold the one transfer slot and listen for replies while `f` runs.
    fn exchange<T>(
        &self,
        bridge: &Bound<'_, PyAny>,
        cancel: Option<&Bound<'_, PyAny>>,
        f: impl FnOnce() -> PyResult<T>,
    ) -> PyResult<T> {
        let py = bridge.py();
        let slot = bridge.getattr("_ftp_lock")?;
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item("blocking", false)?;
        if !slot.call_method("acquire", (), Some(&kwargs))?.is_truthy()? {
            return Err(self.error(py, "another file transfer is in progress", 0));
        }
        let cancel = match cancel {
            Some(c) if !c.is_none() => c.clone().unbind(),
            _ => py.import("threading")?.call_method0("Event")?.unbind(),
        };
        *lock(&self.cancel) = Some(cancel);
        lock(&self.inbox).clear();
        self.listening.store(true, Ordering::SeqCst);
        let result = f();
        self.listening.store(false, Ordering::SeqCst);
        lock(&self.inbox).clear();
        *lock(&self.cancel) = None;
        slot.call_method0("release")?;
        result
    }
}

#[pymethods]
impl FtpEngine {
    /// *env* holds the tunables, the clock and FtpError (``corvus.mavlink_ftp``).
    #[new]
    fn new(env: PyObject) -> Self {
        FtpEngine {
            env,
            inbox: Mutex::new(VecDeque::new()),
            arrived: Condvar::new(),
            listening: AtomicBool::new(false),
            seq: Mutex::new(0),
            cancel: Mutex::new(None),
        }
    }

    #[getter]
    fn listening(&self) -> bool {
        self.listening.load(Ordering::SeqCst)
    }

    #[setter]
    fn set_listening(&self, value: bool) {
        self.listening.store(value, Ordering::SeqCst);
    }

    /// The replies waiting, oldest first.
    #[getter]
    fn inbox<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let items: Vec<FtpReply> = lock(&self.inbox).iter().cloned().collect();
        PyList::new(py, items)
    }

    #[getter]
    fn cancel(&self, py: Python<'_>) -> PyObject {
        lock(&self.cancel)
            .as_ref()
            .map(|c| c.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }

    /// Stop the running transfer at its next wait; it raises FtpError.
    fn abort(&self, py: Python<'_>) -> PyResult<()> {
        let cancel = lock(&self.cancel).as_ref().map(|c| c.clone_ref(py));
        if let Some(c) = cancel {
            c.bind(py).call_method0("set")?;
        }
        let _guard = lock(&self.inbox);
        self.arrived.notify_all();
        Ok(())
    }

    /// Queue a reply for the running transfer. Runs on the receive thread.
    fn handle_message(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if !self.listening.load(Ordering::SeqCst) {
            return Ok(());
        }
        let conn = bridge.getattr("_conn")?;
        if conn.is_none() {
            return Ok(());
        }
        for (field, ours) in [
            ("target_system", "source_system"),
            ("target_component", "source_component"),
        ] {
            let target = msg.getattr(field).unwrap_or_else(|_| obj(py, 0).into_bound(py));
            let expected = conn.getattr(ours).unwrap_or_else(|_| obj(py, 0).into_bound(py));
            if target.is_truthy()? && expected.is_truthy()? && !target.eq(&expected)? {
                return Ok(());
            }
        }
        let payload = msg
            .getattr("payload")
            .unwrap_or_else(|_| PyBytes::new(py, b"").into_any());
        let Some(reply) = decode_ftp_payload(py, &payload)? else {
            return Ok(());
        };
        if reply.opcode != FTP_OP_ACK && reply.opcode != FTP_OP_NAK {
            return Ok(());
        }
        let mut inbox = lock(&self.inbox);
        if !self.listening.load(Ordering::SeqCst) {
            return Ok(());
        }
        if inbox.len() >= INBOX_MAX {
            inbox.pop_front();
        }
        inbox.push_back(reply);
        self.arrived.notify_all();
        Ok(())
    }

    /// Download `path` from the autopilot and return its bytes.
    #[pyo3(signature = (bridge, path, progress=None, cancel=None))]
    fn read_file<'py>(
        &self,
        bridge: &Bound<'py, PyAny>,
        path: &str,
        progress: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let py = bridge.py();
        let progress = progress.filter(|p| !p.is_none());
        let data = self.exchange(bridge, cancel, || {
            let (session, size) = self.open(bridge, path)?;
            let read = self.burst_read(bridge, session, size, progress);
            let closed = self.terminate(bridge, session);
            let data = read?;
            closed?;
            Ok(data)
        })?;
        Ok(PyBytes::new(py, &data))
    }

    /// The CRC32 the vehicle computes of `path`, without transferring it.
    #[pyo3(signature = (bridge, path, cancel=None))]
    fn file_crc32(&self, bridge: &Bound<'_, PyAny>, path: &str, cancel: Option<&Bound<'_, PyAny>>) -> PyResult<u32> {
        let py = bridge.py();
        let encoded = path.as_bytes();
        if encoded.len() >= FTP_DATA_MAX {
            return Err(self.error(py, "path too long", 0));
        }
        let retries = self.retries(py)?;
        let reply = self.exchange(bridge, cancel, || {
            self.request(bridge, FTP_OP_CALC_FILE_CRC32, 0, 0, encoded, None, retries)
        })?;
        if reply.opcode == FTP_OP_NAK {
            return Err(self.error(py, &nak_text(reply.code()), reply.code()));
        }
        if reply.size < 4 {
            return Err(self.error(py, "the vehicle sent no checksum", 0));
        }
        Ok(u32::from_le_bytes([
            reply.data[0],
            reply.data[1],
            reply.data[2],
            reply.data[3],
        ]))
    }

    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.env)?;
        if let Ok(cancel) = self.cancel.try_lock() {
            if let Some(c) = cancel.as_ref() {
                visit.call(c)?;
            }
        }
        Ok(())
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.ftp")?;
    m.add_class::<FtpEngine>()?;
    m.add_class::<FtpReply>()?;
    m.add_function(wrap_pyfunction!(encode_ftp_payload, &m)?)?;
    m.add_function(wrap_pyfunction!(decode_ftp_payload, &m)?)?;
    for (name, value) in [
        ("FTP_OP_TERMINATE_SESSION", FTP_OP_TERMINATE_SESSION),
        ("FTP_OP_RESET_SESSIONS", FTP_OP_RESET_SESSIONS),
        ("FTP_OP_OPEN_FILE_RO", FTP_OP_OPEN_FILE_RO),
        ("FTP_OP_READ_FILE", FTP_OP_READ_FILE),
        ("FTP_OP_CALC_FILE_CRC32", FTP_OP_CALC_FILE_CRC32),
        ("FTP_OP_BURST_READ_FILE", FTP_OP_BURST_READ_FILE),
        ("FTP_OP_ACK", FTP_OP_ACK),
        ("FTP_OP_NAK", FTP_OP_NAK),
        ("FTP_ERR_FAIL", FTP_ERR_FAIL),
        ("FTP_ERR_FAIL_ERRNO", FTP_ERR_FAIL_ERRNO),
        ("FTP_ERR_INVALID_DATA_SIZE", FTP_ERR_INVALID_DATA_SIZE),
        ("FTP_ERR_INVALID_SESSION", FTP_ERR_INVALID_SESSION),
        ("FTP_ERR_NO_SESSIONS", FTP_ERR_NO_SESSIONS),
        ("FTP_ERR_EOF", FTP_ERR_EOF),
        ("FTP_ERR_UNKNOWN_COMMAND", FTP_ERR_UNKNOWN_COMMAND),
        ("FTP_ERR_FILE_EXISTS", FTP_ERR_FILE_EXISTS),
        ("FTP_ERR_FILE_PROTECTED", FTP_ERR_FILE_PROTECTED),
        ("FTP_ERR_FILE_NOT_FOUND", FTP_ERR_FILE_NOT_FOUND),
    ] {
        m.add(name, value)?;
    }
    m.add("FTP_PAYLOAD_LEN", FTP_PAYLOAD_LEN)?;
    m.add("FTP_DATA_MAX", FTP_DATA_MAX)?;
    parent.add_submodule(&m)?;
    py.import("sys")?.getattr("modules")?.set_item("corvus_link.ftp", &m)?;
    Ok(())
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
    fn a_short_payload_is_not_a_reply() {
        assert!(decode(&[0u8; 11]).is_none());
    }

    #[test]
    fn a_size_beyond_the_payload_is_clipped() {
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
    fn a_nak_without_data_is_a_failure() {
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
