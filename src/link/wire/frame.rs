//! The `MAVLink` protocol object: framing, sequence numbers, signing and
//! the byte-stream parser, with the behaviour of pymavlink's generated
//! `MAVLink` class.
//!
//! Every object here is shared between the receive thread and whichever
//! thread sends, so state sits behind short-lived mutexes that are never held
//! across a call back into Python.

use std::collections::HashMap;
use std::sync::Mutex;

use pyo3::exceptions::{PyAttributeError, PyException};
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyDict, PyList, PyTuple};
use pyo3::{PyTraverseError, PyVisit};
use sha2::{Digest, Sha256};

use crate::lock;
use crate::wire::codec;
use crate::wire::crc;
use crate::wire::defs::{self, MsgDef, Wire};
use crate::wire::message::{self, Header, Kind, Message};

pyo3::create_exception!(corvus_link, MAVError, PyException, "MAVLink error class");

pub const PROTOCOL_MARKER_V1: u8 = 0xFE;
pub const PROTOCOL_MARKER_V2: u8 = 0xFD;
pub const HEADER_LEN_V1: usize = 6;
pub const HEADER_LEN_V2: usize = 10;
pub const SIGNATURE_BLOCK_LEN: usize = 13;
pub const IFLAG_SIGNED: u8 = 0x01;

pub fn mav_error(py: Python<'_>, message: impl Into<String>) -> PyErr {
    let message = message.into();
    let err = MAVError::new_err(message.clone());
    let _ = err.value(py).setattr("message", message);
    err
}

fn error_message(py: Python<'_>, err: &PyErr) -> String {
    err.value(py)
        .getattr("message")
        .and_then(|m| m.extract::<String>())
        .unwrap_or_else(|_| err.value(py).to_string())
}

pub use crate::plain::time::now;

fn logger_info(py: Python<'_>, text: &str) {
    if let Ok(logging) = py.import("logging") {
        if let Ok(logger) = logging.call_method1("getLogger", ("corvus_link",)) {
            let _ = logger.call_method1("info", (text,));
        }
    }
}

pub struct SigningState {
    pub secret_key: Option<Vec<u8>>,
    pub timestamp: u64,
    pub link_id: i64,
    pub sign_outgoing: bool,
    pub stream_timestamps: HashMap<(u8, u8, u8), u64>,
    pub sig_count: u64,
    pub badsig_count: u64,
    pub goodsig_count: u64,
    pub unsigned_count: u64,
    pub reject_count: u64,
}

/// `MAVLinkSigning`.
#[pyclass(name = "MAVLinkSigning", module = "corvus_link", frozen)]
pub struct Signing {
    pub state: Mutex<SigningState>,
    pub allow_unsigned_callback: Mutex<Option<PyObject>>,
}

impl Default for Signing {
    fn default() -> Self {
        Self::new()
    }
}

impl Signing {
    pub fn new() -> Self {
        Signing {
            state: Mutex::new(SigningState {
                secret_key: None,
                timestamp: 0,
                link_id: 0,
                sign_outgoing: false,
                stream_timestamps: HashMap::new(),
                sig_count: 0,
                badsig_count: 0,
                goodsig_count: 0,
                unsigned_count: 0,
                reject_count: 0,
            }),
            allow_unsigned_callback: Mutex::new(None),
        }
    }
}

#[pymethods]
impl Signing {
    #[new]
    fn py_new() -> Self {
        Signing::new()
    }

    #[getter]
    fn secret_key<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyBytes>> {
        lock(&self.state).secret_key.as_ref().map(|k| PyBytes::new(py, k))
    }

    #[setter]
    fn set_secret_key(&self, value: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let key = match value {
            None => None,
            Some(v) if v.is_none() => None,
            Some(v) => Some(crc::bytes_of(v)?),
        };
        lock(&self.state).secret_key = key;
        Ok(())
    }

    #[getter]
    fn timestamp(&self) -> u64 {
        lock(&self.state).timestamp
    }

    #[setter]
    fn set_timestamp(&self, value: u64) {
        lock(&self.state).timestamp = value;
    }

    #[getter]
    fn link_id(&self) -> i64 {
        lock(&self.state).link_id
    }

    #[setter]
    fn set_link_id(&self, value: i64) {
        lock(&self.state).link_id = value;
    }

    #[getter]
    fn sign_outgoing(&self) -> bool {
        lock(&self.state).sign_outgoing
    }

    #[setter]
    fn set_sign_outgoing(&self, value: bool) {
        lock(&self.state).sign_outgoing = value;
    }

    #[getter]
    fn allow_unsigned_callback(&self, py: Python<'_>) -> PyObject {
        lock(&self.allow_unsigned_callback)
            .as_ref()
            .map(|c| c.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }

    #[setter]
    fn set_allow_unsigned_callback(&self, value: Option<PyObject>) {
        *lock(&self.allow_unsigned_callback) = value;
    }

    #[getter]
    fn stream_timestamps<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        for (k, v) in lock(&self.state).stream_timestamps.iter() {
            d.set_item(*k, *v)?;
        }
        Ok(d)
    }

    #[getter]
    fn sig_count(&self) -> u64 {
        lock(&self.state).sig_count
    }

    #[setter]
    fn set_sig_count(&self, value: u64) {
        lock(&self.state).sig_count = value;
    }
    #[getter]
    fn badsig_count(&self) -> u64 {
        lock(&self.state).badsig_count
    }

