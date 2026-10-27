//! The bridge's connection cycle and the threads it runs on: start and stop,
//! the connect/receive cycle with its reconnect backoff, connecting (opening
//! the link, the first heartbeat, latching onto the aircraft), the GCS
//! heartbeat at 1 Hz with the Remote ID broadcast riding on it, and the two
//! deferred workers (message intervals once the receive loop drains the
//! socket, one AUTOPILOT_VERSION retry).
//!
//! The threads are `CoreThread`s; the loops on them call the bridge's
//! Python methods, so a bridge or a test that replaces one keeps doing so,
//! and read the clock, `random` and `mavutil` from ``corvus.mavlink_bridge``.

use pyo3::exceptions::{PyConnectionError, PyException, PyOSError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule, PyTuple};

use crate::bridge::receive::within;
use crate::bridge::thread::{self, shutting_down};
use crate::protocol::host::{caught_by, logger, obj, truthy};
use crate::wire::defs::constant_or;

fn kw<'py>(py: Python<'py>, pairs: &[(&str, PyObject)]) -> PyResult<Bound<'py, PyDict>> {
    let kwargs = PyDict::new(py);
    for (k, v) in pairs {
        kwargs.set_item(*k, v)?;
    }
    Ok(kwargs)
}

fn update(bridge: &Bound<'_, PyAny>, fields: Vec<(&str, PyObject)>) -> PyResult<()> {
    let kwargs = kw(bridge.py(), &fields)?;
    bridge.getattr("_store")?.call_method("update", (), Some(&kwargs))?;
    Ok(())
}

fn is_set(bridge: &Bound<'_, PyAny>, event: &str) -> PyResult<bool> {
    bridge.getattr(event)?.call_method0("is_set")?.is_truthy()
}

fn running(bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
    Ok(!shutting_down() && is_set(bridge, "_running")?)
}

fn alive(handle: &Bound<'_, PyAny>) -> PyResult<bool> {
    Ok(handle.is_truthy()? && handle.call_method0("is_alive")?.is_truthy()?)
}

/// `getattr(o, method, lambda: 0)()`.
fn source_call<'py>(o: &Bound<'py, PyAny>, method: &str) -> PyResult<Bound<'py, PyAny>> {
    match o.getattr(method) {
        Ok(f) => f.call0(),
        Err(err) if caught_by(o.py(), &err, &["AttributeError"]) => Ok(obj(o.py(), 0).into_bound(o.py())),
        Err(err) => Err(err),
    }
}

/// `getattr(o, method, lambda: "?")()`, for a log line.
fn named_source<'py>(o: &Bound<'py, PyAny>, method: &str) -> PyResult<Bound<'py, PyAny>> {
    match o.getattr(method) {
        Ok(f) => f.call0(),
        Err(err) if caught_by(o.py(), &err, &["AttributeError"]) => Ok(obj(o.py(), "?").into_bound(o.py())),
        Err(err) => Err(err),
    }
}

fn close_quietly(conn: &Bound<'_, PyAny>) -> PyResult<()> {
    match conn.call_method0("close") {
        Err(err) if !err.is_instance_of::<PyException>(conn.py()) => Err(err),
        _ => Ok(()),
    }
}

fn debug_exc(py: Python<'_>, text: &str) -> PyResult<()> {
    let kwargs = kw(py, &[("exc_info", obj(py, true))])?;
    logger(py)?.call_method("debug", (text,), Some(&kwargs))?;
    Ok(())
}

/// One request the version retry puts on the link.
type Request<'a> = dyn Fn(&Bound<'_, PyAny>) -> PyResult<()> + 'a;

/// The connection cycle of one bridge.
#[pyclass(name = "LinkCycle", module = "corvus_link.bridge", frozen)]
pub struct LinkCycle {
    env: PyObject,
}

impl LinkCycle {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn monotonic(&self, py: Python<'_>) -> PyResult<f64> {
        self.env(py).getattr("time")?.call_method0("monotonic")?.extract()
    }

