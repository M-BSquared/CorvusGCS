//! The MAVLink mission protocol: the upload and download handshakes for
//! missions and geofences, the item requests served during an upload,
//! MISSION_CURRENT and MISSION_ITEM_REACHED turned into a place in the plan,
//! the record of what is on the vehicle, and the vehicle's verdict on a
//! mission it has just accepted.
//!
//! The engine owns the protocol state and its decisions. What belongs to the
//! bridge is reached through it: the link (`_link_send`), the locks, the
//! store, the console, the dialect. The tunables and the clock are read from
//! the Python module the engine was made for (`corvus.mavlink_missions`) at
//! the moment they are needed, so a test that changes one of them there
//! changes it here.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyCFunction, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};

use crate::lock;
use crate::protocol::host::{
    caught_by, console, event_set, link_send, logger, obj, py_float, py_int, set_error, truthy,
};

const MAV_MISSION_ACCEPTED: i64 = 0;
const MAV_MISSION_DENIED: i64 = 14;
const MAV_RESULT_ACCEPTED: i64 = 0;
const MAV_RESULT_DENIED: i64 = 2;
const MAV_RESULT_UNSUPPORTED: i64 = 3;
const MAV_RESULT_FAILED: i64 = 4;
const MAV_FRAME_GLOBAL: i64 = 0;
const MAV_FRAME_GLOBAL_RELATIVE_ALT: i64 = 3;
const MAV_CMD_NAV_WAYPOINT: i64 = 16;
const UNEXPECTED_REQUEST: &str = "req. wp was unexpected";

/// A wake-up the receive thread gives a waiting command thread.
struct Signal {
    set: Mutex<bool>,
    cv: Condvar,
}

impl Signal {
    fn new() -> Self {
        Signal {
            set: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    fn set(&self) {
        *lock(&self.set) = true;
        self.cv.notify_all();
    }

    fn clear(&self) {
        *lock(&self.set) = false;
    }

    /// `threading.Event.wait`: real time, True when set.
    fn wait(&self, timeout: f64) -> bool {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
        let mut set = lock(&self.set);
        while !*set {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            set = self
                .cv
                .wait_timeout(set, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }
}

/// One download in flight: what the receive thread fills in.
struct Download {
    signal: Signal,
    state: Mutex<DownloadState>,
}

struct DownloadState {
    count: Option<i64>,
    waiting_for: i64,
    items: HashMap<i64, PyObject>,
    drops: i64,
}

impl Download {
    fn new() -> Self {
        Download {
            signal: Signal::new(),
            state: Mutex::new(DownloadState {
                count: None,
                waiting_for: -1,
                items: HashMap::new(),
                drops: 0,
            }),
        }
    }
}

/// What the vehicle says about a mission it has just accepted.
#[pyclass(name = "MissionVerdict", module = "corvus_link.missions", frozen)]
pub struct MissionVerdict {
    #[pyo3(get)]
    total: i64,
    inner: Mutex<VerdictState>,
    signal: Signal,
}

struct VerdictState {
    after: f64,
    state: String,
    reasons: Vec<String>,
}

#[pymethods]
impl MissionVerdict {
    #[getter]
    fn state(&self) -> String {
        lock(&self.inner).state.clone()
    }

    #[getter]
    fn reasons(&self) -> Vec<String> {
        lock(&self.inner).reasons.clone()
    }

    #[getter]
    fn after(&self) -> f64 {
        lock(&self.inner).after
    }
}

struct State {
    items: Option<PyObject>,
    upload_type: i64,
    vehicle_fence: Option<PyObject>,
    pending_ack: Option<PyObject>,
    last_request: f64,
    download: Option<Arc<Download>>,
    index: Option<Vec<Option<i64>>>,
    mission_id: Option<i64>,
    id_pending: bool,
    confirmed: bool,
    noted_at: f64,
    revision: i64,
    verdict: Option<Py<MissionVerdict>>,
}

/// The mission protocol of one bridge.
#[pyclass(name = "MissionEngine", module = "corvus_link.missions", frozen)]
pub struct MissionEngine {
    env: PyObject,
    st: Mutex<State>,
}

fn store_update(bridge: &Bound<'_, PyAny>, fields: &Bound<'_, PyDict>) -> PyResult<()> {
    bridge.getattr("_store")?.call_method("update", (), Some(fields))?;
    Ok(())
}

fn snapshot<'py>(bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    bridge.getattr("_store")?.call_method0("get_snapshot")
}

/// `int(getattr(msg, name, default) or 0)`.
fn int_field(msg: &Bound<'_, PyAny>, name: &str, default: i64) -> PyResult<i64> {
    match msg.getattr(name) {
        Ok(v) if v.is_truthy()? => py_int(&v),
        Ok(_) => Ok(0),
        Err(_) => Ok(default),
    }
}

fn finite_or_none(py: Python<'_>, value: Option<Bound<'_, PyAny>>) -> PyObject {
    let Some(value) = value else {
        return py.None();
    };
    match py_float(&value) {
        Ok(v) if v.is_finite() => v
            .into_pyobject(py)
            .map(|f| f.into_any().unbind())
            .unwrap_or_else(|_| py.None()),
        _ => py.None(),
    }
}

fn is_number(value: &Bound<'_, PyAny>) -> bool {
    !value.is_instance_of::<PyBool>() && (value.is_instance_of::<PyInt>() || value.is_instance_of::<PyFloat>())
}

impl MissionEngine {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.st)
    }

    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn tunable(&self, py: Python<'_>, name: &str) -> PyResult<f64> {
        self.env(py).getattr(name)?.extract()
    }

    fn monotonic(&self, py: Python<'_>) -> PyResult<f64> {
        self.env(py).getattr("time")?.call_method0("monotonic")?.extract()
    }

    fn mission_plan<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr("mission_plan")
    }

    fn offset(bridge: &Bound<'_, PyAny>) -> PyResult<i64> {
        Ok(
            if bridge
                .getattr("_dialect")?
                .getattr("mission_seq0_is_home")?
                .is_truthy()?
            {
                1
            } else {
                0
            },
        )
    }

    fn plan_item_of(&self, seq: i64) -> i64 {
        let st = self.state();
        match &st.index {
            Some(index) if seq >= 0 && (seq as usize) < index.len() => index[seq as usize].unwrap_or(-1),
            _ => -1,
        }
    }

    fn download_wait_s(&self, bridge: &Bound<'_, PyAny>) -> PyResult<f64> {
        let py = bridge.py();
        let slow = match bridge.getattr("_is_slow_link") {
            Ok(f) => f.call0()?.is_truthy()?,
            Err(_) => false,
        };
        self.tunable(
            py,
            if slow {
                "MISSION_DOWNLOAD_WAIT_S_SLOW"
            } else {
                "MISSION_DOWNLOAD_WAIT_S"
            },
        )
    }

    /// Send, wait, repeat. Some(true) answered, Some(false) lost, None interrupted.
    fn ask(
        &self,
        bridge: &Bound<'_, PyAny>,
        download: &Arc<Download>,
        method: &str,
        args: &dyn Fn() -> Vec<PyObject>,
        answered: &dyn Fn() -> bool,
        tries: i64,
    ) -> PyResult<Option<bool>> {
        let py = bridge.py();
        for _attempt in 0..tries {
            if event_set(bridge, "_stop_event")? || !truthy(bridge, "_connection_ready")? {
                return Ok(None);
            }
            download.signal.clear();
            let drops = lock(&download.state).drops;
            let conn = bridge.getattr("_conn")?;
            match link_send(bridge, &conn, method, args()) {
                Ok(true) => {}
                Ok(false) => return Ok(None),
                Err(err) => {
                    logger(py)?.call_method1("debug", ("mission download send failed: %s", err.value(py)))?;
                    return Ok(None);
                }
            }
            let deadline = self.monotonic(py)? + self.download_wait_s(bridge)?;
            while self.monotonic(py)? < deadline {
                let d = Arc::clone(download);
                if py.allow_threads(move || d.signal.wait(0.1)) && answered() {
                    return Ok(Some(true));
                }
                if bridge
                    .getattr("_operation_lock")?
                    .call_method0("abort_waiting")?
                    .is_truthy()?
                {
                    return Ok(None);
                }
                if lock(&download.state).drops != drops {
                    break;
                }
            }
            if answered() {
                return Ok(Some(true));
            }
        }
        Ok(Some(false))
    }

