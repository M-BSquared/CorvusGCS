//! RTCM 3 framing, GPS_RTCM_DATA fragments and the UBX messages that make a
//! u-blox receiver a base station: pure functions over bytes.
//!
//! Integers keep Python's meaning where callers can hand in anything: an
//! exact int that fits is worked on here, anything else goes through
//! Python's own operators so the answer, and the error, are the ones the
//! Python module gave.

use pyo3::exceptions::{PyOverflowError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyInt, PyList, PyModule, PySlice, PyTuple};

use crate::protocol::host::py_float;
use crate::protocol::telemetry::py_round;

pub const RTCM_PREAMBLE: u8 = 0xD3;
pub const RTCM_MAX_PAYLOAD: usize = 1023;
pub const MAX_FRAME: usize = 3 + RTCM_MAX_PAYLOAD + 3;
const MAX_BUFFER: usize = MAX_FRAME * 2;
pub const FRAGMENT_BYTES: usize = 180;
const MAX_FRAGMENTS: usize = 4;
pub const MAX_MESSAGE_BYTES: usize = FRAGMENT_BYTES * MAX_FRAGMENTS - 1;

pub const UBX_CLASS_NAV: u8 = 0x01;
pub const UBX_CLASS_CFG: u8 = 0x06;
pub const UBX_CLASS_MON: u8 = 0x0A;
const UBX_CLASS_RTCM3: u8 = 0xF5;
pub const UBX_ID_NAV_SVIN: u8 = 0x3B;
const UBX_ID_CFG_MSG: u8 = 0x01;
const UBX_ID_CFG_TMODE3: u8 = 0x71;
const UBX_ID_CFG_VALSET: u8 = 0x8A;
pub const UBX_ID_MON_VER: u8 = 0x04;
const UBX_LAYER_RAM: i64 = 0x01;
const UBX_MAX_PAYLOAD: usize = 4096;

const KEY_TMODE_MODE: i64 = 0x20030001;
const KEY_TMODE_POS_TYPE: i64 = 0x20030002;
const KEY_TMODE_LAT: i64 = 0x40030009;
const KEY_TMODE_LON: i64 = 0x4003000A;
const KEY_TMODE_HEIGHT: i64 = 0x4003000B;
const KEY_TMODE_LAT_HP: i64 = 0x2003000C;
const KEY_TMODE_LON_HP: i64 = 0x2003000D;
const KEY_TMODE_HEIGHT_HP: i64 = 0x2003000E;
const KEY_TMODE_FIXED_POS_ACC: i64 = 0x4003000F;
const KEY_TMODE_SVIN_MIN_DUR: i64 = 0x40030010;
const KEY_TMODE_SVIN_ACC_LIMIT: i64 = 0x40030011;
const KEY_RATE_MEAS: i64 = 0x30210001;
const KEY_MSGOUT_NAV_SVIN_I2C: i64 = 0x20910088;
const KEY_MSGOUT_RTCM_1005_I2C: i64 = 0x209102BD;
const RTCM_MSM4_KEYS: [i64; 6] = [0x209102BD, 0x2091035E, 0x20910363, 0x20910368, 0x2091036D, 0x20910303];
const RTCM_MSM7_KEYS: [i64; 6] = [0x209102BD, 0x209102CC, 0x209102D1, 0x20910318, 0x209102D6, 0x20910303];
const LEGACY_RTCM_MSM4: [u8; 6] = [0x05, 0x4A, 0x54, 0x5E, 0x7C, 0xE6];
const LEGACY_RTCM_MSM7: [u8; 6] = [0x05, 0x4D, 0x57, 0x61, 0x7F, 0xE6];
const RTCM_1005_RATE: i64 = 5;

// ------------------------------------------------------------ helpers ----

/// The bytes of a bytes-like object, or of `bytes(obj)`.
pub fn bytes_of(obj: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(b) = obj.downcast::<PyBytes>() {
        return Ok(b.as_bytes().to_vec());
    }
    if let Ok(b) = obj.downcast::<PyByteArray>() {
        return Ok(b.to_vec());
    }
    let py = obj.py();
    Ok(py.get_type::<PyBytes>().call1((obj,))?.downcast_into::<PyBytes>()?.as_bytes().to_vec())
}

/// A plain int that fits, as Python would see it (a bool is 0 or 1).
fn small_int(obj: &Bound<'_, PyAny>) -> Option<i64> {
    if obj.is_exact_instance_of::<PyInt>() || obj.is_instance_of::<PyBool>() {
        obj.extract::<i64>().ok()
    } else {
        None
    }
}

/// `obj & mask`, as an unsigned number.
fn masked(obj: &Bound<'_, PyAny>, mask: u64) -> PyResult<u64> {
    match small_int(obj) {
        Some(v) => Ok((v as u64) & mask),
        None => obj.bitand(mask)?.extract(),
    }
}

/// `(key >> 28) & 0x07`.
fn size_field(key: &Bound<'_, PyAny>) -> PyResult<u64> {
    match small_int(key) {
        Some(v) => Ok(((v >> 28) & 0x07) as u64),
        None => key.rshift(28)?.bitand(0x07)?.extract(),
    }
}

