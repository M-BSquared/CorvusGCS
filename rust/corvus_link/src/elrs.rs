//! ExpressLRS telemetry over CRSF, as ``corvus_link.elrs``: the frame
//! checksum, builder and resynchronising parser, the decoders for the
//! frames an aircraft sends down the RC link, the thread-safe telemetry
//! snapshot, the reader thread, and the "fly to" planner.
//!
//! The module that uses it is dormant (switched off, imported by nothing);
//! this is its code, ready for wiring. State stays on the Python objects
//! under their own locks; every step is called through them, so a clock or a
//! decoder a test replaces is the one that runs.

use pyo3::exceptions::{PyException, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyList, PyModule, PyString, PyTuple};

use crate::bridge::receive::within;
use crate::bridge::thread;
use crate::protocol::host::{caught_by, py_float};
use crate::rtk::wire::bytes_of;

const SYNC_ADDRESSES: [u8; 5] = [0xC8, 0xEA, 0xEC, 0xEE, 0xEF];
const MAX_FRAME_LEN: usize = 64;
const MIN_LENGTH_FIELD: usize = 2;

const TYPE_GPS: u8 = 0x02;
const TYPE_VARIO: u8 = 0x07;
const TYPE_BATTERY: u8 = 0x08;
const TYPE_BARO_ALTITUDE: u8 = 0x09;
const TYPE_LINK_STATISTICS: u8 = 0x14;
const TYPE_ATTITUDE: u8 = 0x1E;
const TYPE_FLIGHT_MODE: u8 = 0x21;

fn kw<'py>(py: Python<'py>, pairs: &[(&str, PyObject)]) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    for (k, v) in pairs {
        d.set_item(*k, v)?;
    }
    Ok(d)
}

fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0xD5 } else { crc << 1 };
        }
    }
    crc
}

/// CRC-8/DVB-S2 (polynomial 0xD5), the checksum every CRSF frame carries.
#[pyfunction]
fn crc8_dvb_s2(data: &Bound<'_, PyAny>) -> PyResult<u8> {
    Ok(crc8(&bytes_of(data)?))
}

/// Python's float `%`: the result takes the divisor's sign.
fn py_mod(x: f64, y: f64) -> f64 {
    let r = x % y;
    if r != 0.0 && ((r < 0.0) != (y < 0.0)) {
        r + y
    } else if r == 0.0 {
        0.0f64.copysign(y)
    } else {
        r
    }
}

fn py_bytes<'py>(py: Python<'py>, values: &[Bound<'py, PyAny>]) -> PyResult<Bound<'py, PyAny>> {
    py.get_type::<PyBytes>().call1((PyList::new(py, values)?,))
}

/// Encode one CRSF frame.
#[pyfunction]
#[pyo3(signature = (frame_type, payload, address = None))]
fn build_frame<'py>(frame_type: &Bound<'py, PyAny>, payload: &Bound<'py, PyAny>, address: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    let py = frame_type.py();
    let address = match address {
        Some(a) => a.clone(),
        None => 0xC8u8.into_pyobject(py)?.into_any(),
    };
    let body = py_bytes(py, &[frame_type.clone()])?.add(py.get_type::<PyBytes>().call1((payload,))?)?;
    let n = body.len()?;
    if n + 3 > MAX_FRAME_LEN {
        return Err(PyValueError::new_err(format!("CRSF frame too long ({} > {MAX_FRAME_LEN} bytes)", n + 3)));
    }
    let head = py_bytes(py, &[address, (n + 1).into_pyobject(py)?.into_any()])?;
    let crc = py_bytes(py, &[crc8(&bytes_of(&body)?).into_pyobject(py)?.into_any()])?;
    head.add(&body)?.add(crc)
}

/// Turns a CRSF byte stream into frames, resynchronising on garbage: what
/// is not a well formed frame with a matching CRC costs one byte.
#[pyclass(module = "corvus_link.elrs")]
pub struct CrsfParser {
    buf: Vec<u8>,
    #[pyo3(get, set)]
    frames_ok: i64,
    #[pyo3(get, set)]
    crc_errors: i64,
}

