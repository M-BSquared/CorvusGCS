//! The telemetry log: every raw frame the link receives, written to a
//! `.tlog` file behind one metadata header line (magic prefix + JSON).
//!
//! The receive loop never waits on the disk: a frame is appended to a
//! bounded queue that drops the oldest frame when full, and a writer thread
//! (a `CoreThread`) drains it in batches. A failed write (a full disk in
//! the field) is reported once, frames stop being taken, and stop() still
//! closes the file. One file per connection cycle, and a new one when the
//! vehicle disarms after a flight.
//!
//! The writer's file, queue and locks stay the Python objects on
//! `corvus.tlog.TlogWriter`; this is the logic over them, called through
//! the writer so a test that swaps one part keeps doing so.

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyModule};

use crate::bridge::receive::within;
use crate::bridge::thread;
use crate::protocol::host::obj;

fn is_set(o: &Bound<'_, PyAny>, name: &str) -> PyResult<bool> {
    o.getattr(name)?.call_method0("is_set")?.is_truthy()
}

/// Run `f`, swallowing an Exception (not a BaseException) as Python's
/// `except Exception: pass` would.
fn quietly(py: Python<'_>, f: impl FnOnce() -> PyResult<()>) -> PyResult<()> {
    match f() {
        Err(err) if err.is_instance_of::<PyException>(py) => Ok(()),
        other => other,
    }
}

/// The writer side of one tlog file.
#[pyclass(name = "TlogEngine", module = "corvus_link.bridge", frozen)]
pub struct TlogEngine {
    env: PyObject,
}

impl TlogEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn drain_once(&self, writer: &Bound<'_, PyAny>, batch: Vec<Bound<'_, PyAny>>) -> PyResult<()> {
        let py = writer.py();
        if batch.is_empty() {
            return Ok(());
        }
        writer.call_method0("_ensure_header")?;
        writer.call_method1("_write_batch", (PyList::new(py, batch)?,))?;
        Ok(())
    }
}

#[pymethods]
impl TlogEngine {
    /// *env* is ``corvus.tlog``.
    #[new]
    fn new(env: PyObject) -> Self {
        TlogEngine { env }
    }

    /// Start the writer thread for *writer*.
    fn start(&self, writer: &Bound<'_, PyAny>) -> PyResult<Py<thread::CoreThread>> {
        let target = writer.clone().unbind();
        thread::spawn(writer.py(), "tlog-writer", move |py| {
            target.bind(py).call_method0("_writer_loop")?;
            Ok(())
        })
    }

    /// Queue one frame; never waits on the disk, drops the oldest when full,
    /// and takes nothing once stopped or failed.
    fn write_frame(&self, writer: &Bound<'_, PyAny>, raw: &Bound<'_, PyAny>) -> PyResult<()> {
        if is_set(writer, "_stop")? || writer.getattr("_failed")?.is_truthy()? {
            return Ok(());
        }
        let cond = writer.getattr("_cond")?;
        within(&cond, || {
            writer.getattr("_buf")?.call_method1("append", (raw,))?;
            cond.call_method0("notify")?;
            Ok(())
        })
    }

    /// Drain what is queued, write the header if no frame did, and close the
    /// file; idempotent, joins the writer for at most 2 s, and a failing disk
    /// never skips the close.
    fn stop(&self, writer: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = writer.py();
        writer.getattr("_stop")?.call_method0("set")?;
        let cond = writer.getattr("_cond")?;
        within(&cond, || cond.call_method0("notify_all").map(|_| ()))?;
        let handle = writer.getattr("_thread")?;
        if handle.call_method0("is_alive")?.is_truthy()? {
            let kwargs = PyDict::new(py);
            kwargs.set_item("timeout", 2.0)?;
            handle.call_method("join", (), Some(&kwargs))?;
        }
        if let Err(err) = writer.call_method0("_ensure_header") {
            if !err.is_instance_of::<PyException>(py) {
                return Err(err);
            }
            writer.call_method1("_note_failure", ("tlog header write failed", err.value(py)))?;
        }
        within(&writer.getattr("_file_lock")?, || {
            let file = writer.getattr("_file")?;
            if !file.is_none() && !file.getattr("closed")?.is_truthy()? {
                quietly(py, || file.call_method0("flush").map(|_| ()))?;
                quietly(py, || file.call_method0("close").map(|_| ()))?;
            }
            Ok(())
        })
    }

    /// The writer thread: drain until stopped, report a failure that ends
    /// it, and flush what is written either way.
    fn writer_loop(&self, writer: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = writer.py();
        if let Err(err) = writer.call_method0("_drain_until_stopped") {
            if !err.is_instance_of::<PyException>(py) {
                return Err(err);
            }
            writer.call_method1(
                "_note_failure",
                ("tlog writer stopped after a write failure", err.value(py)),
            )?;
        }
        within(&writer.getattr("_file_lock")?, || {
            quietly(py, || writer.getattr("_file")?.call_method0("flush").map(|_| ()))
        })
    }