    #[setter]
    fn set_badsig_count(&self, value: u64) {
        lock(&self.state).badsig_count = value;
    }
    #[getter]
    fn goodsig_count(&self) -> u64 {
        lock(&self.state).goodsig_count
    }

    #[setter]
    fn set_goodsig_count(&self, value: u64) {
        lock(&self.state).goodsig_count = value;
    }
    #[getter]
    fn unsigned_count(&self) -> u64 {
        lock(&self.state).unsigned_count
    }

    #[setter]
    fn set_unsigned_count(&self, value: u64) {
        lock(&self.state).unsigned_count = value;
    }
    #[getter]
    fn reject_count(&self) -> u64 {
        lock(&self.state).reject_count
    }

    #[setter]
    fn set_reject_count(&self, value: u64) {
        lock(&self.state).reject_count = value;
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        if let Ok(guard) = self.allow_unsigned_callback.try_lock() {
            if let Some(cb) = guard.as_ref() {
                visit.call(cb)?;
            }
        }
        Ok(())
    }

    fn __clear__(&self) {
        if let Ok(mut guard) = self.allow_unsigned_callback.try_lock() {
            *guard = None;
        }
    }
}

pub struct ParserState {
    pub seq: i64,
    pub src_system: i64,
    pub src_component: i64,
    pub robust_parsing: bool,
    pub buf: Vec<u8>,
    pub buf_index: usize,
    pub expected_length: usize,
    pub have_prefix_error: bool,
    pub total_packets_sent: u64,
    pub total_bytes_sent: u64,
    pub total_packets_received: u64,
    pub total_bytes_received: u64,
    pub total_receive_errors: u64,
    pub startup_time: f64,
    pub protocol_marker: i64,
    pub little_endian: bool,
    pub crc_extra: bool,
    pub sort_fields: bool,
}

impl ParserState {
    fn buf_len(&self) -> usize {
        self.buf.len() - self.buf_index
    }
}

#[derive(Default)]
pub struct Refs {
    pub file: Option<PyObject>,
    pub callback: Option<PyObject>,
    pub callback_args: Option<PyObject>,
    pub callback_kwargs: Option<PyObject>,
    pub send_callback: Option<PyObject>,
    pub send_callback_args: Option<PyObject>,
    pub send_callback_kwargs: Option<PyObject>,
}

/// `MAVLink`: one per link and wire version.
#[pyclass(name = "MAVLink", module = "corvus_link", frozen)]
pub struct Mavlink {
    pub wire: Wire,
    pub state: Mutex<ParserState>,
    pub refs: Mutex<Refs>,
    pub signing: Mutex<Py<Signing>>,
}

impl Mavlink {
    pub fn create(
        py: Python<'_>,
        wire: Wire,
        file: Option<PyObject>,
        src_system: i64,
        src_component: i64,
    ) -> PyResult<Py<Mavlink>> {
        let file = file.filter(|f| !f.is_none(py));
        Py::new(
            py,
            Mavlink {
                wire,
                state: Mutex::new(ParserState {
                    seq: 0,
                    src_system,
                    src_component,
                    robust_parsing: false,
                    buf: Vec::new(),
                    buf_index: 0,
                    expected_length: HEADER_LEN_V1 + 2,
                    have_prefix_error: false,
                    total_packets_sent: 0,
                    total_bytes_sent: 0,
                    total_packets_received: 0,
                    total_bytes_received: 0,
                    total_receive_errors: 0,
                    startup_time: now(),
                    protocol_marker: 253,
                    little_endian: true,
                    crc_extra: true,
                    sort_fields: true,
                }),
                refs: Mutex::new(Refs {
                    file,
                    ..Default::default()
                }),
                signing: Mutex::new(Py::new(py, Signing::new())?),
            },
        )
    }

