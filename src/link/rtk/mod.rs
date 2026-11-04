//! RTK corrections, as ``corvus_link.rtk``: the wire formats (`wire`), the
//! settings (`config`), the serial port, NTRIP socket and transports
//! (`io`), and the base station session (`service`).

use pyo3::prelude::*;
use pyo3::types::PyModule;

pub mod config;
pub mod io;
pub mod service;
pub mod wire;

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.rtk")?;
    wire::register(&m)?;
    config::register(&m)?;
    io::register(&m)?;
    service::register(&m)?;
    parent.add_submodule(&m)?;
    py.import("sys")?.getattr("modules")?.set_item("corvus_link.rtk", &m)?;
    Ok(())
}
