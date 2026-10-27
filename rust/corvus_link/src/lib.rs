//! corvus_link: MAVLink for Corvus GCS.
//!
//! A Rust implementation of everything Corvus used pymavlink for: the
//! message set of the `all` dialect, MAVLink 1 and 2 framing, CRC_EXTRA,
//! signing, the byte-stream parser, the serial, UDP, TCP and other links,
//! and the per-system state and mode tables of `mavutil`. The Python
//! interface is pymavlink's, so `corvus_link.mavutil` drops in where
//! `pymavlink.mavutil` was.

use std::sync::{Mutex, MutexGuard};

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};

mod bridge;
mod link;
mod protocol;
mod wire;

use link::{connection, transport};
use protocol::params;
use wire::{crc, defs, dialect, frame, message, modes};

/// Lock a mutex, recovering it if a panic poisoned it: a panic surfaces in
/// Python as an exception, and the link must stay usable afterwards.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[pyfunction]
fn mode_mapping_bynumber(py: Python<'_>, mav_type: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let Ok(value) = mav_type.extract::<i64>() else {
        return Ok(py.None());
    };
    match modes::ap_mode_table(value) {
        Some(table) => Ok(modes::table_dict(py, table)?.into_any().unbind()),
        None => Ok(py.None()),
    }
}

#[pyfunction]
fn mode_mapping_byname(py: Python<'_>, mav_type: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    match mav_type.extract::<i64>() {
        Ok(value) => connection::mode_mapping_byname(py, value),
        Err(_) => Ok(py.None()),
    }
}

#[pyfunction]
fn mode_string_v10(msg: &Bound<'_, PyAny>) -> PyResult<String> {
    modes::mode_string_v10(msg)
}

#[pyfunction]
#[pyo3(signature = (base_mode, custom_mode))]
fn interpret_px4_mode(base_mode: &Bound<'_, PyAny>, custom_mode: i64) -> &'static str {
    let _ = base_mode;
    modes::interpret_px4_mode(custom_mode)
}

#[pyfunction]
fn mode_string_px4(main_state: i64) -> &'static str {
    modes::MAINSTATE_MAPPING_PX4
        .iter()
        .find(|(n, _)| *n == main_state)
        .map(|(_, s)| *s)
        .unwrap_or("Unknown")
}

#[pyfunction]
fn mode_string_apm(mode_number: i64) -> String {
    modes::MODE_MAPPING_APM
        .iter()
        .find(|(n, _)| *n == mode_number)
        .map(|(_, s)| s.to_string())
        .unwrap_or_else(|| format!("Mode({mode_number})"))
}

#[pyfunction]
fn mode_string_acm(mode_number: i64) -> String {
    modes::MODE_MAPPING_ACM
        .iter()
        .find(|(n, _)| *n == mode_number)
        .map(|(_, s)| s.to_string())
        .unwrap_or_else(|| format!("Mode({mode_number})"))
}

/// Accepted for compatibility: corvus_link carries the `all` dialect only,
/// which is the one Corvus pins.
#[pyfunction]
#[pyo3(signature = (dialect, with_type_annotations=None))]
fn set_dialect(dialect: &str, with_type_annotations: Option<bool>) -> PyResult<()> {
    let _ = with_type_annotations;
    if dialect != "all" {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "corvus_link carries the 'all' dialect only, not '{dialect}'"
        )));
    }
    Ok(())
}

#[pyfunction]
fn mavlink10() -> bool {
    true
}

#[pyfunction]
fn mavlink20() -> bool {
    dialect::current_wire() == defs::Wire::V2
}

