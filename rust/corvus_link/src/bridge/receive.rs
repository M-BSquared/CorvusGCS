//! The bridge's receive path and command transaction: the receive loop, the
//! duplicate frame filter, the dispatcher that routes every message to its
//! handler and writes the aircraft's own state, and the COMMAND_LONG /
//! COMMAND_INT waits for their COMMAND_ACK.
//!
//! The state stays where the rest of the bridge reads it (the store, the
//! bridge's attributes, the `_PendingAck` registry); this engine is the logic
//! over it. Handlers and helpers are called through the bridge, so a bridge
//! or a test that replaces one keeps doing so. Constants, the clock and the
//! logger are read from ``corvus.mavlink_bridge``.

use pyo3::exceptions::{PyConnectionError, PyException};
use pyo3::prelude::*;
use pyo3::types::{
    PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyFrozenSet, PyInt, PyList, PyModule, PySet, PyString, PyTuple,
};
use pyo3::BoundObject;

use crate::protocol::host::{caught_by, console, logger, obj, py_int, truthy};
use crate::protocol::telemetry::py_round;
use crate::wire::defs::constant_or;

// ------------------------------------------------------------------ helpers

/// `with ctx:` around `f`.
pub(crate) fn within<T>(ctx: &Bound<'_, PyAny>, f: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    let py = ctx.py();
    ctx.call_method0("__enter__")?;
    let result = f();
    match &result {
        Ok(_) => {
            ctx.call_method1("__exit__", (py.None(), py.None(), py.None()))?;
        }
        Err(err) => {
            ctx.call_method1("__exit__", (err.get_type(py), err.value(py), err.traceback(py)))?;
        }
    }
    result
}

/// `getattr(o, name, fallback)`.
fn attr_or<'py>(o: &Bound<'py, PyAny>, name: &str, fallback: PyObject) -> PyResult<Bound<'py, PyAny>> {
    match o.getattr(name) {
        Ok(v) => Ok(v),
        Err(err) if caught_by(o.py(), &err, &["AttributeError"]) => Ok(fallback.into_bound(o.py())),
        Err(err) => Err(err),
    }
}

/// `getattr(msg, "get_srcSystem", lambda: 0)()`.
fn source_call<'py>(msg: &Bound<'py, PyAny>, method: &str) -> PyResult<Bound<'py, PyAny>> {
    match msg.getattr(method) {
        Ok(f) => f.call0(),
        Err(err) if caught_by(msg.py(), &err, &["AttributeError"]) => Ok(obj(msg.py(), 0).into_bound(msg.py())),
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

/// `round(value, digits)`: Rust's for a float, Python's for anything else
/// (an int stays an int, as it did).
fn round_obj<'py>(value: &Bound<'py, PyAny>, digits: usize) -> PyResult<Bound<'py, PyAny>> {
    let py = value.py();
    if value.is_exact_instance_of::<PyFloat>() {
        return Ok(PyFloat::new(py, py_round(value.extract()?, digits)).into_any());
    }
    py.import("builtins")?.getattr("round")?.call1((value, digits))
}

/// `int(value)` through Python.
fn int_of<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    value.py().get_type::<PyInt>().call1((value,))
}

fn call0<'py>(bridge: &Bound<'py, PyAny>, method: &str) -> PyResult<Bound<'py, PyAny>> {
    bridge.call_method0(method)
}

fn call1<'py>(bridge: &Bound<'py, PyAny>, method: &str, arg: impl IntoPyObject<'py>) -> PyResult<Bound<'py, PyAny>> {
    let arg = arg
        .into_pyobject(bridge.py())
        .map_err(Into::into)?
        .into_any()
        .into_bound();
    bridge.call_method1(method, (arg,))
}

fn store<'py>(bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    bridge.getattr("_store")
}

/// `self._store.update(**fields)`, keys in order.
fn update(bridge: &Bound<'_, PyAny>, fields: Vec<(&str, PyObject)>) -> PyResult<()> {
    let kwargs = PyDict::new(bridge.py());
    for (k, v) in fields {
        kwargs.set_item(k, v)?;
    }
    store(bridge)?.call_method("update", (), Some(&kwargs))?;
    Ok(())
}

fn snapshot<'py>(bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    store(bridge)?.call_method0("get_snapshot")
}

fn with_kw<'py>(py: Python<'py>, pairs: &[(&str, PyObject)]) -> PyResult<Bound<'py, PyDict>> {
    let kwargs = PyDict::new(py);
    for (k, v) in pairs {
        kwargs.set_item(*k, v)?;
    }
    Ok(kwargs)
}

/// `self._store.is_stale(timeout=bridge.<threshold>())`.
fn stale(bridge: &Bound<'_, PyAny>, threshold: &str) -> PyResult<bool> {
    let py = bridge.py();
    let kwargs = with_kw(py, &[("timeout", call0(bridge, threshold)?.unbind())])?;
    store(bridge)?.call_method("is_stale", (), Some(&kwargs))?.is_truthy()
}

/// `int(value) in values`.
fn int_in(value: &Bound<'_, PyAny>, values: &[i64]) -> PyResult<bool> {
    Ok(int_of(value)?
        .extract::<i64>()
        .map(|v| values.contains(&v))
        .unwrap_or(false))
}

// ------------------------------------------------------- module functions

/// Bounds a real HEARTBEAT stays under, far above today's enums so a newer
/// firmware's type still connects.
const HEARTBEAT_LIMITS: [(&str, i64); 3] = [("type", 128), ("autopilot", 64), ("system_status", 32)];

/// MAV_TYPEs that are peripherals rather than aircraft.
fn non_vehicle_types() -> Vec<i64> {
    [
        ("MAV_TYPE_GCS", 6),
        ("MAV_TYPE_GIMBAL", 26),
        ("MAV_TYPE_ADSB", 27),
        ("MAV_TYPE_ONBOARD_CONTROLLER", 18),
        ("MAV_TYPE_CAMERA", 30),
        ("MAV_TYPE_SERVO", 36),
        ("MAV_TYPE_BATTERY", 38),
        ("MAV_TYPE_PARACHUTE", 37),
        ("MAV_TYPE_LOG", 39),
        ("MAV_TYPE_OSD", 40),
        ("MAV_TYPE_IMU", 41),
        ("MAV_TYPE_GPS", 42),
        ("MAV_TYPE_WINCH", 43),
    ]
    .iter()
    .map(|(n, f)| constant_or(n, *f))
    .collect()
}

/// Component ids that are never the autopilot, even under the vehicle's own
/// system id.
fn non_autopilot_components() -> Vec<i64> {
    [
        ("MAV_COMP_ID_GIMBAL", 154),
        ("MAV_COMP_ID_ONBOARD_COMPUTER", 191),
        ("MAV_COMP_ID_TELEMETRY_RADIO", 68),
        ("MAV_COMP_ID_UDP_BRIDGE", 240),
    ]
    .iter()
    .map(|(n, f)| constant_or(n, *f))
    .collect()
}

fn plain_int(value: &Bound<'_, PyAny>) -> bool {
    value.is_instance_of::<PyInt>() && !value.is_instance_of::<PyBool>()
}