#[pymethods]
impl CrsfParser {
    #[new]
    fn new() -> Self {
        CrsfParser { buf: Vec::new(), frames_ok: 0, crc_errors: 0 }
    }

    #[getter]
    fn _buf<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.buf)
    }

    /// Add *data* and return every complete frame it finished.
    fn feed<'py>(&mut self, py: Python<'py>, data: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
        self.buf.extend(bytes_of(data)?);
        let frame_type = py.import("corvus.elrs")?.getattr("CrsfFrame")?;
        let frames = PyList::empty(py);
        while self.buf.len() >= 2 {
            let length = self.buf[1] as usize;
            if !SYNC_ADDRESSES.contains(&self.buf[0]) || !(MIN_LENGTH_FIELD..=MAX_FRAME_LEN - 2).contains(&length) {
                self.buf.remove(0);
                continue;
            }
            let total = length + 2;
            if self.buf.len() < total {
                break;
            }
            if crc8(&self.buf[2..total - 1]) != self.buf[total - 1] {
                self.crc_errors += 1;
                self.buf.remove(0);
                continue;
            }
            frames.append(frame_type.call1((self.buf[0], self.buf[2], PyBytes::new(py, &self.buf[3..total - 1])))?)?;
            self.frames_ok += 1;
            self.buf.drain(..total);
        }
        Ok(frames)
    }
}

// ---------------------------------------------------------------- decoders

fn be_u16(p: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([p[o], p[o + 1]])
}

fn be_i16(p: &[u8], o: usize) -> i16 {
    i16::from_be_bytes([p[o], p[o + 1]])
}

fn be_i32(p: &[u8], o: usize) -> i32 {
    i32::from_be_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]])
}

fn gps<'py>(py: Python<'py>, p: &[u8]) -> PyResult<Option<Bound<'py, PyDict>>> {
    if p.len() < 15 {
        return Ok(None);
    }
    let (lat, lon) = (be_i32(p, 0), be_i32(p, 4));
    let (speed, course, alt, sats) = (be_u16(p, 8), be_u16(p, 10), be_u16(p, 12), p[14]);
    let d = PyDict::new(py);
    d.set_item("lat", lat as f64 / 1e7)?;
    d.set_item("lon", lon as f64 / 1e7)?;
    d.set_item("groundspeed", speed as f64 / 36.0)?;
    d.set_item("heading", py_mod(course as f64 / 100.0, 360.0))?;
    d.set_item("altitude_amsl", (alt as i64 - 1000) as f64)?;
    d.set_item("satellites", sats)?;
    d.set_item("fix", !(lat == 0 && lon == 0))?;
    Ok(Some(d))
}

fn vario<'py>(py: Python<'py>, p: &[u8]) -> PyResult<Option<Bound<'py, PyDict>>> {
    if p.len() < 2 {
        return Ok(None);
    }
    let d = PyDict::new(py);
    d.set_item("vspeed", be_i16(p, 0) as f64 / 100.0)?;
    Ok(Some(d))
}

fn battery<'py>(py: Python<'py>, p: &[u8]) -> PyResult<Option<Bound<'py, PyDict>>> {
    if p.len() < 8 {
        return Ok(None);
    }
    let used = ((p[4] as u32) << 16) | ((p[5] as u32) << 8) | p[6] as u32;
    let d = PyDict::new(py);
    d.set_item("battery_voltage", be_u16(p, 0) as f64 / 10.0)?;
    d.set_item("battery_current", be_u16(p, 2) as f64 / 10.0)?;
    d.set_item("battery_used_mah", used)?;
    d.set_item("battery_percent", p[7].min(100))?;
    Ok(Some(d))
}

fn baro_altitude<'py>(py: Python<'py>, p: &[u8]) -> PyResult<Option<Bound<'py, PyDict>>> {
    if p.len() < 2 {
        return Ok(None);
    }
    let packed = be_u16(p, 0);
    let alt = if packed & 0x8000 != 0 { (packed & 0x7FFF) as f64 } else { (packed as i64 - 10000) as f64 / 10.0 };
    let d = PyDict::new(py);
    d.set_item("altitude_rel", alt)?;
    Ok(Some(d))
}

