//! Smart default auto-connect: which link to start on, and a watcher that
//! dials while the link is idle and only suggests while it is busy.
//!
//! The priority is the command line, then a direct USB flight controller,
//! then a SiK radio, then the configured string, then the UDP fallback.
//! Never steal an active link; a manual connect wins the session; a link
//! whose device came back under another name is followed there whatever the
//! override says, because it is the device the operator chose. No port is
//! ever opened to find out what is behind it.
//!
//! The data classes, the session state and the serial port helpers stay in
//! Python (``corvus.autoconnect``, ``corvus.serial_ports``); the policy and
//! the watcher's thread (a `CoreThread`) are here.

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyModule, PyString, PyTuple};

use crate::bridge::thread;
use crate::protocol::host::obj;

fn kw<'py>(py: Python<'py>, pairs: &[(&str, PyObject)]) -> PyResult<Bound<'py, PyDict>> {
    let kwargs = PyDict::new(py);
    for (k, v) in pairs {
        kwargs.set_item(*k, v)?;
    }
    Ok(kwargs)
}

/// The auto-connect policy and watcher.
#[pyclass(name = "AutoConnectEngine", module = "corvus_link.bridge", frozen)]
pub struct AutoConnectEngine {
    env: PyObject,
}

impl AutoConnectEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn helper<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr(name)
    }

    fn logger<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr("logger")
    }

    fn same_port(&self, py: Python<'_>, a: &Bound<'_, PyAny>, b: &Bound<'_, PyAny>) -> PyResult<bool> {
        self.helper(py, "same_port")?.call1((a, b))?.is_truthy()
    }

    fn reason_for<'py>(&self, py: Python<'py>, kind: &str) -> PyResult<Bound<'py, PyAny>> {
        self.helper(py, if kind == "usb" { "REASON_USB" } else { "REASON_SIK" })
    }
}

#[pymethods]
impl AutoConnectEngine {
    /// *env* is ``corvus.autoconnect``.
    #[new]
    fn new(env: PyObject) -> Self {
        AutoConnectEngine { env }
    }

