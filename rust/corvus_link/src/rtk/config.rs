//! RTK settings: the defaults, the resolver that bounds every field of a
//! hand-edited config file, the redacted copy for HTTP, and the two checks
//! and the progress figure the page shows.

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyModule, PySlice, PyString};

use crate::protocol::host::py_float;
use crate::protocol::telemetry::py_round;

pub const DEFAULT_SURVEY_ACCURACY_M: f64 = 2.0;
pub const DEFAULT_SURVEY_DURATION_S: i64 = 180;
const SURVEY_ACCURACY_MIN_M: f64 = 0.1;
const SURVEY_ACCURACY_MAX_M: f64 = 100.0;
const SURVEY_DURATION_MIN_S: i64 = 10;
const SURVEY_DURATION_MAX_S: i64 = 86400;
const DEFAULT_NTRIP_PORT: i64 = 2101;
const SOURCES: [&str; 2] = ["usb", "ntrip"];
const MODES: [&str; 2] = ["survey", "fixed"];

/// The settings a station with no config file runs with.
#[pyfunction]
pub fn defaults(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("enabled", false)?;
    d.set_item("source", "usb")?;
    d.set_item("device", "")?;
    d.set_item("baud", 0)?;
    d.set_item("mode", "survey")?;
    d.set_item("survey_accuracy", DEFAULT_SURVEY_ACCURACY_M)?;
    d.set_item("survey_duration", DEFAULT_SURVEY_DURATION_S)?;
    d.set_item("msm7", false)?;
    let fixed = PyDict::new(py);
    fixed.set_item("latitude", 0.0)?;
    fixed.set_item("longitude", 0.0)?;
    fixed.set_item("altitude", 0.0)?;
    fixed.set_item("accuracy", 0.1)?;
    d.set_item("fixed", fixed)?;
    let ntrip = PyDict::new(py);
    ntrip.set_item("host", "")?;
    ntrip.set_item("port", DEFAULT_NTRIP_PORT)?;
    ntrip.set_item("mountpoint", "")?;
    ntrip.set_item("username", "")?;
    ntrip.set_item("password", "")?;
    d.set_item("ntrip", ntrip)?;
    Ok(d)
}

/// `float(raw)` bounded to [lo, hi]; the default for what is not a finite
/// number.
fn number(raw: &Bound<'_, PyAny>, default: f64, lo: f64, hi: f64) -> PyResult<f64> {
    let py = raw.py();
    let value = match py_float(raw) {
        Ok(v) => v,
        Err(err) if err.is_instance_of::<PyTypeError>(py) || err.is_instance_of::<PyValueError>(py) => return Ok(default),
        Err(err) => return Err(err),
    };
    if !value.is_finite() {
        return Ok(default);
    }
    let low = if value > lo { value } else { lo };
    Ok(if low < hi { low } else { hi })
}

/// `int(raw)` bounded to [lo, hi], for an int or a float only.
fn count(raw: &Bound<'_, PyAny>, default: i64, lo: i64, hi: i64) -> PyResult<i64> {
    if raw.is_instance_of::<PyBool>() {
        return Ok(default);
    }
    let value = if raw.is_instance_of::<PyInt>() {
        match raw.extract::<i64>() {
            Ok(v) => v,
            Err(_) => {
                if raw.gt(0)? {
                    hi
                } else {
                    lo
                }
            }
        }
    } else if raw.is_instance_of::<PyFloat>() {
        let f: f64 = raw.extract()?;
        if !f.is_finite() {
            return Ok(default);
        }
        let t = f.trunc();
        if t >= hi as f64 {
            hi
        } else if t <= lo as f64 {
            lo
        } else {
            t as i64
        }
    } else {
        return Ok(default);
    };
    Ok(value.max(lo).min(hi))
}

/// `raw.strip()[:limit]` for a str, else "".
fn text<'py>(raw: &Bound<'py, PyAny>, limit: isize) -> PyResult<Bound<'py, PyAny>> {
    let py = raw.py();
    if !raw.is_instance_of::<PyString>() {
        return Ok(PyString::new(py, "").into_any());
    }
    raw.call_method0("strip")?.get_item(PySlice::new(py, 0, limit, 1))
}

