//! The MAVLink parameter protocol: the cache, the download state machine,
//! PARAM_VALUE validation and decoding, the lossy-link watchdog, named reads,
//! confirmed writes, the verify pass and the batch upload.
//!
//! The engine owns the protocol's state and decisions. What belongs to the
//! bridge it runs on stays there and is asked for through the bridge object:
//! the link and its send lock (`_link_send`), whether it is ready, the armed
//! state, the command error, the console, the listeners, and the clock.
//! The clock is `time.monotonic` and `time.sleep` looked up at call time, so
//! a test that replaces them drives the engine exactly as it drove the
//! Python implementation.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PySet, PyString};

use crate::lock;
use crate::protocol::host::{armed, event_set, link_send, logger, obj, set_error, truthy};

pub use crate::plain::param_value::*;

/// `int(x)` as Python does it, for a value that may be any number type.
fn py_int(value: &Bound<'_, PyAny>) -> PyResult<i64> {
    let as_int = value.py().get_type::<PyInt>().call1((value,))?;
    Ok(as_int.extract::<i64>().unwrap_or(i64::MAX))
}

fn py_float(value: &Bound<'_, PyAny>) -> PyResult<f64> {
    value.py().get_type::<PyFloat>().call1((value,))?.extract()
}

/// Python's own text for a float, so messages read as they always have.
fn float_text(py: Python<'_>, value: f64) -> String {
    PyFloat::new(py, value)
        .str()
        .map(|s| s.to_string())
        .unwrap_or_else(|_| value.to_string())
}

#[pyfunction]
pub fn param_type_is_integer(param_type: &Bound<'_, PyAny>) -> bool {
    match py_int(param_type) {
        Ok(t) => is_integer_type(t),
        Err(_) => false,
    }
}

#[pyfunction]
#[pyo3(signature = (value, param_type, encoding, raw=None))]
pub fn decode_param_value(
    value: &Bound<'_, PyAny>,
    param_type: &Bound<'_, PyAny>,
    encoding: &str,
    raw: Option<&Bound<'_, PyAny>>,
) -> PyResult<f64> {
    let t = py_int(param_type)?;
    let v = py_float(value)?;
    let raw: Option<Vec<u8>> = match raw {
        Some(r) if !r.is_none() => Some(crate::wire::frame::bytes_arg(r)?),
        _ => None,
    };
    Ok(decode_value(v, t, encoding, raw.as_deref()))
}

#[pyfunction]
pub fn encode_param_value(value: &Bound<'_, PyAny>, param_type: &Bound<'_, PyAny>, encoding: &str) -> PyResult<f64> {
    let t = py_int(param_type)?;
    let v = py_float(value)?;
    encode_value(v, t, encoding).map_err(PyValueError::new_err)
}

#[pyfunction]
pub fn param_values_match(
    param_type: &Bound<'_, PyAny>,
    got: &Bound<'_, PyAny>,
    wanted: &Bound<'_, PyAny>,
) -> PyResult<bool> {
    let t = py_int(param_type).unwrap_or(-1);
    Ok(values_match(t, py_float(got)?, py_float(wanted)?))
}

/// The name as PARAM_SET/PARAM_REQUEST_READ carry it, or None: ASCII, one to
/// sixteen bytes. Rejected rather than truncated, because a shortened name
/// addresses a different parameter.
#[pyfunction]
pub fn encode_param_name<'py>(py: Python<'py>, name: &Bound<'py, PyAny>) -> Option<Bound<'py, PyBytes>> {
    encode_name(name).map(|b| PyBytes::new(py, &b))
}

fn encode_name(name: &Bound<'_, PyAny>) -> Option<Vec<u8>> {
    let s = name.downcast::<PyString>().ok()?.to_str().ok()?;
    if s.is_empty() || !s.is_ascii() || s.len() > PARAM_ID_MAX_BYTES {
        return None;
    }
    Some(s.as_bytes().to_vec())
}

// ---------------------------------------------------------------- entry ----

#[derive(Clone)]
struct Entry {
    name: String,
    value: f64,
    ptype: i64,
    index: i64,
    count: i64,
    seq: u64,
}

/// One cached parameter.
#[pyclass(name = "ParamEntry", module = "corvus_link.params", eq)]
#[derive(Clone, PartialEq)]
pub struct ParamEntry {
    #[pyo3(get, set)]
    name: String,
    #[pyo3(get, set)]
    value: f64,
    #[pyo3(get, set, name = "type")]
    ptype: i64,
    #[pyo3(get, set)]
    index: i64,
    #[pyo3(get, set)]
    count: i64,
    #[pyo3(get, set)]
    seq: u64,
}

#[pymethods]
impl ParamEntry {
    #[new]
    #[pyo3(signature = (name, value, r#type, index, count, seq=0))]
    fn new(name: String, value: f64, r#type: i64, index: i64, count: i64, seq: u64) -> Self {
        ParamEntry {
            name,
            value,
            ptype: r#type,
            index,
            count,
            seq,
        }
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        format!(
            "ParamEntry(name={}, value={}, type={}, index={}, count={}, seq={})",
            PyString::new(py, &self.name)
                .repr()
                .map(|r| r.to_string())
                .unwrap_or_default(),
            float_text(py, self.value),
            self.ptype,
            self.index,
            self.count,
            self.seq
        )
    }
}

impl From<&Entry> for ParamEntry {
    fn from(e: &Entry) -> Self {
        ParamEntry {
            name: e.name.clone(),
            value: e.value,
            ptype: e.ptype,
            index: e.index,
            count: e.count,
            seq: e.seq,
        }
    }
}

impl From<&ParamEntry> for Entry {
    fn from(e: &ParamEntry) -> Self {
        Entry {
            name: e.name.clone(),
            value: e.value,
            ptype: e.ptype,
            index: e.index,
            count: e.count,
            seq: e.seq,
        }
    }
}

// --------------------------------------------------------------- engine ----

/// A `set_param` waiting for its echo.
struct Pending {
    state: Mutex<(bool, Option<i64>)>,
    cv: Condvar,
}

impl Pending {
    fn new() -> Self {
        Pending {
            state: Mutex::new((false, None)),
            cv: Condvar::new(),
        }
    }

    fn set(&self, result: i64) {
        let mut s = lock(&self.state);
        *s = (true, Some(result));
        self.cv.notify_all();
    }

    fn wait(&self, timeout: f64) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
        let mut s = lock(&self.state);
        while !s.0 {
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            s = self
                .cv
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }

    fn result(&self) -> Option<i64> {
        lock(&self.state).1
    }

    fn clear(&self) {
        *lock(&self.state) = (false, None);
    }
}

#[derive(Clone)]
struct Failure {
    name: String,
    error: String,
}

#[derive(Clone)]
struct UploadResult {
    state: String,
    written: i64,
    failed: Vec<Failure>,
}