fn build_mavutil(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.mavutil")?;
    m.add("mavlink", dialect::namespace(py, dialect::current_wire())?)?;
    m.add("current_dialect", "all")?;
    m.add("UDP_MAX_PACKET_LEN", transport::UDP_MAX_PACKET_LEN)?;
    m.add_function(wrap_pyfunction!(connection::mavlink_connection, &m)?)?;
    m.add_function(wrap_pyfunction!(mode_mapping_bynumber, &m)?)?;
    m.add_function(wrap_pyfunction!(mode_mapping_byname, &m)?)?;
    m.add_function(wrap_pyfunction!(mode_string_v10, &m)?)?;
    m.add_function(wrap_pyfunction!(interpret_px4_mode, &m)?)?;
    m.add_function(wrap_pyfunction!(mode_string_px4, &m)?)?;
    m.add_function(wrap_pyfunction!(mode_string_apm, &m)?)?;
    m.add_function(wrap_pyfunction!(mode_string_acm, &m)?)?;
    m.add_function(wrap_pyfunction!(set_dialect, &m)?)?;
    m.add_function(wrap_pyfunction!(mavlink10, &m)?)?;
    m.add_function(wrap_pyfunction!(mavlink20, &m)?)?;
    m.add("mavfile", py.get_type::<connection::Connection>())?;
    m.add("mavfile_state", py.get_type::<connection::MavfileState>())?;
    m.add("param_state", py.get_type::<connection::ParamState>())?;
    m.add("px4_map", modes::px4_map(py)?)?;
    m.add("mode_mapping_apm", modes::table_dict(py, modes::MODE_MAPPING_APM)?)?;
    m.add("mode_mapping_acm", modes::table_dict(py, modes::MODE_MAPPING_ACM)?)?;
    m.add("mode_mapping_rover", modes::table_dict(py, modes::MODE_MAPPING_ROVER)?)?;
    m.add(
        "mode_mapping_tracker",
        modes::table_dict(py, modes::MODE_MAPPING_TRACKER)?,
    )?;
    m.add("mode_mapping_sub", modes::table_dict(py, modes::MODE_MAPPING_SUB)?)?;
    m.add("mode_mapping_blimp", modes::table_dict(py, modes::MODE_MAPPING_BLIMP)?)?;
    m.add(
        "mainstate_mapping_px4",
        modes::table_dict(py, modes::MAINSTATE_MAPPING_PX4)?,
    )?;
    let ap = PyDict::new(py);
    for t in modes::ap_mode_types() {
        ap.set_item(t, modes::table_dict(py, modes::ap_mode_table(t).unwrap())?)?;
    }
    m.add("AP_MAV_TYPE_MODE_MAP_DEFAULT", ap.copy()?)?;
    m.add("AP_MAV_TYPE_MODE_MAP", ap)?;
    for (name, value) in [
        ("PX4_CUSTOM_MAIN_MODE_MANUAL", modes::PX4_CUSTOM_MAIN_MODE_MANUAL),
        ("PX4_CUSTOM_MAIN_MODE_ALTCTL", modes::PX4_CUSTOM_MAIN_MODE_ALTCTL),
        ("PX4_CUSTOM_MAIN_MODE_POSCTL", modes::PX4_CUSTOM_MAIN_MODE_POSCTL),
        ("PX4_CUSTOM_MAIN_MODE_AUTO", modes::PX4_CUSTOM_MAIN_MODE_AUTO),
        ("PX4_CUSTOM_MAIN_MODE_ACRO", modes::PX4_CUSTOM_MAIN_MODE_ACRO),
        ("PX4_CUSTOM_MAIN_MODE_OFFBOARD", modes::PX4_CUSTOM_MAIN_MODE_OFFBOARD),
        (
            "PX4_CUSTOM_MAIN_MODE_STABILIZED",
            modes::PX4_CUSTOM_MAIN_MODE_STABILIZED,
        ),
        ("PX4_CUSTOM_MAIN_MODE_RATTITUDE", modes::PX4_CUSTOM_MAIN_MODE_RATTITUDE),
        ("PX4_CUSTOM_SUB_MODE_OFFBOARD", modes::PX4_CUSTOM_SUB_MODE_OFFBOARD),
        ("PX4_CUSTOM_SUB_MODE_AUTO_READY", modes::PX4_CUSTOM_SUB_MODE_AUTO_READY),
        (
            "PX4_CUSTOM_SUB_MODE_AUTO_TAKEOFF",
            modes::PX4_CUSTOM_SUB_MODE_AUTO_TAKEOFF,
        ),
        (
            "PX4_CUSTOM_SUB_MODE_AUTO_LOITER",
            modes::PX4_CUSTOM_SUB_MODE_AUTO_LOITER,
        ),
        (
            "PX4_CUSTOM_SUB_MODE_AUTO_MISSION",
            modes::PX4_CUSTOM_SUB_MODE_AUTO_MISSION,
        ),
        ("PX4_CUSTOM_SUB_MODE_AUTO_RTL", modes::PX4_CUSTOM_SUB_MODE_AUTO_RTL),
        ("PX4_CUSTOM_SUB_MODE_AUTO_LAND", modes::PX4_CUSTOM_SUB_MODE_AUTO_LAND),
        ("PX4_CUSTOM_SUB_MODE_AUTO_RTGS", modes::PX4_CUSTOM_SUB_MODE_AUTO_RTGS),
        (
            "PX4_CUSTOM_SUB_MODE_AUTO_FOLLOW_TARGET",
            modes::PX4_CUSTOM_SUB_MODE_AUTO_FOLLOW_TARGET,
        ),
    ] {
        m.add(name, value)?;
    }
    parent.add_submodule(&m)?;
    // `import corvus_link.mavutil` and `from corvus_link import mavutil`
    // both find it.
    py.import("sys")?
        .getattr("modules")?
        .set_item("corvus_link.mavutil", &m)?;

    // When a link moves to MAVLink 2, `mavutil.mavlink` follows, as it does
    // when pymavlink reloads its dialect.
    let module = m.clone().unbind();
    let hook = pyo3::types::PyCFunction::new_closure(py, None, None, move |args, _kwargs| -> PyResult<()> {
        let py = args.py();
        module
            .bind(py)
            .setattr("mavlink", dialect::namespace(py, defs::Wire::V2)?)
    })?;
    dialect::set_switch_hook(hook.into_any().unbind());
    Ok(())
}

#[pymodule]
fn corvus_link(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    dialect::init_wire_from_env();
    m.add("MAVError", py.get_type::<frame::MAVError>())?;
    m.add_class::<crc::X25Crc>()?;
    m.add_class::<message::Message>()?;
    m.add_class::<message::PyHeader>()?;
    m.add_class::<frame::Mavlink>()?;
    m.add_class::<frame::Signing>()?;
    m.add_class::<connection::Connection>()?;
    m.add("dialect_v1", dialect::namespace(py, defs::Wire::V1)?)?;
    m.add("dialect_v2", dialect::namespace(py, defs::Wire::V2)?)?;
    build_mavutil(py, m)?;
    params::register(py, m)?;
    protocol::missions::register(py, m)?;
    protocol::ftp::register(py, m)?;
    protocol::logs::register(py, m)?;
    protocol::shell::register(py, m)?;
    protocol::setup::register(py, m)?;
    protocol::remote_id::register(py, m)?;
    protocol::telemetry::register(py, m)?;
    protocol::flight::register(py, m)?;
    bridge::register(py, m)?;
    Ok(())
}
