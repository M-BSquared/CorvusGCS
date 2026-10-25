//! The Setup pages' commands that are not parameter writes: sensor
//! calibration (start, cancel, ArduPilot's accelerometer positions), the
//! bench motor test and its stop, the autotune (a command on PX4, a flight
//! mode on ArduPilot, with PX4's progress stream), and the two reboots.
//!
//! Which command each step is stays the dialect's (`corvus.autopilot`), and
//! the command transaction is the bridge's (`_send_command_and_wait`); this
//! engine owns the checks, the order and what is remembered between steps.
//! Class-level tunables are read from the bridge, so a test that changes one
//! on the bridge class changes it here.

use std::sync::Mutex;

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyFrozenSet, PyInt, PyList, PyModule};

use crate::lock;
use crate::protocol::host::{armed, console, obj, py_float, py_int, set_error, truthy};
use crate::wire::defs::constant_or;

fn accepted() -> i64 {
    constant_or("MAV_RESULT_ACCEPTED", 0)
}

fn in_progress() -> i64 {
    constant_or("MAV_RESULT_IN_PROGRESS", 5)
}

struct State {
    active_calibration: PyObject,
    autotune_return_mode: String,
    motors_tested: Vec<i64>,
}

/// The setup commands of one bridge.
#[pyclass(name = "SetupEngine", module = "corvus_link.setup", frozen)]
pub struct SetupEngine {
    env: PyObject,
    st: Mutex<State>,
}

/// `bridge._send_command_and_wait(command, params, timeout=..., retries=0, **extra)`.
fn command(
    bridge: &Bound<'_, PyAny>,
    cmd: &Bound<'_, PyAny>,
    params: &Bound<'_, PyAny>,
    timeout: &Bound<'_, PyAny>,
    extra: Option<&Bound<'_, PyDict>>,
) -> PyResult<i64> {
    let kwargs = PyDict::new(bridge.py());
    kwargs.set_item("timeout", timeout)?;
    kwargs.set_item("retries", 0)?;
    if let Some(extra) = extra {
        kwargs.update(extra.as_mapping())?;
    }
    py_int(&bridge.call_method("_send_command_and_wait", (cmd, params), Some(&kwargs))?)
}

fn failure(bridge: &Bound<'_, PyAny>, action: &str, result: i64) -> PyResult<bool> {
    bridge.call_method1("_command_failure", (action, result))?.is_truthy()
}

fn store_update(bridge: &Bound<'_, PyAny>, pairs: &[(&str, PyObject)]) -> PyResult<()> {
    let kwargs = PyDict::new(bridge.py());
    for (k, v) in pairs {
        kwargs.set_item(*k, v)?;
    }
    bridge.getattr("_store")?.call_method("update", (), Some(&kwargs))?;
    Ok(())
}

fn snapshot<'py>(bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    bridge.getattr("_store")?.call_method0("get_snapshot")
}

/// `format(value, "g")`, so a message prints numbers as it always has.
fn g(value: &Bound<'_, PyAny>) -> PyResult<String> {
    value
        .py()
        .import("builtins")?
        .call_method1("format", (value, "g"))?
        .extract()
}

