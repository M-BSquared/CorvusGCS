//! Telemetry the bridge turns into state: battery detail and Corvus's own
//! battery estimate, GNSS jamming and spoofing, the controller setpoints the
//! tuning page plots, the transmitter channels, the radio's link quality and
//! heartbeat jitter, and the stream rates the pages ask for while open.
//!
//! The numbers end up on screen and in tests, so they are computed the way
//! the Python computed them: `round()` rounds exact ties to even, and
//! `sum()` is Python 3.12's compensated (Neumaier) sum. The battery
//! estimator itself (chemistry curves, settings) stays in `corvus.battery`,
//! which this engine calls.

use std::collections::VecDeque;
use std::sync::Mutex;

use pyo3::exceptions::{PyOverflowError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyInt, PyList, PyModule};

use crate::lock;
use crate::protocol::host::{caught_by, monotonic_of, obj, py_float, py_int, set_error, truthy};
use crate::wire::crc;

const HEARTBEAT_WINDOW: usize = 30;
const DRAIN_WINDOW: usize = 400;

/// SiK RSSI is a 0 to 255 relative scale: about 150 is strong, 40 weak.
const SIK_RSSI_STRONG: f64 = 150.0;
const SIK_RSSI_WEAK: f64 = 40.0;

/// GNSS_INTEGRITY (id 441) from development.xml: its id, CRC seed and
/// payload length ("<IHHBBBBBBBBB").
const GNSS_INTEGRITY_ID: u32 = 441;
const GNSS_INTEGRITY_CRC_EXTRA: u8 = 169;
const GNSS_INTEGRITY_LEN: usize = 17;

// ---------------------------------------------------------------- Python math

/// `round(x, digits)`: correctly rounded, exact ties to even, non-finite
/// values unchanged.
pub fn py_round(x: f64, digits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// `round(x)` to an int, raising as Python does for inf and NaN.
fn round_int(x: f64) -> PyResult<i64> {
    if x.is_nan() {
        return Err(PyValueError::new_err("cannot convert float NaN to integer"));
    }
    if x.is_infinite() {
        return Err(PyOverflowError::new_err("cannot convert float infinity to integer"));
    }
    Ok(x.round_ties_even() as i64)
}

/// Python 3.12's `sum()` of floats: the first term added to the int 0, the
/// rest with Neumaier's compensation.
pub fn py_sum(values: &[f64]) -> f64 {
    let Some((first, rest)) = values.split_first() else {
        return 0.0;
    };
    let mut total = 0.0 + first;
    let mut c = 0.0;
    for &x in rest {
        let t = total + x;
        if total.abs() >= x.abs() {
            c += (total - t) + x;
        } else {
            c += (x - t) + total;
        }
        total = t;
    }
    if c != 0.0 && c.is_finite() {
        total += c;
    }
    total
}

/// `min(a, b)`: `a` unless `b` is smaller, so a NaN `b` loses.
fn py_min(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}

/// `max(a, b)`: `a` unless `b` is larger.
fn py_max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

/// Population standard deviation of the intervals, in ms, rounded to 0.1.
fn jitter_ms(times: &VecDeque<f64>) -> f64 {
    if times.len() < 2 {
        return 0.0;
    }
    let intervals: Vec<f64> = times
        .iter()
        .zip(times.iter().skip(1))
        .map(|(a, b)| (b - a) * 1000.0)
        .collect();
    let n = intervals.len() as f64;
    let mean = py_sum(&intervals) / n;
    let squares: Vec<f64> = intervals.iter().map(|v| (v - mean).powi(2)).collect();
    py_round((py_sum(&squares) / n).sqrt(), 1)
}

// ----------------------------------------------------------- Python access

fn attr_or<'py>(o: &Bound<'py, PyAny>, name: &str, fallback: PyObject) -> PyResult<Bound<'py, PyAny>> {
    match o.getattr(name) {
        Ok(v) => Ok(v),
        Err(err) if caught_by(o.py(), &err, &["AttributeError"]) => Ok(fallback.into_bound(o.py())),
        Err(err) => Err(err),
    }
}