fn struct_error(py: Python<'_>, text: String) -> PyErr {
    match py.import("struct").and_then(|s| s.getattr("error")).and_then(|kind| kind.call1((text,))) {
        Ok(value) => PyErr::from_value(value),
        Err(err) => err,
    }
}

/// `a, b = item`, with Python's errors for the wrong number of values.
fn unpack_pair<'py>(item: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let mut values = Vec::new();
    for v in item.try_iter()? {
        values.push(v?);
        if values.len() > 2 {
            return Err(PyValueError::new_err("too many values to unpack (expected 2)"));
        }
    }
    if values.len() < 2 {
        return Err(PyValueError::new_err(format!("not enough values to unpack (expected 2, got {})", values.len())));
    }
    let b = values.pop().expect("two");
    let a = values.pop().expect("two");
    Ok((a, b))
}

/// `struct.pack("<i"/"<b"...)`'s range check for a signed field.
fn signed_field(obj: &Bound<'_, PyAny>, code: char, lo: i64, hi: i64) -> PyResult<i64> {
    let py = obj.py();
    if !obj.is_instance_of::<PyInt>() {
        return Err(struct_error(py, "required argument is not an integer".into()));
    }
    match obj.extract::<i64>() {
        Ok(v) if (lo..=hi).contains(&v) => Ok(v),
        _ => Err(struct_error(py, format!("'{code}' format requires {lo} <= number <= {hi}"))),
    }
}

/// `int(round(x))` for a float: an exact integer as a Python int.
fn round_to_int(py: Python<'_>, x: f64) -> PyResult<PyObject> {
    if x.is_nan() {
        return Err(PyValueError::new_err("cannot convert float NaN to integer"));
    }
    if x.is_infinite() {
        return Err(PyOverflowError::new_err("cannot convert float infinity to integer"));
    }
    let r = x.round_ties_even();
    if r.abs() < 9.0e18 {
        return Ok((r as i64).into_pyobject(py)?.into_any().unbind());
    }
    Ok(py.get_type::<PyInt>().call1((r,))?.unbind())
}

/// `int(obj)`.
fn py_int<'py>(obj: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    obj.py().get_type::<PyInt>().call1((obj,))
}

/// `max(0, obj)` for an int.
fn at_least_zero(obj: Bound<'_, PyAny>) -> PyResult<Bound<'_, PyAny>> {
    let py = obj.py();
    let zero = 0i64.into_pyobject(py)?.into_any();
    Ok(if obj.gt(&zero)? { obj } else { zero })
}

/// `max(0, int(round(float(x) * 10000.0)))`: metres to 0.1 mm.
fn tenth_mm<'py>(x: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = x.py();
    at_least_zero(round_to_int(py, py_float(x)? * 10000.0)?.into_bound(py))
}

// ------------------------------------------------------------- RTCM 3 ----

const fn crc24q_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut byte = 0;
    while byte < 256 {
        let mut reg: u32 = (byte as u32) << 16;
        let mut i = 0;
        while i < 8 {
            reg = if reg & 0x800000 != 0 { (reg << 1) ^ 0x1864CFB } else { reg << 1 };
            i += 1;
        }
        table[byte] = reg & 0xFFFFFF;
        byte += 1;
    }
    table
}

static CRC24Q_TABLE: [u32; 256] = crc24q_table();

pub fn crc24q_of(data: &[u8]) -> u32 {
    let mut reg: u32 = 0;
    for &byte in data {
        reg = ((reg << 8) & 0xFFFFFF) ^ CRC24Q_TABLE[(((reg >> 16) as u8) ^ byte) as usize];
    }
    reg
}

/// The CRC-24Q of *data*.
#[pyfunction]
fn crc24q(data: &Bound<'_, PyAny>) -> PyResult<u32> {
    Ok(crc24q_of(&bytes_of(data)?))
}

pub fn message_number_of(frame: &[u8]) -> u32 {
    if frame.len() < 6 {
        return 0;
    }
    ((frame[3] as u32) << 4) | ((frame[4] as u32) >> 4)
}

/// The RTCM message type of a complete *frame*, or 0.
#[pyfunction]
fn message_number(frame: &Bound<'_, PyAny>) -> PyResult<u32> {
    Ok(message_number_of(&bytes_of(frame)?))
}

/// Byte stream in, complete CRC-valid RTCM 3 frames out; resynchronises one
/// byte at a time and never holds more than two frames' worth.
#[pyclass(module = "corvus_link.rtk")]
pub struct RtcmFramer {
    buf: Vec<u8>,
    #[pyo3(get, set)]
    pub frames: i64,
    #[pyo3(get, set)]
    pub crc_errors: i64,
    #[pyo3(get, set)]
    pub discarded: i64,
}

impl RtcmFramer {
    pub fn new_framer() -> Self {
        RtcmFramer { buf: Vec::new(), frames: 0, crc_errors: 0, discarded: 0 }
    }