    pub fn signing<'py>(&self, py: Python<'py>) -> Bound<'py, Signing> {
        lock(&self.signing).bind(py).clone()
    }

    pub fn set_robust_parsing(&self, value: bool) {
        lock(&self.state).robust_parsing = value;
    }

    pub fn bytes_needed_value(&self) -> usize {
        let state = lock(&self.state);
        let need = state.expected_length as isize - state.buf_len() as isize;
        if need <= 0 {
            1
        } else {
            need as usize
        }
    }

    /// Feed bytes and return at most one message, as `parse_char` does.
    pub fn parse_bytes<'py>(slf: &Bound<'py, Self>, data: &[u8]) -> PyResult<Option<Bound<'py, PyAny>>> {
        let py = slf.py();
        let this = slf.get();
        {
            let mut state = lock(&this.state);
            state.buf.extend_from_slice(data);
            state.total_bytes_received += data.len() as u64;
        }
        let parsed = Self::parse_legacy(slf);
        if let Ok(Some(_)) = &parsed {
            lock(&this.state).total_packets_received += 1;
        }
        {
            let mut state = lock(&this.state);
            if state.buf_index != 0 && state.buf_len() == 0 {
                state.buf = Vec::new();
                state.buf_index = 0;
            }
        }
        let m = parsed?;
        if let Some(m) = &m {
            let (callback, args, kwargs) = {
                let refs = lock(&this.refs);
                (
                    refs.callback.as_ref().map(|c| c.clone_ref(py)),
                    refs.callback_args.as_ref().map(|c| c.clone_ref(py)),
                    refs.callback_kwargs.as_ref().map(|c| c.clone_ref(py)),
                )
            };
            if let (Some(cb), Some(args), Some(kwargs)) = (callback, args, kwargs) {
                let mut all = vec![m.clone().unbind()];
                for a in args.bind(py).try_iter()? {
                    all.push(a?.unbind());
                }
                cb.bind(py)
                    .call(PyTuple::new(py, all)?, Some(kwargs.bind(py).downcast::<PyDict>()?))?;
            }
        }
        Ok(m)
    }

    fn parse_legacy<'py>(slf: &Bound<'py, Self>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let py = slf.py();
        let this = slf.get();
        let mut state = lock(&this.state);
        let mut header_len = HEADER_LEN_V1;
        if state.buf_len() >= 1 && state.buf[state.buf_index] == PROTOCOL_MARKER_V2 {
            header_len = HEADER_LEN_V2;
        }
        if state.buf_len() >= 1
            && state.buf[state.buf_index] != PROTOCOL_MARKER_V1
            && state.buf[state.buf_index] != PROTOCOL_MARKER_V2
        {
            let magic = state.buf[state.buf_index];
            state.buf_index += 1;
            if state.robust_parsing {
                let start = state.buf_index - 1;
                while state.buf_len() >= 1
                    && state.buf[state.buf_index] != PROTOCOL_MARKER_V1
                    && state.buf[state.buf_index] != PROTOCOL_MARKER_V2
                {
                    state.buf_index += 1;
                }
                let data = state.buf[start..state.buf_index].to_vec();
                state.expected_length = header_len + 2;
                state.total_receive_errors += 1;
                drop(state);
                return Ok(Some(message::bad_data(py, &data, "Bad prefix", this.wire)?.into_any()));
            }
            if state.have_prefix_error {
                return Ok(None);
            }
            state.have_prefix_error = true;
            state.total_receive_errors += 1;
            drop(state);
            return Err(mav_error(py, format!("invalid MAVLink prefix '{magic}'")));
        }
        state.have_prefix_error = false;
        let mut magic = 0u8;
        let mut incompat_flags = 0u8;
        if state.buf_len() >= 3 {
            let i = state.buf_index;
            magic = state.buf[i];
            state.expected_length = state.buf[i + 1] as usize;
            incompat_flags = state.buf[i + 2];
            if magic == PROTOCOL_MARKER_V2 && (incompat_flags & IFLAG_SIGNED) != 0 {
                state.expected_length += SIGNATURE_BLOCK_LEN;
            }
            state.expected_length += header_len + 2;
        }
        if state.expected_length >= header_len + 2 && state.buf_len() >= state.expected_length {
            let start = state.buf_index;
            let mbuf = state.buf[start..start + state.expected_length].to_vec();
            state.buf_index += state.expected_length;
            state.expected_length = header_len + 2;
            let robust = state.robust_parsing;
            drop(state);
            let bad_flags = magic == PROTOCOL_MARKER_V2 && (incompat_flags & !IFLAG_SIGNED) != 0;
            let flags_text = || {
                format!(
                    "invalid incompat_flags 0x{:x} 0x{:x} {}",
                    incompat_flags,
                    magic,
                    header_len + 2
                )
            };
            if robust {
                let decoded = if bad_flags {
                    Err(mav_error(py, flags_text()))
                } else {
                    Self::decode_frame(slf, &mbuf)
                };
                return match decoded {
                    Ok(m) => Ok(Some(m)),
                    Err(err) if err.is_instance_of::<MAVError>(py) => {
                        lock(&this.state).total_receive_errors += 1;
                        let reason = error_message(py, &err);
                        Ok(Some(message::bad_data(py, &mbuf, &reason, this.wire)?.into_any()))
                    }
                    Err(err) => Err(err),
                };
            }
            if bad_flags {
                return Err(mav_error(py, flags_text()));
            }
            return Self::decode_frame(slf, &mbuf).map(Some);
        }
        Ok(None)
    }

    /// `check_signature`. The stream table and the outgoing timestamp are
    /// updated exactly as pymavlink updates them, mismatches included.
    pub fn check_signature_bytes(&self, py: Python<'_>, msgbuf: &[u8], src_system: u8, src_component: u8) -> bool {
        let signing = self.signing(py);
        let signing = signing.get();
        let mut state = lock(&signing.state);
        let key = state.secret_key.clone().unwrap_or_default();
        let n = msgbuf.len();
        if n < SIGNATURE_BLOCK_LEN {
            return false;
        }
        let ts = &msgbuf[n - 12..n - 6];
        let timestamp = u32::from_le_bytes([ts[0], ts[1], ts[2], ts[3]]) as u64
            + ((u16::from_le_bytes([ts[4], ts[5]]) as u64) << 32);
        let link_id = msgbuf[n - 13];
        let stream_key = (link_id, src_system, src_component);
        let mut log = None;
        if let Some(&last) = state.stream_timestamps.get(&stream_key) {
            if timestamp <= last {
                drop(state);
                logger_info(py, "old timestamp");
                return false;
            }
        } else {
            if timestamp + 6000 * 1000 < state.timestamp {
                let year = 100.0 * 1000.0 * 60.0 * 60.0 * 24.0 * 365.0;
                let text = format!(
                    "bad new stream {} {}",
                    timestamp as f64 / year,
                    state.timestamp as f64 / year
                );
                drop(state);
                logger_info(py, &text);
                return false;
            }
            log = Some("new stream");
        }
        state.stream_timestamps.insert(stream_key, timestamp);
        let mut hasher = Sha256::new();
        hasher.update(&key);
        hasher.update(&msgbuf[..n - 6]);
        let digest = hasher.finalize();
        let ok = digest[..6] == msgbuf[n - 6..];
        if ok {
            state.timestamp = state.timestamp.max(timestamp);
        }
        drop(state);
        if let Some(text) = log {
            logger_info(py, text);
        }
        if !ok {
            logger_info(py, "sig mismatch");
        }
        ok
    }

    pub fn decode_frame<'py>(slf: &Bound<'py, Self>, msgbuf: &[u8]) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.get();
        let header_len;
        let (magic, mlen, incompat_flags, compat_flags, seq, src_system, src_component, msg_id);
        if msgbuf.first() != Some(&PROTOCOL_MARKER_V1) {
            header_len = HEADER_LEN_V2;
            if msgbuf.len() < header_len {
                return Err(mav_error(
                    py,
                    "Unable to unpack MAVLink header: unpack requires a buffer of 10 bytes",
                ));
            }
            magic = msgbuf[0];
            mlen = msgbuf[1] as usize;
            incompat_flags = msgbuf[2];
            compat_flags = msgbuf[3];
            seq = msgbuf[4];
            src_system = msgbuf[5];
            src_component = msgbuf[6];
            msg_id = msgbuf[7] as u32 | ((msgbuf[8] as u32) << 8) | ((msgbuf[9] as u32) << 16);
        } else {
            header_len = HEADER_LEN_V1;
            if msgbuf.len() < header_len {
                return Err(mav_error(
                    py,
                    "Unable to unpack MAVLink header: unpack requires a buffer of 6 bytes",
                ));
            }
            magic = msgbuf[0];
            mlen = msgbuf[1] as usize;
            seq = msgbuf[2];
            src_system = msgbuf[3];
            src_component = msgbuf[4];
            msg_id = msgbuf[5] as u32;
            incompat_flags = 0;
            compat_flags = 0;
        }
        let signature_len = if incompat_flags & IFLAG_SIGNED != 0 {
            SIGNATURE_BLOCK_LEN
        } else {
            0
        };
        if magic != PROTOCOL_MARKER_V1 && magic != PROTOCOL_MARKER_V2 {
            return Err(mav_error(py, format!("invalid MAVLink prefix '0x{magic:x}'")));
        }
        let got = msgbuf.len() as isize - (header_len + 2 + signature_len) as isize;
        if mlen as isize != got {
            return Err(mav_error(
                py,
                format!(
                    "invalid MAVLink message length. Got {got} expected {mlen}, msgId={msg_id} headerlen={header_len}"
                ),
            ));
        }
        let Some(def) = defs::message_by_id(msg_id, this.wire) else {
            return Ok(message::unknown(py, msg_id, msgbuf, this.wire)?.into_any());
        };
        let tail = msgbuf.len() - (2 + signature_len);
        let crc = u16::from_le_bytes([msgbuf[tail], msgbuf[tail + 1]]);
        let mut crcbuf = msgbuf[1..tail].to_vec();
        crcbuf.push(def.crc_extra);
        let crc2 = crc::x25(&crcbuf);
        let ignore_crc = std::env::var("MAV_IGNORE_CRC")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false);
        if crc != crc2 && !ignore_crc {
            return Err(mav_error(
                py,
                format!("invalid MAVLink CRC in msgID {msg_id} 0x{crc:04x} should be 0x{crc2:04x}"),
            ));
        }

        let mut sig_ok = false;
        let signing = this.signing(py);
        let has_key = {
            let mut s = lock(&signing.get().state);
            if signature_len == SIGNATURE_BLOCK_LEN {
                s.sig_count += 1;
            }
            s.secret_key.is_some()
        };
        if has_key {
            let callback = lock(&signing.get().allow_unsigned_callback)
                .as_ref()
                .map(|c| c.clone_ref(py));
            let mut accept = false;
            let ask = |accept: &mut bool| -> PyResult<()> {
                if let Some(cb) = &callback {
                    *accept = cb.bind(py).call1((slf.clone(), msg_id))?.is_truthy()?;
                    let mut s = lock(&signing.get().state);
                    if *accept {
                        s.unsigned_count += 1;
                    } else {
                        s.reject_count += 1;
                    }
                }
                Ok(())
            };
            if signature_len == SIGNATURE_BLOCK_LEN {
                sig_ok = this.check_signature_bytes(py, msgbuf, src_system, src_component);
                accept = sig_ok;
                {
                    let mut s = lock(&signing.get().state);
                    if sig_ok {
                        s.goodsig_count += 1;
                    } else {
                        s.badsig_count += 1;
                    }
                }
                if !accept {
                    ask(&mut accept)?;
                }
            } else {
                ask(&mut accept)?;
            }
            if !accept {
                return Err(mav_error(py, "Invalid signature"));
            }
        }

        let csize = def.payload_len(this.wire);
        let mut mbuf = if tail > header_len {
            msgbuf[header_len..tail].to_vec()
        } else {
            Vec::new()
        };
        mbuf.resize(csize, 0);
        let values = codec::unpack_payload(py, def, this.wire, &mbuf)?;
        let m = build_decoded(py, def, this.wire, &values)?;
        {
            let mut meta = m.get().meta();
            meta.signed = sig_ok;
            if sig_ok {
                meta.link_id = Some(msgbuf[msgbuf.len() - 13]);
            }
            meta.msgbuf = msgbuf.to_vec();
            meta.payload = Some(if tail > 6 { msgbuf[6..tail].to_vec() } else { Vec::new() });
            meta.crc = Some(crc);
            meta.header = Header {
                msg_id: msg_id as i64,
                incompat_flags: incompat_flags as i64,
                compat_flags: compat_flags as i64,
                mlen: mlen as i64,
                seq: seq as i64,
                src_system: src_system as i64,
                src_component: src_component as i64,
            };
        }
        Ok(m.into_any())
    }

    pub fn file_obj(&self, py: Python<'_>) -> Option<PyObject> {
        lock(&self.refs).file.as_ref().map(|f| f.clone_ref(py))
    }

    /// `send`: pack, write to the file, then advance the sequence.
    pub fn send_message<'py>(slf: &Bound<'py, Self>, mavmsg: &Bound<'py, PyAny>, force_mavlink1: bool) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        let kwargs = PyDict::new(py);
        kwargs.set_item("force_mavlink1", force_mavlink1)?;
        let buf = mavmsg.call_method("pack", (slf.clone(),), Some(&kwargs))?;
        let len = buf.len()?;
        let file = this.file_obj(py);
        match file {
            Some(f) => {
                f.bind(py).call_method1("write", (buf,))?;
            }
            None => {
                return Err(PyAttributeError::new_err("'NoneType' object has no attribute 'write'"));
            }
        }
        {
            let mut state = lock(&this.state);
            state.seq = (state.seq + 1).rem_euclid(256);
            state.total_packets_sent += 1;
            state.total_bytes_sent += len as u64;
        }
        let (cb, args, kw) = {
            let refs = lock(&this.refs);
            (
                refs.send_callback.as_ref().map(|c| c.clone_ref(py)),
                refs.send_callback_args.as_ref().map(|c| c.clone_ref(py)),
                refs.send_callback_kwargs.as_ref().map(|c| c.clone_ref(py)),
            )
        };
        if let (Some(cb), Some(args), Some(kw)) = (cb, args, kw) {
            let mut all = vec![mavmsg.clone().unbind()];
            for a in args.bind(py).try_iter()? {
                all.push(a?.unbind());
            }
            cb.bind(py)
                .call(PyTuple::new(py, all)?, Some(kw.bind(py).downcast::<PyDict>()?))?;
        }
        Ok(())
    }
}

