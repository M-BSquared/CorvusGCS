//! The RTK base station session: find a source of RTCM corrections, make a
//! u-blox receiver a base if it is one, and keep its corrections on the
//! link, on one `CoreThread`.
//!
//! The service's state stays on the Python object (``corvus.rtk_service.
//! RtkService``) under its own lock, where `status()` reads it from HTTP
//! threads; every step is called through that object, so a step a caller
//! replaces is the one that runs.

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyModule, PyString, PyTuple};

use crate::bridge::receive::within;
use crate::bridge::thread;
use crate::rtk::io::{ntrip_request, read_ntrip_header};
use crate::rtk::wire::{UBX_CLASS_MON, UBX_CLASS_NAV, UBX_ID_MON_VER, UBX_ID_NAV_SVIN};

const STATE_OFF: &str = "off";
const STATE_SEARCHING: &str = "searching";
const STATE_CONNECTING: &str = "connecting";
const STATE_SURVEYING: &str = "surveying";
const STATE_ACTIVE: &str = "active";
const STATE_ERROR: &str = "error";

fn kw<'py>(py: Python<'py>, pairs: &[(&str, PyObject)]) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    for (k, v) in pairs {
        d.set_item(*k, v)?;
    }
    Ok(d)
}

fn s<'py>(py: Python<'py>, text: &str) -> Bound<'py, PyAny> {
    PyString::new(py, text).into_any()
}

fn none(py: Python<'_>) -> Bound<'_, PyAny> {
    py.None().into_bound(py)
}

/// The RTK session, run against a ``corvus.rtk_service.RtkService``.
#[pyclass(name = "RtkEngine", module = "corvus_link.rtk", frozen)]
pub struct RtkEngine {
    env: PyObject,
}

impl RtkEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn rtk<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr("rtk")
    }

    fn constant(&self, py: Python<'_>, name: &str) -> PyResult<f64> {
        self.env(py).getattr(name)?.extract()
    }

    fn logger<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr("logger")
    }

    fn now(&self, py: Python<'_>) -> PyResult<f64> {
        self.env(py).getattr("time")?.call_method0("monotonic")?.extract()
    }

    fn sleep(&self, py: Python<'_>, seconds: f64) -> PyResult<()> {
        self.env(py).getattr("time")?.call_method1("sleep", (seconds,))?;
        Ok(())
    }

    fn stopped(&self, svc: &Bound<'_, PyAny>) -> PyResult<bool> {
        Ok(thread::shutting_down() || svc.getattr("_stop")?.call_method0("is_set")?.is_truthy()?)
    }

    fn restarting(&self, svc: &Bound<'_, PyAny>) -> PyResult<bool> {
        svc.getattr("_restart")?.call_method0("is_set")?.is_truthy()
    }

    fn is_rtk_error(&self, py: Python<'_>, err: &PyErr) -> PyResult<bool> {
        Ok(err.is_instance(py, &self.env(py).getattr("RtkError")?))
    }

    fn rtk_error(&self, py: Python<'_>, text: String) -> PyResult<PyErr> {
        Ok(PyErr::from_value(self.env(py).getattr("RtkError")?.call1((text,))?))
    }

    fn locked<T>(&self, svc: &Bound<'_, PyAny>, f: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
        within(&svc.getattr("_lock")?, f)
    }

    fn wait(&self, svc: &Bound<'_, PyAny>, seconds: f64) -> PyResult<()> {
        svc.getattr("_stop")?.call_method1("wait", (seconds,))?;
        Ok(())
    }

    fn log_exception(&self, py: Python<'_>, text: &str, err: &PyErr) -> PyResult<()> {
        let kwargs = kw(py, &[("exc_info", err.value(py).clone().into_any().unbind())])?;
        self.logger(py)?.call_method("error", (text,), Some(&kwargs))?;
        Ok(())
    }

    fn state_is_error(&self, svc: &Bound<'_, PyAny>) -> PyResult<bool> {
        svc.getattr("_state")?.eq(STATE_ERROR)
    }

    fn copy_or_none<'py>(&self, py: Python<'py>, value: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        if value.is_truthy()? {
            py.get_type::<PyDict>().call1((value,))
        } else {
            Ok(none(py))
        }
    }
}

#[pymethods]
impl RtkEngine {
    /// *env* is ``corvus.rtk_service``.
    #[new]
    fn new(env: PyObject) -> Self {
        RtkEngine { env }
    }

    // --------------------------------------------------------- lifecycle

    /// Start the session thread. Idempotent.
    fn start(&self, svc: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        let current = svc.getattr("_thread")?;
        if !current.is_none() && current.call_method0("is_alive")?.is_truthy()? {
            return Ok(());
        }
        svc.getattr("_stop")?.call_method0("clear")?;
        svc.getattr("_restart")?.call_method0("clear")?;
        let target = svc.clone().unbind();
        let handle = thread::spawn(py, "rtk-base", move |py| {
            target.bind(py).call_method0("_loop")?;
            Ok(())
        })?;
        svc.setattr("_thread", handle)?;
        let intent = svc.call_method0("_describe_intent")?;
        self.logger(py)?.call_method1("info", ("RTK service started (%s)", intent))?;
        Ok(())
    }

