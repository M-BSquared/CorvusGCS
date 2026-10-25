//! What every protocol engine asks of the bridge it runs on: its logger,
//! command error, console, events, store and send lock, and Python's own
//! number conversions where a protocol must behave exactly as the Python did.

use pyo3::exceptions::{PyAttributeError, PyIndexError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyFloat, PyInt, PyString, PyTuple};

/// The `corvus.mavlink` logger every protocol writes to.
pub fn logger(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    py.import("logging")?.call_method1("getLogger", ("corvus.mavlink",))
}

/// `bool(obj.method())`.
pub fn truthy(obj: &Bound<'_, PyAny>, method: &str) -> PyResult<bool> {
    obj.call_method0(method)?.is_truthy()
}

/// `bridge.<name>.is_set()` for the bridge's threading events.
pub fn event_set(bridge: &Bound<'_, PyAny>, name: &str) -> PyResult<bool> {
    bridge.getattr(name)?.call_method0("is_set")?.is_truthy()
}

pub fn set_error(bridge: &Bound<'_, PyAny>, text: &str) -> PyResult<()> {
    bridge.call_method1("_set_command_error", (text,))?;
    Ok(())
}

pub fn console(bridge: &Bound<'_, PyAny>, name: &str, text: &str, level: &str) -> PyResult<()> {
    bridge.call_method1("_console_publish", (name, text, level))?;
    Ok(())
}

pub fn armed(bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
    bridge
        .getattr("_store")?
        .call_method0("get_snapshot")?
        .call_method1("get", ("armed",))?
        .is_truthy()
}

/// `time.monotonic()` of the given module (`time` itself, or a module whose
/// `time` attribute a test may replace).
pub fn monotonic_of(module: &Bound<'_, PyAny>) -> PyResult<f64> {
    module.call_method0("monotonic")?.extract()
}

/// `bridge._link_send(conn, method, *args)`: True when sent, False when the
/// link it was meant for is gone; a failed send raises.
pub fn link_send(
    bridge: &Bound<'_, PyAny>,
    conn: &Bound<'_, PyAny>,
    method: &str,
    args: Vec<PyObject>,
) -> PyResult<bool> {
    let py = bridge.py();
    let mut all: Vec<PyObject> = vec![conn.clone().unbind(), PyString::new(py, method).into_any().unbind()];
    all.extend(args);
    bridge.call_method1("_link_send", PyTuple::new(py, all)?)?.is_truthy()
}

/// Run `f` holding the bridge's send lock, released however `f` ends.
pub fn with_send_lock<T>(bridge: &Bound<'_, PyAny>, f: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    let lock = bridge.getattr("_send_lock")?;
    lock.call_method0("acquire")?;
    let result = f();
    lock.call_method0("release")?;
    result
}

pub fn py_int(value: &Bound<'_, PyAny>) -> PyResult<i64> {
    value.py().get_type::<PyInt>().call1((value,))?.extract()
}

pub fn py_float(value: &Bound<'_, PyAny>) -> PyResult<f64> {
    value.py().get_type::<PyFloat>().call1((value,))?.extract()
}

/// A plain Rust value as a Python object.
pub fn obj<'py, T: IntoPyObject<'py>>(py: Python<'py>, v: T) -> PyObject
where
    T::Error: Into<PyErr>,
{
    use pyo3::IntoPyObjectExt;
    v.into_py_any(py).expect("plain values convert")
}

/// Is `err` one of the exception types a Python `except` clause named?
pub fn caught_by(py: Python<'_>, err: &PyErr, kinds: &[&str]) -> bool {
    kinds.iter().any(|kind| match *kind {
        "AttributeError" => err.is_instance_of::<PyAttributeError>(py),
        "TypeError" => err.is_instance_of::<PyTypeError>(py),
        "ValueError" => err.is_instance_of::<PyValueError>(py),
        "IndexError" => err.is_instance_of::<PyIndexError>(py),
        _ => false,
    })
}
