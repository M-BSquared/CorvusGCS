//! SiK telemetry radio configuration, as ``corvus_link.sik``: what a radio's
//! AT replies mean and which writes are acceptable (`SikConfigEngine`), and
//! the AT dialogue itself, the baud scan and the port hand-over around it
//! (`SikServiceEngine`).
//!
//! The register tables, their labels and hints stay in ``corvus.sik_config``
//! as data and are read from there at call time. Python's own operators
//! decide wherever the caller can hand in anything, so the answers, and the
//! errors, are the ones the Python module gave.

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyModule, PyString, PyTuple};

use crate::protocol::host::{caught_by, py_float};
use crate::protocol::telemetry::py_round;

fn kw<'py>(py: Python<'py>, pairs: &[(&str, PyObject)]) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    for (k, v) in pairs {
        d.set_item(*k, v)?;
    }
    Ok(d)
}

/// `template.format(*args)`.
fn fmt<'py, A: pyo3::call::PyCallArgs<'py>>(py: Python<'py>, template: &str, args: A) -> PyResult<Bound<'py, PyAny>> {
    PyString::new(py, template).call_method1("format", args)
}

fn int_of<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    value.py().get_type::<PyInt>().call1((value,))
}

fn text_of<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = value.py();
    let held = if value.is_truthy()? {
        value.clone()
    } else {
        PyString::new(py, "").into_any()
    };
    Ok(held.str()?.into_any())
}

/// `str(text or "").replace("\r", "\n").split("\n")`.
fn lines_of<'py>(text: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    text_of(text)?
        .call_method1("replace", ("\r", "\n"))?
        .call_method1("split", ("\n",))
}

fn or_empty_dict(value: Bound<'_, PyAny>) -> PyResult<Bound<'_, PyAny>> {
    if value.is_truthy()? {
        Ok(value)
    } else {
        Ok(PyDict::new(value.py()).into_any())
    }
}

// ------------------------------------------------------------- config ----

/// The reading and checking half of the radio page.
#[pyclass(name = "SikConfigEngine", module = "corvus_link.sik", frozen)]
pub struct SikConfigEngine {
    env: PyObject,
}

impl SikConfigEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn error<'py>(&self, py: Python<'py>, text: Bound<'py, PyAny>) -> PyResult<PyErr> {
        Ok(PyErr::from_value(
            self.env(py).getattr("SikConfigError")?.call1((text,))?,
        ))
    }
}

#[pymethods]
impl SikConfigEngine {
    /// *env* is ``corvus.sik_config``.
    #[new]
    fn new(env: PyObject) -> Self {
        SikConfigEngine { env }
    }

