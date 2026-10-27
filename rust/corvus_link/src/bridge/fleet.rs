//! The vehicle list's view of one link: what the one autopilot the link
//! flies last said about itself, enough to draw it on the map and list it,
//! and its warnings for the notification board. Passive: it reads frames the
//! bridge already received and decides nothing about the link. Until a
//! system is bound it takes nothing, so a stray frame on a noisy radio never
//! becomes a vehicle.
//!
//! The tracker's lock, binding, entry and events stay on
//! `corvus.fleet.FleetTracker`; this is the logic over them. The lock is a
//! Python lock because the mode decode it covers is Python: a Rust mutex
//! held across a call that releases the GIL could deadlock against it.

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyInt, PyList, PyModule, PyTuple};

use crate::bridge::receive::within;
use crate::protocol::host::{caught_by, obj};
use crate::protocol::telemetry::py_round;

/// `float(value)` when it is finite, else None.
fn finite(value: &Bound<'_, PyAny>) -> PyResult<Option<f64>> {
    match value.py().get_type::<PyFloat>().call1((value,)) {
        Ok(v) => {
            let f: f64 = v.extract()?;
            Ok(f.is_finite().then_some(f))
        }
        Err(err) if caught_by(value.py(), &err, &["TypeError", "ValueError"]) => Ok(None),
        Err(err) => Err(err),
    }
}

fn attr_or<'py>(o: &Bound<'py, PyAny>, name: &str, fallback: PyObject) -> PyResult<Bound<'py, PyAny>> {
    match o.getattr(name) {
        Ok(v) => Ok(v),
        Err(err) if caught_by(o.py(), &err, &["AttributeError"]) => Ok(fallback.into_bound(o.py())),
        Err(err) => Err(err),
    }
}

fn int_of<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    value.py().get_type::<PyInt>().call1((value,))
}