struct State {
    params: IndexMap<String, Entry>,
    count: i64,
    received: i64,
    state: String,
    seen: HashSet<i64>,
    rx_seq: u64,
    misses: HashMap<String, i64>,
    started_at: f64,
    retransmit_round: i64,
    last_value_at: f64,
    pending: HashMap<String, Arc<Pending>>,
    upload_total: i64,
    upload_done: i64,
    upload_result: Option<UploadResult>,
    state_before_upload: String,
}

/// The parameter protocol of one bridge.
#[pyclass(name = "ParamEngine", module = "corvus_link.params", frozen)]
pub struct ParamEngine {
    st: Mutex<State>,
    upload_cancel: AtomicBool,
}

fn monotonic(py: Python<'_>) -> PyResult<f64> {
    py.import("time")?.getattr("monotonic")?.call0()?.extract()
}

fn sleep(py: Python<'_>, seconds: f64) -> PyResult<()> {
    py.import("time")?.getattr("sleep")?.call1((seconds,))?;
    Ok(())
}

fn status_dict<'py>(py: Python<'py>, pairs: &[(&str, PyObject)]) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    for (k, v) in pairs {
        d.set_item(*k, v)?;
    }
    Ok(d)
}

impl ParamEngine {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.st)
    }

    fn progress(st: &State) -> (i64, i64) {
        if st.state == "uploading" {
            (st.upload_total, st.upload_done)
        } else {
            (st.count, st.received)
        }
    }

    fn notify(bridge: &Bound<'_, PyAny>, status: Bound<'_, PyDict>) -> PyResult<()> {
        bridge.call_method1("_notify_param_listeners", (status,))?;
        Ok(())
    }

    fn entry_obj(py: Python<'_>, e: &Entry) -> PyResult<PyObject> {
        Ok(Py::new(py, ParamEntry::from(e))?.into_any())
    }

    fn finish_incomplete(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let (count, received) = {
            let mut st = self.state();
            if st.state != "downloading" {
                return Ok(());
            }
            st.state = "incomplete".to_string();
            (st.count, st.received)
        };
        let status = status_dict(
            py,
            &[
                ("state", obj(py, "incomplete")),
                ("count", obj(py, count)),
                ("received", obj(py, received)),
            ],
        )?;
        Self::notify(bridge, status)
    }

    fn finish_upload_inner(&self, bridge: &Bound<'_, PyAny>, failed: Vec<Failure>, interrupted: bool) -> PyResult<()> {
        let py = bridge.py();
        let (outcome, total, done) = {
            let mut st = self.state();
            let outcome = if interrupted { "interrupted" } else { "upload_complete" };
            let before = st.state_before_upload.clone();
            st.state = if interrupted || (before != "complete" && before != "incomplete") {
                "idle".to_string()
            } else {
                before
            };
            st.upload_result = Some(UploadResult {
                state: outcome.to_string(),
                written: st.upload_done,
                failed,
            });
            (outcome, st.upload_total, st.upload_done)
        };
        let status = status_dict(
            py,
            &[
                ("state", obj(py, outcome)),
                ("count", obj(py, total)),
                ("received", obj(py, done)),
            ],
        )?;
        Self::notify(bridge, status)
    }

    /// `_param_value_wire_bytes`: the four bytes param_value arrived as.
    fn wire_bytes(msg: &Bound<'_, PyAny>) -> Option<Vec<u8>> {
        let get_buf = msg.getattr("get_msgbuf").ok()?;
        if !get_buf.is_callable() {
            return None;
        }
        let buf = get_buf.call0().ok()?;
        let buf: Vec<u8> = if buf.is_none() {
            Vec::new()
        } else {
            crate::wire::frame::bytes_arg(&buf).ok()?
        };
        let header = match buf.first()? {
            0xFD => 10,
            0xFE => 6,
            _ => return None,
        };
        if buf.len() < header + 4 {
            return None;
        }
        Some(buf[header..header + 4].to_vec())
    }
}

#[pymethods]
impl ParamEngine {
    #[new]
    fn new() -> Self {
        ParamEngine {
            st: Mutex::new(State {
                params: IndexMap::new(),
                count: -1,
                received: 0,
                state: "idle".to_string(),
                seen: HashSet::new(),
                rx_seq: 0,
                misses: HashMap::new(),
                started_at: 0.0,
                retransmit_round: 0,
                last_value_at: 0.0,
                pending: HashMap::new(),
                upload_total: 0,
                upload_done: 0,
                upload_result: None,
                state_before_upload: "idle".to_string(),
            }),
            upload_cancel: AtomicBool::new(false),
        }
    }

    // ---- the state, as the bridge's attributes used to hold it ----