fn get<'py>(raw: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    raw.call_method1("get", (key,))
}

fn choice(value: &Bound<'_, PyAny>, allowed: &[&str]) -> PyResult<bool> {
    if !value.is_instance_of::<PyString>() {
        return Ok(false);
    }
    for a in allowed {
        if value.eq(*a)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Stored RTK settings resolved against the defaults, every field bounded.
#[pyfunction]
pub fn settings<'py>(raw: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = raw.py();
    let base = defaults(py)?;
    if !raw.is_instance_of::<PyDict>() {
        return Ok(base);
    }
    let out = base.copy()?;
    let enabled = get(raw, "enabled")?;
    if enabled.is_instance_of::<PyBool>() {
        out.set_item("enabled", get(raw, "enabled")?)?;
    }
    let source = get(raw, "source")?;
    if choice(&source, &SOURCES)? {
        out.set_item("source", source)?;
    }
    let mode = get(raw, "mode")?;
    if choice(&mode, &MODES)? {
        out.set_item("mode", mode)?;
    }
    out.set_item("device", text(&get(raw, "device")?, 256)?)?;
    out.set_item("baud", count(&get(raw, "baud")?, 0, 0, 4_000_000)?)?;
    out.set_item(
        "survey_accuracy",
        py_round(number(&get(raw, "survey_accuracy")?, DEFAULT_SURVEY_ACCURACY_M, SURVEY_ACCURACY_MIN_M, SURVEY_ACCURACY_MAX_M)?, 3),
    )?;
    out.set_item(
        "survey_duration",
        count(&get(raw, "survey_duration")?, DEFAULT_SURVEY_DURATION_S, SURVEY_DURATION_MIN_S, SURVEY_DURATION_MAX_S)?,
    )?;
    if get(raw, "msm7")?.is_instance_of::<PyBool>() {
        out.set_item("msm7", get(raw, "msm7")?)?;
    }
    let fixed = get(raw, "fixed")?;
    if fixed.is_instance_of::<PyDict>() {
        let f = PyDict::new(py);
        f.set_item("latitude", py_round(number(&get(&fixed, "latitude")?, 0.0, -90.0, 90.0)?, 9))?;
        f.set_item("longitude", py_round(number(&get(&fixed, "longitude")?, 0.0, -180.0, 180.0)?, 9))?;
        f.set_item("altitude", py_round(number(&get(&fixed, "altitude")?, 0.0, -1000.0, 20000.0)?, 4))?;
        f.set_item("accuracy", py_round(number(&get(&fixed, "accuracy")?, 0.1, 0.001, 100.0)?, 4))?;
        out.set_item("fixed", f)?;
    }
    let ntrip = get(raw, "ntrip")?;
    if ntrip.is_instance_of::<PyDict>() {
        let n = PyDict::new(py);
        n.set_item("host", text(&get(&ntrip, "host")?, 253)?)?;
        n.set_item("port", count(&get(&ntrip, "port")?, DEFAULT_NTRIP_PORT, 1, 65535)?)?;
        n.set_item("mountpoint", text(&get(&ntrip, "mountpoint")?, 100)?)?;
        n.set_item("username", text(&get(&ntrip, "username")?, 128)?)?;
        n.set_item("password", text(&get(&ntrip, "password")?, 128)?)?;
        out.set_item("ntrip", n)?;
    }
    Ok(out)
}

/// A copy of *settings* safe for an HTTP response: the NTRIP password
/// replaced by `has_password`.
#[pyfunction]
pub fn public_settings<'py>(settings: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = settings.py();
    let out = PyDict::new(py);
    let source = if settings.is_truthy()? { settings.clone() } else { PyDict::new(py).into_any() };
    for item in source.call_method0("items")?.try_iter()? {
        let (k, v): (Bound<'py, PyAny>, Bound<'py, PyAny>) = item?.extract()?;
        let v = if v.is_instance_of::<PyDict>() { py.get_type::<PyDict>().call1((v,))? } else { v };
        out.set_item(k, v)?;
    }
    if let Some(ntrip) = out.get_item("ntrip")? {
        if ntrip.is_instance_of::<PyDict>() {
            let has = ntrip.call_method1("get", ("password",))?.is_truthy()?;
            ntrip.set_item("has_password", has)?;
            ntrip.set_item("password", "")?;
        }
    }
    Ok(out)
}