/// A message built from decoded values: char fields arrive as bytes and get
/// their string form natively.
fn build_decoded<'py>(
    py: Python<'py>,
    def: &'static MsgDef,
    wire: Wire,
    values: &[Bound<'py, PyAny>],
) -> PyResult<Bound<'py, Message>> {
    let obj = Bound::new(
        py,
        Message {
            meta: Mutex::new(Message::new_meta(Kind::Def(def), wire)),
        },
    )?;
    let any = obj.as_any();
    for (field, value) in def.fields(wire).iter().zip(values) {
        if field.ty == defs::Ty::Char {
            let bytes: &[u8] = value.downcast::<PyBytes>()?.as_bytes();
            any.setattr(format!("_{}_raw", field.name).as_str(), value)?;
            any.setattr(field.name, codec::ascii_replace(bytes))?;
        } else {
            any.setattr(field.name, value)?;
        }
    }
    Ok(obj)
}

/// `_pack`: trim, header, CRC_EXTRA, optional signature. Stores the result
/// on the message exactly as pymavlink does.
pub fn frame_message(
    py: Python<'_>,
    msg: &Message,
    mav: &Bound<'_, PyAny>,
    def: &MsgDef,
    wire: Wire,
    payload: Vec<u8>,
    force_mavlink1: bool,
) -> PyResult<Vec<u8>> {
    let v2 = wire == Wire::V2 && !force_mavlink1;
    let mut plen = payload.len();
    if v2 {
        while plen > 1 && payload[plen - 1] == 0 {
            plen -= 1;
        }
    }
    let payload = payload[..plen].to_vec();

    // Read what the protocol object would give pymavlink's _pack: works for
    // a Rust MAVLink and for anything else with the same attributes.
    let (seq, src_system, src_component, sign_outgoing, signing_obj) = if let Ok(m) = mav.downcast::<Mavlink>() {
        let (seq, sys, comp) = {
            let s = lock(&m.get().state);
            (s.seq, s.src_system, s.src_component)
        };
        let signing = m.get().signing(py);
        let outgoing = lock(&signing.get().state).sign_outgoing;
        (seq, sys, comp, outgoing, Some(signing.into_any()))
    } else {
        let signing = mav.getattr("signing")?;
        (
            mav.getattr("seq")?.extract()?,
            mav.getattr("srcSystem")?.extract()?,
            mav.getattr("srcComponent")?.extract()?,
            signing.getattr("sign_outgoing")?.is_truthy()?,
            Some(signing),
        )
    };
    let mut incompat_flags = 0i64;
    if sign_outgoing {
        incompat_flags |= IFLAG_SIGNED as i64;
    }
    let header = Header {
        msg_id: def.id as i64,
        incompat_flags,
        compat_flags: 0,
        mlen: payload.len() as i64,
        seq,
        src_system,
        src_component,
    };
    let mut msgbuf = header.pack(py, v2)?;
    msgbuf.extend_from_slice(&payload);
    let mut crc = crc::x25(&msgbuf[1..]);
    crc = crc::accumulate(crc, &[def.crc_extra]);
    msgbuf.extend_from_slice(&crc.to_le_bytes());
    if sign_outgoing && !force_mavlink1 {
        sign_packet(py, &mut msgbuf, signing_obj.as_ref().unwrap())?;
    }
    let mut meta = msg.meta();
    meta.payload = Some(payload);
    meta.header = header;
    meta.crc = Some(crc);
    meta.msgbuf = msgbuf.clone();
    Ok(msgbuf)
}

