//! Remote ID: the identity a ground station broadcasts for its aircraft, as
//! the four OPEN_DRONE_ID messages it sends at 1 Hz, and the aircraft's
//! OPEN_DRONE_ID_ARM_STATUS verdict on it.
//!
//! Every field arrives from a browser and leaves on a radio, so the stored
//! identity is coerced on the way in: an out of range enum falls back to its
//! default, text is cut to printable ASCII and the field width, numbers are
//! clamped. The coercion follows Python's own `int()`, `float()` and `str()`
//! (the identity is a Python dict), so it treats every input the same way the
//! Python did. The filing checks the Remote ID page shows stay in
//! `corvus.remote_id`.

use std::ops::RangeInclusive;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use pyo3::exceptions::{PyException, PyOverflowError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyList, PyModule, PyString, PyTuple};

use crate::lock;
use crate::protocol::host::{caught_by, monotonic_of, obj};

/// Field widths from the MAVLink message definitions: the hard cap on what
/// can be sent, since a longer value shifts the field rather than truncating.
pub const UAS_ID_MAX: usize = 20;
pub const OPERATOR_ID_MAX: usize = 20;
pub const DESCRIPTION_MAX: usize = 23;
/// The ODID "unknown" sentinel for altitudes and the operational volume.
pub const ALTITUDE_UNKNOWN: f64 = -1000.0;
/// OPEN_DRONE_ID_SYSTEM.timestamp counts seconds from 2019-01-01 UTC.
const ODID_EPOCH_UNIX: f64 = 1_546_300_800.0;

const ARM_STATUS_GOOD: i64 = 0;
const ARM_STATUS_FAIL: i64 = 1;
const LOCATION_TAKEOFF: i64 = 0;
const LOCATION_FIXED: i64 = 2;
const CLASSIFICATION_EU: i64 = 1;
const REGIONS: [&str; 2] = ["faa", "eu"];

/// The values `corvus.remote_id` offers for each MAV_ODID enum; anything else
/// is replaced by the field's default.
const ENUMS: [(&str, RangeInclusive<i64>); 8] = [
    ("id_type", 0..=4),
    ("ua_type", 0..=15),
    ("operator_id_type", 0..=0),
    ("description_type", 0..=2),
    ("operator_location_type", 0..=2),
    ("classification_type", 0..=1),
    ("category_eu", 0..=3),
    ("class_eu", 0..=7),
];

fn values(name: &str) -> RangeInclusive<i64> {
    ENUMS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, r)| r.clone())
        .expect("known enum")
}

/// `int(raw)`, or None where Python's `int()` raised TypeError or ValueError.
fn int_of<'py>(raw: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    match raw.py().get_type::<pyo3::types::PyInt>().call1((raw,)) {
        Ok(v) => Ok(Some(v)),
        Err(err) if caught_by(raw.py(), &err, &["TypeError", "ValueError"]) => Ok(None),
        Err(err) => Err(err),
    }
}

/// `float(raw)`, or None where Python's `float()` raised TypeError or ValueError.
fn float_of(raw: &Bound<'_, PyAny>) -> PyResult<Option<f64>> {
    match raw.py().get_type::<PyFloat>().call1((raw,)) {
        Ok(v) => Ok(Some(v.extract()?)),
        Err(err) if caught_by(raw.py(), &err, &["TypeError", "ValueError"]) => Ok(None),
        Err(err) => Err(err),
    }
}

fn enum_value(raw: &Bound<'_, PyAny>, name: &str, default: i64) -> PyResult<i64> {
    let Some(value) = int_of(raw)? else {
        return Ok(default);
    };
    Ok(match value.extract::<i64>() {
        Ok(v) if values(name).contains(&v) => v,
        _ => default,
    })
}

/// `min(max(value, lo), hi)` as Python orders it.
fn clamp(value: f64, lo: f64, hi: f64) -> f64 {
    let low = if lo > value { lo } else { value };
    if hi < low {
        hi
    } else {
        low
    }
}