    fn ask_list(&self, bridge: &Bound<'_, PyAny>, download: &Arc<Download>) -> PyResult<Option<bool>> {
        let py = bridge.py();
        lock(&download.state).count = None;
        let tries: i64 = self.env(py).getattr("MISSION_DOWNLOAD_TRIES")?.extract()?;
        self.ask(
            bridge,
            download,
            "mission_request_list_send",
            &|| vec![obj(py, 0)],
            &|| lock(&download.state).count.is_some(),
            tries,
        )
    }

    /// The download handshake. None on failure, with the reason set.
    fn run_download(&self, bridge: &Bound<'_, PyAny>, download: &Arc<Download>) -> PyResult<Option<Vec<PyObject>>> {
        let py = bridge.py();
        let got = self.ask_list(bridge, download)?;
        match got {
            None => {
                bridge.call_method0("_cancel_mission_download")?;
                set_error(bridge, "mission download interrupted")?;
                return Ok(None);
            }
            Some(false) => {
                set_error(bridge, "the vehicle did not answer the mission request")?;
                return Ok(None);
            }
            Some(true) => {}
        }
        let total = lock(&download.state).count.unwrap_or(0);
        let offset = if total != 0 { Self::offset(bridge)? } else { 0 };
        let max_items: i64 = self.mission_plan(py)?.getattr("MISSION_MAX_ITEMS")?.extract()?;
        if total - offset > max_items {
            bridge.call_method0("_cancel_mission_download")?;
            set_error(
                bridge,
                &format!(
                    "the vehicle's mission has {} items, more than the {max_items} the planner holds",
                    total - offset
                ),
            )?;
            return Ok(None);
        }
        let restarts_max: i64 = self.env(py).getattr("MISSION_DOWNLOAD_RESTARTS")?.extract()?;
        for seq in 0..total {
            lock(&download.state).waiting_for = seq;
            let mut restarts = 0;
            loop {
                let got = self.ask(
                    bridge,
                    download,
                    "mission_request_int_send",
                    &|| vec![obj(py, seq), obj(py, 0)],
                    &|| lock(&download.state).items.contains_key(&seq),
                    1,
                )?;
                match got {
                    Some(true) => break,
                    None => {
                        bridge.call_method0("_cancel_mission_download")?;
                        set_error(bridge, "mission download interrupted")?;
                        return Ok(None);
                    }
                    Some(false) => {}
                }
                let drops = lock(&download.state).drops;
                if restarts >= restarts_max {
                    bridge.call_method0("_cancel_mission_download")?;
                    let reason = if drops != 0 {
                        ". The vehicle keeps reporting the request as unexpected, so another station (a companion computer or a second ground station) is probably reading the mission at the same time"
                    } else {
                        ""
                    };
                    set_error(
                        bridge,
                        &format!("mission item {} of {total} never arrived{reason}", seq + 1),
                    )?;
                    return Ok(None);
                }
                restarts += 1;
                let lead = if drops != 0 {
                    "The vehicle ended the transfer and reported the request as unexpected. ".to_string()
                } else {
                    format!("Mission item {} did not arrive. ", seq + 1)
                };
                console(
                    bridge,
                    "MISSION",
                    &format!("{lead}Reading the mission again from the start"),
                    "info",
                )?;
                lock(&download.state).waiting_for = -1;
                let again = self.ask_list(bridge, download)?;
                if again != Some(true) {
                    bridge.call_method0("_cancel_mission_download")?;
                    set_error(
                        bridge,
                        if again.is_none() {
                            "mission download interrupted"
                        } else {
                            "the vehicle did not answer the mission request"
                        },
                    )?;
                    return Ok(None);
                }
                if lock(&download.state).count.unwrap_or(0) != total {
                    bridge.call_method0("_cancel_mission_download")?;
                    set_error(bridge, "the mission on the vehicle changed while it was being read")?;
                    return Ok(None);
                }
                lock(&download.state).waiting_for = seq;
            }
        }
        if total != 0 {
            bridge.call_method1("_send_mission_ack", (MAV_MISSION_ACCEPTED,))?;
        }
        let st = lock(&download.state);
        Ok(Some((offset..total).map(|seq| st.items[&seq].clone_ref(py)).collect()))
    }

    fn note(&self, bridge: &Bound<'_, PyAny>, plan_index: Option<Vec<Option<i64>>>) -> PyResult<()> {
        let py = bridge.py();
        let now = self.monotonic(py)?;
        let (known, total, revision) = {
            let mut st = self.state();
            let total = plan_index.as_ref().map(|i| i.len() as i64).unwrap_or(-1);
            let known = plan_index.is_some();
            st.index = plan_index;
            st.id_pending = true;
            st.confirmed = false;
            st.noted_at = now;
            st.revision += 1;
            (known, total, st.revision)
        };
        let fields = PyDict::new(py);
        fields.set_item("mission_known", known)?;
        fields.set_item("mission_revision", revision)?;
        fields.set_item("mission_total", total)?;
        fields.set_item("mission_item", -1)?;
        fields.set_item("mission_reached_item", -1)?;
        store_update(bridge, &fields)
    }

    fn forget(&self, bridge: &Bound<'_, PyAny>, reason: &str) -> PyResult<()> {
        let py = bridge.py();
        let (had, revision) = {
            let mut st = self.state();
            let had = st.index.is_some();
            st.index = None;
            st.revision += 1;
            (had, st.revision)
        };
        let fields = PyDict::new(py);
        fields.set_item("mission_known", false)?;
        fields.set_item("mission_revision", revision)?;
        fields.set_item("mission_item", -1)?;
        fields.set_item("mission_reached_item", -1)?;
        store_update(bridge, &fields)?;
        if had && !reason.is_empty() {
            let text: String = PyString::new(py, reason).call_method0("capitalize")?.extract()?;
            console(bridge, "MISSION", &text, "info")?;
        }
        Ok(())
    }

    fn home_lat_lon_inner(bridge: &Bound<'_, PyAny>) -> PyResult<Option<(f64, f64)>> {
        let value = snapshot(bridge)?.call_method1("get", ("home",))?;
        let value = if value.is_truthy()? {
            value
        } else {
            PyList::empty(bridge.py()).into_any()
        };
        let pair =
            (|| -> PyResult<(f64, f64)> { Ok((py_float(&value.get_item(0)?)?, py_float(&value.get_item(1)?)?)) })();
        let (lon, lat) = match pair {
            Ok(p) => p,
            Err(err) if caught_by(bridge.py(), &err, &["TypeError", "ValueError", "IndexError"]) => return Ok(None),
            Err(err) => return Err(err),
        };
        if (lat, lon) == (0.0, 0.0) || !(lat.is_finite() && lon.is_finite()) {
            return Ok(None);
        }
        Ok(Some((lat, lon)))
    }