fn link_statistics<'py>(py: Python<'py>, p: &[u8]) -> PyResult<Option<Bound<'py, PyDict>>> {
    if p.len() < 10 {
        return Ok(None);
    }
    let d = PyDict::new(py);
    d.set_item("uplink_rssi_1", -(p[0] as i64))?;
    d.set_item("uplink_rssi_2", -(p[1] as i64))?;
    d.set_item("uplink_lq", p[2])?;
    d.set_item("uplink_snr", p[3] as i8)?;
    d.set_item("active_antenna", p[4])?;
    d.set_item("rf_mode", p[5])?;
    d.set_item("uplink_tx_power", p[6])?;
    d.set_item("downlink_rssi", -(p[7] as i64))?;
    d.set_item("downlink_lq", p[8])?;
    d.set_item("downlink_snr", p[9] as i8)?;
    Ok(Some(d))
}

fn attitude<'py>(py: Python<'py>, p: &[u8]) -> PyResult<Option<Bound<'py, PyDict>>> {
    if p.len() < 6 {
        return Ok(None);
    }
    let d = PyDict::new(py);
    d.set_item("pitch", (be_i16(p, 0) as f64 / 10000.0).to_degrees())?;
    d.set_item("roll", (be_i16(p, 2) as f64 / 10000.0).to_degrees())?;
    d.set_item("yaw", py_mod((be_i16(p, 4) as f64 / 10000.0).to_degrees(), 360.0))?;
    Ok(Some(d))
}

fn is_py_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\x1c'..='\x1f' | ' ')
}

fn flight_mode<'py>(py: Python<'py>, p: &[u8]) -> PyResult<Option<Bound<'py, PyDict>>> {
    let end = p.iter().position(|&b| b == 0).unwrap_or(p.len());
    let text: String = p[..end].iter().map(|&b| if b < 0x80 { b as char } else { '\u{FFFD}' }).collect();
    let name = text.trim_matches(is_py_space);
    if name.is_empty() {
        return Ok(None);
    }
    let d = PyDict::new(py);
    d.set_item("mode_label", name)?;
    Ok(Some(d))
}

macro_rules! decoder {
    ($name:ident, $inner:ident) => {
        #[pyfunction]
        fn $name<'py>(payload: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyDict>>> {
            $inner(payload.py(), &bytes_of(payload)?)
        }
    };
}

decoder!(decode_gps, gps);
decoder!(decode_vario, vario);
decoder!(decode_battery, battery);
decoder!(decode_baro_altitude, baro_altitude);
decoder!(decode_link_statistics, link_statistics);
decoder!(decode_attitude, attitude);
decoder!(decode_flight_mode, flight_mode);

/// The snapshot fields of one frame, or None for a type not shown.
#[pyfunction]
fn decode_frame<'py>(frame: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyDict>>> {
    let py = frame.py();
    let kind: u8 = match frame.getattr("type")?.extract() {
        Ok(k) => k,
        Err(_) => return Ok(None),
    };
    let payload = bytes_of(&frame.getattr("payload")?)?;
    let decoded = match kind {
        TYPE_GPS => gps(py, &payload),
        TYPE_VARIO => vario(py, &payload),
        TYPE_BATTERY => battery(py, &payload),
        TYPE_BARO_ALTITUDE => baro_altitude(py, &payload),
        TYPE_LINK_STATISTICS => link_statistics(py, &payload),
        TYPE_ATTITUDE => attitude(py, &payload),
        TYPE_FLIGHT_MODE => flight_mode(py, &payload),
        _ => return Ok(None),
    };
    match decoded {
        Err(err) if caught_by(py, &err, &["ValueError"]) => Ok(None),
        other => other,
    }
}

// ------------------------------------------------------------------ engine

/// The telemetry snapshot, the reader thread and the fly-to planner, run
/// against ``corvus.elrs``.
#[pyclass(name = "ElrsEngine", module = "corvus_link.elrs", frozen)]
pub struct ElrsEngine {
    env: PyObject,
}

