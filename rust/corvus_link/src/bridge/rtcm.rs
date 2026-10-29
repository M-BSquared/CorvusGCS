//! RTCM corrections onto the link as GPS_RTCM_DATA: fire and forget under
//! the send lock only, never raising, a correction too long for four
//! fragments dropped whole and counted.

use pyo3::exceptions::{PyException, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyModule};

use crate::bridge::receive::within;
use crate::rtk::wire::FRAGMENT_BYTES;

/// The bridge's RTCM injection and its counters.
#[pyclass(name = "RtcmEngine", module = "corvus_link.bridge", frozen)]
pub struct RtcmEngine {
    env: PyObject,
}

impl RtcmEngine {
    fn count(&self, bridge: &Bound<'_, PyAny>, field: &str, by: usize) -> PyResult<()> {
        within(&bridge.getattr("_rtcm_lock")?, || {
            bridge.setattr(field, bridge.getattr(field)?.add(by)?)
        })
    }
}

#[pymethods]
impl RtcmEngine {
    /// *env* is ``corvus.mavlink_bridge``.
    #[new]
    fn new(env: PyObject) -> Self {
        RtcmEngine { env }
    }

    /// Put one RTCM 3 correction on the link; True when every fragment was
    /// written.
    fn inject_rtcm(&self, bridge: &Bound<'_, PyAny>, frame: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = bridge.py();
        let env = self.env.bind(py);
        if !frame.is_truthy()? {
            return Ok(false);
        }
        if !bridge.call_method0("_connection_ready")?.is_truthy()? {
            self.count(bridge, "_rtcm_dropped", 1)?;
            return Ok(false);
        }
        let sequence = within(&bridge.getattr("_rtcm_lock")?, || {
            let sequence = bridge.getattr("_rtcm_sequence")?;
            bridge.setattr("_rtcm_sequence", sequence.add(1)?.bitand(0x1F)?)?;
            Ok(sequence)
        })?;
        let parts = env.getattr("rtk")?.call_method1("fragments", (frame, sequence))?;
        if !parts.is_truthy()? {
            self.count(bridge, "_rtcm_dropped", 1)?;
            env.getattr("logger")?.call_method1(
                "warning",
                (
                    "RTCM message of %d bytes is too long for GPS_RTCM_DATA; dropped",
                    frame.len()?,
                ),
            )?;
            return Ok(false);
        }
        let sent = within(&bridge.getattr("_send_lock")?, || {
            let conn = bridge.getattr("_conn")?;
            if conn.is_none() {
                return Err(PyRuntimeError::new_err("link closed"));
            }
            let mav = conn.getattr("mav")?;
            for part in parts.try_iter()? {
                let part = part?;
                let flags = part.get_item(0)?;
                let chunk = part.get_item(1)?;
                let n = chunk.len()?;
                let data = PyList::empty(py);
                for byte in chunk.try_iter()? {
                    data.append(byte?)?;
                }
                for _ in n..FRAGMENT_BYTES {
                    data.append(0)?;
                }
                mav.call_method1("gps_rtcm_data_send", (flags, n, data))?;
            }
            Ok(())
        });
        if let Err(err) = sent {
            if !err.is_instance_of::<PyException>(py) {
                return Err(err);
            }
            self.count(bridge, "_rtcm_dropped", 1)?;
            env.getattr("logger")?
                .call_method1("debug", ("RTCM injection failed: %s", err.value(py)))?;
            return Ok(false);
        }
        let now = env.getattr("time")?.call_method0("monotonic")?;
        let length = frame.len()?;
        within(&bridge.getattr("_rtcm_lock")?, || {
            bridge.setattr("_rtcm_bytes", bridge.getattr("_rtcm_bytes")?.add(length)?)?;
            bridge.setattr("_rtcm_messages", bridge.getattr("_rtcm_messages")?.add(1)?)?;
            bridge.setattr("_rtcm_last_send", &now)
        })?;
        Ok(true)
    }

    /// What has reached the aircraft, for the RTK page.
    fn rtcm_stats<'py>(&self, bridge: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
        let py = bridge.py();
        let env = self.env.bind(py);
        within(&bridge.getattr("_rtcm_lock")?, || {
            let last = bridge.getattr("_rtcm_last_send")?;
            let d = PyDict::new(py);
            d.set_item("bytes", bridge.getattr("_rtcm_bytes")?)?;
            d.set_item("messages", bridge.getattr("_rtcm_messages")?)?;
            d.set_item("dropped", bridge.getattr("_rtcm_dropped")?)?;
            let age = if last.is_truthy()? {
                env.getattr("time")?.call_method0("monotonic")?.sub(&last)?
            } else {
                py.None().into_bound(py)
            };
            d.set_item("age", age)?;
            Ok(d)
        })
    }

    /// Reset the injection counters when the correction source changes.
    fn forget_rtcm_session(&self, bridge: &Bound<'_, PyAny>) -> PyResult<()> {
        within(&bridge.getattr("_rtcm_lock")?, || {
            bridge.setattr("_rtcm_bytes", 0)?;
            bridge.setattr("_rtcm_messages", 0)?;
            bridge.setattr("_rtcm_dropped", 0)?;
            bridge.setattr("_rtcm_last_send", 0.0)
        })
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<RtcmEngine>()
}