    fn constant<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr(name)
    }

    fn superseded_on(bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        bridge.call_method0("_superseded")?.is_truthy()
    }

    /// Open the link for this bridge's connection string.
    fn open<'py>(&self, bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = bridge.py();
        let mavutil = self.env(py).getattr("mavutil")?;
        let system = self.constant(py, "GCS_SYSTEM_ID")?.unbind();
        let component = self.constant(py, "GCS_COMPONENT_ID")?.unbind();
        if truthy(bridge, "_is_serial")? {
            let parsed = bridge.call_method1("_parse_serial", (bridge.getattr("_conn_str")?,))?;
            let kwargs = kw(
                py,
                &[
                    ("baud", parsed.get_item(1)?.unbind()),
                    ("source_system", system),
                    ("source_component", component),
                ],
            )?;
            mavutil.call_method("mavlink_connection", (parsed.get_item(0)?,), Some(&kwargs))
        } else {
            let kwargs = kw(
                py,
                &[
                    ("timeout", obj(py, 2)),
                    ("source_system", system),
                    ("source_component", component),
                ],
            )?;
            mavutil.call_method("mavlink_connection", (bridge.getattr("_conn_str")?,), Some(&kwargs))
        }
    }

    /// Forget everything the previous connection taught this bridge.
    fn reset_for_connect(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        for method in ["_reset_shell_state", "_reset_parameter_cache", "_reset_param_metadata"] {
            bridge.call_method0(method)?;
        }
        bridge.setattr("_request_sent", false)?;
        bridge.setattr("_param_encoding_declared", "")?;
        bridge.setattr("_firmware_version", py.None())?;
        bridge.setattr("_last_boot_ms", py.None())?;
        bridge.call_method0("_clear_prearm_reasons")?;
        bridge.setattr("_prearm_requested_at", 0.0)?;
        bridge.setattr("_kill_evidence_at", 0.0)?;
        bridge.setattr("_prearm_last", py.None())?;
        bridge.setattr("_prearm_report_started_at", 0.0)?;
        bridge.setattr("_prearm_report_kill", false)?;
        bridge.setattr("_home_alt_amsl", py.None())?;
        bridge.setattr("_position_home_alt_amsl", py.None())?;
        // The displayed home goes too: a marker must be something the
        // aircraft said in this session or nothing at all.
        update(
            bridge,
            vec![("home", pyo3::types::PyList::new(py, [0.0, 0.0])?.into_any().unbind())],
        )?;
        bridge.call_method0("_forget_link_quality")?;
        bridge.setattr("_degraded_warned", false)?;
        bridge.call_method0("_forget_battery_cells")?;
        bridge.call_method0("_forget_gnss_integrity")?;
        bridge.call_method0("_forget_remote_id_session")?;
        bridge.call_method0("_forget_vehicle_mission")?;
        bridge.setattr("_vehicle_mission_id", py.None())?;
        bridge.setattr("_vehicle_fence", py.None())?;
        update(
            bridge,
            vec![
                ("mission_state", obj(py, "")),
                ("mission_seq", obj(py, -1)),
                ("mission_reached", obj(py, -1)),
                ("mission_total", obj(py, -1)),
            ],
        )?;
        update(
            bridge,
            vec![
                ("link_status", obj(py, "connecting")),
                ("link_connection", bridge.getattr("_conn_str")?.unbind()),
                ("link_error", obj(py, "")),
            ],
        )
    }

    /// The operator facing reason a link would not open, or the error itself.
    fn open_failure(&self, bridge: &Bound<'_, PyAny>, err: PyErr) -> PyResult<PyErr> {
        let py = bridge.py();
        let exc = err.value(py).clone();
        let message = if truthy(bridge, "_is_serial")? {
            Some(bridge.call_method1("_serial_open_error", (&exc,))?.str()?.to_string())
        } else {
            let code = match exc.getattr("errno") {
                Ok(c) => c,
                Err(e) if caught_by(py, &e, &["AttributeError"]) => py.None().into_bound(py),
                Err(e) => return Err(e),
            };
            if code.eq(self.env(py).getattr("errno")?.getattr("EADDRINUSE")?)? {
                Some(format!(
                    "{}: port already in use by another program (QGroundControl, MAVROS/MAVSDK or a second Corvus). Close it or connect on a different port",
                    bridge.getattr("_conn_str")?.str()?
                ))
            } else {
                None
            }
        };
        let Some(message) = message else {
            return Ok(err);
        };
        update(bridge, vec![("link_error", obj(py, message.as_str()))])?;
        let raised = PyConnectionError::new_err(message);
        raised.set_cause(py, Some(err));
        Ok(raised)
    }
}