    pub fn feed_bytes(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            let Some(start) = self.buf.iter().position(|&b| b == RTCM_PREAMBLE) else {
                let keep = self.buf.len().min(2);
                self.discarded += (self.buf.len() - keep) as i64;
                self.buf.drain(..self.buf.len() - keep);
                break;
            };
            if start > 0 {
                self.discarded += start as i64;
                self.buf.drain(..start);
            }
            if self.buf.len() < 3 {
                break;
            }
            let length = (((self.buf[1] & 0x03) as usize) << 8) | self.buf[2] as usize;
            let total = 3 + length + 3;
            if self.buf.len() < total {
                break;
            }
            let expected = ((self.buf[total - 3] as u32) << 16) | ((self.buf[total - 2] as u32) << 8) | self.buf[total - 1] as u32;
            if crc24q_of(&self.buf[..total - 3]) == expected {
                out.push(self.buf.drain(..total).collect());
                self.frames += 1;
                continue;
            }
            self.crc_errors += 1;
            self.discarded += 1;
            self.buf.drain(..1);
        }
        if self.buf.len() > MAX_BUFFER {
            let over = self.buf.len() - MAX_BUFFER;
            self.discarded += over as i64;
            self.buf.drain(..over);
        }
        out
    }
}

#[pymethods]
impl RtcmFramer {
    #[new]
    fn py_new() -> Self {
        Self::new_framer()
    }

    #[classattr]
    const MAX_FRAME: usize = MAX_FRAME;
    #[classattr]
    const _MAX_BUFFER: usize = MAX_BUFFER;

    #[getter]
    fn _buf<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.buf)
    }

    /// Add *chunk* and return every complete, CRC-valid frame it finished.
    fn feed<'py>(&mut self, py: Python<'py>, chunk: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
        let data = if chunk.is_truthy()? { bytes_of(chunk)? } else { Vec::new() };
        PyList::new(py, self.feed_bytes(&data).iter().map(|f| PyBytes::new(py, f)))
    }

    /// Drop any partial frame.
    fn reset(&mut self) {
        self.buf.clear();
    }
}

/// `(flags, data)` pairs for GPS_RTCM_DATA, or none for an empty frame or
/// one too long to express in four fragments.
#[pyfunction]
fn fragments<'py>(frame: &Bound<'py, PyAny>, sequence: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
    let py = frame.py();
    let out = PyList::empty(py);
    if !frame.is_truthy()? {
        return Ok(out);
    }
    let len = frame.len()?;
    if len > MAX_MESSAGE_BYTES {
        return Ok(out);
    }
    let seq_bits = masked(&py_int(sequence)?, 0x1F)? << 3;
    if len < FRAGMENT_BYTES {
        out.append((seq_bits, frame))?;
        return Ok(out);
    }
    let mut index = 0u64;
    let mut start = 0;
    while start < len {
        let end = (start + FRAGMENT_BYTES).min(len);
        let part = frame.get_item(PySlice::new(py, start as isize, end as isize, 1))?;
        out.append((1 | (index << 1) | seq_bits, part))?;
        index += 1;
        start += FRAGMENT_BYTES;
    }
    if len % FRAGMENT_BYTES == 0 {
        out.append((1 | (index << 1) | seq_bits, PyBytes::new(py, b"")))?;
    }
    Ok(out)
}


// ---------------------------------------------------------------- UBX ----

fn checksum(payload: &[u8]) -> (u8, u8) {
    let (mut a, mut b) = (0u8, 0u8);
    for &byte in payload {
        a = a.wrapping_add(byte);
        b = b.wrapping_add(a);
    }
    (a, b)
}

/// The 8-bit Fletcher checksum UBX ends every frame with.
#[pyfunction]
fn ubx_checksum(payload: &Bound<'_, PyAny>) -> PyResult<(u8, u8)> {
    Ok(checksum(&bytes_of(payload)?))
}

pub fn frame_of(class: u8, id: u8, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![class, id];
    body.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    body.extend_from_slice(payload);
    let (a, b) = checksum(&body);
    let mut out = vec![0xB5, 0x62];
    out.extend(body);
    out.extend([a, b]);
    out
}

/// One complete UBX message, ready to write to the port.
#[pyfunction]
#[pyo3(signature = (msg_class, msg_id, payload = None))]
fn ubx_frame<'py>(msg_class: &Bound<'py, PyAny>, msg_id: &Bound<'py, PyAny>, payload: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyBytes>> {
    let py = msg_class.py();
    let class = masked(msg_class, 0xFF)? as u8;
    let id = masked(msg_id, 0xFF)? as u8;
    let data = match payload {
        Some(p) => bytes_of(p)?,
        None => Vec::new(),
    };
    if data.len() > 0xFFFF {
        return Err(struct_error(py, "'H' format requires 0 <= number <= 65535".into()));
    }
    Ok(PyBytes::new(py, &frame_of(class, id, &data)))
}

