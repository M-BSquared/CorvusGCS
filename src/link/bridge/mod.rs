//! The bridge's own link work, moved from Python: the receive path and the
//! command transaction (`receive`), its own requests and signing
//! (`requests`), the vehicle list's tracker (`fleet`), the second
//! station forwarder (`forward`), the auto-connect policy and watcher
//! (`autoconnect`), RTCM injection (`rtcm`), what the vehicle reports in words
//! (`status`), the telemetry log (`tlog`), the connection cycle (`cycle`) and
//! the threads it runs on (`thread`), as ``corvus_link.bridge``.

use pyo3::prelude::*;
use pyo3::types::PyModule;

pub mod autoconnect;
pub mod cycle;
pub mod fleet;
pub mod forward;
pub mod receive;
pub mod requests;
pub mod rtcm;
pub mod status;
pub mod thread;
pub mod tlog;

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.bridge")?;
    receive::register(py, &m)?;
    thread::register(py, &m)?;
    autoconnect::register(&m)?;
    cycle::register(&m)?;
    fleet::register(&m)?;
    forward::register(&m)?;
    status::register(&m)?;
    tlog::register(&m)?;
    requests::register(&m)?;
    rtcm::register(&m)?;
    parent.add_submodule(&m)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("corvus_link.bridge", &m)?;
    Ok(())
}