fn number(raw: &Bound<'_, PyAny>, default: f64, lo: f64, hi: f64) -> PyResult<f64> {
    match float_of(raw)? {
        Some(v) if v.is_finite() => Ok(clamp(v, lo, hi)),
        _ => Ok(default),
    }
}

fn count(raw: &Bound<'_, PyAny>, default: i64, lo: i64, hi: i64) -> PyResult<i64> {
    if raw.is_instance_of::<PyBool>() {
        return Ok(default);
    }
    let Some(value) = float_of(raw)? else {
        return Ok(default);
    };
    if value.is_nan() {
        return Ok(default);
    }
    if value.is_infinite() {
        return Err(PyOverflowError::new_err("cannot convert float infinity to integer"));
    }
    Ok(clamp(value.trunc(), lo as f64, hi as f64) as i64)
}

/// Python's `str.isspace()`, which also counts the separators 0x1C to 0x1F.
fn python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Printable ASCII of `raw`, at most `limit` characters. A fixed width char
/// field that arrives as bytes is read up to its first NUL.
fn text(raw: &Bound<'_, PyAny>, limit: usize) -> PyResult<String> {
    if raw.is_none() {
        return Ok(String::new());
    }
    let full: String = if raw.is_instance_of::<PyBytes>() || raw.is_instance_of::<PyByteArray>() {
        let bytes: Vec<u8> = raw.extract()?;
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        bytes[..end]
            .iter()
            .filter(|b| b.is_ascii())
            .map(|b| *b as char)
            .collect()
    } else {
        raw.str()?.to_cow()?.into_owned()
    };
    let without_nul: String = full.chars().filter(|c| *c != '\0').collect();
    Ok(without_nul
        .trim_matches(python_space)
        .chars()
        .filter(|c| (0x20..=0x7E).contains(&(*c as u32)))
        .take(limit)
        .collect())
}

/// `str(text or "")` as ASCII, zero padded to exactly `length` bytes.
fn fixed_bytes(value: &Bound<'_, PyAny>, length: usize) -> PyResult<Vec<u8>> {
    let mut raw: Vec<u8> = if value.is_truthy()? {
        value
            .str()?
            .to_cow()?
            .chars()
            .filter(char::is_ascii)
            .map(|c| c as u8)
            .collect()
    } else {
        Vec::new()
    };
    raw.truncate(length);
    raw.resize(length, 0);
    Ok(raw)
}

/// `data.get(key)` when it is a dict, else an empty dict.
fn section<'py>(data: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    let value = data.call_method1("get", (key,))?;
    if value.is_instance_of::<PyDict>() {
        Ok(data.call_method1("get", (key,))?)
    } else {
        Ok(PyDict::new(data.py()).into_any())
    }
}

fn field<'py>(part: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    part.call_method1("get", (key,))
}

fn put<'py, V: IntoPyObject<'py>>(dict: &Bound<'py, PyDict>, key: &str, value: V) -> PyResult<()> {
    dict.set_item(key, value)
}