/// Byte stream in, `(class, id, payload)` out, checksum-valid only.
#[pyclass(module = "corvus_link.rtk")]
pub struct UbxParser {
    buf: Vec<u8>,
}

impl UbxParser {
    pub fn new_parser() -> Self {
        UbxParser { buf: Vec::new() }
    }

    pub fn feed_bytes(&mut self, chunk: &[u8]) -> Vec<(u8, u8, Vec<u8>)> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            let Some(start) = self.buf.windows(2).position(|w| w == [0xB5, 0x62]) else {
                let keep = self.buf.len().min(1);
                self.buf.drain(..self.buf.len() - keep);
                break;
            };
            if start > 0 {
                self.buf.drain(..start);
            }
            if self.buf.len() < 6 {
                break;
            }
            let length = u16::from_le_bytes([self.buf[4], self.buf[5]]) as usize;
            if length > UBX_MAX_PAYLOAD {
                self.buf.drain(..2);
                continue;
            }
            let total = 6 + length + 2;
            if self.buf.len() < total {
                break;
            }
            let (a, b) = checksum(&self.buf[2..6 + length]);
            if (a, b) == (self.buf[total - 2], self.buf[total - 1]) {
                out.push((self.buf[2], self.buf[3], self.buf[6..6 + length].to_vec()));
                self.buf.drain(..total);
                continue;
            }
            self.buf.drain(..2);
        }
        out
    }
}

#[pymethods]
impl UbxParser {
    #[new]
    fn py_new() -> Self {
        Self::new_parser()
    }

    #[classattr]
    const _MAX_PAYLOAD: usize = UBX_MAX_PAYLOAD;

    #[getter]
    fn _buf<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.buf)
    }

    /// Add *chunk* and return every complete, checksum-valid message.
    fn feed<'py>(&mut self, py: Python<'py>, chunk: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
        let data = if chunk.is_truthy()? { bytes_of(chunk)? } else { Vec::new() };
        let out = PyList::empty(py);
        for (class, id, payload) in self.feed_bytes(&data) {
            out.append((class, id, PyBytes::new(py, &payload)))?;
        }
        Ok(out)
    }

    /// Drop any partial message.
    fn reset(&mut self) {
        self.buf.clear();
    }
}

/// *value* at the width the key's size bits declare, little endian.
#[pyfunction]
fn _value_bytes<'py>(key: &Bound<'py, PyAny>, value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let py = key.py();
    Ok(PyBytes::new(py, &value_bytes(key, value)?))
}

fn value_bytes(key: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    let width = match size_field(key)? {
        0..=2 => 1,
        3 => 2,
        _ => 4,
    };
    let v = masked(value, (1u64 << (8 * width)) - 1)?;
    Ok(v.to_le_bytes()[..width].to_vec())
}

/// A CFG-VALSET message carrying *items* as `(key, value)` pairs.
#[pyfunction]
#[pyo3(signature = (items, layers = None))]
pub fn valset<'py>(items: &Bound<'py, PyAny>, layers: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyBytes>> {
    let py = items.py();
    let layers = match layers {
        Some(l) => masked(l, 0xFF)? as u8,
        None => UBX_LAYER_RAM as u8,
    };
    let mut payload = vec![0x00, layers, 0x00, 0x00];
    for item in items.try_iter()? {
        let item = item?;
        let (key, value) = unpack_pair(&item)?;
        payload.extend_from_slice(&(masked(&key, 0xFFFF_FFFF)? as u32).to_le_bytes());
        payload.extend(value_bytes(&key, &value)?);
    }
    Ok(PyBytes::new(py, &frame_of(UBX_CLASS_CFG, UBX_ID_CFG_VALSET, &payload)))
}