/// `value or fallback`.
fn or(value: Bound<'_, PyAny>, fallback: PyObject) -> PyResult<Bound<'_, PyAny>> {
    if value.is_truthy()? {
        Ok(value)
    } else {
        Ok(fallback.into_bound(value.py()))
    }
}

/// `int(getattr(o, name, fallback) or 0)`.
fn int_attr(o: &Bound<'_, PyAny>, name: &str, fallback: i64) -> PyResult<i64> {
    let py = o.py();
    py_int(&or(attr_or(o, name, obj(py, fallback))?, obj(py, 0))?)
}

fn update(bridge: &Bound<'_, PyAny>, pairs: Vec<(&str, PyObject)>) -> PyResult<()> {
    let kwargs = PyDict::new(bridge.py());
    for (k, v) in pairs {
        kwargs.set_item(k, v)?;
    }
    bridge.getattr("_store")?.call_method("update", (), Some(&kwargs))?;
    Ok(())
}

fn caught(py: Python<'_>, result: PyResult<()>, kinds: &[&str]) -> PyResult<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(err) if caught_by(py, &err, kinds) => Ok(false),
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------- functions

/// RC_CHANNELS.rssi as 0 to 100, or -1 when the receiver does not report it.
/// 255 is "unknown"; above 100 can only be the raw 0 to 254 scale, so it is
/// scaled; at or below 100 it is already the percentage it claims to be.
#[pyfunction]
pub fn rc_rssi_percent(value: &Bound<'_, PyAny>) -> PyResult<i64> {
    let rssi = match value.py().get_type::<PyInt>().call1((value,)) {
        Ok(v) => match v.extract::<i64>() {
            Ok(v) => v,
            Err(_) => return Ok(-1),
        },
        Err(err) if caught_by(value.py(), &err, &["TypeError", "ValueError"]) => return Ok(-1),
        Err(err) => return Err(err),
    };
    if !(0..255).contains(&rssi) {
        return Ok(-1);
    }
    if rssi <= 100 {
        return Ok(rssi);
    }
    Ok(round_int(rssi as f64 * 100.0 / 254.0)?.clamp(0, 100))
}

/// A MAVLink attitude quaternion as roll, pitch and yaw in degrees, the
/// convention of ATTITUDE. Pitch is clamped before `asin`, since a quaternion
/// a hair off unit length would otherwise land outside its domain.
#[pyfunction]
pub fn quaternion_to_euler_deg(w: f64, x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let roll = (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y));
    let pitch = py_max(-1.0, py_min(1.0, 2.0 * (w * y - z * x))).asin();
    let yaw = (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z));
    (roll.to_degrees(), pitch.to_degrees(), yaw.to_degrees())
}

/// A raw MAVLink 2 GNSS_INTEGRITY frame as its fields, or None for anything
/// that is not an intact frame of that message (MAVLink 1, short, bad CRC).
/// The trimmed trailing zeros of a v2 payload are put back before reading.
#[pyfunction]
pub fn decode_gnss_integrity<'py>(frame: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyDict>>> {
    let py = frame.py();
    let buf: Vec<u8> = match py.import("builtins")?.getattr("bytes")?.call1((frame,)) {
        Ok(b) => b.extract()?,
        Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => return Ok(None),
        Err(err) => return Err(err),
    };
    if buf.len() < 12 || buf[0] != 0xFD {
        return Ok(None);
    }
    let length = buf[1] as usize;
    if u32::from_le_bytes([buf[7], buf[8], buf[9], 0]) != GNSS_INTEGRITY_ID {
        return Ok(None);
    }
    let end = 10 + length;
    if buf.len() < end + 2 || length > GNSS_INTEGRITY_LEN {
        return Ok(None);
    }
    let sum = crc::accumulate(crc::x25(&buf[1..end]), &[GNSS_INTEGRITY_CRC_EXTRA]);
    if sum != u16::from_le_bytes([buf[end], buf[end + 1]]) {
        return Ok(None);
    }
    let mut payload = buf[10..end].to_vec();
    payload.resize(GNSS_INTEGRITY_LEN, 0);
    let out = PyDict::new(py);
    out.set_item("system", buf[5])?;
    out.set_item("id", payload[8])?;
    out.set_item("jamming_state", payload[10])?;
    out.set_item("spoofing_state", payload[11])?;
    out.set_item("gnss_signal_quality", payload[15])?;
    Ok(Some(out))
}

fn integrity_state(code: i64) -> &'static str {
    match code {
        1 => "ok",
        2 => "mitigated",
        3 => "detected",
        _ => "",
    }
}

// ------------------------------------------------------------------- engine

struct State {
    hb_times: VecDeque<f64>,
    radio_status_seen: bool,
    uplink_score: i64,
    battery_settings: PyObject,
    battery_cells: i64,
    drain_samples: VecDeque<(f64, f64)>,
    drain_source: String,
}

/// The telemetry state of one bridge.
#[pyclass(name = "TelemetryEngine", module = "corvus_link.telemetry", frozen)]
pub struct TelemetryEngine {
    env: PyObject,
    st: Mutex<State>,
}