/// The stored identity, every field bounded and typed.
#[pyfunction]
pub fn settings<'py>(py: Python<'py>, raw: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let data = if raw.is_instance_of::<PyDict>() {
        raw.clone()
    } else {
        PyDict::new(py).into_any()
    };
    let basic = section(&data, "basic_id")?;
    let operator = section(&data, "operator_id")?;
    let self_id = section(&data, "self_id")?;
    let system = section(&data, "system")?;

    let out = PyDict::new(py);
    put(
        &out,
        "enabled",
        data.call_method1("get", ("enabled", false))?.is_truthy()?,
    )?;
    let region = data.call_method1("get", ("region",))?;
    let region = if region.is_truthy()? {
        region
    } else {
        PyString::new(py, "eu").into_any()
    };
    let region: String = region.str()?.call_method0("lower")?.extract()?;
    put(
        &out,
        "region",
        if REGIONS.contains(&region.as_str()) {
            region
        } else {
            "eu".to_string()
        },
    )?;

    let b = PyDict::new(py);
    put(&b, "id_type", enum_value(&field(&basic, "id_type")?, "id_type", 1)?)?;
    put(&b, "ua_type", enum_value(&field(&basic, "ua_type")?, "ua_type", 0)?)?;
    put(&b, "uas_id", text(&field(&basic, "uas_id")?, UAS_ID_MAX)?)?;
    put(&out, "basic_id", b)?;

    let o = PyDict::new(py);
    put(
        &o,
        "operator_id_type",
        enum_value(&field(&operator, "operator_id_type")?, "operator_id_type", 0)?,
    )?;
    put(
        &o,
        "operator_id",
        text(&field(&operator, "operator_id")?, OPERATOR_ID_MAX)?,
    )?;
    put(&out, "operator_id", o)?;

    let s = PyDict::new(py);
    put(
        &s,
        "description_type",
        enum_value(&field(&self_id, "description_type")?, "description_type", 0)?,
    )?;
    put(
        &s,
        "description",
        text(&field(&self_id, "description")?, DESCRIPTION_MAX)?,
    )?;
    put(&out, "self_id", s)?;

    let y = PyDict::new(py);
    let get = |key: &str| field(&system, key);
    put(
        &y,
        "operator_location_type",
        enum_value(
            &get("operator_location_type")?,
            "operator_location_type",
            LOCATION_TAKEOFF,
        )?,
    )?;
    put(
        &y,
        "operator_latitude",
        number(&get("operator_latitude")?, 0.0, -90.0, 90.0)?,
    )?;
    put(
        &y,
        "operator_longitude",
        number(&get("operator_longitude")?, 0.0, -180.0, 180.0)?,
    )?;
    put(
        &y,
        "operator_altitude_geo",
        number(&get("operator_altitude_geo")?, ALTITUDE_UNKNOWN, -1000.0, 31767.0)?,
    )?;
    put(
        &y,
        "classification_type",
        enum_value(&get("classification_type")?, "classification_type", 0)?,
    )?;
    put(&y, "category_eu", enum_value(&get("category_eu")?, "category_eu", 0)?)?;
    put(&y, "class_eu", enum_value(&get("class_eu")?, "class_eu", 0)?)?;
    put(&y, "area_count", count(&get("area_count")?, 1, 1, 65535)?)?;
    put(&y, "area_radius", count(&get("area_radius")?, 0, 0, 65535)?)?;
    put(
        &y,
        "area_ceiling",
        number(&get("area_ceiling")?, ALTITUDE_UNKNOWN, -1000.0, 31767.0)?,
    )?;
    put(
        &y,
        "area_floor",
        number(&get("area_floor")?, ALTITUDE_UNKNOWN, -1000.0, 31767.0)?,
    )?;
    put(&out, "system", y)?;
    Ok(out)
}

/// Seconds since the ODID epoch (2019-01-01 UTC), clamped to uint32. A naive
/// datetime is read as UTC.
#[pyfunction]
#[pyo3(signature = (now=None))]
pub fn system_timestamp(py: Python<'_>, now: Option<&Bound<'_, PyAny>>) -> PyResult<u32> {
    let seconds: f64 = match now {
        Some(moment) if moment.is_truthy()? => {
            let datetime = py.import("datetime")?;
            let utc = datetime.getattr("UTC")?;
            let moment = if moment.getattr("tzinfo")?.is_none() {
                let kwargs = PyDict::new(py);
                kwargs.set_item("tzinfo", &utc)?;
                moment.call_method("replace", (), Some(&kwargs))?
            } else {
                moment.clone()
            };
            let epoch = datetime.getattr("datetime")?.call1((2019, 1, 1, 0, 0, 0, 0, &utc))?;
            moment.sub(epoch)?.call_method0("total_seconds")?.extract()?
        }
        _ => {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
                - ODID_EPOCH_UNIX
        }
    };
    let whole = seconds.trunc();
    Ok(if whole <= 0.0 {
        0
    } else if whole >= u32::MAX as f64 {
        u32::MAX
    } else {
        whole as u32
    })
}