fn sign_packet(py: Python<'_>, msgbuf: &mut Vec<u8>, signing: &Bound<'_, PyAny>) -> PyResult<()> {
    if let Ok(s) = signing.downcast::<Signing>() {
        let mut state = lock(&s.get().state);
        let Some(key) = state.secret_key.clone() else {
            return Err(pyo3::exceptions::PyAssertionError::new_err(()));
        };
        let link_id = u8::try_from(state.link_id)
            .map_err(|_| codec::struct_error(py, "ubyte format requires 0 <= number <= 255"))?;
        msgbuf.push(link_id);
        msgbuf.extend_from_slice(&state.timestamp.to_le_bytes()[..6]);
        let mut hasher = Sha256::new();
        hasher.update(&key);
        hasher.update(&msgbuf[..]);
        msgbuf.extend_from_slice(&hasher.finalize()[..6]);
        state.timestamp += 1;
        return Ok(());
    }
    let key = signing.getattr("secret_key")?;
    if key.is_none() {
        return Err(pyo3::exceptions::PyAssertionError::new_err(()));
    }
    let key = crc::bytes_of(&key)?;
    let link_id: u8 = signing.getattr("link_id")?.extract()?;
    let timestamp: u64 = signing.getattr("timestamp")?.extract()?;
    msgbuf.push(link_id);
    msgbuf.extend_from_slice(&timestamp.to_le_bytes()[..6]);
    let mut hasher = Sha256::new();
    hasher.update(&key);
    hasher.update(&msgbuf[..]);
    msgbuf.extend_from_slice(&hasher.finalize()[..6]);
    signing.setattr("timestamp", timestamp + 1)?;
    Ok(())
}

