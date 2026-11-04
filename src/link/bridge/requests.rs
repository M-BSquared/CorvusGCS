//! What the bridge asks of the vehicle on its own: the telemetry rates
//! (per message SET_MESSAGE_INTERVAL, the legacy REQUEST_DATA_STREAM only
//! where the vehicle shows it needs it), home and the firmware version, a
//! warning when an ArduPilot vehicle will ignore our sticks, and MAVLink 2
//! signing with its key and the three messages exempt from it.
//!
//! The constants, the clock and `os` (whose `open` a test replaces) come
//! from ``corvus.mavlink_bridge``; every step is called through the bridge.

use pyo3::exceptions::{PyConnectionError, PyException, PyPermissionError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyFrozenSet, PyList, PyModule, PyTuple};

use crate::bridge::receive::within;
use crate::protocol::host::{caught_by, console, logger, obj, set_error, truthy};
use crate::wire::defs::constant_or;

/// RADIO_STATUS (the SiK modem, which has no key), ADSB_VEHICLE and
/// COLLISION (transponders outside the vehicle): none can command anything.
fn unsigned_ids() -> [i64; 3] {
    [
        constant_or("MAVLINK_MSG_ID_RADIO_STATUS", 109),
        constant_or("MAVLINK_MSG_ID_ADSB_VEHICLE", 246),
        constant_or("MAVLINK_MSG_ID_COLLISION", 247),
    ]
}

/// May this message arrive unsigned on a signed link?
#[pyfunction]
pub fn allow_unsigned_signed_link(_mav: &Bound<'_, PyAny>, msg_id: &Bound<'_, PyAny>) -> PyResult<bool> {
    let id = msg_id.py().get_type::<pyo3::types::PyInt>().call1((msg_id,))?;
    Ok(id
        .extract::<i64>()
        .map(|v| unsigned_ids().contains(&v))
        .unwrap_or(false))
}

const KEY_FORMAT: &str = "MAVLink signing key must be 32 raw bytes or 64 hex characters";

/// The opt-in signing key from the owner only file the environment names,
/// or None. Opened without following a link and, on Windows, in binary
/// mode: a text mode read stops at 0x1A, which one key in eight contains.
#[pyfunction]
pub fn load_signing_key(py: Python<'_>, env: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let os = py.import("os")?;
    let variable = env.getattr("MAVLINK_SIGNING_KEY_FILE_ENV")?;
    let raw = os.getattr("environ")?.call_method1("get", (variable,))?;
    let raw = if raw.is_truthy()? {
        raw
    } else {
        obj(py, "").into_bound(py)
    };
    let path = raw.call_method0("strip")?;
    if !path.is_truthy()? {
        return Ok(py.None());
    }
    let mut flags = os.getattr("O_RDONLY")?;
    for name in ["O_NOFOLLOW", "O_BINARY"] {
        if os.hasattr(name)? {
            flags = flags.bitor(os.getattr(name)?)?;
        }
    }
    let fd = os.call_method1("open", (&path, flags))?;
    let read = (|| -> PyResult<Bound<'_, PyAny>> {
        let info = os.call_method1("fstat", (&fd,))?;
        let mode = info.getattr("st_mode")?;
        if !py.import("stat")?.call_method1("S_ISREG", (&mode,))?.is_truthy()? {
            return Err(PyValueError::new_err("MAVLink signing key path is not a regular file"));
        }
        if os.getattr("name")?.ne("nt")? {
            if mode.bitand(0o077)?.is_truthy()? {
                return Err(PyPermissionError::new_err(
                    "MAVLink signing key file must have mode 0600",
                ));
            }
            if os.hasattr("getuid")? && info.getattr("st_uid")?.ne(os.call_method0("getuid")?)? {
                return Err(PyPermissionError::new_err(
                    "MAVLink signing key file must be owned by this user",
                ));
            }
        }
        os.call_method1("read", (&fd, 129))
    })();
    // As Python's finally: a failing close wins over a failing read.
    os.call_method1("close", (&fd,))?;
    let data = read?;
    let bytes: Vec<u8> = data.extract()?;
    let stripped = data.call_method0("strip")?;
    let key: Vec<u8> = if bytes.len() == 32 {
        bytes
    } else if stripped.len()? == 64 {
        let decoded = stripped
            .call_method1("decode", ("ascii",))
            .and_then(|text| py.get_type::<PyBytes>().call_method1("fromhex", (text,)));
        match decoded {
            Ok(key) => key.extract()?,
            Err(err)
                if caught_by(py, &err, &["ValueError"])
                    || err.is_instance_of::<pyo3::exceptions::PyUnicodeError>(py) =>
            {
                let raised = PyValueError::new_err(KEY_FORMAT);
                raised.set_cause(py, Some(err));
                return Err(raised);
            }
            Err(err) => return Err(err),
        }
    } else {
        return Err(PyValueError::new_err(KEY_FORMAT));
    };
    if key.iter().all(|b| *b == 0) {
        return Err(PyValueError::new_err("MAVLink signing key must not be all zero"));
    }
    Ok(PyBytes::new(py, &key).into_any().unbind())
}

