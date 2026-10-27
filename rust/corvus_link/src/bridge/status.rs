//! What the vehicle says in words: STATUSTEXT reassembled from its MAVLink 2
//! chunks and published to the console and the warning board, the events it
//! reports (the kill switch above all), and why it will not arm.
//!
//! SYS_STATUS says whether the vehicle would arm, not why; the why is text
//! ("Preflight Fail:" on PX4, "PreArm:" on ArduPilot), and PX4 sends it only
//! when a result changes or when asked, so the station asks
//! (MAV_CMD_RUN_PREARM_CHECKS) when the vehicle says "not ready" without a
//! reason. The kill switch is latched from its own line, from a check that
//! names it, and released by its line, by a ready verdict well after it, or
//! by a whole report asked for since that does not name it.
//!
//! Which line means what is the dialect's (`corvus.autopilot`). The state
//! stays on the bridge, where the rest of it reads it; constants and the
//! clock come from ``corvus.mavlink_bridge``.

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyInt, PyList, PyModule, PySet, PyString, PyTuple};

use crate::bridge::receive::within;
use crate::protocol::host::{caught_by, logger, obj};
use crate::wire::defs::constant_or;

fn int_of<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    value.py().get_type::<PyInt>().call1((value,))
}

/// `getattr(o, name, fallback)`.
fn attr_or<'py>(o: &Bound<'py, PyAny>, name: &str, fallback: PyObject) -> PyResult<Bound<'py, PyAny>> {
    match o.getattr(name) {
        Ok(v) => Ok(v),
        Err(err) if caught_by(o.py(), &err, &["AttributeError"]) => Ok(fallback.into_bound(o.py())),
        Err(err) => Err(err),
    }
}

/// `getattr(msg, method, lambda: 0)()`.
fn source_call<'py>(msg: &Bound<'py, PyAny>, method: &str) -> PyResult<Bound<'py, PyAny>> {
    match msg.getattr(method) {
        Ok(f) => f.call0(),
        Err(err) if caught_by(msg.py(), &err, &["AttributeError"]) => Ok(obj(msg.py(), 0).into_bound(msg.py())),
        Err(err) => Err(err),
    }
}

fn store<'py>(bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    bridge.getattr("_store")
}

fn snapshot<'py>(bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    store(bridge)?.call_method0("get_snapshot")
}

fn snap_flag(snap: &Bound<'_, PyAny>, key: &str) -> PyResult<bool> {
    snap.call_method1("get", (key,))?.is_truthy()
}

fn update(bridge: &Bound<'_, PyAny>, fields: Vec<(&str, PyObject)>) -> PyResult<()> {
    let kwargs = PyDict::new(bridge.py());
    for (k, v) in fields {
        kwargs.set_item(k, v)?;
    }
    store(bridge)?.call_method("update", (), Some(&kwargs))?;
    Ok(())
}

fn resolve(bridge: &Bound<'_, PyAny>, event: &str) -> PyResult<()> {
    store(bridge)?.call_method1("resolve_warnings", (PySet::new(bridge.py(), [event])?,))?;
    Ok(())
}

fn warn(bridge: &Bound<'_, PyAny>, text: &Bound<'_, PyAny>, level: &str, event: PyObject) -> PyResult<()> {
    let kwargs = PyDict::new(bridge.py());
    kwargs.set_item("event", event)?;
    bridge.call_method("_store_update_warning", (text, level), Some(&kwargs))?;
    Ok(())
}

fn float_attr(bridge: &Bound<'_, PyAny>, name: &str) -> PyResult<f64> {
    bridge.getattr(name)?.extract()
}