impl ElrsEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn log_exception(&self, py: Python<'_>, level: &str, text: &str, err: &PyErr) -> PyResult<()> {
        let kwargs = kw(py, &[("exc_info", err.value(py).clone().into_any().unbind())])?;
        self.env(py).getattr("logger")?.call_method(level, (text,), Some(&kwargs))?;
        Ok(())
    }

    fn plan<'py>(&self, py: Python<'py>, args: &[Bound<'py, PyAny>], kwargs: &[(&str, PyObject)]) -> PyResult<Bound<'py, PyAny>> {
        let plan = self.env(py).getattr("FlyToPlan")?;
        let kwargs = kw(py, kwargs)?;
        plan.call(PyTuple::new(py, args)?, Some(&kwargs))
    }

    fn refuse<'py>(&self, py: Python<'py>, reason: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        self.plan(py, &[false.into_pyobject(py)?.to_owned().into_any(), reason], &[])
    }
}

#[pymethods]
impl ElrsEngine {
    /// *env* is ``corvus.elrs``.
    #[new]
    fn new(env: PyObject) -> Self {
        ElrsEngine { env }
    }

    /// Parse raw bytes from the link and apply every frame; the frame count.
    fn feed(&self, telemetry: &Bound<'_, PyAny>, data: &Bound<'_, PyAny>) -> PyResult<usize> {
        within(&telemetry.getattr("_lock")?, || {
            let frames = telemetry.getattr("_parser")?.call_method1("feed", (data,))?;
            for frame in frames.try_iter()? {
                telemetry.call_method1("_apply_locked", (frame?,))?;
            }
            frames.len()
        })
    }

    /// Fold one frame into the snapshot data.
    fn apply_locked(&self, telemetry: &Bound<'_, PyAny>, frame: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = telemetry.py();
        let env = self.env(py);
        let fields = env.call_method1("decode_frame", (frame,))?;
        if !fields.is_truthy()? {
            return Ok(());
        }
        let now = telemetry.call_method0("_clock")?;
        telemetry.setattr("_last_frame", &now)?;
        let data = telemetry.getattr("_data")?;
        let kind = frame.getattr("type")?;
        if kind.eq(env.getattr("TYPE_GPS")?)? {
            if fields.call_method1("pop", ("fix",))?.is_truthy()? {
                let lat = fields.call_method1("pop", ("lat",))?;
                let lon = fields.call_method1("pop", ("lon",))?;
                data.set_item("position", PyList::new(py, [lat, lon])?)?;
                telemetry.setattr("_last_position", &now)?;
            } else {
                for key in ["lat", "lon", "altitude_amsl", "heading"] {
                    fields.call_method1("pop", (key,))?;
                }
            }
            data.call_method1("update", (&fields,))?;
        } else if kind.eq(env.getattr("TYPE_LINK_STATISTICS")?)? {
            data.set_item("link", &fields)?;
        } else {
            data.call_method1("update", (&fields,))?;
        }
        Ok(())
    }

