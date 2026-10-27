//! Threads the bridge's link runs on, owned by Rust.
//!
//! A `CoreThread` is a native thread that holds a Python thread state for
//! its whole life and calls into Python with the GIL, releasing it wherever
//! Python does (a blocking read, a sleep, an event wait). To the bridge it
//! looks like the `threading.Thread` it replaces: `name`, `daemon`,
//! `is_alive()` and `join(timeout)`, and `join` waits with the GIL released
//! so the thread it waits for can finish.
//!
//! A native thread must not be inside the interpreter while it shuts down:
//! a thread that wakes up then is ended by CPython in the middle of its Rust
//! frames. So every thread is registered here, and an `atexit` hook that
//! runs after the application's own teardown sets `shutting_down()`, which
//! the link loops check, and joins whatever is still running. A thread is
//! never started once that flag is set.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyModule;

use crate::lock;

static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// Set once the interpreter is on its way out; every link loop stops.
pub fn shutting_down() -> bool {
    SHUTTING_DOWN.load(Ordering::SeqCst)
}

struct Finished {
    done: Mutex<bool>,
    signal: Condvar,
}

impl Finished {
    fn mark(&self) {
        *lock(&self.done) = true;
        self.signal.notify_all();
    }

    /// Wait until done or *timeout* passes; true when done.
    fn wait(&self, timeout: Option<Duration>) -> bool {
        let mut done = lock(&self.done);
        let deadline = timeout.map(|t| Instant::now() + t);
        while !*done {
            match deadline {
                None => done = self.signal.wait(done).unwrap_or_else(|e| e.into_inner()),
                Some(at) => {
                    let now = Instant::now();
                    if now >= at {
                        return false;
                    }
                    done = self
                        .signal
                        .wait_timeout(done, at - now)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
        }
        true
    }
}

static LIVE: Mutex<Vec<Arc<Finished>>> = Mutex::new(Vec::new());

thread_local! {
    static CURRENT: RefCell<Option<Py<CoreThread>>> = const { RefCell::new(None) };
}

/// A native thread running one of the bridge's link loops.
#[pyclass(name = "CoreThread", module = "corvus_link.bridge", frozen)]
pub struct CoreThread {
    name: String,
    finished: Arc<Finished>,
}

#[pymethods]
impl CoreThread {
    #[getter]
    fn name(&self) -> String {
        self.name.clone()
    }

    /// Never keeps the process alive, like the daemon threads it replaces.
    #[getter]
    fn daemon(&self) -> bool {
        true
    }

    fn is_alive(&self) -> bool {
        !*lock(&self.finished.done)
    }

    /// Wait for the thread to finish, at most *timeout* seconds; the GIL is
    /// released while waiting.
    #[pyo3(signature = (timeout=None))]
    fn join(&self, py: Python<'_>, timeout: Option<f64>) {
        let finished = self.finished.clone();
        let timeout = timeout.map(|t| Duration::from_secs_f64(t.max(0.0)));
        py.allow_threads(move || finished.wait(timeout));
    }

    fn __repr__(&self) -> String {
        let state = if self.is_alive() { "started daemon" } else { "stopped" };
        format!("<CoreThread({}, {state})>", self.name)
    }
}

/// Drop the `threading` entry Python made for this native thread. Python
/// keeps a dummy entry for every foreign thread that touched `threading`,
/// and it outlives the thread: `threading.enumerate()` would go on listing a
/// stopped link thread by name, which is what a leak check looks for.
fn forget_dummy(py: Python<'_>) {
    let forgotten = (|| -> PyResult<()> {
        let threading = py.import("threading")?;
        let ident = threading.call_method0("get_ident")?;
        let guard = threading.getattr("_active_limbo_lock")?;
        guard.call_method0("acquire")?;
        let popped = threading.getattr("_active")?.call_method1("pop", (ident, py.None()));
        guard.call_method0("release")?;
        popped.map(|_| ())
    })();
    let _ = forgotten;
}

/// Start *name* running *body*. The handle is returned only once the thread
/// exists, so whoever joins it never meets a thread that has not started.
pub fn spawn(
    py: Python<'_>,
    name: &str,
    body: impl FnOnce(Python<'_>) -> PyResult<()> + Send + 'static,
) -> PyResult<Py<CoreThread>> {
    let finished = Arc::new(Finished {
        done: Mutex::new(false),
        signal: Condvar::new(),
    });
    let handle = Py::new(
        py,
        CoreThread {
            name: name.to_string(),
            finished: finished.clone(),
        },
    )?;
    if shutting_down() {
        finished.mark();
        return Ok(handle);
    }
    {
        let mut live = lock(&LIVE);
        live.retain(|t| !*lock(&t.done));
        live.push(finished.clone());
    }
    let own = handle.clone_ref(py);
    let thread_name = name.to_string();
    let spawned = std::thread::Builder::new().name(thread_name.clone()).spawn(move || {
        if !shutting_down() {
            Python::with_gil(|py| {
                CURRENT.with(|c| *c.borrow_mut() = Some(own.clone_ref(py)));
                // Log records name the thread as the threading module would.
                if let Ok(current) = py.import("threading").and_then(|t| t.call_method0("current_thread")) {
                    let _ = current.setattr("name", &thread_name);
                }
                if let Err(err) = body(py) {
                    if err.is_instance_of::<PyException>(py) {
                        err.display(py);
                    }
                }
                CURRENT.with(|c| *c.borrow_mut() = None);
                forget_dummy(py);
                drop(own);
            });
        }
        finished.mark();
    });
    if let Err(err) = spawned {
        handle.get().finished.mark();
        return Err(PyException::new_err(format!("cannot start thread {name}: {err}")));
    }
    Ok(handle)
}

/// Start *target(*args)* on a new `CoreThread` named *name*; for Python
/// workers (the tlog writer, the parameter upload and watchdog) that should
/// live under the same shutdown guard as the link loops.
#[pyfunction]
#[pyo3(signature = (name, target, *args))]
pub fn start_thread(
    py: Python<'_>,
    name: &str,
    target: PyObject,
    args: Py<pyo3::types::PyTuple>,
) -> PyResult<Py<CoreThread>> {
    spawn(py, name, move |py| {
        target.bind(py).call1(args.bind(py))?;
        Ok(())
    })
}

/// The thread a loop is running on: its `CoreThread`, or the Python thread
/// that called it directly.
pub fn current(py: Python<'_>) -> PyResult<PyObject> {
    if let Some(handle) = CURRENT.with(|c| c.borrow().as_ref().map(|h| h.clone_ref(py))) {
        return Ok(handle.into_any());
    }
    Ok(py.import("threading")?.call_method0("current_thread")?.unbind())
}

/// The `atexit` hook: stop the link loops and wait for their threads, so
/// none is inside the interpreter when it finalises.
#[pyfunction]
#[pyo3(signature = (timeout=5.0))]
pub fn join_at_exit(py: Python<'_>, timeout: f64) -> usize {
    SHUTTING_DOWN.store(true, Ordering::SeqCst);
    let live: Vec<Arc<Finished>> = lock(&LIVE).clone();
    let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
    py.allow_threads(move || {
        live.iter()
            .filter(|t| !t.wait(Some(deadline.saturating_duration_since(Instant::now()))))
            .count()
    })
}

pub fn register(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<CoreThread>()?;
    m.add_function(wrap_pyfunction!(join_at_exit, m)?)?;
    m.add_function(wrap_pyfunction!(start_thread, m)?)?;
    py.import("atexit")?
        .call_method1("register", (m.getattr("join_at_exit")?,))?;
    Ok(())
}
