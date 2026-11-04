//! Flight commands: flight mode, arming, takeoff, landing, return, home,
//! hold, the plugin command table and velocity setpoints, and the sequences
//! that start a mission (fly to points, mission start, reposition).
//!
//! A sequence is several commands with telemetry waits between them, and the
//! command lock is held for each command's own round trip only, so Disarm,
//! Land, RTL or a mode change never queue behind a wait. Every later step
//! first asks whether one of those ran meanwhile (`_sequence_interrupted`).
//!
//! The steps call each other through the bridge (`bridge.arm`,
//! `bridge._enter_guided`, ...), so a bridge that overrides one, or a test
//! that replaces one, changes the sequence the way it did in Python. The
//! stack differences come from the dialect (`corvus.autopilot`); the clock
//! and the module constants from ``corvus.mavlink_flight``.

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyModule, PyTuple};
use pyo3::BoundObject;

use crate::protocol::host::{caught_by, console, logger, obj, py_float, py_int, set_error, truthy};
use crate::wire::defs::constant_or;

fn accepted() -> i64 {
    constant_or("MAV_RESULT_ACCEPTED", 0)
}

fn nan() -> f64 {
    f64::NAN
}

/// `with ctx:` around `f`, handing a raised error to `__exit__` as Python does.
fn within<T>(ctx: &Bound<'_, PyAny>, f: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
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

fn op_lock<'py>(bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    bridge.getattr("_operation_lock")
}

fn dialect<'py>(bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    bridge.getattr("_dialect")
}

fn type_id<'py>(bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    bridge.getattr("_mav_type_id")
}

fn snapshot<'py>(bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    bridge.getattr("_store")?.call_method0("get_snapshot")
}

fn snap_get<'py>(bridge: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    snapshot(bridge)?.call_method1("get", (key,))
}

fn failure(bridge: &Bound<'_, PyAny>, action: &str, result: i64) -> PyResult<bool> {
    bridge.call_method1("_command_failure", (action, result))?.is_truthy()
}

fn warn_store(bridge: &Bound<'_, PyAny>, text: &str, level: &str) -> PyResult<()> {
    bridge.call_method1("_store_update_warning", (text, level))?;
    Ok(())
}

fn calls_true<'py>(
    bridge: &Bound<'py, PyAny>,
    method: &str,
    args: impl IntoPyObject<'py, Target = PyTuple>,
) -> PyResult<bool> {
    let args = args.into_pyobject(bridge.py()).map_err(Into::into)?.into_bound();
    bridge.call_method1(method, args)?.is_truthy()
}

/// `bridge._send_command_and_wait(command, params, timeout=..., retries=..., **extra)`.
fn command(
    bridge: &Bound<'_, PyAny>,
    cmd: &Bound<'_, PyAny>,
    params: Bound<'_, PyList>,
    timeout: f64,
    retries: i64,
    accept_in_progress: bool,
) -> PyResult<i64> {
    let kwargs = PyDict::new(bridge.py());
    kwargs.set_item("timeout", timeout)?;
    kwargs.set_item("retries", retries)?;
    if accept_in_progress {
        kwargs.set_item("accept_in_progress", true)?;
    }
    py_int(&bridge.call_method("_send_command_and_wait", (cmd, params), Some(&kwargs))?)
}

/// `bridge._send_command_int_and_wait(command, frame, params, x=, y=, z=, timeout=5.0, retries=1)`.
fn command_int(
    bridge: &Bound<'_, PyAny>,
    cmd: &Bound<'_, PyAny>,
    frame: i64,
    params: Bound<'_, PyList>,
    x: i64,
    y: i64,
    z: &Bound<'_, PyAny>,
) -> PyResult<i64> {
    let kwargs = PyDict::new(bridge.py());
    kwargs.set_item("x", x)?;
    kwargs.set_item("y", y)?;
    kwargs.set_item("z", z)?;
    kwargs.set_item("timeout", 5.0)?;
    kwargs.set_item("retries", 1)?;
    py_int(&bridge.call_method("_send_command_int_and_wait", (cmd, frame, params), Some(&kwargs))?)
}

fn cmd_id<'py>(py: Python<'py>, name: &str, fallback: i64) -> Bound<'py, PyAny> {
    obj(py, constant_or(name, fallback)).into_bound(py)
}

/// `int(round(degrees * 1e7))`.
fn e7(degrees: f64) -> i64 {
    (degrees * 1e7).round_ties_even() as i64
}

/// Python's `f"{value:.{digits}f}"` for a float.
fn fixed(value: f64, digits: usize) -> String {
    format!("{value:.digits$}")
}

fn floats<'py>(py: Python<'py>, values: &[f64]) -> PyResult<Bound<'py, PyList>> {
    PyList::new(py, values)
}

/// The finite float of a bool-free int or float, else None.
fn plain_number(value: &Bound<'_, PyAny>) -> PyResult<Option<f64>> {
    if value.is_instance_of::<PyBool>() || !(value.is_instance_of::<PyInt>() || value.is_instance_of::<PyFloat>()) {
        return Ok(None);
    }
    Ok(Some(value.extract::<f64>()?))
}

/// The flight commands of one bridge.
#[pyclass(name = "FlightEngine", module = "corvus_link.flight", frozen)]
pub struct FlightEngine {
    env: PyObject,
}

impl FlightEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn monotonic(&self, py: Python<'_>) -> PyResult<f64> {
        self.env(py).getattr("time")?.call_method0("monotonic")?.extract()
    }

    fn sleep(&self, py: Python<'_>, seconds: f64) -> PyResult<()> {
        self.env(py).getattr("time")?.call_method1("sleep", (seconds,))?;
        Ok(())
    }

    fn ground(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        self.env(bridge.py())
            .getattr("is_ground_vehicle")?
            .call1((type_id(bridge)?,))?
            .is_truthy()
    }

    fn result_text(&self, py: Python<'_>, result: i64) -> PyResult<String> {
        Ok(self
            .env(py)
            .getattr("MAV_RESULT_TEXT")?
            .call_method1("get", (result, format!("RESULT_{result}")))?
            .str()?
            .to_string())
    }

    fn interrupted(bridge: &Bound<'_, PyAny>, mark: &Bound<'_, PyAny>, action: &str) -> PyResult<bool> {
        bridge
            .call_method1("_sequence_interrupted", (mark, action))?
            .is_truthy()
    }
}