/// `value or fallback`.
fn or(value: Bound<'_, PyAny>, fallback: PyObject) -> PyResult<Bound<'_, PyAny>> {
    if value.is_truthy()? {
        Ok(value)
    } else {
        Ok(fallback.into_bound(value.py()))
    }
}

/// Python's `x % y` for floats: the result takes the divisor's sign.
fn py_mod(x: f64, y: f64) -> f64 {
    let m = x % y;
    if m != 0.0 {
        if (y < 0.0) != (m < 0.0) {
            m + y
        } else {
            m
        }
    } else {
        0.0f64.copysign(y)
    }
}

/// The fleet tracker's logic.
#[pyclass(name = "FleetEngine", module = "corvus_link.bridge", frozen)]
pub struct FleetEngine {
    env: PyObject,
}

impl FleetEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn now(&self, py: Python<'_>, now: Option<f64>) -> PyResult<f64> {
        match now {
            Some(t) => Ok(t),
            None => self.env(py).getattr("time")?.call_method0("monotonic")?.extract(),
        }
    }

    fn new_entry<'py>(
        py: Python<'py>,
        system: i64,
        component: &Bound<'py, PyAny>,
        stamp: f64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let entry = PyDict::new(py);
        entry.set_item("sysid", system)?;
        entry.set_item("component", component)?;
        entry.set_item("first_heartbeat", stamp)?;
        entry.set_item("last_heartbeat", stamp)?;
        for (k, v) in [
            ("type", obj(py, "")),
            ("type_id", obj(py, 0)),
            ("autopilot", obj(py, 0)),
            ("stack", obj(py, "")),
        ] {
            entry.set_item(k, v)?;
        }
        for (k, v) in [
            ("mode", obj(py, "")),
            ("mode_label", obj(py, "")),
            ("armed", obj(py, false)),
            ("system_status", obj(py, 0)),
        ] {
            entry.set_item(k, v)?;
        }
        for k in [
            "position",
            "altitude_rel",
            "altitude_amsl",
            "heading",
            "groundspeed",
            "battery",
            "voltage",
        ] {
            entry.set_item(k, py.None())?;
        }
        Ok(entry)
    }

    fn take_heartbeat(
        &self,
        tracker: &Bound<'_, PyAny>,
        entry: &Bound<'_, PyAny>,
        msg: &Bound<'_, PyAny>,
        stamp: f64,
    ) -> PyResult<()> {
        let py = tracker.py();
        entry.set_item("last_heartbeat", stamp)?;
        let type_id = match int_of(&or(attr_or(msg, "type", obj(py, 0))?, obj(py, 0))?) {
            Ok(v) => v,
            Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => obj(py, 0).into_bound(py),
            Err(err) => return Err(err),
        };
        entry.set_item("type_id", &type_id)?;
        let name = tracker
            .getattr("_type_names")?
            .call_method1("get", (&type_id, format!("TYPE_{}", type_id.str()?)))?;
        entry.set_item("type", name)?;
        let autopilot_id = attr_or(msg, "autopilot", obj(py, 0))?;
        entry.set_item("autopilot", &autopilot_id)?;
        let base_mode = int_of(&or(attr_or(msg, "base_mode", obj(py, 0))?, obj(py, 0))?)?;
        entry.set_item("armed", base_mode.bitand(128)?.is_truthy()?)?;
        entry.set_item(
            "system_status",
            int_of(&or(attr_or(msg, "system_status", obj(py, 0))?, obj(py, 0))?)?,
        )?;
        let decoded = (|| -> PyResult<()> {
            let dialect = self
                .env(py)
                .getattr("autopilot_dialect")?
                .call_method1("dialect_for", (&autopilot_id,))?;
            let mode = dialect.call_method1(
                "decode_mode",
                (attr_or(msg, "custom_mode", obj(py, 0))?, &base_mode, &type_id),
            )?;
            entry.set_item("stack", dialect.getattr("stack")?)?;
            entry.set_item("mode", &mode)?;
            let label = if mode.is_truthy()? {
                dialect.call_method1("mode_label", (&mode,))?
            } else {
                obj(py, "").into_bound(py)
            };
            entry.set_item("mode_label", label)
        })();
        match decoded {
            Err(err) if err.is_instance_of::<PyException>(py) => {
                entry.set_item("mode", "")?;
                entry.set_item("mode_label", "")
            }
            other => other,
        }
    }

    fn take_statustext(
        &self,
        tracker: &Bound<'_, PyAny>,
        entry: &Bound<'_, PyAny>,
        msg: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = tracker.py();
        let severity = match int_of(&attr_or(msg, "severity", obj(py, 6))?) {
            Ok(v) => v,
            Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => return Ok(()),
            Err(err) => return Err(err),
        };
        if severity.gt(self.env(py).getattr("EVENT_MAX_SEVERITY")?)? {
            return Ok(());
        }
        let mut raw = attr_or(msg, "text", obj(py, ""))?;
        if raw.is_instance_of::<PyBytes>() || raw.is_instance_of::<PyByteArray>() {
            let bytes: Vec<u8> = raw.extract()?;
            let head = bytes.split(|b| *b == 0).next().unwrap_or(&[]).to_vec();
            raw = PyBytes::new(py, &head).call_method1("decode", ("utf-8", "replace"))?;
        }
        let first = raw.str()?.call_method1("split", ("\0", 1))?.get_item(0)?;
        let text = pyo3::types::PyString::new(py, " ").call_method1("join", (first.call_method0("split")?,))?;
        if !text.is_truthy()? {
            return Ok(());
        }
        let seq = tracker.getattr("_event_seq")?.add(1)?;
        tracker.setattr("_event_seq", &seq)?;
        let events = tracker.getattr("_events")?;
        events.call_method1(
            "append",
            (PyTuple::new(py, [seq, entry.get_item("sysid")?, severity, text])?,),
        )?;
        let most: usize = self.env(py).getattr("MAX_EVENTS")?.extract()?;
        let count = events.len()?;
        if count > most {
            events.del_item(pyo3::types::PySlice::new(py, 0, (count - most) as isize, 1))?;
        }
        Ok(())
    }

    fn take_position(&self, entry: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = entry.py();
        let lat = finite(&attr_or(msg, "lat", py.None())?)?;
        let lon = finite(&attr_or(msg, "lon", py.None())?)?;
        let (Some(lat), Some(lon)) = (lat, lon) else {
            return Ok(());
        };
        if lat == 0.0 && lon == 0.0 {
            return Ok(());
        }
        if !((-90e7..=90e7).contains(&lat) && (-180e7..=180e7).contains(&lon)) {
            return Ok(());
        }
        entry.set_item(
            "position",
            PyList::new(py, [py_round(lon / 1e7, 7), py_round(lat / 1e7, 7)])?,
        )?;
        let rel = finite(&attr_or(msg, "relative_alt", py.None())?)?;
        let amsl = finite(&attr_or(msg, "alt", py.None())?)?;
        entry.set_item("altitude_rel", rel.map(|v| py_round(v / 1000.0, 1)))?;
        entry.set_item("altitude_amsl", amsl.map(|v| py_round(v / 1000.0, 1)))?;
        let hdg = attr_or(msg, "hdg", obj(py, 65535))?;
        if (hdg.is_instance_of::<PyInt>() || hdg.is_instance_of::<PyFloat>()) && hdg.ne(65535)? {
            let value: f64 = py.get_type::<PyFloat>().call1((&hdg,))?.extract()?;
            entry.set_item("heading", py_round(value / 100.0, 1))?;
        }
        let vx = finite(&attr_or(msg, "vx", obj(py, 0))?)?
            .filter(|v| *v != 0.0)
            .unwrap_or(0.0);
        let vy = finite(&attr_or(msg, "vy", obj(py, 0))?)?
            .filter(|v| *v != 0.0)
            .unwrap_or(0.0);
        entry.set_item("groundspeed", py_round(vx.hypot(vy) / 100.0, 1))
    }
}