fn int_obj(py: Python<'_>, v: i64) -> Bound<'_, PyAny> {
    v.into_pyobject(py).expect("ints convert").into_any()
}

fn pairs<'py>(py: Python<'py>, items: &[(i64, Bound<'py, PyAny>)]) -> PyResult<Bound<'py, PyAny>> {
    let out = PyList::empty(py);
    for (k, v) in items {
        out.append(PyTuple::new(py, [int_obj(py, *k), v.clone()])?)?;
    }
    Ok(out.into_any())
}

fn port_keys(key: i64) -> [i64; 2] {
    [key + 1, key + 3]
}

fn msgout<'py>(py: Python<'py>, keys: &[i64], rate: i64) -> Vec<(i64, Bound<'py, PyAny>)> {
    keys.iter().flat_map(|&k| port_keys(k)).map(|k| (k, int_obj(py, rate))).collect()
}

fn rtcm_key_set(msm7: bool) -> &'static [i64; 6] {
    if msm7 { &RTCM_MSM7_KEYS } else { &RTCM_MSM4_KEYS }
}

fn frames_list<'py>(py: Python<'py>, frames: Vec<Bound<'py, PyBytes>>) -> PyResult<Bound<'py, PyList>> {
    PyList::new(py, frames)
}

/// Silence the RTCM output before the time mode is changed.
#[pyfunction]
#[pyo3(signature = (msm7 = false))]
fn stop_output_frames(py: Python<'_>, msm7: bool) -> PyResult<Bound<'_, PyList>> {
    frames_list(py, vec![valset(&pairs(py, &msgout(py, rtcm_key_set(msm7), 0))?, None)?])
}

/// Configure survey-in: stop the old one, then start the new one.
#[pyfunction]
#[pyo3(signature = (duration_s, accuracy_m, msm7 = false))]
fn survey_in_frames<'py>(duration_s: &Bound<'py, PyAny>, accuracy_m: &Bound<'py, PyAny>, msm7: bool) -> PyResult<Bound<'py, PyList>> {
    let py = duration_s.py();
    let stop = valset(&pairs(py, &[(KEY_TMODE_MODE, int_obj(py, 0))])?, None)?;
    let mut items = vec![
        (KEY_TMODE_MODE, int_obj(py, 1)),
        (KEY_TMODE_SVIN_MIN_DUR, at_least_zero(py_int(duration_s)?)?),
        (KEY_TMODE_SVIN_ACC_LIMIT, tenth_mm(accuracy_m)?),
    ];
    items.extend(msgout(py, &[KEY_MSGOUT_NAV_SVIN_I2C], 5));
    let start = valset(&pairs(py, &items)?, None)?;
    let mut frames: Vec<Bound<'py, PyBytes>> = stop_output_frames(py, msm7)?.iter().map(|f| f.downcast_into::<PyBytes>().expect("bytes")).collect();
    frames.push(stop);
    frames.push(start);
    frames_list(py, frames)
}

/// A coarse count and a [-99, 99] high precision remainder.
fn hp_split<'py>(value: &Bound<'py, PyAny>, scale: f64) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let py = value.py();
    let x = py_float(value)? * scale;
    let fine = round_to_int(py, x)?.into_bound(py);
    if let Some(f) = small_int(&fine) {
        if f.abs() < (1 << 52) {
            let coarse = ((f as f64) / 100.0).trunc() as i64;
            return Ok((int_obj(py, coarse), int_obj(py, f - coarse * 100)));
        }
    }
    let quotient = fine.call_method1("__truediv__", (100,))?;
    let coarse = py_int(&quotient)?;
    let hp = fine.sub(coarse.mul(100)?)?;
    Ok((coarse, hp))
}

#[pyfunction]
fn _hp_split<'py>(value: &Bound<'py, PyAny>, scale: f64) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    hp_split(value, scale)
}

/// Configure a base at a position the operator already knows.
#[pyfunction]
#[pyo3(signature = (latitude, longitude, altitude, accuracy_m, msm7 = false))]
fn fixed_position_frames<'py>(
    latitude: &Bound<'py, PyAny>,
    longitude: &Bound<'py, PyAny>,
    altitude: &Bound<'py, PyAny>,
    accuracy_m: &Bound<'py, PyAny>,
    msm7: bool,
) -> PyResult<Bound<'py, PyList>> {
    let py = latitude.py();
    let (lat, lat_hp) = hp_split(latitude, 1e9)?;
    let (lon, lon_hp) = hp_split(longitude, 1e9)?;
    let (height, height_hp) = hp_split(altitude, 1e4)?;
    let accuracy = accuracy_m.clone();
    let apply = valset(
        &pairs(
            py,
            &[
                (KEY_TMODE_MODE, int_obj(py, 2)),
                (KEY_TMODE_POS_TYPE, int_obj(py, 1)),
                (KEY_TMODE_LAT, lat),
                (KEY_TMODE_LAT_HP, lat_hp),
                (KEY_TMODE_LON, lon),
                (KEY_TMODE_LON_HP, lon_hp),
                (KEY_TMODE_HEIGHT, height),
                (KEY_TMODE_HEIGHT_HP, height_hp),
                (KEY_TMODE_FIXED_POS_ACC, tenth_mm(&accuracy)?),
            ],
        )?,
        None,
    )?;
    let mut frames: Vec<Bound<'py, PyBytes>> = stop_output_frames(py, msm7)?.iter().map(|f| f.downcast_into::<PyBytes>().expect("bytes")).collect();
    frames.push(apply);
    frames_list(py, frames)
}