#[pymethods]
impl Mavlink {
    #[getter]
    fn seq(&self) -> i64 {
        lock(&self.state).seq
    }

    #[setter]
    fn set_seq(&self, value: i64) {
        lock(&self.state).seq = value;
    }

    #[getter(srcSystem)]
    fn src_system(&self) -> i64 {
        lock(&self.state).src_system
    }

    #[setter(srcSystem)]
    fn set_src_system(&self, value: i64) {
        lock(&self.state).src_system = value;
    }

    #[getter(srcComponent)]
    fn src_component(&self) -> i64 {
        lock(&self.state).src_component
    }

    #[setter(srcComponent)]
    fn set_src_component(&self, value: i64) {
        lock(&self.state).src_component = value;
    }

    #[getter(robust_parsing)]
    fn get_robust_parsing(&self) -> bool {
        lock(&self.state).robust_parsing
    }

    #[setter(robust_parsing)]
    fn set_robust_parsing_attr(&self, value: bool) {
        lock(&self.state).robust_parsing = value;
    }
    #[getter(expected_length)]
    fn get_expected_length(&self) -> usize {
        lock(&self.state).expected_length
    }

    #[setter(expected_length)]
    fn set_expected_length(&self, value: usize) {
        lock(&self.state).expected_length = value;
    }
    #[getter(buf_index)]
    fn get_buf_index(&self) -> usize {
        lock(&self.state).buf_index
    }

    #[setter(buf_index)]
    fn set_buf_index(&self, value: usize) {
        lock(&self.state).buf_index = value;
    }
    #[getter(have_prefix_error)]
    fn get_have_prefix_error(&self) -> bool {
        lock(&self.state).have_prefix_error
    }

    #[setter(have_prefix_error)]
    fn set_have_prefix_error(&self, value: bool) {
        lock(&self.state).have_prefix_error = value;
    }
    #[getter(total_packets_sent)]
    fn get_total_packets_sent(&self) -> u64 {
        lock(&self.state).total_packets_sent
    }

    #[setter(total_packets_sent)]
    fn set_total_packets_sent(&self, value: u64) {
        lock(&self.state).total_packets_sent = value;
    }
    #[getter(total_bytes_sent)]
    fn get_total_bytes_sent(&self) -> u64 {
        lock(&self.state).total_bytes_sent
    }

    #[setter(total_bytes_sent)]
    fn set_total_bytes_sent(&self, value: u64) {
        lock(&self.state).total_bytes_sent = value;
    }
    #[getter(total_packets_received)]
    fn get_total_packets_received(&self) -> u64 {
        lock(&self.state).total_packets_received
    }

    #[setter(total_packets_received)]
    fn set_total_packets_received(&self, value: u64) {
        lock(&self.state).total_packets_received = value;
    }
    #[getter(total_bytes_received)]
    fn get_total_bytes_received(&self) -> u64 {
        lock(&self.state).total_bytes_received
    }

    #[setter(total_bytes_received)]
    fn set_total_bytes_received(&self, value: u64) {
        lock(&self.state).total_bytes_received = value;
    }
    #[getter(total_receive_errors)]
    fn get_total_receive_errors(&self) -> u64 {
        lock(&self.state).total_receive_errors
    }

    #[setter(total_receive_errors)]
    fn set_total_receive_errors(&self, value: u64) {
        lock(&self.state).total_receive_errors = value;
    }
    #[getter(startup_time)]
    fn get_startup_time(&self) -> f64 {
        lock(&self.state).startup_time
    }

    #[setter(startup_time)]
    fn set_startup_time(&self, value: f64) {
        lock(&self.state).startup_time = value;
    }
    #[getter(protocol_marker)]
    fn get_protocol_marker(&self) -> i64 {
        lock(&self.state).protocol_marker
    }

    #[setter(protocol_marker)]
    fn set_protocol_marker(&self, value: i64) {
        lock(&self.state).protocol_marker = value;
    }
    #[getter(little_endian)]
    fn get_little_endian(&self) -> bool {
        lock(&self.state).little_endian
    }