    #[getter(state)]
    fn get_state(&self) -> String {
        self.state().state.clone()
    }
    #[setter(state)]
    fn set_state(&self, value: String) {
        self.state().state = value;
    }
    #[getter(count)]
    fn get_count(&self) -> i64 {
        self.state().count
    }
    #[setter(count)]
    fn set_count(&self, value: i64) {
        self.state().count = value;
    }
    #[getter(received)]
    fn get_received(&self) -> i64 {
        self.state().received
    }
    #[setter(received)]
    fn set_received(&self, value: i64) {
        self.state().received = value;
    }
    #[getter(rx_seq)]
    fn get_rx_seq(&self) -> u64 {
        self.state().rx_seq
    }
    #[setter(rx_seq)]
    fn set_rx_seq(&self, value: u64) {
        self.state().rx_seq = value;
    }
    #[getter(seen_indices)]
    fn get_seen<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PySet>> {
        let seen: Vec<i64> = self.state().seen.iter().copied().collect();
        PySet::new(py, seen)
    }
    #[setter(seen_indices)]
    fn set_seen(&self, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let mut seen = HashSet::new();
        for item in value.try_iter()? {
            seen.insert(item?.extract::<i64>()?);
        }
        self.state().seen = seen;
        Ok(())
    }
    #[getter(misses)]
    fn get_misses<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        for (k, v) in self.state().misses.iter() {
            d.set_item(k, v)?;
        }
        Ok(d)
    }
    #[setter(misses)]
    fn set_misses(&self, value: &Bound<'_, PyDict>) -> PyResult<()> {
        let mut misses = HashMap::new();
        for (k, v) in value.iter() {
            misses.insert(k.extract::<String>()?, v.extract::<i64>()?);
        }
        self.state().misses = misses;
        Ok(())
    }
    #[getter(download_started_at)]
    fn get_started_at(&self) -> f64 {
        self.state().started_at
    }
    #[setter(download_started_at)]
    fn set_started_at(&self, value: f64) {
        self.state().started_at = value;
    }
    #[getter(retransmit_round)]
    fn get_round(&self) -> i64 {
        self.state().retransmit_round
    }
    #[setter(retransmit_round)]
    fn set_round(&self, value: i64) {
        self.state().retransmit_round = value;
    }
    #[getter(last_value_at)]
    fn get_last_value_at(&self) -> f64 {
        self.state().last_value_at
    }
    #[setter(last_value_at)]
    fn set_last_value_at(&self, value: f64) {
        self.state().last_value_at = value;
    }
    #[getter(upload_total)]
    fn get_upload_total(&self) -> i64 {
        self.state().upload_total
    }
    #[setter(upload_total)]
    fn set_upload_total(&self, value: i64) {
        self.state().upload_total = value;
    }
    #[getter(upload_done)]
    fn get_upload_done(&self) -> i64 {
        self.state().upload_done
    }
    #[setter(upload_done)]
    fn set_upload_done(&self, value: i64) {
        self.state().upload_done = value;
    }
    #[getter(state_before_upload)]
    fn get_before(&self) -> String {
        self.state().state_before_upload.clone()
    }
    #[setter(state_before_upload)]
    fn set_before(&self, value: String) {
        self.state().state_before_upload = value;
    }
    #[getter(upload_cancelled)]
    fn get_cancelled(&self) -> bool {
        self.upload_cancel.load(Ordering::SeqCst)
    }

    /// The cache as a mapping of name to ParamEntry, live.
    #[getter(cache)]
    fn cache(slf: &Bound<'_, Self>) -> PyResult<Py<ParamCache>> {
        Py::new(
            slf.py(),
            ParamCache {
                engine: slf.clone().unbind(),
            },
        )
    }

    // ---- lifecycle ----

    /// Wake every writer waiting for an echo, stop an upload and drop the
    /// claim that this vehicle's parameters are complete.
    fn abort(&self) {
        self.upload_cancel.store(true, Ordering::SeqCst);
        let mut st = self.state();
        st.state = "idle".to_string();
        for pending in st.pending.values() {
            pending.set(-2);
        }
        st.pending.clear();
    }

    /// Forget the vehicle's parameters for a new link cycle.
    fn reset(&self) {
        self.abort();
        let mut st = self.state();
        st.params.clear();
        st.count = -1;
        st.received = 0;
        st.state = "idle".to_string();
        st.seen.clear();
        st.started_at = 0.0;
        st.retransmit_round = 0;
        st.last_value_at = 0.0;
        st.misses.clear();
    }

    // ---- reads of the cache ----

    fn get_params<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let mut entries: Vec<Entry> = self.state().params.values().cloned().collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let out = PyList::empty(py);
        for e in entries {
            out.append(status_dict(
                py,
                &[
                    ("name", obj(py, e.name.as_str())),
                    ("value", obj(py, e.value)),
                    ("type", obj(py, e.ptype)),
                ],
            )?)?;
        }
        Ok(out)
    }

    fn get_param<'py>(&self, py: Python<'py>, name: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyDict>>> {
        let Ok(name) = name.extract::<String>() else {
            return Ok(None);
        };
        let entry = self.state().params.get(&name).cloned();
        match entry {
            None => Ok(None),
            Some(e) => Ok(Some(status_dict(
                py,
                &[
                    ("name", obj(py, e.name.as_str())),
                    ("value", obj(py, e.value)),
                    ("type", obj(py, e.ptype)),
                ],
            )?)),
        }
    }

    fn entry(&self, py: Python<'_>, name: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        let Ok(name) = name.extract::<String>() else {
            return Ok(py.None());
        };
        let entry = self.state().params.get(&name).cloned();
        match entry {
            Some(e) => Self::entry_obj(py, &e),
            None => Ok(py.None()),
        }
    }

    fn status<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let (state, count, received) = {
            let st = self.state();
            let (c, r) = Self::progress(&st);
            (st.state.clone(), c, r)
        };
        status_dict(
            py,
            &[
                ("state", obj(py, state.as_str())),
                ("count", obj(py, count)),
                ("received", obj(py, received)),
            ],
        )
    }

    fn progress_tuple(&self) -> (i64, i64) {
        Self::progress(&self.state())
    }

    fn upload_result<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let result = self.state().upload_result.clone();
        let Some(r) = result else {
            return status_dict(
                py,
                &[
                    ("state", obj(py, "idle")),
                    ("written", obj(py, 0)),
                    ("failed", obj(py, 0)),
                    ("errors", PyList::empty(py).into_any().unbind()),
                ],
            );
        };
        let errors = PyList::empty(py);
        for f in &r.failed {
            errors.append(status_dict(
                py,
                &[("name", obj(py, f.name.as_str())), ("error", obj(py, f.error.as_str()))],
            )?)?;
        }
        status_dict(
            py,
            &[
                ("state", obj(py, r.state.as_str())),
                ("written", obj(py, r.written)),
                ("failed", obj(py, r.failed.len())),
                ("errors", errors.into_any().unbind()),
            ],
        )
    }

    /// `_param_matches`: the read back value against the wanted one, at the
    /// parameter's own precision.
    fn matches(&self, name: &str, got: f64, wanted: f64) -> bool {
        let ptype = self
            .state()
            .params
            .get(name)
            .map(|e| e.ptype)
            .unwrap_or(MAV_PARAM_TYPE_REAL32);
        values_match(ptype, got, wanted)
    }

    #[staticmethod]
    fn echo_confirms(echoed: &ParamEntry, value: f64) -> bool {
        echo_confirms(echoed.ptype, echoed.value, value)
    }

    // ---- receive ----

    /// Cache a received PARAM_VALUE, wake a writer waiting for it, and tell
    /// the listeners.
    fn handle_param_value(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let decoded = (|| -> PyResult<(Bound<'_, PyAny>, String, i64, f64, i64, i64)> {
            let raw_id = msg.getattr("param_id")?;
            let name_obj = if raw_id.is_instance_of::<PyBytes>() {
                let head = raw_id
                    .call_method1("split", (PyBytes::new(py, b"\x00"), 1))?
                    .get_item(0)?;
                head.call_method1("decode", ("ascii",))?
            } else {
                raw_id.str()?.call_method1("split", ("\x00", 1))?.get_item(0)?
            };
            let pname: String = name_obj.extract()?;
            let ptype = py_int(&msg.getattr("param_type")?)?;
            let value = py_float(&msg.getattr("param_value")?)?;
            let encoding: String = bridge.call_method0("_param_encoding")?.extract()?;
            let pval = decode_value(value, ptype, &encoding, Self::wire_bytes(msg).as_deref());
            let mut pindex = py_int(&msg.getattr("param_index")?)?;
            let pcount = py_int(&msg.getattr("param_count")?)?;
            if pindex == PARAM_INDEX_NONE {
                pindex = -1;
            }
            Ok((name_obj, pname, ptype, pval, pindex, pcount))
        })();
        let (name_obj, pname, ptype, pval, pindex, pcount) = match decoded {
            Ok(v) => v,
            Err(err) => {
                let caught = [
                    py.get_type::<pyo3::exceptions::PyAttributeError>(),
                    py.get_type::<pyo3::exceptions::PyTypeError>(),
                    py.get_type::<pyo3::exceptions::PyValueError>(),
                    py.get_type::<pyo3::exceptions::PyUnicodeError>(),
                ];
                let struct_error = py.import("struct")?.getattr("error")?;
                if caught.iter().any(|t| err.is_instance(py, t)) || err.is_instance(py, &struct_error) {
                    logger(py)?.call_method1("debug", ("discarding malformed PARAM_VALUE",))?;
                    return Ok(());
                }
                return Err(err);
            }
        };
        if pname == PARAM_HASH_CHECK {
            return Ok(());
        }
        if encode_name(&name_obj).is_none()
            || !pval.is_finite()
            || !(1..=10).contains(&ptype)
            || !(-1..PARAM_CACHE_MAX_ENTRIES).contains(&pindex)
            || !(0..=PARAM_CACHE_MAX_ENTRIES).contains(&pcount)
            || (pcount > 0 && pindex >= pcount)
        {
            logger(py)?.call_method1(
                "warning",
                (
                    "discarding invalid PARAM_VALUE id=%r index=%s count=%s type=%s",
                    name_obj,
                    pindex,
                    pcount,
                    ptype,
                ),
            )?;
            return Ok(());
        }
        let now = monotonic(py)?;
        let status = {
            let mut st = self.state();
            let is_new = !st.params.contains_key(&pname);
            st.misses.remove(&pname);
            if is_new && st.params.len() as i64 >= PARAM_CACHE_MAX_ENTRIES {
                drop(st);
                logger(py)?.call_method1("warning", ("parameter cache limit reached; dropping %s", pname))?;
                return Ok(());
            }
            if is_new {
                st.received += 1;
            }
            if pindex >= 0 {
                st.seen.insert(pindex);
            }
            st.last_value_at = now;
            st.rx_seq += 1;
            let seq = st.rx_seq;
            st.params.insert(
                pname.clone(),
                Entry {
                    name: pname.clone(),
                    value: pval,
                    ptype,
                    index: pindex,
                    count: pcount,
                    seq,
                },
            );
            if pcount > 0 {
                st.count = st.count.max(pcount);
            }
            if st.state != "uploading" && st.count > 0 && st.received >= st.count {
                st.state = "complete".to_string();
            }
            let (count, received) = Self::progress(&st);
            if let Some(pending) = st.pending.get(&pname) {
                pending.set(0);
            }
            (st.state.clone(), count, received)
        };
        let d = status_dict(
            py,
            &[
                ("state", obj(py, status.0.as_str())),
                ("count", obj(py, status.1)),
                ("received", obj(py, status.2)),
                ("name", obj(py, pname.as_str())),
                ("value", obj(py, pval)),
                ("type", obj(py, ptype)),
            ],
        )?;
        Self::notify(bridge, d)
    }

    // ---- download ----

    /// PARAM_REQUEST_LIST, with the state machine around it. The caller
    /// holds the bridge's operation lock.
    fn request_param_list(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        if !truthy(bridge, "_connection_ready")? {
            set_error(bridge, "not connected")?;
            return Ok(false);
        }
        let now = monotonic(py)?;
        {
            let mut st = self.state();
            if st.state == "downloading" || st.state == "uploading" {
                drop(st);
                set_error(bridge, "another parameter operation in progress")?;
                return Ok(false);
            }
            st.state = "downloading".to_string();
            st.params.clear();
            st.received = 0;
            st.count = -1;
            st.seen = HashSet::new();
            st.started_at = now;
            st.last_value_at = now;
            st.retransmit_round = 0;
        }
        let conn = bridge.getattr("_conn")?;
        let sent = match link_send(bridge, &conn, "param_request_list_send", vec![]) {
            Ok(true) => Ok(()),
            Ok(false) => Err("link closed".to_string()),
            Err(err) => Err(err.value(py).str()?.to_string()),
        };
        if let Err(text) = sent {
            {
                let mut st = self.state();
                if st.state == "downloading" {
                    st.state = "idle".to_string();
                }
            }
            set_error(bridge, &format!("parameter list request failed: {text}"))?;
            return Ok(false);
        }
        bridge.call_method0("_start_param_watchdog")?;
        Ok(true)
    }

    /// PARAM_REQUEST_READ by name. The caller holds the operation lock.
    fn request_param(&self, bridge: &Bound<'_, PyAny>, name: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        if !truthy(bridge, "_connection_ready")? {
            return Ok(false);
        }
        let Some(encoded) = encode_name(name) else {
            set_error(bridge, "invalid parameter name")?;
            return Ok(false);
        };
        let conn = bridge.getattr("_conn")?;
        match link_send(
            bridge,
            &conn,
            "param_request_read_send",
            vec![PyBytes::new(py, &encoded).into_any().unbind(), obj(py, -1)],
        ) {
            Ok(sent) => Ok(sent),
            Err(err) => {
                logger(py)?.call_method1("debug", ("param read request failed (%s): %s", name, err.value(py)))?;
                Ok(false)
            }
        }
    }

    /// Re-request what a lossy link lost, then give up to "incomplete".
    fn run_watchdog(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        while event_set(bridge, "_running")? && !event_set(bridge, "_stop_event")? {
            bridge.call_method1("_interruptible_sleep", (PARAM_WATCHDOG_TICK_S,))?;
            let (state, count, received, seen, started_at, last_value, round) = {
                let st = self.state();
                (
                    st.state.clone(),
                    st.count,
                    st.received,
                    st.seen.clone(),
                    st.started_at,
                    st.last_value_at,
                    st.retransmit_round,
                )
            };
            if state != "downloading" {
                return Ok(());
            }
            if count <= 0 {
                continue;
            }
            if received >= count {
                return Ok(());
            }
            let now = monotonic(py)?;
            let budget: f64 = bridge.call_method0("_param_download_budget_s")?.extract()?;
            if now - started_at > budget {
                return self.finish_incomplete(bridge);
            }
            if now - last_value < PARAM_DOWNLOAD_INACTIVITY_S {
                continue;
            }
            if round >= PARAM_DOWNLOAD_MAX_ROUNDS {
                return self.finish_incomplete(bridge);
            }
            let mut missing: Vec<i64> = (0..count).filter(|i| !seen.contains(i)).collect();
            if missing.is_empty() {
                return Ok(());
            }
            self.state().retransmit_round = round + 1;
            let conn = bridge.getattr("_conn")?;
            if conn.is_none() || !conn.is(&bridge.getattr("_conn")?) {
                return Ok(());
            }
            let (per_round, gap) = if truthy(bridge, "_is_slow_link")? {
                (PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL, PARAM_RETRANSMIT_GAP_S_SERIAL)
            } else {
                (PARAM_RETRANSMIT_MAX_PER_ROUND_UDP, PARAM_RETRANSMIT_GAP_S_UDP)
            };
            missing.truncate(per_round);
            for idx in missing {
                if event_set(bridge, "_stop_event")? {
                    return Ok(());
                }
                match link_send(
                    bridge,
                    &conn,
                    "param_request_read_send",
                    vec![PyBytes::new(py, b"").into_any().unbind(), obj(py, idx)],
                ) {
                    Ok(true) => {}
                    Ok(false) => return Ok(()),
                    Err(err) => {
                        logger(py)?
                            .call_method1("debug", ("param retransmit idx=%d failed: %s", idx, err.value(py)))?;
                    }
                }
                bridge.call_method1("_interruptible_sleep", (gap,))?;
            }
        }
        Ok(())
    }

    fn finish_download_incomplete(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        self.finish_incomplete(bridge)
    }

    // ---- named reads ----

    /// Poll the cache until `name` appears or `timeout` passes.
    #[pyo3(signature = (name, timeout=2.0))]
    fn wait_for_param(&self, py: Python<'_>, name: &Bound<'_, PyAny>, timeout: f64) -> PyResult<PyObject> {
        let key: Option<String> = name.extract().ok();
        let deadline = monotonic(py)? + timeout;
        while monotonic(py)? < deadline {
            let entry = key.as_ref().and_then(|k| self.state().params.get(k).cloned());
            if let Some(e) = entry {
                return Self::entry_obj(py, &e);
            }
            sleep(py, 0.02)?;
        }
        Ok(py.None())
    }

    /// `fetch_params`: a named set without a full download, ended by a
    /// barrier read once the vehicle has visibly worked through the burst.
    #[pyo3(signature = (bridge, names, timeout=4.0, fresh=false))]
    fn fetch_params<'py>(
        &self,
        bridge: &Bound<'py, PyAny>,
        names: &Bound<'py, PyAny>,
        timeout: f64,
        fresh: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        let py = bridge.py();
        let mut wanted: Vec<String> = Vec::new();
        for item in names.try_iter()? {
            let item = item?;
            if let Ok(s) = item.downcast::<PyString>() {
                let s = s.to_str()?.to_string();
                if !s.is_empty() {
                    wanted.push(s);
                }
            }
        }
        if wanted.is_empty() {
            return Ok(PyDict::new(py));
        }
        if !truthy(bridge, "_connection_ready")? {
            set_error(bridge, "not connected")?;
            return Ok(PyDict::new(py));
        }
        let (since, known_absent) = {
            let st = self.state();
            let since = if fresh { Some(st.rx_seq) } else { None };
            let complete = st.state == "complete";
            let absent: HashSet<String> = wanted
                .iter()
                .filter(|n| {
                    !st.params.contains_key(*n)
                        && (complete || st.misses.get(*n).copied().unwrap_or(0) >= PARAM_ABSENT_AFTER_MISSES)
                })
                .cloned()
                .collect();
            (since, absent)
        };
        let snapshot = || -> IndexMap<String, f64> {
            let st = self.state();
            let mut out = IndexMap::new();
            for n in &wanted {
                if let Some(e) = st.params.get(n) {
                    if since.is_none_or(|s| e.seq > s) && !out.contains_key(n) {
                        out.insert(n.clone(), e.value);
                    }
                }
            }
            out
        };
        let still_missing = |have: &IndexMap<String, f64>| -> Vec<String> {
            wanted
                .iter()
                .filter(|n| !have.contains_key(*n) && !known_absent.contains(*n))
                .cloned()
                .collect()
        };
        let to_dict = |have: &IndexMap<String, f64>| -> PyResult<Bound<'py, PyDict>> {
            let d = PyDict::new(py);
            for (k, v) in have {
                d.set_item(k, v)?;
            }
            Ok(d)
        };
        let barrier_name =
            |batch: &HashSet<String>, batch_sent_after: u64, have: &IndexMap<String, f64>| -> Option<String> {
                let st = self.state();
                if st.state == "downloading" || st.state == "uploading" {
                    return None;
                }
                let usable = |name: &str| match st.params.get(name) {
                    Some(e) => !batch.contains(name) || e.seq > batch_sent_after,
                    None => false,
                };
                have.keys()
                    .find(|n| usable(n))
                    .cloned()
                    .or_else(|| st.params.keys().find(|n| usable(n)).cloned())
            };
        let barrier_answered =
            |name: &str, sent_after: u64| -> bool { self.state().params.get(name).is_some_and(|e| e.seq > sent_after) };

        let deadline = monotonic(py)? + f64::max(0.5, timeout);
        let mut have = snapshot();
        let mut missing = still_missing(&have);
        for round_index in 0..2 {
            if missing.is_empty() {
                break;
            }
            let batch: HashSet<String> = missing.iter().cloned().collect();
            let batch_sent_after = self.state().rx_seq;
            for name in missing.clone() {
                if event_set(bridge, "_stop_event")? || !truthy(bridge, "_connection_ready")? {
                    return to_dict(&snapshot());
                }
                bridge.call_method1("request_param", (name,))?;
            }
            let round_deadline = if round_index > 0 {
                deadline
            } else {
                let now = monotonic(py)?;
                now + f64::max(0.25, (deadline - monotonic(py)?) / 2.0)
            };
            let mut barrier: Option<(String, u64)> = None;
            let mut barrier_sent = false;
            while monotonic(py)? < f64::min(round_deadline, deadline) {
                have = snapshot();
                missing = still_missing(&have);
                if missing.is_empty() {
                    break;
                }
                if event_set(bridge, "_stop_event")? {
                    return to_dict(&have);
                }
                if let Some((name, after)) = &barrier {
                    if barrier_answered(name, *after) {
                        break;
                    }
                }
                if !barrier_sent {
                    if let Some(name) = barrier_name(&batch, batch_sent_after, &have) {
                        barrier_sent = true;
                        let sent_after = self.state().rx_seq;
                        if bridge.call_method1("request_param", (name.as_str(),))?.is_truthy()? {
                            barrier = Some((name, sent_after));
                        }
                        continue;
                    }
                }
                sleep(py, 0.02)?;
            }
            have = snapshot();
            missing = still_missing(&have);
        }
        if !missing.is_empty() && wanted.iter().any(|n| have.contains_key(n)) {
            let mut st = self.state();
            for name in &missing {
                *st.misses.entry(name.clone()).or_insert(0) += 1;
            }
        }
        to_dict(&have)
    }

    // ---- writes ----

    /// Write one parameter and confirm it from the echoed PARAM_VALUE. The
    /// caller holds the operation lock.
    fn set_param(
        &self,
        bridge: &Bound<'_, PyAny>,
        name: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let Some(encoded) = encode_name(name) else {
            set_error(bridge, "invalid parameter name")?;
            return Ok(false);
        };
        let name_str: String = name.extract()?;
        if value.is_instance_of::<PyBool>() || !(value.is_instance_of::<PyInt>() || value.is_instance_of::<PyFloat>()) {
            set_error(bridge, "parameter value must be a number")?;
            return Ok(false);
        }
        let value = py_float(value)?;
        if !value.is_finite() {
            set_error(bridge, "parameter value must be finite")?;
            return Ok(false);
        }
        if !truthy(bridge, "_connection_ready")? {
            set_error(bridge, "not connected")?;
            return Ok(false);
        }
        if armed(bridge)? {
            set_error(bridge, "cannot set parameter while armed")?;
            return Ok(false);
        }
        let mut ptype = self.state().params.get(&name_str).map(|e| e.ptype);
        if ptype.is_none() {
            bridge.call_method1("request_param", (name_str.as_str(),))?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("timeout", 2.0)?;
            let found = bridge.call_method("_wait_for_param", (name_str.as_str(),), Some(&kwargs))?;
            if found.is_none() {
                set_error(bridge, "parameter not found on vehicle")?;
                return Ok(false);
            }
            ptype = Some(found.getattr("type")?.extract()?);
        }
        let ptype = ptype.unwrap();
        let encoding: String = bridge.call_method0("_param_encoding")?.extract()?;
        let wire_value = match encode_value(value, ptype, &encoding) {
            Ok(v) => v,
            Err(text) => {
                set_error(bridge, &format!("{name_str}: {text}"))?;
                return Ok(false);
            }
        };
        let pending = Arc::new(Pending::new());
        self.state().pending.insert(name_str.clone(), Arc::clone(&pending));
        let outcome = (|| -> PyResult<Result<(Option<Entry>, bool), ()>> {
            let mut echoed: Option<Entry> = None;
            let mut confirmed = false;
            for _attempt in 0..3 {
                if !truthy(bridge, "_connection_ready")? {
                    set_error(bridge, "not connected")?;
                    return Ok(Err(()));
                }
                let conn = bridge.getattr("_conn")?;
                let args = vec![
                    PyBytes::new(py, &encoded).into_any().unbind(),
                    obj(py, wire_value),
                    obj(py, ptype),
                ];
                match link_send(bridge, &conn, "param_set_send", args) {
                    Ok(true) => {}
                    Ok(false) => {
                        set_error(bridge, "not connected")?;
                        return Ok(Err(()));
                    }
                    Err(err) => {
                        set_error(bridge, &format!("parameter write failed: {}", err.value(py).str()?))?;
                        return Ok(Err(()));
                    }
                }
                let mut answered = false;
                let deadline = monotonic(py)? + 1.0;
                loop {
                    let remaining = deadline - monotonic(py)?;
                    if remaining <= 0.0 {
                        break;
                    }
                    let p = Arc::clone(&pending);
                    if !py.allow_threads(move || p.wait(remaining)) {
                        break;
                    }
                    if pending.result() == Some(-2) {
                        set_error(bridge, "not connected")?;
                        return Ok(Err(()));
                    }
                    answered = true;
                    let st = self.state();
                    echoed = st.params.get(&name_str).cloned();
                    if let Some(e) = &echoed {
                        if echo_confirms(e.ptype, e.value, value) {
                            confirmed = true;
                            break;
                        }
                    }
                    pending.clear();
                }
                if confirmed || answered {
                    break;
                }
            }
            Ok(Ok((echoed, confirmed)))
        })();
        {
            let mut st = self.state();
            if st.pending.get(&name_str).is_some_and(|p| Arc::ptr_eq(p, &pending)) {
                st.pending.remove(&name_str);
            }
        }
        let (echoed, confirmed) = match outcome? {
            Ok(v) => v,
            Err(()) => return Ok(false),
        };
        let Some(echoed) = echoed else {
            set_error(bridge, "parameter write not confirmed (timeout)")?;
            return Ok(false);
        };
        if confirmed {
            let text = format!("{name_str} set to {}", float_text(py, value));
            bridge.call_method1("_console_publish", ("PARAM", text, "success"))?;
            return Ok(true);
        }
        set_error(
            bridge,
            &format!(
                "parameter write not confirmed (got {} expected {})",
                float_text(py, echoed.value),
                float_text(py, value)
            ),
        )?;
        Ok(false)
    }

    /// `verify_params`: read every name fresh, write a differing target again
    /// (never while armed) and report what the vehicle holds now.
    #[pyo3(signature = (bridge, targets, names=None, rewrite=true, timeout=4.0))]
    fn verify_params<'py>(
        &self,
        bridge: &Bound<'py, PyAny>,
        targets: &Bound<'py, PyAny>,
        names: Option<&Bound<'py, PyAny>>,
        rewrite: bool,
        timeout: f64,
    ) -> PyResult<PyObject> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let names = names.filter(|n| !n.is_none());
        if !targets.is_instance_of::<PyList>() || names.is_some_and(|n| !n.is_instance_of::<PyList>()) {
            set_error(bridge, "invalid parameter list")?;
            return Ok(py.None());
        }
        let mut wanted: IndexMap<String, f64> = IndexMap::new();
        for entry in targets.try_iter()? {
            let entry = entry?;
            let Ok(entry) = entry.downcast::<PyDict>() else {
                set_error(bridge, "invalid parameter list")?;
                return Ok(py.None());
            };
            let name = entry.get_item("name")?.unwrap_or_else(|| py.None().into_bound(py));
            let value = entry.get_item("value")?.unwrap_or_else(|| py.None().into_bound(py));
            if encode_name(&name).is_none() {
                set_error(bridge, "invalid parameter name")?;
                return Ok(py.None());
            }
            let name: String = name.extract()?;
            let numeric = !value.is_instance_of::<PyBool>()
                && (value.is_instance_of::<PyInt>() || value.is_instance_of::<PyFloat>());
            if !numeric || !py_float(&value)?.is_finite() {
                set_error(bridge, &format!("{name}: value must be a finite number"))?;
                return Ok(py.None());
            }
            wanted.insert(name, py_float(&value)?);
        }
        let mut read_names: Vec<String> = wanted.keys().cloned().collect();
        if let Some(names) = names {
            for name in names.try_iter()? {
                let name = name?;
                if encode_name(&name).is_none() {
                    set_error(bridge, "invalid parameter name")?;
                    return Ok(py.None());
                }
                let name: String = name.extract()?;
                if !read_names.contains(&name) {
                    read_names.push(name);
                }
            }
        }
        if read_names.is_empty() {
            set_error(bridge, "nothing to check")?;
            return Ok(py.None());
        }
        if read_names.len() > PARAM_VERIFY_MAX_NAMES {
            set_error(
                bridge,
                &format!(
                    "too many parameters ({}); the maximum is {PARAM_VERIFY_MAX_NAMES}",
                    read_names.len()
                ),
            )?;
            return Ok(py.None());
        }
        if !truthy(bridge, "_connection_ready")? {
            set_error(bridge, "not connected")?;
            return Ok(py.None());
        }
        let fetch = |names: Vec<String>, timeout: f64| -> PyResult<Bound<'py, PyAny>> {
            let kwargs = PyDict::new(py);
            kwargs.set_item("timeout", timeout)?;
            kwargs.set_item("fresh", true)?;
            bridge.call_method("fetch_params", (PyList::new(py, names)?,), Some(&kwargs))
        };
        let values = fetch(read_names.clone(), timeout)?;
        let results = PyList::empty(py);
        let mut confirmed_count = 0usize;
        for (name, value) in &wanted {
            let before = values.call_method1("get", (name.as_str(),))?;
            let row = PyDict::new(py);
            row.set_item("name", name)?;
            row.set_item("wanted", *value)?;
            row.set_item("before", &before)?;
            row.set_item("after", &before)?;
            row.set_item("rewritten", false)?;
            row.set_item("ok", false)?;
            row.set_item("error", "")?;
            results.append(&row)?;
            if before.is_none() {
                row.set_item("error", "the vehicle did not answer for this parameter")?;
                continue;
            }
            if self.matches(name, before.extract()?, *value) {
                row.set_item("ok", true)?;
                confirmed_count += 1;
                continue;
            }
            if !rewrite {
                row.set_item("error", "the vehicle holds a different value")?;
                continue;
            }
            if armed(bridge)? {
                row.set_item("error", "not written again: the vehicle is armed")?;
                continue;
            }
            for _attempt in 0..PARAM_VERIFY_REWRITES {
                row.set_item("rewritten", true)?;
                row.set_item("error", "")?;
                if !bridge.call_method1("set_param", (name.as_str(), *value))?.is_truthy()? {
                    let last: String = bridge
                        .call_method0("get_last_command_error")?
                        .extract()
                        .unwrap_or_default();
                    let text = if last.is_empty() {
                        "write failed".to_string()
                    } else {
                        last
                    };
                    row.set_item("error", &text)?;
                    if text.contains("not connected") {
                        break;
                    }
                }
                let after = fetch(vec![name.clone()], 2.0)?.call_method1("get", (name.as_str(),))?;
                if !after.is_none() {
                    row.set_item("after", &after)?;
                    values.set_item(name.as_str(), &after)?;
                }
                if !after.is_none() && self.matches(name, after.extract()?, *value) {
                    row.set_item("ok", true)?;
                    row.set_item("error", "")?;
                    confirmed_count += 1;
                    break;
                }
                let error: String = row
                    .get_item("error")?
                    .map(|e| e.extract().unwrap_or_default())
                    .unwrap_or_default();
                if error.is_empty() {
                    let text = if after.is_none() {
                        "no answer when read back"
                    } else {
                        "the vehicle kept a different value"
                    };
                    row.set_item("error", text)?;
                }
            }
        }
        set_error(bridge, "")?;
        let missing: Vec<String> = read_names
            .iter()
            .filter(|n| !values.contains(n.as_str()).unwrap_or(false))
            .cloned()
            .collect();
        let out = PyDict::new(py);
        out.set_item("results", &results)?;
        out.set_item("values", &values)?;
        out.set_item("missing", missing)?;
        out.set_item("confirmed", confirmed_count)?;
        out.set_item("failed", wanted.len() - confirmed_count)?;
        out.set_item("all_confirmed", confirmed_count == wanted.len())?;
        Ok(out.into_any().unbind())
    }

    // ---- batch upload ----

    /// Validate a parameter file and claim the protocol for its upload. The
    /// caller holds the operation lock and starts the worker on True.
    fn begin_upload(&self, bridge: &Bound<'_, PyAny>, params: &Bound<'_, PyAny>) -> PyResult<bool> {
        let invalid = |bridge: &Bound<'_, PyAny>| -> PyResult<bool> {
            set_error(bridge, "invalid parameter list")?;
            Ok(false)
        };
        let Ok(list) = params.downcast::<PyList>() else {
            return invalid(bridge);
        };
        if list.is_empty() {
            return invalid(bridge);
        }
        for entry in list.iter() {
            let Ok(entry) = entry.downcast::<PyDict>() else {
                return invalid(bridge);
            };
            let name = entry.get_item("name")?;
            let value = entry.get_item("value")?;
            let name_ok = name.as_ref().is_some_and(|n| {
                n.downcast::<PyString>()
                    .is_ok_and(|s| !s.to_str().unwrap_or("").is_empty())
            });
            let value_ok = value.as_ref().is_some_and(|v| {
                !v.is_instance_of::<PyBool>() && (v.is_instance_of::<PyInt>() || v.is_instance_of::<PyFloat>())
            });
            if !name_ok || !value_ok {
                return invalid(bridge);
            }
        }
        if !truthy(bridge, "_connection_ready")? {
            set_error(bridge, "not connected")?;
            return Ok(false);
        }
        if armed(bridge)? {
            set_error(bridge, "cannot upload parameters while armed")?;
            return Ok(false);
        }
        let mut st = self.state();
        if st.state == "downloading" || st.state == "uploading" {
            drop(st);
            set_error(bridge, "another parameter operation in progress")?;
            return Ok(false);
        }
        st.state_before_upload = st.state.clone();
        st.state = "uploading".to_string();
        st.upload_total = list.len() as i64;
        st.upload_done = 0;
        st.upload_result = None;
        self.upload_cancel.store(false, Ordering::SeqCst);
        Ok(true)
    }

    /// The upload worker: every parameter through the bridge's confirmed
    /// write, progress to the listeners, and always a final announcement.
    fn run_upload(&self, bridge: &Bound<'_, PyAny>, params: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let mut failed: Vec<Failure> = Vec::new();
        let walked = (|| -> PyResult<()> {
            for p in params.try_iter()? {
                let p = p?;
                if !event_set(bridge, "_running")? || self.upload_cancel.load(Ordering::SeqCst) {
                    break;
                }
                let name = p.get_item("name")?;
                let value = py_float(&p.get_item("value")?)?;
                let ok = bridge.call_method1("set_param", (&name, value))?.is_truthy()?;
                let name_text: String = name.str()?.to_string();
                let status = {
                    let mut st = self.state();
                    if ok {
                        st.upload_done += 1;
                    } else {
                        drop(st);
                        let last: String = bridge
                            .call_method0("get_last_command_error")?
                            .extract()
                            .unwrap_or_default();
                        failed.push(Failure {
                            name: name_text.clone(),
                            error: if last.is_empty() {
                                "write failed".to_string()
                            } else {
                                last
                            },
                        });
                        st = self.state();
                    }
                    (st.state.clone(), st.upload_total, st.upload_done)
                };
                let d = status_dict(
                    py,
                    &[
                        ("state", obj(py, status.0.as_str())),
                        ("count", obj(py, status.1)),
                        ("received", obj(py, status.2)),
                        ("name", name.clone().unbind()),
                        ("value", obj(py, value)),
                    ],
                )?;
                Self::notify(bridge, d)?;
            }
            Ok(())
        })();
        if let Err(err) = walked {
            logger(py)?.call_method1("error", ("param upload failed: %s", err.value(py)))?;
            return self.finish_upload_inner(bridge, failed, false);
        }
        let interrupted = self.upload_cancel.load(Ordering::SeqCst) || !event_set(bridge, "_running")?;
        self.finish_upload_inner(bridge, failed, interrupted)
    }
}