impl TelemetryEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn battery_math<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr("battery_math")
    }

    fn msg_id(&self, py: Python<'_>, name: &str) -> PyResult<PyObject> {
        Ok(self
            .env(py)
            .getattr("mavutil")?
            .getattr("mavlink")?
            .getattr(name)?
            .unbind())
    }

    /// The interval for a page's request: never slower than the session's own
    /// rate for that message, and that rate back (or 0, the firmware's
    /// default) when the page is done.
    fn interval(bridge: &Bound<'_, PyAny>, msg_id: &Bound<'_, PyAny>, requested: Option<i64>) -> PyResult<i64> {
        let baseline = py_int(
            &bridge
                .call_method0("_message_intervals")?
                .call_method1("get", (msg_id, 0))?,
        )?;
        Ok(match requested {
            None => baseline,
            Some(r) if baseline > 0 => r.min(baseline),
            Some(r) => r,
        })
    }

    fn set_interval(bridge: &Bound<'_, PyAny>, msg_id: &Bound<'_, PyAny>, interval: i64) -> PyResult<bool> {
        bridge
            .call_method1("set_message_interval", (msg_id, interval))?
            .is_truthy()
    }

    /// An integer rate in 1 to 50 Hz as its interval, or the refusal.
    fn rate_interval(bridge: &Bound<'_, PyAny>, rate_hz: &Bound<'_, PyAny>, what: &str) -> PyResult<Option<i64>> {
        if rate_hz.is_instance_of::<PyBool>() || !rate_hz.is_instance_of::<PyInt>() {
            set_error(bridge, &format!("{what} rate must be an integer"))?;
            return Ok(None);
        }
        match rate_hz.extract::<i64>() {
            Ok(rate) if (1..=50).contains(&rate) => Ok(Some(((1_000_000.0 / rate as f64) as i64).max(1))),
            _ => {
                set_error(bridge, &format!("{what} rate must be between 1 and 50 Hz"))?;
                Ok(None)
            }
        }
    }
}

#[pymethods]
impl TelemetryEngine {
    /// *env* is ``corvus.mavlink_telemetry``, which holds ``battery_math``,
    /// ``rc_config``, ``mavutil`` and the clock ``time``.
    #[new]
    fn new(py: Python<'_>, env: PyObject) -> PyResult<Self> {
        let settings = env
            .bind(py)
            .getattr("battery_math")?
            .call_method1("settings", (py.None(),))?
            .unbind();
        Ok(TelemetryEngine {
            env,
            st: Mutex::new(State {
                hb_times: VecDeque::with_capacity(HEARTBEAT_WINDOW),
                radio_status_seen: false,
                uplink_score: 0,
                battery_settings: settings,
                battery_cells: 0,
                drain_samples: VecDeque::with_capacity(DRAIN_WINDOW),
                drain_source: String::new(),
            }),
        })
    }

    #[getter]
    fn hb_times(&self) -> Vec<f64> {
        lock(&self.st).hb_times.iter().copied().collect()
    }
    #[getter]
    fn radio_status_seen(&self) -> bool {
        lock(&self.st).radio_status_seen
    }
    #[setter]
    fn set_radio_status_seen(&self, value: bool) {
        lock(&self.st).radio_status_seen = value;
    }
    #[getter]
    fn uplink_score(&self) -> i64 {
        lock(&self.st).uplink_score
    }
    #[setter]
    fn set_uplink_score(&self, value: i64) {
        lock(&self.st).uplink_score = value;
    }
    #[getter]
    fn battery_cells(&self) -> i64 {
        lock(&self.st).battery_cells
    }
    #[setter]
    fn set_battery_cells(&self, value: i64) {
        lock(&self.st).battery_cells = value;
    }
    #[getter]
    fn drain_samples(&self) -> Vec<(f64, f64)> {
        lock(&self.st).drain_samples.iter().copied().collect()
    }
    #[getter]
    fn drain_source(&self) -> String {
        lock(&self.st).drain_source.clone()
    }

    /// A heartbeat landed at *stamp* (the store's clock).
    fn note_heartbeat(&self, stamp: f64) {
        let mut st = lock(&self.st);
        if st.hb_times.len() == HEARTBEAT_WINDOW {
            st.hb_times.pop_front();
        }
        st.hb_times.push_back(stamp);
    }

    /// Start a fresh jitter window and radio view for a new link.
    fn forget_link(&self) {
        let mut st = lock(&self.st);
        st.hb_times.clear();
        st.radio_status_seen = false;
        st.uplink_score = 0;
    }

    // ---------------------------------------------------------- battery

    /// Install the operator's estimator settings and return them resolved;
    /// the latched cell count goes with the old ones.
    fn set_battery_settings(&self, py: Python<'_>, raw: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        let resolved = self.battery_math(py)?.call_method1("settings", (raw,))?.unbind();
        let mut st = lock(&self.st);
        st.battery_settings = resolved.clone_ref(py);
        st.battery_cells = 0;
        Ok(resolved)
    }

