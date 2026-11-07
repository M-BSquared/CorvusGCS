//! The PX4 NuttShell (NSH) over SERIAL_CONTROL, device SHELL.
//!
//! Two readers, and whoever wrote to the shell last gets its answer: the
//! MAVLink console, which wants whole lines (reassembled from the 70 byte
//! chunks, carriage returns dropped, backspaces applied), and a terminal,
//! which wants the text exactly as the shell printed it, kept in a backlog
//! for a terminal that attaches late.
//!
//! The critical sections run under the bridge's `_send_lock` and
//! `_shell_lock`, as they did in Python, so a reply is never handled halfway
//! through a send. The tunables are read from `corvus.mavlink_shell`.

use std::sync::Mutex;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyModule, PyString};

use crate::lock;
use crate::protocol::host::{console, logger, py_int, set_error};
use crate::wire::defs::{self, constant_or};
use crate::wire::dialect;

pub use crate::plain::shell_text::{assemble_lines, trim_backlog, Utf8Stream, CHUNK};

struct State {
    active: bool,
    raw: bool,
    buffer: String,
    decoder: Utf8Stream,
    backlog: String,
    subs: Vec<PyObject>,
}

/// The NSH shell of one bridge.
#[pyclass(name = "ShellEngine", module = "corvus_link.shell", frozen)]
pub struct ShellEngine {
    env: PyObject,
    st: Mutex<State>,
}

/// Run `f` holding a Python lock of the bridge, released however `f` ends.
fn holding<T>(bridge: &Bound<'_, PyAny>, name: &str, f: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    let lock = bridge.getattr(name)?;
    lock.call_method0("acquire")?;
    let result = f();
    lock.call_method0("release")?;
    result
}

fn flags_stream() -> i64 {
    constant_or("SERIAL_CONTROL_FLAG_RESPOND", 2)
        | constant_or("SERIAL_CONTROL_FLAG_EXCLUSIVE", 4)
        | constant_or("SERIAL_CONTROL_FLAG_MULTI", 16)
}

/// `text.encode("utf-8", errors="replace")`, by Python, for a str that may
/// hold a lone surrogate.
fn utf8_of(text: &Bound<'_, PyString>) -> PyResult<Vec<u8>> {
    let kwargs = PyDict::new(text.py());
    kwargs.set_item("errors", "replace")?;
    Ok(text
        .call_method("encode", ("utf-8",), Some(&kwargs))?
        .downcast_into::<PyBytes>()?
        .as_bytes()
        .to_vec())
}

impl ShellEngine {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.st)
    }

    fn tunable(&self, py: Python<'_>, name: &str) -> PyResult<usize> {
        self.env.bind(py).getattr(name)?.extract()
    }

    fn reset_locked(&self) {
        let mut st = self.state();
        st.active = false;
        st.buffer.clear();
        st.decoder.reset();
    }

    /// SERIAL_CONTROL to the shell, with the target fields when this view of
    /// the dialect has them.
    fn serial_control(
        bridge: &Bound<'_, PyAny>,
        conn: &Bound<'_, PyAny>,
        flags: i64,
        count: usize,
        data: &[u8],
    ) -> PyResult<()> {
        let py = bridge.py();
        let mut padded = data.to_vec();
        padded.resize(CHUNK, 0);
        let mut args: Vec<PyObject> = vec![
            constant_or("SERIAL_CONTROL_DEV_SHELL", 10)
                .into_pyobject(py)?
                .into_any()
                .unbind(),
            flags.into_pyobject(py)?.into_any().unbind(),
            0i64.into_pyobject(py)?.into_any().unbind(),
            0i64.into_pyobject(py)?.into_any().unbind(),
            count.into_pyobject(py)?.into_any().unbind(),
            PyBytes::new(py, &padded).into_any().unbind(),
        ];
        let targeted = defs::message_by_name("SERIAL_CONTROL", dialect::current_wire())
            .map(|m| {
                let fields = m.fields(dialect::current_wire());
                fields.iter().any(|f| f.name == "target_system") && fields.iter().any(|f| f.name == "target_component")
            })
            .unwrap_or(false);
        if targeted {
            args.push(bridge.getattr("_target_system")?.unbind());
            args.push(bridge.getattr("_target_component")?.unbind());
        }
        conn.getattr("mav")?
            .call_method1("serial_control_send", pyo3::types::PyTuple::new(py, args)?)?;
        Ok(())
    }

    fn send_chunks(bridge: &Bound<'_, PyAny>, conn: &Bound<'_, PyAny>, payload: &[u8]) -> PyResult<()> {
        for chunk in payload.chunks(CHUNK) {
            Self::serial_control(bridge, conn, flags_stream(), chunk.len(), chunk)?;
        }
        Ok(())
    }

    fn label(bridge: &Bound<'_, PyAny>) -> PyResult<String> {
        Ok(bridge.getattr("_dialect")?.getattr("label")?.str()?.to_string())
    }

    fn supports_shell(bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        bridge.getattr("_dialect")?.getattr("supports_shell")?.is_truthy()
    }
}

