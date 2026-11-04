//! On-board log transfer, the request side: list the vehicle's logs, ask for
//! a chunk of one, erase them all, and end the session. The LOG_ENTRY and
//! LOG_DATA replies go to the sink the bridge's dispatcher hands them to.

use pyo3::prelude::*;
use pyo3::types::PyModule;

use crate::protocol::host::{armed, console, py_int, set_error, truthy, with_send_lock};

/// `self._conn.mav.<method>(target, *args)` under the send lock, as the
/// Python did it: a link that is gone fails the call like any other error.
fn send(bridge: &Bound<'_, PyAny>, method: &str, args: Vec<PyObject>) -> PyResult<()> {
    let py = bridge.py();
    with_send_lock(bridge, || {
        let mut all: Vec<PyObject> = vec![
            bridge.getattr("_target_system")?.unbind(),
            bridge.getattr("_target_component")?.unbind(),
        ];
        all.extend(args);
        bridge
            .getattr("_conn")?
            .getattr("mav")?
            .call_method1(method, pyo3::types::PyTuple::new(py, all)?)?;
        Ok(())
    })
}

fn int_arg(value: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    Ok(crate::protocol::host::obj(value.py(), py_int(value)?))
}

/// The log protocol of one bridge. Every method expects the caller to hold
/// the bridge's operation lock.
#[pyclass(name = "LogEngine", module = "corvus_link.logs", frozen)]
pub struct LogEngine;

#[pymethods]
impl LogEngine {
    #[new]
    fn new() -> Self {
        LogEngine
    }

    /// LOG_REQUEST_LIST: the vehicle answers with one LOG_ENTRY per log.
    #[pyo3(signature = (bridge, start, end))]
    fn request_list(
        &self,
        bridge: &Bound<'_, PyAny>,
        start: &Bound<'_, PyAny>,
        end: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        if !truthy(bridge, "_connection_ready")? {
            set_error(bridge, "not connected")?;
            return Ok(false);
        }
        let sent = (|| send(bridge, "log_request_list_send", vec![int_arg(start)?, int_arg(end)?]))();
        if let Err(err) = sent {
            set_error(bridge, &format!("log list request failed: {}", err.value(bridge.py())))?;
            return Ok(false);
        }
        Ok(true)
    }

    /// LOG_REQUEST_DATA for one chunk; answered with LOG_DATA.
    fn request_data(
        &self,
        bridge: &Bound<'_, PyAny>,
        log_id: &Bound<'_, PyAny>,
        offset: &Bound<'_, PyAny>,
        count: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        if !truthy(bridge, "_connection_ready")? {
            set_error(bridge, "not connected")?;
            return Ok(false);
        }
        let sent = (|| {
            send(
                bridge,
                "log_request_data_send",
                vec![int_arg(log_id)?, int_arg(offset)?, int_arg(count)?],
            )
        })();
        if let Err(err) = sent {
            set_error(bridge, &format!("log data request failed: {}", err.value(bridge.py())))?;
            return Ok(false);
        }
        Ok(true)
    }

    /// LOG_ERASE: every log on the vehicle. Refused while armed.
    fn erase(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        set_error(bridge, "")?;
        if !truthy(bridge, "_connection_ready")? {
            set_error(bridge, "not connected")?;
            return Ok(false);
        }
        if armed(bridge)? {
            set_error(bridge, "cannot erase logs while armed")?;
            return Ok(false);
        }
        if let Err(err) = send(bridge, "log_erase_send", vec![]) {
            set_error(bridge, &format!("log erase failed: {}", err.value(bridge.py())))?;
            return Ok(false);
        }
        console(bridge, "LOGS", "erasing all on-board logs", "warning")?;
        Ok(true)
    }

    /// LOG_REQUEST_END, without which PX4 keeps the session and blocks the
    /// next flight's logging. Best effort.
    fn request_end(&self, bridge: &Bound<'_, PyAny>) -> PyResult<bool> {
        if !truthy(bridge, "_connection_ready")? {
            return Ok(false);
        }
        Ok(send(bridge, "log_request_end_send", vec![]).is_ok())
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.logs")?;
    m.add_class::<LogEngine>()?;
    parent.add_submodule(&m)?;
    py.import("sys")?.getattr("modules")?.set_item("corvus_link.logs", &m)?;
    Ok(())
}