    /// Batches out of the queue until stopped, then whatever is left, then
    /// the header even for a file that got no frame.
    fn drain_until_stopped(&self, writer: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = writer.py();
        let cond = writer.getattr("_cond")?;
        let buf = writer.getattr("_buf")?;
        let wait = self.env(py).getattr("_DRAIN_TIMEOUT_S")?;
        while !is_set(writer, "_stop")? {
            let batch = within(&cond, || -> PyResult<Option<Vec<Bound<'_, PyAny>>>> {
                while !buf.is_truthy()? && !is_set(writer, "_stop")? {
                    let kwargs = PyDict::new(py);
                    kwargs.set_item("timeout", &wait)?;
                    cond.call_method("wait", (), Some(&kwargs))?;
                }
                if !buf.is_truthy()? && is_set(writer, "_stop")? {
                    return Ok(None);
                }
                let mut batch = Vec::new();
                while buf.is_truthy()? {
                    batch.push(buf.call_method0("popleft")?);
                }
                Ok(Some(batch))
            })?;
            let Some(batch) = batch else {
                break;
            };
            self.drain_once(writer, batch)?;
        }
        let rest = within(&cond, || -> PyResult<Vec<Bound<'_, PyAny>>> {
            let rest: Vec<Bound<'_, PyAny>> = buf.try_iter()?.collect::<PyResult<_>>()?;
            buf.call_method0("clear")?;
            Ok(rest)
        })?;
        self.drain_once(writer, rest)?;
        writer.call_method0("_ensure_header")?;
        Ok(())
    }

    /// Report the first write failure (once) and take no further frames.
    #[pyo3(signature = (writer, message, exc=None))]
    fn note_failure(&self, writer: &Bound<'_, PyAny>, message: &str, exc: Option<PyObject>) -> PyResult<()> {
        let py = writer.py();
        let already = writer.getattr("_failed")?.is_truthy()?;
        writer.setattr("_failed", true)?;
        let cond = writer.getattr("_cond")?;
        within(&cond, || writer.getattr("_buf")?.call_method0("clear").map(|_| ()))?;
        if !already {
            let kwargs = PyDict::new(py);
            kwargs.set_item("exc_info", exc.unwrap_or_else(|| obj(py, true)))?;
            self.env(py).getattr("logger")?.call_method(
                "warning",
                ("%s: %s", message, writer.getattr("_path")?),
                Some(&kwargs),
            )?;
        }
        Ok(())
    }

    /// Write the metadata header once.
    fn ensure_header(&self, writer: &Bound<'_, PyAny>) -> PyResult<()> {
        if writer.getattr("_header_written")?.is_truthy()? {
            return Ok(());
        }
        let header = writer.call_method0("_build_header")?;
        within(&writer.getattr("_file_lock")?, || {
            if writer.getattr("_header_written")?.is_truthy()? {
                return Ok(());
            }
            let file = writer.getattr("_file")?;
            file.call_method1("write", (&header,))?;
            file.call_method0("flush")?;
            writer.setattr("_header_written", true)
        })
    }

    /// The header line: product, the running version, the connection, when
    /// it started and the format.
    fn build_header<'py>(&self, writer: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = writer.py();
        let version = py.import("corvus.version")?.getattr("get_version")?.call0()?;
        let datetime = py.import("datetime")?;
        let started = datetime
            .getattr("datetime")?
            .call_method1("now", (datetime.getattr("UTC")?,))?
            .call_method0("isoformat")?;
        let meta = PyDict::new(py);
        meta.set_item("product", "Corvus GCS")?;
        meta.set_item("version", version)?;
        meta.set_item("conn", writer.getattr("_conn_str")?)?;
        meta.set_item("started_at", started)?;
        meta.set_item("format", "mavlink-raw")?;
        let body = py
            .import("json")?
            .call_method1("dumps", (meta,))?
            .call_method1("encode", ("utf-8",))?;
        self.env(py)
            .getattr("_MAGIC")?
            .add(body)?
            .add(pyo3::types::PyBytes::new(py, b"\n"))
    }

    /// Write one batch and flush, unless the file is already closed.
    fn write_batch(&self, writer: &Bound<'_, PyAny>, batch: &Bound<'_, PyAny>) -> PyResult<()> {
        within(&writer.getattr("_file_lock")?, || {
            let file = writer.getattr("_file")?;
            if file.is_none() || file.getattr("closed")?.is_truthy()? {
                return Ok(());
            }
            for raw in batch.try_iter()? {
                file.call_method1("write", (raw?,))?;
            }
            file.call_method0("flush")?;
            Ok(())
        })
    }
}

/// The bridge's side: which file, when to start and roll one, and handing
/// each frame to it and to the forwarder.
#[pyclass(name = "TlogSession", module = "corvus_link.bridge", frozen)]
pub struct TlogSession {
    env: PyObject,
}

impl TlogSession {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn debug(&self, py: Python<'_>, text: &str, err: &PyErr) -> PyResult<()> {
        crate::protocol::host::logger(py)?.call_method1("debug", (text, err.value(py)))?;
        Ok(())
    }
}