#[pymethods]
impl ShellEngine {
    /// *env* holds the tunables (``corvus.mavlink_shell``).
    #[new]
    fn new(env: PyObject) -> Self {
        ShellEngine {
            env,
            st: Mutex::new(State {
                active: false,
                raw: false,
                buffer: String::new(),
                decoder: Utf8Stream::default(),
                backlog: String::new(),
                subs: Vec::new(),
            }),
        }
    }

    #[getter]
    fn active(&self) -> bool {
        self.state().active
    }
    #[setter]
    fn set_active(&self, value: bool) {
        self.state().active = value;
    }
    #[getter]
    fn raw(&self) -> bool {
        self.state().raw
    }
    #[setter]
    fn set_raw(&self, value: bool) {
        self.state().raw = value;
    }
    #[getter]
    fn buffer(&self) -> String {
        self.state().buffer.clone()
    }
    #[setter]
    fn set_buffer(&self, value: String) {
        self.state().buffer = value;
    }
    #[getter]
    fn backlog(&self) -> String {
        self.state().backlog.clone()
    }
    #[setter]
    fn set_backlog(&self, value: String) {
        self.state().backlog = value;
    }

    /// Forget the current shell session (active flag, half line, decoder).
    fn reset(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        holding(bridge, "_shell_lock", || {
            self.reset_locked();
            Ok(())
        })
    }

    fn send_serial_control(
        &self,
        bridge: &Bound<'_, PyAny>,
        conn: &Bound<'_, PyAny>,
        flags: i64,
        count: usize,
        data: Vec<u8>,
    ) -> PyResult<()> {
        Self::serial_control(bridge, conn, flags, count, &data)
    }