    /// Arm a verdict for the receive thread; returns it and the console
    /// listener that collects the critical STATUSTEXT lines meanwhile.
    fn listen(&self, bridge: &Bound<'_, PyAny>, total: i64) -> PyResult<(Py<MissionVerdict>, PyObject)> {
        let py = bridge.py();
        let verdict = Py::new(
            py,
            MissionVerdict {
                total,
                inner: Mutex::new(VerdictState {
                    after: f64::INFINITY,
                    state: String::new(),
                    reasons: Vec::new(),
                }),
                signal: Signal::new(),
            },
        )?;
        let target = verdict.clone_ref(py);
        let collect = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, _kwargs| -> PyResult<()> {
                let entry = args.get_item(0)?;
                let name = entry.call_method1("get", ("name",))?;
                let level = entry.call_method1("get", ("level",))?;
                if name.eq("STATUSTEXT")? && level.eq("critical")? {
                    let raw = entry.call_method1("get", ("text",))?;
                    let raw = if raw.is_truthy()? {
                        raw
                    } else {
                        PyString::new(args.py(), "").into_any()
                    };
                    let text: String = raw.str()?.call_method0("strip")?.extract()?;
                    if !text.is_empty() {
                        lock(&target.get().inner).reasons.push(text);
                    }
                }
                Ok(())
            },
        )?
        .into_any()
        .unbind();
        if let Ok(add) = bridge.getattr("add_console_sub") {
            if add.is_callable() {
                add.call1((collect.bind(py),))?;
            }
        }
        self.state().verdict = Some(verdict.clone_ref(py));
        Ok((verdict, collect))
    }

    fn unlisten(&self, bridge: &Bound<'_, PyAny>, verdict: &Py<MissionVerdict>, collect: &PyObject) -> PyResult<()> {
        let py = bridge.py();
        {
            let mut st = self.state();
            if st.verdict.as_ref().is_some_and(|v| v.is(verdict)) {
                st.verdict = None;
            }
        }
        if let Ok(remove) = bridge.getattr("remove_console_sub") {
            if remove.is_callable() {
                remove.call1((collect.bind(py),))?;
            }
        }
        Ok(())
    }

    /// The vehicle's reason for refusing a mission it has just accepted, or "".
    fn await_verdict(&self, bridge: &Bound<'_, PyAny>, verdict: Option<&Py<MissionVerdict>>) -> PyResult<String> {
        let py = bridge.py();
        let Some(verdict) = verdict else {
            return Ok(String::new());
        };
        let v = verdict.get();
        lock(&v.inner).after = self.monotonic(py)? + self.tunable(py, "MISSION_VERDICT_SETTLE_S")?;
        let deadline = self.monotonic(py)? + self.tunable(py, "MISSION_VERDICT_WAIT_S")?;
        loop {
            let target = verdict.clone_ref(py);
            if py.allow_threads(move || target.get().signal.wait(0.1)) {
                break;
            }
            if self.monotonic(py)? >= deadline {
                return Ok(String::new());
            }
            if event_set(bridge, "_stop_event")?
                || bridge
                    .getattr("_operation_lock")?
                    .call_method0("abort_waiting")?
                    .is_truthy()?
            {
                return Ok(String::new());
            }
        }
        let (state, all) = {
            let inner = lock(&v.inner);
            (inner.state.clone(), inner.reasons.clone())
        };
        if state != "no_mission" {
            return Ok(String::new());
        }
        let named: Vec<String> = all
            .iter()
            .filter(|t| t.to_lowercase().contains("mission"))
            .cloned()
            .collect();
        let reasons = if named.is_empty() { all } else { named };
        if let Some(last) = reasons.last() {
            let stripped: String = self
                .env(py)
                .getattr("_REJECTED_PREFIX")?
                .call_method1("sub", ("", last.as_str()))?
                .extract()?;
            return Ok(format!("The vehicle rejected the mission: {stripped}"));
        }
        if Self::home_lat_lon_inner(bridge)?.is_none() {
            return Ok("The vehicle rejected the mission: it has no home position yet. Every height in the plan is measured from home, so wait for a GPS fix and upload again.".to_string());
        }
        let hint = bridge.getattr("_dialect")?.getattr("mission_rejection_hint")?;
        let hint: String = if hint.is_truthy()? {
            hint.str()?.to_string()
        } else {
            String::new()
        };
        Ok(format!(
            "The vehicle stored the mission but will not fly it.{}",
            if hint.is_empty() {
                String::new()
            } else {
                format!(" {hint}")
            }
        ))
    }

    /// `bridge._upload_mission(items[, mission_type])`, through the bridge so
    /// a replaced method is the one that runs.
    fn bridge_upload(bridge: &Bound<'_, PyAny>, items: &Bound<'_, PyAny>, mission_type: i64) -> PyResult<i64> {
        let result = if mission_type != 0 {
            bridge.call_method1("_upload_mission", (items, mission_type))?
        } else {
            bridge.call_method1("_upload_mission", (items,))?
        };
        result.extract()
    }

    fn bridge_note(bridge: &Bound<'_, PyAny>, index: &Bound<'_, PyAny>) -> PyResult<()> {
        bridge.call_method1("_note_vehicle_mission", (index,))?;
        Ok(())
    }

    fn bridge_ack_result(bridge: &Bound<'_, PyAny>, ack: i64) -> PyResult<i64> {
        if ack < 0 {
            return Ok(ack);
        }
        bridge.call_method1("_mission_ack_to_result", (ack,))?.extract()
    }

    fn command_failure(bridge: &Bound<'_, PyAny>, action: &str, result: i64) -> PyResult<bool> {
        bridge.call_method1("_command_failure", (action, result))?.is_truthy()
    }

    fn index_of(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<Option<Vec<Option<i64>>>> {
        if value.is_none() {
            return Ok(None);
        }
        let mut out = Vec::new();
        for item in value.try_iter()? {
            let item = item?;
            out.push(if item.is_none() { None } else { Some(py_int(&item)?) });
        }
        let _ = py;
        Ok(Some(out))
    }
}

pub const MAV_MISSION_OPERATION_CANCELLED: i64 = 15;

#[pymethods]
impl MissionEngine {
    /// *env* is the module that holds the tunables and the clock
    /// (``corvus.mavlink_missions``).
    #[new]
    fn new(env: PyObject) -> Self {
        MissionEngine {
            env,
            st: Mutex::new(State {
                items: None,
                upload_type: 0,
                vehicle_fence: None,
                pending_ack: None,
                last_request: 0.0,
                download: None,
                index: None,
                mission_id: None,
                id_pending: false,
                confirmed: false,
                noted_at: 0.0,
                revision: 0,
                verdict: None,
            }),
        }
    }

    // ---- the state, as the bridge's attributes used to hold it ----