/// The four Remote ID messages, as `(message name, kwargs)` pairs the bridge
/// sends verbatim. All four go every cycle: both stacks time the last one out.
#[pyfunction]
#[pyo3(signature = (identity, *, target_system, target_component, now=None))]
pub fn messages<'py>(
    py: Python<'py>,
    identity: &Bound<'py, PyAny>,
    target_system: &Bound<'py, PyAny>,
    target_component: &Bound<'py, PyAny>,
    now: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyList>> {
    let resolved = settings(py, identity)?;
    let part = |key: &str| -> PyResult<Bound<'py, PyAny>> { resolved.as_any().get_item(key) };
    let (basic, operator, self_id, system) = (
        part("basic_id")?,
        part("operator_id")?,
        part("self_id")?,
        part("system")?,
    );
    let int = |d: &Bound<'py, PyAny>, k: &str| -> PyResult<i64> { d.get_item(k)?.extract() };
    let float = |d: &Bound<'py, PyAny>, k: &str| -> PyResult<f64> { d.get_item(k)?.extract() };
    let target_system = py.get_type::<pyo3::types::PyInt>().call1((target_system,))?;
    let target_component = py.get_type::<pyo3::types::PyInt>().call1((target_component,))?;

    let addressed = || -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("target_system", &target_system)?;
        d.set_item("target_component", &target_component)?;
        d.set_item("id_or_mac", PyBytes::new(py, &[0u8; 20]))?;
        Ok(d)
    };
    let bytes = |d: &Bound<'py, PyAny>, k: &str, n: usize| -> PyResult<Bound<'py, PyBytes>> {
        Ok(PyBytes::new(py, &fixed_bytes(&d.get_item(k)?, n)?))
    };

    let location = int(&system, "operator_location_type")?;
    let (latitude, longitude, altitude) = if location == LOCATION_FIXED {
        (
            (float(&system, "operator_latitude")? * 1e7).round_ties_even() as i64,
            (float(&system, "operator_longitude")? * 1e7).round_ties_even() as i64,
            float(&system, "operator_altitude_geo")?,
        )
    } else {
        (0, 0, ALTITUDE_UNKNOWN)
    };
    let classification = int(&system, "classification_type")?;
    let (category_eu, class_eu) = if classification == CLASSIFICATION_EU {
        (int(&system, "category_eu")?, int(&system, "class_eu")?)
    } else {
        (0, 0)
    };

    let basic_msg = addressed()?;
    basic_msg.set_item("id_type", int(&basic, "id_type")?)?;
    basic_msg.set_item("ua_type", int(&basic, "ua_type")?)?;
    basic_msg.set_item("uas_id", bytes(&basic, "uas_id", UAS_ID_MAX)?)?;

    let operator_msg = addressed()?;
    operator_msg.set_item("operator_id_type", int(&operator, "operator_id_type")?)?;
    operator_msg.set_item("operator_id", bytes(&operator, "operator_id", OPERATOR_ID_MAX)?)?;

    let self_msg = addressed()?;
    self_msg.set_item("description_type", int(&self_id, "description_type")?)?;
    self_msg.set_item("description", bytes(&self_id, "description", DESCRIPTION_MAX)?)?;

    let system_msg = addressed()?;
    system_msg.set_item("operator_location_type", location)?;
    system_msg.set_item("classification_type", classification)?;
    system_msg.set_item("operator_latitude", latitude)?;
    system_msg.set_item("operator_longitude", longitude)?;
    system_msg.set_item("area_count", int(&system, "area_count")?)?;
    system_msg.set_item("area_radius", int(&system, "area_radius")?)?;
    system_msg.set_item("area_ceiling", float(&system, "area_ceiling")?)?;
    system_msg.set_item("area_floor", float(&system, "area_floor")?)?;
    system_msg.set_item("category_eu", category_eu)?;
    system_msg.set_item("class_eu", class_eu)?;
    system_msg.set_item("operator_altitude_geo", altitude)?;
    system_msg.set_item("timestamp", system_timestamp(py, now)?)?;

    let pairs = [
        ("open_drone_id_basic_id", basic_msg),
        ("open_drone_id_operator_id", operator_msg),
        ("open_drone_id_self_id", self_msg),
        ("open_drone_id_system", system_msg),
    ];
    let out = PyList::empty(py);
    for (name, kwargs) in pairs {
        out.append(PyTuple::new(
            py,
            [PyString::new(py, name).into_any(), kwargs.into_any()],
        )?)?;
    }
    Ok(out)
}