/// Start the RTCM stream and stop the survey reports.
#[pyfunction]
#[pyo3(signature = (msm7 = false, reduce_rate = true))]
fn activate_output_frames(py: Python<'_>, msm7: bool, reduce_rate: bool) -> PyResult<Bound<'_, PyList>> {
    let mut items = Vec::new();
    if reduce_rate {
        items.push((KEY_RATE_MEAS, int_obj(py, 1000)));
    }
    items.extend(msgout(py, &[KEY_MSGOUT_RTCM_1005_I2C], RTCM_1005_RATE));
    let observations: Vec<i64> = rtcm_key_set(msm7).iter().copied().filter(|&k| k != KEY_MSGOUT_RTCM_1005_I2C).collect();
    items.extend(msgout(py, &observations, 1));
    items.extend(msgout(py, &[KEY_MSGOUT_NAV_SVIN_I2C], 0));
    frames_list(py, vec![valset(&pairs(py, &items)?, None)?])
}

/// Put the receiver back the way it was found.
#[pyfunction]
#[pyo3(signature = (msm7 = false))]
fn disable_base_frames(py: Python<'_>, msm7: bool) -> PyResult<Bound<'_, PyList>> {
    let mut items = msgout(py, rtcm_key_set(msm7), 0);
    items.extend(msgout(py, &[KEY_MSGOUT_NAV_SVIN_I2C], 0));
    items.push((KEY_TMODE_MODE, int_obj(py, 0)));
    frames_list(py, vec![valset(&pairs(py, &items)?, None)?])
}

fn cfg_msg(class: u8, id: u8, rate: u8) -> Vec<u8> {
    frame_of(UBX_CLASS_CFG, UBX_ID_CFG_MSG, &[class, id, rate])
}

/// CFG-MSG: set the output rate of one message on the current port.
#[pyfunction]
fn legacy_cfg_msg<'py>(msg_class: &Bound<'py, PyAny>, msg_id: &Bound<'py, PyAny>, rate: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let py = msg_class.py();
    Ok(PyBytes::new(py, &cfg_msg(masked(msg_class, 0xFF)? as u8, masked(msg_id, 0xFF)? as u8, masked(rate, 0xFF)? as u8)))
}

fn legacy_set(msm7: bool) -> &'static [u8; 6] {
    if msm7 { &LEGACY_RTCM_MSM7 } else { &LEGACY_RTCM_MSM4 }
}