    #[getter(items)]
    fn get_items(&self, py: Python<'_>) -> PyObject {
        self.state()
            .items
            .as_ref()
            .map(|i| i.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }
    #[setter(items)]
    fn set_items(&self, py: Python<'_>, value: PyObject) {
        self.state().items = if value.is_none(py) { None } else { Some(value) };
    }
    #[getter(upload_type)]
    fn get_upload_type(&self) -> i64 {
        self.state().upload_type
    }
    #[setter(upload_type)]
    fn set_upload_type(&self, value: i64) {
        self.state().upload_type = value;
    }
    #[getter(vehicle_fence)]
    fn get_vehicle_fence(&self, py: Python<'_>) -> PyObject {
        self.state()
            .vehicle_fence
            .as_ref()
            .map(|i| i.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }
    #[setter(vehicle_fence)]
    fn set_vehicle_fence(&self, py: Python<'_>, value: PyObject) {
        self.state().vehicle_fence = if value.is_none(py) { None } else { Some(value) };
    }
    #[getter(pending_ack)]
    fn get_pending_ack(&self, py: Python<'_>) -> PyObject {
        self.state()
            .pending_ack
            .as_ref()
            .map(|i| i.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }
    #[setter(pending_ack)]
    fn set_pending_ack(&self, py: Python<'_>, value: PyObject) {
        self.state().pending_ack = if value.is_none(py) { None } else { Some(value) };
    }
    #[getter(last_request)]
    fn get_last_request(&self) -> f64 {
        self.state().last_request
    }
    #[setter(last_request)]
    fn set_last_request(&self, value: f64) {
        self.state().last_request = value;
    }
    #[getter(vehicle_index)]
    fn get_index(&self) -> Option<Vec<Option<i64>>> {
        self.state().index.clone()
    }
    #[setter(vehicle_index)]
    fn set_index(&self, py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let index = Self::index_of(py, value)?;
        self.state().index = index;
        Ok(())
    }
    #[getter(vehicle_mission_id)]
    fn get_mission_id(&self) -> Option<i64> {
        self.state().mission_id
    }
    #[setter(vehicle_mission_id)]
    fn set_mission_id(&self, value: Option<i64>) {
        self.state().mission_id = value;
    }
    #[getter(vehicle_mission_id_pending)]
    fn get_id_pending(&self) -> bool {
        self.state().id_pending
    }
    #[setter(vehicle_mission_id_pending)]
    fn set_id_pending(&self, value: bool) {
        self.state().id_pending = value;
    }
    #[getter(vehicle_mission_confirmed)]
    fn get_confirmed(&self) -> bool {
        self.state().confirmed
    }
    #[setter(vehicle_mission_confirmed)]
    fn set_confirmed(&self, value: bool) {
        self.state().confirmed = value;
    }
    #[getter(vehicle_mission_noted_at)]
    fn get_noted_at(&self) -> f64 {
        self.state().noted_at
    }
    #[setter(vehicle_mission_noted_at)]
    fn set_noted_at(&self, value: f64) {
        self.state().noted_at = value;
    }
    #[getter(revision)]
    fn get_revision(&self) -> i64 {
        self.state().revision
    }
    #[setter(revision)]
    fn set_revision(&self, value: i64) {
        self.state().revision = value;
    }
    #[getter(verdict)]
    fn get_verdict(&self, py: Python<'_>) -> PyObject {
        self.state()
            .verdict
            .as_ref()
            .map(|v| v.clone_ref(py).into_any())
            .unwrap_or_else(|| py.None())
    }

    /// Wake a blocked uploader so a teardown cannot hang on it.
    fn cancel_upload(&self) -> PyResult<()> {
        let pending = {
            let mut st = self.state();
            st.items = None;
            st.pending_ack.take()
        };
        if let Some(pending) = pending {
            Python::with_gil(|py| -> PyResult<()> {
                let p = pending.bind(py);
                p.setattr("result", -2)?;
                p.getattr("event")?.call_method0("set")?;
                Ok(())
            })?;
        }
        Ok(())
    }

    // ---- receive ----

    /// Serve one MISSION_ITEM(_INT) during an upload this bridge started.
    fn handle_request(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>, as_int: bool) -> PyResult<()> {
        let py = bridge.py();
        let conn = bridge.getattr("_conn")?;
        if !conn.is_truthy()? || !bridge.call_method1("_ack_is_for_us", (msg,))?.is_truthy()? {
            return Ok(());
        }
        let seq = py_int(
            &msg.getattr("seq")
                .unwrap_or_else(|_| (-1i64).into_pyobject(py).unwrap().into_any()),
        )?;
        let kind = int_field(msg, "mission_type", 0)?;
        let (items, upload_type) = {
            let st = self.state();
            (st.items.as_ref().map(|i| i.clone_ref(py)), st.upload_type)
        };
        let Some(items) = items else {
            return Ok(());
        };
        let items = items.bind(py);
        let len = items.len()? as i64;
        if !(0..len).contains(&seq) || kind != upload_type {
            return Ok(());
        }
        let item = items.get_item(seq)?;
        let now = self.monotonic(py)?;
        self.state().last_request = now;
        let sent = (|| -> PyResult<()> {
            let key = |k: &str| -> PyResult<PyObject> { Ok(item.get_item(k)?.unbind()) };
            let (x, y) = if as_int { ("x_int", "y_int") } else { ("x_f", "y_f") };
            let mut args = vec![
                obj(py, seq),
                key("frame")?,
                key("command")?,
                key("current")?,
                key("autocontinue")?,
                key("param1")?,
                key("param2")?,
                key("param3")?,
                key("param4")?,
                key(x)?,
                key(y)?,
                key("z")?,
            ];
            if upload_type != 0 {
                args.push(obj(py, upload_type));
            }
            let method = if as_int {
                "mission_item_int_send"
            } else {
                "mission_item_send"
            };
            link_send(bridge, &conn, method, args)?;
            Ok(())
        })();
        if let Err(err) = sent {
            logger(py)?.call_method1("debug", ("mission item send failed (seq=%d): %s", seq, err.value(py)))?;
        }
        Ok(())
    }

    /// The vehicle's answer to MISSION_REQUEST_LIST: how many items.
    fn handle_count(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        if !bridge.call_method1("_ack_is_for_us", (msg,))?.is_truthy()? || int_field(msg, "mission_type", 0)? != 0 {
            return Ok(());
        }
        let download = self.state().download.clone();
        let Some(download) = download else {
            return Ok(());
        };
        if lock(&download.state).count.is_some() {
            return Ok(());
        }
        let count = match msg.getattr("count").and_then(|c| py_int(&c)) {
            Ok(c) => c.max(0),
            Err(err) if caught_by(msg.py(), &err, &["TypeError", "ValueError"]) => return Ok(()),
            Err(err) => return Err(err),
        };
        lock(&download.state).count = Some(count);
        download.signal.set();
        Ok(())
    }

    /// One item of a download, kept only if it is the one asked for.
    fn handle_item(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>, as_int: bool) -> PyResult<()> {
        let py = bridge.py();
        if !bridge.call_method1("_ack_is_for_us", (msg,))?.is_truthy()? || int_field(msg, "mission_type", 0)? != 0 {
            return Ok(());
        }
        let built = (|| -> PyResult<(i64, Bound<'_, PyDict>)> {
            let seq = py_int(&msg.getattr("seq")?)?;
            let x = msg.getattr("x")?;
            let y = msg.getattr("y")?;
            let item = PyDict::new(py);
            item.set_item("command", py_int(&msg.getattr("command")?)?)?;
            item.set_item("frame", py_int(&msg.getattr("frame")?)?)?;
            let (lat, lon) = if as_int {
                (py_int(&x)? as f64 / 1e7, py_int(&y)? as f64 / 1e7)
            } else {
                (py_float(&x)?, py_float(&y)?)
            };
            item.set_item("lat", lat)?;
            item.set_item("lon", lon)?;
            let z = finite_or_none(py, Some(msg.getattr("z")?));
            let alt = if z.bind(py).is_truthy()? { z } else { obj(py, 0.0) };
            item.set_item("alt", alt)?;
            let params: Vec<PyObject> = (1..=4)
                .map(|n| finite_or_none(py, msg.getattr(format!("param{n}").as_str()).ok()))
                .collect();
            item.set_item("params", PyList::new(py, params)?)?;
            let autocontinue = match msg.getattr("autocontinue") {
                Ok(v) if v.is_truthy()? => py_int(&v)?,
                Ok(_) => 0,
                Err(_) => 1,
            };
            item.set_item("autocontinue", autocontinue)?;
            Ok((seq, item))
        })();
        let (seq, item) = match built {
            Ok(v) => v,
            Err(err) if caught_by(py, &err, &["AttributeError", "TypeError", "ValueError"]) => {
                logger(py)?.call_method1("debug", ("discarding malformed mission item",))?;
                return Ok(());
            }
            Err(err) => return Err(err),
        };
        let download = self.state().download.clone();
        let Some(download) = download else {
            return Ok(());
        };
        {
            let mut st = lock(&download.state);
            if seq != st.waiting_for {
                return Ok(());
            }
            st.items.insert(seq, item.into_any().unbind());
        }
        download.signal.set();
        Ok(())
    }

    /// Which item the vehicle is flying to, and whether its mission changed.
    fn handle_current(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let offset = Self::offset(bridge)?;
        let seq = match msg.getattr("seq").and_then(|s| py_int(&s)) {
            Ok(s) => s - offset,
            Err(err) if caught_by(py, &err, &["AttributeError", "TypeError", "ValueError"]) => return Ok(()),
            Err(err) => return Err(err),
        };
        let fields = PyDict::new(py);
        fields.set_item("mission_seq", if seq >= 0 { seq } else { -1 })?;
        fields.set_item("mission_item", self.plan_item_of(seq))?;
        let total = msg.getattr("total").ok();
        let total_int: Option<i64> = match &total {
            Some(t) if t.is_instance_of::<PyInt>() => Some(t.extract()?),
            _ => None,
        };
        let total_none: i64 = self.env(py).getattr("MISSION_TOTAL_NONE")?.extract()?;
        let mut mission_total: Option<i64> = None;
        if let Some(t) = total_int.filter(|t| *t != 0) {
            let value = if t == total_none { 0 } else { (t - offset).max(0) };
            fields.set_item("mission_total", value)?;
            mission_total = Some(value);
        }
        let state_names = self.env(py).getattr("MISSION_STATE_NAMES")?;
        let state = state_names.call_method1("get", (int_field(msg, "mission_state", 0)?,))?;
        let state: Option<String> = if state.is_truthy()? {
            Some(state.extract()?)
        } else {
            None
        };
        if let Some(s) = &state {
            fields.set_item("mission_state", s)?;
        }
        let verdict = self.state().verdict.as_ref().map(|v| v.clone_ref(py));
        if let Some(verdict) = verdict {
            let v = verdict.get();
            if let Some(s) = &state {
                if total_int == Some(v.total) && self.monotonic(py)? >= lock(&v.inner).after {
                    lock(&v.inner).state = s.clone();
                    v.signal.set();
                }
            }
        }
        let mut changed = false;
        let mission_id = int_field(msg, "mission_id", 0)?;
        {
            let mut st = self.state();
            if mission_id != 0 {
                if st.id_pending || st.mission_id.is_none() {
                    st.mission_id = Some(mission_id);
                    st.id_pending = false;
                } else if Some(mission_id) != st.mission_id {
                    st.mission_id = Some(mission_id);
                    changed = true;
                }
            }
        }
        let (index_len, confirmed, noted_at) = {
            let st = self.state();
            (st.index.as_ref().map(|i| i.len() as i64), st.confirmed, st.noted_at)
        };
        if let (Some(len), Some(total)) = (index_len, mission_total) {
            if total == len {
                self.state().confirmed = true;
            } else if confirmed || self.monotonic(py)? - noted_at >= self.tunable(py, "MISSION_CHANGE_GRACE_S")? {
                changed = true;
            }
        }
        if changed && index_len.is_some() {
            bridge.call_method1(
                "_forget_vehicle_mission",
                ("the vehicle holds a different mission now",),
            )?;
            fields.set_item("mission_item", -1)?;
        }
        store_update(bridge, &fields)
    }

    fn handle_item_reached(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let offset = Self::offset(bridge)?;
        let seq = match msg.getattr("seq").and_then(|s| py_int(&s)) {
            Ok(s) => s - offset,
            Err(err) if caught_by(py, &err, &["AttributeError", "TypeError", "ValueError"]) => return Ok(()),
            Err(err) => return Err(err),
        };
        if seq >= 0 {
            let fields = PyDict::new(py);
            fields.set_item("mission_reached", seq)?;
            fields.set_item("mission_reached_item", self.plan_item_of(seq))?;
            store_update(bridge, &fields)?;
        }
        Ok(())
    }

    /// Resolve the pending upload waiter with the MISSION_ACK result.
    fn handle_ack(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if !bridge.call_method1("_ack_is_for_us", (msg,))?.is_truthy()? {
            return Ok(());
        }
        let ack_type = py_int(
            &msg.getattr("type")
                .unwrap_or_else(|_| (-1i64).into_pyobject(py).unwrap().into_any()),
        )?;
        let level = if ack_type == MAV_MISSION_ACCEPTED {
            "success"
        } else {
            "error"
        };
        console(bridge, "GOTOPOINTS", &format!("MISSION_ACK: type={ack_type}"), level)?;
        let pending = self.state().pending_ack.as_ref().map(|p| p.clone_ref(py));
        if let Some(pending) = pending {
            let p = pending.bind(py);
            if p.getattr("result")?.is_none() {
                p.setattr("result", ack_type)?;
                p.getattr("event")?.call_method0("set")?;
            }
        }
        Ok(())
    }

    /// MISSION_ACK to end a transfer; a send that fails is only logged, the
    /// vehicle times the transfer out anyway.
    #[pyo3(signature = (bridge, result, mission_type=0))]
    fn send_ack(&self, bridge: &Bound<'_, PyAny>, result: i64, mission_type: i64) -> PyResult<()> {
        let py = bridge.py();
        let conn = bridge.getattr("_conn")?;
        if conn.is_none() {
            return Ok(());
        }
        if let Err(err) = link_send(
            bridge,
            &conn,
            "mission_ack_send",
            vec![obj(py, result), obj(py, mission_type)],
        ) {
            logger(py)?.call_method1("debug", ("mission ack send failed: %s", err.value(py)))?;
        }
        Ok(())
    }

    // ---- what is on the vehicle ----

    fn mission_offset(&self, bridge: &Bound<'_, PyAny>) -> PyResult<i64> {
        Self::offset(bridge)
    }

    #[pyo3(name = "plan_item_of")]
    fn py_plan_item_of(&self, seq: i64) -> i64 {
        self.plan_item_of(seq)
    }

    fn note_vehicle_mission(&self, bridge: &Bound<'_, PyAny>, plan_index: &Bound<'_, PyAny>) -> PyResult<()> {
        let index = Self::index_of(bridge.py(), plan_index)?;
        self.note(bridge, index)
    }

    #[pyo3(signature = (bridge, reason=""))]
    fn forget_vehicle_mission(&self, bridge: &Bound<'_, PyAny>, reason: &str) -> PyResult<()> {
        self.forget(bridge, reason)
    }

    fn home_lat_lon(&self, bridge: &Bound<'_, PyAny>) -> PyResult<Option<(f64, f64)>> {
        Self::home_lat_lon_inner(bridge)
    }

    // ---- transfers ----

    /// Upload mission items and wait for MISSION_ACK. MAV_MISSION_ACCEPTED,
    /// the ACK type on rejection, -1 timeout, -2 disconnect, -3 cancelled for
    /// a waiting abort command.
    #[pyo3(signature = (bridge, items, mission_type=0))]
    fn upload(&self, bridge: &Bound<'_, PyAny>, items: &Bound<'_, PyAny>, mission_type: i64) -> PyResult<i64> {
        let py = bridge.py();
        let env = self.env(py);
        let pending = env.getattr("_PendingAck")?.call0()?;
        let items = PyList::new(py, items.try_iter()?.collect::<PyResult<Vec<_>>>()?)?;
        let count = items.len() as i64;
        let started = self.monotonic(py)?;
        {
            let mut st = self.state();
            st.upload_type = mission_type;
            st.items = Some(items.clone().into_any().unbind());
            st.pending_ack = Some(pending.clone().unbind());
            st.last_request = started;
        }
        let slow = match bridge.getattr("_is_slow_link") {
            Ok(f) => f.call0()?.is_truthy()?,
            Err(_) => false,
        };
        let count_retry_s = self.tunable(
            py,
            if slow {
                "MISSION_COUNT_RETRY_S_SLOW"
            } else {
                "MISSION_COUNT_RETRY_S"
            },
        )?;
        let send_count = || -> PyResult<bool> {
            let conn = bridge.getattr("_conn")?;
            let mut args = vec![obj(py, count)];
            if mission_type != 0 {
                args.push(obj(py, mission_type));
            }
            match link_send(bridge, &conn, "mission_count_send", args) {
                Ok(sent) => Ok(sent),
                Err(err) => {
                    logger(py)?.call_method1("error", ("mission_count_send failed: %s", err.value(py)))?;
                    Ok(false)
                }
            }
        };
        let outcome = (|| -> PyResult<i64> {
            if !send_count()? {
                return Ok(-2);
            }
            let mut count_sent_at = started;
            let mut count_sends: i64 = 1;
            let tries: i64 = env.getattr("MISSION_COUNT_TRIES")?.extract()?;
            let max_s = self.tunable(py, "MISSION_UPLOAD_MAX_S")?;
            let deadline = self.monotonic(py)? + f64::min(max_s, 5.0 + 2.0 * count as f64);
            loop {
                if pending.getattr("event")?.call_method1("wait", (0.2,))?.is_truthy()? {
                    let result = pending.getattr("result")?;
                    return Ok(if result.is_none() { -1 } else { result.extract()? });
                }
                if bridge
                    .getattr("_operation_lock")?
                    .call_method0("abort_waiting")?
                    .is_truthy()?
                {
                    bridge.call_method1("_send_mission_ack", (MAV_MISSION_OPERATION_CANCELLED, mission_type))?;
                    logger(py)?.call_method1("info", ("mission upload cancelled: an abort command is waiting",))?;
                    return Ok(-3);
                }
                let now = self.monotonic(py)?;
                if now > deadline {
                    return Ok(-1);
                }
                let last = self.state().last_request;
                if last <= started && count_sends < tries && now - count_sent_at > count_retry_s {
                    logger(py)?.call_method1("info", ("no item requested yet, sending MISSION_COUNT again",))?;
                    if !send_count()? {
                        return Ok(-2);
                    }
                    count_sent_at = now;
                    count_sends += 1;
                    continue;
                }
                if now - last > self.tunable(py, "MISSION_UPLOAD_QUIET_S")? {
                    return Ok(-1);
                }
            }
        })();
        {
            let mut st = self.state();
            if st.pending_ack.as_ref().is_some_and(|p| p.bind(py).is(&pending)) {
                st.pending_ack = None;
            }
            st.items = None;
            st.upload_type = 0;
        }
        outcome
    }

    /// Read the mission stored on the vehicle back as a plan. The caller holds
    /// the operation lock.
    fn download_mission(&self, bridge: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        let py = bridge.py();
        set_error(bridge, "")?;
        if !truthy(bridge, "_connection_ready")? {
            Self::command_failure(bridge, "Mission download", -2)?;
            return Ok(py.None());
        }
        let download = Arc::new(Download::new());
        let target = Arc::clone(&download);
        let watch = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, _kwargs| -> PyResult<()> {
                let entry = args.get_item(0)?;
                if entry.call_method1("get", ("name",))?.eq("STATUSTEXT")? {
                    let raw = entry.call_method1("get", ("text",))?;
                    let raw = if raw.is_truthy()? {
                        raw
                    } else {
                        PyString::new(args.py(), "").into_any()
                    };
                    let text: String = raw.str()?.to_string().to_lowercase();
                    if text.contains(UNEXPECTED_REQUEST) {
                        lock(&target.state).drops += 1;
                        target.signal.set();
                    }
                }
                Ok(())
            },
        )?;
        let add = bridge.getattr("add_console_sub").ok().filter(|a| a.is_callable());
        let remove = bridge.getattr("remove_console_sub").ok().filter(|r| r.is_callable());
        if let Some(add) = &add {
            add.call1((&watch,))?;
        }
        self.state().download = Some(Arc::clone(&download));
        let items = self.run_download(bridge, &download);
        {
            let mut st = self.state();
            if st.download.as_ref().is_some_and(|d| Arc::ptr_eq(d, &download)) {
                st.download = None;
            }
        }
        if let Some(remove) = &remove {
            remove.call1((&watch,))?;
        }
        let Some(items) = items? else {
            return Ok(py.None());
        };
        let known = bridge.call_method0("_home_lat_lon")?;
        let known: Option<(f64, f64)> = if known.is_none() { None } else { Some(known.extract()?) };
        let home = match known {
            Some((lat, lon)) => {
                let d = PyDict::new(py);
                d.set_item("lat", lat)?;
                d.set_item("lon", lon)?;
                d.into_any().unbind()
            }
            None => py.None(),
        };
        let items_list = PyList::new(py, items.iter().map(|i| i.clone_ref(py)))?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("home", home)?;
        kwargs.set_item("home_alt_amsl", bridge.getattr("_home_alt_amsl")?)?;
        let converted = self
            .mission_plan(py)?
            .call_method("items_to_plan", (&items_list,), Some(&kwargs))?;
        Self::bridge_note(bridge, &converted.get_item("plan_index")?)?;
        console(
            bridge,
            "MISSION",
            &format!("Read {} mission item(s) from the vehicle", items.len()),
            "success",
        )?;
        let out = PyDict::new(py);
        out.call_method1("update", (&converted,))?;
        out.set_item("items", &items_list)?;
        out.set_item("count", items.len())?;
        out.set_item("revision", self.state().revision)?;
        Ok(out.into_any().unbind())
    }

    // ---- items ----

    /// One MISSION_ITEM_INT / MISSION_ITEM send-spec.
    #[staticmethod]
    #[pyo3(signature = (seq, command, lat, lon, alt, p1, p2, p3, p4, frame=None))]
    #[allow(clippy::too_many_arguments)]
    fn build_item_spec<'py>(
        py: Python<'py>,
        seq: i64,
        command: &Bound<'py, PyAny>,
        lat: &Bound<'py, PyAny>,
        lon: &Bound<'py, PyAny>,
        alt: &Bound<'py, PyAny>,
        p1: &Bound<'py, PyAny>,
        p2: &Bound<'py, PyAny>,
        p3: &Bound<'py, PyAny>,
        p4: &Bound<'py, PyAny>,
        frame: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let frame = match frame {
            Some(f) if !f.is_none() => py_int(f)?,
            _ => MAV_FRAME_GLOBAL_RELATIVE_ALT,
        };
        let lat_f = py_float(lat)?;
        let lon_f = py_float(lon)?;
        let lat_x = lat_f * 1e7;
        let lon_x = lon_f * 1e7;
        if !lat_x.is_finite() || !lon_x.is_finite() {
            // round() of a NaN or infinity raises in Python, and so here.
            let bad = if lat_x.is_finite() { lon_x } else { lat_x };
            return Err(if bad.is_nan() {
                PyValueError::new_err("cannot convert float NaN to integer")
            } else {
                pyo3::exceptions::PyOverflowError::new_err("cannot convert float infinity to integer")
            });
        }
        let d = PyDict::new(py);
        d.set_item("frame", frame)?;
        d.set_item("command", command)?;
        d.set_item("current", if seq == 0 { 1 } else { 0 })?;
        d.set_item("autocontinue", 1)?;
        d.set_item("param1", py_float(p1)?)?;
        d.set_item("param2", py_float(p2)?)?;
        d.set_item("param3", py_float(p3)?)?;
        d.set_item("param4", py_float(p4)?)?;
        d.set_item("x_int", lat_x.round_ties_even() as i64)?;
        d.set_item("y_int", lon_x.round_ties_even() as i64)?;
        d.set_item("x_f", lat_f)?;
        d.set_item("y_f", lon_f)?;
        d.set_item("z", py_float(alt)?)?;
        Ok(d)
    }

    /// ArduPilot keeps home in slot 0: prepend a placeholder there.
    fn with_home_slot<'py>(
        &self,
        bridge: &Bound<'py, PyAny>,
        specs: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = bridge.py();
        if !specs.is_truthy()?
            || !bridge
                .getattr("_dialect")?
                .getattr("mission_seq0_is_home")?
                .is_truthy()?
        {
            return Ok(specs.clone());
        }
        let home = snapshot(bridge)?.call_method1("get", ("home",))?;
        let home = if home.is_truthy()? {
            home
        } else {
            PyList::new(py, [0.0, 0.0])?.into_any()
        };
        let pair =
            (|| -> PyResult<(f64, f64)> { Ok((py_float(&home.get_item(0)?)?, py_float(&home.get_item(1)?)?)) })();
        let (home_lon, home_lat) = match pair {
            Ok(p) => p,
            Err(err) if caught_by(py, &err, &["TypeError", "ValueError", "IndexError"]) => (0.0, 0.0),
            Err(err) => return Err(err),
        };
        let alt = bridge.getattr("_home_alt_amsl")?;
        let home_alt = if alt.is_truthy()? { py_float(&alt)? } else { 0.0 };
        let f = |v: f64| PyFloat::new(py, v).into_any();
        let placeholder = Self::build_item_spec(
            py,
            0,
            &obj(py, MAV_CMD_NAV_WAYPOINT).into_bound(py),
            &f(home_lat),
            &f(home_lon),
            &f(home_alt),
            &f(0.0),
            &f(0.0),
            &f(0.0),
            &f(0.0),
            Some(&obj(py, MAV_FRAME_GLOBAL).into_bound(py)),
        )?;
        let out = PyList::new(py, [placeholder.into_any()])?;
        for item in specs.try_iter()? {
            let shifted = item?.downcast_into::<PyDict>()?.copy()?;
            shifted.set_item("current", 0)?;
            out.append(shifted)?;
        }
        Ok(out.into_any())
    }