    fn battery_settings<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let settings = lock(&self.st).battery_settings.clone_ref(py);
        py.get_type::<PyDict>().call1((settings,))
    }

    /// Drop the latched cell count and the drain series (a new link may be a
    /// new pack).
    fn forget_battery_cells(&self) {
        let mut st = lock(&self.st);
        st.battery_cells = 0;
        st.drain_samples.clear();
    }

    /// The battery half of a SYS_STATUS update, the autopilot's percentage
    /// and Corvus's estimate side by side. The cell count is latched on the
    /// first reading of a connection, where the voltage is unambiguous.
    fn battery_fields<'py>(
        &self,
        py: Python<'py>,
        voltage: f64,
        current: f64,
        reported: i64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let (settings, latched) = {
            let st = lock(&self.st);
            (st.battery_settings.clone_ref(py), st.battery_cells)
        };
        let resolved = py.get_type::<PyDict>().call1((settings,))?.downcast_into::<PyDict>()?;
        let configured = py_int(&or(resolved.as_any().call_method1("get", ("cells",))?, obj(py, 0))?)?;
        if latched != 0 && configured == 0 {
            resolved.set_item("cells", latched)?;
        }
        let est = self
            .battery_math(py)?
            .call_method1("estimate", (voltage, current, &resolved))?;
        let cells = py_int(&est.get_item("cells")?)?;
        if cells != 0 && latched == 0 && configured == 0 {
            let mut st = lock(&self.st);
            if st.battery_cells == 0 {
                st.battery_cells = cells;
            }
        }
        let estimated = py_float(&est.get_item("percent")?)?;
        let use_estimate = resolved.as_any().call_method1("get", ("estimate",))?.is_truthy()? && estimated >= 0.0;
        let shown = if use_estimate {
            round_int(estimated)?
        } else if reported >= 0 {
            reported
        } else {
            0
        };
        let out = PyDict::new(py);
        out.set_item("battery_voltage", py_round(voltage, 1))?;
        out.set_item("battery_current", py_round(current, 1))?;
        out.set_item("battery_percent", shown)?;
        out.set_item("battery_percent_fc", reported)?;
        out.set_item("battery_percent_est", estimated)?;
        out.set_item("battery_source", if use_estimate { "estimate" } else { "autopilot" })?;
        out.set_item("battery_cells", cells)?;
        out.set_item("battery_cell_voltage", py_float(&est.get_item("cell_voltage")?)?)?;
        Ok(out)
    }

    /// Corvus's own time to empty from the drain rate of the percentage being
    /// flown by: kept only while armed, restarted when that percentage's
    /// source changes. *percent* is None when neither source has one.
    #[pyo3(signature = (percent, source, armed, now=None))]
    fn battery_endurance_fields<'py>(
        &self,
        py: Python<'py>,
        percent: &Bound<'py, PyAny>,
        source: String,
        armed: bool,
        now: Option<f64>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let stamp = match now {
            Some(t) => t,
            None => monotonic_of(&self.env(py).getattr("time")?)?,
        };
        let percent = if percent.is_none() { None } else { Some(percent) };
        let out = PyDict::new(py);
        let samples = {
            let mut st = lock(&self.st);
            if !armed || percent.is_none() || source != st.drain_source {
                st.drain_samples.clear();
                st.drain_source = source;
            }
            let Some(percent) = percent else {
                out.set_item("battery_endurance_est", -1)?;
                return Ok(out);
            };
            if !armed {
                out.set_item("battery_endurance_est", -1)?;
                return Ok(out);
            }
            let value = py_float(percent)?;
            if st.drain_samples.len() == DRAIN_WINDOW {
                st.drain_samples.pop_front();
            }
            st.drain_samples.push_back((stamp, value));
            st.drain_samples.iter().copied().collect::<Vec<_>>()
        };
        let seconds = py_float(&self.battery_math(py)?.call_method1("drain_endurance", (samples,))?)?;
        out.set_item(
            "battery_endurance_est",
            if seconds >= 0.0 { seconds.trunc() as i64 } else { -1 },
        )?;
        Ok(out)
    }

    /// BATTERY_STATUS detail, first battery only, each field only when the
    /// message carries a real value. A pack voltage in slot 0 (or carried
    /// into slot 1 above 65.534 V) is not a cell reading.
    fn handle_battery_status(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if int_attr(msg, "id", 0)? != 0 {
            return Ok(());
        }
        let builtins = py.import("builtins")?;
        let mut slots: Vec<i64> = Vec::new();
        for name in ["voltages", "voltages_ext"] {
            let raw = or(
                attr_or(msg, name, PyList::empty(py).into_any().unbind())?,
                PyList::empty(py).into_any().unbind(),
            )?;
            for item in builtins.getattr("list")?.call1((raw,))?.try_iter()? {
                slots.push(py_int(&item?)?);
            }
        }
        let used: Vec<i64> = slots.iter().copied().filter(|v| *v != 0 && *v != 65535).collect();
        let pack_only = !slots.is_empty()
            && ((used.len() == 1 && slots[0] == used[0])
                || (slots[0] == 65534 && used.len() == 2 && slots.get(1) == used.get(1)));
        let cells: Vec<f64> = if pack_only {
            Vec::new()
        } else {
            used.iter().map(|v| py_round(*v as f64 / 1000.0, 3)).collect()
        };

        let mut fields: Vec<(&str, PyObject)> = vec![("battery_cell_voltages", obj(py, cells))];
        let raw_consumed = attr_or(msg, "current_consumed", obj(py, -1))?;
        let consumed = if raw_consumed.is_none() {
            -1
        } else {
            py_int(&raw_consumed)?
        };
        if consumed >= 0 {
            fields.push(("battery_consumed_mah", obj(py, consumed as f64)));
        }
        let temperature = int_attr(msg, "temperature", 32767)?;
        let temperature = if temperature == 32767 {
            py.None()
        } else {
            obj(py, py_round(temperature as f64 / 100.0, 1))
        };
        fields.push(("battery_temperature", temperature));
        fields.push((
            "battery_time_remaining",
            obj(py, int_attr(msg, "time_remaining", 0)?.max(0)),
        ));
        update(bridge, fields)
    }

    // ---------------------------------------------------- GNSS integrity

    /// The receiver's own jamming and spoofing verdicts, first receiver of
    /// the aircraft only. An UNKNOWN_441 frame is decoded and checked here.
    fn handle_gnss_integrity(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let (system, id, jamming, spoofing, signal) = if msg.call_method0("get_type")?.eq("GNSS_INTEGRITY")? {
            let system = match msg.getattr("get_srcSystem") {
                Ok(f) => f.call0()?,
                Err(err) if caught_by(py, &err, &["AttributeError"]) => obj(py, 0).into_bound(py),
                Err(err) => return Err(err),
            };
            (
                system,
                int_attr(msg, "id", 0)?,
                int_attr(msg, "jamming_state", 0)?,
                int_attr(msg, "spoofing_state", 0)?,
                py_int(&attr_or(msg, "gnss_signal_quality", obj(py, 255))?)?,
            )
        } else {
            let data = attr_or(msg, "data", pyo3::types::PyBytes::new(py, b"").into_any().unbind())?;
            let Some(fields) = decode_gnss_integrity(&data)? else {
                return Ok(());
            };
            let get = |k: &str| -> PyResult<i64> { fields.as_any().get_item(k)?.extract() };
            (
                fields.as_any().get_item("system")?,
                get("id")?,
                get("jamming_state")?,
                get("spoofing_state")?,
                get("gnss_signal_quality")?,
            )
        };
        let target = int_attr(bridge, "_target_system", 0)?;
        if system.is_truthy()? && target != 0 && system.ne(target)? {
            return Ok(());
        }
        if id != 0 {
            return Ok(());
        }
        update(
            bridge,
            vec![
                ("gps_jamming", obj(py, integrity_state(jamming))),
                ("gps_spoofing", obj(py, integrity_state(spoofing))),
                (
                    "gps_signal_quality",
                    obj(py, if (0..=10).contains(&signal) { signal } else { -1 }),
                ),
            ],
        )
    }

    fn forget_gnss_integrity(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        update(
            bridge,
            vec![
                ("gps_jamming", obj(py, "")),
                ("gps_spoofing", obj(py, "")),
                ("gps_signal_quality", obj(py, -1)),
            ],
        )
    }

    // -------------------------------------------------------- setpoints

    /// ATTITUDE_TARGET: the attitude and body rate setpoints, in degrees. A
    /// malformed message is dropped, a short quaternion leaves the attitude
    /// out: an invented setpoint on a tuning graph is worse than none.
    fn handle_attitude_target(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let mut quat: Vec<Bound<'_, PyAny>> = Vec::new();
        let mut rates = [0.0f64; 3];
        let read = (|| -> PyResult<()> {
            let q = or(attr_or(msg, "q", py.None())?, PyList::empty(py).into_any().unbind())?;
            for item in py.import("builtins")?.getattr("list")?.call1((q,))?.try_iter()? {
                quat.push(item?);
            }
            for (slot, name) in rates
                .iter_mut()
                .zip(["body_roll_rate", "body_pitch_rate", "body_yaw_rate"])
            {
                *slot = py_float(&msg.getattr(name)?)?.to_degrees();
            }
            Ok(())
        })();
        if !caught(py, read, &["TypeError", "ValueError", "AttributeError"])? {
            return Ok(());
        }
        let mut fields: Vec<(&str, PyObject)> = vec![
            ("rollspeed_sp", obj(py, py_round(rates[0], 1))),
            ("pitchspeed_sp", obj(py, py_round(rates[1], 1))),
            ("yawspeed_sp", obj(py, py_round(rates[2], 1))),
            ("setpoints_live", obj(py, true)),
        ];
        if quat.len() >= 4 {
            let parts: PyResult<Vec<f64>> = quat[..4].iter().map(py_float).collect();
            match parts {
                Ok(q) => {
                    let (roll, pitch, yaw) = quaternion_to_euler_deg(q[0], q[1], q[2], q[3]);
                    fields.push(("roll_sp", obj(py, py_round(roll, 1))));
                    fields.push(("pitch_sp", obj(py, py_round(pitch, 1))));
                    fields.push(("yaw_sp", obj(py, py_round(yaw, 1))));
                }
                Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => {}
                Err(err) => return Err(err),
            }
        }
        update(bridge, fields)
    }

    /// POSITION_TARGET_LOCAL_NED: the velocity setpoint only, whatever the
    /// type mask says (PX4 fills it with the controller's own setpoint).
    fn handle_position_target(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let result = (|| -> PyResult<()> {
            let v = |name: &str| -> PyResult<PyObject> { Ok(obj(py, py_round(py_float(&msg.getattr(name)?)?, 2))) };
            update(
                bridge,
                vec![
                    ("vx_sp", v("vx")?),
                    ("vy_sp", v("vy")?),
                    ("vz_sp", v("vz")?),
                    ("setpoints_live", obj(py, true)),
                ],
            )
        })();
        caught(py, result, &["TypeError", "ValueError", "AttributeError"]).map(|_| ())
    }

    // ------------------------------------------------------- RC channels

    /// The transmitter channels in microseconds. RC_CHANNELS carries all of
    /// them; RC_CHANNELS_RAW (*raw*) one bank of eight named by `port`, merged
    /// into what the other bank published. UINT16_MAX ("not delivered") is 0.
    #[pyo3(signature = (bridge, msg, raw=false))]
    fn handle_rc_channels(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>, raw: bool) -> PyResult<()> {
        let py = bridge.py();
        let max: i64 = self.env(py).getattr("rc_config")?.getattr("MAX_CHANNELS")?.extract()?;
        let offset = if raw {
            match int_attr(msg, "port", 0) {
                Ok(port) => port * 8,
                Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => return Ok(()),
                Err(err) => return Err(err),
            }
        } else {
            0
        };
        let width = if raw { 8 } else { max };
        if offset < 0 || offset >= max {
            return Ok(());
        }
        let mut values: Vec<i64> = Vec::new();
        for index in 1..=width {
            let value = attr_or(msg, &format!("chan{index}_raw"), py.None())?;
            if value.is_none() {
                break;
            }
            let pulse = match py_int(&value) {
                Ok(p) => p,
                Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => 0,
                Err(err) => return Err(err),
            };
            values.push(if pulse == 65535 { 0 } else { pulse.clamp(0, 65534) });
        }
        let count;
        if raw {
            let room = (max - offset) as usize;
            values.truncate(room);
            if values.is_empty() {
                return Ok(());
            }
            let snapshot = bridge.getattr("_store")?.call_method0("get_snapshot")?;
            let current = or(
                snapshot.call_method1("get", ("rc_channels",))?,
                PyList::empty(py).into_any().unbind(),
            )?;
            let mut existing: Vec<PyObject> = Vec::new();
            for item in py.import("builtins")?.getattr("list")?.call1((current,))?.try_iter()? {
                existing.push(item?.unbind());
            }
            let start = offset as usize;
            let needed = start + values.len();
            while existing.len() < needed {
                existing.push(obj(py, 0));
            }
            for (i, v) in values.iter().enumerate() {
                existing[start + i] = obj(py, *v);
            }
            count = existing.len() as i64;
            let channels = PyList::new(py, existing)?;
            return self.publish_rc(bridge, msg, channels.into_any().unbind(), count);
        }
        let claimed = match int_attr(msg, "chancount", 0) {
            Ok(c) => c,
            Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => 0,
            Err(err) => return Err(err),
        };
        if claimed > 0 && claimed as usize <= values.len() {
            values.truncate(claimed as usize);
            count = claimed;
        } else {
            count = values.len() as i64;
        }
        self.publish_rc(bridge, msg, obj(py, values), count)
    }

    fn publish_rc(
        &self,
        bridge: &Bound<'_, PyAny>,
        msg: &Bound<'_, PyAny>,
        channels: PyObject,
        count: i64,
    ) -> PyResult<()> {
        let py = bridge.py();
        let rssi = rc_rssi_percent(&attr_or(msg, "rssi", obj(py, 255))?)?;
        update(
            bridge,
            vec![
                ("rc_channels", channels),
                ("rc_channel_count", obj(py, count)),
                ("rc_rssi", obj(py, rssi)),
                ("rc_live", obj(py, true)),
            ],
        )
    }

    // ------------------------------------------------------ link quality

    /// RADIO_STATUS: the uplink score from the drone's signal as the ground
    /// radio sees it (remrssi) and the radio's buffer. remrssi 0 or 255 is no
    /// reading, which keeps the last score rather than flapping to poor.
    fn handle_radio_status(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let remrssi = py_float(&attr_or(msg, "remrssi", obj(py, 0))?)?;
        let txbuf = py_float(&attr_or(msg, "txbuf", obj(py, 0))?)?;
        let rxerrors = py_int(&attr_or(msg, "rxerrors", obj(py, 0))?)?;
        let fixed = py_int(&attr_or(msg, "fixed", obj(py, 0))?)?;
        let score = {
            let mut st = lock(&self.st);
            st.radio_status_seen = true;
            if remrssi != 0.0 && remrssi != 255.0 {
                let remrssi_pct = py_max(
                    0.0,
                    py_min(
                        100.0,
                        (remrssi - SIK_RSSI_WEAK) / (SIK_RSSI_STRONG - SIK_RSSI_WEAK) * 100.0,
                    ),
                );
                let txbuf_pct = py_max(0.0, py_min(100.0, txbuf));
                st.uplink_score = round_int(0.7 * remrssi_pct + 0.3 * txbuf_pct)?.clamp(0, 100);
            }
            st.uplink_score
        };
        update(
            bridge,
            vec![
                ("uplink", obj(py, score)),
                ("uplink_rssi", obj(py, remrssi)),
                ("uplink_rxerrors", obj(py, rxerrors)),
                ("uplink_fixed", obj(py, fixed)),
            ],
        )?;
        self.publish_link_quality(bridge)
    }

    /// The rolling standard deviation of heartbeat intervals, in ms; 0
    /// before there are two heartbeats.
    fn publish_heartbeat_jitter(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let jitter = jitter_ms(&lock(&self.st).hb_times);
        update(bridge, vec![("heartbeat_jitter_ms", obj(bridge.py(), jitter))])
    }

    fn current_jitter_ms(&self) -> f64 {
        jitter_ms(&lock(&self.st).hb_times)
    }

    /// good, fair or poor from the uplink score and the jitter; without a
    /// radio (UDP SITL) a fresh heartbeat is good, and staleness is the drop
    /// path's to judge.
    fn publish_link_quality(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let (jitter, seen, uplink) = {
            let st = lock(&self.st);
            (jitter_ms(&st.hb_times), st.radio_status_seen, st.uplink_score)
        };
        let quality = if !seen || (uplink >= 70 && jitter < 50.0) {
            "good"
        } else if uplink >= 40 {
            "fair"
        } else {
            "poor"
        };
        update(bridge, vec![("link_quality", obj(bridge.py(), quality))])
    }

    // ------------------------------------------------------ stream rates

    #[pyo3(signature = (bridge, msg_id, requested_us))]
    fn stream_interval_us(
        &self,
        bridge: &Bound<'_, PyAny>,
        msg_id: &Bound<'_, PyAny>,
        requested_us: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<i64> {
        let requested = match requested_us {
            Some(r) if !r.is_none() => Some(py_int(r)?),
            _ => None,
        };
        Self::interval(bridge, msg_id, requested)
    }

    /// VIBRATION at *rate_hz* while the vibration page is open, the
    /// firmware's default (0) after. The caller holds the operation lock.
    fn vibration_stream(
        &self,
        bridge: &Bound<'_, PyAny>,
        enabled: &Bound<'_, PyAny>,
        rate_hz: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let interval = if enabled.is_truthy()? {
            let rate = if rate_hz.is_instance_of::<PyInt>() {
                rate_hz.extract::<i64>().ok()
            } else {
                None
            };
            let Some(rate) = rate.filter(|r| (1..=50).contains(r)) else {
                set_error(bridge, "vibration rate must be between 1 and 50 Hz")?;
                return Ok(false);
            };
            ((1_000_000.0 / rate as f64) as i64).max(1)
        } else {
            0
        };
        let id = self.msg_id(py, "MAVLINK_MSG_ID_VIBRATION")?;
        Self::set_interval(bridge, id.bind(py), interval)
    }

    /// DISTANCE_SENSOR for the top bar while the operator has it on. The
    /// caller holds the operation lock.
    fn rangefinder_stream(&self, bridge: &Bound<'_, PyAny>, enabled: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        if !truthy(bridge, "_connection_ready")? {
            set_error(bridge, "not connected")?;
            update(bridge, vec![("rangefinder_distance", py.None())])?;
            return Ok(false);
        }
        let enabled = enabled.is_truthy()?;
        let requested = if enabled {
            Some(if truthy(bridge, "_is_slow_link")? {
                1_000_000
            } else {
                500_000
            })
        } else {
            None
        };
        let id = self.msg_id(py, "MAVLINK_MSG_ID_DISTANCE_SENSOR")?;
        let interval = Self::interval(bridge, id.bind(py), requested)?;
        let ok = Self::set_interval(bridge, id.bind(py), interval)?;
        if !enabled {
            bridge.setattr("_rangefinder_seen", py.None())?;
            update(bridge, vec![("rangefinder_distance", py.None())])?;
        }
        Ok(ok)
    }

    /// RC_CHANNELS at a calibration rate while the Radio Control page is
    /// open, the session's own rate after. The caller holds the operation lock.
    fn rc_stream(
        &self,
        bridge: &Bound<'_, PyAny>,
        enabled: &Bound<'_, PyAny>,
        rate_hz: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let enabled = enabled.is_truthy()?;
        let requested = if enabled {
            match Self::rate_interval(bridge, rate_hz, "RC")? {
                Some(i) => Some(i),
                None => return Ok(false),
            }
        } else {
            None
        };
        let id = self.msg_id(py, "MAVLINK_MSG_ID_RC_CHANNELS")?;
        let interval = Self::interval(bridge, id.bind(py), requested)?;
        if !truthy(bridge, "_connection_ready")? {
            set_error(bridge, "not connected")?;
            update(bridge, vec![("rc_live", obj(py, false))])?;
            return Ok(false);
        }
        let ok = Self::set_interval(bridge, id.bind(py), interval)?;
        if !enabled {
            update(bridge, vec![("rc_live", obj(py, false))])?;
        }
        Ok(ok)
    }

    /// The tuning page's commanded and achieved pairs at *rate_hz*, each
    /// handed back after. True only if every message was accepted. The caller
    /// holds the operation lock.
    fn tuning_stream(
        &self,
        bridge: &Bound<'_, PyAny>,
        enabled: &Bound<'_, PyAny>,
        rate_hz: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let enabled = enabled.is_truthy()?;
        let requested = if enabled {
            match Self::rate_interval(bridge, rate_hz, "tuning")? {
                Some(i) => Some(i),
                None => return Ok(false),
            }
        } else {
            None
        };
        if !truthy(bridge, "_connection_ready")? {
            set_error(bridge, "not connected")?;
            update(bridge, vec![("setpoints_live", obj(py, false))])?;
            return Ok(false);
        }
        let mut ok = true;
        for id in bridge.getattr("_TUNING_MSG_IDS")?.try_iter()? {
            let id = id?;
            let interval = Self::interval(bridge, &id, requested)?;
            if !Self::set_interval(bridge, &id, interval)? {
                ok = false;
            }
        }
        if !enabled {
            update(bridge, vec![("setpoints_live", obj(py, false))])?;
        }
        Ok(ok)
    }

    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.env)?;
        if let Ok(st) = self.st.try_lock() {
            visit.call(&st.battery_settings)?;
        }
        Ok(())
    }
}