fn legacy_silence(py: Python<'_>, msm7: bool) -> Vec<Bound<'_, PyBytes>> {
    legacy_set(msm7).iter().map(|&id| PyBytes::new(py, &cfg_msg(UBX_CLASS_RTCM3, id, 0))).collect()
}

/// Silence an M8P generation receiver's RTCM output.
#[pyfunction]
#[pyo3(signature = (msm7 = false))]
fn legacy_stop_output_frames(py: Python<'_>, msm7: bool) -> PyResult<Bound<'_, PyList>> {
    frames_list(py, legacy_silence(py, msm7))
}

/// One CFG-TMODE3 message: 40 bytes in the order the receiver reads them.
#[allow(clippy::too_many_arguments)]
fn tmode3<'py>(
    py: Python<'py>,
    flags: u64,
    lat: &Bound<'py, PyAny>,
    lon: &Bound<'py, PyAny>,
    alt: &Bound<'py, PyAny>,
    lat_hp: &Bound<'py, PyAny>,
    lon_hp: &Bound<'py, PyAny>,
    alt_hp: &Bound<'py, PyAny>,
    fixed_pos_acc: &Bound<'py, PyAny>,
    svin_min_dur: &Bound<'py, PyAny>,
    svin_acc_limit: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyBytes>> {
    let mut payload = vec![0u8, 0u8];
    payload.extend_from_slice(&((flags & 0xFFFF) as u16).to_le_bytes());
    for v in [lat, lon, alt] {
        payload.extend_from_slice(&(signed_field(v, 'i', i32::MIN as i64, i32::MAX as i64)? as i32).to_le_bytes());
    }
    for v in [lat_hp, lon_hp, alt_hp] {
        payload.push(signed_field(v, 'b', -128, 127)? as i8 as u8);
    }
    payload.push(0);
    for v in [fixed_pos_acc, svin_min_dur, svin_acc_limit] {
        payload.extend_from_slice(&(masked(v, 0xFFFF_FFFF)? as u32).to_le_bytes());
    }
    payload.extend_from_slice(&[0u8; 8]);
    Ok(PyBytes::new(py, &frame_of(UBX_CLASS_CFG, UBX_ID_CFG_TMODE3, &payload)))
}

fn tmode3_off(py: Python<'_>) -> PyResult<Bound<'_, PyBytes>> {
    let z = int_obj(py, 0);
    tmode3(py, 0, &z, &z, &z, &z, &z, &z, &z, &z, &z)
}

/// CFG-TMODE3 survey-in, for an M8P generation receiver.
#[pyfunction]
#[pyo3(signature = (duration_s, accuracy_m, msm7 = false))]
fn legacy_survey_in_frames<'py>(duration_s: &Bound<'py, PyAny>, accuracy_m: &Bound<'py, PyAny>, msm7: bool) -> PyResult<Bound<'py, PyList>> {
    let py = duration_s.py();
    let mut frames = legacy_silence(py, msm7);
    frames.push(tmode3_off(py)?);
    let z = int_obj(py, 0);
    let duration = at_least_zero(py_int(duration_s)?)?;
    let accuracy = tenth_mm(accuracy_m)?;
    frames.push(tmode3(py, 1, &z, &z, &z, &z, &z, &z, &z, &duration, &accuracy)?);
    frames.push(PyBytes::new(py, &cfg_msg(UBX_CLASS_NAV, UBX_ID_NAV_SVIN, 5)));
    frames_list(py, frames)
}

/// CFG-TMODE3 fixed position, for an M8P generation receiver.
#[pyfunction]
#[pyo3(signature = (latitude, longitude, altitude, accuracy_m, msm7 = false))]
fn legacy_fixed_position_frames<'py>(
    latitude: &Bound<'py, PyAny>,
    longitude: &Bound<'py, PyAny>,
    altitude: &Bound<'py, PyAny>,
    accuracy_m: &Bound<'py, PyAny>,
    msm7: bool,
) -> PyResult<Bound<'py, PyList>> {
    let py = latitude.py();
    let (lat, lat_hp) = hp_split(latitude, 1e9)?;
    let (lon, lon_hp) = hp_split(longitude, 1e9)?;
    let (height, height_hp) = hp_split(altitude, 1e4)?;
    let mut frames = legacy_silence(py, msm7);
    frames.push(tmode3_off(py)?);
    let accuracy = accuracy_m.clone();
    let z = int_obj(py, 0);
    let acc = tenth_mm(&accuracy)?;
    frames.push(tmode3(py, 2 | (1 << 8), &lat, &lon, &height, &lat_hp, &lon_hp, &height_hp, &acc, &z, &z)?);
    frames_list(py, frames)
}

/// Start the RTCM stream on an M8P generation receiver.
#[pyfunction]
#[pyo3(signature = (msm7 = false))]
fn legacy_activate_output_frames(py: Python<'_>, msm7: bool) -> PyResult<Bound<'_, PyList>> {
    let mut frames = vec![PyBytes::new(py, &cfg_msg(UBX_CLASS_NAV, UBX_ID_NAV_SVIN, 0))];
    for &id in legacy_set(msm7) {
        let rate = if id == 0x05 { RTCM_1005_RATE as u8 } else { 1 };
        frames.push(PyBytes::new(py, &cfg_msg(UBX_CLASS_RTCM3, id, rate)));
    }
    frames_list(py, frames)
}

/// Take an M8P generation receiver back out of base mode.
#[pyfunction]
#[pyo3(signature = (msm7 = false))]
fn legacy_disable_base_frames(py: Python<'_>, msm7: bool) -> PyResult<Bound<'_, PyList>> {
    let mut frames = legacy_silence(py, msm7);
    frames.push(PyBytes::new(py, &cfg_msg(UBX_CLASS_NAV, UBX_ID_NAV_SVIN, 0)));
    frames.push(tmode3_off(py)?);
    frames_list(py, frames)
}

/// A zero-length UBX message, which is how one is polled for.
#[pyfunction]
fn poll_frame<'py>(msg_class: &Bound<'py, PyAny>, msg_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    ubx_frame(msg_class, msg_id, None)
}


// --------------------------------------------------------- what comes back

pub fn nav_svin_dict<'py>(py: Python<'py>, p: &[u8]) -> PyResult<Option<Bound<'py, PyDict>>> {
    if p.len() < 40 {
        return Ok(None);
    }
    let u32_at = |o: usize| u32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]]);
    let i32_at = |o: usize| i32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]]);
    let d = PyDict::new(py);
    d.set_item("duration", u32_at(8))?;
    d.set_item("accuracy", py_round(u32_at(28) as f64 / 10000.0, 4))?;
    d.set_item("observations", u32_at(32))?;
    d.set_item("valid", p[36] != 0)?;
    d.set_item("active", p[37] != 0)?;
    d.set_item("ecef", PyList::new(py, [i32_at(12), i32_at(16), i32_at(20)])?)?;
    Ok(Some(d))
}

/// NAV-SVIN: how the survey is going, in the operator's units.
#[pyfunction]
fn parse_nav_svin<'py>(payload: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyDict>>> {
    nav_svin_dict(payload.py(), &bytes_of(payload)?)
}

fn is_py_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\x1c'..='\x1f' | ' ')
}

/// `raw.split(b"\0", 1)[0].decode("ascii", errors="replace").strip()`.
fn ascii_text(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let text: String = raw[..end].iter().map(|&b| if b < 0x80 { b as char } else { '\u{FFFD}' }).collect();
    text.trim_matches(is_py_space).to_string()
}