    /// MAV_MISSION_RESULT to MAV_RESULT, for the failure message.
    #[staticmethod]
    fn ack_to_result(ack_type: i64) -> i64 {
        match ack_type {
            MAV_MISSION_ACCEPTED => MAV_RESULT_ACCEPTED,
            MAV_MISSION_DENIED => MAV_RESULT_DENIED,
            2 | 3 => MAV_RESULT_UNSUPPORTED,
            _ => MAV_RESULT_FAILED,
        }
    }

    /// The operator's points, checked: [(lat, lon, alt_agl)] or None with the
    /// reason set.
    fn validate_points(&self, bridge: &Bound<'_, PyAny>, points: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        let py = bridge.py();
        let env = self.env(py);
        let Ok(list) = points.downcast::<PyList>() else {
            set_error(bridge, "points must be a non-empty list")?;
            return Ok(py.None());
        };
        if list.is_empty() {
            set_error(bridge, "points must be a non-empty list")?;
            return Ok(py.None());
        }
        let max_points: i64 = env.getattr("FLY_TO_MAX_POINTS")?.extract()?;
        if list.len() as i64 > max_points {
            set_error(
                bridge,
                &format!("too many points ({}); the maximum is {max_points}", list.len()),
            )?;
            return Ok(py.None());
        }
        let ground = env
            .getattr("is_ground_vehicle")?
            .call1((bridge.getattr("_mav_type_id")?,))?
            .is_truthy()?;
        let alt_min: f64 = env.getattr("TAKEOFF_ALTITUDE_MIN_M")?.extract()?;
        let alt_max: f64 = env.getattr("TAKEOFF_ALTITUDE_MAX_M")?.extract()?;
        let number = |v: Option<Bound<'_, PyAny>>| -> PyResult<Option<f64>> {
            match v {
                Some(v) if is_number(&v) => Ok(Some(py_float(&v)?)),
                _ => Ok(None),
            }
        };
        let cleaned = PyList::empty(py);
        for (idx, entry) in list.iter().enumerate() {
            let Ok(entry) = entry.downcast::<PyDict>() else {
                set_error(bridge, &format!("point {idx} must be an object"))?;
                return Ok(py.None());
            };
            let lat = number(entry.get_item("lat")?)?;
            let lon = number(entry.get_item("lon")?)?;
            let raw_alt = entry.get_item("alt_agl")?;
            let mut alt = number(raw_alt.clone())?;
            if ground && raw_alt.as_ref().is_none_or(|a| a.is_none()) {
                alt = Some(0.0);
            }
            let Some(lat) = lat else {
                set_error(bridge, &format!("point {idx} lat must be a number"))?;
                return Ok(py.None());
            };
            if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
                set_error(bridge, &format!("point {idx} lat out of range"))?;
                return Ok(py.None());
            }
            let Some(lon) = lon else {
                set_error(bridge, &format!("point {idx} lon must be a number"))?;
                return Ok(py.None());
            };
            if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
                set_error(bridge, &format!("point {idx} lon out of range"))?;
                return Ok(py.None());
            }
            let Some(mut alt) = alt else {
                set_error(bridge, &format!("point {idx} alt_agl must be a number"))?;
                return Ok(py.None());
            };
            if ground {
                alt = 0.0;
            } else if !alt.is_finite() || !(alt_min..=alt_max).contains(&alt) {
                set_error(
                    bridge,
                    &format!("point {idx} alt_agl must be between {alt_min:.0} and {alt_max:.0} m"),
                )?;
                return Ok(py.None());
            }
            cleaned.append(PyTuple::new(py, [lat, lon, alt])?)?;
        }
        Ok(cleaned.into_any().unbind())
    }

    /// Where a prepended takeoff item goes: the vehicle, else home, else the
    /// first point. Never 0/0.
    fn takeoff_item_position(&self, bridge: &Bound<'_, PyAny>, first: &Bound<'_, PyAny>) -> PyResult<(f64, f64)> {
        let py = bridge.py();
        let snap = snapshot(bridge)?;
        for key in ["position", "home"] {
            let value = snap.call_method1("get", (key,))?;
            let value = if value.is_truthy()? {
                value
            } else {
                PyList::empty(py).into_any()
            };
            let pair =
                (|| -> PyResult<(f64, f64)> { Ok((py_float(&value.get_item(0)?)?, py_float(&value.get_item(1)?)?)) })();
            let (lon, lat) = match pair {
                Ok(p) => p,
                Err(err) if caught_by(py, &err, &["TypeError", "ValueError", "IndexError"]) => continue,
                Err(err) => return Err(err),
            };
            if (lat, lon) == (0.0, 0.0) {
                continue;
            }
            if lat.is_finite() && lon.is_finite() && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) {
                return Ok((lat, lon));
            }
        }
        Ok((first.get_item(0)?.extract()?, first.get_item(1)?.extract()?))
    }

    /// Why this stack will not take *items*, or "".
    fn refusal(&self, bridge: &Bound<'_, PyAny>, items: &Bound<'_, PyAny>) -> PyResult<String> {
        let py = bridge.py();
        let refusals = bridge
            .getattr("_dialect")?
            .call_method1("mission_refusals", (bridge.getattr("_mav_type_id")?,))?;
        for (n, entry) in items.try_iter()?.enumerate() {
            let entry = entry?;
            let command = match entry.call_method1("get", ("command",)).and_then(|c| py_int(&c)) {
                Ok(c) => c,
                Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => continue,
                Err(err) => return Err(err),
            };
            let reason = refusals.call_method1("get", (command,))?;
            if !reason.is_truthy()? {
                continue;
            }
            let item = entry.call_method1("get", ("item",))?;
            let where_ = if item.is_instance_of::<PyInt>() {
                format!("item {}", py_int(&item)? + 1)
            } else {
                format!("item {}", n + 1)
            };
            let label: String = self
                .mission_plan(py)?
                .call_method1("label_of_command", (command,))?
                .str()?
                .to_string();
            return Ok(format!(
                "Mission upload refused: {label} ({where_}). {}.",
                reason.str()?
            ));
        }
        Ok(String::new())
    }

    /// Upload a planned mission; does not start it. The caller holds the
    /// operation lock and has lowered the plan.
    fn upload_plan(&self, bridge: &Bound<'_, PyAny>, items: &Bound<'_, PyAny>, drawn: i64) -> PyResult<bool> {
        let py = bridge.py();
        let max_points: i64 = self.env(py).getattr("FLY_TO_MAX_POINTS")?.extract()?;
        let n = items.len()? as i64;
        if n > max_points {
            let circles = if n > drawn {
                " This autopilot flies a circle as waypoints around it: give the circles fewer turns."
            } else {
                ""
            };
            set_error(
                bridge,
                &format!("too many mission items ({n}); the maximum is {max_points}.{circles}"),
            )?;
            return Ok(false);
        }
        if !truthy(bridge, "_connection_ready")? {
            return Self::command_failure(bridge, "Mission upload", -2);
        }
        let refusal: String = bridge.call_method1("_mission_refusal", (items,))?.extract()?;
        if !refusal.is_empty() {
            set_error(bridge, &refusal)?;
            console(bridge, "MISSION", &refusal, "error")?;
            return Ok(false);
        }
        let positionless = self.mission_plan(py)?.getattr("POSITIONLESS_COMMANDS")?;
        let do_frame = self.env(py).getattr("MISSION_DO_FRAME")?;
        let specs = PyList::empty(py);
        for (seq, entry) in items.try_iter()?.enumerate() {
            let entry = entry?;
            let params = entry.call_method1("get", ("params",))?;
            let mut values: Vec<Bound<'_, PyAny>> = if params.is_truthy()? {
                params.try_iter()?.collect::<PyResult<_>>()?
            } else {
                (0..4).map(|_| PyFloat::new(py, 0.0).into_any()).collect()
            };
            while values.len() < 4 {
                values.push(PyFloat::new(py, 0.0).into_any());
            }
            let command = py_int(&entry.get_item("command")?)?;
            let frame = if positionless.contains(command)? {
                Some(do_frame.clone())
            } else {
                None
            };
            let get = |key: &str| -> PyResult<Bound<'_, PyAny>> {
                Ok(PyFloat::new(py, py_float(&entry.call_method1("get", (key, 0.0))?)?).into_any())
            };
            let kwargs = PyDict::new(py);
            kwargs.set_item("frame", frame.unwrap_or_else(|| py.None().into_bound(py)))?;
            specs.append(bridge.call_method(
                "_build_mission_item_spec",
                (
                    seq as i64,
                    command,
                    get("lat")?,
                    get("lon")?,
                    get("alt")?,
                    &values[0],
                    &values[1],
                    &values[2],
                    &values[3],
                ),
                Some(&kwargs),
            )?)?;
        }
        console(
            bridge,
            "MISSION",
            &format!("Uploading {} mission item(s) …", specs.len()),
            "info",
        )?;
        let wire = bridge.call_method1("_with_mission_home_slot", (&specs,))?;
        let reported = bridge
            .getattr("_dialect")?
            .getattr("mission_validity_reported")?
            .is_truthy()?;
        let listening = if reported {
            Some(self.listen(bridge, wire.len()? as i64)?)
        } else {
            None
        };
        let result = (|| -> PyResult<Result<String, ()>> {
            let ack = Self::bridge_upload(bridge, &wire, 0)?;
            if ack != MAV_MISSION_ACCEPTED {
                Self::command_failure(bridge, "Mission upload", Self::bridge_ack_result(bridge, ack)?)?;
                return Ok(Err(()));
            }
            let mut index: Vec<Option<i64>> = Vec::new();
            let mut all_ints = true;
            for entry in items.try_iter()? {
                let item = entry?.call_method1("get", ("item",))?;
                if item.is_instance_of::<PyInt>() {
                    index.push(Some(item.extract()?));
                } else {
                    all_ints = false;
                }
            }
            let index = if all_ints {
                PyList::new(py, index)?.into_any()
            } else {
                py.None().into_bound(py)
            };
            Self::bridge_note(bridge, &index)?;
            Ok(Ok(self.await_verdict(bridge, listening.as_ref().map(|(v, _)| v))?))
        })();
        if let Some((verdict, collect)) = &listening {
            self.unlisten(bridge, verdict, collect)?;
        }
        let rejection = match result? {
            Ok(r) => r,
            Err(()) => return Ok(false),
        };
        if !rejection.is_empty() {
            set_error(bridge, &rejection)?;
            console(bridge, "MISSION", &rejection, "error")?;
            return Ok(false);
        }
        console(bridge, "MISSION", "Mission accepted by the vehicle", "success")?;
        Ok(true)
    }

    /// Wipe the mission on the vehicle: a zero-item upload. The caller holds
    /// the operation lock.
    fn clear_mission(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        if !truthy(bridge, "_connection_ready")? {
            return Self::command_failure(bridge, "Mission clear", -2);
        }
        let ack = Self::bridge_upload(bridge, PyList::empty(py).as_any(), 0)?;
        if ack != MAV_MISSION_ACCEPTED {
            return Self::command_failure(bridge, "Mission clear", Self::bridge_ack_result(bridge, ack)?);
        }
        Self::bridge_note(bridge, PyList::empty(py).as_any())?;
        console(bridge, "MISSION", "Mission cleared", "success")?;
        Ok(true)
    }

    /// Put an inclusion fence on the vehicle. The caller holds the operation
    /// lock.
    fn upload_fence(&self, bridge: &Bound<'_, PyAny>, polygon: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let geofence = self.env(py).getattr("geofence")?;
        let min_vertices: i64 = geofence.getattr("MIN_VERTICES")?.extract()?;
        let ok_shape = polygon
            .downcast::<PyList>()
            .is_ok_and(|l| l.len() as i64 >= min_vertices);
        if !ok_shape {
            set_error(bridge, &format!("an area needs at least {min_vertices} corners"))?;
            return Ok(false);
        }
        if !truthy(bridge, "_connection_ready")? {
            return Self::command_failure(bridge, "Geofence upload", -2);
        }
        let specs = PyList::empty(py);
        for (seq, entry) in geofence
            .call_method1("fence_items", (polygon,))?
            .try_iter()?
            .enumerate()
        {
            let entry = entry?;
            let params: Vec<Bound<'_, PyAny>> = entry.get_item("params")?.try_iter()?.collect::<PyResult<_>>()?;
            let command = py_int(&entry.get_item("command")?)?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("frame", MAV_FRAME_GLOBAL)?;
            let spec = bridge.call_method(
                "_build_mission_item_spec",
                (
                    seq as i64,
                    command,
                    entry.get_item("lat")?,
                    entry.get_item("lon")?,
                    0.0,
                    &params[0],
                    &params[1],
                    &params[2],
                    &params[3],
                ),
                Some(&kwargs),
            )?;
            spec.set_item("current", 0)?;
            specs.append(spec)?;
        }
        console(
            bridge,
            "GEOFENCE",
            &format!("Uploading a geofence with {} corners …", specs.len()),
            "info",
        )?;
        let fence_type: i64 = geofence.getattr("MAV_MISSION_TYPE_FENCE")?.extract()?;
        let ack = Self::bridge_upload(bridge, specs.as_any(), fence_type)?;
        if ack != MAV_MISSION_ACCEPTED {
            return Self::command_failure(bridge, "Geofence upload", Self::bridge_ack_result(bridge, ack)?);
        }
        let copy = PyList::empty(py);
        for point in polygon.try_iter()? {
            copy.append(PyList::new(py, point?.try_iter()?.collect::<PyResult<Vec<_>>>()?)?)?;
        }
        self.state().vehicle_fence = Some(copy.into_any().unbind());
        console(bridge, "GEOFENCE", "Geofence accepted by the vehicle", "success")?;
        Ok(true)
    }

    /// Remove every fence the vehicle holds. The caller holds the operation
    /// lock.
    fn clear_fence(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        if !truthy(bridge, "_connection_ready")? {
            return Self::command_failure(bridge, "Geofence clear", -2);
        }
        let fence_type: i64 = self
            .env(py)
            .getattr("geofence")?
            .getattr("MAV_MISSION_TYPE_FENCE")?
            .extract()?;
        let ack = Self::bridge_upload(bridge, PyList::empty(py).as_any(), fence_type)?;
        if ack != MAV_MISSION_ACCEPTED {
            return Self::command_failure(bridge, "Geofence clear", Self::bridge_ack_result(bridge, ack)?);
        }
        self.state().vehicle_fence = Some(PyList::empty(py).into_any().unbind());
        console(bridge, "GEOFENCE", "Geofence removed from the vehicle", "success")?;
        Ok(true)
    }

    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.env)?;
        if let Ok(st) = self.st.try_lock() {
            for o in [&st.items, &st.vehicle_fence, &st.pending_ack].into_iter().flatten() {
                visit.call(o)?;
            }
            if let Some(v) = &st.verdict {
                visit.call(v)?;
            }
        }
        Ok(())
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.missions")?;
    m.add_class::<MissionEngine>()?;
    m.add_class::<MissionVerdict>()?;
    m.add("MAV_MISSION_OPERATION_CANCELLED", MAV_MISSION_OPERATION_CANCELLED)?;
    parent.add_submodule(&m)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("corvus_link.missions", &m)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mission_results_map_to_command_results() {
        assert_eq!(MissionEngine::ack_to_result(MAV_MISSION_ACCEPTED), MAV_RESULT_ACCEPTED);
        assert_eq!(MissionEngine::ack_to_result(MAV_MISSION_DENIED), MAV_RESULT_DENIED);
        assert_eq!(MissionEngine::ack_to_result(2), MAV_RESULT_UNSUPPORTED);
        assert_eq!(MissionEngine::ack_to_result(3), MAV_RESULT_UNSUPPORTED);
        assert_eq!(MissionEngine::ack_to_result(4), MAV_RESULT_FAILED);
    }

    #[test]
    fn a_signal_wakes_a_waiter_and_times_out_without_one() {
        let signal = Arc::new(Signal::new());
        assert!(!signal.wait(0.01));
        let other = Arc::clone(&signal);
        let waker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            other.set();
        });
        assert!(signal.wait(2.0));
        waker.join().unwrap();
        signal.clear();
        assert!(!signal.wait(0.0));
    }
}