    /// The real rate a SERIAL_SPEED register stands for.
    fn baud_from_register<'py>(&self, value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = value.py();
        let number = int_of(value)?;
        if number.gt(2000)? {
            return Ok(number);
        }
        let table = self.env(py).getattr("_BAUD_BY_REGISTER")?;
        let fallback = number.mul(1000)?;
        table.call_method1("get", (&number, fallback))
    }

    /// The inverse, for a real baud rate.
    fn register_from_baud<'py>(&self, baud: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = baud.py();
        let number = int_of(baud)?;
        let table = self.env(py).getattr("_REGISTER_BY_BAUD")?;
        if table.contains(&number)? {
            return table.get_item(&number);
        }
        if number.ge(2000)? {
            return number.floor_div(1000);
        }
        Ok(number)
    }

    /// A raw RSSI count in dBm.
    fn rssi_to_dbm(&self, rssi: &Bound<'_, PyAny>) -> PyResult<f64> {
        Ok((py_float(rssi)? / 1.9) - 127.0)
    }

    /// An ``ATI5`` / ``RTI5`` report as ``{name: {number, value}}``.
    fn parse_registers<'py>(&self, text: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
        let py = text.py();
        let out = PyDict::new(py);
        let pattern = self.env(py).getattr("_ATI5_RE")?;
        for line in lines_of(text)?.try_iter()? {
            let found = pattern.call_method1("match", (line?,))?;
            if found.is_none() {
                continue;
            }
            let number = found.call_method1("group", (1,))?;
            let name = found.call_method1("group", (2,))?.call_method0("upper")?;
            let value = found.call_method1("group", (3,))?;
            let entry = PyDict::new(py);
            let built = (|| -> PyResult<()> {
                entry.set_item("number", int_of(&number)?)?;
                entry.set_item("value", int_of(&value)?)
            })();
            match built {
                Err(err) if caught_by(py, &err, &["ValueError"]) => continue,
                Err(err) => return Err(err),
                Ok(()) => out.set_item(name, entry)?,
            }
        }
        Ok(out)
    }

    /// An ``ATI7`` link report as signal, noise and fade margin, or None.
    fn parse_rssi<'py>(&self, text: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyDict>>> {
        let py = text.py();
        let found = self
            .env(py)
            .getattr("_RSSI_RE")?
            .call_method1("search", (text_of(text)?,))?;
        if found.is_none() {
            return Ok(None);
        }
        let mut counts = Vec::new();
        for i in 1..=4 {
            counts.push(int_of(&found.call_method1("group", (i,))?)?);
        }
        let dbm = |rssi: &Bound<'py, PyAny>| -> PyResult<f64> { Ok(py_round(self.rssi_to_dbm(rssi)?, 1)) };
        let margin = |signal: &Bound<'py, PyAny>, noise: &Bound<'py, PyAny>| -> PyResult<f64> {
            Ok(py_round(py_float(&signal.sub(noise)?.div(2.0)?)?, 1))
        };
        let d = PyDict::new(py);
        d.set_item("local_rssi", &counts[0])?;
        d.set_item("remote_rssi", &counts[1])?;
        d.set_item("local_noise", &counts[2])?;
        d.set_item("remote_noise", &counts[3])?;
        d.set_item("local_dbm", dbm(&counts[0])?)?;
        d.set_item("remote_dbm", dbm(&counts[1])?)?;
        d.set_item("local_margin_db", margin(&counts[0], &counts[2])?)?;
        d.set_item("remote_margin_db", margin(&counts[1], &counts[3])?)?;
        Ok(Some(d))
    }

    /// The firmware banner of an ``ATI`` reply, or "".
    fn parse_version<'py>(&self, text: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = text.py();
        for line in lines_of(text)?.try_iter()? {
            let candidate = line?.call_method0("strip")?;
            if !candidate.is_truthy()? {
                continue;
            }
            let upper = candidate.call_method0("upper")?;
            if upper.eq("OK")? || upper.eq("ERROR")? {
                continue;
            }
            if upper
                .call_method1("startswith", (PyTuple::new(py, ["AT", "RT"])?,))?
                .is_truthy()?
            {
                continue;
            }
            return Ok(candidate);
        }
        Ok(PyString::new(py, "").into_any())
    }

    /// Registers that must match at both ends and do not.
    fn compare<'py>(&self, local: &Bound<'py, PyAny>, remote: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
        let py = local.py();
        let out = PyList::empty(py);
        if !remote.is_truthy()? {
            return Ok(out);
        }
        let env = self.env(py);
        let must_match = env.getattr("MUST_MATCH")?;
        let local_values = or_empty_dict(local.call_method1("get", ("values",))?)?;
        let remote_values = or_empty_dict(remote.call_method1("get", ("values",))?)?;
        for spec in env.getattr("REGISTERS")?.try_iter()? {
            let spec = spec?;
            let name = spec.get_item("name")?;
            if !must_match.contains(&name)? || !local_values.contains(&name)? || !remote_values.contains(&name)? {
                continue;
            }
            let (l, r) = (
                int_of(&local_values.get_item(&name)?)?,
                int_of(&remote_values.get_item(&name)?)?,
            );
            if l.eq(&r)? {
                continue;
            }
            let d = PyDict::new(py);
            d.set_item("name", &name)?;
            d.set_item("label", spec.call_method1("get", ("label", &name))?)?;
            d.set_item("local", int_of(&local_values.get_item(&name)?)?)?;
            d.set_item("remote", int_of(&remote_values.get_item(&name)?)?)?;
            out.append(d)?;
        }
        Ok(out)
    }

    /// The values that would make the remote radio agree with the local one.
    fn copy_to_remote<'py>(
        &self,
        local: &Bound<'py, PyAny>,
        remote: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let py = local.py();
        let out = PyDict::new(py);
        if !remote.is_truthy()? {
            return Ok(out);
        }
        let must_match = self.env(py).getattr("MUST_MATCH")?;
        let local_values = or_empty_dict(local.call_method1("get", ("values",))?)?;
        let remote_values = or_empty_dict(remote.call_method1("get", ("values",))?)?;
        for name in py
            .import("builtins")?
            .call_method1("sorted", (must_match,))?
            .try_iter()?
        {
            let name = name?;
            if local_values.contains(&name)?
                && remote_values.contains(&name)?
                && int_of(&local_values.get_item(&name)?)?.ne(int_of(&remote_values.get_item(&name)?)?)?
            {
                out.set_item(&name, int_of(&local_values.get_item(&name)?)?)?;
            }
        }
        Ok(out)
    }

    /// Check requested values against the radio in front of us, ordered for
    /// writing; unchanged registers are dropped.
    fn validate_writes<'py>(
        &self,
        settings: &Bound<'py, PyAny>,
        present: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyList>> {
        let py = settings.py();
        let env = self.env(py);
        if !settings.is_instance_of::<PyDict>() {
            return Err(self.error(py, PyString::new(py, "settings must be an object").into_any())?);
        }
        let read_only = env.getattr("READ_ONLY")?;
        let by_name = env.getattr("_BY_NAME")?;
        let writes = PyList::empty(py);
        for item in settings.call_method0("items")?.try_iter()? {
            let item = item?;
            let (name, raw) = (item.get_item(0)?, item.get_item(1)?);
            let key = name.str()?.call_method0("upper")?;
            let entry = present.call_method1("get", (&key,))?;
            if entry.is_none() {
                return Err(self.error(py, fmt(py, "this radio has no {} setting", (&key,))?)?);
            }
            if read_only.contains(&key)? {
                return Err(self.error(
                    py,
                    fmt(py, "{} is set by the radio firmware and cannot be written", (&key,))?,
                )?);
            }
            if raw.is_instance_of::<PyBool>() || !(raw.is_instance_of::<PyInt>() || raw.is_instance_of::<PyFloat>()) {
                return Err(self.error(py, fmt(py, "{} must be a number", (&key,))?)?);
            }
            let value = int_of(&raw)?;
            if value.ne(&raw)? {
                return Err(self.error(py, fmt(py, "{} must be a whole number", (&key,))?)?);
            }
            let spec = by_name.call_method1("get", (&key,))?;
            if !spec.is_none() {
                self.check_against_spec(&spec, &key, &value)?;
            }
            if value.ne(int_of(&entry.get_item("value")?)?)? {
                let d = PyDict::new(py);
                d.set_item("name", &key)?;
                d.set_item("register", int_of(&entry.get_item("number")?)?)?;
                d.set_item("value", &value)?;
                writes.append(d)?;
            }
        }
        self.check_band(settings, present)?;
        let key = py.import("operator")?.call_method1("itemgetter", ("register",))?;
        writes.call_method("sort", (), Some(&kw(py, &[("key", key.unbind())])?))?;
        Ok(writes)
    }

    fn check_against_spec(
        &self,
        spec: &Bound<'_, PyAny>,
        key: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = spec.py();
        let label = spec.call_method1("get", ("label", key))?;
        if spec.call_method1("get", ("kind",))?.eq("enum")? {
            let mut allowed = Vec::new();
            for option in spec.call_method1("get", ("options", PyList::empty(py)))?.try_iter()? {
                allowed.push(int_of(&option?.get_item("value")?)?);
            }
            let mut found = false;
            for a in &allowed {
                if a.eq(value)? {
                    found = true;
                    break;
                }
            }
            if !found {
                let names: Vec<String> = allowed
                    .iter()
                    .map(|v| v.str().map(|s| s.to_string_lossy().into_owned()))
                    .collect::<PyResult<_>>()?;
                return Err(self.error(
                    py,
                    fmt(
                        py,
                        "{} does not accept {}; allowed values are {}",
                        (&label, value, names.join(", ")),
                    )?,
                )?);
            }
            return Ok(());
        }
        let (low, high) = (spec.call_method1("get", ("min",))?, spec.call_method1("get", ("max",))?);
        if !low.is_none() && value.lt(&low)? {
            return Err(self.error(py, fmt(py, "{} must be at least {}", (&label, &low))?)?);
        }
        if !high.is_none() && value.gt(&high)? {
            return Err(self.error(py, fmt(py, "{} must be at most {}", (&label, &high))?)?);
        }
        Ok(())
    }

    /// Refuse a frequency band that is inverted or empty.
    fn check_band(&self, settings: &Bound<'_, PyAny>, present: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = settings.py();
        let resolve = |name: &str| -> PyResult<Option<Bound<'_, PyAny>>> {
            if settings.contains(name)? {
                return match int_of(&settings.get_item(name)?) {
                    Ok(v) => Ok(Some(v)),
                    Err(err) if caught_by(py, &err, &["TypeError", "ValueError"]) => Ok(None),
                    Err(err) => Err(err),
                };
            }
            let entry = present.call_method1("get", (name,))?;
            if entry.is_none() {
                return Ok(None);
            }
            Ok(Some(int_of(&entry.get_item("value")?)?))
        };
        let (low, high) = (resolve("MIN_FREQ")?, resolve("MAX_FREQ")?);
        let (Some(low), Some(high)) = (low, high) else {
            return Ok(());
        };
        if high.le(&low)? {
            return Err(self.error(
                py,
                PyString::new(py, "the maximum frequency must be above the minimum frequency").into_any(),
            )?);
        }
        Ok(())
    }

    /// The AT command sequence that commits *writes*.
    fn write_plan<'py>(&self, writes: &Bound<'py, PyAny>, remote: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
        let py = writes.py();
        let prefix = if remote.is_truthy()? { "RT" } else { "AT" };
        let commands = PyList::empty(py);
        for write in writes.try_iter()? {
            let write = write?;
            commands.append(fmt(
                py,
                "{}S{}={}",
                (prefix, write.get_item("register")?, write.get_item("value")?),
            )?)?;
        }
        if commands.is_empty() {
            return Ok(commands);
        }
        commands.append(format!("{prefix}&W"))?;
        commands.append(format!("{prefix}Z"))?;
        Ok(commands)
    }
}