/// An OPEN_DRONE_ID_ARM_STATUS as `{"ok", "status", "error"}`; a status that
/// is not a number counts as a failure.
#[pyfunction]
pub fn arm_status<'py>(
    py: Python<'py>,
    status: &Bound<'py, PyAny>,
    error: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyDict>> {
    let code = match int_of(status)? {
        Some(code) => code,
        None => obj(py, ARM_STATUS_FAIL).into_bound(py),
    };
    let out = PyDict::new(py);
    out.set_item("ok", code.eq(ARM_STATUS_GOOD)?)?;
    out.set_item("status", code)?;
    out.set_item("error", text(error, 50)?)?;
    Ok(out)
}

struct State {
    identity: PyObject,
    sent_at: f64,
    error: String,
    arm: Option<PyObject>,
}

/// The Remote ID broadcast of one bridge: the identity in force, when it last
/// went out, why it did not, and the aircraft's verdict.
#[pyclass(name = "RemoteIdEngine", module = "corvus_link.remote_id", frozen)]
pub struct RemoteIdEngine {
    env: PyObject,
    st: Mutex<State>,
}

fn identity_enabled(identity: &Bound<'_, PyAny>) -> PyResult<bool> {
    identity.call_method1("get", ("enabled",))?.is_truthy()
}

impl RemoteIdEngine {
    fn now(&self, py: Python<'_>) -> PyResult<f64> {
        monotonic_of(&self.env.bind(py).getattr("time")?)
    }
}

#[pymethods]
impl RemoteIdEngine {
    /// *env* is ``corvus.mavlink_remote_id``, whose ``time`` is the clock.
    #[new]
    fn new(py: Python<'_>, env: PyObject) -> PyResult<Self> {
        let identity = settings(py, &py.None().into_bound(py))?.into_any().unbind();
        Ok(RemoteIdEngine {
            env,
            st: Mutex::new(State {
                identity,
                sent_at: 0.0,
                error: String::new(),
                arm: None,
            }),
        })
    }

    #[getter]
    fn identity(&self, py: Python<'_>) -> PyObject {
        lock(&self.st).identity.clone_ref(py)
    }
    #[getter]
    fn sent_at(&self) -> f64 {
        lock(&self.st).sent_at
    }
    #[setter]
    fn set_sent_at(&self, value: f64) {
        lock(&self.st).sent_at = value;
    }
    #[getter]
    fn error(&self) -> String {
        lock(&self.st).error.clone()
    }
    #[setter]
    fn set_error(&self, value: String) {
        lock(&self.st).error = value;
    }
    #[getter]
    fn arm(&self, py: Python<'_>) -> Option<PyObject> {
        lock(&self.st).arm.as_ref().map(|a| a.clone_ref(py))
    }