    #[setter(little_endian)]
    fn set_little_endian(&self, value: bool) {
        lock(&self.state).little_endian = value;
    }
    #[getter(crc_extra)]
    fn get_crc_extra(&self) -> bool {
        lock(&self.state).crc_extra
    }

    #[setter(crc_extra)]
    fn set_crc_extra(&self, value: bool) {
        lock(&self.state).crc_extra = value;
    }
    #[getter(sort_fields)]
    fn get_sort_fields(&self) -> bool {
        lock(&self.state).sort_fields
    }

    #[setter(sort_fields)]
    fn set_sort_fields(&self, value: bool) {
        lock(&self.state).sort_fields = value;
    }

    #[getter(callback)]
    fn get_callback(&self, py: Python<'_>) -> PyObject {
        lock(&self.refs)
            .callback
            .as_ref()
            .map(|c| c.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }

    #[setter(callback)]
    fn set_callback_attr(&self, py: Python<'_>, value: PyObject) {
        lock(&self.refs).callback = if value.is_none(py) { None } else { Some(value) };
    }
    #[getter(callback_args)]
    fn get_callback_args(&self, py: Python<'_>) -> PyObject {
        lock(&self.refs)
            .callback_args
            .as_ref()
            .map(|c| c.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }

    #[setter(callback_args)]
    fn set_callback_args(&self, py: Python<'_>, value: PyObject) {
        lock(&self.refs).callback_args = if value.is_none(py) { None } else { Some(value) };
    }
    #[getter(callback_kwargs)]
    fn get_callback_kwargs(&self, py: Python<'_>) -> PyObject {
        lock(&self.refs)
            .callback_kwargs
            .as_ref()
            .map(|c| c.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }

    #[setter(callback_kwargs)]
    fn set_callback_kwargs(&self, py: Python<'_>, value: PyObject) {
        lock(&self.refs).callback_kwargs = if value.is_none(py) { None } else { Some(value) };
    }
    #[getter(send_callback)]
    fn get_send_callback(&self, py: Python<'_>) -> PyObject {
        lock(&self.refs)
            .send_callback
            .as_ref()
            .map(|c| c.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }

    #[setter(send_callback)]
    fn set_send_callback_attr(&self, py: Python<'_>, value: PyObject) {
        lock(&self.refs).send_callback = if value.is_none(py) { None } else { Some(value) };
    }
    #[getter(send_callback_args)]
    fn get_send_callback_args(&self, py: Python<'_>) -> PyObject {
        lock(&self.refs)
            .send_callback_args
            .as_ref()
            .map(|c| c.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }

    #[setter(send_callback_args)]
    fn set_send_callback_args(&self, py: Python<'_>, value: PyObject) {
        lock(&self.refs).send_callback_args = if value.is_none(py) { None } else { Some(value) };
    }
    #[getter(send_callback_kwargs)]
    fn get_send_callback_kwargs(&self, py: Python<'_>) -> PyObject {
        lock(&self.refs)
            .send_callback_kwargs
            .as_ref()
            .map(|c| c.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }

    #[setter(send_callback_kwargs)]
    fn set_send_callback_kwargs(&self, py: Python<'_>, value: PyObject) {
        lock(&self.refs).send_callback_kwargs = if value.is_none(py) { None } else { Some(value) };
    }

    #[getter]
    fn file(&self, py: Python<'_>) -> PyObject {
        lock(&self.refs)
            .file
            .as_ref()
            .map(|c| c.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }

    #[setter(file)]
    fn set_file(&self, py: Python<'_>, value: PyObject) {
        lock(&self.refs).file = if value.is_none(py) { None } else { Some(value) };
    }

    #[getter]
    fn buf<'py>(&self, py: Python<'py>) -> Bound<'py, PyByteArray> {
        PyByteArray::new(py, &lock(&self.state).buf)
    }

    #[getter(signing)]
    fn get_signing(&self, py: Python<'_>) -> Py<Signing> {
        lock(&self.signing).clone_ref(py)
    }

    #[setter(signing)]
    fn set_signing(&self, value: Py<Signing>) {
        *lock(&self.signing) = value;
    }

    #[getter(WIRE_PROTOCOL_VERSION)]
    fn wire_protocol_version(&self) -> &'static str {
        self.wire.version_text()
    }

    #[pyo3(signature = (callback, *args, **kwargs))]
    fn set_callback(
        &self,
        py: Python<'_>,
        callback: PyObject,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) {
        let mut refs = lock(&self.refs);
        refs.callback = Some(callback);
        refs.callback_args = Some(args.clone().into_any().unbind());
        refs.callback_kwargs = Some(
            kwargs
                .map(|k| k.clone().into_any().unbind())
                .unwrap_or_else(|| PyDict::new(py).into_any().unbind()),
        );
    }

    #[pyo3(signature = (callback, *args, **kwargs))]
    fn set_send_callback(
        &self,
        py: Python<'_>,
        callback: PyObject,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) {
        let mut refs = lock(&self.refs);
        refs.send_callback = Some(callback);
        refs.send_callback_args = Some(args.clone().into_any().unbind());
        refs.send_callback_kwargs = Some(
            kwargs
                .map(|k| k.clone().into_any().unbind())
                .unwrap_or_else(|| PyDict::new(py).into_any().unbind()),
        );
    }

    #[pyo3(signature = (mavmsg, force_mavlink1=false))]
    fn send(slf: &Bound<'_, Self>, mavmsg: &Bound<'_, PyAny>, force_mavlink1: bool) -> PyResult<()> {
        Self::send_message(slf, mavmsg, force_mavlink1)
    }

    fn buf_len(&self) -> usize {
        lock(&self.state).buf_len()
    }

    fn bytes_needed(&self) -> usize {
        self.bytes_needed_value()
    }

    fn parse_char<'py>(slf: &Bound<'py, Self>, c: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let data = bytes_arg(c)?;
        Self::parse_bytes(slf, &data)
    }

    fn parse_buffer<'py>(slf: &Bound<'py, Self>, s: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyList>>> {
        let data = bytes_arg(s)?;
        let Some(first) = Self::parse_bytes(slf, &data)? else {
            return Ok(None);
        };
        let mut out = vec![first];
        while let Some(m) = Self::parse_bytes(slf, &[])? {
            out.push(m);
        }
        Ok(Some(PyList::new(slf.py(), out)?))
    }

    #[allow(non_snake_case)]
    fn check_signature(
        &self,
        py: Python<'_>,
        msgbuf: &Bound<'_, PyAny>,
        srcSystem: u8,
        srcComponent: u8,
    ) -> PyResult<bool> {
        let data = bytes_arg(msgbuf)?;
        if lock(&self.signing(py).get().state).secret_key.is_none() {
            return Err(pyo3::exceptions::PyAssertionError::new_err(()));
        }
        Ok(self.check_signature_bytes(py, &data, srcSystem, srcComponent))
    }

    fn decode<'py>(slf: &Bound<'py, Self>, msgbuf: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let data = bytes_arg(msgbuf)?;
        Self::decode_frame(slf, &data)
    }

    /// `<name>_encode` and `<name>_send` for every message in this view.
    fn __getattr__(slf: &Bound<'_, Self>, name: &str) -> PyResult<Py<Sender>> {
        let py = slf.py();
        let wire = slf.get().wire;
        let (base, send) = if let Some(base) = name.strip_suffix("_send") {
            (base, true)
        } else if let Some(base) = name.strip_suffix("_encode") {
            (base, false)
        } else {
            ("", false)
        };
        if let Some(def) = (!base.is_empty()).then(|| defs::message_by_lower(base, wire)).flatten() {
            return Py::new(
                py,
                Sender {
                    mav: slf.clone().unbind(),
                    def,
                    send,
                },
            );
        }
        Err(PyAttributeError::new_err(format!(
            "'MAVLink' object has no attribute '{name}'"
        )))
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        if let Ok(refs) = self.refs.try_lock() {
            for obj in [
                &refs.file,
                &refs.callback,
                &refs.callback_args,
                &refs.callback_kwargs,
                &refs.send_callback,
                &refs.send_callback_args,
                &refs.send_callback_kwargs,
            ]
            .into_iter()
            .flatten()
            {
                visit.call(obj)?;
            }
        }
        if let Ok(signing) = self.signing.try_lock() {
            visit.call(&*signing)?;
        }
        Ok(())
    }

    fn __clear__(&self) {
        if let Ok(mut refs) = self.refs.try_lock() {
            *refs = Refs::default();
        }
    }
}

/// A bound `<name>_encode` or `<name>_send`.
#[pyclass(module = "corvus_link", frozen)]
pub struct Sender {
    mav: Py<Mavlink>,
    def: &'static MsgDef,
    send: bool,
}

#[pymethods]
impl Sender {
    #[pyo3(signature = (*args, **kwargs))]
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<PyObject> {
        let mav = self.mav.bind(py);
        let wire = mav.get().wire;
        let what = format!("{}_{}", self.def.lower, if self.send { "send" } else { "encode" });
        let nfields = self.def.fields(wire).len();
        let mut force = false;
        let mut args = args.clone();
        let mut kwargs = kwargs.map(|k| k.copy()).transpose()?;
        if self.send {
            if let Some(k) = &kwargs {
                if let Some(v) = k.get_item("force_mavlink1")? {
                    force = v.is_truthy()?;
                    k.del_item("force_mavlink1")?;
                }
            }
            if args.len() == nfields + 1 {
                force = args.get_item(nfields)?.is_truthy()?;
                args = args.get_slice(0, nfields);
            }
        }
        if kwargs.as_ref().is_some_and(|k| k.is_empty()) {
            kwargs = None;
        }
        let values = message::bind_arguments(py, self.def, wire, &args, kwargs.as_ref(), true, &what)?;
        let msg = message::build(py, self.def, wire, &values)?;
        if self.send {
            Mavlink::send_message(mav, msg.as_any(), force)?;
            Ok(py.None())
        } else {
            Ok(msg.into_any().unbind())
        }
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.mav)
    }
}

/// What `bytearray.extend` accepts.
pub fn bytes_arg(value: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(b) = value.downcast::<PyBytes>() {
        return Ok(b.as_bytes().to_vec());
    }
    if let Ok(b) = value.downcast::<PyByteArray>() {
        return Ok(b.to_vec());
    }
    if value.is_instance_of::<pyo3::types::PyString>() {
        return Err(pyo3::exceptions::PyTypeError::new_err(
            "expected iterable of integers; got: 'str'",
        ));
    }
    if let Ok(view) = value.call_method0("tobytes") {
        if let Ok(b) = view.downcast::<PyBytes>() {
            return Ok(b.as_bytes().to_vec());
        }
    }
    let mut out = Vec::new();
    for item in value.try_iter()? {
        out.push(item?.extract::<u8>()?);
    }
    Ok(out)
}