    /// A copy of the current state, with ``stale`` set from the clock.
    fn snapshot<'py>(&self, telemetry: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = telemetry.py();
        let env = self.env(py);
        within(&telemetry.getattr("_lock")?, || {
            let data = telemetry.getattr("_data")?;
            let out = py.get_type::<PyDict>().call1((&data,))?;
            out.set_item("link", py.get_type::<PyDict>().call1((data.get_item("link")?,))?)?;
            let position = out.get_item("position")?;
            if !position.is_none() {
                out.set_item("position", py.get_type::<PyList>().call1((position,))?)?;
            }
            let last = telemetry.getattr("_last_position")?;
            let stale = last.is_none() || telemetry.call_method0("_clock")?.sub(&last)?.gt(env.getattr("STALE_S")?)?;
            out.set_item("stale", stale)?;
            out.set_item("has_position", !out.get_item("position")?.is_none())?;
            let parser = telemetry.getattr("_parser")?;
            out.set_item("frames_ok", parser.getattr("frames_ok")?)?;
            out.set_item("crc_errors", parser.getattr("crc_errors")?)?;
            Ok(out)
        })
    }

    // ------------------------------------------------------------ reader

    /// Open the source and start reading; refused while the switch is off.
    fn start(&self, reader: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = reader.py();
        if !self.env(py).getattr("ENABLED")?.is_truthy()? {
            return Err(PyRuntimeError::new_err("ExpressLRS telemetry is not enabled in this build"));
        }
        if reader.getattr("running")?.is_truthy()? {
            return Ok(());
        }
        reader.getattr("_stop")?.call_method0("clear")?;
        let source = reader.call_method0("_open_source")?;
        within(&reader.getattr("_source_lock")?, || reader.setattr("_source", source))?;
        let target = reader.clone().unbind();
        let handle = thread::spawn(py, "corvus-elrs", move |py| {
            target.bind(py).call_method0("_run")?;
            Ok(())
        })?;
        reader.setattr("_thread", handle)
    }

    /// Stop reading, close the source and join the thread; safe twice.
    fn stop(&self, reader: &Bound<'_, PyAny>, timeout: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = reader.py();
        reader.getattr("_stop")?.call_method0("set")?;
        reader.call_method0("_close_source")?;
        let thread = reader.getattr("_thread")?;
        if !thread.is_none() && !thread.is(thread::current(py)?.into_bound(py)) {
            thread.call_method1("join", (timeout,))?;
        }
        reader.setattr("_thread", py.None())
    }

    fn close_source(&self, reader: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = reader.py();
        let source = within(&reader.getattr("_source_lock")?, || {
            let source = reader.getattr("_source")?;
            reader.setattr("_source", py.None())?;
            Ok(source)
        })?;
        if !source.is_none() {
            match source.call_method0("close") {
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    self.log_exception(py, "debug", "Closing the ExpressLRS source failed", &err)?;
                }
                Err(err) => return Err(err),
                Ok(_) => {}
            }
        }
        Ok(())
    }

    /// The reader thread: read, feed, report each update.
    fn run(&self, reader: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = reader.py();
        let env = self.env(py);
        let stop = reader.getattr("_stop")?;
        while !thread::shutting_down() && !stop.call_method0("is_set")?.is_truthy()? {
            let source = within(&reader.getattr("_source_lock")?, || reader.getattr("_source"))?;
            if source.is_none() {
                break;
            }
            let data = match source.call_method1("read", (env.getattr("READ_CHUNK")?,)) {
                Ok(d) => d,
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    if !stop.call_method0("is_set")?.is_truthy()? {
                        self.log_exception(py, "warning", "ExpressLRS telemetry source failed", &err)?;
                    }
                    break;
                }
                Err(err) => return Err(err),
            };
            if !data.is_truthy()? {
                continue;
            }
            let telemetry = reader.getattr("telemetry")?;
            if telemetry.call_method1("feed", (data,))?.is_truthy()? && !reader.getattr("_on_update")?.is_none() {
                let update = (|| -> PyResult<()> {
                    reader.getattr("_on_update")?.call1((telemetry.call_method0("snapshot")?,))?;
                    Ok(())
                })();
                match update {
                    Err(err) if err.is_instance_of::<PyException>(py) => {
                        self.log_exception(py, "error", "ExpressLRS update callback failed", &err)?;
                    }
                    Err(err) => return Err(err),
                    Ok(()) => {}
                }
            }
        }
        reader.call_method0("_close_source")?;
        Ok(())
    }

    // --------------------------------------------------------- fly to

    /// What "Fly to this point" would send over an ExpressLRS link, or why
    /// it cannot.
    #[pyo3(signature = (link_mode, stack, mav_type, lat, lon, alt_agl, home_alt_amsl = None))]
    #[allow(clippy::too_many_arguments)]
    fn plan_fly_to<'py>(
        &self,
        link_mode: &Bound<'py, PyAny>,
        stack: &Bound<'py, PyAny>,
        mav_type: &Bound<'py, PyAny>,
        lat: &Bound<'py, PyAny>,
        lon: &Bound<'py, PyAny>,
        alt_agl: &Bound<'py, PyAny>,
        home_alt_amsl: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = link_mode.py();
        let env = self.env(py);
        let text = |s: &str| PyString::new(py, s).into_any();
        if !env.getattr("LINK_MODES")?.contains(link_mode)? {
            return self.refuse(py, PyString::new(py, "Unknown ExpressLRS link mode: {!r}").call_method1("format", (link_mode,))?);
        }
        if link_mode.eq("crsf")? {
            return self.refuse(
                py,
                text("Not available over CRSF telemetry. Switch the ExpressLRS link to MAVLink mode (ELRS 3.5 or later) to send a position."),
            );
        }
        let floats = (|| -> PyResult<(f64, f64, f64)> { Ok((py_float(lat)?, py_float(lon)?, py_float(alt_agl)?)) })();
        let (lat_f, lon_f, alt_f) = match floats {
            Ok(v) => v,
            Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => return self.refuse(py, text("Point is not a number")),
            Err(err) => return Err(err),
        };
        if [lat, lon, alt_agl].iter().any(|v| v.is_instance_of::<PyBool>()) || ![lat_f, lon_f, alt_f].iter().all(|v| v.is_finite()) {
            return self.refuse(py, text("Point is not a number"));
        }
        if !((-90.0..=90.0).contains(&lat_f) && (-180.0..=180.0).contains(&lon_f)) {
            return self.refuse(py, text("Point is outside valid coordinates"));
        }
        let dialect = env.getattr("autopilot_dialect")?.call_method1("dialect_for_stack", (stack,))?;
        let params = PyTuple::new(py, [-1.0, env.getattr("DO_REPOSITION_CHANGE_MODE")?.extract::<f64>()?, 0.0, f64::NAN])?;
        let (frame, z): (Bound<'py, PyAny>, f64) = if dialect.call_method1("reposition_altitude_frame", (mav_type,))?.eq("relative")? {
            (env.getattr("MAV_FRAME_GLOBAL_RELATIVE_ALT")?, alt_f)
        } else {
            match home_alt_amsl {
                None => return self.refuse(py, text("No home altitude yet to convert the height above ground")),
                Some(h) if h.is_none() => return self.refuse(py, text("No home altitude yet to convert the height above ground")),
                Some(h) => (env.getattr("MAV_FRAME_GLOBAL")?, py_float(h)? + alt_f),
            }
        };
        let to_e7 = |v: f64| -> PyObject { ((v * 1e7).round_ties_even() as i64).into_pyobject(py).expect("ints convert").into_any().unbind() };
        self.plan(
            py,
            &[true.into_pyobject(py)?.to_owned().into_any()],
            &[
                ("command", env.getattr("MAV_CMD_DO_REPOSITION")?.unbind()),
                ("frame", frame.unbind()),
                ("params", params.into_any().unbind()),
                ("x", to_e7(lat_f)),
                ("y", to_e7(lon_f)),
                ("z", PyFloat::new(py, z).into_any().unbind()),
            ],
        )
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.elrs")?;
    m.add_class::<CrsfParser>()?;
    m.add_class::<ElrsEngine>()?;
    m.add_function(wrap_pyfunction!(crc8_dvb_s2, &m)?)?;
    m.add_function(wrap_pyfunction!(build_frame, &m)?)?;
    m.add_function(wrap_pyfunction!(decode_gps, &m)?)?;
    m.add_function(wrap_pyfunction!(decode_vario, &m)?)?;
    m.add_function(wrap_pyfunction!(decode_battery, &m)?)?;
    m.add_function(wrap_pyfunction!(decode_baro_altitude, &m)?)?;
    m.add_function(wrap_pyfunction!(decode_link_statistics, &m)?)?;
    m.add_function(wrap_pyfunction!(decode_attitude, &m)?)?;
    m.add_function(wrap_pyfunction!(decode_flight_mode, &m)?)?;
    m.add_function(wrap_pyfunction!(decode_frame, &m)?)?;
    parent.add_submodule(&m)?;
    py.import("sys")?.getattr("modules")?.set_item("corvus_link.elrs", &m)?;
    Ok(())
}