#[pymethods]
impl FleetEngine {
    /// *env* is ``corvus.fleet``.
    #[new]
    fn new(env: PyObject) -> Self {
        FleetEngine { env }
    }

    /// Track *system* from now on, and only it; another system forgets the
    /// last one's entry.
    #[pyo3(signature = (tracker, system, component=None))]
    fn bind(
        &self,
        tracker: &Bound<'_, PyAny>,
        system: &Bound<'_, PyAny>,
        component: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let py = tracker.py();
        let zero = obj(py, 0).into_bound(py);
        let parsed = (|| -> PyResult<(Bound<'_, PyAny>, Bound<'_, PyAny>)> {
            let system = int_of(system)?;
            let component = int_of(&or(component.unwrap_or(&zero).clone(), obj(py, 0))?)?;
            Ok((system, component))
        })();
        let (system, component) = match parsed {
            Ok(v) => v,
            Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => return Ok(()),
            Err(err) => return Err(err),
        };
        if !(system.ge(1)? && system.le(255)?) {
            return Ok(());
        }
        within(&tracker.getattr("_lock")?, || {
            if system.ne(tracker.getattr("_system")?)? {
                tracker.setattr("_entry", py.None())?;
            }
            tracker.setattr("_system", &system)?;
            tracker.setattr("_component", &component)?;
            let entry = tracker.getattr("_entry")?;
            if !entry.is_none() && component.is_truthy()? {
                entry.set_item("component", &component)?;
            }
            Ok(())
        })
    }

    /// Take one frame. *vehicle_heartbeat* is the bridge's verdict that a
    /// HEARTBEAT is an autopilot's and not a gimbal's or a station's.
    #[pyo3(signature = (tracker, msg, vehicle_heartbeat=false, now=None))]
    fn observe(
        &self,
        tracker: &Bound<'_, PyAny>,
        msg: &Bound<'_, PyAny>,
        vehicle_heartbeat: bool,
        now: Option<f64>,
    ) -> PyResult<()> {
        let py = tracker.py();
        let read = (|| -> PyResult<(String, Bound<'_, PyAny>, Bound<'_, PyAny>)> {
            let name = msg.call_method0("get_type")?;
            let system = int_of(&or(msg.call_method0("get_srcSystem")?, obj(py, 0))?)?;
            let component = int_of(&or(msg.call_method0("get_srcComponent")?, obj(py, 0))?)?;
            Ok((name.extract().unwrap_or_default(), system, component))
        })();
        let (name, system, component) = match read {
            Ok(v) => v,
            Err(err) if err.is_instance_of::<PyException>(py) => return Ok(()),
            Err(err) => return Err(err),
        };
        let stamp = self.now(py, now)?;
        within(&tracker.getattr("_lock")?, || {
            let bound = tracker.getattr("_system")?;
            if bound.is_none() || system.ne(&bound)? {
                return Ok(());
            }
            let mut entry = tracker.getattr("_entry")?;
            if name == "HEARTBEAT" {
                if !vehicle_heartbeat {
                    return Ok(());
                }
                let own = tracker.getattr("_component")?;
                if own.is_truthy()? && component.is_truthy()? && component.ne(&own)? {
                    return Ok(());
                }
                if entry.is_none() {
                    let which = if component.is_truthy()? { component.clone() } else { own };
                    let sysid: i64 = system.extract()?;
                    entry = Self::new_entry(py, sysid, &which, stamp)?.into_any();
                    tracker.setattr("_entry", &entry)?;
                }
                return self.take_heartbeat(tracker, &entry, msg, stamp);
            }
            if entry.is_none() {
                return Ok(());
            }
            let latched = entry.get_item("component")?;
            if latched.is_truthy()? && component.ne(&latched)? {
                return Ok(());
            }
            match name.as_str() {
                "GLOBAL_POSITION_INT" => self.take_position(&entry, msg),
                "VFR_HUD" => {
                    if let Some(heading) = finite(&attr_or(msg, "heading", py.None())?)? {
                        entry.set_item("heading", heading)?;
                    }
                    if let Some(speed) = finite(&attr_or(msg, "groundspeed", py.None())?)? {
                        entry.set_item("groundspeed", py_round(speed, 1))?;
                    }
                    Ok(())
                }
                "ATTITUDE" => {
                    if entry.call_method1("get", ("heading",))?.is_none() {
                        if let Some(yaw) = finite(&attr_or(msg, "yaw", py.None())?)? {
                            entry.set_item("heading", py_round(py_mod(yaw.to_degrees(), 360.0), 1))?;
                        }
                    }
                    Ok(())
                }
                "STATUSTEXT" => self.take_statustext(tracker, &entry, msg),
                "SYS_STATUS" => {
                    let remaining = attr_or(msg, "battery_remaining", obj(py, -1))?;
                    if remaining.is_instance_of::<PyInt>() && remaining.ge(0)? && remaining.le(100)? {
                        entry.set_item("battery", &remaining)?;
                    }
                    let voltage = attr_or(msg, "voltage_battery", obj(py, 65535))?;
                    if voltage.is_instance_of::<PyInt>() && voltage.gt(0)? && voltage.ne(65535)? {
                        let volts: f64 = py.get_type::<PyFloat>().call1((&voltage,))?.extract()?;
                        entry.set_item("voltage", py_round(volts / 1000.0, 1))?;
                    }
                    Ok(())
                }
                _ => Ok(()),
            }
        })
    }