fn nans(py: Python<'_>, first: f64, second: f64) -> PyResult<Bound<'_, PyList>> {
    let nan = f64::NAN;
    PyList::new(py, [first, second, nan, nan, nan, nan, nan])
}

impl SetupEngine {
    fn dialect<'py>(bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        bridge.getattr("_dialect")
    }

    fn plan_ok(plan: &Bound<'_, PyAny>) -> PyResult<bool> {
        plan.getattr("ok")?.is_truthy()
    }

    fn refuse(bridge: &Bound<'_, PyAny>, plan: &Bound<'_, PyAny>) -> PyResult<bool> {
        set_error(bridge, &plan.getattr("unsupported")?.str()?.to_string())?;
        Ok(false)
    }

    fn send_plan(
        bridge: &Bound<'_, PyAny>,
        plan: &Bound<'_, PyAny>,
        timeout: &Bound<'_, PyAny>,
        extra: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<i64> {
        command(
            bridge,
            &plan.getattr("command")?,
            &plan.call_method0("param_list")?,
            timeout,
            extra,
        )
    }

    fn precondition(bridge: &Bound<'_, PyAny>) -> PyResult<String> {
        let snap = snapshot(bridge)?;
        if !snap.call_method1("get", ("armed",))?.is_truthy()? {
            return Ok("the autotune runs in flight: arm the vehicle and take off before starting it".to_string());
        }
        let landed = snap.call_method1("get", ("landed_state",))?;
        let landed = if landed.is_truthy()? { py_int(&landed)? } else { 0 };
        if landed == constant_or("MAV_LANDED_STATE_ON_GROUND", 1) {
            return Ok(
                "the vehicle is still on the ground: hold a stable hover before starting the autotune".to_string(),
            );
        }
        Ok(String::new())
    }
}

#[pymethods]
impl SetupEngine {
    /// *env* is ``corvus.mavlink_setup``, which holds the dialect module and
    /// the result texts.
    #[new]
    fn new(py: Python<'_>, env: PyObject) -> Self {
        SetupEngine {
            env,
            st: Mutex::new(State {
                active_calibration: obj(py, ""),
                autotune_return_mode: String::new(),
                motors_tested: Vec::new(),
            }),
        }
    }

    #[getter]
    fn active_calibration(&self, py: Python<'_>) -> PyObject {
        lock(&self.st).active_calibration.clone_ref(py)
    }
    #[setter]
    fn set_active_calibration(&self, value: PyObject) {
        lock(&self.st).active_calibration = value;
    }
    #[getter]
    fn autotune_return_mode(&self) -> String {
        lock(&self.st).autotune_return_mode.clone()
    }
    #[setter]
    fn set_autotune_return_mode(&self, value: String) {
        lock(&self.st).autotune_return_mode = value;
    }
    #[getter]
    fn motors_tested(&self) -> Vec<i64> {
        lock(&self.st).motors_tested.clone()
    }
    #[setter]
    fn set_motors_tested(&self, value: Vec<i64>) {
        lock(&self.st).motors_tested = value;
    }

    /// Start a sensor calibration. The caller holds the operation lock.
    fn calibrate(&self, bridge: &Bound<'_, PyAny>, sensor: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let name = sensor.str()?.to_string();
        let dialect = Self::dialect(bridge)?;
        let plan = dialect.call_method1("calibration", (sensor,))?;
        if plan.is_none() {
            let px4 = self
                .env
                .bind(py)
                .getattr("autopilot_dialect")?
                .getattr("PX4Dialect")?
                .getattr("_CALIBRATION")?;
            let text = if px4.contains(sensor)? {
                format!("{} has no {name} calibration", dialect.getattr("label")?.str()?)
            } else {
                format!("unknown calibration: {name}")
            };
            set_error(bridge, &text)?;
            return Ok(false);
        }
        if !Self::plan_ok(&plan)? {
            return Self::refuse(bridge, &plan);
        }
        if armed(bridge)? {
            set_error(bridge, "cannot calibrate while armed")?;
            return Ok(false);
        }
        let action = format!("Calibrate {name}");
        if !truthy(bridge, "_connection_ready")? {
            return failure(bridge, &action, -2);
        }
        let result = Self::send_plan(bridge, &plan, PyFloat::new(py, 5.0).as_any(), None)?;
        if result != accepted() {
            return failure(bridge, &action, result);
        }
        lock(&self.st).active_calibration = sensor.clone().unbind();
        console(bridge, "CALIBRATE", &format!("{name} calibration started"), "success")?;
        Ok(true)
    }

    /// Abort whatever calibration is running; never gated on the armed state.
    /// The caller holds the operation lock with priority.
    fn cancel_calibration(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        if !truthy(bridge, "_connection_ready")? {
            return failure(bridge, "Cancel calibration", -2);
        }
        let active = lock(&self.st).active_calibration.clone_ref(py);
        let plan = Self::dialect(bridge)?.call_method1("cancel_calibration", (active,))?;
        if !Self::plan_ok(&plan)? {
            return Self::refuse(bridge, &plan);
        }
        // PX4 answers a cancel twice: TEMPORARILY_REJECTED from Commander at
        // once, ACCEPTED from the calibration routine when it next checks.
        let extra = PyDict::new(py);
        extra.set_item(
            "interim",
            PyFrozenSet::new(py, [constant_or("MAV_RESULT_TEMPORARILY_REJECTED", 1)])?,
        )?;
        let timeout = bridge.getattr("CALIBRATION_CANCEL_ACK_S")?;
        let result = Self::send_plan(bridge, &plan, &timeout, Some(&extra))?;
        if result != accepted() {
            return failure(bridge, "Cancel calibration", result);
        }
        lock(&self.st).active_calibration = obj(py, "");
        console(bridge, "CALIBRATE", "calibration cancelled", "warning")?;
        Ok(true)
    }

    /// Confirm the accelerometer position ArduPilot asked for. The caller
    /// holds the operation lock.
    fn accel_position(&self, bridge: &Bound<'_, PyAny>, position: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let plan = Self::dialect(bridge)?.call_method1("accel_position", (position,))?;
        if !Self::plan_ok(&plan)? {
            return Self::refuse(bridge, &plan);
        }
        if !truthy(bridge, "_connection_ready")? {
            return failure(bridge, "Calibration position", -2);
        }
        let result = Self::send_plan(bridge, &plan, PyFloat::new(py, 5.0).as_any(), None)?;
        if result != accepted() {
            return failure(bridge, "Calibration position", result);
        }
        console(
            bridge,
            "CALIBRATE",
            &format!("position confirmed: {}", position.str()?),
            "info",
        )?;
        Ok(true)
    }

    /// Spin one motor on the bench, propellers off, for a time the vehicle
    /// counts down itself. The caller holds the operation lock.
    fn motor_test(
        &self,
        bridge: &Bound<'_, PyAny>,
        motor: &Bound<'_, PyAny>,
        throttle_pct: &Bound<'_, PyAny>,
        duration_s: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let number = if motor.is_instance_of::<PyBool>() || !motor.is_instance_of::<PyInt>() {
            None
        } else {
            motor.extract::<i64>().ok().filter(|m| (1..=16).contains(m))
        };
        let Some(number) = number else {
            set_error(bridge, "motor must be between 1 and 16")?;
            return Ok(false);
        };
        let throttle = py_float(throttle_pct)?;
        if !(0.0..=100.0).contains(&throttle) {
            set_error(bridge, "throttle must be between 0 and 100 percent")?;
            return Ok(false);
        }
        let max: f64 = bridge.getattr("MOTOR_TEST_MAX_DURATION_S")?.extract()?;
        let wanted = py_float(duration_s)?;
        let capped = if wanted < max { wanted } else { max };
        let duration = if capped > 0.0 { capped } else { 0.0 };
        if armed(bridge)? {
            set_error(bridge, "cannot test motors while armed")?;
            return Ok(false);
        }
        let action = format!("Motor test {number}");
        if !truthy(bridge, "_connection_ready")? {
            return failure(bridge, &action, -2);
        }
        let plan = Self::dialect(bridge)?.call_method1("motor_test", (number, throttle, duration))?;
        let result = Self::send_plan(bridge, &plan, PyFloat::new(py, 5.0).as_any(), None)?;
        if result != accepted() {
            return failure(bridge, &action, result);
        }
        {
            let mut st = lock(&self.st);
            st.motors_tested.retain(|m| *m != number);
            st.motors_tested.push(number);
        }
        let text = format!(
            "motor {number} test at {}% for {}s",
            g(throttle_pct)?,
            g(&PyFloat::new(py, duration))?
        );
        console(bridge, "MOTOR", &text, "warning")?;
        Ok(true)
    }

    /// Stop every motor, the ones this session spun first. Never gated on the
    /// armed state. The caller holds the operation lock with priority.
    fn stop_motor_test(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        if !truthy(bridge, "_connection_ready")? {
            return failure(bridge, "Stop motor test", -2);
        }
        let tested: Vec<i64> = lock(&self.st).motors_tested.iter().rev().copied().collect();
        let mut order = tested.clone();
        order.extend((1..=8).filter(|m| !tested.contains(m)));
        let mut ok = true;
        let dialect = Self::dialect(bridge)?;
        for number in order {
            let plan = dialect.call_method1("motor_stop", (number,))?;
            if Self::send_plan(bridge, &plan, PyFloat::new(py, 2.0).as_any(), None)? != accepted() {
                ok = false;
            }
        }
        if !ok {
            set_error(bridge, "motor stop not confirmed for every motor")?;
        }
        console(bridge, "MOTOR", "motor test stopped", "warning")?;
        Ok(ok)
    }

    /// PX4's autotune, a command. The caller holds the operation lock.
    fn autotune(&self, bridge: &Bound<'_, PyAny>, axis: &Bound<'_, PyAny>, enable: bool) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let axis_val = bridge.getattr("_AUTOTUNE_AXIS_MAP")?.call_method1("get", (axis,))?;
        let name = axis.str()?.to_string();
        if axis_val.is_none() {
            set_error(bridge, &format!("unknown autotune axis: {name}"))?;
            return Ok(false);
        }
        let plan = Self::dialect(bridge)?.call_method1("autotune_plan", (bridge.getattr("_mav_type_id")?, enable))?;
        if !Self::plan_ok(&plan)? {
            return Self::refuse(bridge, &plan);
        }
        if enable {
            let problem = Self::precondition(bridge)?;
            if !problem.is_empty() {
                set_error(bridge, &problem)?;
                return Ok(false);
            }
        }
        let action = format!("Autotune {name}");
        if !truthy(bridge, "_connection_ready")? {
            return failure(bridge, &action, -2);
        }
        let extra = PyDict::new(py);
        extra.set_item("accept_in_progress", true)?;
        let params = nans(py, if enable { 1.0 } else { 0.0 }, py_float(&axis_val)?)?;
        let result = command(
            bridge,
            &plan.getattr("command")?,
            params.as_any(),
            PyFloat::new(py, 5.0).as_any(),
            Some(&extra),
        )?;
        if result != accepted() && result != in_progress() {
            if enable {
                store_update(
                    bridge,
                    &[("autotune_state", obj(py, "failed")), ("autotune_progress", obj(py, 0))],
                )?;
            }
            return failure(bridge, &action, result);
        }
        if enable {
            store_update(
                bridge,
                &[
                    ("autotune_state", obj(py, "running")),
                    ("autotune_progress", obj(py, 0)),
                ],
            )?;
            console(bridge, "AUTOTUNE", "Autotune started", "success")?;
        } else {
            store_update(
                bridge,
                &[("autotune_state", obj(py, "")), ("autotune_progress", obj(py, 0))],
            )?;
            console(bridge, "AUTOTUNE", "Autotune stopped", "warning")?;
        }
        Ok(true)
    }

    /// ArduPilot's autotune, a flight mode; stopping returns to the mode the
    /// vehicle was in, else LOITER.
    fn autotune_by_mode(&self, bridge: &Bound<'_, PyAny>, enable: bool) -> PyResult<bool> {
        let py = bridge.py();
        let dialect = Self::dialect(bridge)?;
        let mode = dialect.call_method1("autotune_mode", (bridge.getattr("_mav_type_id")?,))?;
        if !mode.is_truthy()? {
            set_error(
                bridge,
                &format!(
                    "{} has no autotune mode on this vehicle",
                    dialect.getattr("label")?.str()?
                ),
            )?;
            return Ok(false);
        }
        let mode: String = mode.extract()?;
        if enable {
            let problem = Self::precondition(bridge)?;
            if !problem.is_empty() {
                set_error(bridge, &problem)?;
                return Ok(false);
            }
            let previous = snapshot(bridge)?.call_method1("get", ("mode",))?;
            let previous: String = if previous.is_truthy()? {
                previous.extract()?
            } else {
                String::new()
            };
            if !bridge.call_method1("set_mode", (mode.as_str(),))?.is_truthy()? {
                store_update(
                    bridge,
                    &[("autotune_state", obj(py, "failed")), ("autotune_progress", obj(py, 0))],
                )?;
                return Ok(false);
            }
            lock(&self.st).autotune_return_mode = if previous != mode { previous } else { String::new() };
            store_update(
                bridge,
                &[
                    ("autotune_state", obj(py, "running")),
                    ("autotune_progress", obj(py, 0)),
                ],
            )?;
            console(
                bridge,
                "AUTOTUNE",
                &format!("{mode} engaged. ArduPilot narrates the tune over the console"),
                "success",
            )?;
            return Ok(true);
        }
        let modes = bridge.getattr("_mode_values")?;
        let mut target = lock(&self.st).autotune_return_mode.clone();
        if !modes.contains(target.as_str())? {
            target = if modes.contains("LOITER")? {
                "LOITER".to_string()
            } else {
                String::new()
            };
        }
        if target.is_empty() {
            set_error(
                bridge,
                "no mode to return to. Select one on the mode selector to leave the autotune",
            )?;
            return Ok(false);
        }
        if !bridge.call_method1("set_mode", (target.as_str(),))?.is_truthy()? {
            return Ok(false);
        }
        lock(&self.st).autotune_return_mode.clear();
        store_update(
            bridge,
            &[("autotune_state", obj(py, "")), ("autotune_progress", obj(py, 0))],
        )?;
        console(
            bridge,
            "AUTOTUNE",
            &format!("Autotune left, now in {target}"),
            "warning",
        )?;
        Ok(true)
    }

    /// Why the autotune cannot start right now, or "".
    fn autotune_precondition(&self, bridge: &Bound<'_, PyAny>) -> PyResult<String> {
        Self::precondition(bridge)
    }

    /// Latch PX4's autotune progress and outcome out of its COMMAND_ACKs.
    fn handle_autotune_ack(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let result = match msg.getattr("result").and_then(|r| py_int(&r)) {
            Ok(r) => r,
            Err(err) if crate::protocol::host::caught_by(py, &err, &["TypeError", "ValueError"]) => return Ok(()),
            Err(err) => return Err(err),
        };
        if result == in_progress() {
            let progress = match msg.getattr("progress") {
                Ok(p) if p.is_truthy()? => py_int(&p).unwrap_or(0),
                _ => 0,
            };
            store_update(
                bridge,
                &[
                    ("autotune_state", obj(py, "running")),
                    ("autotune_progress", obj(py, progress.clamp(0, 100))),
                ],
            )?;
            return Ok(());
        }
        if result == accepted() {
            let state = snapshot(bridge)?.call_method1("get", ("autotune_state",))?;
            if state.eq("running")? {
                store_update(
                    bridge,
                    &[("autotune_state", obj(py, "done")), ("autotune_progress", obj(py, 100))],
                )?;
            }
            return Ok(());
        }
        store_update(bridge, &[("autotune_state", obj(py, "failed"))])?;
        let text = self
            .env
            .bind(py)
            .getattr("MAV_RESULT_TEXT")?
            .call_method1("get", (result, format!("result={result}")))?;
        bridge.getattr("_store")?.call_method1(
            "merge_warning",
            (format!("Autotune refused: {}", text.str()?), "warning"),
        )?;
        Ok(())
    }

    /// Restart the autopilot; refused while armed. The caller holds the
    /// operation lock.
    fn reboot(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        if armed(bridge)? {
            set_error(bridge, "cannot reboot while armed")?;
            return Ok(false);
        }
        if !truthy(bridge, "_connection_ready")? {
            return failure(bridge, "Reboot", -2);
        }
        let cmd = obj(py, constant_or("MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN", 246)).into_bound(py);
        let params = PyList::new(py, [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])?;
        let result = command(bridge, &cmd, params.as_any(), PyFloat::new(py, 3.0).as_any(), None)?;
        if result != accepted() {
            return failure(bridge, "Reboot", result);
        }
        bridge.call_method0("_reset_parameter_cache")?;
        console(
            bridge,
            "REBOOT",
            "Reboot accepted. The autopilot restarts now",
            "success",
        )?;
        Ok(true)
    }

    /// Restart into the USB bootloader for flashing; refused while armed.
    /// The caller holds the operation lock.
    fn reboot_to_bootloader(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        if armed(bridge)? {
            set_error(bridge, "cannot reboot to bootloader while armed")?;
            return Ok(false);
        }
        if !truthy(bridge, "_connection_ready")? {
            return failure(bridge, "Reboot to bootloader", -2);
        }
        let cmd = obj(py, constant_or("MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN", 246)).into_bound(py);
        let params = nans(py, 3.0, f64::NAN)?;
        let result = command(bridge, &cmd, params.as_any(), PyFloat::new(py, 3.0).as_any(), None)?;
        if result != accepted() {
            return failure(bridge, "Reboot to bootloader", result);
        }
        console(bridge, "REBOOT", "Reboot to bootloader accepted", "success")?;
        Ok(true)
    }

    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.env)?;
        if let Ok(st) = self.st.try_lock() {
            visit.call(&st.active_calibration)?;
        }
        Ok(())
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.setup")?;
    m.add_class::<SetupEngine>()?;
    parent.add_submodule(&m)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("corvus_link.setup", &m)?;
    Ok(())
}