pub fn mon_ver_dict<'py>(py: Python<'py>, payload: &[u8]) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    if payload.len() < 40 {
        d.set_item("software", "")?;
        d.set_item("hardware", "")?;
        d.set_item("extensions", PyList::empty(py))?;
        d.set_item("protocol", 0.0)?;
        d.set_item("model", "")?;
        return Ok(d);
    }
    let software = ascii_text(&payload[0..30]);
    let hardware = ascii_text(&payload[30..40]);
    let mut extensions = Vec::new();
    let mut offset = 40;
    while offset + 30 <= payload.len() {
        extensions.push(ascii_text(&payload[offset..offset + 30]));
        offset += 30;
    }
    let mut protocol = PyFloat::new(py, 0.0).into_any();
    let mut model = String::new();
    let float = py.get_type::<PyFloat>();
    for line in &extensions {
        if line.starts_with("PROTVER") {
            let after = line.split_once('=').map(|(_, r)| r).unwrap_or(line.as_str());
            let mut text = after.trim_matches(is_py_space).to_string();
            if text.is_empty() {
                text = line.chars().skip(7).collect::<String>().trim_matches(is_py_space).to_string();
            }
            protocol = match float.call1((text,)) {
                Ok(v) => v,
                Err(err) if err.is_instance_of::<PyValueError>(py) => PyFloat::new(py, 0.0).into_any(),
                Err(err) => return Err(err),
            };
        } else if let Some(rest) = line.strip_prefix("MOD=") {
            model = rest.trim_matches(is_py_space).to_string();
        }
    }
    d.set_item("software", software)?;
    d.set_item("hardware", hardware)?;
    d.set_item("extensions", PyList::new(py, extensions)?)?;
    d.set_item("protocol", protocol)?;
    d.set_item("model", model)?;
    Ok(d)
}

/// MON-VER: who answered, and which protocol they speak.
#[pyfunction]
fn parse_mon_ver<'py>(payload: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    mon_ver_dict(payload.py(), &bytes_of(payload)?)
}

/// One line naming the receiver, for the page's status row.
#[pyfunction]
pub fn receiver_label(version: &Bound<'_, PyAny>) -> PyResult<String> {
    let py = version.py();
    if !version.is_truthy()? {
        return Ok(String::new());
    }
    let field = |name: &str| -> PyResult<String> {
        let v = version.call_method1("get", (name,))?;
        let v = if v.is_truthy()? { v } else { pyo3::types::PyString::new(py, "").into_any() };
        v.str()?.call_method0("strip")?.extract()
    };
    let model = field("model")?;
    let software = field("software")?;
    let protocol = version.call_method1("get", ("protocol",))?;
    let protocol = if protocol.is_truthy()? { protocol } else { PyFloat::new(py, 0.0).into_any() };
    let mut parts = vec![if model.is_empty() { "u-blox receiver".to_string() } else { format!("u-blox {model}") }];
    if !software.is_empty() {
        parts.push(software);
    }
    if protocol.is_truthy()? {
        let text: String = py.import("builtins")?.call_method1("format", (&protocol, "g"))?.extract()?;
        parts.push(format!("protocol {text}"));
    }
    Ok(parts.join(" \u{b7} "))
}

/// Does this receiver take CFG-VALSET rather than the CFG-* structs?
#[pyfunction]
pub fn supports_valset(version: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = version.py();
    if !version.is_truthy()? {
        return Ok(false);
    }
    let protocol = version.call_method1("get", ("protocol",))?;
    let protocol = if protocol.is_truthy()? { protocol } else { PyFloat::new(py, 0.0).into_any() };
    match py_float(&protocol) {
        Ok(v) => Ok(v >= 27.0),
        Err(err) if err.is_instance_of::<pyo3::exceptions::PyTypeError>(py) || err.is_instance_of::<PyValueError>(py) => Ok(false),
        Err(err) => Err(err),
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<RtcmFramer>()?;
    m.add_class::<UbxParser>()?;
    m.add_function(wrap_pyfunction!(crc24q, m)?)?;
    m.add_function(wrap_pyfunction!(message_number, m)?)?;
    m.add_function(wrap_pyfunction!(fragments, m)?)?;
    m.add_function(wrap_pyfunction!(ubx_checksum, m)?)?;
    m.add_function(wrap_pyfunction!(ubx_frame, m)?)?;
    m.add_function(wrap_pyfunction!(_value_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(valset, m)?)?;
    m.add_function(wrap_pyfunction!(stop_output_frames, m)?)?;
    m.add_function(wrap_pyfunction!(survey_in_frames, m)?)?;
    m.add_function(wrap_pyfunction!(_hp_split, m)?)?;
    m.add_function(wrap_pyfunction!(fixed_position_frames, m)?)?;
    m.add_function(wrap_pyfunction!(activate_output_frames, m)?)?;
    m.add_function(wrap_pyfunction!(disable_base_frames, m)?)?;
    m.add_function(wrap_pyfunction!(legacy_cfg_msg, m)?)?;
    m.add_function(wrap_pyfunction!(legacy_stop_output_frames, m)?)?;
    m.add_function(wrap_pyfunction!(legacy_survey_in_frames, m)?)?;
    m.add_function(wrap_pyfunction!(legacy_fixed_position_frames, m)?)?;
    m.add_function(wrap_pyfunction!(legacy_activate_output_frames, m)?)?;
    m.add_function(wrap_pyfunction!(legacy_disable_base_frames, m)?)?;
    m.add_function(wrap_pyfunction!(poll_frame, m)?)?;
    m.add_function(wrap_pyfunction!(parse_nav_svin, m)?)?;
    m.add_function(wrap_pyfunction!(parse_mon_ver, m)?)?;
    m.add_function(wrap_pyfunction!(receiver_label, m)?)?;
    m.add_function(wrap_pyfunction!(supports_valset, m)?)?;
    Ok(())
}