    /// Install a new identity and return it resolved. The old identity's
    /// send history and verdict say nothing about the new one.
    fn set(&self, py: Python<'_>, raw: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        let resolved = settings(py, raw)?.into_any().unbind();
        let mut st = lock(&self.st);
        st.identity = resolved.clone_ref(py);
        st.sent_at = 0.0;
        st.error.clear();
        st.arm = None;
        Ok(resolved)
    }

    /// The identity in force, resolved afresh so the caller cannot reach
    /// into the stored one.
    fn settings<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let identity = lock(&self.st).identity.clone_ref(py);
        settings(py, identity.bind(py))
    }

    /// Drop what Remote ID learned from the link that just ended.
    fn forget(&self) {
        let mut st = lock(&self.st);
        st.sent_at = 0.0;
        st.error.clear();
        st.arm = None;
    }

    /// Whether the live link can carry the MAVLink 2 only ODID messages.
    fn supported(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let conn = bridge.getattr("_conn")?;
        let mav = match conn.getattr("mav") {
            Ok(mav) => mav,
            Err(err) if caught_by(bridge.py(), &err, &["AttributeError"]) => return Ok(false),
            Err(err) => return Err(err),
        };
        Ok(!mav.is_none() && mav.hasattr("open_drone_id_basic_id_send")?)
    }

    /// One round of the identity, from the heartbeat loop with the send lock
    /// held. Never raises an Exception: a throw there would turn the 1 Hz
    /// heartbeat into its 10 Hz reconnect spin, so the reason is kept for the
    /// page instead.
    fn send(&self, bridge: &Bound<'_, PyAny>, conn: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let identity = lock(&self.st).identity.clone_ref(py).into_bound(py);
        if !identity_enabled(&identity)? {
            return Ok(());
        }
        let mav = match conn.getattr("mav") {
            Ok(mav) => Some(mav),
            Err(err) if caught_by(py, &err, &["AttributeError"]) => None,
            Err(err) => return Err(err),
        };
        let mav = match mav {
            Some(mav) if !mav.is_none() && mav.hasattr("open_drone_id_basic_id_send")? => mav,
            _ => {
                lock(&self.st).error =
                    "this link is MAVLink 1, and the Remote ID messages are MAVLink 2 only".to_string();
                return Ok(());
            }
        };
        let sent = (|| -> PyResult<()> {
            let ts = bridge.getattr("_target_system")?;
            let tc = bridge.getattr("_target_component")?;
            for pair in messages(py, &identity, &ts, &tc, None)?.iter() {
                let name: String = pair.get_item(0)?.extract()?;
                let kwargs = pair.get_item(1)?.downcast_into::<PyDict>()?;
                mav.getattr(format!("{name}_send"))?.call((), Some(&kwargs))?;
            }
            Ok(())
        })();
        match sent {
            Ok(()) => {
                let now = self.now(py)?;
                let mut st = lock(&self.st);
                st.sent_at = now;
                st.error.clear();
            }
            Err(err) if err.is_instance_of::<PyException>(py) => {
                let value = err.value(py);
                let text = value.str()?.to_string();
                let text = if text.is_empty() {
                    value.get_type().name()?.to_string()
                } else {
                    text
                };
                lock(&self.st).error = text;
            }
            Err(err) => return Err(err),
        }
        Ok(())
    }

    /// Latch the aircraft's verdict on the identity it is being sent.
    fn handle_arm_status(&self, py: Python<'_>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let attr = |name: &str, fallback: PyObject| -> PyResult<Bound<'_, PyAny>> {
            match msg.getattr(name) {
                Ok(v) => Ok(v),
                Err(err) if caught_by(py, &err, &["AttributeError"]) => Ok(fallback.into_bound(py)),
                Err(err) => Err(err),
            }
        };
        let status = attr("status", py.None())?;
        let error = attr("error", PyBytes::new(py, b"").into_any().unbind())?;
        let verdict = arm_status(py, &status, &error)?.into_any().unbind();
        lock(&self.st).arm = Some(verdict);
        Ok(())
    }

    /// What the broadcast is doing right now. ``broadcasting`` means a send
    /// landed in the last 5 s, not that the switch is on.
    fn status<'py>(&self, bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
        let py = bridge.py();
        let (identity, sent_at, error, arm) = {
            let st = lock(&self.st);
            (
                st.identity.clone_ref(py),
                st.sent_at,
                st.error.clone(),
                st.arm.as_ref().map(|a| a.clone_ref(py)),
            )
        };
        let enabled = identity_enabled(identity.bind(py))?;
        let arm = match arm {
            Some(a) if a.bind(py).is_truthy()? => Some(py.get_type::<PyDict>().call1((a,))?),
            _ => None,
        };
        let age = if sent_at != 0.0 {
            Some(self.now(py)? - sent_at)
        } else {
            None
        };
        let out = PyDict::new(py);
        out.set_item("enabled", enabled)?;
        out.set_item("supported", bridge.call_method0("remote_id_supported")?.is_truthy()?)?;
        out.set_item("broadcasting", enabled && age.is_some_and(|a| a < 5.0))?;
        let rounded = match age {
            Some(a) => py.import("builtins")?.call_method1("round", (a, 1))?.unbind(),
            None => py.None(),
        };
        out.set_item("last_sent_age", rounded)?;
        out.set_item("error", error)?;
        out.set_item("arm_status", arm)?;
        Ok(out)
    }

    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.env)?;
        if let Ok(st) = self.st.try_lock() {
            visit.call(&st.identity)?;
            if let Some(arm) = &st.arm {
                visit.call(arm)?;
            }
        }
        Ok(())
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.remote_id")?;
    m.add_class::<RemoteIdEngine>()?;
    m.add_function(wrap_pyfunction!(settings, &m)?)?;
    m.add_function(wrap_pyfunction!(messages, &m)?)?;
    m.add_function(wrap_pyfunction!(arm_status, &m)?)?;
    m.add_function(wrap_pyfunction!(system_timestamp, &m)?)?;
    let enums = PyDict::new(py);
    for (name, range) in ENUMS.iter() {
        enums.set_item(*name, range.clone().collect::<Vec<i64>>())?;
    }
    m.add("ENUMS", enums)?;
    m.add("UAS_ID_MAX", UAS_ID_MAX)?;
    m.add("OPERATOR_ID_MAX", OPERATOR_ID_MAX)?;
    m.add("DESCRIPTION_MAX", DESCRIPTION_MAX)?;
    m.add("ALTITUDE_UNKNOWN", ALTITUDE_UNKNOWN)?;
    m.add("ARM_STATUS_GOOD", ARM_STATUS_GOOD)?;
    m.add("ARM_STATUS_FAIL", ARM_STATUS_FAIL)?;
    parent.add_submodule(&m)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("corvus_link.remote_id", &m)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_orders_like_python_min_max() {
        assert_eq!(clamp(95.0, -90.0, 90.0), 90.0);
        assert_eq!(clamp(-91.0, -90.0, 90.0), -90.0);
        assert!(clamp(-0.0, -90.0, 90.0).is_sign_negative());
        assert_eq!(clamp(12.5, -90.0, 90.0), 12.5);
    }

    #[test]
    fn python_whitespace_includes_the_separators() {
        for c in ['\u{1c}', '\u{1f}', ' ', '\t', '\u{2003}', '\u{85}'] {
            assert!(python_space(c), "{c:?}");
        }
        assert!(!python_space('a'));
        assert!(!python_space('\0'));
    }

    #[test]
    fn enum_ranges_match_the_odid_enums() {
        assert_eq!(values("id_type"), 0..=4);
        assert_eq!(values("class_eu"), 0..=7);
        assert_eq!(values("operator_id_type"), 0..=0);
    }
}