    /// `(last seq, [(system, severity, text), ...])` after *seq*.
    fn events_since<'py>(&self, tracker: &Bound<'py, PyAny>, seq: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
        let py = tracker.py();
        within(&tracker.getattr("_lock")?, || {
            let fresh = PyList::empty(py);
            for event in tracker.getattr("_events")?.try_iter()? {
                let event = event?;
                if event.get_item(0)?.gt(seq)? {
                    fresh.append(PyTuple::new(
                        py,
                        [event.get_item(1)?, event.get_item(2)?, event.get_item(3)?],
                    )?)?;
                }
            }
            PyTuple::new(py, [tracker.getattr("_event_seq")?, fresh.into_any()])
        })
    }

    /// The vehicle as a list of none or one plain dict, with `stale` past
    /// STALE_S; forgotten past FORGET_S.
    #[pyo3(signature = (tracker, now=None))]
    fn systems<'py>(&self, tracker: &Bound<'py, PyAny>, now: Option<f64>) -> PyResult<Bound<'py, PyList>> {
        let py = tracker.py();
        let stamp = self.now(py, now)?;
        let env = self.env(py);
        let picked = within(
            &tracker.getattr("_lock")?,
            || -> PyResult<Option<(Bound<'_, PyDict>, f64)>> {
                let entry = tracker.getattr("_entry")?;
                if entry.is_none() {
                    return Ok(None);
                }
                let last: f64 = py
                    .get_type::<PyFloat>()
                    .call1((entry.get_item("last_heartbeat")?,))?
                    .extract()?;
                let age = stamp - last;
                if age > env.getattr("FORGET_S")?.extract::<f64>()? {
                    tracker.setattr("_entry", py.None())?;
                    return Ok(None);
                }
                let item = PyDict::new(py);
                for (k, v) in entry.downcast::<PyDict>()?.iter() {
                    let key: String = k.extract().unwrap_or_default();
                    if !matches!(
                        key.as_str(),
                        "first_heartbeat" | "last_heartbeat" | "component" | "autopilot"
                    ) {
                        item.set_item(k, v)?;
                    }
                }
                Ok(Some((item, age)))
            },
        )?;
        let Some((item, age)) = picked else {
            return Ok(PyList::empty(py));
        };
        item.set_item(
            "stale",
            PyBool::new(py, age > env.getattr("STALE_S")?.extract::<f64>()?),
        )?;
        PyList::new(py, [item])
    }

    /// Drop the vehicle and the binding: the link flies nothing now.
    fn forget(&self, tracker: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = tracker.py();
        within(&tracker.getattr("_lock")?, || {
            tracker.setattr("_entry", py.None())?;
            tracker.setattr("_system", py.None())?;
            tracker.setattr("_component", 0)
        })
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<FleetEngine>()
}

#[cfg(test)]
mod tests {
    use super::py_mod;

    #[test]
    fn modulo_takes_the_divisor_sign() {
        assert_eq!(py_mod(-90.0, 360.0), 270.0);
        assert_eq!(py_mod(370.0, 360.0), 10.0);
        assert_eq!(py_mod(-1e-20, 360.0), 360.0);
        assert!(py_mod(-0.0, 360.0).is_sign_positive());
    }
}