#[pymethods]
impl TlogSession {
    /// *env* is ``corvus.mavlink_bridge``.
    #[new]
    fn new(env: PyObject) -> Self {
        TlogSession { env }
    }

    /// A tlog path in *log_dir* that is not taken: the time to the
    /// microsecond, and a zero padded counter when the clock's tick is too
    /// coarse to keep two sessions apart.
    fn next_path(&self, py: Python<'_>, log_dir: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        let env = self.env(py);
        let stamp = env
            .getattr("datetime")?
            .getattr("datetime")?
            .call_method0("now")?
            .call_method1("strftime", ("%Y%m%d-%H%M%S-%f",))?
            .str()?
            .to_string();
        let ospath = env.getattr("os")?.getattr("path")?;
        let mut path = ospath.call_method1("join", (log_dir, format!("{stamp}.tlog")))?;
        let mut n = 1;
        while ospath.call_method1("exists", (&path,))?.is_truthy()? {
            path = ospath.call_method1("join", (log_dir, format!("{stamp}_{n:02}.tlog")))?;
            n += 1;
        }
        Ok(path.unbind())
    }

    /// Open a fresh tlog for this session; only while the bridge runs, and
    /// a log directory that cannot be used disables the tlog, never the link.
    fn start(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        if !is_set(bridge, "_running")? {
            return Ok(());
        }
        bridge.call_method0("_stop_tlog")?;
        let env = self.env(py);
        let opened = (|| -> PyResult<()> {
            let log_dir = env.getattr("default_log_dir")?.call0()?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("exist_ok", true)?;
            env.getattr("os")?.call_method("makedirs", (&log_dir,), Some(&kwargs))?;
            let path = bridge.call_method1("_next_tlog_path", (&log_dir,))?;
            let writer = env.getattr("TlogWriter")?.call1((path,))?;
            bridge.setattr("_tlog", &writer)?;
            writer.call_method1("set_conn", (bridge.getattr("_conn_str")?,))?;
            Ok(())
        })();
        match opened {
            Err(err) if err.is_instance_of::<PyException>(py) => {
                crate::protocol::host::logger(py)?.call_method1("warning", ("tlog disabled: %s", err.value(py)))?;
                bridge.setattr("_tlog", py.None())
            }
            other => other,
        }
    }

    /// Start the next file when the vehicle disarms after a flight, and ask
    /// for the version again so the new file carries its own.
    fn roll_on_disarm(&self, bridge: &Bound<'_, PyAny>, armed: bool) -> PyResult<()> {
        let was = bridge.getattr("_tlog_armed")?;
        bridge.setattr("_tlog_armed", armed)?;
        if !was.is_truthy()? || armed || bridge.getattr("_tlog")?.is_none() {
            return Ok(());
        }
        bridge.call_method0("_start_tlog")?;
        if !bridge.getattr("_tlog")?.is_none() {
            bridge.call_method0("_request_version")?;
        }
        Ok(())
    }

    /// Stop and close the current tlog; idempotent.
    fn stop(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let tlog = bridge.getattr("_tlog")?;
        if tlog.is_none() {
            return Ok(());
        }
        bridge.setattr("_tlog", py.None())?;
        match tlog.call_method0("stop") {
            Err(err) if err.is_instance_of::<PyException>(py) => self.debug(py, "tlog stop failed: %s", &err),
            Err(err) => Err(err),
            Ok(_) => Ok(()),
        }
    }

    /// Hand one frame's bytes to the tlog and the forwarder, whichever are
    /// attached; neither ever breaks the link.
    fn write(&self, bridge: &Bound<'_, PyAny>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = bridge.py();
        let tlog = bridge.getattr("_tlog")?;
        let sink = bridge.getattr("_frame_sink")?;
        if tlog.is_none() && sink.is_none() {
            return Ok(());
        }
        let raw = (|| -> PyResult<Option<Bound<'_, PyAny>>> {
            let get_buf = match msg.getattr("get_msgbuf") {
                Ok(f) => f,
                Err(err) if err.is_instance_of::<pyo3::exceptions::PyAttributeError>(py) => return Ok(None),
                Err(err) => return Err(err),
            };
            Ok(Some(get_buf.call0()?))
        })();
        let raw = match raw {
            Ok(Some(raw)) => raw,
            Ok(None) => return Ok(()),
            Err(err) if err.is_instance_of::<PyException>(py) => {
                return self.debug(py, "frame capture failed: %s", &err)
            }
            Err(err) => return Err(err),
        };
        if !tlog.is_none() {
            match tlog.call_method1("write_frame", (&raw,)) {
                Err(err) if err.is_instance_of::<PyException>(py) => self.debug(py, "tlog write failed: %s", &err)?,
                Err(err) => return Err(err),
                Ok(_) => {}
            }
        }
        if !sink.is_none() {
            match sink.call1((&raw,)) {
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    self.debug(py, "frame forward failed: %s", &err)?
                }
                Err(err) => return Err(err),
                Ok(_) => {}
            }
        }
        Ok(())
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<TlogEngine>()?;
    m.add_class::<TlogSession>()
}