/// The cache seen as the dict it used to be: `bridge._params`.
#[pyclass(name = "ParamCache", module = "corvus_link.params", frozen, mapping)]
pub struct ParamCache {
    engine: Py<ParamEngine>,
}

impl ParamCache {
    fn with<R>(&self, py: Python<'_>, f: impl FnOnce(&mut State) -> R) -> R {
        let engine = self.engine.bind(py).get();
        let mut st = engine.state();
        f(&mut st)
    }
}

#[pymethods]
impl ParamCache {
    fn __len__(&self, py: Python<'_>) -> usize {
        self.with(py, |st| st.params.len())
    }

    fn __contains__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> bool {
        match key.extract::<String>() {
            Ok(k) => self.with(py, |st| st.params.contains_key(&k)),
            Err(_) => false,
        }
    }

    fn __getitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        let k: String = key.extract().map_err(|_| PyKeyError::new_err(key.clone().unbind()))?;
        match self.with(py, |st| st.params.get(&k).cloned()) {
            Some(e) => ParamEngine::entry_obj(py, &e),
            None => Err(PyKeyError::new_err(k)),
        }
    }

    fn __setitem__(&self, py: Python<'_>, key: String, value: PyRef<'_, ParamEntry>) {
        let mut entry = Entry::from(&*value);
        if entry.name.is_empty() {
            entry.name = key.clone();
        }
        self.with(py, |st| {
            st.params.insert(key, entry);
        });
    }

    fn __delitem__(&self, py: Python<'_>, key: String) -> PyResult<()> {
        match self.with(py, |st| st.params.shift_remove(&key)) {
            Some(_) => Ok(()),
            None => Err(PyKeyError::new_err(key)),
        }
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let keys = self.keys(py);
        PyList::new(py, keys)?.into_any().try_iter().map(|i| i.into_any())
    }

    fn keys(&self, py: Python<'_>) -> Vec<String> {
        self.with(py, |st| st.params.keys().cloned().collect())
    }

    fn values(&self, py: Python<'_>) -> PyResult<Vec<PyObject>> {
        let entries: Vec<Entry> = self.with(py, |st| st.params.values().cloned().collect());
        entries.iter().map(|e| ParamEngine::entry_obj(py, e)).collect()
    }

    fn items(&self, py: Python<'_>) -> PyResult<Vec<(String, PyObject)>> {
        let entries: Vec<Entry> = self.with(py, |st| st.params.values().cloned().collect());
        entries
            .iter()
            .map(|e| Ok((e.name.clone(), ParamEngine::entry_obj(py, e)?)))
            .collect()
    }

    #[pyo3(signature = (key, default=None))]
    fn get(&self, py: Python<'_>, key: &Bound<'_, PyAny>, default: Option<PyObject>) -> PyResult<PyObject> {
        let found = key
            .extract::<String>()
            .ok()
            .and_then(|k| self.with(py, |st| st.params.get(&k).cloned()));
        match found {
            Some(e) => ParamEngine::entry_obj(py, &e),
            None => Ok(default.unwrap_or_else(|| py.None())),
        }
    }

    #[pyo3(signature = (key, default=None))]
    fn pop(&self, py: Python<'_>, key: &Bound<'_, PyAny>, default: Option<PyObject>) -> PyResult<PyObject> {
        let removed = key
            .extract::<String>()
            .ok()
            .and_then(|k| self.with(py, |st| st.params.shift_remove(&k)));
        match (removed, default) {
            (Some(e), _) => ParamEngine::entry_obj(py, &e),
            (None, Some(d)) => Ok(d),
            (None, None) => Err(PyKeyError::new_err(key.clone().unbind())),
        }
    }

    fn clear(&self, py: Python<'_>) {
        self.with(py, |st| st.params.clear());
    }

    fn copy<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        for (k, v) in self.items(py)? {
            d.set_item(k, v)?;
        }
        Ok(d)
    }

    fn __eq__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<bool> {
        let mine = self.copy(py)?;
        if let Ok(cache) = other.downcast::<ParamCache>() {
            return mine.eq(cache.get().copy(py)?);
        }
        mine.eq(other)
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(self.copy(py)?.repr()?.to_string())
    }

    fn __bool__(&self, py: Python<'_>) -> bool {
        self.__len__(py) > 0
    }

    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.engine)
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.params")?;
    m.add_class::<ParamEngine>()?;
    m.add_class::<ParamEntry>()?;
    m.add_class::<ParamCache>()?;
    m.add_function(wrap_pyfunction!(param_type_is_integer, &m)?)?;
    m.add_function(wrap_pyfunction!(decode_param_value, &m)?)?;
    m.add_function(wrap_pyfunction!(encode_param_value, &m)?)?;
    m.add_function(wrap_pyfunction!(param_values_match, &m)?)?;
    m.add_function(wrap_pyfunction!(encode_param_name, &m)?)?;
    m.add("PARAM_DOWNLOAD_INACTIVITY_S", PARAM_DOWNLOAD_INACTIVITY_S)?;
    m.add("PARAM_DOWNLOAD_MAX_ROUNDS", PARAM_DOWNLOAD_MAX_ROUNDS)?;
    m.add("PARAM_DOWNLOAD_TIMEOUT_S", PARAM_DOWNLOAD_TIMEOUT_S)?;
    m.add("PARAM_DOWNLOAD_TIMEOUT_S_SERIAL", PARAM_DOWNLOAD_TIMEOUT_S_SERIAL)?;
    m.add("PARAM_WATCHDOG_TICK_S", PARAM_WATCHDOG_TICK_S)?;
    m.add(
        "PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL",
        PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL,
    )?;
    m.add("PARAM_RETRANSMIT_MAX_PER_ROUND_UDP", PARAM_RETRANSMIT_MAX_PER_ROUND_UDP)?;
    m.add("PARAM_RETRANSMIT_GAP_S_SERIAL", PARAM_RETRANSMIT_GAP_S_SERIAL)?;
    m.add("PARAM_RETRANSMIT_GAP_S_UDP", PARAM_RETRANSMIT_GAP_S_UDP)?;
    m.add("PARAM_CACHE_MAX_ENTRIES", PARAM_CACHE_MAX_ENTRIES)?;
    m.add("PARAM_INDEX_NONE", PARAM_INDEX_NONE)?;
    m.add("PARAM_VERIFY_MAX_NAMES", PARAM_VERIFY_MAX_NAMES)?;
    m.add("PARAM_VERIFY_REWRITES", PARAM_VERIFY_REWRITES)?;
    m.add("PARAM_ABSENT_AFTER_MISSES", PARAM_ABSENT_AFTER_MISSES)?;
    m.add("PARAM_ID_MAX_BYTES", PARAM_ID_MAX_BYTES)?;
    m.add("PARAM_HASH_CHECK", PARAM_HASH_CHECK)?;
    m.add("PARAM_ENCODING_BYTEWISE", PARAM_ENCODING_BYTEWISE)?;
    m.add("PARAM_ENCODING_C_CAST", PARAM_ENCODING_C_CAST)?;
    parent.add_submodule(&m)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("corvus_link.params", &m)?;
    Ok(())
}