/// The words of one STATUSTEXT chunk, and its raw bytes: bytes are read up
/// to their NUL as UTF-8, anything else as its `str()`.
fn chunk_text<'py>(py: Python<'py>, raw: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Vec<u8>)> {
    if raw.is_instance_of::<PyBytes>() {
        let bytes: Vec<u8> = raw.extract()?;
        let head = bytes.split(|b| *b == 0).next().unwrap_or(&[]).to_vec();
        let text = PyBytes::new(py, &head).call_method1("decode", ("utf-8", "replace"))?;
        return Ok((text, bytes));
    }
    let as_text = raw.str()?;
    let bytes: Vec<u8> = as_text.call_method1("encode", ("utf-8", "replace"))?.extract()?;
    let text = as_text.call_method1("split", ("\0", 1))?.get_item(0)?;
    Ok((text, bytes))
}

/// The status reporting of one bridge.
#[pyclass(name = "StatusEngine", module = "corvus_link.bridge", frozen)]
pub struct StatusEngine {
    env: PyObject,
}

impl StatusEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn monotonic(&self, py: Python<'_>) -> PyResult<f64> {
        self.env(py).getattr("time")?.call_method0("monotonic")?.extract()
    }

    fn tunable(&self, py: Python<'_>, name: &str) -> PyResult<f64> {
        self.env(py).getattr(name)?.extract()
    }
}

#[pymethods]
impl StatusEngine {
    /// *env* is ``corvus.mavlink_bridge``.
    #[new]
    fn new(env: PyObject) -> Self {
        StatusEngine { env }
    }