#[pymethods]
impl LinkCycle {
    /// *env* is ``corvus.mavlink_bridge``.
    #[new]
    fn new(env: PyObject) -> Self {
        LinkCycle { env }
    }

    /// Start the connect/receive thread, unless a healthy one is running.
    /// The caller holds the lifecycle lock.
    fn start_locked(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        bridge.getattr("_stop_event")?.call_method0("clear")?;
        let current = bridge.getattr("_thread")?;
        if alive(&current)? {
            if is_set(bridge, "_running")? {
                return Ok(());
            }
            // stop() gave up on it while it was blocked; give it a moment.
            let kwargs = kw(py, &[("timeout", obj(py, 1.0))])?;
            current.call_method("join", (), Some(&kwargs))?;
        }
        bridge.getattr("_running")?.call_method0("set")?;
        let generation = bridge.getattr("_run_generation")?.add(1)?;
        bridge.setattr("_run_generation", &generation)?;
        let target = bridge.clone().unbind();
        let generation = generation.unbind();
        let handle = thread::spawn(py, "mavlink", move |py| {
            target.bind(py).call_method1("_run", (generation,))?;
            Ok(())
        })?;
        bridge.setattr("_thread", handle)
    }

    /// Tear the link down: every waiter woken, the shell released, the
    /// workers joined, the socket closed. The caller holds the lifecycle lock.
    fn stop_locked(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        bridge.getattr("_running")?.call_method0("clear")?;
        bridge.getattr("_stop_event")?.call_method0("set")?;
        bridge.call_method0("_stop_tlog")?;
        update(bridge, vec![("link_status", obj(py, "disconnected"))])?;
        bridge.getattr("_store")?.call_method0("set_disconnected")?;
        bridge.call_method0("_cancel_pending_commands")?;
        bridge.call_method0("_abort_parameter_operations")?;
        match bridge.call_method0("stop_shell") {
            Err(err) if !err.is_instance_of::<PyException>(py) => return Err(err),
            _ => {}
        }
        let join = |attr: &str, timeout: f64, clear: bool| -> PyResult<()> {
            let handle = bridge.getattr(attr)?;
            if handle.is_truthy()? {
                let kwargs = kw(py, &[("timeout", obj(py, timeout))])?;
                handle.call_method("join", (), Some(&kwargs))?;
                if clear {
                    bridge.setattr(attr, py.None())?;
                }
            }
            Ok(())
        };
        join("_param_upload_thread", 3.0, true)?;
        bridge.call_method0("_stop_param_metadata")?;
        let conn = bridge.getattr("_conn")?;
        if conn.is_truthy()? {
            close_quietly(&conn)?;
            bridge.setattr("_conn", py.None())?;
        }
        join("_param_watchdog_thread", 2.0, true)?;
        join("_version_retry_thread", 2.0, true)?;
        join("_intervals_thread", 2.0, true)?;
        join("_thread", 3.0, false)?;
        join("_hb_thread", 2.0, false)
    }

    /// Has a later start() replaced the thread this runs on? False on a
    /// thread that is not a connect/receive thread.
    fn superseded(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        let generation = match bridge.getattr("_cycle")?.getattr("generation") {
            Ok(g) => g,
            Err(err) if caught_by(py, &err, &["AttributeError"]) => return Ok(false),
            Err(err) => return Err(err),
        };
        Ok(!generation.is_none() && generation.ne(bridge.getattr("_run_generation")?)?)
    }