#[pymethods]
impl FlightEngine {
    /// *env* is ``corvus.mavlink_flight``: the clock ``time``, ``ModeCommand``,
    /// ``is_ground_vehicle``, ``PLUGIN_POSITION_COMMANDS``, ``MAV_RESULT_TEXT``,
    /// the takeoff and velocity limits and ``DO_REPOSITION_CHANGE_MODE``.
    #[new]
    fn new(env: PyObject) -> Self {
        FlightEngine { env }
    }

    // ------------------------------------------------------------ modes

    /// Change flight mode, ACK confirmed. The caller holds the operation lock
    /// in the priority tier: a mode change is how a flight is taken back.
    fn set_mode(&self, bridge: &Bound<'_, PyAny>, mode: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        (|| -> PyResult<bool> {
            set_error(bridge, "")?;
            if !truthy(bridge, "_connection_ready")? {
                return failure(bridge, "Mode change", -2);
            }
            let value = bridge.getattr("_mode_values")?.call_method1("get", (mode,))?;
            let usable = value.downcast::<PyTuple>().ok().filter(|t| t.len() >= 3);
            let Some(value) = usable else {
                let text = if bridge.getattr("_modes_unsupported")?.is_truthy()? {
                    "Mode changes are not supported on this autopilot: Corvus does not know how it encodes a mode"
                        .to_string()
                } else {
                    format!(
                        "Unknown or unsupported {} mode: {}",
                        dialect(bridge)?.getattr("label")?.str()?,
                        mode.str()?
                    )
                };
                logger(py)?.call_method1("error", ("%s", &text))?;
                set_error(bridge, &text)?;
                return Ok(false);
            };
            let params = self
                .env(py)
                .getattr("ModeCommand")?
                .call1(value.get_slice(0, 3))?
                .call_method0("params")?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("timeout", 3.0)?;
            kwargs.set_item("retries", 1)?;
            let result = py_int(&bridge.call_method(
                "_send_command_and_wait",
                (cmd_id(py, "MAV_CMD_DO_SET_MODE", 176), params),
                Some(&kwargs),
            )?)?;
            if result != accepted() {
                return failure(bridge, "Mode change", result);
            }
            console(bridge, "MODE", &format!("Mode accepted: {}", mode.str()?), "success")?;
            Ok(true)
        })()
    }

    /// Did another thread's abort run since *mark*? Then the sequence stops
    /// with the reason rather than send its next step on top of it.
    fn sequence_interrupted(&self, bridge: &Bound<'_, PyAny>, mark: &Bound<'_, PyAny>, action: &str) -> PyResult<bool> {
        if !op_lock(bridge)?.call_method1("aborted_since", (mark,))?.is_truthy()? {
            return Ok(false);
        }
        let text = format!("{action} stopped: another command took over");
        set_error(bridge, &text)?;
        console(bridge, &action.to_uppercase(), &text, "warning")?;
        Ok(true)
    }

    /// Does fresh telemetry already say the vehicle is *armed* (or not)?
    fn reports_armed_state(&self, bridge: &Bound<'_, PyAny>, armed: bool) -> PyResult<bool> {
        let snap = snapshot(bridge)?;
        if !snap.call_method1("get", ("connected",))?.is_truthy()? {
            return Ok(false);
        }
        if snap.call_method1("get", ("armed",))?.is_truthy()? != armed {
            return Ok(false);
        }
        let kwargs = PyDict::new(bridge.py());
        kwargs.set_item("timeout", 2.0)?;
        Ok(!bridge
            .getattr("_store")?
            .call_method("is_stale", (), Some(&kwargs))?
            .is_truthy()?)
    }

    /// Watch the store until it reports *armed*, for up to *timeout* seconds.
    fn wait_for_armed_state(&self, bridge: &Bound<'_, PyAny>, armed: bool, timeout: f64) -> PyResult<bool> {
        let py = bridge.py();
        let deadline = self.monotonic(py)? + timeout;
        while self.monotonic(py)? < deadline {
            let snap = snapshot(bridge)?;
            if snap.call_method1("get", ("connected",))?.is_truthy()?
                && snap.call_method1("get", ("armed",))?.is_truthy()? == armed
            {
                return Ok(true);
            }
            self.sleep(py, 0.05)?;
        }
        Ok(false)
    }

    /// The arm or disarm command, ACK confirmed: the verdict when it ends
    /// here, None when the vehicle accepted and the confirmation is next. The
    /// caller holds the operation lock (the priority tier to disarm).
    fn arm_command(&self, bridge: &Bound<'_, PyAny>, arm: bool) -> PyResult<Option<bool>> {
        let py = bridge.py();
        let action = if arm { "Arm" } else { "Disarm" };
        set_error(bridge, "")?;
        if !truthy(bridge, "_connection_ready")? {
            return Ok(Some(failure(bridge, action, -2)?));
        }
        let params = floats(
            py,
            &[if arm { 1.0 } else { 0.0 }, 0.0, nan(), nan(), nan(), nan(), nan()],
        )?;
        let result = command(
            bridge,
            &cmd_id(py, "MAV_CMD_COMPONENT_ARM_DISARM", 400),
            params,
            3.0,
            2,
            false,
        )?;
        if result != accepted() {
            // ArduPilot answers FAILED to arming an armed vehicle; fresh
            // telemetry in the state asked for is not a failure.
            if result >= 0 && calls_true(bridge, "_reports_armed_state", (arm,))? {
                return Ok(Some(true));
            }
            return Ok(Some(failure(bridge, action, result)?));
        }
        Ok(None)
    }

    /// Watch telemetry for the state the vehicle accepted, outside the lock.
    /// A confirmation that never arrives is reported as unconfirmed, not as
    /// failed: the ACK is taken at its word.
    fn arm_confirm(&self, bridge: &Bound<'_, PyAny>, arm: bool) -> PyResult<bool> {
        let action = if arm { "Arm" } else { "Disarm" };
        if !calls_true(bridge, "_wait_for_armed_state", (arm,))? {
            let wait: f64 = py_float(&bridge.getattr("ARMED_STATE_CONFIRM_S")?)?;
            let text = format!(
                "{action} accepted by the vehicle, but no telemetry confirmation within {}s. Check the armed indicator",
                fixed(wait, 0)
            );
            console(bridge, "ARM", &text, "warning")?;
            warn_store(bridge, &text, "warning")?;
            return Ok(true);
        }
        let completed = if arm { "Armed" } else { "Disarmed" };
        console(bridge, "ARM", &format!("{completed} successfully"), "success")?;
        Ok(true)
    }

    /// Is the aircraft off the ground? Its landed state first, else height
    /// above home over the fallback.
    fn is_airborne(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        let snap = snapshot(bridge)?;
        let landed = snap.call_method1("get", ("landed_state",))?;
        if bridge.getattr("_AIRBORNE_LANDED_STATES")?.contains(&landed)? {
            return Ok(true);
        }
        if landed.eq(1)? {
            return Ok(false);
        }
        let agl = snap.call_method1("get", ("altitude_agl", 0.0))?;
        match py_float(&agl) {
            Ok(v) => Ok(v > py_float(&bridge.getattr("AIRBORNE_FALLBACK_M")?)?),
            Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Home altitude plus *altitude_agl*, or None without a reference.
    fn takeoff_altitude_amsl(&self, bridge: &Bound<'_, PyAny>, altitude_agl: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        let py = bridge.py();
        let mut reference = bridge.getattr("_home_alt_amsl")?;
        if reference.is_none() {
            reference = bridge.getattr("_position_home_alt_amsl")?;
        }
        if reference.is_none() {
            return Ok(py.None());
        }
        Ok(reference.add(altitude_agl)?.unbind())
    }

    /// Ask for home once and wait briefly for an altitude reference.
    fn await_takeoff_reference(
        &self,
        bridge: &Bound<'_, PyAny>,
        altitude_agl: &Bound<'_, PyAny>,
    ) -> PyResult<PyObject> {
        let py = bridge.py();
        bridge.call_method0("_request_home")?;
        let deadline = self.monotonic(py)? + py_float(&bridge.getattr("TAKEOFF_REFERENCE_WAIT_S")?)?;
        while self.monotonic(py)? < deadline {
            if bridge.getattr("_stop_event")?.call_method0("is_set")?.is_truthy()?
                || !truthy(bridge, "_connection_ready")?
            {
                return Ok(py.None());
            }
            let amsl = bridge.call_method1("_takeoff_altitude_amsl", (altitude_agl,))?;
            if !amsl.is_none() {
                return Ok(amsl.unbind());
            }
            self.sleep(py, 0.05)?;
        }
        Ok(bridge.call_method1("_takeoff_altitude_amsl", (altitude_agl,))?.unbind())
    }

    /// Watch the store until the vehicle reports *mode*; sends nothing.
    fn wait_for_mode(&self, bridge: &Bound<'_, PyAny>, mode: &Bound<'_, PyAny>, timeout: f64) -> PyResult<bool> {
        let py = bridge.py();
        let deadline = self.monotonic(py)? + timeout;
        while self.monotonic(py)? < deadline {
            if bridge.getattr("_stop_event")?.call_method0("is_set")?.is_truthy()?
                || !truthy(bridge, "_connection_ready")?
            {
                return Ok(false);
            }
            if snap_get(bridge, "mode")?.eq(mode)? {
                return Ok(true);
            }
            self.sleep(py, 0.05)?;
        }
        snap_get(bridge, "mode")?.eq(mode)
    }

    /// Put the vehicle in the mode a guided command needs, if the stack needs
    /// one (ArduPilot's GUIDED); a vehicle already in it is left alone.
    fn enter_guided(&self, bridge: &Bound<'_, PyAny>, action: &str) -> PyResult<bool> {
        let wanted = dialect(bridge)?.call_method1("guided_mode", (type_id(bridge)?,))?;
        if !wanted.is_truthy()? {
            return Ok(true);
        }
        let current = snap_get(bridge, "mode")?;
        let current = if current.is_truthy()? {
            current
        } else {
            obj(bridge.py(), "").into_bound(bridge.py())
        };
        if current.eq(&wanted)? {
            return Ok(true);
        }
        let name = wanted.str()?.to_string();
        if !bridge.call_method1("set_mode", (&wanted,))?.is_truthy()? {
            let existing = bridge.call_method0("get_last_command_error")?;
            let mut text = format!("{action} needs {name} mode, which the vehicle refused");
            if existing.is_truthy()? {
                text.push_str(&format!(": {}", existing.str()?));
            }
            set_error(bridge, &text)?;
            return Ok(false);
        }
        if !bridge.call_method1("_wait_for_mode", (&wanted,))?.is_truthy()? {
            let text = format!(
                "{name} accepted but not yet confirmed in telemetry, continuing with {}",
                action.to_lowercase()
            );
            console(bridge, "MODE", &text, "warning")?;
        }
        Ok(true)
    }

    /// Switch to the mode that runs a stored mission (MISSION on PX4, AUTO on
    /// ArduPilot).
    fn enter_mission_mode(&self, bridge: &Bound<'_, PyAny>, action: &str) -> PyResult<bool> {
        let mode = dialect(bridge)?.getattr("mission_mode")?;
        if !mode.is_truthy()? {
            set_error(
                bridge,
                &format!("{action}: Corvus does not know this autopilot's mission mode"),
            )?;
            return Ok(false);
        }
        let modes = bridge.getattr("_mode_values")?;
        if !modes.contains(&mode)? && modes.is_truthy()? {
            set_error(
                bridge,
                &format!("{action}: this vehicle does not offer a {} mode", mode.str()?),
            )?;
            return Ok(false);
        }
        bridge.call_method1("set_mode", (mode,))?.is_truthy()
    }

    // ---------------------------------------------------------- takeoff

    /// A guided takeoff to *altitude_agl* metres, in the stack's order:
    /// PX4 takes the command (param7 AMSL) and is then armed; ArduPilot is put
    /// in GUIDED, armed, then commanded (param7 relative to home) and disarmed
    /// again if it refuses.
    fn takeoff(&self, bridge: &Bound<'_, PyAny>, altitude_agl: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        let env = self.env(py);
        set_error(bridge, "")?;
        if self.ground(bridge)? {
            set_error(bridge, "Takeoff is not available on a rover. Arm it and drive.")?;
            return Ok(false);
        }
        if altitude_agl.is_instance_of::<PyBool>() {
            set_error(bridge, "Takeoff altitude must be a number")?;
            return Ok(false);
        }
        let agl = match py_float(altitude_agl) {
            Ok(v) => v,
            Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => {
                set_error(bridge, "Takeoff altitude must be a number")?;
                return Ok(false);
            }
            Err(err) => return Err(err),
        };
        if !agl.is_finite() {
            set_error(bridge, "Takeoff altitude must be finite")?;
            return Ok(false);
        }
        let low = py_float(&env.getattr("TAKEOFF_ALTITUDE_MIN_M")?)?;
        let high = py_float(&env.getattr("TAKEOFF_ALTITUDE_MAX_M")?)?;
        if !(low <= agl && agl <= high) {
            set_error(
                bridge,
                &format!(
                    "Takeoff altitude must be between {} and {} m AGL",
                    fixed(low, 0),
                    fixed(high, 0)
                ),
            )?;
            return Ok(false);
        }
        if !truthy(bridge, "_connection_ready")? {
            return failure(bridge, "Takeoff", -2);
        }
        let mark = op_lock(bridge)?.call_method0("abort_mark")?;
        let plan = dialect(bridge)?.call_method1("takeoff_plan", (type_id(bridge)?,))?;
        let agl_obj = PyFloat::new(py, agl);
        let takeoff_alt: PyObject;
        let note: String;
        if plan.getattr("altitude_frame")?.eq("relative")? {
            takeoff_alt = obj(py, agl);
            note = format!("Requesting {} m above home", fixed(agl, 0));
        } else {
            let mut amsl = bridge.call_method1("_takeoff_altitude_amsl", (&agl_obj,))?;
            if amsl.is_none() {
                amsl = bridge.call_method1("_await_takeoff_reference", (&agl_obj,))?;
            }
            if amsl.is_none() {
                // NaN is the protocol's "use your own default": the vehicle
                // takes off to its configured altitude rather than Corvus
                // refusing a flight over a conversion.
                takeoff_alt = obj(py, nan());
                let text = format!(
                    "No altitude reference yet (no HOME_POSITION or global position), so asking the vehicle to take off to its own configured altitude instead of {} m",
                    fixed(agl, 0)
                );
                logger(py)?.call_method1("warning", ("%s", &text))?;
                console(bridge, "TAKEOFF", &text, "warning")?;
                warn_store(bridge, &text, "warning")?;
                note = String::new();
            } else {
                note = format!(
                    "Requesting {} m AGL ({} m AMSL)",
                    fixed(agl, 0),
                    fixed(py_float(&amsl)?, 1)
                );
                takeoff_alt = amsl.unbind();
            }
        }
        let order = plan.getattr("order")?;
        if order.eq("mode_arm_takeoff")? && !bridge.call_method1("_enter_guided", ("Takeoff",))?.is_truthy()? {
            return Ok(false);
        }
        let params = PyList::new(
            py,
            [
                plan.getattr("min_pitch_deg")?.unbind(),
                obj(py, nan()),
                obj(py, nan()),
                obj(py, nan()),
                obj(py, nan()),
                obj(py, nan()),
                takeoff_alt,
            ],
        )?;
        if Self::interrupted(bridge, &mark, "Takeoff")? {
            return Ok(false);
        }
        if !truthy(bridge, "_connection_ready")? {
            return failure(bridge, "Takeoff", -2);
        }
        if !note.is_empty() {
            console(bridge, "TAKEOFF", &note, "info")?;
        }
        let takeoff_cmd = cmd_id(py, "MAV_CMD_NAV_TAKEOFF", 22);
        if order.eq("takeoff_then_arm")? {
            let result = within(&op_lock(bridge)?, || {
                command(bridge, &takeoff_cmd, params.clone(), 5.0, 1, false)
            })?;
            if result != accepted() {
                return failure(bridge, "Takeoff", result);
            }
            console(bridge, "TAKEOFF", "Takeoff target accepted; arming …", "info")?;
            if Self::interrupted(bridge, &mark, "Takeoff")? {
                return Ok(false);
            }
            if !calls_true(bridge, "arm", (true,))? {
                return Ok(false);
            }
        } else {
            console(bridge, "TAKEOFF", "Arming before takeoff …", "info")?;
            if !calls_true(bridge, "arm", (true,))? {
                return Ok(false);
            }
            if Self::interrupted(bridge, &mark, "Takeoff")? {
                return Ok(false);
            }
            let result = within(&op_lock(bridge)?, || {
                command(bridge, &takeoff_cmd, params.clone(), 5.0, 1, false)
            })?;
            if result != accepted() {
                let failure_text = format!("Takeoff failed: {}", self.result_text(py, result)?);
                console(
                    bridge,
                    "TAKEOFF",
                    "Takeoff refused after arming, disarming again",
                    "warning",
                )?;
                bridge.call_method1("arm", (false,))?;
                set_error(bridge, &failure_text)?;
                console(bridge, "TAKEOFF", &failure_text, "error")?;
                warn_store(bridge, &failure_text, "critical")?;
                return Ok(false);
            }
        }
        console(bridge, "TAKEOFF", "Takeoff accepted and vehicle armed", "success")?;
        Ok(true)
    }

    /// Land where the vehicle is. The caller holds the priority tier.
    fn land(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        (|| -> PyResult<bool> {
            set_error(bridge, "")?;
            if self.ground(bridge)? {
                set_error(bridge, "Land is not available on a rover. Disarm it to stop.")?;
                return Ok(false);
            }
            let params = floats(py, &[0.0, 0.0, nan(), nan(), nan(), nan(), nan()])?;
            let result = command(bridge, &cmd_id(py, "MAV_CMD_NAV_LAND", 21), params, 5.0, 1, false)?;
            if result != accepted() {
                return failure(bridge, "Land", result);
            }
            console(bridge, "LAND", "Land accepted", "success")?;
            Ok(true)
        })()
    }

    /// Return to launch. The caller holds the priority tier.
    fn rtl(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        (|| -> PyResult<bool> {
            set_error(bridge, "")?;
            let params = floats(py, &[nan(); 7])?;
            let result = command(
                bridge,
                &cmd_id(py, "MAV_CMD_NAV_RETURN_TO_LAUNCH", 20),
                params,
                5.0,
                1,
                false,
            )?;
            if result != accepted() {
                return failure(bridge, "RTL", result);
            }
            console(bridge, "RTL", "RTL accepted", "success")?;
            Ok(true)
        })()
    }

    /// One coordinate pair coerced and range checked, or None with the error
    /// set. A bool is not a coordinate.
    fn validate_lat_lon(
        &self,
        bridge: &Bound<'_, PyAny>,
        lat: &Bound<'_, PyAny>,
        lon: &Bound<'_, PyAny>,
        action: &str,
    ) -> PyResult<Option<(f64, f64)>> {
        let lat_f = bridge.call_method1("_fly_to_point_number", (lat,))?;
        let lon_f = bridge.call_method1("_fly_to_point_number", (lon,))?;
        let check = |v: &Bound<'_, PyAny>, limit: f64| -> PyResult<Option<f64>> {
            if v.is_none() {
                return Ok(None);
            }
            let f = py_float(v)?;
            Ok((f.is_finite() && -limit <= f && f <= limit).then_some(f))
        };
        let Some(lat_v) = check(&lat_f, 90.0)? else {
            set_error(bridge, &format!("{action} failed: lat out of range"))?;
            return Ok(None);
        };
        let Some(lon_v) = check(&lon_f, 180.0)? else {
            set_error(bridge, &format!("{action} failed: lon out of range"))?;
            return Ok(None);
        };
        Ok(Some((lat_v, lon_v)))
    }

    /// Move home to (*lat*, *lon*) at its current altitude, a purely
    /// horizontal move; refused rather than sent with a made up altitude.
    fn set_home(&self, bridge: &Bound<'_, PyAny>, lat: &Bound<'_, PyAny>, lon: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let coords = bridge.call_method1("_validate_lat_lon", (lat, lon, "Set home"))?;
        if coords.is_none() {
            return Ok(false);
        }
        let (lat_f, lon_f): (f64, f64) = coords.extract()?;
        let zero = PyFloat::new(py, 0.0);
        let mut alt = bridge.call_method1("_takeoff_altitude_amsl", (&zero,))?;
        if alt.is_none() {
            alt = bridge.call_method1("_await_takeoff_reference", (&zero,))?;
        }
        within(&op_lock(bridge)?, || {
            if alt.is_none() {
                let text = "Set home failed: no home or global altitude reference";
                set_error(bridge, "no home or global altitude reference")?;
                console(bridge, "SETHOME", text, "error")?;
                warn_store(bridge, text, "critical")?;
                return Ok(false);
            }
            let params = floats(py, &[0.0, nan(), nan(), nan()])?;
            let result = command_int(
                bridge,
                &cmd_id(py, "MAV_CMD_DO_SET_HOME", 179),
                constant_or("MAV_FRAME_GLOBAL", 0),
                params,
                e7(lat_f),
                e7(lon_f),
                &alt,
            )?;
            if result != accepted() {
                return failure(bridge, "Set home", result);
            }
            bridge.call_method0("_request_home")?;
            console(
                bridge,
                "SETHOME",
                &format!("Home set to {}, {}", fixed(lat_f, 7), fixed(lon_f, 7)),
                "success",
            )?;
            Ok(true)
        })
    }

    // ---------------------------------------------------------- plugins

    /// Stop and stay, in the stack's hold mode; a plain mode change.
    fn hold(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let mode = dialect(bridge)?.call_method1("hold_mode", (type_id(bridge)?,))?;
        if !mode.is_truthy()? {
            set_error(bridge, "Hold is not available on this vehicle")?;
            return Ok(false);
        }
        bridge.call_method1("set_mode", (mode,))?.is_truthy()
    }

    /// One MAV_CMD from the dialect's plugin table, ACK confirmed. A command
    /// that names a place travels as COMMAND_INT, its relative altitude
    /// converted to AMSL where the stack reads it so.
    #[pyo3(signature = (bridge, command, params=None, position=None))]
    fn plugin_command(
        &self,
        bridge: &Bound<'_, PyAny>,
        command: &Bound<'_, PyAny>,
        params: Option<&Bound<'_, PyAny>>,
        position: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let py = bridge.py();
        let env = self.env(py);
        set_error(bridge, "")?;
        let name = dialect(bridge)?
            .call_method1("plugin_commands", (type_id(bridge)?,))?
            .call_method1("get", (command,))?;
        if name.is_none() {
            let known = env
                .getattr("mavutil")?
                .getattr("mavlink")?
                .getattr("enums")?
                .get_item("MAV_CMD")?
                .call_method1("get", (command,))?;
            let label = if known.is_truthy()? {
                known
                    .getattr("name")?
                    .call_method1("removeprefix", ("MAV_CMD_",))?
                    .str()?
                    .to_string()
            } else {
                command.str()?.to_string()
            };
            set_error(bridge, &format!("Command {label} is not available on this autopilot"))?;
            return Ok(false);
        }
        let name = name.str()?.to_string();
        let label = format!("Command {name}");
        let position = position.filter(|p| !p.is_none());
        let positional = env.getattr("PLUGIN_POSITION_COMMANDS")?.contains(command)?;
        if positional && position.is_none() {
            set_error(bridge, &format!("{label} needs lat and lon"))?;
            return Ok(false);
        }
        if !positional && position.is_some() {
            set_error(bridge, &format!("{label} takes no position"))?;
            return Ok(false);
        }
        let mut values: Vec<f64> = Vec::new();
        if let Some(params) = params.filter(|p| p.is_truthy().unwrap_or(true)) {
            for v in params.try_iter()? {
                values.push(py_float(&v?)?);
            }
        }
        let result = if let Some(position) = position {
            let parts: Vec<Bound<'_, PyAny>> = position.try_iter()?.collect::<PyResult<_>>()?;
            if parts.len() != 4 {
                let kind = if parts.len() > 4 {
                    "too many values to unpack (expected 4)"
                } else {
                    ""
                };
                let text = if kind.is_empty() {
                    format!("not enough values to unpack (expected 4, got {})", parts.len())
                } else {
                    kind.to_string()
                };
                return Err(pyo3::exceptions::PyValueError::new_err(text));
            }
            let coords = bridge.call_method1("_validate_lat_lon", (&parts[0], &parts[1], label.as_str()))?;
            if coords.is_none() {
                return Ok(false);
            }
            let (lat_f, lon_f): (f64, f64) = coords.extract()?;
            let mut altitude = PyFloat::new(py, py_float(&parts[2])?).into_any();
            let mut wire_frame = constant_or("MAV_FRAME_GLOBAL", 0);
            if parts[3].eq("relative")? {
                let frame = dialect(bridge)?.call_method1("reposition_altitude_frame", (type_id(bridge)?,))?;
                if frame.eq("amsl")? {
                    let mut amsl = bridge.call_method1("_takeoff_altitude_amsl", (&altitude,))?;
                    if amsl.is_none() {
                        amsl = bridge.call_method1("_await_takeoff_reference", (&altitude,))?;
                    }
                    if amsl.is_none() {
                        set_error(
                            bridge,
                            &format!("{label} failed: no home altitude to measure the height from"),
                        )?;
                        return Ok(false);
                    }
                    altitude = amsl;
                } else {
                    wire_frame = constant_or("MAV_FRAME_GLOBAL_RELATIVE_ALT", 3);
                }
            }
            let mut four = values.clone();
            four.resize(4.max(four.len()), nan());
            four.truncate(4);
            command_int(
                bridge,
                command,
                wire_frame,
                floats(py, &four)?,
                e7(lat_f),
                e7(lon_f),
                &altitude,
            )?
        } else {
            let mut seven = values.clone();
            seven.resize(7.max(seven.len()), nan());
            seven.truncate(7);
            self::command(bridge, command, floats(py, &seven)?, 5.0, 1, true)?
        };
        if result != accepted() && result != constant_or("MAV_RESULT_IN_PROGRESS", 5) {
            return failure(bridge, &label, result);
        }
        console(bridge, "COMMAND", &format!("{name} accepted"), "success")?;
        Ok(true)
    }

    /// One velocity setpoint as SET_POSITION_TARGET_LOCAL_NED, no ACK: a
    /// stream whose end the vehicle's own timeout catches.
    #[pyo3(signature = (bridge, vx, vy, vz, yaw_rate_deg=None, body=None))]
    fn velocity_setpoint(
        &self,
        bridge: &Bound<'_, PyAny>,
        vx: &Bound<'_, PyAny>,
        vy: &Bound<'_, PyAny>,
        vz: &Bound<'_, PyAny>,
        yaw_rate_deg: Option<&Bound<'_, PyAny>>,
        body: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let py = bridge.py();
        let env = self.env(py);
        set_error(bridge, "")?;
        if !dialect(bridge)?
            .call_method1("velocity_mode", (type_id(bridge)?,))?
            .is_truthy()?
        {
            set_error(bridge, "Velocity control is not available on this vehicle")?;
            return Ok(false);
        }
        let yaw = yaw_rate_deg.filter(|y| !y.is_none());
        let mut inputs = vec![vx, vy, vz];
        if let Some(y) = yaw {
            inputs.push(y);
        }
        let mut numbers = Vec::new();
        for v in inputs {
            match plain_number(v)? {
                Some(f) if f.is_finite() => numbers.push(f),
                _ => {
                    set_error(bridge, "Velocity control failed: not a finite number")?;
                    return Ok(false);
                }
            }
        }
        let vmax = py_float(&env.getattr("VELOCITY_MAX_MS")?)?;
        let ymax = py_float(&env.getattr("YAW_RATE_MAX_DEG_S")?)?;
        if numbers[..3].iter().any(|v| v.abs() > vmax) || (numbers.len() == 4 && numbers[3].abs() > ymax) {
            set_error(bridge, "Velocity control failed: out of range")?;
            return Ok(false);
        }
        let lost = || -> PyResult<String> {
            Ok(format!(
                "Velocity control failed: {}",
                env.getattr("MAV_RESULT_TEXT")?.get_item(-2)?.str()?
            ))
        };
        if !truthy(bridge, "_connection_ready")? {
            set_error(bridge, &lost()?)?;
            return Ok(false);
        }
        let mut mask = py_int(&bridge.getattr("_VELOCITY_TYPE_MASK")?)?;
        if yaw.is_none() {
            mask |= py_int(&bridge.getattr("_YAW_RATE_IGNORE")?)?;
        }
        let frame = if body.map(|b| b.is_truthy()).transpose()?.unwrap_or(false) {
            constant_or("MAV_FRAME_BODY_NED", 8)
        } else {
            constant_or("MAV_FRAME_LOCAL_NED", 1)
        };
        let sent = (|| -> PyResult<bool> {
            let conn = bridge.getattr("_conn")?;
            within(&bridge.getattr("_send_lock")?, || {
                if conn.is_none() || !conn.is(&bridge.getattr("_conn")?) {
                    set_error(bridge, &lost()?)?;
                    return Ok(false);
                }
                let yaw_rad = if numbers.len() == 4 {
                    numbers[3].to_radians()
                } else {
                    0.0
                };
                let args = PyTuple::new(
                    py,
                    [
                        obj(py, 0),
                        bridge.getattr("_target_system")?.unbind(),
                        bridge.getattr("_target_component")?.unbind(),
                        obj(py, frame),
                        obj(py, mask),
                        obj(py, 0.0),
                        obj(py, 0.0),
                        obj(py, 0.0),
                        obj(py, numbers[0]),
                        obj(py, numbers[1]),
                        obj(py, numbers[2]),
                        obj(py, 0.0),
                        obj(py, 0.0),
                        obj(py, 0.0),
                        obj(py, 0.0),
                        obj(py, yaw_rad),
                    ],
                )?;
                conn.getattr("mav")?
                    .call_method1("set_position_target_local_ned_send", args)?;
                Ok(true)
            })
        })();
        match sent {
            Ok(done) => Ok(done),
            Err(err) if err.is_instance_of::<PyException>(py) => {
                let value = err.value(py);
                logger(py)?.call_method1("error", ("velocity setpoint send failed: %s", value))?;
                set_error(bridge, &format!("Velocity control send failed: {}", value.str()?))?;
                Ok(false)
            }
            Err(err) => Err(err),
        }
    }

    // ---------------------------------------------------------- missions

    /// MISSION_START, the mission mode and arming in the stack's order. PX4's
    /// MISSION_START switches and arms by itself and is then only watched;
    /// ArduPilot is put in its arming mode and armed first, and disarmed
    /// again if the start is refused after that.
    #[pyo3(signature = (bridge, action, count, mark=None))]
    fn start_uploaded_mission(
        &self,
        bridge: &Bound<'_, PyAny>,
        action: &str,
        count: &Bound<'_, PyAny>,
        mark: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let py = bridge.py();
        let mark = match mark.filter(|m| !m.is_none()) {
            Some(m) => m.clone(),
            None => op_lock(bridge)?.call_method0("abort_mark")?,
        };
        let range = bridge.call_method1("_mission_start_range", (count,))?;
        let (first, last) = (range.get_item(0)?, range.get_item(1)?);
        let d = dialect(bridge)?;
        let arm_mode = d.call_method1("mission_arm_mode", (type_id(bridge)?,))?;
        let mut armed_here = false;
        if arm_mode.is_truthy()? && !snap_get(bridge, "armed")?.is_truthy()? {
            let entered = if arm_mode.eq(d.getattr("mission_mode")?)? {
                calls_true(bridge, "_enter_mission_mode", (action,))?
            } else {
                calls_true(bridge, "_enter_guided", (action,))?
            };
            if !entered {
                return Ok(false);
            }
            if Self::interrupted(bridge, &mark, action)? {
                return Ok(false);
            }
            if !calls_true(bridge, "arm", (true,))? {
                return Ok(false);
            }
            armed_here = true;
        }
        if Self::interrupted(bridge, &mark, action)? {
            return Ok(false);
        }
        let params = PyList::new(
            py,
            [
                first.unbind(),
                last.unbind(),
                obj(py, nan()),
                obj(py, nan()),
                obj(py, nan()),
                obj(py, nan()),
                obj(py, nan()),
            ],
        )?;
        let start = cmd_id(py, "MAV_CMD_MISSION_START", 300);
        let result = within(&op_lock(bridge)?, || {
            command(bridge, &start, params.clone(), 5.0, 1, false)
        })?;
        if result != accepted() {
            if armed_here {
                console(
                    bridge,
                    "MISSION",
                    "Mission start refused after arming, disarming again",
                    "warning",
                )?;
                bridge.call_method1("arm", (false,))?;
            }
            return failure(bridge, action, result);
        }
        if arm_mode.is_truthy()? {
            return Ok(true);
        }
        let mode = d.getattr("mission_mode")?;
        if mode.is_truthy()? && !bridge.call_method1("_wait_for_mode", (&mode,))?.is_truthy()? {
            let reported = snap_get(bridge, "mode")?;
            if reported.ne(&mode)? {
                let shown = if reported.is_truthy()? {
                    reported.str()?.to_string()
                } else {
                    "no mode".to_string()
                };
                let text = format!(
                    "{action}: accepted, but the vehicle reports {shown} rather than {}",
                    mode.str()?
                );
                console(bridge, "MISSION", &text, "warning")?;
            }
        }
        Ok(true)
    }

    /// One point for an airborne vehicle as MAV_CMD_DO_REPOSITION; None when
    /// the vehicle cannot take it, so the caller falls back to a mission.
    fn reposition_to(
        &self,
        bridge: &Bound<'_, PyAny>,
        lat: f64,
        lon: f64,
        alt_agl: &Bound<'_, PyAny>,
    ) -> PyResult<Option<bool>> {
        let py = bridge.py();
        let (frame, altitude) = if dialect(bridge)?
            .call_method1("reposition_altitude_frame", (type_id(bridge)?,))?
            .eq("relative")?
        {
            (
                constant_or("MAV_FRAME_GLOBAL_RELATIVE_ALT", 3),
                PyFloat::new(py, py_float(alt_agl)?),
            )
        } else {
            let amsl = bridge.call_method1("_takeoff_altitude_amsl", (alt_agl,))?;
            if amsl.is_none() {
                return Ok(None);
            }
            (constant_or("MAV_FRAME_GLOBAL", 0), PyFloat::new(py, py_float(&amsl)?))
        };
        let ground = self.ground(bridge)?;
        let action = if ground { "Drive to point" } else { "Fly to point" };
        if !calls_true(bridge, "_enter_guided", (action,))? {
            return Ok(Some(false));
        }
        let change_mode = py_float(&self.env(py).getattr("DO_REPOSITION_CHANGE_MODE")?)?;
        let params = floats(py, &[-1.0, change_mode, 0.0, nan()])?;
        let result = command_int(
            bridge,
            &cmd_id(py, "MAV_CMD_DO_REPOSITION", 192),
            frame,
            params,
            e7(lat),
            e7(lon),
            altitude.as_any(),
        )?;
        if result == constant_or("MAV_RESULT_UNSUPPORTED", 3) {
            return Ok(None);
        }
        if result != accepted() {
            return Ok(Some(failure(bridge, action, result)?));
        }
        if ground {
            console(bridge, "GOTOPOINTS", "Driving to point", "success")?;
        } else {
            let text = format!("Flying to point at {} m AGL", fixed(py_float(alt_agl)?, 0));
            console(bridge, "GOTOPOINTS", &text, "success")?;
        }
        Ok(Some(true))
    }

    /// One point as a reposition rather than a mission: an aircraft in the
    /// air, or an armed rover on a stack that reads the altitude as relative.
    fn can_reposition(&self, bridge: &Bound<'_, PyAny>, ground: bool, on_ground: bool) -> PyResult<bool> {
        if !ground {
            return Ok(!on_ground);
        }
        let relative = dialect(bridge)?
            .call_method1("reposition_altitude_frame", (type_id(bridge)?,))?
            .eq("relative")?;
        Ok(relative && snap_get(bridge, "armed")?.is_truthy()?)
    }

    /// Fly to the given points in order by uploading a mission and starting
    /// it in the stack's order; one point for an airborne vehicle is a
    /// reposition, which leaves the stored mission alone.
    fn fly_to_points(&self, bridge: &Bound<'_, PyAny>, points: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let mark = op_lock(bridge)?.call_method0("abort_mark")?;
        let ground = self.ground(bridge)?;
        let label = if ground { "Drive to points" } else { "Fly to points" };
        let cleaned = bridge.call_method1("_validate_fly_to_points", (points,))?;
        if cleaned.is_none() {
            return Ok(false);
        }
        if !truthy(bridge, "_connection_ready")? {
            return failure(bridge, label, -2);
        }
        let on_ground = !ground && !truthy(bridge, "_is_airborne")?;
        let cleaned: Vec<Bound<'_, PyAny>> = cleaned.try_iter()?.collect::<PyResult<_>>()?;
        if cleaned.len() == 1 && calls_true(bridge, "_can_reposition", (ground, on_ground))? {
            let point = py
                .get_type::<PyTuple>()
                .call1((&cleaned[0],))?
                .downcast_into::<PyTuple>()?;
            let handled = bridge.call_method1("_reposition_to", point)?;
            if !handled.is_none() {
                return handled.is_truthy();
            }
        }
        let mut items: Vec<Bound<'_, PyAny>> = Vec::new();
        let mut seq: i64 = 0;
        let spec = |seq: i64, cmd: PyObject, lat: PyObject, lon: PyObject, alt: PyObject, p1: f64| {
            bridge.call_method1(
                "_build_mission_item_spec",
                (seq, cmd, lat, lon, alt, p1, nan(), nan(), nan()),
            )
        };
        if on_ground {
            let takeoff_agl = cleaned[0].get_item(2)?;
            let at = bridge.call_method1("_takeoff_item_position", (&cleaned[0],))?;
            items.push(spec(
                seq,
                cmd_id(py, "MAV_CMD_NAV_TAKEOFF", 22).unbind(),
                at.get_item(0)?.unbind(),
                at.get_item(1)?.unbind(),
                takeoff_agl.unbind(),
                nan(),
            )?);
            seq += 1;
        }
        for point in &cleaned {
            let (lat, lon, alt) = (point.get_item(0)?, point.get_item(1)?, point.get_item(2)?);
            items.push(spec(
                seq,
                cmd_id(py, "MAV_CMD_NAV_WAYPOINT", 16).unbind(),
                lat.unbind(),
                lon.unbind(),
                alt.unbind(),
                0.0,
            )?);
            seq += 1;
        }
        let planned = items.len();
        let items = bridge.call_method1("_with_mission_home_slot", (PyList::new(py, items)?,))?;
        let uploaded = within(&op_lock(bridge)?, || -> PyResult<Option<bool>> {
            if !truthy(bridge, "_connection_ready")? {
                return Ok(Some(failure(bridge, label, -2)?));
            }
            console(
                bridge,
                "GOTOPOINTS",
                &format!("Uploading {planned} mission item(s) …"),
                "info",
            )?;
            let ack = py_int(&bridge.call_method1("_upload_mission", (&items,))?)?;
            if ack != constant_or("MAV_MISSION_ACCEPTED", 0) {
                let result = if ack < 0 {
                    ack
                } else {
                    py_int(&bridge.call_method1("_mission_ack_to_result", (ack,))?)?
                };
                return Ok(Some(failure(bridge, label, result)?));
            }
            bridge.call_method1("_note_vehicle_mission", (py.None(),))?;
            Ok(None)
        })?;
        if let Some(done) = uploaded {
            return Ok(done);
        }
        console(bridge, "GOTOPOINTS", "Mission accepted; starting …", "info")?;
        if !bridge
            .call_method1("_start_uploaded_mission", (label, planned, &mark))?
            .is_truthy()?
        {
            return Ok(false);
        }
        console(bridge, "GOTOPOINTS", &format!("{label} started"), "success")?;
        Ok(true)
    }

    /// Run the mission already on the vehicle; *count* is how many items
    /// were uploaded.
    fn start_mission(&self, bridge: &Bound<'_, PyAny>, count: &Bound<'_, PyAny>) -> PyResult<bool> {
        set_error(bridge, "")?;
        let mark = op_lock(bridge)?.call_method0("abort_mark")?;
        let valid = !count.is_instance_of::<PyBool>() && count.is_instance_of::<PyInt>() && count.gt(0)?;
        if !valid {
            set_error(bridge, "mission item count must be positive")?;
            return Ok(false);
        }
        if !truthy(bridge, "_connection_ready")? {
            return failure(bridge, "Mission start", -2);
        }
        if !bridge
            .call_method1("_start_uploaded_mission", ("Mission start", count, &mark))?
            .is_truthy()?
        {
            return Ok(false);
        }
        console(bridge, "MISSION", "Mission started", "success")?;
        Ok(true)
    }

    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.env)
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.flight")?;
    m.add_class::<FlightEngine>()?;
    parent.add_submodule(&m)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("corvus_link.flight", &m)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_round_half_to_even() {
        assert_eq!(e7(48.1234567), 481234567);
        assert_eq!(e7(-11.00000005), -110000000);
        assert_eq!(e7(0.00000015), 2);
    }

    #[test]
    fn fixed_point_text_matches_python_format() {
        assert_eq!(fixed(10.0, 0), "10");
        assert_eq!(fixed(2.5, 0), "2");
        assert_eq!(fixed(512.45, 1), "512.5");
        assert_eq!(fixed(-0.4, 0), "-0");
    }
}