/// `round(x, digits)` for the tests that hold it to Python's.
#[pyfunction]
#[pyo3(name = "_round")]
fn round_for_tests(x: f64, digits: usize) -> f64 {
    py_round(x, digits)
}

/// `sum(values)` for the tests that hold it to Python's.
#[pyfunction]
#[pyo3(name = "_sum")]
fn sum_for_tests(values: Vec<f64>) -> f64 {
    py_sum(&values)
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.telemetry")?;
    m.add_class::<TelemetryEngine>()?;
    m.add_function(wrap_pyfunction!(rc_rssi_percent, &m)?)?;
    m.add_function(wrap_pyfunction!(quaternion_to_euler_deg, &m)?)?;
    m.add_function(wrap_pyfunction!(decode_gnss_integrity, &m)?)?;
    m.add_function(wrap_pyfunction!(round_for_tests, &m)?)?;
    m.add_function(wrap_pyfunction!(sum_for_tests, &m)?)?;
    parent.add_submodule(&m)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("corvus_link.telemetry", &m)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_ties_go_to_even_on_exact_values() {
        assert_eq!(py_round(0.25, 1), 0.2);
        assert_eq!(py_round(0.35, 1), 0.3);
        assert_eq!(py_round(2.675, 2), 2.67);
        assert_eq!(py_round(-0.04, 1).to_bits(), (-0.0f64).to_bits());
        assert!(py_round(f64::NAN, 1).is_nan());
    }

    #[test]
    fn the_sum_is_compensated() {
        assert_eq!(py_sum(&[0.1; 10]), 1.0);
        assert_eq!(py_sum(&[1e100, 1.0, -1e100, 1.0]), 2.0);
        assert_eq!(py_sum(&[]), 0.0);
    }

    #[test]
    fn rssi_and_quaternions() {
        let (r, p, y) = quaternion_to_euler_deg(1.0, 0.0, 0.0, 0.0);
        assert_eq!((r, p, y), (0.0, 0.0, 0.0));
        assert_eq!(jitter_ms(&VecDeque::from(vec![0.0, 1.0, 2.0])), 0.0);
    }
}