    /// Stop the session thread and release the port or socket; the session
    /// takes a configured receiver out of base mode on its own way out.
    fn stop(&self, svc: &Bound<'_, PyAny>, timeout: f64) -> PyResult<()> {
        let py = svc.py();
        svc.getattr("_stop")?.call_method0("set")?;
        svc.getattr("_restart")?.call_method0("set")?;
        let thread = svc.getattr("_thread")?;
        if !thread.is_none() && thread.call_method0("is_alive")?.is_truthy()? {
            thread.call_method("join", (), Some(&kw(py, &[("timeout", timeout.into_pyobject(py)?.into_any().unbind())])?))?;
            if thread.call_method0("is_alive")?.is_truthy()? {
                let transport = svc.getattr("_transport")?;
                if !transport.is_none() {
                    transport.call_method0("close")?;
                }
                thread.call_method("join", (), Some(&kw(py, &[("timeout", 1.0f64.into_pyobject(py)?.into_any().unbind())])?))?;
            }
            if thread.call_method0("is_alive")?.is_truthy()? {
                self.logger(py)?.call_method1("warning", ("RTK session thread did not stop within %.1fs", timeout))?;
            }
        }
        svc.setattr("_thread", py.None())?;
        svc.call_method1("_publish", (STATE_OFF, "RTK corrections are not running"))?;
        Ok(())
    }

    // ---------------------------------------------------------- settings