    /// One STATUSTEXT: published at once when it is whole (id 0), else held
    /// until every chunk up to the last has arrived. The least recently
    /// touched partial makes room when too many are open.
    fn handle_statustext(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let now = self.monotonic(py)?;
        bridge.call_method1("_cleanup_statustext_chunks", (now,))?;
        let (text, raw) = chunk_text(py, &msg.getattr("text")?)?;
        let severity = int_of(&msg.getattr("severity")?)?;
        let chunk_id = int_of(&attr_or(msg, "id", obj(py, 0))?)?;
        let chunk_seq = int_of(&attr_or(msg, "chunk_seq", obj(py, 0))?)?;
        let chunk_bytes: usize = self.env(py).getattr("STATUSTEXT_CHUNK_BYTES")?.extract()?;
        let is_final = raw.contains(&0) || raw.len() < chunk_bytes;
        if chunk_id.eq(0)? {
            bridge.call_method1("_publish_statustext", (&text, &severity))?;
            return Ok(());
        }
        let source_system = source_call(msg, "get_srcSystem")?;
        let source_component = source_call(msg, "get_srcComponent")?;
        let key = PyTuple::new(py, [int_of(&source_system)?, int_of(&source_component)?, chunk_id])?;
        let chunks = bridge.getattr("_statustext_chunks")?;
        let open = chunks.downcast::<PyDict>()?;
        let max_partials: usize = self.env(py).getattr("STATUSTEXT_MAX_PARTIALS")?.extract()?;
        if !open.contains(&key)? && open.len() >= max_partials {
            let mut oldest: Option<(Bound<'_, PyAny>, f64)> = None;
            for (k, partial) in open.iter() {
                let updated: f64 = py
                    .get_type::<pyo3::types::PyFloat>()
                    .call1((partial.get_item("updated")?,))?
                    .extract()?;
                if oldest.as_ref().is_none_or(|(_, at)| updated < *at) {
                    oldest = Some((k, updated));
                }
            }
            if let Some((k, _)) = oldest {
                chunks.call_method1("pop", (k, py.None()))?;
            }
        }
        let fresh = PyDict::new(py);
        fresh.set_item("parts", PyDict::new(py))?;
        fresh.set_item("final_seq", py.None())?;
        fresh.set_item("severity", &severity)?;
        fresh.set_item("updated", now)?;
        let partial = chunks.call_method1("setdefault", (&key, fresh))?;
        partial.get_item("parts")?.set_item(&chunk_seq, &text)?;
        let held = int_of(&partial.get_item("severity")?)?;
        partial.set_item("severity", if severity.lt(&held)? { &severity } else { &held })?;
        partial.set_item("updated", now)?;
        if is_final {
            partial.set_item("final_seq", &chunk_seq)?;
        }
        let final_seq = partial.get_item("final_seq")?;
        if final_seq.is_none() {
            return Ok(());
        }
        let last: i64 = final_seq.extract()?;
        let parts = partial.get_item("parts")?;
        let mut pieces: Vec<Bound<'_, PyAny>> = Vec::new();
        for seq in 0..=last {
            match parts.call_method1("get", (seq,))? {
                piece if piece.is_none() && !parts.contains(seq)? => return Ok(()),
                piece => pieces.push(piece),
            }
        }
        let complete = PyString::new(py, "").call_method1("join", (PyList::new(py, pieces)?,))?;
        let complete_severity = int_of(&partial.get_item("severity")?)?;
        chunks.call_method1("pop", (&key, py.None()))?;
        bridge.call_method1("_publish_statustext", (complete, complete_severity))?;
        Ok(())
    }

    /// Drop partial STATUSTEXTs whose next chunk is overdue.
    #[pyo3(signature = (bridge, now=None))]
    fn cleanup_statustext_chunks(&self, bridge: &Bound<'_, PyAny>, now: Option<f64>) -> PyResult<()> {
        let py = bridge.py();
        let current = match now {
            Some(t) => t,
            None => self.monotonic(py)?,
        };
        let timeout = self.tunable(py, "STATUSTEXT_CHUNK_TIMEOUT_S")?;
        let chunks = bridge.getattr("_statustext_chunks")?;
        let mut stale = Vec::new();
        for (key, partial) in chunks.downcast::<PyDict>()?.iter() {
            let updated: f64 = py
                .get_type::<pyo3::types::PyFloat>()
                .call1((partial.get_item("updated")?,))?
                .extract()?;
            if current - updated > timeout {
                stale.push(key);
            }
        }
        for key in stale {
            chunks.call_method1("pop", (key, py.None()))?;
        }
        Ok(())
    }

    /// One whole line: to the console, then as an event, a preflight reason
    /// (a kill switch named by the check it fails is latched from it), or a
    /// warning unless it is routine chatter.
    fn publish_statustext(
        &self,
        bridge: &Bound<'_, PyAny>,
        text: &Bound<'_, PyAny>,
        severity: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = bridge.py();
        // PX4 ends a line that doubles one of its events with a tab.
        let text = text.call_method0("rstrip")?;
        let level = if severity.le(3)? {
            "critical"
        } else if severity.le(5)? {
            "warning"
        } else {
            "info"
        };
        // The string itself, not a Rust copy: a lone surrogate reaches the
        // console as Python had it.
        bridge.call_method1("_console_publish", ("STATUSTEXT", &text, level))?;
        let dialect = bridge.getattr("_dialect")?;
        let event = dialect.call_method1("vehicle_event", (&text,))?;
        if !event.is_none() {
            bridge.call_method1("_note_vehicle_event", (event, level))?;
            return Ok(());
        }
        let reason = dialect.call_method1("prearm_failure", (&text,))?;
        let has_reason = reason.is_truthy()?;
        if has_reason {
            bridge.call_method1("_note_prearm_failure", (&reason,))?;
            let kill = dialect.call_method1("prearm_event", (&reason,))?;
            if !kill.is_none() {
                bridge.setattr("_prearm_report_kill", true)?;
                bridge.setattr("_kill_evidence_at", self.monotonic(py)?)?;
                if !snap_flag(&snapshot(bridge)?, "kill_switch")? {
                    bridge.call_method1("_note_vehicle_event", (kill, level))?;
                }
                return Ok(());
            }
        }
        if level != "info" || !dialect.call_method1("is_routine_statustext", (&text,))?.is_truthy()? {
            let event = if has_reason { obj(py, "prearm") } else { py.None() };
            warn(bridge, &text, level, event)?;
        }
        Ok(())
    }

    /// Latch the kill switch a line reports and put the line on the board,
    /// at the worse of the line's level and the event's.
    fn note_vehicle_event(
        &self,
        bridge: &Bound<'_, PyAny>,
        event: &Bound<'_, PyAny>,
        level: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = bridge.py();
        let kind = event.getattr("kind")?;
        if kind.eq("kill")? {
            bridge.setattr("_kill_evidence_at", self.monotonic(py)?)?;
            update(bridge, vec![("kill_switch", obj(py, true))])?;
        }
        let rank = self.env(py).getattr("_LEVEL_RANK")?;
        let event_level = event.getattr("level")?;
        let own_rank = rank.get_item(level)?;
        let worst = if rank.get_item(&event_level)?.gt(own_rank)? {
            event_level
        } else {
            level.clone()
        };
        let kwargs = PyDict::new(py);
        kwargs.set_item("event", &kind)?;
        bridge.call_method(
            "_store_update_warning",
            (event.getattr("message")?, worst),
            Some(&kwargs),
        )?;
        if kind.eq("unkill")? {
            bridge.call_method0("_release_kill_switch")?;
        }
        Ok(())
    }

    /// The switch is off: clear it, and the lockdown blamed on it, now rather
    /// than at the next 1 Hz heartbeat.
    fn release_kill_switch(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let mut changes = vec![("kill_switch", obj(py, false))];
        if snap_flag(&snapshot(bridge)?, "kill_switch")? {
            changes.push(("flight_termination", obj(py, false)));
        }
        update(bridge, changes)?;
        resolve(bridge, "kill")
    }

    /// One reason the vehicle will not arm; a line after a gap starts the
    /// next report.
    fn note_prearm_failure(&self, bridge: &Bound<'_, PyAny>, reason: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let now = self.monotonic(py)?;
        if now - float_attr(bridge, "_prearm_reason_at")? > self.tunable(py, "PREARM_REPORT_GAP_S")? {
            bridge.setattr("_prearm_reasons", PyList::empty(py))?;
            bridge.setattr("_prearm_report_started_at", now)?;
            bridge.setattr("_prearm_report_kill", false)?;
        }
        bridge.setattr("_prearm_reason_at", now)?;
        let reasons = bridge.getattr("_prearm_reasons")?;
        let most: usize = self.env(py).getattr("PREARM_REASONS_MAX")?.extract()?;
        if reasons.contains(reason)? || reasons.len()? >= most {
            return Ok(());
        }
        reasons.call_method1("append", (reason,))?;
        update(
            bridge,
            vec![("prearm_reasons", py.get_type::<PyList>().call1((&reasons,))?.unbind())],
        )
    }

    fn clear_prearm_reasons(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        bridge.setattr("_prearm_reasons", PyList::empty(py))?;
        bridge.setattr("_prearm_reason_at", 0.0)?;
        update(bridge, vec![("prearm_reasons", PyList::empty(py).into_any().unbind())])
    }

    /// React to SYS_STATUS's verdict: clear the reasons (and, disarmed, the
    /// kill switch once the verdict cannot predate it), or go and get them.
    fn note_prearm_state(&self, bridge: &Bound<'_, PyAny>, prearm: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let now = self.monotonic(py)?;
        let previous = bridge.getattr("_prearm_last")?;
        bridge.setattr("_prearm_last", prearm)?;
        let snap = snapshot(bridge)?;
        let yes = PyBool::new(py, true);
        let no = PyBool::new(py, false);
        if prearm.is(&*yes) {
            if bridge.getattr("_prearm_reasons")?.is_truthy()? {
                bridge.call_method0("_clear_prearm_reasons")?;
            }
            resolve(bridge, "prearm")?;
            if !snap_flag(&snap, "armed")? {
                if snap_flag(&snap, "kill_switch")? {
                    if now - float_attr(bridge, "_kill_evidence_at")? >= self.tunable(py, "KILL_SETTLE_S")? {
                        bridge.call_method0("_release_kill_switch")?;
                    }
                } else {
                    resolve(bridge, "kill")?;
                }
            }
            return Ok(());
        }
        if !prearm.is(&*no) {
            return Ok(());
        }
        bridge.call_method1("_check_kill_released_by_report", (now, &snap))?;
        let requested_at = float_attr(bridge, "_prearm_requested_at")?;
        if previous.is(&*yes)
            && !snap_flag(&snap, "armed")?
            && now - requested_at >= self.tunable(py, "PREARM_CHANGE_REQUEST_MIN_S")?
        {
            bridge.setattr("_prearm_requested_at", now)?;
            bridge.call_method0("_request_prearm_report")?;
            return Ok(());
        }
        let latest = requested_at.max(float_attr(bridge, "_kill_evidence_at")?);
        if snap_flag(&snap, "kill_switch")?
            && !snap_flag(&snap, "armed")?
            && bridge
                .getattr("_dialect")?
                .getattr("prearm_report_complete")?
                .is_truthy()?
            && now - latest >= self.tunable(py, "KILL_RECHECK_S")?
        {
            bridge.setattr("_prearm_requested_at", now)?;
            bridge.call_method0("_request_prearm_report")?;
            return Ok(());
        }
        if bridge.getattr("_prearm_reasons")?.is_truthy()? {
            return Ok(());
        }
        if now - float_attr(bridge, "_prearm_requested_at")? < self.tunable(py, "PREARM_REPORT_REQUEST_EVERY_S")? {
            return Ok(());
        }
        bridge.setattr("_prearm_requested_at", now)?;
        bridge.call_method0("_request_prearm_report")?;
        Ok(())
    }

    /// Release the switch when a whole report, asked for after it was last
    /// reported engaged, does not name it. Only on a stack whose report names
    /// every failing check, only disarmed, and never on a report still
    /// arriving.
    fn check_kill_released_by_report(
        &self,
        bridge: &Bound<'_, PyAny>,
        now: f64,
        snap: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = bridge.py();
        if !snap_flag(snap, "kill_switch")? || snap_flag(snap, "armed")? {
            return Ok(());
        }
        if !bridge
            .getattr("_dialect")?
            .getattr("prearm_report_complete")?
            .is_truthy()?
        {
            return Ok(());
        }
        let started = float_attr(bridge, "_prearm_report_started_at")?;
        if bridge.getattr("_prearm_report_kill")?.is_truthy()? || started == 0.0 {
            return Ok(());
        }
        let requested = float_attr(bridge, "_prearm_requested_at")?;
        if requested <= float_attr(bridge, "_kill_evidence_at")? {
            return Ok(());
        }
        if started < requested {
            return Ok(());
        }
        if now - float_attr(bridge, "_prearm_reason_at")? <= self.tunable(py, "PREARM_REPORT_GAP_S")? {
            return Ok(());
        }
        bridge.call_method0("_release_kill_switch")?;
        Ok(())
    }

    /// Ask for the preflight report now, fire and forget: it runs on the
    /// receive thread, which cannot wait for its own ACK.
    fn request_prearm_report(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let conn = bridge.getattr("_conn")?;
        if conn.is_none() {
            return Ok(());
        }
        let nan = f64::NAN;
        let sent = within(&bridge.getattr("_send_lock")?, || -> PyResult<()> {
            if !conn.is(&bridge.getattr("_conn")?) {
                return Ok(());
            }
            let args = PyTuple::new(
                py,
                [
                    bridge.getattr("_target_system")?.unbind(),
                    bridge.getattr("_target_component")?.unbind(),
                    obj(py, constant_or("MAV_CMD_RUN_PREARM_CHECKS", 401)),
                    obj(py, 0),
                    obj(py, nan),
                    obj(py, nan),
                    obj(py, nan),
                    obj(py, nan),
                    obj(py, nan),
                    obj(py, nan),
                    obj(py, nan),
                ],
            )?;
            conn.getattr("mav")?.call_method1("command_long_send", args)?;
            Ok(())
        });
        match sent {
            Err(err) if err.is_instance_of::<PyException>(py) => {
                logger(py)?.call_method1("debug", ("prearm report request failed: %s", err.value(py)))?;
                Ok(())
            }
            other => other,
        }
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<StatusEngine>()
}