    /// `list_serial_ports()` rows as classified ports; a malformed row, a
    /// phantom and a pseudo terminal are dropped.
    fn classify_ports<'py>(&self, py: Python<'py>, rows: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
        let out = PyList::empty(py);
        let rows = if rows.is_truthy()? { rows.clone() } else { PyTuple::empty(py).into_any() };
        let from_row = self.helper(py, "PortInfo")?.getattr("from_row")?;
        let phantom = self.helper(py, "is_phantom_device")?;
        for row in rows.try_iter()? {
            let port = match row.and_then(|row| from_row.call1((row,))) {
                Ok(p) => p,
                Err(err) if err.is_instance_of::<PyException>(py) => continue,
                Err(err) => return Err(err),
            };
            let device = port.getattr("device")?;
            if !device.is_truthy()? || phantom.call1((&device,))?.is_truthy()? {
                continue;
            }
            out.append(port)?;
        }
        Ok(out)
    }

    /// May auto-connect dial this port? Never a bootloader.
    fn usable(&self, port: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = port.py();
        Ok(!self
            .helper(py, "is_bootloader_port")?
            .call1((port.getattr("device")?, port.getattr("hwid")?, port.getattr("description")?))?
            .is_truthy()?)
    }

    /// The first usable port of *kind* by device name, the same on every
    /// launch whatever enumerated first.
    fn pick<'py>(&self, ports: &Bound<'py, PyAny>, kind: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
        let mut matches: Vec<(String, Bound<'py, PyAny>)> = Vec::new();
        for port in ports.try_iter()? {
            let port = port?;
            if port.getattr("kind")?.eq(kind)? && self.usable(&port)? {
                matches.push((port.getattr("device")?.str()?.to_string(), port));
            }
        }
        matches.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(matches.into_iter().next().map(|(_, p)| p))
    }

    /// `serial:<device>:<baud>`.
    fn serial_connection_string(&self, device: &Bound<'_, PyAny>, baud: &Bound<'_, PyAny>) -> PyResult<String> {
        let py = device.py();
        Ok(format!("serial:{}:{}", device.str()?, py.get_type::<pyo3::types::PyInt>().call1((baud,))?.str()?))
    }

    /// The device of a `serial:` connection string, else "".
    fn serial_device<'py>(&self, conn: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = conn.py();
        if !conn.is_instance_of::<PyString>() || !conn.call_method1("startswith", ("serial:",))?.is_truthy()? {
            return Ok(obj(py, "").into_bound(py));
        }
        self.helper(py, "split_serial_connection")?.call1((conn,))?.get_item(0)
    }

    /// The baud rate already chosen for *device* in any of *known*, else the
    /// factory default.
    #[pyo3(signature = (device, *known))]
    fn baud_for<'py>(&self, device: &Bound<'py, PyAny>, known: &Bound<'py, PyTuple>) -> PyResult<Bound<'py, PyAny>> {
        let py = device.py();
        for conn in known.iter() {
            let other = self.serial_device(&conn)?;
            if other.is_truthy()? && self.same_port(py, device, &other)? {
                return self.helper(py, "split_serial_connection")?.call1((&conn,))?.get_item(1);
            }
        }
        self.helper(py, "DEFAULT_BAUD")
    }

    /// `(device, baud, kind)` for the best serial candidate, USB before SiK.
    fn pick_usb_serial(&self, ports: &Bound<'_, PyAny>, usb_enabled: bool, sik_enabled: bool) -> PyResult<PyObject> {
        let py = ports.py();
        for (enabled, kind) in [(usb_enabled, "usb"), (sik_enabled, "sik")] {
            if !enabled {
                continue;
            }
            if let Some(hit) = self.pick(ports, kind)? {
                return Ok(PyTuple::new(py, [hit.getattr("device")?, self.helper(py, "DEFAULT_BAUD")?, obj(py, kind).into_bound(py)])?
                    .into_any()
                    .unbind());
            }
        }
        Ok(py.None())
    }

    /// Would the bridge accept *conn*? Anything unvalidatable is not usable.
    fn is_valid(&self, conn: &Bound<'_, PyAny>, validator: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = conn.py();
        if !conn.is_instance_of::<PyString>() || !conn.call_method0("strip")?.is_truthy()? {
            return Ok(false);
        }
        if validator.is_none() {
            return Ok(true);
        }
        match validator.call1((conn,)) {
            Ok(_) => Ok(true),
            Err(err) if err.is_instance_of::<PyException>(py) => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// The connection to start on, and why.
    #[allow(clippy::too_many_arguments)]
    fn resolve_startup_connection<'py>(
        &self,
        cli_arg: &Bound<'py, PyAny>,
        configured: &Bound<'py, PyAny>,
        ports: &Bound<'py, PyAny>,
        usb_enabled: bool,
        sik_enabled: bool,
        udp_fallback_enabled: bool,
        default_connection: &Bound<'py, PyAny>,
        udp_fallback: &Bound<'py, PyAny>,
        validator: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = cli_arg.py();
        let decision = self.helper(py, "StartupDecision")?;
        if self.is_valid(cli_arg, validator)? {
            return decision.call1((cli_arg.str()?.call_method0("strip")?, self.helper(py, "REASON_CLI")?));
        }
        let pick = self.pick_usb_serial(ports, usb_enabled, sik_enabled)?.into_bound(py);
        if !pick.is_none() {
            let device = pick.get_item(0)?;
            let kind: String = pick.get_item(2)?.extract()?;
            let baud = self.baud_for(&device, &PyTuple::new(py, [configured])?)?;
            let conn = self.serial_connection_string(&device, &baud)?;
            let mut candidates = 0;
            for port in ports.try_iter()? {
                let port = port?;
                if port.getattr("kind")?.eq(kind.as_str())? && self.usable(&port)? {
                    candidates += 1;
                }
            }
            let detail = if candidates < 2 {
                device.str()?.to_string()
            } else {
                format!("{} ({candidates} candidates)", device.str()?)
            };
            return decision.call1((conn, self.reason_for(py, &kind)?, detail));
        }
        if self.is_valid(configured, validator)? {
            return decision.call1((configured.str()?.call_method0("strip")?, self.helper(py, "REASON_CONFIGURED")?));
        }
        if udp_fallback_enabled && self.is_valid(udp_fallback, validator)? {
            return decision.call1((udp_fallback, self.helper(py, "REASON_UDP_FALLBACK")?));
        }
        decision.call1((default_connection, self.helper(py, "REASON_DEFAULT_FALLBACK")?))
    }

    /// A device worth offering while the link is busy, or None; never once
    /// the operator chose a link by hand, never the one already connected.
    fn suggest_connection<'py>(&self, snapshot: &Bound<'py, PyAny>, ports: &Bound<'py, PyAny>, session: &Bound<'py, PyAny>) -> PyResult<PyObject> {
        let py = snapshot.py();
        if !session.getattr("enabled")?.is_truthy()? || session.getattr("manual_override")?.is_truthy()? {
            return Ok(py.None());
        }
        let kwargs = kw(py, &[("usb_enabled", session.getattr("usb")?.unbind()), ("sik_enabled", session.getattr("sik")?.unbind())])?;
        let pick = self.helper(py, "pick_usb_serial")?.call((ports,), Some(&kwargs))?;
        if pick.is_none() {
            return Ok(py.None());
        }
        let device = pick.get_item(0)?;
        let kind: String = pick.get_item(2)?.extract()?;
        let link = snapshot.call_method1("get", ("link_connection", ""))?;
        let link = if link.is_truthy()? { link.str()?.into_any() } else { obj(py, "").into_bound(py) };
        if self.same_port(py, &device, &self.serial_device(&link)?)? {
            return Ok(py.None());
        }
        let conn = self.serial_connection_string(&device, &self.helper(py, "DEFAULT_BAUD")?)?;
        let kwargs = kw(
            py,
            &[
                ("connection_string", obj(py, conn)),
                ("kind", obj(py, if kind == "usb" { "usb-direct" } else { "sik" })),
                ("device", device.unbind()),
            ],
        )?;
        Ok(self.helper(py, "Suggestion")?.call((), Some(&kwargs))?.unbind())
    }

    // ---------------------------------------------------------- watcher

    /// Begin watching; idempotent, and a no-op when auto-connect is off.
    fn start(&self, watcher: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = watcher.py();
        if !watcher.getattr("_session")?.getattr("enabled")?.is_truthy()? {
            self.logger(py)?.call_method1("info", ("autoconnect: watcher not started (disabled in config)",))?;
            return Ok(());
        }
        let current = watcher.getattr("_thread")?;
        if !current.is_none() && current.call_method0("is_alive")?.is_truthy()? {
            return Ok(());
        }
        watcher.getattr("_stop_event")?.call_method0("clear")?;
        let target = watcher.clone().unbind();
        let handle = thread::spawn(py, "autoconnect-watcher", move |py| {
            target.bind(py).call_method0("_loop")?;
            Ok(())
        })?;
        watcher.setattr("_thread", handle)
    }

    /// The watcher thread: a pass every poll interval until stopped; a pass
    /// that fails is logged and never ends the watcher.
    fn run(&self, watcher: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = watcher.py();
        while !thread::shutting_down()
            && !watcher.getattr("_stop_event")?.call_method1("wait", (watcher.getattr("_poll_s")?,))?.is_truthy()?
        {
            match watcher.call_method0("tick") {
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    let kwargs = kw(py, &[("exc_info", err.value(py).clone().into_any().unbind())])?;
                    self.logger(py)?.call_method("error", ("autoconnect: watcher iteration failed",), Some(&kwargs))?;
                }
                Err(err) => return Err(err),
                Ok(_) => {}
            }
        }
        Ok(())
    }

    /// One decision pass; returns what it did.
    fn tick(&self, watcher: &Bound<'_, PyAny>) -> PyResult<String> {
        let py = watcher.py();
        let session = watcher.getattr("_session")?;
        if watcher.getattr("_stop_event")?.call_method0("is_set")?.is_truthy()? || !session.getattr("enabled")?.is_truthy()? {
            return Ok("disabled".into());
        }
        let manual = session.getattr("manual_override")?.is_truthy()?;
        if !watcher.getattr("_bridge")?.call_method0("is_running")?.is_truthy()? {
            return Ok(if manual { "noop: manual-override" } else { "noop: bridge-stopped" }.into());
        }
        let snapshot = watcher.call_method0("_snapshot")?;
        let status = snapshot.call_method1("get", ("link_status", ""))?;
        let status = if status.is_truthy()? { status.str()?.call_method0("lower")? } else { obj(py, "").into_bound(py) };
        let mut ports = self.helper(py, "classify_ports")?.call1((watcher.getattr("_list_ports")?.call0()?,))?;
        let mut busy = Vec::new();
        for d in watcher.call_method0("_busy")?.try_iter()? {
            let d = d?;
            if d.is_truthy()? {
                busy.push(d);
            }
        }
        if !busy.is_empty() {
            let kept = PyList::empty(py);
            for port in ports.try_iter()? {
                let port = port?;
                let device = port.getattr("device")?;
                let mut taken = false;
                for d in &busy {
                    if self.same_port(py, &device, d)? {
                        taken = true;
                        break;
                    }
                }
                if !taken {
                    kept.append(port)?;
                }
            }
            ports = kept.into_any();
        }
        watcher.call_method1("_publish_bootloader", (&ports,))?;
        let link = watcher.call_method1("_link_connection", (&snapshot,))?;
        let device = self.serial_device(&link)?;
        let idle_states = self.helper(py, "IDLE_STATES")?;
        if device.is_truthy()? {
            let names = PyList::empty(py);
            for port in ports.try_iter()? {
                names.append(port?.getattr("device")?)?;
            }
            if self.helper(py, "port_present")?.call1((&device, names))?.is_truthy()? {
                watcher.call_method1("_remember_identity", (&link, &device, &ports))?;
            } else if !idle_states.contains(&status)? {
                return Ok("noop: link-device-gone".into());
            } else {
                let moved = watcher.call_method1("_moved_port", (&link, &device, &ports))?;
                if !moved.is_none() {
                    return watcher.call_method1("_follow", (&link, moved))?.extract();
                }
            }
        }
        if manual {
            return Ok("noop: manual-override".into());
        }
        let idle = idle_states.contains(&status)? && watcher.call_method0("_is_stale")?.is_truthy()?;
        let kwargs = kw(py, &[("usb_enabled", session.getattr("usb")?.unbind()), ("sik_enabled", session.getattr("sik")?.unbind())])?;
        let pick = self.helper(py, "pick_usb_serial")?.call((&ports,), Some(&kwargs))?;
        if pick.is_none() {
            watcher.call_method1("_publish_suggestion", (py.None(),))?;
            session.setattr("last_dial_key", py.None())?;
            return Ok(format!("noop: {}", self.helper(py, "REASON_NO_CANDIDATE")?.str()?));
        }
        if !idle {
            let suggestion = self.helper(py, "suggest_connection")?.call1((&snapshot, &ports, &session))?;
            let published = watcher.call_method1("_publish_suggestion", (&suggestion,))?.is_truthy()?;
            if suggestion.is_none() {
                return Ok("noop: fresh-heartbeat".into());
            }
            return Ok(if published { "suggested" } else { "noop: suggestion-unchanged" }.into());
        }
        watcher.call_method1("_auto_dial", (pick,))?.extract()
    }

    /// Dial the winner once per appearance; the bridge's backoff owns the
    /// retries.
    fn auto_dial(&self, watcher: &Bound<'_, PyAny>, pick: &Bound<'_, PyAny>) -> PyResult<String> {
        let py = watcher.py();
        let session = watcher.getattr("_session")?;
        let bridge = watcher.getattr("_bridge")?;
        let device = pick.get_item(0)?;
        let kind: String = pick.get_item(2)?.extract()?;
        let link = watcher.call_method1("_link_connection", (watcher.call_method0("_snapshot")?,))?;
        let conn = self.serial_connection_string(&device, &self.baud_for(&device, &PyTuple::new(py, [link])?)?)?;
        if session.getattr("last_dial_key")?.eq(conn.as_str())? {
            return Ok("noop: already-dialled".into());
        }
        if let Err(err) = bridge.call_method1("validate_connection", (conn.as_str(),)) {
            if !err.is_instance_of::<pyo3::exceptions::PyValueError>(py) {
                return Err(err);
            }
            self.logger(py)?.call_method1("warning", ("autoconnect: refusing %s (%s)", conn.as_str(), err.value(py)))?;
            session.setattr("last_dial_key", conn.as_str())?;
            return Ok("noop: invalid".into());
        }
        let reason = self.reason_for(py, &kind)?;
        self.logger(py)?
            .call_method1("info", ("autoconnect: dialling winner=%s reason=%s detail=%s", conn.as_str(), &reason, &device))?;
        session.setattr("last_dial_key", conn.as_str())?;
        if let Err(err) = bridge.call_method1("switch_connection", (conn.as_str(),)) {
            if !err.is_instance_of::<PyException>(py) {
                return Err(err);
            }
            let kwargs = kw(py, &[("exc_info", err.value(py).clone().into_any().unbind())])?;
            self.logger(py)?.call_method("error", ("autoconnect: dial of %s failed", conn.as_str()), Some(&kwargs))?;
            return Ok("dial-failed".into());
        }
        session.call_method1("note_decision", (&reason, conn.as_str()))?;
        watcher.call_method1("_publish_suggestion", (py.None(),))?;
        watcher.call_method0("_publish_auto")?;
        Ok(format!("dialled: {}", reason.str()?))
    }

    /// Move the link to the node its device came back on, at the same baud
    /// rate and with the same override: the operator's link, renamed.
    fn follow(&self, watcher: &Bound<'_, PyAny>, link: &Bound<'_, PyAny>, port: &Bound<'_, PyAny>) -> PyResult<String> {
        let py = watcher.py();
        let session = watcher.getattr("_session")?;
        let bridge = watcher.getattr("_bridge")?;
        let old = self.serial_device(link)?;
        let baud = self.helper(py, "split_serial_connection")?.call1((link,))?.get_item(1)?;
        let conn = self.serial_connection_string(&port.getattr("device")?, &baud)?;
        if let Err(err) = bridge.call_method1("validate_connection", (conn.as_str(),)) {
            if !err.is_instance_of::<pyo3::exceptions::PyValueError>(py) {
                return Err(err);
            }
            self.logger(py)?.call_method1("warning", ("autoconnect: refusing %s (%s)", conn.as_str(), err.value(py)))?;
            return Ok("noop: invalid".into());
        }
        self.logger(py)?.call_method1(
            "info",
            ("autoconnect: %s is gone, following the same device to %s", &old, port.getattr("device")?),
        )?;
        session.setattr("last_dial_key", conn.as_str())?;
        if let Err(err) = bridge.call_method1("switch_connection", (conn.as_str(),)) {
            if !err.is_instance_of::<PyException>(py) {
                return Err(err);
            }
            let kwargs = kw(py, &[("exc_info", err.value(py).clone().into_any().unbind())])?;
            self.logger(py)?.call_method("error", ("autoconnect: dial of %s failed", conn.as_str()), Some(&kwargs))?;
            return Ok("dial-failed".into());
        }
        let identity = watcher.getattr("_link_identity")?;
        if !identity.is_none() {
            watcher.setattr("_link_identity", PyTuple::new(py, [obj(py, conn.as_str()).into_bound(py), identity.get_item(1)?])?)?;
        }
        let reason = if session.getattr("manual_override")?.is_truthy()? {
            self.helper(py, "REASON_MANUAL")?
        } else {
            let kind: String = port.getattr("kind")?.str()?.to_string();
            match kind.as_str() {
                "usb" | "sik" => self.reason_for(py, &kind)?,
                _ => session.getattr("reason")?,
            }
        };
        session.call_method1("note_decision", (reason, conn.as_str()))?;
        watcher.call_method1("_publish_suggestion", (py.None(),))?;
        watcher.call_method0("_publish_auto")?;
        Ok("followed".into())
    }

    /// Note the USB identity of the port the link is on; never a bootloader's.
    fn remember_identity(&self, watcher: &Bound<'_, PyAny>, link: &Bound<'_, PyAny>, device: &Bound<'_, PyAny>, ports: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = watcher.py();
        for port in ports.try_iter()? {
            let port = port?;
            if self.same_port(py, device, &port.getattr("device")?)? {
                let identity = self.helper(py, "usb_identity")?.call1((port.getattr("hwid")?,))?;
                if identity.is_truthy()? && self.usable(&port)? {
                    watcher.setattr("_link_identity", PyTuple::new(py, [link.clone(), identity])?)?;
                }
                return Ok(());
            }
        }
        Ok(())
    }

    /// The one present port carrying the identity the link's device had;
    /// two candidates is no answer.
    fn moved_port(&self, watcher: &Bound<'_, PyAny>, link: &Bound<'_, PyAny>, device: &Bound<'_, PyAny>, ports: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        let py = watcher.py();
        let remembered = watcher.getattr("_link_identity")?;
        if remembered.is_none() || remembered.get_item(0)?.ne(link)? {
            return Ok(py.None());
        }
        let identity = remembered.get_item(1)?;
        let usb_identity = self.helper(py, "usb_identity")?;
        let mut matches = Vec::new();
        for port in ports.try_iter()? {
            let port = port?;
            if usb_identity.call1((port.getattr("hwid")?,))?.eq(&identity)?
                && !self.same_port(py, &port.getattr("device")?, device)?
                && self.usable(&port)?
            {
                matches.push(port);
            }
        }
        Ok(if matches.len() == 1 { matches.remove(0).unbind() } else { py.None() })
    }

    /// Push or clear `link_suggestion`; True when it changed.
    fn publish_suggestion(&self, watcher: &Bound<'_, PyAny>, suggestion: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = watcher.py();
        let session = watcher.getattr("_session")?;
        let key = if suggestion.is_truthy()? { suggestion.getattr("connection_string")? } else { py.None().into_bound(py) };
        if key.eq(session.getattr("last_suggestion_key")?)? {
            return Ok(false);
        }
        session.setattr("last_suggestion_key", &key)?;
        if !suggestion.is_none() {
            self.logger(py)?.call_method1(
                "info",
                ("autoconnect: suggestion-published device=%s kind=%s", suggestion.getattr("device")?, suggestion.getattr("kind")?),
            )?;
        }
        let value = if suggestion.is_truthy()? { suggestion.call_method0("as_dict")?.unbind() } else { py.None() };
        let kwargs = kw(py, &[("link_suggestion", value)])?;
        watcher.call_method("_update_store", (), Some(&kwargs))?;
        Ok(true)
    }

    /// Push or clear `link_bootloader`, once per change.
    fn publish_bootloader(&self, watcher: &Bound<'_, PyAny>, ports: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = watcher.py();
        let bootloader = self.helper(py, "is_bootloader_port")?;
        let mut found = None;
        for port in ports.try_iter()? {
            let port = port?;
            if bootloader.call1((port.getattr("device")?, port.getattr("hwid")?, port.getattr("description")?))?.is_truthy()? {
                found = Some(port);
                break;
            }
        }
        let held = watcher.call_method0("_snapshot")?.call_method1("get", ("link_bootloader",))?;
        let held = if held.is_truthy()? { held } else { PyDict::new(py).into_any() };
        let current = held.call_method1("get", ("device",))?;
        let device = match &found {
            Some(p) => p.getattr("device")?,
            None => py.None().into_bound(py),
        };
        if device.eq(&current)? {
            return Ok(());
        }
        let value = match &found {
            Some(p) => {
                let d = PyDict::new(py);
                d.set_item("device", p.getattr("device")?)?;
                d.set_item("description", p.getattr("description")?)?;
                d.into_any().unbind()
            }
            None => py.None(),
        };
        let kwargs = kw(py, &[("link_bootloader", value)])?;
        watcher.call_method("_update_store", (), Some(&kwargs))?;
        Ok(())
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<AutoConnectEngine>()
}