// ------------------------------------------------------------ service ----

/// The AT dialogue over a serial port, and the port and bridge hand-over
/// around one session.
#[pyclass(name = "SikServiceEngine", module = "corvus_link.sik", frozen)]
pub struct SikServiceEngine {
    env: PyObject,
}

impl SikServiceEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn constant(&self, py: Python<'_>, name: &str) -> PyResult<f64> {
        self.env(py).getattr(name)?.extract()
    }

    fn now(&self, py: Python<'_>) -> PyResult<f64> {
        self.env(py).getattr("time")?.call_method0("monotonic")?.extract()
    }

    fn sleep(&self, py: Python<'_>, seconds: f64) -> PyResult<()> {
        self.env(py).getattr("time")?.call_method1("sleep", (seconds,))?;
        Ok(())
    }

    fn logger<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr("logger")
    }

    fn config<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr("sik_config")
    }

    fn sik_error<'py>(&self, py: Python<'py>, text: Bound<'py, PyAny>, cause: Option<&PyErr>) -> PyResult<PyErr> {
        let err = PyErr::from_value(self.env(py).getattr("SikError")?.call1((text,))?);
        if let Some(cause) = cause {
            err.set_cause(py, Some(cause.clone_ref(py)));
        }
        Ok(err)
    }

    fn is_sik_error(&self, py: Python<'_>, err: &PyErr) -> PyResult<bool> {
        Ok(err.is_instance(py, &self.env(py).getattr("SikError")?))
    }

    fn log_debug(&self, py: Python<'_>, text: &str, err: &PyErr) -> PyResult<()> {
        let kwargs = kw(py, &[("exc_info", err.value(py).clone().into_any().unbind())])?;
        self.logger(py)?.call_method("debug", (text,), Some(&kwargs))?;
        Ok(())
    }
}