fn kw<'py>(py: Python<'py>, pairs: &[(&str, PyObject)]) -> PyResult<Bound<'py, PyDict>> {
    let kwargs = PyDict::new(py);
    for (k, v) in pairs {
        kwargs.set_item(*k, v)?;
    }
    Ok(kwargs)
}

/// The bridge's own requests to the vehicle.
#[pyclass(name = "RequestEngine", module = "corvus_link.bridge", frozen)]
pub struct RequestEngine {
    env: PyObject,
}

impl RequestEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn mavlink<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr("mavutil")?.getattr("mavlink")
    }

    fn table<'py>(&self, py: Python<'py>, rows: &[(&str, i64)]) -> PyResult<Bound<'py, PyDict>> {
        let m = self.mavlink(py)?;
        let out = PyDict::new(py);
        for (name, value) in rows {
            out.set_item(m.getattr(*name)?, value)?;
        }
        Ok(out)
    }

    /// `conn.mav.command_long_send(ts, tc, command, 0, *params)` under the
    /// send lock, unless the link changed; an Exception is swallowed.
    fn fire(
        &self,
        bridge: &Bound<'_, PyAny>,
        conn: Option<&Bound<'_, PyAny>>,
        command: &Bound<'_, PyAny>,
        params: &[f64],
    ) -> PyResult<Option<PyErr>> {
        let py = bridge.py();
        let sent = within(&bridge.getattr("_send_lock")?, || -> PyResult<()> {
            let link = match conn {
                Some(conn) => {
                    if !conn.is(&bridge.getattr("_conn")?) {
                        return Ok(());
                    }
                    conn.clone()
                }
                None => bridge.getattr("_conn")?,
            };
            let mut args: Vec<PyObject> = vec![
                bridge.getattr("_target_system")?.unbind(),
                bridge.getattr("_target_component")?.unbind(),
                command.clone().unbind(),
                obj(py, 0),
            ];
            args.extend(params.iter().map(|p| obj(py, *p)));
            link.getattr("mav")?
                .call_method1("command_long_send", PyTuple::new(py, args)?)?;
            Ok(())
        });
        match sent {
            Ok(()) => Ok(None),
            Err(err) if err.is_instance_of::<PyException>(py) => Ok(Some(err)),
            Err(err) => Err(err),
        }
    }
}

#[pymethods]
impl RequestEngine {
    /// *env* is ``corvus.mavlink_bridge``.
    #[new]
    fn new(env: PyObject) -> Self {
        RequestEngine { env }
    }