    /// The connect/receive cycle of one start(): connect, defer the interval
    /// requests, receive until the link ends, tear down and back off before
    /// the next attempt. A cycle a later start() replaced closes only what
    /// it opened and leaves.
    #[pyo3(signature = (bridge, generation=None))]
    fn run(&self, bridge: &Bound<'_, PyAny>, generation: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let py = bridge.py();
        let cycle = bridge.getattr("_cycle")?;
        cycle.setattr(
            "generation",
            generation.map(|g| g.clone().unbind()).unwrap_or_else(|| py.None()),
        )?;
        cycle.setattr("conn", py.None())?;
        while running(bridge)? && !Self::superseded_on(bridge)? {
            let attempt = (|| -> PyResult<()> {
                bridge.call_method0("_connect")?;
                bridge.setattr("_reconnect_attempt", 1)?;
                bridge.call_method0("_schedule_message_intervals")?;
                bridge.call_method0("_receive_loop")?;
                Ok(())
            })();
            let cycle_exc = match attempt {
                Ok(()) => None,
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    let value = err.value(py).clone();
                    if is_set(bridge, "_running")? && !Self::superseded_on(bridge)? {
                        logger(py)?.call_method1("error", ("MAVLink error: %s", &value))?;
                    } else {
                        logger(py)?.call_method1("debug", ("MAVLink link closed for shutdown: %s", &value))?;
                    }
                    Some(value)
                }
                Err(err) => return Err(err),
            };
            if Self::superseded_on(bridge)? {
                let own = match bridge.getattr("_cycle")?.getattr("conn") {
                    Ok(c) => c,
                    Err(err) if caught_by(py, &err, &["AttributeError"]) => py.None().into_bound(py),
                    Err(err) => return Err(err),
                };
                if !own.is_none() && !own.is(&bridge.getattr("_conn")?) {
                    close_quietly(&own)?;
                }
                logger(py)?.call_method1(
                    "info",
                    ("MAVLink cycle superseded by a restart; leaving it to the new one",),
                )?;
                return Ok(());
            }
            bridge.call_method0("_cancel_pending_commands")?;
            bridge.call_method0("_abort_parameter_operations")?;
            bridge.call_method0("_reset_shell_state")?;
            bridge.call_method0("_stop_tlog")?;
            let conn = bridge.getattr("_conn")?;
            bridge.setattr("_conn", py.None())?;
            if conn.is_truthy()? {
                close_quietly(&conn)?;
            }
            bridge.getattr("_store")?.call_method0("set_disconnected")?;
            if is_set(bridge, "_running")? {
                let attempt = bridge.getattr("_reconnect_attempt")?;
                let delay = bridge.call_method1("_reconnect_delay", (&attempt,))?;
                logger(py)?.call_method1("info", ("Reconnecting in %.1fs (attempt %d) …", &delay, &attempt))?;
                match &cycle_exc {
                    Some(exc) => update(
                        bridge,
                        vec![
                            ("link_status", obj(py, "reconnecting")),
                            ("link_error", obj(py, exc.str()?.to_string())),
                        ],
                    )?,
                    None => update(bridge, vec![("link_status", obj(py, "reconnecting"))])?,
                }
                bridge.call_method1("_interruptible_sleep", (&delay,))?;
                bridge.setattr("_reconnect_attempt", bridge.getattr("_reconnect_attempt")?.add(1)?)?;
            }
        }
        Ok(())
    }

    /// Exponential backoff with jitter: base * 2^(attempt-1), capped, then
    /// +/- the jitter, never below the floor, for any number of attempts.
    fn reconnect_delay(&self, py: Python<'_>, attempt: i64) -> PyResult<f64> {
        let env = self.env(py);
        let base: f64 = env.getattr("RECONNECT_BASE_S")?.extract()?;
        let cap: f64 = env.getattr("RECONNECT_CAP_S")?.extract()?;
        let jitter: f64 = env.getattr("RECONNECT_JITTER")?.extract()?;
        let floor: f64 = env.getattr("RECONNECT_MIN_S")?.extract()?;
        // Capped well before 2^n leaves the float range: past the cap the
        // delay is the cap whatever the exponent. Uncapped, the Python this
        // replaced raised OverflowError on attempt 1025, about 2.3 hours of
        // failed reconnects, and the link thread died with it.
        let exponent = attempt.saturating_sub(1).clamp(0, 1023) as i32;
        let scaled = base * 2f64.powi(exponent);
        let raw = if cap < scaled { cap } else { scaled };
        let factor = 1.0
            + env
                .getattr("random")?
                .call_method1("uniform", (-jitter, jitter))?
                .extract::<f64>()?;
        let delayed = raw * factor;
        Ok(if delayed > floor { delayed } else { floor })
    }

    /// Open the link and latch onto the aircraft on it: speak first, wait
    /// for an autopilot's heartbeat, pin the target, latch the dialect and
    /// the mode table, ask for version, home and (on serial) the streams,
    /// and start this session's tlog.
    fn connect(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let log = logger(py)?;
        log.call_method1("info", ("Connecting to %s …", bridge.getattr("_conn_str")?))?;
        self.reset_for_connect(bridge)?;
        let opened = (|| -> PyResult<()> {
            let opened = self.open(bridge)?;
            // Opening can block long enough for a restart to take over; the
            // link is then nobody's.
            if Self::superseded_on(bridge)? {
                close_quietly(&opened)?;
                return Err(PyConnectionError::new_err("superseded by a restart"));
            }
            bridge.setattr("_conn", &opened)?;
            bridge.getattr("_cycle")?.setattr("conn", &opened)?;
            Ok(())
        })();
        match opened {
            Ok(()) => {}
            Err(err) if err.is_instance_of::<PyOSError>(py) => return Err(self.open_failure(bridge, err)?),
            Err(err) => return Err(err),
        }
        if truthy(bridge, "_is_serial")? {
            let device = bridge
                .call_method1("_parse_serial", (bridge.getattr("_conn_str")?,))?
                .get_item(0)?;
            bridge.call_method1("_claim_serial_exclusive", (device,))?;
        } else {
            bridge.call_method0("_warn_if_onboard_port")?;
        }
        bridge.call_method0("_apply_signing")?;
        // Speak first: a far end that waits to hear from a station and a
        // station that waits for the vehicle would wait for each other.
        bridge.call_method1("_send_gcs_heartbeat", (bridge.getattr("_conn")?,))?;
        bridge.call_method0("_start_gcs_heartbeat")?;
        log.call_method1("info", ("Waiting for heartbeat …",))?;
        let hb = bridge.call_method1("_wait_vehicle_heartbeat", (self.constant(py, "HEARTBEAT_WAIT_S")?,))?;
        if hb.is_none() {
            return Err(PyConnectionError::new_err("No heartbeat received"));
        }
        let conn = bridge.getattr("_conn")?;
        let src_sys = source_call(&hb, "get_srcSystem")?;
        let src_sys = if src_sys.is_truthy()? {
            src_sys
        } else {
            conn.getattr("target_system")?
        };
        let src_comp = source_call(&hb, "get_srcComponent")?;
        let src_comp = if src_comp.is_truthy()? {
            src_comp
        } else {
            conn.getattr("target_component")?
        };
        bridge.setattr(
            "_target_system",
            if src_sys.is_truthy()? {
                src_sys
            } else {
                obj(py, 1).into_bound(py)
            },
        )?;
        bridge.setattr(
            "_target_component",
            if src_comp.is_truthy()? {
                src_comp
            } else {
                obj(py, 1).into_bound(py)
            },
        )?;
        bridge.setattr("_ignored_systems", pyo3::types::PySet::empty(py)?)?;
        let target_system = bridge.getattr("_target_system")?;
        let target_component = bridge.getattr("_target_component")?;
        let fleet = (|| -> PyResult<()> {
            let tracker = bridge.getattr("fleet_tracker")?;
            tracker.call_method1("bind", (&target_system, &target_component))?;
            let kwargs = kw(py, &[("vehicle_heartbeat", obj(py, true))])?;
            tracker.call_method("observe", (&hb,), Some(&kwargs))?;
            Ok(())
        })();
        match fleet {
            Err(err) if err.is_instance_of::<PyException>(py) => debug_exc(py, "fleet tracker failed")?,
            Err(err) => return Err(err),
            Ok(()) => {}
        }
        // Tell the link which node we picked, so the parts of mavutil that
        // read its own latched target agree with the aircraft Corvus flies.
        let pinned = (|| -> PyResult<()> {
            conn.setattr("target_system", &target_system)?;
            conn.setattr("target_component", &target_component)?;
            let states = conn.getattr("sysid_state").or_else(|err| {
                if caught_by(py, &err, &["AttributeError"]) {
                    Ok(py.None().into_bound(py))
                } else {
                    Err(err)
                }
            })?;
            let states = if states.is_truthy()? {
                states
            } else {
                PyDict::new(py).into_any()
            };
            if states.contains(&target_system)? {
                conn.setattr("sysid", &target_system)?;
            }
            Ok(())
        })();
        match pinned {
            Err(err) if err.is_instance_of::<PyException>(py) => {
                log.call_method1("debug", ("could not pin mavutil target: %s", err.value(py)))?;
            }
            Err(err) => return Err(err),
            Ok(()) => {}
        }
        let env = self.env(py);
        let kind = hb.getattr("type")?;
        let autopilot_id = hb.getattr("autopilot")?;
        let vtype = env
            .getattr("MAV_TYPE_MAP")?
            .call_method1("get", (&kind, format!("TYPE_{}", kind.str()?)))?;
        let autopilot = env
            .getattr("MAV_AUTOPILOT_MAP")?
            .call_method1("get", (&autopilot_id, format!("AP_{}", autopilot_id.str()?)))?;
        bridge.call_method1("_latch_dialect", (&autopilot_id, &kind))?;
        let armed = hb
            .getattr("base_mode")?
            .bitand(constant_or("MAV_MODE_FLAG_SAFETY_ARMED", 128))?
            .is_truthy()?;
        let mode = bridge.call_method1("_decode_mode", (&hb,))?;
        let vehicle_id = format!("{}:{}", bridge.getattr("fleet_link_id")?.str()?, target_system.str()?);
        let previous = bridge
            .getattr("_store")?
            .call_method0("get_snapshot")?
            .call_method1("get", ("vehicle_id", ""))?;
        if previous.is_truthy()? && previous.ne(vehicle_id.as_str())? {
            bridge.getattr("_store")?.call_method0("forget_vehicle")?;
        }
        let dialect = bridge.getattr("_dialect")?;
        log.call_method1(
            "info",
            (
                "Heartbeat: %s / %s (%s) mode=%s (sys=%d comp=%d)",
                &vtype,
                &autopilot,
                dialect.getattr("label")?,
                &mode,
                &target_system,
                &target_component,
            ),
        )?;
        update(
            bridge,
            vec![
                ("connected", obj(py, true)),
                ("vehicle_type", vtype.unbind()),
                ("autopilot", autopilot.unbind()),
                ("armed", obj(py, armed)),
                ("mode", mode.clone().unbind()),
                ("mode_label", dialect.call_method1("mode_label", (&mode,))?.unbind()),
                ("link_status", obj(py, "connected")),
                ("link_error", obj(py, "")),
                ("autopilot_stack", dialect.getattr("stack")?.unbind()),
                ("vehicle_id", obj(py, vehicle_id)),
            ],
        )?;
        bridge.getattr("_store")?.call_method0("heartbeat")?;
        bridge.call_method0("_build_mode_mapping")?;
        if truthy(bridge, "_is_serial")? {
            bridge.call_method0("_request_streams")?;
        }
        bridge.call_method0("_request_version")?;
        bridge.call_method0("_request_home")?;
        bridge.setattr("_tlog_armed", py.None())?;
        bridge.call_method0("_start_tlog")?;
        Ok(())
    }

    /// The first HEARTBEAT from an actual autopilot within *timeout*: behind
    /// a router the first heartbeat is often a ground station's. Each wait
    /// is capped at a second so a stop is noticed; a transport that answers
    /// "nothing" at once is taken at its word.
    fn wait_vehicle_heartbeat(&self, bridge: &Bound<'_, PyAny>, timeout: f64) -> PyResult<PyObject> {
        let py = bridge.py();
        let deadline = self.monotonic(py)? + timeout;
        loop {
            if shutting_down() || is_set(bridge, "_stop_event")? || Self::superseded_on(bridge)? {
                return Ok(py.None());
            }
            let remaining = deadline - self.monotonic(py)?;
            if remaining <= 0.0 {
                return Ok(py.None());
            }
            let conn = bridge.getattr("_conn")?;
            if conn.is_none() {
                return Ok(py.None());
            }
            let wait_for = if remaining < 1.0 { remaining } else { 1.0 };
            let waited_from = self.monotonic(py)?;
            let kwargs = kw(py, &[("blocking", obj(py, true)), ("timeout", obj(py, wait_for))])?;
            let hb = conn.call_method("wait_heartbeat", (), Some(&kwargs))?;
            if hb.is_none() {
                if self.monotonic(py)? - waited_from < wait_for / 2.0 {
                    return Ok(py.None());
                }
                continue;
            }
            if self
                .env(py)
                .getattr("_is_vehicle_heartbeat")?
                .call1((&hb,))?
                .is_truthy()?
            {
                return Ok(hb.unbind());
            }
            logger(py)?.call_method1(
                "info",
                (
                    "ignoring heartbeat from sys=%s comp=%s: not an autopilot",
                    named_source(&hb, "get_srcSystem")?,
                    named_source(&hb, "get_srcComponent")?,
                ),
            )?;
        }
    }

    /// Start the GCS heartbeat thread unless one is running.
    fn start_gcs_heartbeat(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if alive(&bridge.getattr("_hb_thread")?)? {
            return Ok(());
        }
        let target = bridge.clone().unbind();
        let handle = thread::spawn(py, "gcs-hb", move |py| {
            target.bind(py).call_method0("_gcs_hb_loop")?;
            Ok(())
        })?;
        bridge.setattr("_hb_thread", handle)
    }

    /// One GCS heartbeat on *conn* now. Never raises.
    fn send_gcs_heartbeat(&self, bridge: &Bound<'_, PyAny>, conn: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        if conn.is_none() {
            return Ok(false);
        }
        let sent = within(&bridge.getattr("_send_lock")?, || -> PyResult<bool> {
            if !conn.is(&bridge.getattr("_conn")?) {
                return Ok(false);
            }
            heartbeat_send(conn)?;
            Ok(true)
        });
        match sent {
            Err(err) if err.is_instance_of::<PyException>(py) => {
                logger(py)?.call_method1("debug", ("GCS heartbeat send failed: %s", err.value(py)))?;
                Ok(false)
            }
            other => other,
        }
    }

    /// The GCS heartbeat at 1 Hz for as long as the bridge runs, degraded
    /// link or not, with the Remote ID identity riding on it once the vehicle
    /// is known. A failed send is ridden out; only a stop ends the loop, and
    /// it clears the handle only if the handle is its own.
    fn gcs_hb_loop(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let me = thread::current(py)?;
        while running(bridge)? && !is_set(bridge, "_stop_event")? {
            let conn = bridge.getattr("_conn")?;
            if !conn.is_truthy()? {
                bridge.call_method1("_interruptible_sleep", (1,))?;
                continue;
            }
            let vehicle_known = bridge
                .getattr("_store")?
                .call_method0("get_snapshot")?
                .call_method1("get", ("connected",))?
                .is_truthy()?;
            let step = within(&bridge.getattr("_send_lock")?, || -> PyResult<&'static str> {
                if !is_set(bridge, "_running")? {
                    return Ok("break");
                }
                if !conn.is(&bridge.getattr("_conn")?) {
                    return Ok("continue");
                }
                heartbeat_send(&conn)?;
                if vehicle_known {
                    bridge.call_method1("_send_remote_id", (&conn,))?;
                }
                Ok("sent")
            });
            match step {
                Ok("break") => break,
                Ok("continue") => continue,
                Ok(_) => {}
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    bridge.call_method1("_interruptible_sleep", (0.1,))?;
                    continue;
                }
                Err(err) => return Err(err),
            }
            bridge.call_method1("_interruptible_sleep", (1,))?;
        }
        if bridge.getattr("_hb_thread")?.is(&me) {
            bridge.setattr("_hb_thread", py.None())?;
        }
        Ok(())
    }

    /// Sleep in 0.1 s slices of the bridge module's `time.sleep`, returning
    /// as soon as the bridge stops.
    fn interruptible_sleep(&self, bridge: &Bound<'_, PyAny>, seconds: f64) -> PyResult<()> {
        let py = bridge.py();
        let sleep = self.env(py).getattr("time")?;
        let mut slept = 0.0;
        while slept < seconds {
            if shutting_down() || is_set(bridge, "_stop_event")? {
                return Ok(());
            }
            let chunk = if seconds - slept < 0.1 { seconds - slept } else { 0.1 };
            sleep.call_method1("sleep", (chunk,))?;
            slept += chunk;
        }
        Ok(())
    }

    /// Run the message interval requests on a worker once the receive loop
    /// is draining the socket, so their COMMAND_ACKs are delivered.
    fn schedule_message_intervals(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if alive(&bridge.getattr("_intervals_thread")?)? {
            return Ok(());
        }
        let target = bridge.clone().unbind();
        let handle = thread::spawn(py, "mavlink-intervals", move |py| {
            let bridge = target.bind(py);
            bridge.call_method1("_interruptible_sleep", (0.05,))?;
            if !is_set(bridge, "_running")? || !bridge.getattr("_conn")?.is_truthy()? {
                return Ok(());
            }
            if is_set(bridge, "_stop_event")? {
                return Ok(());
            }
            for (method, text) in [
                ("_request_message_intervals", "deferred message intervals failed: %s"),
                ("_check_gcs_authority", "GCS authority check failed: %s"),
            ] {
                match bridge.call_method0(method) {
                    Err(err) if err.is_instance_of::<PyException>(py) => {
                        logger(py)?.call_method1("debug", (text, err.value(py)))?;
                    }
                    Err(err) => return Err(err),
                    Ok(_) => {}
                }
            }
            Ok(())
        })?;
        bridge.setattr("_intervals_thread", handle)
    }

    /// One AUTOPILOT_VERSION retry about 4 s after connect if the first got
    /// no reply (PX4 v1.18 alpha SITL answers the capabilities command with
    /// UNSUPPORTED). Silent after a stop.
    fn schedule_version_retry(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if !is_set(bridge, "_running")? {
            return Ok(());
        }
        if alive(&bridge.getattr("_version_retry_thread")?)? {
            return Ok(());
        }
        let target = bridge.clone().unbind();
        let handle = thread::spawn(py, "px4-ver-retry", move |py| {
            let bridge = target.bind(py);
            bridge.call_method1("_interruptible_sleep", (4.0,))?;
            if !is_set(bridge, "_running")? || !bridge.getattr("_conn")?.is_truthy()? {
                return Ok(());
            }
            let known = bridge
                .getattr("_store")?
                .call_method0("get_snapshot")?
                .call_method1("get", ("px4_version",))?;
            if known.is_truthy()? {
                return Ok(());
            }
            let nan = f64::NAN;
            let requests: [&Request; 2] = [
                &|conn| {
                    let msg_id: f64 = bridge.getattr("_MSG_ID_AUTOPILOT_VERSION")?.extract()?;
                    let args = PyTuple::new(
                        py,
                        [
                            bridge.getattr("_target_system")?.unbind(),
                            bridge.getattr("_target_component")?.unbind(),
                            obj(py, constant_or("MAV_CMD_REQUEST_MESSAGE", 512)),
                            obj(py, 0),
                            obj(py, msg_id),
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
                },
                &|conn| {
                    conn.getattr("mav")?.call_method1(
                        "autopilot_version_request_send",
                        (bridge.getattr("_target_system")?, bridge.getattr("_target_component")?),
                    )?;
                    Ok(())
                },
            ];
            for request in requests {
                let sent = within(&bridge.getattr("_send_lock")?, || -> PyResult<bool> {
                    let conn = bridge.getattr("_conn")?;
                    if !is_set(bridge, "_running")? || !conn.is_truthy()? {
                        return Ok(false);
                    }
                    request(&conn)?;
                    Ok(true)
                });
                match sent {
                    Ok(false) => return Ok(()),
                    Ok(true) => {}
                    Err(err) if err.is_instance_of::<PyException>(py) => {}
                    Err(err) => return Err(err),
                }
            }
            Ok(())
        })?;
        bridge.setattr("_version_retry_thread", handle)
    }
}

/// `conn.mav.heartbeat_send(MAV_TYPE_GCS, MAV_AUTOPILOT_INVALID, 0, 0, 0)`.
fn heartbeat_send(conn: &Bound<'_, PyAny>) -> PyResult<()> {
    conn.getattr("mav")?.call_method1(
        "heartbeat_send",
        (
            constant_or("MAV_TYPE_GCS", 6),
            constant_or("MAV_AUTOPILOT_INVALID", 8),
            0,
            0,
            0,
        ),
    )?;
    Ok(())
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<LinkCycle>()
}