#[pymethods]
impl SikServiceEngine {
    /// *env* is ``corvus.sik_service``.
    #[new]
    fn new(env: PyObject) -> Self {
        SikServiceEngine { env }
    }

    /// Read until the radio goes quiet, or until *timeout*; the end of a
    /// reply is defined by silence.
    fn read_reply<'py>(&self, session: &Bound<'py, PyAny>, timeout: f64) -> PyResult<Bound<'py, PyAny>> {
        let py = session.py();
        let port = session.getattr("_port")?;
        let deadline = self.now(py)? + timeout;
        let mut chunks: Vec<u8> = Vec::new();
        let mut any = false;
        let mut last = self.now(py)?;
        while self.now(py)? < deadline {
            let waiting = (|| -> PyResult<Bound<'py, PyAny>> {
                let waiting = match port.getattr("in_waiting") {
                    Ok(v) => v,
                    Err(err) if caught_by(py, &err, &["AttributeError"]) => 0i64.into_pyobject(py)?.into_any(),
                    Err(err) => return Err(err),
                };
                let waiting = if waiting.is_truthy()? {
                    waiting
                } else {
                    0i64.into_pyobject(py)?.into_any()
                };
                int_of(&waiting)
            })();
            let waiting = match waiting {
                Ok(w) => w,
                Err(err) if err.is_instance_of::<PyException>(py) => 0i64.into_pyobject(py)?.into_any(),
                Err(err) => return Err(err),
            };
            let mut data: Vec<u8> = Vec::new();
            if waiting.is_truthy()? {
                match port.call_method1("read", (&waiting,)) {
                    Ok(d) => {
                        if d.is_truthy()? {
                            data = crate::rtk::wire::bytes_of(&d)?;
                        }
                    }
                    Err(err) if err.is_instance_of::<PyException>(py) => {
                        let text = fmt(py, "serial read failed: {}", (err.value(py),))?;
                        return Err(self.sik_error(py, text, Some(&err))?);
                    }
                    Err(err) => return Err(err),
                }
            }
            if !data.is_empty() {
                chunks.extend(data);
                any = true;
                last = self.now(py)?;
            } else if any && (self.now(py)? - last) >= self.constant(py, "IDLE_S")? {
                break;
            } else {
                self.sleep(py, 0.02)?;
            }
        }
        let kwargs = kw(py, &[("errors", "replace".into_pyobject(py)?.into_any().unbind())])?;
        PyBytes::new(py, &chunks).call_method("decode", ("ascii",), Some(&kwargs))
    }

    /// Send one AT/RT command and return whatever came back.
    #[pyo3(signature = (session, text, timeout = None))]
    fn command<'py>(
        &self,
        session: &Bound<'py, PyAny>,
        text: &Bound<'py, PyAny>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = session.py();
        let remote = text
            .call_method0("upper")?
            .call_method1("startswith", ("RT",))?
            .is_truthy()?;
        let timeout = match timeout {
            Some(t) => t,
            None => self.constant(py, if remote { "REMOTE_TIMEOUT_S" } else { "LOCAL_TIMEOUT_S" })?,
        };
        let port = session.getattr("_port")?;
        let sent = (|| -> PyResult<()> {
            port.call_method0("reset_input_buffer")?;
            port.call_method1("write", (text.add("\r\n")?.call_method1("encode", ("ascii",))?,))?;
            port.call_method0("flush")?;
            Ok(())
        })();
        match sent {
            Err(err) if err.is_instance_of::<PyException>(py) => {
                let text = fmt(py, "serial write failed: {}", (err.value(py),))?;
                return Err(self.sik_error(py, text, Some(&err))?);
            }
            Err(err) => return Err(err),
            Ok(()) => {}
        }
        session.call_method1("_read_reply", (timeout,))
    }

    /// ``+++`` with its guard times. True when the radio answered.
    #[pyo3(signature = (session, attempts = 2))]
    fn enter_command_mode(&self, session: &Bound<'_, PyAny>, attempts: i64) -> PyResult<bool> {
        let py = session.py();
        let port = session.getattr("_port")?;
        let config = self.config(py)?;
        for attempt in 1..=attempts.max(1) {
            match port.call_method0("reset_input_buffer") {
                Err(err) if !err.is_instance_of::<PyException>(py) => return Err(err),
                _ => {}
            }
            self.sleep(py, self.constant(py, "GUARD_S")?)?;
            let sent = (|| -> PyResult<()> {
                port.call_method1("write", (PyBytes::new(py, b"+++"),))?;
                port.call_method0("flush")?;
                Ok(())
            })();
            match sent {
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    let text = fmt(py, "serial write failed: {}", (err.value(py),))?;
                    return Err(self.sik_error(py, text, Some(&err))?);
                }
                Err(err) => return Err(err),
                Ok(()) => {}
            }
            self.sleep(py, self.constant(py, "GUARD_S")?)?;
            let reply = session.call_method1("_read_reply", (self.constant(py, "LOCAL_TIMEOUT_S")?,))?;
            if reply.call_method0("upper")?.contains("OK")? {
                return Ok(true);
            }
            if config
                .call_method1("parse_version", (session.call_method1("command", ("ATI",))?,))?
                .is_truthy()?
            {
                return Ok(true);
            }
            self.logger(py)?
                .call_method1("debug", ("sik: +++ attempt %d got %r", attempt, &reply))?;
        }
        Ok(false)
    }

    /// ``ATO``: hand the port back to the telemetry stream; best effort.
    fn leave_command_mode(&self, session: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = session.py();
        let kwargs = kw(py, &[("timeout", 0.4f64.into_pyobject(py)?.into_any().unbind())])?;
        match session.call_method("command", ("ATO",), Some(&kwargs)) {
            Err(err) if err.is_instance_of::<PyException>(py) => self.log_debug(py, "sik: ATO failed", &err),
            Err(err) => Err(err),
            Ok(_) => Ok(()),
        }
    }

    /// Read one end of the link, or None when the radio did not answer.
    fn read_radio<'py>(&self, session: &Bound<'py, PyAny>, remote: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = session.py();
        let config = self.config(py)?;
        let prefix = if remote.is_truthy()? { "RT" } else { "AT" };
        let registers = config.call_method1(
            "parse_registers",
            (session.call_method1("command", (format!("{prefix}I5"),))?,),
        )?;
        if !registers.is_truthy()? {
            return Ok(py.None().into_bound(py));
        }
        let version = config.call_method1(
            "parse_version",
            (session.call_method1("command", (format!("{prefix}I"),))?,),
        )?;
        let board = config.call_method1(
            "parse_version",
            (session.call_method1("command", (format!("{prefix}I2"),))?,),
        )?;
        let kwargs = kw(py, &[("version", version.unbind()), ("board", board.unbind())])?;
        config.call_method("describe_radio", (registers,), Some(&kwargs))
    }

    /// ``ATI7``: signal and noise at both ends, or None.
    fn read_link_quality<'py>(&self, session: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = session.py();
        let reply = match session.call_method1("command", ("ATI7",)) {
            Ok(r) => r,
            Err(err) if self.is_sik_error(py, &err)? => return Ok(py.None().into_bound(py)),
            Err(err) => return Err(err),
        };
        self.config(py)?.call_method1("parse_rssi", (reply,))
    }

    /// Run a write plan, checking each command before sending the next.
    fn apply(&self, session: &Bound<'_, PyAny>, writes: &Bound<'_, PyAny>, remote: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = session.py();
        for command in self
            .config(py)?
            .call_method1("write_plan", (writes, remote))?
            .try_iter()?
        {
            let command = command?;
            let reply = session.call_method1("command", (&command,))?;
            if command
                .call_method0("upper")?
                .call_method1("endswith", ("Z",))?
                .is_truthy()?
            {
                self.sleep(py, self.constant(py, "REBOOT_S")?)?;
                continue;
            }
            if reply.call_method0("upper")?.contains("ERROR")? {
                return Err(self.sik_error(py, fmt(py, "the radio rejected {}", (&command,))?, None)?);
            }
        }
        Ok(())
    }

    /// ``AT&F``, then write and reboot: back to the firmware's defaults.
    fn factory_reset(&self, session: &Bound<'_, PyAny>, remote: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = session.py();
        let prefix = if remote.is_truthy()? { "RT" } else { "AT" };
        let reply = session.call_method1("command", (format!("{prefix}&F"),))?;
        if reply.call_method0("upper")?.contains("ERROR")? {
            return Err(self.sik_error(
                py,
                PyString::new(py, "the radio rejected the factory reset").into_any(),
                None,
            )?);
        }
        session.call_method1("command", (format!("{prefix}&W"),))?;
        session.call_method1("command", (format!("{prefix}Z"),))?;
        self.sleep(py, self.constant(py, "REBOOT_S")?)
    }

    /// Open *device* and escape into command mode, scanning bauds if needed.
    fn open<'py>(
        &self,
        service: &Bound<'py, PyAny>,
        device: &Bound<'py, PyAny>,
        baud: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyTuple>> {
        let py = service.py();
        let env = self.env(py);
        let serial = env.getattr("_serial")?;
        if serial.is_none() {
            return Err(self.sik_error(py, env.getattr("NO_SERIAL_MESSAGE")?, None)?);
        }
        let mut candidates: Vec<Bound<'py, PyAny>> = Vec::new();
        let mut wanted = vec![baud.clone()];
        for b in self.config(py)?.getattr("BAUD_CANDIDATES")?.try_iter()? {
            wanted.push(b?);
        }
        for candidate in wanted {
            if candidate.is_truthy()? {
                let n = int_of(&candidate)?;
                let mut seen = false;
                for c in &candidates {
                    if c.eq(&n)? {
                        seen = true;
                        break;
                    }
                }
                if !seen {
                    candidates.push(n);
                }
            }
        }
        let session_type = env.getattr("SikSession")?;
        let mut last_error = String::new();
        for (index, candidate) in candidates.iter().enumerate() {
            let kwargs = kw(
                py,
                &[
                    ("baudrate", candidate.clone().unbind()),
                    ("timeout", 0.1f64.into_pyobject(py)?.into_any().unbind()),
                    ("write_timeout", 2.0f64.into_pyobject(py)?.into_any().unbind()),
                ],
            )?;
            let port = match serial.call_method("Serial", (device,), Some(&kwargs)) {
                Ok(p) => p,
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    last_error = err.value(py).str()?.to_string_lossy().into_owned();
                    continue;
                }
                Err(err) => return Err(err),
            };
            let session = session_type.call1((&port,))?;
            let kwargs = kw(
                py,
                &[(
                    "attempts",
                    (if index == 0 { 2i64 } else { 1 })
                        .into_pyobject(py)?
                        .into_any()
                        .unbind(),
                )],
            )?;
            if session
                .call_method("enter_command_mode", (), Some(&kwargs))?
                .is_truthy()?
            {
                return PyTuple::new(py, [port, session, candidate.clone()]);
            }
            match port.call_method0("close") {
                Err(err) if !err.is_instance_of::<PyException>(py) => return Err(err),
                _ => {}
            }
            last_error = format!("no answer at {} baud", candidate.str()?.to_string_lossy());
        }
        let text = if last_error.is_empty() {
            fmt(py, "could not reach a radio on {}", (device,))?
        } else {
            fmt(
                py,
                "could not reach a radio on {} ({}). Check that this is the radio's port, that nothing else has it open, and that the radio has power.",
                (device, last_error),
            )?
        };
        Err(self.sik_error(py, text, None)?)
    }

    /// Take the port, run *body* in an AT session, and give the port back.
    fn run_locked<'py>(
        &self,
        service: &Bound<'py, PyAny>,
        device: &Bound<'py, PyAny>,
        baud: &Bound<'py, PyAny>,
        body: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = service.py();
        let env = self.env(py);
        let mavlink = service.getattr("mavlink")?;
        let same_device = env.getattr("_same_device")?;
        let owns_link = !mavlink.is_none()
            && same_device
                .call1((device, mavlink.call_method0("serial_device")?))?
                .is_truthy()?
            && mavlink.call_method0("is_running")?.is_truthy()?;
        let restore_conn = if owns_link {
            mavlink.call_method0("connection_string")?
        } else {
            PyString::new(py, "").into_any()
        };
        if owns_link {
            self.logger(py)?
                .call_method1("info", ("sik: stopping the MAVLink bridge to configure %s", device))?;
            match mavlink.call_method0("stop") {
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    self.log_debug(py, "sik: bridge stop raised", &err)?
                }
                Err(err) => return Err(err),
                Ok(_) => {}
            }
        }
        let mut session: Option<Bound<'py, PyAny>> = None;
        let mut port: Option<Bound<'py, PyAny>> = None;
        let outcome = (|| -> PyResult<Bound<'py, PyAny>> {
            let opened = service.call_method1("_open", (device, baud))?;
            let (p, s, reached) = (opened.get_item(0)?, opened.get_item(1)?, opened.get_item(2)?);
            port = Some(p);
            session = Some(s.clone());
            let result = body.call1((&s, &reached))?;
            result.call_method1("setdefault", ("device", device))?;
            result.call_method1("setdefault", ("baud", &reached))?;
            Ok(result)
        })();
        if let Some(s) = &session {
            s.call_method0("leave_command_mode")?;
        }
        if let Some(p) = &port {
            match p.call_method0("close") {
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    let kwargs = kw(py, &[("exc_info", err.value(py).clone().into_any().unbind())])?;
                    self.logger(py)?
                        .call_method("debug", ("sik: closing %s raised", device), Some(&kwargs))?;
                }
                Err(err) => return Err(err),
                Ok(_) => {}
            }
        }
        service.call_method1("_restart_bridge", (restore_conn,))?;
        outcome
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.sik")?;
    m.add_class::<SikConfigEngine>()?;
    m.add_class::<SikServiceEngine>()?;
    parent.add_submodule(&m)?;
    py.import("sys")?.getattr("modules")?.set_item("corvus_link.sik", &m)?;
    Ok(())
}