    fn settings<'py>(&self, svc: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = svc.py();
        let rtk = self.rtk(py)?;
        self.locked(svc, || rtk.call_method1("settings", (svc.getattr("_settings")?,)))
    }

    /// Replace the settings and restart the session against them.
    fn apply_settings<'py>(&self, svc: &Bound<'py, PyAny>, raw: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = svc.py();
        let resolved = self.rtk(py)?.call_method1("settings", (raw,))?;
        self.locked(svc, || {
            svc.setattr("_settings", &resolved)?;
            svc.setattr("_receiver", py.None())?;
            svc.setattr("_survey", py.None())?;
            svc.setattr("_warning", "")?;
            svc.setattr("_error", "")
        })?;
        svc.call_method0("_reset_counters")?;
        svc.getattr("_restart")?.call_method0("set")?;
        if resolved.call_method1("get", ("enabled",))?.is_truthy()?
            && !svc.call_method0("is_alive")?.is_truthy()?
            && !svc.getattr("_stop")?.call_method0("is_set")?.is_truthy()?
        {
            svc.call_method0("start")?;
        }
        Ok(resolved)
    }

    /// Start the survey again from zero.
    fn restart_survey(&self, svc: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        self.locked(svc, || {
            svc.setattr("_survey", py.None())?;
            svc.setattr("_warning", "")
        })?;
        svc.getattr("_restart")?.call_method0("set")?;
        Ok(())
    }

    // ------------------------------------------------------------ status

    /// Everything the RTK page shows, in one read.
    fn status<'py>(&self, svc: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
        let py = svc.py();
        let rtk = self.rtk(py)?;
        let settings = svc.call_method0("settings")?;
        let fields = [
            "_state", "_message", "_error", "_device", "_baud", "_receiver", "_survey", "_warning", "_frames",
            "_frame_bytes", "_crc_errors", "_last_frame_at", "_started_at", "_messages_seen",
        ];
        let held: Vec<Bound<'py, PyAny>> = self.locked(svc, || {
            let mut out = Vec::new();
            for name in fields {
                let v = svc.getattr(name)?;
                out.push(match name {
                    "_receiver" | "_survey" => self.copy_or_none(py, v)?,
                    "_messages_seen" => py.get_type::<PyDict>().call1((v,))?,
                    _ => v,
                });
            }
            Ok(out)
        })?;
        let [state, message, error, device, baud, receiver, survey, warning, frames, frame_bytes, crc_errors, last_frame_at, started_at, messages_seen]: [Bound<'py, PyAny>; 14] =
            held.try_into().map_err(|_| PyException::new_err("status fields"))?;

        let injected = match svc.getattr("_mavlink").and_then(|m| m.call_method0("rtcm_stats")) {
            Ok(v) => v,
            Err(err) if err.is_instance_of::<PyException>(py) => {
                let d = PyDict::new(py);
                d.set_item("bytes", 0)?;
                d.set_item("messages", 0)?;
                d.set_item("dropped", 0)?;
                d.set_item("age", py.None())?;
                d.into_any()
            }
            Err(err) => return Err(err),
        };
        let snapshot = match svc.getattr("_store").and_then(|m| m.call_method0("get_snapshot")) {
            Ok(v) => v,
            Err(err) if err.is_instance_of::<PyException>(py) => PyDict::new(py).into_any(),
            Err(err) => return Err(err),
        };
        let progress = rtk.call_method1(
            "survey_progress",
            (&survey, settings.get_item("survey_duration")?, settings.get_item("survey_accuracy")?),
        )?;
        let out = PyDict::new(py);
        out.set_item("enabled", settings.call_method1("get", ("enabled",))?.is_truthy()?)?;
        out.set_item("state", state)?;
        out.set_item("message", message)?;
        out.set_item("error", error)?;
        out.set_item("warning", warning)?;
        out.set_item("source", settings.call_method1("get", ("source", "usb"))?)?;
        out.set_item("mode", settings.call_method1("get", ("mode", "survey"))?)?;
        out.set_item("device", device)?;
        out.set_item("baud", baud)?;
        out.set_item("receiver_label", rtk.call_method1("receiver_label", (&receiver,))?)?;
        out.set_item("receiver", receiver)?;
        out.set_item("survey", &survey)?;
        out.set_item("survey_progress", progress)?;
        let target = PyDict::new(py);
        target.set_item("accuracy", settings.get_item("survey_accuracy")?)?;
        target.set_item("duration", settings.get_item("survey_duration")?)?;
        out.set_item("survey_target", target)?;
        out.set_item("frames", frames)?;
        out.set_item("frame_bytes", frame_bytes)?;
        out.set_item("crc_errors", crc_errors)?;
        let age = |since: &Bound<'py, PyAny>| -> PyResult<Bound<'py, PyAny>> {
            if since.is_truthy()? {
                self.env(py).getattr("time")?.call_method0("monotonic")?.sub(since)
            } else {
                Ok(none(py))
            }
        };
        out.set_item("source_age", age(&last_frame_at)?)?;
        out.set_item("uptime", age(&started_at)?)?;
        out.set_item("messages", messages_seen)?;
        out.set_item("injected", injected)?;
        out.set_item("link_ready", svc.call_method0("_link_ready")?)?;
        let vehicle = PyDict::new(py);
        vehicle.set_item("fix", snapshot.call_method1("get", ("gps_fix", ""))?)?;
        vehicle.set_item("satellites", snapshot.call_method1("get", ("gps_satellites", 0))?)?;
        vehicle.set_item("hdop", snapshot.call_method1("get", ("gps_hdop", 0))?)?;
        out.set_item("vehicle", vehicle)?;
        out.set_item("ports", svc.call_method0("_candidate_ports")?)?;
        out.set_item("settings", rtk.call_method1("public_settings", (&settings,))?)?;
        out.set_item("defaults", rtk.call_method1("public_settings", (rtk.call_method0("defaults")?,))?)?;
        // Same key order as the Python: the label after the receiver.
        let ordered = PyDict::new(py);
        for key in [
            "enabled", "state", "message", "error", "warning", "source", "mode", "device", "baud", "receiver",
            "receiver_label", "survey", "survey_progress", "survey_target", "frames", "frame_bytes", "crc_errors",
            "source_age", "uptime", "messages", "injected", "link_ready", "vehicle", "ports", "settings", "defaults",
        ] {
            ordered.set_item(key, out.get_item(key)?)?;
        }
        Ok(ordered)
    }

    // ---------------------------------------------------- session thread

    /// Open a source, run it until it ends, and start over.
    fn run(&self, svc: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        while !self.stopped(svc)? {
            let settings = svc.call_method0("settings")?;
            if !settings.call_method1("get", ("enabled",))?.is_truthy()? {
                svc.call_method1("_publish", (STATE_OFF, "RTK corrections are switched off"))?;
                self.wait(svc, self.constant(py, "SEARCH_INTERVAL_S")?)?;
                continue;
            }
            svc.getattr("_restart")?.call_method0("clear")?;
            let mut transport = none(py);
            match svc.call_method1("_acquire", (&settings,)) {
                Ok(t) => transport = t,
                Err(err) if self.is_rtk_error(py, &err)? => {
                    svc.call_method1("_fail", (err.value(py).str()?,))?;
                }
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    self.log_exception(py, "RTK source could not be opened", &err)?;
                    svc.call_method1("_fail", (format!("could not open the correction source: {}", err.value(py).str()?),))?;
                }
                Err(err) => return Err(err),
            }
            if transport.is_none() {
                let pause = if self.state_is_error(svc)? { "RETRY_INTERVAL_S" } else { "SEARCH_INTERVAL_S" };
                self.wait(svc, self.constant(py, pause)?)?;
                continue;
            }
            svc.setattr("_transport", &transport)?;
            let receiver = self.locked(svc, || self.copy_or_none(py, svc.getattr("_receiver")?))?;
            let outcome = match svc.call_method1("_run_session", (&transport, &settings, &receiver)) {
                Ok(_) => Ok(()),
                Err(err) if self.is_rtk_error(py, &err)? => svc.call_method1("_fail", (err.value(py).str()?,)).map(|_| ()),
                Err(err) if err.is_instance_of::<PyException>(py) => self
                    .log_exception(py, "RTK session failed", &err)
                    .and_then(|_| svc.call_method1("_fail", (format!("the correction session failed: {}", err.value(py).str()?),)).map(|_| ())),
                Err(err) => Err(err),
            };
            svc.call_method1("_release", (&transport, &settings, &receiver))?;
            svc.setattr("_transport", py.None())?;
            outcome?;
            if !self.stopped(svc)? {
                let pause = if self.state_is_error(svc)? { self.constant(py, "RETRY_INTERVAL_S")? } else { 0.5 };
                self.wait(svc, pause)?;
            }
        }
        Ok(())
    }

    /// Open whichever source the settings name, or None to retry.
    fn acquire<'py>(&self, svc: &Bound<'py, PyAny>, settings: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        if settings.call_method1("get", ("source",))?.eq("ntrip")? {
            return svc.call_method1("_acquire_ntrip", (settings,));
        }
        svc.call_method1("_acquire_serial", (settings,))
    }

    // ------------------------------------------------------------ serial

    fn acquire_serial<'py>(&self, svc: &Bound<'py, PyAny>, settings: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = svc.py();
        let pinned = settings.call_method1("get", ("device",))?;
        let pinned = if pinned.is_truthy()? { pinned } else { s(py, "") };
        let pinned = pinned.str()?.call_method0("strip")?;
        let candidates = svc.call_method0("_candidate_ports")?;
        let device = if pinned.is_truthy()? {
            if svc.call_method0("_link_device")?.eq(&pinned)? {
                return Err(self.rtk_error(
                    py,
                    "that port is the vehicle's telemetry link. Pick another, or let Corvus find the base station itself".into(),
                )?);
            }
            pinned
        } else {
            if !candidates.is_truthy()? {
                svc.call_method1(
                    "_publish",
                    (STATE_SEARCHING, "Looking for a base station. Plug an RTK GNSS receiver into this computer"),
                )?;
                return Ok(none(py));
            }
            candidates.get_item(0)?.get_item("device")?
        };
        svc.call_method1("_publish", (STATE_CONNECTING, format!("Opening {}", device.str()?)))?;
        let bauds: Vec<Bound<'py, PyAny>> = if settings.call_method1("get", ("baud",))?.is_truthy()? {
            vec![py.get_type::<pyo3::types::PyInt>().call1((settings.get_item("baud")?,))?]
        } else {
            self.rtk(py)?.getattr("BAUD_CANDIDATES")?.try_iter()?.collect::<PyResult<Vec<_>>>()?
        };
        let silent = "the receiver did not answer";
        let mut last_error = String::new();
        let transport_type = self.env(py).getattr("SerialTransport")?;
        for baud in &bauds {
            let port = match svc.call_method1("_open_port", (&device, baud)) {
                Ok(p) => p,
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    let text = err.value(py).str()?.to_string();
                    last_error = if text.is_empty() { err.get_type(py).name()?.to_string() } else { text };
                    continue;
                }
                Err(err) => return Err(err),
            };
            let transport = transport_type.call1((port, &device, baud))?;
            let probe = svc.call_method1("_probe", (&transport,))?;
            let kind = probe.get_item(0)?;
            if !kind.is_none() {
                let version = probe.get_item(1)?;
                self.locked(svc, || {
                    svc.setattr("_device", &device)?;
                    svc.setattr("_baud", baud)?;
                    svc.setattr("_receiver", &version)
                })?;
                return Ok(transport);
            }
            transport.call_method0("close")?;
            last_error = silent.into();
        }
        if !last_error.is_empty() && last_error != silent {
            return Err(self.rtk_error(py, format!("could not open {}: {last_error}", device.str()?))?);
        }
        let first = bauds.first().cloned().ok_or_else(|| PyException::new_err("no baud rate to try"))?;
        let port = svc.call_method1("_open_port", (&device, &first))?;
        self.locked(svc, || {
            svc.setattr("_device", &device)?;
            svc.setattr("_baud", &first)?;
            svc.setattr("_receiver", py.None())
        })?;
        transport_type.call1((port, &device, &first))
    }

    /// What is on this port at this baud rate: `("ubx", version)` for a
    /// u-blox receiver, `("rtcm", None)` for a base already streaming.
    fn probe<'py>(&self, svc: &Bound<'py, PyAny>, transport: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
        let py = svc.py();
        let nothing = || PyTuple::new(py, [none(py), none(py)]);
        let rtk = self.rtk(py)?;
        let poll = rtk.call_method1("poll_frame", (UBX_CLASS_MON, UBX_ID_MON_VER))?;
        if let Err(err) = transport.call_method1("write", (poll,)) {
            if self.is_rtk_error(py, &err)? {
                return nothing();
            }
            return Err(err);
        }
        let ubx = rtk.getattr("UbxParser")?.call0()?;
        let framer = rtk.getattr("RtcmFramer")?.call0()?;
        let deadline = self.now(py)? + self.constant(py, "IDENTIFY_TIMEOUT_S")?;
        while self.now(py)? < deadline && !self.stopped(svc)? {
            let chunk = match transport.call_method0("read") {
                Ok(c) => c,
                Err(err) if self.is_rtk_error(py, &err)? => return nothing(),
                Err(err) => return Err(err),
            };
            if !chunk.is_truthy()? {
                self.sleep(py, 0.02)?;
                continue;
            }
            for message in ubx.call_method1("feed", (&chunk,))?.try_iter()? {
                let (class, id, payload): (Bound<'py, PyAny>, Bound<'py, PyAny>, Bound<'py, PyAny>) = message?.extract()?;
                if class.eq(UBX_CLASS_MON)? && id.eq(UBX_ID_MON_VER)? {
                    let version = rtk.call_method1("parse_mon_ver", (payload,))?;
                    let label = rtk.call_method1("receiver_label", (&version,))?;
                    let label = if label.is_truthy()? { label } else { s(py, "unknown") };
                    self.logger(py)?.call_method1(
                        "info",
                        ("RTK receiver on %s at %d: %s", transport.getattr("device")?, transport.getattr("baud")?, label),
                    )?;
                    return PyTuple::new(py, [s(py, "ubx"), version]);
                }
            }
            if framer.call_method1("feed", (&chunk,))?.is_truthy()? {
                self.logger(py)?.call_method1(
                    "info",
                    ("RTK corrections already streaming from %s at %d", transport.getattr("device")?, transport.getattr("baud")?),
                )?;
                return PyTuple::new(py, [s(py, "rtcm"), none(py)]);
            }
        }
        nothing()
    }

    // ------------------------------------------------------------- NTRIP

    /// Open a caster stream, or raise with the reason it could not be.
    fn acquire_ntrip<'py>(&self, svc: &Bound<'py, PyAny>, settings: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = svc.py();
        let rtk = self.rtk(py)?;
        let ntrip = settings.call_method1("get", ("ntrip",))?;
        let ntrip = if ntrip.is_truthy()? { ntrip } else { PyDict::new(py).into_any() };
        let problem = rtk.call_method1("ntrip_problem", (&ntrip,))?;
        if problem.is_truthy()? {
            return Err(self.rtk_error(py, problem.str()?.to_string())?);
        }
        let host = ntrip.get_item("host")?.str()?.call_method0("strip")?;
        let port = py.get_type::<pyo3::types::PyInt>().call1((ntrip.get_item("port")?,))?;
        let mountpoint = ntrip.get_item("mountpoint")?.str()?.call_method0("strip")?.call_method1("lstrip", ("/",))?;
        let label = PyString::new(py, "{}:{}/{}").call_method1("format", (&host, &port, &mountpoint))?;
        svc.call_method1("_publish", (STATE_CONNECTING, PyString::new(py, "Connecting to {}").call_method1("format", (&label,))?))?;
        let sock = svc.call_method1("_open_socket", (&host, &port, self.constant(py, "NTRIP_CONNECT_TIMEOUT_S")?))?;
        let transport = self.env(py).getattr("NtripTransport")?.call1((&sock, &label))?;
        let text_or_blank = |key: &str| -> PyResult<Bound<'py, PyAny>> {
            let v = ntrip.call_method1("get", (key,))?;
            if v.is_truthy()? { Ok(v.str()?.into_any()) } else { Ok(s(py, "")) }
        };
        let exchange = (|| -> PyResult<Bound<'py, PyAny>> {
            let request = self.env(py).call_method1(
                "_ntrip_request",
                (&host, &port, &mountpoint, text_or_blank("username")?, text_or_blank("password")?),
            )?;
            sock.call_method1("sendall", (request,))?;
            self.env(py).call_method1("_read_ntrip_header", (&sock,))
        })();
        let header = match exchange {
            Ok(h) => h,
            Err(err) if self.is_rtk_error(py, &err)? => {
                transport.call_method0("close")?;
                return Err(err);
            }
            Err(err) if err.is_instance_of::<pyo3::exceptions::PyOSError>(py) => {
                transport.call_method0("close")?;
                let text = PyString::new(py, "could not reach {}: {}").call_method1("format", (&label, err.value(py)))?;
                let wrapped = PyErr::from_value(self.env(py).getattr("RtkError")?.call1((text,))?);
                wrapped.set_cause(py, Some(err));
                return Err(wrapped);
            }
            Err(err) => return Err(err),
        };
        self.locked(svc, || {
            svc.setattr("_device", &label)?;
            svc.setattr("_baud", 0)
        })?;
        transport.setattr("pending", header)?;
        Ok(transport)
    }

    /// The NTRIP request, stamped with this build's version.
    fn ntrip_request<'py>(
        &self,
        host: &Bound<'py, PyAny>,
        port: &Bound<'py, PyAny>,
        mountpoint: &Bound<'py, PyAny>,
        username: &Bound<'py, PyAny>,
        password: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        ntrip_request(host, port, mountpoint, username, password)
    }

    /// The caster's response header consumed; the bytes after it returned.
    fn read_ntrip_header<'py>(&self, sock: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
        let py = sock.py();
        let clock = self.env(py).getattr("time")?;
        read_ntrip_header(sock, &clock, self.constant(py, "NTRIP_CONNECT_TIMEOUT_S")?)
    }

    // ------------------------------------------------------- one session

    /// Configure the source if it can be, then forward until it ends.
    fn run_session(&self, svc: &Bound<'_, PyAny>, transport: &Bound<'_, PyAny>, settings: &Bound<'_, PyAny>, version: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        let rtk = self.rtk(py)?;
        let framer = rtk.getattr("RtcmFramer")?.call0()?;
        let ubx = rtk.getattr("UbxParser")?.call0()?;
        svc.call_method0("_reset_counters")?;
        let started = self.now(py)?;
        self.locked(svc, || svc.setattr("_started_at", started))?;
        let pending = transport.getattr("pending")?;
        if pending.is_truthy()? {
            svc.call_method1("_consume", (&pending, &framer, &ubx))?;
            transport.setattr("pending", PyBytes::new(py, b""))?;
        }
        let configurable = transport.getattr("configurable")?.is_truthy()?
            && !version.is_none()
            && !settings.call_method1("get", ("source",))?.eq("ntrip")?;
        if configurable {
            svc.call_method1("_configure", (transport, settings, &framer, &ubx, version))?;
        } else {
            svc.call_method1("_publish", (STATE_ACTIVE, format!("Forwarding corrections from {}", transport.getattr("label")?.str()?)))?;
        }
        let deadline_quiet = self.now(py)? + self.constant(py, "LISTEN_TIMEOUT_S")?;
        while !self.stopped(svc)? && !self.restarting(svc)? {
            let chunk = transport.call_method0("read")?;
            if chunk.is_truthy()? {
                svc.call_method1("_consume", (&chunk, &framer, &ubx))?;
            } else {
                self.sleep(py, 0.02)?;
            }
            let (last, frames) = self.locked(svc, || Ok((svc.getattr("_last_frame_at")?, svc.getattr("_frames")?)))?;
            let now = self.now(py)?;
            let label = || -> PyResult<String> { Ok(transport.getattr("label")?.str()?.to_string()) };
            if !frames.is_truthy()? && now > deadline_quiet {
                return Err(self.rtk_error(
                    py,
                    format!("{} produced no corrections. Check that the base station has power and a clear view of the sky", label()?),
                )?);
            }
            if frames.is_truthy()? && last.is_truthy()? && now - last.extract::<f64>()? > self.constant(py, "SOURCE_SILENCE_S")? {
                return Err(self.rtk_error(py, format!("{} stopped sending corrections", label()?))?);
            }
        }
        Ok(())
    }

    /// Put a recognised receiver into base mode and wait until it is ready.
    fn configure(
        &self,
        svc: &Bound<'_, PyAny>,
        transport: &Bound<'_, PyAny>,
        settings: &Bound<'_, PyAny>,
        framer: &Bound<'_, PyAny>,
        ubx: &Bound<'_, PyAny>,
        version: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = svc.py();
        let rtk = self.rtk(py)?;
        let modern = rtk.call_method1("supports_valset", (version,))?.is_truthy()?;
        let msm7 = settings.call_method1("get", ("msm7",))?.is_truthy()?;
        if settings.call_method1("get", ("mode",))?.eq("fixed")? {
            let fixed = settings.call_method1("get", ("fixed",))?;
            let fixed = if fixed.is_truthy()? { fixed } else { PyDict::new(py).into_any() };
            let problem = rtk.call_method1("fixed_position_problem", (&fixed,))?;
            if problem.is_truthy()? {
                return Err(self.rtk_error(py, problem.str()?.to_string())?);
            }
            let float = |key: &str| -> PyResult<Bound<'_, PyAny>> { py.get_type::<pyo3::types::PyFloat>().call1((fixed.get_item(key)?,)) };
            let builder = if modern { "fixed_position_frames" } else { "legacy_fixed_position_frames" };
            let frames = rtk.call_method1(builder, (float("latitude")?, float("longitude")?, float("altitude")?, float("accuracy")?, msm7))?;
            svc.call_method1("_publish", (STATE_CONNECTING, "Setting the fixed base position"))?;
            svc.call_method1("_write_all", (transport, frames))?;
            svc.call_method1("_activate", (transport, modern, msm7))?;
            svc.call_method1("_publish", (STATE_ACTIVE, "Correcting from the fixed base position"))?;
            return Ok(());
        }
        svc.call_method1("_publish", (STATE_CONNECTING, "Starting the survey"))?;
        let stopper = if modern { "stop_output_frames" } else { "legacy_stop_output_frames" };
        svc.call_method1("_write_all", (transport, rtk.call_method1(stopper, (msm7,))?))?;
        svc.call_method1("_await_survey_stopped", (transport, ubx, framer, modern))?;
        let builder = if modern { "survey_in_frames" } else { "legacy_survey_in_frames" };
        let duration = py.get_type::<pyo3::types::PyInt>().call1((settings.get_item("survey_duration")?,))?;
        let accuracy = py.get_type::<pyo3::types::PyFloat>().call1((settings.get_item("survey_accuracy")?,))?;
        let frames = rtk.call_method1(builder, (duration, accuracy, msm7))?;
        svc.call_method1("_write_all", (transport, frames))?;
        svc.call_method1("_publish", (STATE_SURVEYING, "Surveying: the base is learning where it is"))?;
        svc.call_method1("_await_survey_complete", (transport, ubx, framer, settings))?;
        svc.call_method1("_activate", (transport, modern, msm7))?;
        svc.call_method1("_publish", (STATE_ACTIVE, "The survey is complete and corrections are flowing"))?;
        Ok(())
    }

    fn activate(&self, svc: &Bound<'_, PyAny>, transport: &Bound<'_, PyAny>, modern: bool, msm7: bool) -> PyResult<()> {
        let py = svc.py();
        let rtk = self.rtk(py)?;
        let frames = if modern {
            rtk.call_method1("activate_output_frames", (msm7,))?
        } else {
            rtk.call_method1("legacy_activate_output_frames", (msm7,))?
        };
        svc.call_method1("_write_all", (transport, frames))?;
        Ok(())
    }

    /// Write *frames* in order with a pause between them; *force* writes
    /// even once stopping, for the teardown that has to happen because of it.
    #[pyo3(signature = (svc, transport, frames, force = false))]
    fn write_all(&self, svc: &Bound<'_, PyAny>, transport: &Bound<'_, PyAny>, frames: &Bound<'_, PyAny>, force: bool) -> PyResult<()> {
        let py = svc.py();
        for frame in frames.try_iter()? {
            let frame = frame?;
            if svc.getattr("_stop")?.call_method0("is_set")?.is_truthy()? && !force {
                return Ok(());
            }
            transport.call_method1("write", (frame,))?;
            self.sleep(py, 0.05)?;
        }
        Ok(())
    }

    /// Wait until NAV-SVIN reports no survey running; a receiver that never
    /// says is a warning, not an error.
    fn await_survey_stopped(&self, svc: &Bound<'_, PyAny>, transport: &Bound<'_, PyAny>, ubx: &Bound<'_, PyAny>, framer: &Bound<'_, PyAny>, modern: bool) -> PyResult<()> {
        let py = svc.py();
        let _ = modern;
        let deadline = self.now(py)? + self.constant(py, "SURVEY_STOP_TIMEOUT_S")?;
        let poll = self.rtk(py)?.call_method1("poll_frame", (UBX_CLASS_NAV, UBX_ID_NAV_SVIN))?;
        let mut seen = false;
        while self.now(py)? < deadline && !self.stopped(svc)? {
            transport.call_method1("write", (&poll,))?;
            let end = self.now(py)? + 0.3;
            while self.now(py)? < end {
                let chunk = transport.call_method0("read")?;
                if !chunk.is_truthy()? {
                    self.sleep(py, 0.02)?;
                    continue;
                }
                let survey = svc.call_method1("_consume", (chunk, framer, ubx))?;
                if survey.is_none() {
                    continue;
                }
                seen = true;
                if !survey.get_item("active")?.is_truthy()? && !survey.get_item("valid")?.is_truthy()? {
                    return Ok(());
                }
            }
        }
        let text = if seen {
            "the previous survey did not stop; its settings may still apply"
        } else {
            "the receiver did not report its survey state"
        };
        svc.call_method1("_warn", (text,))?;
        Ok(())
    }

    /// Read NAV-SVIN until the survey is valid and no longer running; no
    /// timeout, on purpose.
    fn await_survey_complete(&self, svc: &Bound<'_, PyAny>, transport: &Bound<'_, PyAny>, ubx: &Bound<'_, PyAny>, framer: &Bound<'_, PyAny>, settings: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        let _ = settings;
        let poll = self.rtk(py)?.call_method1("poll_frame", (UBX_CLASS_NAV, UBX_ID_NAV_SVIN))?;
        let mut next_poll = 0.0;
        while !self.stopped(svc)? && !self.restarting(svc)? {
            let now = self.now(py)?;
            if now >= next_poll {
                transport.call_method1("write", (&poll,))?;
                next_poll = now + 1.0;
            }
            let chunk = transport.call_method0("read")?;
            if !chunk.is_truthy()? {
                self.sleep(py, 0.05)?;
                continue;
            }
            let survey = svc.call_method1("_consume", (chunk, framer, ubx))?;
            if survey.is_truthy()? && survey.get_item("valid")?.is_truthy()? && !survey.get_item("active")?.is_truthy()? {
                return Ok(());
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------- bytes in

    /// Feed one read to both parsers, inject the corrections it finished,
    /// and return the survey report it carried.
    fn consume<'py>(&self, svc: &Bound<'py, PyAny>, chunk: &Bound<'py, PyAny>, framer: &Bound<'py, PyAny>, ubx: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = svc.py();
        let rtk = self.rtk(py)?;
        let mut survey = none(py);
        for frame in framer.call_method1("feed", (chunk,))?.try_iter()? {
            let frame = frame?;
            svc.call_method1("_note_frame", (&frame,))?;
            svc.getattr("_mavlink")?.call_method1("inject_rtcm", (&frame,))?;
        }
        for message in ubx.call_method1("feed", (chunk,))?.try_iter()? {
            let (class, id, payload): (Bound<'py, PyAny>, Bound<'py, PyAny>, Bound<'py, PyAny>) = message?.extract()?;
            if class.eq(UBX_CLASS_NAV)? && id.eq(UBX_ID_NAV_SVIN)? {
                let parsed = rtk.call_method1("parse_nav_svin", (payload,))?;
                if !parsed.is_none() {
                    survey = parsed.clone();
                    self.locked(svc, || svc.setattr("_survey", &parsed))?;
                    svc.call_method1("_publish_progress", (&parsed,))?;
                }
            }
        }
        let crc = framer.getattr("crc_errors")?;
        self.locked(svc, || svc.setattr("_crc_errors", crc))?;
        Ok(survey)
    }

    fn note_frame(&self, svc: &Bound<'_, PyAny>, frame: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        let number = self.rtk(py)?.call_method1("message_number", (frame,))?;
        let length = frame.len()?;
        self.locked(svc, || {
            svc.setattr("_frames", svc.getattr("_frames")?.add(1)?)?;
            svc.setattr("_frame_bytes", svc.getattr("_frame_bytes")?.add(length)?)?;
            svc.setattr("_last_frame_at", self.now(py)?)?;
            if number.is_truthy()? {
                let key = number.str()?;
                let seen = svc.getattr("_messages_seen")?;
                seen.set_item(&key, seen.call_method1("get", (&key, 0))?.add(1)?)?;
            }
            Ok(())
        })
    }

    // ---------------------------------------------------------- teardown

    /// Close the source, taking a configured receiver out of base mode.
    fn release(&self, svc: &Bound<'_, PyAny>, transport: &Bound<'_, PyAny>, settings: &Bound<'_, PyAny>, version: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        if transport.getattr("configurable")?.is_truthy()? && !version.is_none() {
            let rtk = self.rtk(py)?;
            let msm7 = settings.call_method1("get", ("msm7",))?.is_truthy()?;
            let frames = if rtk.call_method1("supports_valset", (version,))?.is_truthy()? {
                rtk.call_method1("disable_base_frames", (msm7,))?
            } else {
                rtk.call_method1("legacy_disable_base_frames", (msm7,))?
            };
            let kwargs = kw(py, &[("force", true.into_pyobject(py)?.to_owned().into_any().unbind())])?;
            match svc.call_method("_write_all", (transport, frames), Some(&kwargs)) {
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    self.logger(py)?.call_method1("debug", ("could not take the receiver out of base mode",))?;
                }
                Err(err) => return Err(err),
                Ok(_) => {}
            }
        }
        transport.call_method0("close")?;
        Ok(())
    }

    // ------------------------------------------------------------- state

    /// The serial device the MAVLink bridge is using, or "".
    fn link_device<'py>(&self, svc: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = svc.py();
        let conn = match svc.getattr("_mavlink").and_then(|m| m.call_method0("connection_string")) {
            Ok(c) => {
                let c = if c.is_truthy()? { c } else { s(py, "") };
                c.str()?.into_any()
            }
            Err(err) if err.is_instance_of::<PyException>(py) => return Ok(s(py, "")),
            Err(err) => return Err(err),
        };
        if !conn.call_method1("startswith", ("serial:",))?.is_truthy()? {
            return Ok(s(py, ""));
        }
        conn.call_method1("split", (":", 2))?.get_item(1)
    }

    fn link_ready(&self, svc: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = svc.py();
        match svc.getattr("_mavlink").and_then(|m| m.call_method0("is_connected")) {
            Ok(v) => v.is_truthy(),
            Err(err) if err.is_instance_of::<PyException>(py) => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Serial ports that look like a base station, the link's port excluded.
    fn candidate_ports<'py>(&self, svc: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
        let py = svc.py();
        let out = PyList::empty(py);
        let rows = match svc.call_method0("_list_ports") {
            Ok(r) => r,
            Err(err) if err.is_instance_of::<PyException>(py) => return Ok(out),
            Err(err) => return Err(err),
        };
        let link = svc.call_method0("_link_device")?;
        let env = self.env(py);
        let phantom = env.getattr("is_phantom_device")?;
        let rtk_device = env.getattr("is_rtk_device")?;
        let rows = if rows.is_truthy()? { rows } else { PyList::empty(py).into_any() };
        for row in rows.try_iter()? {
            let row = row?;
            let field = |name: &str| -> PyResult<Bound<'py, PyAny>> {
                let v = row.call_method1("get", (name,))?;
                let v = if v.is_truthy()? { v } else { s(py, "") };
                Ok(v.str()?.into_any())
            };
            let device = field("device")?;
            if !device.is_truthy()? || device.eq(&link)? || phantom.call1((&device,))?.is_truthy()? {
                continue;
            }
            let hwid = field("hwid")?;
            let description = field("description")?;
            if !rtk_device.call1((&device, &hwid, &description))?.is_truthy()? {
                continue;
            }
            let d = PyDict::new(py);
            d.set_item("device", device)?;
            d.set_item("description", description)?;
            d.set_item("hwid", hwid)?;
            out.append(d)?;
        }
        Ok(out)
    }

    fn publish(&self, svc: &Bound<'_, PyAny>, state: &Bound<'_, PyAny>, message: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        let rtk = self.rtk(py)?;
        let (survey, settings) = self.locked(svc, || {
            svc.setattr("_state", state)?;
            svc.setattr("_message", message)?;
            if !state.eq(STATE_ERROR)? {
                svc.setattr("_error", "")?;
            }
            Ok((self.copy_or_none(py, svc.getattr("_survey")?)?, rtk.call_method1("settings", (svc.getattr("_settings")?,))?))
        })?;
        svc.call_method1("_store_update", (state, survey, settings))?;
        self.logger(py)?.call_method1("debug", ("RTK %s: %s", state, message))?;
        Ok(())
    }

    fn publish_progress(&self, svc: &Bound<'_, PyAny>, survey: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        let rtk = self.rtk(py)?;
        let (state, settings) = self.locked(svc, || Ok((svc.getattr("_state")?, rtk.call_method1("settings", (svc.getattr("_settings")?,))?)))?;
        svc.call_method1("_store_update", (state, survey, settings))?;
        Ok(())
    }

    /// The four scalar fields the rest of the app reads, into the store.
    fn store_update(&self, svc: &Bound<'_, PyAny>, state: &Bound<'_, PyAny>, survey: &Bound<'_, PyAny>, settings: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        let result = (|| -> PyResult<()> {
            let source = settings.call_method1("get", ("source",))?;
            let source = if source.is_truthy()? { source } else { s(py, "") };
            let held = if survey.is_truthy()? { survey.clone() } else { PyDict::new(py).into_any() };
            let accuracy = held.call_method1("get", ("accuracy",))?;
            let accuracy = if accuracy.is_truthy()? { accuracy } else { 0.0f64.into_pyobject(py)?.into_any() };
            let accuracy = py.get_type::<pyo3::types::PyFloat>().call1((accuracy,))?;
            let progress = self.rtk(py)?.call_method1(
                "survey_progress",
                (survey, settings.get_item("survey_duration")?, settings.get_item("survey_accuracy")?),
            )?;
            let kwargs = kw(
                py,
                &[
                    ("rtk_state", state.clone().unbind()),
                    ("rtk_source", source.str()?.into_any().unbind()),
                    ("rtk_accuracy", accuracy.unbind()),
                    ("rtk_progress", progress.unbind()),
                ],
            )?;
            svc.getattr("_store")?.call_method("update", (), Some(&kwargs))?;
            Ok(())
        })();
        match result {
            Err(err) if err.is_instance_of::<PyException>(py) => {
                self.logger(py)?.call_method1("debug", ("RTK store update failed",))?;
                Ok(())
            }
            other => other,
        }
    }

    fn fail(&self, svc: &Bound<'_, PyAny>, reason: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        let rtk = self.rtk(py)?;
        let (survey, settings) = self.locked(svc, || {
            svc.setattr("_state", STATE_ERROR)?;
            svc.setattr("_error", reason)?;
            svc.setattr("_message", reason)?;
            Ok((self.copy_or_none(py, svc.getattr("_survey")?)?, rtk.call_method1("settings", (svc.getattr("_settings")?,))?))
        })?;
        svc.call_method1("_store_update", (STATE_ERROR, survey, settings))?;
        self.logger(py)?.call_method1("info", ("RTK: %s", reason))?;
        Ok(())
    }

    fn warn(&self, svc: &Bound<'_, PyAny>, text: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        self.locked(svc, || svc.setattr("_warning", text))?;
        self.logger(py)?.call_method1("info", ("RTK: %s", text))?;
        Ok(())
    }

    fn reset_counters(&self, svc: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        self.locked(svc, || {
            svc.setattr("_frames", 0)?;
            svc.setattr("_frame_bytes", 0)?;
            svc.setattr("_crc_errors", 0)?;
            svc.setattr("_last_frame_at", 0.0)?;
            svc.setattr("_messages_seen", PyDict::new(py))
        })?;
        match svc.getattr("_mavlink").and_then(|m| m.call_method0("forget_rtcm_session")) {
            Err(err) if !err.is_instance_of::<PyException>(py) => Err(err),
            _ => Ok(()),
        }
    }

    fn describe_intent(&self, svc: &Bound<'_, PyAny>) -> PyResult<String> {
        let settings = svc.call_method0("settings")?;
        if !settings.call_method1("get", ("enabled",))?.is_truthy()? {
            return Ok("disabled".into());
        }
        if settings.call_method1("get", ("source",))?.eq("ntrip")? {
            return Ok("NTRIP".into());
        }
        Ok(format!("USB base station, {}", settings.call_method1("get", ("mode",))?.str()?))
    }

    /// The default serial port: corvus_link's own, at *baud*.
    fn open_port<'py>(&self, py: Python<'py>, device: &Bound<'py, PyAny>, baud: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let kwargs = kw(
            py,
            &[
                ("timeout", self.constant(py, "READ_TIMEOUT_S")?.into_pyobject(py)?.into_any().unbind()),
                ("write_timeout", 2.0f64.into_pyobject(py)?.into_any().unbind()),
            ],
        )?;
        py.import("corvus_link.rtk")?.getattr("SerialPort")?.call((device, baud), Some(&kwargs))
    }

    /// The default caster socket: a TCP connection with a read timeout.
    fn open_socket<'py>(&self, py: Python<'py>, host: &Bound<'py, PyAny>, port: &Bound<'py, PyAny>, timeout: f64) -> PyResult<Bound<'py, PyAny>> {
        let sock = py.import("corvus_link.rtk")?.call_method1("connect_tcp", (host, port, timeout))?;
        sock.call_method1("settimeout", (self.constant(py, "READ_TIMEOUT_S")?,))?;
        Ok(sock)
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<RtkEngine>()
}