fn float_or_zero(fixed: &Bound<'_, PyAny>, key: &str) -> PyResult<f64> {
    let v = get(fixed, key)?;
    if v.is_truthy()? { py_float(&v) } else { Ok(0.0) }
}

/// Why a fixed base position cannot be used, or "".
#[pyfunction]
pub fn fixed_position_problem(fixed: &Bound<'_, PyAny>) -> PyResult<&'static str> {
    let lat = float_or_zero(fixed, "latitude")?;
    let lon = float_or_zero(fixed, "longitude")?;
    if lat.abs() < 1e-9 && lon.abs() < 1e-9 {
        return Ok("the fixed base position is still 0, 0. Enter the surveyed mark first");
    }
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Ok("the fixed base position is outside the range of a coordinate");
    }
    Ok("")
}

fn blank(ntrip: &Bound<'_, PyAny>, key: &str) -> PyResult<bool> {
    let v = get(ntrip, key)?;
    let v = if v.is_truthy()? { v } else { PyString::new(ntrip.py(), "").into_any() };
    Ok(!v.str()?.call_method0("strip")?.is_truthy()?)
}

/// Why an NTRIP caster cannot be reached, or "".
#[pyfunction]
pub fn ntrip_problem(ntrip: &Bound<'_, PyAny>) -> PyResult<&'static str> {
    if blank(ntrip, "host")? {
        return Ok("no NTRIP caster address is set");
    }
    if blank(ntrip, "mountpoint")? {
        return Ok("no NTRIP mountpoint is set");
    }
    Ok("")
}

/// How far along a survey is, 0 to 100, held at 99 until the receiver says
/// valid.
#[pyfunction]
pub fn survey_progress(survey: &Bound<'_, PyAny>, target_duration: &Bound<'_, PyAny>, target_accuracy: &Bound<'_, PyAny>) -> PyResult<i64> {
    let py = survey.py();
    if !survey.is_truthy()? {
        return Ok(0);
    }
    if get(survey, "valid")?.is_truthy()? && !get(survey, "active")?.is_truthy()? {
        return Ok(100);
    }
    let int = py.get_type::<PyInt>();
    let raw = get(survey, "duration")?;
    let raw = if raw.is_truthy()? { raw } else { 0i64.into_pyobject(py)?.into_any() };
    let duration = int.call1((raw,))?;
    let duration = if duration.gt(0)? { duration } else { 0i64.into_pyobject(py)?.into_any() };
    let acc = get(survey, "accuracy")?;
    let accuracy = if acc.is_truthy()? { py_float(&acc)? } else { 0.0 };
    let target = int.call1((target_duration,))?;
    let one = 1i64.into_pyobject(py)?.into_any();
    let target = if target.gt(&one)? { target } else { one };
    let by_time = 100.0 * py_float(&duration)? / py_float(&target)?;
    let target_accuracy: Option<f64> = if accuracy > 0.0 { Some(target_accuracy.extract()?) } else { None };
    let by_accuracy = if let Some(target_accuracy) = target_accuracy.filter(|t| *t > 0.0) {
        let ratio = target_accuracy / accuracy;
        100.0 * if ratio < 1.0 { ratio } else { 1.0 }
    } else {
        0.0
    };
    let least = if by_accuracy < by_time { by_accuracy } else { by_time };
    if least.is_nan() {
        return Err(PyValueError::new_err("cannot convert float NaN to integer"));
    }
    if least.is_infinite() {
        return Err(pyo3::exceptions::PyOverflowError::new_err("cannot convert float infinity to integer"));
    }
    Ok((least.trunc() as i64).clamp(0, 99))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(defaults, m)?)?;
    m.add_function(wrap_pyfunction!(settings, m)?)?;
    m.add_function(wrap_pyfunction!(public_settings, m)?)?;
    m.add_function(wrap_pyfunction!(fixed_position_problem, m)?)?;
    m.add_function(wrap_pyfunction!(ntrip_problem, m)?)?;
    m.add_function(wrap_pyfunction!(survey_progress, m)?)?;
    Ok(())
}
