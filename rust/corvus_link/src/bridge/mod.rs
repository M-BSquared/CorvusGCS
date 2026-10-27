//! The bridge's own link work, moved from Python: the receive path and the
//! command transaction (`receive`), its own requests and signing
//! (`requests`), what the vehicle reports in words
//! (`status`), the telemetry log (`tlog`), the connection cycle (`cycle`) and
//! the threads it runs on (`thread`), as ``corvus_link.bridge``.

use pyo3::prelude::*;
use pyo3::types::PyModule;

pub mod cycle;
pub mod receive;
pub mod requests;
pub mod status;
pub mod thread;
pub mod tlog;

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.bridge")?;
    receive::register(py, &m)?;
    thread::register(py, &m)?;
    cycle::register(&m)?;
    status::register(&m)?;
    tlog::register(&m)?;
    requests::register(&m)?;
    parent.add_submodule(&m)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("corvus_link.bridge", &m)?;
    Ok(())
}