/// Could this HEARTBEAT have been sent by a real node? A frame that passed
/// the checksum by chance carries an odd `mavlink_version` or out of range
/// enums; a field that is not an int (a synthetic frame) is not judged.
#[pyfunction]
pub fn is_plausible_heartbeat(hb: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = hb.py();
    let version = attr_or(hb, "mavlink_version", py.None())?;
    if plain_int(&version) && version.ne(3)? {
        return Ok(false);
    }
    for (field, limit) in HEARTBEAT_LIMITS {
        let value = attr_or(hb, field, py.None())?;
        if !plain_int(&value) {
            continue;
        }
        let inside = value.ge(0)? && value.lt(limit)?;
        if !inside {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Is this HEARTBEAT the flight controller's? Rejects a forged frame, a
/// component that is never an autopilot, a peripheral MAV_TYPE, and an
/// announced MAV_AUTOPILOT_INVALID.
#[pyfunction]
pub fn is_vehicle_heartbeat(hb: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = hb.py();
    if !is_plausible_heartbeat(hb)? {
        return Ok(false);
    }
    let judged = (|| -> PyResult<bool> {
        let component = source_call(hb, "get_srcComponent")?;
        Ok(component.is_truthy()? && int_in(&component, &non_autopilot_components())?)
    })();
    match judged {
        Ok(true) => return Ok(false),
        Ok(false) => {}
        Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => {}
        Err(err) => return Err(err),
    }
    let peripheral = (|| -> PyResult<bool> {
        let vehicle_type = attr_or(hb, "type", py.None())?;
        Ok(!vehicle_type.is_none() && int_in(&vehicle_type, &non_vehicle_types())?)
    })();
    match peripheral {
        Ok(true) => return Ok(false),
        Ok(false) => {}
        Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => {}
        Err(err) => return Err(err),
    }
    let autopilot = attr_or(hb, "autopilot", py.None())?;
    if autopilot.is_none() {
        return Ok(true);
    }
    match int_of(&autopilot) {
        Ok(v) => Ok(v.ne(constant_or("MAV_AUTOPILOT_INVALID", 8))?),
        Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => Ok(true),
        Err(err) => Err(err),
    }
}

/// `getattr(msg, name, 0) or 0`.
fn field_or_zero<'py>(msg: &Bound<'py, PyAny>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    let py = msg.py();
    or(attr_or(msg, name, obj(py, 0))?, obj(py, 0))
}

/// SYS_STATUS: is *bit* reported (present or enabled), and is it healthy?
fn sensor_bit(msg: &Bound<'_, PyAny>, bit: i64) -> PyResult<Option<bool>> {
    let enabled = field_or_zero(msg, "onboard_control_sensors_enabled")?;
    let present = field_or_zero(msg, "onboard_control_sensors_present")?;
    if !enabled.bitor(&present)?.bitand(bit)?.is_truthy()? {
        return Ok(None);
    }
    let health = field_or_zero(msg, "onboard_control_sensors_health")?;
    Ok(Some(health.bitand(bit)?.is_truthy()?))
}

/// SYS_STATUS: "ok" or "fault" for one sensor, "" when it is not reported.
#[pyfunction]
pub fn sensor_health(msg: &Bound<'_, PyAny>, bit: i64) -> PyResult<&'static str> {
    Ok(match sensor_bit(msg, bit)? {
        None => "",
        Some(true) => "ok",
        Some(false) => "fault",
    })
}

/// SYS_STATUS: ready to arm, refused, or None when not reported.
#[pyfunction]
pub fn prearm_ok(msg: &Bound<'_, PyAny>) -> PyResult<Option<bool>> {
    sensor_bit(msg, constant_or("MAV_SYS_STATUS_PREARM_CHECK", 268435456))
}

const DOWNWARD_ORIENTATION: i64 = 25;

/// DISTANCE_SENSOR in metres: the downward sensor only, inside its own range.
#[pyfunction]
pub fn rangefinder_distance_m(msg: &Bound<'_, PyAny>) -> PyResult<Option<f64>> {
    let py = msg.py();
    let read = (|| -> PyResult<Option<(i64, i64, i64)>> {
        if py_int(&msg.getattr("orientation")?)? != DOWNWARD_ORIENTATION {
            return Ok(None);
        }
        let current = py_int(&msg.getattr("current_distance")?)?;
        let low = py_int(&field_or_zero(msg, "min_distance")?)?;
        let high = py_int(&field_or_zero(msg, "max_distance")?)?;
        Ok(Some((current, low, high)))
    })();
    let (current, low, high) = match read {
        Ok(Some(v)) => v,
        Ok(None) => return Ok(None),
        Err(err) if caught_by(py, &err, &["AttributeError", "TypeError", "ValueError"]) => return Ok(None),
        Err(err) => return Err(err),
    };
    if current < low || (high > 0 && current >= high) {
        return Ok(None);
    }
    Ok(Some(py_round(current as f64 / 100.0, 2)))
}

/// GPS_RAW_INT altitude in metres AMSL, None without a 3D fix.
#[pyfunction]
pub fn gps_altitude_m(msg: &Bound<'_, PyAny>) -> PyResult<Option<f64>> {
    let py = msg.py();
    let read = (|| -> PyResult<Option<f64>> {
        if py_int(&field_or_zero(msg, "fix_type")?)? < 3 {
            return Ok(None);
        }
        Ok(Some(py_round(py_int(&msg.getattr("alt")?)? as f64 / 1000.0, 1)))
    })();
    match read {
        Err(err) if caught_by(py, &err, &["AttributeError", "TypeError", "ValueError"]) => Ok(None),
        other => other,
    }
}

/// GPS_RAW_INT h_acc / v_acc in metres, -1 when not reported (0).
#[pyfunction]
pub fn gps_accuracy_m(raw: &Bound<'_, PyAny>) -> PyResult<f64> {
    let py = raw.py();
    let value = match py_int(&or(raw.clone(), obj(py, 0))?) {
        Ok(v) => v,
        Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => return Ok(-1.0),
        Err(err) => return Err(err),
    };
    Ok(if value > 0 {
        py_round(value as f64 / 1000.0, 2)
    } else {
        -1.0
    })
}

/// AUTOPILOT_VERSION.flight_custom_version as a git hash: ArduPilot's is
/// the hash as text, PX4's the first five SHA-1 bytes packed into the high
/// bytes of a little endian uint64. "" when empty.
#[pyfunction]
pub fn decode_git_hash(raw: &Bound<'_, PyAny>) -> PyResult<String> {
    let py = raw.py();
    let bytes: Vec<u8> = if raw.is_instance_of::<PyString>() {
        raw.downcast::<PyString>()?
            .to_cow()?
            .chars()
            .map(|c| if (c as u32) < 256 { c as u32 as u8 } else { b'?' })
            .collect()
    } else if raw.is_instance_of::<pyo3::types::PyList>() || raw.is_instance_of::<PyTuple>() {
        let mut out = Vec::new();
        for item in raw.try_iter()? {
            match int_of(&item?)
                .and_then(|v| v.bitand(0xFF))
                .and_then(|v| v.extract::<u8>())
            {
                Ok(b) => out.push(b),
                Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => return Ok(String::new()),
                Err(err) => return Err(err),
            }
        }
        out
    } else if raw.is_instance_of::<PyBytes>() || raw.is_instance_of::<PyByteArray>() {
        raw.extract()?
    } else {
        return Ok(String::new());
    };
    let start = bytes.iter().position(|b| *b != 0);
    let Some(start) = start else {
        return Ok(String::new());
    };
    let end = bytes.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    let trimmed = &bytes[start..end];
    if trimmed.len() >= 6 && trimmed.iter().all(u8::is_ascii_hexdigit) {
        return Ok(String::from_utf8_lossy(trimmed).to_lowercase());
    }
    let mut raw = bytes.clone();
    if raw.first() == Some(&0) {
        raw.reverse();
    }
    while raw.last() == Some(&0) {
        raw.pop();
    }
    Ok(raw.iter().fold(String::new(), |mut out, b| {
        out.push_str(&format!("{b:02x}"));
        out
    }))
}

// ------------------------------------------------------------------- engine

/// Puts one command on the link: (connection, attempt, padded params).
type SendFn<'a> = dyn Fn(&Bound<'_, PyAny>, i64, Vec<PyObject>) -> PyResult<()> + 'a;

/// The receive path and command transaction of one bridge.
#[pyclass(name = "ReceiveEngine", module = "corvus_link.bridge", frozen)]
pub struct ReceiveEngine {
    env: PyObject,
}

impl ReceiveEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn monotonic(&self, py: Python<'_>) -> PyResult<f64> {
        self.env(py).getattr("time")?.call_method0("monotonic")?.extract()
    }

    fn tunable(&self, py: Python<'_>, name: &str) -> PyResult<f64> {
        self.env(py).getattr(name)?.extract()
    }

    fn from_vehicle(bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<bool> {
        call1(bridge, "_is_from_vehicle", msg)?.is_truthy()
    }

    fn from_autopilot(bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<bool> {
        call1(bridge, "_is_from_autopilot", msg)?.is_truthy()
    }

    fn map_name(&self, py: Python<'_>, table: &str, key: &Bound<'_, PyAny>, prefix: &str) -> PyResult<PyObject> {
        let fallback = format!("{prefix}{}", key.str()?);
        Ok(self
            .env(py)
            .getattr(table)?
            .call_method1("get", (key, fallback))?
            .unbind())
    }

    // ---------------------------------------------------------- handlers

    fn command_ack(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let result = msg.getattr("result")?;
        let command = msg.getattr("command")?;
        let text = self
            .env(py)
            .getattr("MAV_RESULT_TEXT")?
            .call_method1("get", (&result, format!("result={}", result.str()?)))?;
        let formatted = format!("COMMAND_ACK: cmd={} {}", command.str()?, text.str()?);
        let level = if result.eq(0)? {
            "success"
        } else if result.eq(5)? {
            "info"
        } else {
            "error"
        };
        console(bridge, "COMMAND_ACK", &formatted, level)?;
        if command.eq(constant_or("MAV_CMD_DO_AUTOTUNE_ENABLE", 212))? {
            call1(bridge, "_handle_autotune_ack", msg)?;
        }
        if !call1(bridge, "_ack_is_for_us", msg)?.is_truthy()? {
            return Ok(());
        }
        let in_progress = constant_or("MAV_RESULT_IN_PROGRESS", 5);
        within(&bridge.getattr("_ack_lock")?, || {
            let pending = bridge.getattr("_pending_acks")?.call_method1("get", (&command,))?;
            if !pending.is_none() && result.eq(in_progress)? && !pending.getattr("accept_in_progress")?.is_truthy()? {
                pending.setattr("in_progress", true)?;
            } else if !pending.is_none() && pending.getattr("interim")?.contains(&result)? {
                pending.setattr("interim_result", &result)?;
            } else if pending.is_truthy()?
                && (result.ne(in_progress)? || pending.getattr("accept_in_progress")?.is_truthy()?)
            {
                pending.setattr("result", &result)?;
                pending.getattr("event")?.call_method0("set")?;
            }
            Ok(())
        })
    }

    fn heartbeat(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if !Self::from_autopilot(bridge, msg)? {
            return Ok(());
        }
        let beat = store(bridge)?.call_method0("heartbeat")?;
        call1(bridge, "_note_heartbeat", beat)?;
        call0(bridge, "_publish_heartbeat_jitter")?;
        let seen = bridge.getattr("_rangefinder_seen")?;
        if !seen.is_none() && self.monotonic(py)? - seen.extract::<f64>()? > self.tunable(py, "_RANGEFINDER_STALE_S")? {
            bridge.setattr("_rangefinder_seen", py.None())?;
            update(bridge, vec![("rangefinder_distance", py.None())])?;
        }
        if bridge.getattr("_degraded_warned")?.is_truthy()? {
            bridge.setattr("_degraded_warned", false)?;
            update(
                bridge,
                vec![("link_status", obj(py, "connected")), ("link_error", obj(py, ""))],
            )?;
            logger(py)?.call_method1("info", ("Link recovered: heartbeat back",))?;
        }
        let kind = msg.getattr("type")?;
        let autopilot_id = msg.getattr("autopilot")?;
        let vtype = self.map_name(py, "MAV_TYPE_MAP", &kind, "TYPE_")?;
        let autopilot = self.map_name(py, "MAV_AUTOPILOT_MAP", &autopilot_id, "AP_")?;
        if bridge
            .call_method1("_latch_dialect", (&autopilot_id, &kind))?
            .is_truthy()?
            && !bridge.getattr("_conn")?.is_none()
        {
            call0(bridge, "_build_mode_mapping")?;
        }
        let armed = msg
            .getattr("base_mode")?
            .bitand(constant_or("MAV_MODE_FLAG_SAFETY_ARMED", 128))?
            .is_truthy()?;
        let mode = call1(bridge, "_decode_mode", msg)?;
        let status = int_of(&attr_or(msg, "system_status", obj(py, 0))?)?;
        let terminated = status.eq(constant_or("MAV_STATE_FLIGHT_TERMINATION", 7))?;
        let before = snapshot(bridge)?;
        if terminated && before.call_method1("get", ("kill_switch",))?.is_truthy()? {
            bridge.setattr("_kill_evidence_at", self.monotonic(py)?)?;
        }
        let lifted = !terminated && before.call_method1("get", ("flight_termination",))?.is_truthy()?;
        let dialect = bridge.getattr("_dialect")?;
        let mut fields = vec![
            ("vehicle_type", vtype),
            ("autopilot", autopilot),
            ("armed", obj(py, armed)),
            ("mode", mode.clone().unbind()),
            ("mode_label", dialect.call_method1("mode_label", (&mode,))?.unbind()),
            ("autopilot_stack", dialect.getattr("stack")?.unbind()),
            ("flight_termination", obj(py, terminated)),
        ];
        if lifted {
            fields.push(("kill_switch", obj(py, false)));
        }
        update(bridge, fields)?;
        if lifted {
            store(bridge)?.call_method1("resolve_warnings", (PySet::new(py, ["kill"])?,))?;
        }
        if armed {
            if bridge.getattr("_prearm_reasons")?.is_truthy()? {
                call0(bridge, "_clear_prearm_reasons")?;
            }
            store(bridge)?.call_method1("resolve_warnings", (PySet::new(py, ["prearm"])?,))?;
        }
        call1(bridge, "_roll_tlog_on_disarm", armed)?;
        call0(bridge, "_publish_link_quality")?;
        Ok(())
    }

    fn global_position(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let amsl = msg.getattr("alt")?.div(1000.0)?;
        let agl = msg.getattr("relative_alt")?.div(1000.0)?;
        bridge.setattr("_position_home_alt_amsl", amsl.sub(&agl)?)?;
        let position = PyList::new(py, [msg.getattr("lon")?.div(1e7)?, msg.getattr("lat")?.div(1e7)?])?;
        let hdg = msg.getattr("hdg")?;
        let heading = if hdg.ne(65535)? {
            hdg.div(100.0)?.unbind()
        } else {
            obj(py, 0)
        };
        let velocity = |name: &str| -> PyResult<PyObject> {
            let raw = field_or_zero(msg, name)?;
            let v: f64 = py.get_type::<PyFloat>().call1((raw,))?.extract()?;
            Ok(obj(py, py_round(v / 100.0, 2)))
        };
        update(
            bridge,
            vec![
                ("position", position.into_any().unbind()),
                ("altitude_amsl", round_obj(&amsl, 1)?.unbind()),
                ("altitude_agl", round_obj(&agl, 1)?.unbind()),
                ("heading", heading),
                ("vx", velocity("vx")?),
                ("vy", velocity("vy")?),
                ("vz", velocity("vz")?),
            ],
        )
    }

    fn attitude(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let deg = |name: &str| -> PyResult<PyObject> {
            let v: f64 = msg.getattr(name)?.extract()?;
            Ok(obj(py, py_round(v.to_degrees(), 1)))
        };
        update(
            bridge,
            vec![
                ("roll", deg("roll")?),
                ("pitch", deg("pitch")?),
                ("yaw", deg("yaw")?),
                ("rollspeed", deg("rollspeed")?),
                ("pitchspeed", deg("pitchspeed")?),
                ("yawspeed", deg("yawspeed")?),
            ],
        )
    }

    fn vfr_hud(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        // In the order Python evaluated them, so a frame missing two fields
        // fails on the same one.
        let ground = round_obj(&msg.getattr("groundspeed")?, 1)?.unbind();
        let airspeed = msg.getattr("airspeed")?;
        let air = if airspeed.eq(&airspeed)? {
            round_obj(&airspeed, 1)?.unbind()
        } else {
            obj(py, 0)
        };
        let heading = msg.getattr("heading")?.unbind();
        let climb = round_obj(&msg.getattr("climb")?, 1)?.unbind();
        update(
            bridge,
            vec![
                ("groundspeed", ground),
                ("airspeed", air),
                ("heading", heading),
                ("vspeed", climb),
            ],
        )
    }

    fn gps_raw(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let fix_type = msg.getattr("fix_type")?;
        let fix = self.map_name(py, "GPS_FIX_MAP", &fix_type, "FIX_")?;
        let dop = |value: Bound<'_, PyAny>| -> PyResult<PyObject> {
            let v = if value.ne(65535)? && value.gt(0)? {
                value.div(100.0)?
            } else {
                PyFloat::new(py, 99.0).into_any()
            };
            Ok(round_obj(&v, 1)?.unbind())
        };
        let hdop = dop(msg.getattr("eph")?)?;
        let vdop = dop(attr_or(msg, "epv", obj(py, 65535))?)?;
        update(
            bridge,
            vec![
                ("gps_fix", fix),
                ("gps_satellites", msg.getattr("satellites_visible")?.unbind()),
                ("gps_hdop", hdop),
                ("gps_vdop", vdop),
                (
                    "gps_h_acc",
                    obj(py, gps_accuracy_m(&attr_or(msg, "h_acc", obj(py, 0))?)?),
                ),
                (
                    "gps_v_acc",
                    obj(py, gps_accuracy_m(&attr_or(msg, "v_acc", obj(py, 0))?)?),
                ),
                ("gps_altitude", obj(py, gps_altitude_m(msg)?)),
            ],
        )
    }

    fn sys_status(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let raw_voltage = msg.getattr("voltage_battery")?;
        let voltage = if raw_voltage.ne(65535)? {
            raw_voltage.div(1000.0)?
        } else {
            obj(py, 0).into_bound(py)
        };
        let raw_current = msg.getattr("current_battery")?;
        let current = if raw_current.ne(-1)? {
            raw_current.div(100.0)?
        } else {
            obj(py, 0).into_bound(py)
        };
        let remaining = msg.getattr("battery_remaining")?;
        let reported = if remaining.ne(-1)? {
            int_of(&remaining)?
        } else {
            obj(py, -1).into_bound(py)
        };
        let prearm = prearm_ok(msg)?;
        let battery = bridge.call_method1("_battery_fields", (&voltage, &current, &reported))?;
        let source = battery.get_item("battery_source")?;
        let known = source.eq("estimate")? || reported.ge(0)?;
        let percent = if known {
            battery.get_item("battery_percent")?.unbind()
        } else {
            py.None()
        };
        let armed = snapshot(bridge)?.call_method1("get", ("armed",))?.is_truthy()?;
        let endurance = bridge.call_method1("_battery_endurance_fields", (percent, &source, armed))?;
        battery.call_method1("update", (endurance,))?;
        let kwargs = with_kw(
            py,
            &[
                ("prearm_ok", obj(py, prearm)),
                (
                    "gps_health",
                    obj(py, sensor_health(msg, constant_or("MAV_SYS_STATUS_SENSOR_GPS", 32))?),
                ),
            ],
        )?;
        kwargs.update(battery.downcast::<pyo3::types::PyMapping>()?)?;
        store(bridge)?.call_method("update", (), Some(&kwargs))?;
        call1(bridge, "_note_prearm_state", prearm)?;
        Ok(())
    }

    fn system_time(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if !Self::from_autopilot(bridge, msg)? {
            return Ok(());
        }
        let usec = msg.getattr("time_unix_usec")?;
        if usec.is_truthy()? {
            let datetime = self.env(py).getattr("datetime")?;
            let converted = (|| -> PyResult<Bound<'_, PyAny>> {
                datetime
                    .getattr("datetime")?
                    .call_method1("fromtimestamp", (usec.div(1e6)?, datetime.getattr("UTC")?))
            })();
            match converted {
                Ok(t) => {
                    update(
                        bridge,
                        vec![("time", t.call_method1("strftime", ("%H:%M:%S UTC",))?.unbind())],
                    )?;
                }
                Err(err)
                    if caught_by(py, &err, &["TypeError", "ValueError"])
                        || err.is_instance_of::<pyo3::exceptions::PyOverflowError>(py)
                        || err.is_instance_of::<pyo3::exceptions::PyOSError>(py) =>
                {
                    logger(py)?.call_method1("debug", ("SYSTEM_TIME out of range: %s", &usec))?;
                }
                Err(err) => return Err(err),
            }
        }
        let boot = attr_or(msg, "time_boot_ms", py.None())?;
        let number = boot.is_instance_of::<PyInt>() || boot.is_instance_of::<PyFloat>();
        if number && boot.ge(0)? {
            let ms = int_of(&boot)?;
            call1(bridge, "_note_boot_time", &ms)?;
            update(bridge, vec![("boot_ms", ms.unbind())])?;
        }
        Ok(())
    }

    fn autopilot_version(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if !Self::from_autopilot(bridge, msg)? {
            return Ok(());
        }
        let sw = msg.getattr("flight_sw_version")?;
        if sw.is_truthy()? {
            let part = |shift: i64| -> PyResult<Bound<'_, PyAny>> { sw.rshift(shift)?.bitand(0xFF) };
            let (major, minor, patch) = (part(24)?, part(16)?, part(8)?);
            let custom = attr_or(msg, "flight_custom_version", PyBytes::new(py, b"").into_any().unbind())?;
            let detail = call1(bridge, "_decode_git_hash", custom)?;
            let label = format!("v{}.{}.{}", major.str()?, minor.str()?, patch.str()?);
            update(
                bridge,
                vec![("px4_version", obj(py, label)), ("px4_version_detail", detail.unbind())],
            )?;
            let version = PyTuple::new(py, [&major, &minor, &patch])?;
            if version.ne(bridge.getattr("_firmware_version")?)? {
                bridge.setattr("_firmware_version", version)?;
                call0(bridge, "_build_mode_mapping")?;
            }
        }
        let ids = (|| -> PyResult<()> {
            bridge.setattr("_board_vendor_id", int_of(&field_or_zero(msg, "vendor_id")?)?)?;
            bridge.setattr("_board_product_id", int_of(&field_or_zero(msg, "product_id")?)?)?;
            Ok(())
        })();
        match ids {
            Err(err) if !caught_by(py, &err, &["TypeError", "ValueError"]) => return Err(err),
            _ => {}
        }
        let capabilities = match int_of(&field_or_zero(msg, "capabilities")?) {
            Ok(v) => v,
            Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => obj(py, 0).into_bound(py),
            Err(err) => return Err(err),
        };
        let env = self.env(py);
        let dialect = env.getattr("autopilot_dialect")?;
        if capabilities
            .bitand(env.getattr("_CAP_PARAM_ENCODE_BYTEWISE")?)?
            .is_truthy()?
        {
            bridge.setattr("_param_encoding_declared", dialect.getattr("PARAM_ENCODING_BYTEWISE")?)?;
        } else if capabilities
            .bitand(env.getattr("_CAP_PARAM_ENCODE_C_CAST")?)?
            .is_truthy()?
        {
            bridge.setattr("_param_encoding_declared", dialect.getattr("PARAM_ENCODING_C_CAST")?)?;
        }
        Ok(())
    }

    fn log_frame(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if !Self::from_autopilot(bridge, msg)? {
            return Ok(());
        }
        let sink = bridge.getattr("_log_sink")?;
        if sink.is_none() {
            return Ok(());
        }
        match sink.call1((msg,)) {
            Ok(_) => Ok(()),
            Err(err) if err.is_instance_of::<PyException>(py) => {
                let kwargs = with_kw(py, &[("exc_info", err.value(py).clone().into_any().unbind())])?;
                logger(py)?.call_method("debug", ("log sink raised",), Some(&kwargs))?;
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn vibration(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let level = |name: &str| -> PyResult<PyObject> {
            let v: f64 = py.get_type::<PyFloat>().call1((msg.getattr(name)?,))?.extract()?;
            Ok(obj(py, py_round(v, 4)))
        };
        let clip = |name: &str| -> PyResult<PyObject> { Ok(int_of(&msg.getattr(name)?)?.unbind()) };
        update(
            bridge,
            vec![
                ("vibration_x", level("vibration_x")?),
                ("vibration_y", level("vibration_y")?),
                ("vibration_z", level("vibration_z")?),
                ("clipping_0", clip("clipping_0")?),
                ("clipping_1", clip("clipping_1")?),
                ("clipping_2", clip("clipping_2")?),
            ],
        )
    }

    fn extended_sys_state(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let landed = match int_of(&field_or_zero(msg, "landed_state")?) {
            Ok(v) => v,
            Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => obj(py, 0).into_bound(py),
            Err(err) => return Err(err),
        };
        if landed.ge(0)? && landed.le(4)? {
            update(bridge, vec![("landed_state", landed.unbind())])?;
        }
        Ok(())
    }

    fn home_position(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if !Self::from_autopilot(bridge, msg)? {
            return Ok(());
        }
        let lat = msg.getattr("latitude")?.div(1e7)?;
        let lon = msg.getattr("longitude")?.div(1e7)?;
        bridge.setattr("_home_alt_amsl", msg.getattr("altitude")?.div(1000.0)?)?;
        update(bridge, vec![("home", PyList::new(py, [lon, lat])?.into_any().unbind())])
    }
}

#[pymethods]
impl ReceiveEngine {
    /// *env* is ``corvus.mavlink_bridge``.
    #[new]
    fn new(env: PyObject) -> Self {
        ReceiveEngine { env }
    }

    /// Route one message. Messages from another system on the link reach
    /// only the handlers that check their own source (COMMAND_ACK, the
    /// mission handshake, the shell, the radio, GNSS integrity); everything
    /// that writes the aircraft's state needs this vehicle, and the mode,
    /// arm state and versions its autopilot component.
    fn dispatch(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let name_obj = msg.call_method0("get_type")?;
        let name: String = name_obj.str()?.to_string();
        bridge.call_method1("_observe_fleet", (msg, &name_obj))?;
        let as_int = |flag: bool| -> PyResult<Bound<'_, PyDict>> { with_kw(py, &[("as_int", obj(py, flag))]) };

        if name == "COMMAND_ACK" && Self::from_vehicle(bridge, msg)? {
            self.command_ack(bridge, msg)?;
        }
        match name.as_str() {
            "MISSION_REQUEST_INT" | "MISSION_REQUEST" => {
                bridge.call_method(
                    "_handle_mission_request",
                    (msg,),
                    Some(&as_int(name == "MISSION_REQUEST_INT")?),
                )?;
            }
            "MISSION_ACK" => {
                call1(bridge, "_handle_mission_ack", msg)?;
            }
            "MISSION_COUNT" => {
                call1(bridge, "_handle_mission_count", msg)?;
            }
            "MISSION_ITEM_INT" | "MISSION_ITEM" => {
                bridge.call_method(
                    "_handle_mission_item",
                    (msg,),
                    Some(&as_int(name == "MISSION_ITEM_INT")?),
                )?;
            }
            _ => {}
        }
        if name == "SERIAL_CONTROL" {
            call1(bridge, "_handle_serial_control", msg)?;
        }
        if name == "RADIO_STATUS" {
            call1(bridge, "_handle_radio_status", msg)?;
            return Ok(());
        }
        if name == "UNKNOWN_441" || name == "GNSS_INTEGRITY" {
            call1(bridge, "_handle_gnss_integrity", msg)?;
            return Ok(());
        }
        if !Self::from_vehicle(bridge, msg)? {
            return Ok(());
        }
        if name == "OPEN_DRONE_ID_ARM_STATUS" {
            call1(bridge, "_handle_remote_id_arm_status", msg)?;
            return Ok(());
        }
        match name.as_str() {
            "HEARTBEAT" => self.heartbeat(bridge, msg),
            "GLOBAL_POSITION_INT" => self.global_position(bridge, msg),
            "DISTANCE_SENSOR" => {
                if py_int(&attr_or(msg, "orientation", obj(py, -1))?)? == DOWNWARD_ORIENTATION {
                    bridge.setattr("_rangefinder_seen", self.monotonic(py)?)?;
                    update(
                        bridge,
                        vec![("rangefinder_distance", obj(py, rangefinder_distance_m(msg)?))],
                    )?;
                }
                Ok(())
            }
            "ATTITUDE" => self.attitude(bridge, msg),
            "VFR_HUD" => self.vfr_hud(bridge, msg),
            "GPS_RAW_INT" => self.gps_raw(bridge, msg),
            "SYS_STATUS" => self.sys_status(bridge, msg),
            "BATTERY_STATUS" => call1(bridge, "_handle_battery_status", msg).map(|_| ()),
            "SYSTEM_TIME" => self.system_time(bridge, msg),
            "STATUSTEXT" => call1(bridge, "_handle_statustext", msg).map(|_| ()),
            "AUTOPILOT_VERSION" => self.autopilot_version(bridge, msg),
            "LOG_ENTRY" | "LOG_DATA" => self.log_frame(bridge, msg),
            "PARAM_VALUE" => {
                if Self::from_autopilot(bridge, msg)? {
                    call1(bridge, "_handle_param_value", msg)?;
                }
                Ok(())
            }
            "FILE_TRANSFER_PROTOCOL" => {
                if Self::from_autopilot(bridge, msg)? {
                    call1(bridge, "_handle_ftp_message", msg)?;
                }
                Ok(())
            }
            "VIBRATION" => self.vibration(bridge, msg),
            "EXTENDED_SYS_STATE" => self.extended_sys_state(bridge, msg),
            "RC_CHANNELS" | "RC_CHANNELS_RAW" => {
                let kwargs = with_kw(py, &[("raw", obj(py, name == "RC_CHANNELS_RAW"))])?;
                bridge.call_method("_handle_rc_channels", (msg,), Some(&kwargs))?;
                Ok(())
            }
            "ATTITUDE_TARGET" => call1(bridge, "_handle_attitude_target", msg).map(|_| ()),
            "POSITION_TARGET_LOCAL_NED" => call1(bridge, "_handle_position_target", msg).map(|_| ()),
            "MISSION_CURRENT" | "MISSION_ITEM_REACHED" => {
                if Self::from_autopilot(bridge, msg)? {
                    let handler = if name == "MISSION_CURRENT" {
                        "_handle_mission_current"
                    } else {
                        "_handle_mission_item_reached"
                    };
                    call1(bridge, handler, msg)?;
                }
                Ok(())
            }
            "HOME_POSITION" => self.home_position(bridge, msg),
            _ => Ok(()),
        }
    }

    /// Log, count and dispatch one frame. A handler that fails is logged
    /// once per message type and never costs the link.
    fn process_frame(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let handled = (|| -> PyResult<()> {
            if call1(bridge, "_is_duplicate_frame", msg)?.is_truthy()? {
                return Ok(());
            }
            call1(bridge, "_write_tlog", msg)?;
            bridge.getattr("message_scope")?.call_method1("record", (msg,))?;
            call1(bridge, "_dispatch", msg)?;
            Ok(())
        })();
        let Err(err) = handled else {
            return Ok(());
        };
        if !err.is_instance_of::<PyException>(py) {
            return Err(err);
        }
        let name = match msg
            .call_method0("get_type")
            .and_then(|n| n.str().map(|s| s.to_string()))
        {
            Ok(n) => n,
            Err(inner) if inner.is_instance_of::<PyException>(py) => "?".to_string(),
            Err(inner) => return Err(inner),
        };
        let failed = bridge.getattr("_dispatch_failed")?;
        if !failed.contains(&name)? {
            failed.call_method1("add", (&name,))?;
            let kwargs = with_kw(py, &[("exc_info", err.value(py).clone().into_any().unbind())])?;
            logger(py)?.call_method(
                "error",
                ("handling %s failed; further failures are not logged", &name),
                Some(&kwargs),
            )?;
        }
        Ok(())
    }

    /// Has this exact frame (sequence number included) arrived inside the
    /// duplicate window? A looped HEARTBEAT or COMMAND_ACK read twice is
    /// stale news that flips state back or confirms what was never confirmed.
    fn is_duplicate_frame(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        let get_buf = attr_or(msg, "get_msgbuf", py.None())?;
        if get_buf.is_none() {
            return Ok(false);
        }
        let raw = match get_buf
            .call0()
            .and_then(|b| py.import("builtins")?.getattr("bytes")?.call1((b,)))
        {
            Ok(r) => r,
            Err(err) if err.is_instance_of::<PyException>(py) => return Ok(false),
            Err(err) => return Err(err),
        };
        if !raw.is_truthy()? {
            return Ok(false);
        }
        let window = self.tunable(py, "DUPLICATE_FRAME_WINDOW_S")?;
        let now = self.monotonic(py)?;
        let mut recent = bridge.getattr("_recent_frames")?;
        let swept: f64 = bridge.getattr("_recent_frames_swept")?.extract()?;
        if now - swept > window {
            let kept = PyDict::new(py);
            for (frame, seen) in recent.downcast::<PyDict>()?.iter() {
                if now - seen.extract::<f64>()? <= window {
                    kept.set_item(frame, seen)?;
                }
            }
            bridge.setattr("_recent_frames", &kept)?;
            bridge.setattr("_recent_frames_swept", now)?;
            recent = kept.into_any();
        }
        let seen = recent.call_method1("get", (&raw,))?;
        recent.set_item(&raw, now)?;
        if !seen.is_none() && now - seen.extract::<f64>()? <= window {
            let count = bridge.getattr("_duplicate_frames")?.add(1)?;
            bridge.setattr("_duplicate_frames", count)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Hand a frame of the vehicle this link flies to the vehicle list; an
    /// other autopilot's heartbeat is noted once and ignored. Never raises.
    fn observe_fleet(
        &self,
        bridge: &Bound<'_, PyAny>,
        msg: &Bound<'_, PyAny>,
        name: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = bridge.py();
        let observed = (|| -> PyResult<()> {
            let source = int_of(&or(source_call(msg, "get_srcSystem")?, obj(py, 0))?)?;
            let heartbeat = name.eq("HEARTBEAT")?;
            if source.ne(bridge.getattr("_target_system")?)? {
                if heartbeat && source.is_truthy()? {
                    bridge.call_method1("_note_ignored_system", (msg, &source))?;
                }
                return Ok(());
            }
            let vehicle = heartbeat && is_vehicle_heartbeat(msg)?;
            let kwargs = with_kw(py, &[("vehicle_heartbeat", obj(py, vehicle))])?;
            bridge
                .getattr("fleet_tracker")?
                .call_method("observe", (msg,), Some(&kwargs))?;
            Ok(())
        })();
        match observed {
            Err(err) if err.is_instance_of::<PyException>(py) => {
                let kwargs = with_kw(py, &[("exc_info", obj(py, true))])?;
                logger(py)?.call_method("debug", ("fleet tracker failed",), Some(&kwargs))?;
                Ok(())
            }
            other => other,
        }
    }

    /// Tell the console, once per system and for at most 16, that another
    /// autopilot on this link is ignored.
    fn note_ignored_system(
        &self,
        bridge: &Bound<'_, PyAny>,
        hb: &Bound<'_, PyAny>,
        system: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = bridge.py();
        let ignored = bridge.getattr("_ignored_systems")?;
        if ignored.contains(system)? || ignored.len()? >= 16 {
            return Ok(());
        }
        if system.eq(self.env(py).getattr("GCS_SYSTEM_ID")?)? || !is_vehicle_heartbeat(hb)? {
            return Ok(());
        }
        ignored.call_method1("add", (system,))?;
        let target = bridge.getattr("_target_system")?;
        logger(py)?.call_method1(
            "info",
            ("ignoring autopilot sys=%d: this link flies sys=%d", system, &target),
        )?;
        let text = format!(
            "Another autopilot (system {}) is on this link and is ignored. This link flies system {}; a second vehicle needs a radio and a link of its own.",
            system.str()?,
            target.str()?
        );
        console(bridge, "LINK", &text, "warning")
    }

    /// Notice a vehicle that rebooted while the link stayed up: its uptime
    /// stepped back further than two frames out of order could explain.
    fn note_boot_time(&self, bridge: &Bound<'_, PyAny>, boot_ms: i64) -> PyResult<()> {
        let py = bridge.py();
        let last = bridge.getattr("_last_boot_ms")?;
        bridge.setattr("_last_boot_ms", boot_ms)?;
        if last.is_none() {
            return Ok(());
        }
        let last: i64 = last.extract()?;
        let backstep: i64 = self.env(py).getattr("VEHICLE_REBOOT_BACKSTEP_MS")?.extract()?;
        if boot_ms + backstep >= last {
            return Ok(());
        }
        let text = "The vehicle rebooted. Parameters are read again when next needed";
        logger(py)?.call_method1("info", ("%s (uptime %d ms after %d ms)", text, boot_ms, last))?;
        console(bridge, "LINK", text, "info")?;
        call0(bridge, "_reset_parameter_cache")?;
        call0(bridge, "_reset_param_metadata")?;
        bridge.setattr("_param_encoding_declared", "")?;
        bridge.setattr("_request_sent", false)?;
        if truthy(bridge, "_is_serial")? {
            call0(bridge, "_request_streams")?;
        }
        call0(bridge, "_request_version")?;
        call0(bridge, "_request_home")?;
        call0(bridge, "_schedule_message_intervals")?;
        Ok(())
    }

    /// Mark the link degraded once per connection cycle.
    fn mark_degraded_once(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if bridge.getattr("_degraded_warned")?.is_truthy()? {
            return Ok(());
        }
        bridge.setattr("_degraded_warned", true)?;
        update(
            bridge,
            vec![("link_status", obj(py, "degraded")), ("link_quality", obj(py, "poor"))],
        )?;
        logger(py)?.call_method1(
            "warning",
            (
                "Link degraded: heartbeat late (>%.0fs)",
                call0(bridge, "_warn_timeout")?,
            ),
        )?;
        Ok(())
    }

    /// From the aircraft this bridge flies? Source system 0 (no sender id)
    /// is accepted: this rejects an identified other node.
    fn is_from_vehicle(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<bool> {
        let source = source_call(msg, "get_srcSystem")?;
        Ok(!source.is_truthy()? || source.eq(bridge.getattr("_target_system")?)?)
    }

    /// Tighter: the flight controller's own component.
    fn is_from_autopilot(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<bool> {
        if !Self::from_vehicle(bridge, msg)? {
            return Ok(false);
        }
        let component = source_call(msg, "get_srcComponent")?;
        Ok(!component.is_truthy()? || component.eq(bridge.getattr("_target_component")?)?)
    }

    /// Is this COMMAND_ACK from our vehicle and, when addressed, to us?
    fn ack_is_for_us(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        let conn = bridge.getattr("_conn")?;
        if conn.is_none() {
            return Ok(false);
        }
        let source = source_call(msg, "get_srcSystem")?;
        if source.is_truthy()? && source.ne(bridge.getattr("_target_system")?)? {
            return Ok(false);
        }
        let component = source_call(msg, "get_srcComponent")?;
        if component.is_truthy()? && component.ne(bridge.getattr("_target_component")?)? {
            return Ok(false);
        }
        let target_system = attr_or(msg, "target_system", obj(py, 0))?;
        let expected_system = attr_or(&conn, "source_system", obj(py, 0))?;
        if target_system.is_truthy()? && expected_system.is_truthy()? && target_system.ne(&expected_system)? {
            return Ok(false);
        }
        let target_component = attr_or(msg, "target_component", obj(py, 0))?;
        let expected_component = attr_or(&conn, "source_component", obj(py, 0))?;
        Ok(!(target_component.is_truthy()?
            && expected_component.is_truthy()?
            && target_component.ne(&expected_component)?))
    }

    /// Wake every waiting command with -2, and a blocked mission upload.
    fn cancel_pending_commands(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        within(&bridge.getattr("_ack_lock")?, || {
            for pending in bridge.getattr("_pending_acks")?.call_method0("values")?.try_iter()? {
                let pending = pending?;
                pending.setattr("result", -2)?;
                pending.getattr("event")?.call_method0("set")?;
            }
            Ok(())
        })?;
        bridge.getattr("_mission_engine")?.call_method1("cancel_upload", ())?;
        Ok(())
    }

    /// A link, a vehicle on it, and a heartbeat inside the command timeout.
    fn connection_ready(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        if !bridge.getattr("_conn")?.is_truthy()? {
            return Ok(false);
        }
        if !snapshot(bridge)?.call_method1("get", ("connected",))?.is_truthy()? {
            return Ok(false);
        }
        Ok(!stale(bridge, "_heartbeat_timeout")?)
    }

    /// The receive loop of one connection cycle. Late heartbeats mark the
    /// link degraded, lost ones end the cycle (ConnectionError); a read that
    /// fails costs a short pause, and enough failures in a row end the cycle
    /// too rather than spin.
    fn receive_loop(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let backoff = self.env(py).getattr("RECV_ERROR_BACKOFF_S")?;
        let mut consecutive: i64 = 0;
        let check_stale = || -> PyResult<()> {
            if stale(bridge, "_warn_timeout")? {
                if stale(bridge, "_drop_timeout")? {
                    return Err(PyConnectionError::new_err("heartbeat timeout"));
                }
                call0(bridge, "_mark_degraded_once")?;
            }
            Ok(())
        };
        loop {
            if crate::bridge::thread::shutting_down()
                || !bridge.getattr("_running")?.call_method0("is_set")?.is_truthy()?
                || !bridge.getattr("_conn")?.is_truthy()?
            {
                return Ok(());
            }
            let conn = bridge.getattr("_conn")?;
            let step = (|| -> PyResult<()> {
                check_stale()?;
                let kwargs = with_kw(py, &[("blocking", obj(py, true)), ("timeout", obj(py, 1))])?;
                let msg = conn.call_method("recv_match", (), Some(&kwargs))?;
                consecutive = 0;
                call0(bridge, "_cleanup_statustext_chunks")?;
                if msg.is_none() || !conn.is(&bridge.getattr("_conn")?) {
                    return Ok(());
                }
                call1(bridge, "_process_frame", &msg)?;
                check_stale()
            })();
            match step {
                Ok(()) => {}
                Err(err) if err.is_instance_of::<PyConnectionError>(py) => return Err(err),
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    consecutive += 1;
                    let value = err.value(py);
                    logger(py)?.call_method1("debug", ("recv error (%d in a row): %s", consecutive, value))?;
                    let limit: i64 = self.env(py).getattr("RECV_ERROR_LIMIT")?.extract()?;
                    if consecutive >= limit {
                        let ended = PyConnectionError::new_err(format!("link read failed: {}", value.str()?));
                        ended.set_cause(py, Some(err));
                        return Err(ended);
                    }
                    call1(bridge, "_interruptible_sleep", &backoff)?;
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// COMMAND_LONG and its COMMAND_ACK. -2 without a link (or when it is
    /// swapped), -1 when no ACK arrives. An interim result does not end the
    /// wait; IN_PROGRESS stops the resends. The caller holds the operation
    /// lock, which keeps the one registry slot per command id safe.
    #[pyo3(signature = (bridge, command, params=None, timeout=None, retries=None, accept_in_progress=false, interim=None))]
    #[allow(clippy::too_many_arguments)]
    fn send_command_and_wait(
        &self,
        bridge: &Bound<'_, PyAny>,
        command: &Bound<'_, PyAny>,
        params: Option<&Bound<'_, PyAny>>,
        timeout: Option<&Bound<'_, PyAny>>,
        retries: Option<i64>,
        accept_in_progress: bool,
        interim: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PyObject> {
        let py = bridge.py();
        let interim = match interim {
            Some(i) => i.clone(),
            None => PyFrozenSet::empty(py)?.into_any(),
        };
        let kwargs = with_kw(
            py,
            &[
                ("accept_in_progress", obj(py, accept_in_progress)),
                ("interim", interim.unbind()),
            ],
        )?;
        let pending = self.env(py).getattr("_PendingAck")?.call((), Some(&kwargs))?;
        self.wait_for_ack(
            bridge,
            command,
            params,
            7,
            timeout,
            retries.unwrap_or(2),
            &pending,
            &|conn, attempt, p| {
                let mut args: Vec<PyObject> = vec![
                    bridge.getattr("_target_system")?.unbind(),
                    bridge.getattr("_target_component")?.unbind(),
                    command.clone().unbind(),
                    obj(py, if attempt == 0 { 0 } else { 1 }),
                ];
                args.extend(p);
                conn.getattr("mav")?
                    .call_method1("command_long_send", PyTuple::new(py, args)?)?;
                Ok(())
            },
            "send command %d failed: %s",
            true,
        )
    }

    /// COMMAND_INT, whose x and y carry a position at full precision, and
    /// its COMMAND_ACK; otherwise as :meth:`send_command_and_wait`.
    #[pyo3(signature = (bridge, command, frame, params=None, x=None, y=None, z=None, timeout=None, retries=None))]
    #[allow(clippy::too_many_arguments)]
    fn send_command_int_and_wait(
        &self,
        bridge: &Bound<'_, PyAny>,
        command: &Bound<'_, PyAny>,
        frame: &Bound<'_, PyAny>,
        params: Option<&Bound<'_, PyAny>>,
        x: Option<&Bound<'_, PyAny>>,
        y: Option<&Bound<'_, PyAny>>,
        z: Option<&Bound<'_, PyAny>>,
        timeout: Option<&Bound<'_, PyAny>>,
        retries: Option<i64>,
    ) -> PyResult<PyObject> {
        let py = bridge.py();
        let pending = self.env(py).getattr("_PendingAck")?.call0()?;
        let x = x.cloned().unwrap_or_else(|| obj(py, 0).into_bound(py));
        let y = y.cloned().unwrap_or_else(|| obj(py, 0).into_bound(py));
        let z = z.cloned().unwrap_or_else(|| PyFloat::new(py, 0.0).into_any());
        self.wait_for_ack(
            bridge,
            command,
            params,
            4,
            timeout,
            retries.unwrap_or(2),
            &pending,
            &|conn, _attempt, p| {
                let mut args: Vec<PyObject> = vec![
                    bridge.getattr("_target_system")?.unbind(),
                    bridge.getattr("_target_component")?.unbind(),
                    frame.clone().unbind(),
                    command.clone().unbind(),
                    obj(py, 0),
                    obj(py, 0),
                ];
                args.extend(p);
                args.extend([
                    int_of(&x)?.unbind(),
                    int_of(&y)?.unbind(),
                    py.get_type::<PyFloat>().call1((&z,))?.unbind(),
                ]);
                conn.getattr("mav")?
                    .call_method1("command_int_send", PyTuple::new(py, args)?)?;
                Ok(())
            },
            "send command_int %d failed: %s",
            false,
        )
    }

    /// The vehicle said IN_PROGRESS: wait for its verdict, never resend.
    fn await_final_ack(
        &self,
        bridge: &Bound<'_, PyAny>,
        pending: &Bound<'_, PyAny>,
        timeout: &Bound<'_, PyAny>,
    ) -> PyResult<PyObject> {
        let py = bridge.py();
        let kwargs = with_kw(py, &[("timeout", timeout.clone().unbind())])?;
        if pending
            .getattr("event")?
            .call_method("wait", (), Some(&kwargs))?
            .is_truthy()?
        {
            let result = pending.getattr("result")?;
            return Ok(if result.is_none() { obj(py, -1) } else { result.unbind() });
        }
        Ok(obj(py, constant_or("MAV_RESULT_IN_PROGRESS", 5)))
    }
}

impl ReceiveEngine {
    #[allow(clippy::too_many_arguments)]
    fn wait_for_ack(
        &self,
        bridge: &Bound<'_, PyAny>,
        command: &Bound<'_, PyAny>,
        params: Option<&Bound<'_, PyAny>>,
        width: usize,
        timeout: Option<&Bound<'_, PyAny>>,
        retries: i64,
        pending: &Bound<'_, PyAny>,
        send: &SendFn,
        failure_text: &str,
        interim: bool,
    ) -> PyResult<PyObject> {
        let py = bridge.py();
        if !truthy(bridge, "_connection_ready")? {
            return Ok(obj(py, -2));
        }
        let conn = bridge.getattr("_conn")?;
        let mut p: Vec<PyObject> = Vec::new();
        if let Some(params) = params.filter(|p| p.is_truthy().unwrap_or(true)) {
            for item in py.import("builtins")?.getattr("list")?.call1((params,))?.try_iter()? {
                if p.len() == width {
                    break;
                }
                p.push(item?.unbind());
            }
        }
        while p.len() < width {
            p.push(obj(py, f64::NAN));
        }
        let timeout = match timeout {
            Some(t) => t.clone(),
            None => PyFloat::new(py, 3.0).into_any(),
        };
        within(&bridge.getattr("_ack_lock")?, || {
            bridge.getattr("_pending_acks")?.set_item(command, pending)
        })?;
        let outcome = (|| -> PyResult<PyObject> {
            for attempt in 0..=retries {
                if !conn.is(&bridge.getattr("_conn")?) || !truthy(bridge, "_connection_ready")? {
                    return Ok(obj(py, -2));
                }
                let sent = within(&bridge.getattr("_send_lock")?, || -> PyResult<bool> {
                    if !conn.is(&bridge.getattr("_conn")?) {
                        return Ok(false);
                    }
                    send(&conn, attempt, p.iter().map(|v| v.clone_ref(py)).collect())?;
                    Ok(true)
                });
                match sent {
                    Ok(true) => {}
                    Ok(false) => return Ok(obj(py, -2)),
                    Err(err) if err.is_instance_of::<PyException>(py) => {
                        logger(py)?.call_method1("error", (failure_text, command, err.value(py)))?;
                        return Ok(obj(py, -2));
                    }
                    Err(err) => return Err(err),
                }
                if !conn.is(&bridge.getattr("_conn")?) {
                    return Ok(obj(py, -2));
                }
                let kwargs = with_kw(py, &[("timeout", timeout.clone().unbind())])?;
                if pending
                    .getattr("event")?
                    .call_method("wait", (), Some(&kwargs))?
                    .is_truthy()?
                {
                    let result = pending.getattr("result")?;
                    return Ok(if result.is_none() { obj(py, -1) } else { result.unbind() });
                }
                if interim {
                    let early = pending.getattr("interim_result")?;
                    if !early.is_none() {
                        return Ok(early.unbind());
                    }
                }
                if pending.getattr("in_progress")?.is_truthy()? {
                    return Ok(bridge.call_method1("_await_final_ack", (pending, &timeout))?.unbind());
                }
                logger(py)?.call_method1("warning", ("Command %d ACK timeout (attempt %d)", command, attempt + 1))?;
            }
            Ok(obj(py, -1))
        })();
        let cleared = within(&bridge.getattr("_ack_lock")?, || {
            let registry = bridge.getattr("_pending_acks")?;
            if registry.call_method1("get", (command,))?.is(pending) {
                registry.call_method1("pop", (command, py.None()))?;
            }
            Ok(())
        });
        let outcome = outcome?;
        cleared?;
        Ok(outcome)
    }
}

pub fn register(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<ReceiveEngine>()?;
    m.add_function(wrap_pyfunction!(is_plausible_heartbeat, m)?)?;
    m.add_function(wrap_pyfunction!(is_vehicle_heartbeat, m)?)?;
    m.add_function(wrap_pyfunction!(sensor_health, m)?)?;
    m.add_function(wrap_pyfunction!(prearm_ok, m)?)?;
    m.add_function(wrap_pyfunction!(rangefinder_distance_m, m)?)?;
    m.add_function(wrap_pyfunction!(gps_altitude_m, m)?)?;
    m.add_function(wrap_pyfunction!(gps_accuracy_m, m)?)?;
    m.add_function(wrap_pyfunction!(decode_git_hash, m)?)?;
    m.add("NON_VEHICLE_TYPES", PyFrozenSet::new(py, non_vehicle_types())?)?;
    m.add(
        "NON_AUTOPILOT_COMPONENTS",
        PyFrozenSet::new(py, non_autopilot_components())?,
    )?;
    m.add("HEARTBEAT_LIMITS", PyTuple::new(py, HEARTBEAT_LIMITS)?)?;
    m.add("DOWNWARD_ORIENTATION", DOWNWARD_ORIENTATION)?;
    Ok(())
}