    /// A line for the console's shell: sent with a newline, the answer comes
    /// back as console lines named SHELL.
    fn send_command(&self, bridge: &Bound<'_, PyAny>, text: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let Ok(text) = text.downcast::<PyString>() else {
            set_error(bridge, "shell command must be text")?;
            return Ok(false);
        };
        if !Self::supports_shell(bridge)? {
            set_error(
                bridge,
                &format!(
                    "{} has no MAVLink shell. The console is a PX4 feature (NSH over SERIAL_CONTROL)",
                    Self::label(bridge)?
                ),
            )?;
            return Ok(false);
        }
        holding(bridge, "_send_lock", || {
            let conn = bridge.getattr("_conn")?;
            if !conn.is(&bridge.getattr("_conn")?) || !bridge.call_method0("_connection_ready")?.is_truthy()? {
                return bridge.call_method1("_command_failure", ("Shell", -2))?.is_truthy();
            }
            let line = if text.call_method1("endswith", ("\n",))?.is_truthy()? {
                text.clone()
            } else {
                text.as_any().add("\n")?.downcast_into::<PyString>()?
            };
            let payload = utf8_of(&line)?;
            holding(bridge, "_shell_lock", || {
                self.reset_locked();
                {
                    let mut st = self.state();
                    st.active = true;
                    st.raw = false;
                }
                if let Err(err) = Self::send_chunks(bridge, &conn, &payload) {
                    self.reset_locked();
                    logger(py)?.call_method1("error", ("shell command send failed: %s", err.value(py)))?;
                    set_error(bridge, &format!("shell send failed: {}", err.value(py)))?;
                    return Ok(false);
                }
                Ok(true)
            })
        })
    }

    /// The terminal's half: `data` exactly as typed, the output from now on
    /// to the subscribers and the backlog.
    fn write(&self, bridge: &Bound<'_, PyAny>, data: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        set_error(bridge, "")?;
        let text = match data.downcast::<PyString>() {
            Ok(t) if t.len()? > 0 => t.clone(),
            _ => {
                set_error(bridge, "nothing to send to the shell")?;
                return Ok(false);
            }
        };
        let max = self.tunable(py, "SHELL_WRITE_MAX_CHARS")?;
        let length = text.len()?;
        if length > max {
            set_error(
                bridge,
                &format!("too much at once for the shell ({length} characters, at most {max})"),
            )?;
            return Ok(false);
        }
        if !Self::supports_shell(bridge)? {
            set_error(
                bridge,
                &format!(
                    "{} has no NuttX shell. It is a PX4 feature (NSH over SERIAL_CONTROL)",
                    Self::label(bridge)?
                ),
            )?;
            return Ok(false);
        }
        holding(bridge, "_send_lock", || {
            let conn = bridge.getattr("_conn")?;
            if conn.is_none() || !bridge.call_method0("_connection_ready")?.is_truthy()? {
                set_error(bridge, "not connected")?;
                return Ok(false);
            }
            let payload = utf8_of(&text)?;
            holding(bridge, "_shell_lock", || {
                {
                    let mut st = self.state();
                    if !st.raw {
                        st.buffer.clear();
                        st.decoder.reset();
                    }
                    st.raw = true;
                    st.active = true;
                }
                if let Err(err) = Self::send_chunks(bridge, &conn, &payload) {
                    logger(py)?.call_method1("error", ("shell write failed: %s", err.value(py)))?;
                    set_error(bridge, &format!("shell send failed: {}", err.value(py)))?;
                    return Ok(false);
                }
                Ok(true)
            })
        })
    }

    /// Hear the terminal's output; returns what it printed so far.
    fn add_sub(&self, bridge: &Bound<'_, PyAny>, fn_: PyObject) -> PyResult<String> {
        holding(bridge, "_shell_lock", || {
            let mut st = self.state();
            st.subs.push(fn_);
            Ok(st.backlog.clone())
        })
    }

    fn remove_sub(&self, bridge: &Bound<'_, PyAny>, fn_: &Bound<'_, PyAny>) -> PyResult<()> {
        holding(bridge, "_shell_lock", || {
            let subs: Vec<PyObject> = {
                let st = self.state();
                st.subs.iter().map(|s| s.clone_ref(bridge.py())).collect()
            };
            for (i, sub) in subs.iter().enumerate() {
                if sub.bind(bridge.py()).eq(fn_)? {
                    self.state().subs.remove(i);
                    break;
                }
            }
            Ok(())
        })
    }

    /// Release exclusive shell mode on the autopilot; best effort.
    fn stop(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        holding(bridge, "_send_lock", || {
            let conn = bridge.getattr("_conn")?;
            holding(bridge, "_shell_lock", || {
                self.reset_locked();
                Ok(())
            })?;
            holding(bridge, "_shell_lock", || {
                let mut st = self.state();
                st.raw = false;
                st.backlog.clear();
                Ok(())
            })?;
            if conn.is_none() || !conn.is(&bridge.getattr("_conn")?) {
                return Ok(());
            }
            let _ = Self::serial_control(bridge, &conn, 0, 0, &[0u8; CHUNK]);
            Ok(())
        })
    }

    /// One SERIAL_CONTROL reply from the shell, to whoever owns it. Runs on
    /// the receive thread; never raises.
    fn handle(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let handled = (|| -> PyResult<()> {
            let device = match msg.getattr("device") {
                Ok(d) => py_int(&d)?,
                Err(_) => -1,
            };
            if device != constant_or("SERIAL_CONTROL_DEV_SHELL", 10) {
                return Ok(());
            }
            if !self.state().active || !bridge.call_method1("_ack_is_for_us", (msg,))?.is_truthy()? {
                return Ok(());
            }
            let line_max = self.tunable(py, "SHELL_LINE_MAX_CHARS")?;
            let backlog_max = self.tunable(py, "SHELL_BACKLOG_CHARS")?;
            let (lines, raw_subs, text) = holding(
                bridge,
                "_shell_lock",
                || -> PyResult<(Vec<String>, Vec<PyObject>, String)> {
                    if !self.state().active {
                        return Ok((vec![], vec![], String::new()));
                    }
                    let count = match msg.getattr("count") {
                        Ok(c) => py_int(&c)?,
                        Err(_) => 0,
                    };
                    if count <= 0 || count > CHUNK as i64 {
                        return Ok((vec![], vec![], String::new()));
                    }
                    let raw_data = py.get_type::<PyBytes>().call1((msg.getattr("data")?,))?;
                    let raw_data = raw_data.downcast::<PyBytes>()?.as_bytes();
                    if count as usize > raw_data.len() {
                        return Ok((vec![], vec![], String::new()));
                    }
                    let mut st = self.state();
                    let text = st.decoder.decode(&raw_data[..count as usize]);
                    if st.raw {
                        let mut subs = Vec::new();
                        if !text.is_empty() {
                            let joined = format!("{}{}", st.backlog, text);
                            st.backlog = trim_backlog(&joined, backlog_max);
                            subs = st.subs.iter().map(|s| s.clone_ref(py)).collect();
                        }
                        Ok((vec![], subs, text))
                    } else {
                        let lines = assemble_lines(&mut st.buffer, &text, line_max);
                        Ok((lines, vec![], text))
                    }
                },
            )?;
            if !raw_subs.is_empty() {
                let entry = PyDict::new(py);
                entry.set_item("text", &text)?;
                for sub in raw_subs {
                    let _ = sub.bind(py).call1((&entry,));
                }
            }
            for line in lines {
                console(bridge, "SHELL", &line, "info")?;
            }
            Ok(())
        })();
        if let Err(err) = handled {
            logger(py)?.call_method1("debug", ("SERIAL_CONTROL handling failed: %s", err.value(py)))?;
        }
        Ok(())
    }

    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        visit.call(&self.env)?;
        if let Ok(st) = self.st.try_lock() {
            for sub in &st.subs {
                visit.call(sub)?;
            }
        }
        Ok(())
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.shell")?;
    m.add_class::<ShellEngine>()?;
    parent.add_submodule(&m)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("corvus_link.shell", &m)?;
    Ok(())
}