    /// REQUEST_DATA_STREAM rates, throttled on a radio link. Only ArduPilot
    /// honours them; PX4 ignores the message.
    fn stream_rates<'py>(&self, bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
        let slow = truthy(bridge, "_is_slow_link")?;
        let rows: [(&str, i64); 7] = if slow {
            [
                ("MAV_DATA_STREAM_EXTENDED_STATUS", 2),
                ("MAV_DATA_STREAM_POSITION", 5),
                ("MAV_DATA_STREAM_EXTRA1", 10),
                ("MAV_DATA_STREAM_EXTRA2", 5),
                ("MAV_DATA_STREAM_EXTRA3", 2),
                ("MAV_DATA_STREAM_RAW_SENSORS", 0),
                ("MAV_DATA_STREAM_RC_CHANNELS", 0),
            ]
        } else {
            [
                ("MAV_DATA_STREAM_EXTENDED_STATUS", 2),
                ("MAV_DATA_STREAM_POSITION", 10),
                ("MAV_DATA_STREAM_EXTRA1", 50),
                ("MAV_DATA_STREAM_EXTRA2", 10),
                ("MAV_DATA_STREAM_EXTRA3", 5),
                ("MAV_DATA_STREAM_RAW_SENSORS", 5),
                ("MAV_DATA_STREAM_RC_CHANNELS", 5),
            ]
        };
        self.table(bridge.py(), &rows)
    }

    /// Send the legacy REQUEST_DATA_STREAM set, at most once per link.
    fn request_streams(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let conn = bridge.getattr("_conn")?;
        if bridge.getattr("_request_sent")?.is_truthy()? || conn.is_none() {
            return Ok(());
        }
        for (stream, rate) in bridge.call_method0("_stream_rates")?.downcast::<PyDict>()?.iter() {
            if rate.eq(0)? {
                continue;
            }
            let sent = within(&bridge.getattr("_send_lock")?, || -> PyResult<bool> {
                if !conn.is(&bridge.getattr("_conn")?) {
                    return Ok(false);
                }
                conn.getattr("mav")?.call_method1(
                    "request_data_stream_send",
                    (
                        bridge.getattr("_target_system")?,
                        bridge.getattr("_target_component")?,
                        &stream,
                        &rate,
                        1,
                    ),
                )?;
                Ok(true)
            });
            match sent {
                Ok(false) => return Ok(()),
                Ok(true) => {}
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    logger(py)?.call_method1("debug", ("REQUEST_DATA_STREAM %s failed: %s", &stream, err.value(py)))?;
                }
                Err(err) => return Err(err),
            }
        }
        bridge.setattr("_request_sent", true)
    }

    /// Per message SET_MESSAGE_INTERVAL targets in microseconds, throttled
    /// to fit a 57 kbps radio. No HEARTBEAT: PX4 answers that with FAILED.
    fn message_intervals<'py>(&self, bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
        let rows: &[(&str, i64)] = if truthy(bridge, "_is_slow_link")? {
            &[
                ("MAVLINK_MSG_ID_ATTITUDE", 100_000),
                ("MAVLINK_MSG_ID_GLOBAL_POSITION_INT", 200_000),
                ("MAVLINK_MSG_ID_VFR_HUD", 200_000),
                ("MAVLINK_MSG_ID_SYS_STATUS", 1_000_000),
                ("MAVLINK_MSG_ID_GPS_RAW_INT", 1_000_000),
                ("MAVLINK_MSG_ID_HOME_POSITION", 5_000_000),
                ("MAVLINK_MSG_ID_EXTENDED_SYS_STATE", 1_000_000),
                ("MAVLINK_MSG_ID_BATTERY_STATUS", 2_000_000),
                ("MAVLINK_MSG_ID_MISSION_CURRENT", 1_000_000),
                ("MAVLINK_MSG_ID_SYSTEM_TIME", 2_000_000),
            ]
        } else {
            &[
                ("MAVLINK_MSG_ID_ATTITUDE", 20_000),
                ("MAVLINK_MSG_ID_GLOBAL_POSITION_INT", 100_000),
                ("MAVLINK_MSG_ID_VFR_HUD", 100_000),
                ("MAVLINK_MSG_ID_SYS_STATUS", 1_000_000),
                ("MAVLINK_MSG_ID_GPS_RAW_INT", 1_000_000),
                ("MAVLINK_MSG_ID_HOME_POSITION", 1_000_000),
                ("MAVLINK_MSG_ID_EXTENDED_SYS_STATE", 1_000_000),
                ("MAVLINK_MSG_ID_RC_CHANNELS", 200_000),
                ("MAVLINK_MSG_ID_BATTERY_STATUS", 1_000_000),
                ("MAVLINK_MSG_ID_MISSION_CURRENT", 1_000_000),
                ("MAVLINK_MSG_ID_SYSTEM_TIME", 1_000_000),
            ]
        };
        self.table(bridge.py(), rows)
    }

    /// Warn once per connection when an ArduPilot vehicle takes sticks only
    /// from another ground station id: it drops ours without a word.
    fn check_gcs_authority(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let env = self.env(py);
        let ardupilot = env.getattr("autopilot_dialect")?.getattr("STACK_ARDUPILOT")?;
        if bridge.getattr("_dialect")?.getattr("stack")?.ne(ardupilot)? {
            return Ok(());
        }
        if !truthy(bridge, "_connection_ready")? {
            return Ok(());
        }
        let names = bridge.getattr("_GCS_AUTHORITY_PARAMS")?;
        let kwargs = kw(py, &[("timeout", obj(py, 3.0))])?;
        let values = bridge.call_method(
            "fetch_params",
            (py.get_type::<PyList>().call1((&names,))?,),
            Some(&kwargs),
        )?;
        let mut found = None;
        for name in names.try_iter()? {
            let name = name?;
            if values.contains(&name)? {
                found = Some(name);
                break;
            }
        }
        let Some(name) = found else {
            return Ok(());
        };
        let current = py
            .import("builtins")?
            .getattr("round")?
            .call1((values.get_item(&name)?,))?;
        let current = py.get_type::<pyo3::types::PyInt>().call1((current,))?;
        let ours = env.getattr("GCS_SYSTEM_ID")?;
        if current.eq(&ours)? {
            return Ok(());
        }
        let (name, current, ours) = (
            name.str()?.to_string(),
            current.str()?.to_string(),
            ours.str()?.to_string(),
        );
        let text = format!(
            "This vehicle only accepts stick input from ground station {current} ({name}), and Corvus is {ours}, so the joystick will be ignored until {name} is set to {ours} in Parameters. Flight commands and mode changes are unaffected."
        );
        logger(py)?.call_method1("info", ("%s", &text))?;
        console(bridge, "JOYSTICK", &text, "warning")?;
        bridge.call_method1("_store_update_warning", (&text, "warning"))?;
        Ok(())
    }

    /// The connect time interval batch, and the evidence for the legacy
    /// fallback: UNSUPPORTED means the command is unknown (fall back and
    /// stop), no ACK means silence (fall back once as insurance, stop after a
    /// few in a row), anything else proves the modern path works.
    fn request_message_intervals(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let limit: i64 = self.env(py).getattr("INTERVAL_REQUEST_SILENT_LIMIT")?.extract()?;
        let unsupported = constant_or("MAV_RESULT_UNSUPPORTED", 3);
        let mut silent: i64 = 0;
        for (msg_id, interval) in bridge.call_method0("_message_intervals")?.downcast::<PyDict>()?.iter() {
            let kwargs = kw(py, &[("quiet", obj(py, true))])?;
            let answered = bridge
                .call_method("_set_message_interval", (&msg_id, &interval), Some(&kwargs))
                .and_then(|pair| {
                    let (_ok, result): (Bound<'_, PyAny>, Bound<'_, PyAny>) = pair.extract()?;
                    Ok(result)
                });
            let result = match answered {
                Ok(result) => result,
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    logger(py)?.call_method1(
                        "debug",
                        ("SET_MESSAGE_INTERVAL msg %d failed: %s", &msg_id, err.value(py)),
                    )?;
                    continue;
                }
                Err(err) => return Err(err),
            };
            if result.eq(-2)? {
                return Ok(());
            }
            if result.eq(unsupported)? {
                logger(py)?.call_method1(
                    "info",
                    ("SET_MESSAGE_INTERVAL unsupported (msg %d), falling back to REQUEST_DATA_STREAM for this connection", &msg_id),
                )?;
                bridge.call_method0("_request_streams")?;
                return Ok(());
            }
            if result.eq(-1)? {
                bridge.call_method0("_request_streams")?;
                silent += 1;
                if silent >= limit {
                    logger(py)?.call_method1(
                        "info",
                        ("no COMMAND_ACK for %d interval requests, leaving the rest to REQUEST_DATA_STREAM for this connection", silent),
                    )?;
                    return Ok(());
                }
            } else {
                silent = 0;
            }
        }
        Ok(())
    }

    /// Ask for HOME_POSITION now; fire and forget.
    fn request_home(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let conn = bridge.getattr("_conn")?;
        if conn.is_none() {
            return Ok(());
        }
        let home: f64 = bridge.getattr("_MSG_ID_HOME_POSITION")?.extract()?;
        let nan = f64::NAN;
        let command = obj(py, constant_or("MAV_CMD_REQUEST_MESSAGE", 512)).into_bound(py);
        if let Some(err) = self.fire(bridge, Some(&conn), &command, &[home, nan, nan, nan, nan, nan, nan])? {
            logger(py)?.call_method1("debug", ("HOME_POSITION request failed: %s", err.value(py)))?;
        }
        Ok(())
    }

    /// Ask for AUTOPILOT_VERSION three ways (REQUEST_MESSAGE, the deprecated
    /// capabilities command, the legacy request message), fire and forget,
    /// then schedule the one retry.
    fn request_version(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if bridge.getattr("_conn")?.is_truthy()? {
            let nan = f64::NAN;
            let version: f64 = bridge.getattr("_MSG_ID_AUTOPILOT_VERSION")?.extract()?;
            let requests = [
                (
                    constant_or("MAV_CMD_REQUEST_MESSAGE", 512),
                    [version, nan, nan, nan, nan, nan, nan],
                ),
                (
                    constant_or("MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES", 520),
                    [1.0, nan, nan, nan, nan, nan, nan],
                ),
            ];
            for (command, params) in requests {
                self.fire(bridge, None, &obj(py, command).into_bound(py), &params)?;
            }
            let sent = within(&bridge.getattr("_send_lock")?, || {
                bridge.getattr("_conn")?.getattr("mav")?.call_method1(
                    "autopilot_version_request_send",
                    (bridge.getattr("_target_system")?, bridge.getattr("_target_component")?),
                )?;
                Ok(())
            });
            if let Err(err) = sent {
                if !err.is_instance_of::<PyException>(py) {
                    return Err(err);
                }
            }
        }
        bridge.call_method0("_schedule_version_retry")?;
        Ok(())
    }

    /// MAV_CMD_SET_MESSAGE_INTERVAL and its ACK, as (accepted, result). The
    /// caller holds the operation lock. *quiet* keeps the outcome off the
    /// console and the status bar; the reason is still the last error.
    fn set_message_interval<'py>(
        &self,
        bridge: &Bound<'py, PyAny>,
        msg_id: &Bound<'py, PyAny>,
        interval_us: &Bound<'py, PyAny>,
        quiet: bool,
    ) -> PyResult<Bound<'py, PyTuple>> {
        let py = bridge.py();
        set_error(bridge, "")?;
        if !truthy(bridge, "_connection_ready")? {
            return PyTuple::new(py, [obj(py, false), obj(py, -2)]);
        }
        let float =
            |v: &Bound<'_, PyAny>| -> PyResult<f64> { py.get_type::<pyo3::types::PyFloat>().call1((v,))?.extract() };
        let nan = f64::NAN;
        let params = PyList::new(py, [float(msg_id)?, float(interval_us)?, nan, nan, nan, nan, nan])?;
        let kwargs = kw(py, &[("timeout", obj(py, 3.0)), ("retries", obj(py, 1))])?;
        let command = constant_or("MAV_CMD_SET_MESSAGE_INTERVAL", 511);
        let result = bridge.call_method("_send_command_and_wait", (command, params), Some(&kwargs))?;
        if result.ne(constant_or("MAV_RESULT_ACCEPTED", 0))? {
            if quiet {
                let text = self
                    .env(py)
                    .getattr("MAV_RESULT_TEXT")?
                    .call_method1("get", (&result, format!("RESULT_{}", result.str()?)))?;
                set_error(bridge, &format!("Set message interval failed: {}", text.str()?))?;
                logger(py)?.call_method1("debug", ("SET_MESSAGE_INTERVAL msg %d: result %s", msg_id, &result))?;
                return PyTuple::new(py, [obj(py, false), result.unbind()]);
            }
            let verdict = bridge.call_method1("_command_failure", ("Set message interval", &result))?;
            return PyTuple::new(py, [verdict.unbind(), result.unbind()]);
        }
        if !quiet {
            let text = format!("Message {} interval set to {} us", msg_id.str()?, interval_us.str()?);
            console(bridge, "STREAM", &text, "success")?;
        }
        PyTuple::new(py, [obj(py, true), result.unbind()])
    }

    /// Turn on MAVLink 2 signing when a key is configured. A key that cannot
    /// be loaded, or a link that will not sign, fails the connection rather
    /// than silently downgrading to an unsigned one.
    fn apply_signing(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let env = self.env(py);
        let fail = |text: String, err: PyErr| -> PyResult<()> {
            logger(py)?.call_method1("error", ("%s", &text))?;
            let kwargs = kw(py, &[("link_error", obj(py, text.as_str()))])?;
            bridge.getattr("_store")?.call_method("update", (), Some(&kwargs))?;
            let raised = PyConnectionError::new_err(text);
            raised.set_cause(py, Some(err));
            Err(raised)
        };
        let key = match env.getattr("_load_signing_key")?.call0() {
            Ok(key) => key,
            Err(err)
                if err.is_instance_of::<pyo3::exceptions::PyOSError>(py) || caught_by(py, &err, &["ValueError"]) =>
            {
                let text = format!("MAVLink signing key unusable: {}", err.value(py).str()?);
                return fail(text, err);
            }
            Err(err) => return Err(err),
        };
        if key.is_none() {
            return Ok(());
        }
        let enabled = (|| -> PyResult<()> {
            let conn = bridge.getattr("_conn")?;
            let version = match conn.getattr("WIRE_PROTOCOL_VERSION") {
                Ok(v) => v,
                Err(err) if caught_by(py, &err, &["AttributeError"]) => obj(py, "2.0").into_bound(py),
                Err(err) => return Err(err),
            };
            if version.ne("2.0")? {
                conn.call_method1("auto_mavlink_version", (PyBytes::new(py, b"\xfd"),))?;
            }
            let kwargs = kw(
                py,
                &[
                    ("sign_outgoing", obj(py, true)),
                    (
                        "allow_unsigned_callback",
                        env.getattr("_allow_unsigned_signed_link")?.unbind(),
                    ),
                ],
            )?;
            conn.call_method("setup_signing", (&key,), Some(&kwargs))?;
            Ok(())
        })();
        match enabled {
            Err(err) if err.is_instance_of::<PyException>(py) => {
                let text = format!("MAVLink signing could not be enabled: {}", err.value(py).str()?);
                return fail(text, err);
            }
            Err(err) => return Err(err),
            Ok(()) => {}
        }
        logger(py)?.call_method1(
            "info",
            ("MAVLink 2 signing enabled for %s", bridge.getattr("_conn_str")?),
        )?;
        console(bridge, "LINK", "MAVLink signing enabled", "success")
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<RequestEngine>()?;
    m.add_function(wrap_pyfunction!(allow_unsigned_signed_link, m)?)?;
    m.add_function(wrap_pyfunction!(load_signing_key, m)?)?;
    m.add(
        "SIGNING_UNSIGNED_MESSAGE_IDS",
        PyFrozenSet::new(m.py(), unsigned_ids())?,
    )?;
    Ok(())
}
