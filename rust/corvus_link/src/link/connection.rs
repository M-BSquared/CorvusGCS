//! `mavfile` and `mavlink_connection`: a link plus the per-system state
//! pymavlink's `mavutil` keeps on top of it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use pyo3::exceptions::{PyAttributeError, PyNotImplementedError, PyOSError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyString, PyTuple};
use pyo3::{PyTraverseError, PyVisit};

#[cfg(unix)]
use crate::link::transport::{ChildExec, Uds};
use crate::link::transport::{LogFile, Mcast, Serial, Tcp, TcpEvent, TcpIn, Udp};
use crate::lock;
use crate::wire::defs::{self, constant_or, Wire};
use crate::wire::dialect;
use crate::wire::frame::{self, Mavlink};
use crate::wire::modes;

static GLOBAL_LINK_ID: AtomicI64 = AtomicI64::new(0);

pub fn os_error(err: &std::io::Error) -> PyErr {
    match err.raw_os_error() {
        Some(code) => {
            let text = err.to_string();
            let text = match text.rfind(" (os error") {
                Some(i) => text[..i].to_string(),
                None => text,
            };
            PyOSError::new_err((code, text))
        }
        None => PyOSError::new_err(err.to_string()),
    }
}

fn py_print(py: Python<'_>, text: &str) {
    if let Ok(builtins) = py.import("builtins") {
        let _ = builtins.call_method1("print", (text,));
    }
}

/// Per-system state: `mavfile_state`.
#[pyclass(name = "mavfile_state", module = "corvus_link")]
pub struct MavfileState {
    #[pyo3(get, set)]
    messages: Py<PyDict>,
    #[pyo3(get, set)]
    flightmode: PyObject,
    #[pyo3(get, set)]
    vehicle_type: PyObject,
    #[pyo3(get, set)]
    mav_type: PyObject,
    #[pyo3(get, set)]
    mav_autopilot: PyObject,
    #[pyo3(get, set)]
    base_mode: PyObject,
    #[pyo3(get, set)]
    armed: PyObject,
}

impl MavfileState {
    pub fn create(py: Python<'_>) -> PyResult<Py<MavfileState>> {
        let messages = PyDict::new(py);
        let state = Py::new(
            py,
            MavfileState {
                messages: messages.clone().unbind(),
                flightmode: "UNKNOWN".into_pyobject(py)?.into_any().unbind(),
                vehicle_type: "UNKNOWN".into_pyobject(py)?.into_any().unbind(),
                mav_type: constant_or("MAV_TYPE_FIXED_WING", 1)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind(),
                mav_autopilot: constant_or("MAV_AUTOPILOT_GENERIC", 0)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind(),
                base_mode: 0i64.into_pyobject(py)?.into_any().unbind(),
                armed: false.into_pyobject(py)?.to_owned().into_any().unbind(),
            },
        )?;
        messages.set_item("MAV", state.clone_ref(py))?;
        let wire = dialect::current_wire();
        if let Some(def) = defs::message_by_name("GPS_RAW_INT", wire) {
            let args = PyTuple::new(py, vec![0i64; def.base_fields])?;
            let values = crate::wire::message::bind_arguments(py, def, wire, &args, None, false, "__init__")?;
            messages.set_item("HOME", crate::wire::message::build(py, def, wire, &values)?)?;
        }
        Ok(state)
    }
}

#[pymethods]
impl MavfileState {
    #[new]
    fn py_new(py: Python<'_>) -> PyResult<Py<MavfileState>> {
        MavfileState::create(py)
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.messages)?;
        visit.call(&self.flightmode)?;
        visit.call(&self.vehicle_type)?;
        visit.call(&self.mav_type)?;
        visit.call(&self.mav_autopilot)?;
        visit.call(&self.base_mode)?;
        visit.call(&self.armed)
    }

    fn __clear__(&mut self, py: Python<'_>) {
        self.messages = PyDict::new(py).unbind();
    }
}

/// Parameters seen for one (system, component): `param_state`.
#[pyclass(name = "param_state", module = "corvus_link")]
pub struct ParamState {
    #[pyo3(get, set)]
    params: Py<PyDict>,
}

#[pymethods]
impl ParamState {
    #[new]
    fn new(py: Python<'_>) -> Self {
        ParamState {
            params: PyDict::new(py).unbind(),
        }
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.params)
    }
}

/// State `mavlogfile` keeps between frames.
pub struct LogState {
    planner_format: bool,
    last_timestamp: Option<f64>,
    last_was_bad: bool,
    link: i64,
    percent: f64,
}

pub enum Link {
    Udp(Udp),
    Tcp(Tcp),
    TcpIn(TcpIn),
    Serial(Serial),
    Mcast(Mcast),
    #[cfg(unix)]
    Uds(Uds),
    #[cfg(unix)]
    Child(ChildExec),
    Log(LogFile, Mutex<LogState>),
}

impl Link {
    fn wait_readable(&self, timeout: f64) -> bool {
        match self {
            Link::Udp(l) => l.wait_readable(timeout),
            Link::Tcp(l) => l.wait_readable(timeout),
            Link::TcpIn(l) => l.wait_readable(timeout),
            Link::Serial(l) => l.wait_readable(timeout),
            Link::Mcast(l) => l.wait_readable(timeout),
            #[cfg(unix)]
            Link::Uds(l) => l.wait_readable(timeout),
            #[cfg(unix)]
            Link::Child(l) => l.wait_readable(timeout),
            Link::Log(..) => {
                // No descriptor: pymavlink's select just sleeps.
                std::thread::sleep(std::time::Duration::from_secs_f64(timeout.clamp(0.0, 0.5)));
                true
            }
        }
    }

    fn fileno(&self) -> Option<i64> {
        match self {
            Link::Udp(l) => l.raw().map(|h| h as i64),
            Link::Tcp(l) => l.raw().map(|h| h as i64),
            Link::TcpIn(l) => l.raw().map(|h| h as i64),
            Link::Serial(l) => l.fileno(),
            Link::Mcast(l) => l.raw().map(|h| h as i64),
            #[cfg(unix)]
            Link::Uds(l) => l.raw().map(|h| h as i64),
            #[cfg(unix)]
            Link::Child(l) => l.raw().map(|h| h as i64),
            Link::Log(..) => None,
        }
    }

    fn close(&self) {
        match self {
            Link::Udp(l) => l.close(),
            Link::Tcp(l) => l.close(),
            Link::TcpIn(l) => l.close(),
            Link::Serial(l) => l.close(),
            Link::Mcast(l) => l.close(),
            #[cfg(unix)]
            Link::Uds(l) => l.close(),
            #[cfg(unix)]
            Link::Child(l) => l.close(),
            Link::Log(f, _) => f.close(),
        }
    }

    /// One datagram per read, and no stream loop: UDP and multicast.
    fn is_datagram(&self) -> bool {
        matches!(self, Link::Udp(_) | Link::Mcast(_))
    }
}

struct ConnState {
    first_byte: bool,
    wire_version: String,
    robust_parsing: bool,
    sysid: i64,
    param_sysid: (i64, i64),
    last_seq: HashMap<(i64, i64), i64>,
    mav_loss: i64,
    mav_count: i64,
    timestamp: f64,
    uptime: f64,
    notimestamps: bool,
    ts_override: Option<f64>,
    source_system: i64,
    source_component: i64,
    address: String,
    start_time: f64,
    stop_on_eof: bool,
    portdead: bool,
    param_fetch_start: f64,
}

struct ConnRefs {
    mav: Py<Mavlink>,
    sysid_state: Py<PyDict>,
    param_state: Py<PyDict>,
    logfile: Option<PyObject>,
    logfile_raw: Option<PyObject>,
    message_hooks: Py<PyList>,
    idle_hooks: Py<PyList>,
}

/// `mavfile`, and through `kind` the subclass pymavlink would have built.
#[pyclass(name = "mavfile", module = "corvus_link", subclass, dict, frozen)]
pub struct Connection {
    link: Arc<Link>,
    kind: &'static str,
    state: Mutex<ConnState>,
    refs: Mutex<ConnRefs>,
}

/// The object behind `conn.port`: enough of a socket or pyserial port for
/// `fileno()`, `getsockname()` and `close()`.
#[pyclass(name = "port", module = "corvus_link", frozen)]
pub struct PortHandle {
    link: Arc<Link>,
}

#[pymethods]
impl PortHandle {
    fn fileno(&self) -> PyResult<i64> {
        self.link
            .fileno()
            .ok_or_else(|| PyOSError::new_err("port has no file descriptor"))
    }

    fn getsockname<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let addr = match &*self.link {
            Link::Udp(l) => l.local_addr(),
            Link::Tcp(l) => l.local_addr(),
            Link::TcpIn(l) => l.local_addr(),
            Link::Mcast(l) => l.local_addr(),
            _ => return Err(PyAttributeError::new_err("this port has no attribute 'getsockname'")),
        }
        .map_err(|e| os_error(&e))?;
        PyTuple::new(
            py,
            [
                addr.ip().to_string().into_pyobject(py)?.into_any(),
                addr.port().into_pyobject(py)?.into_any(),
            ],
        )
    }

    fn close(&self) {
        self.link.close();
    }

    #[pyo3(name = "inWaiting")]
    fn in_waiting(&self) -> usize {
        0
    }
}

impl Connection {
    fn new_object(
        py: Python<'_>,
        link: Link,
        kind: &'static str,
        address: String,
        source_system: i64,
        source_component: i64,
    ) -> PyResult<Py<Connection>> {
        let wire = dialect::current_wire();
        let sysid_state = PyDict::new(py);
        sysid_state.set_item(0, MavfileState::create(py)?)?;
        let param_state = PyDict::new(py);
        param_state.set_item((0, 0), Py::new(py, ParamState::new(py))?)?;
        let mav = Mavlink::create(py, wire, None, source_system, source_component)?;
        mav.get().set_robust_parsing(true);
        let conn = Py::new(
            py,
            Connection {
                link: Arc::new(link),
                kind,
                state: Mutex::new(ConnState {
                    first_byte: true,
                    wire_version: wire.version_text().to_string(),
                    robust_parsing: true,
                    sysid: 0,
                    param_sysid: (0, 0),
                    last_seq: HashMap::new(),
                    mav_loss: 0,
                    mav_count: 0,
                    timestamp: 0.0,
                    uptime: 0.0,
                    notimestamps: false,
                    ts_override: None,
                    source_system,
                    source_component,
                    address,
                    start_time: frame::now(),
                    stop_on_eof: false,
                    portdead: false,
                    param_fetch_start: 0.0,
                }),
                refs: Mutex::new(ConnRefs {
                    mav: mav.clone_ref(py),
                    sysid_state: sysid_state.unbind(),
                    param_state: param_state.unbind(),
                    logfile: None,
                    logfile_raw: None,
                    message_hooks: PyList::empty(py).unbind(),
                    idle_hooks: PyList::empty(py).unbind(),
                }),
            },
        )?;
        lock(&mav.get().refs).file = Some(conn.clone_ref(py).into_any());
        Ok(conn)
    }

    fn mav<'py>(&self, py: Python<'py>) -> Bound<'py, Mavlink> {
        lock(&self.refs).mav.bind(py).clone()
    }

    fn sysid_state<'py>(&self, py: Python<'py>) -> Bound<'py, PyDict> {
        lock(&self.refs).sysid_state.bind(py).clone()
    }

    fn param_state_dict<'py>(&self, py: Python<'py>) -> Bound<'py, PyDict> {
        lock(&self.refs).param_state.bind(py).clone()
    }

    fn current_state<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let sysid = lock(&self.state).sysid;
        self.sysid_state(py)
            .get_item(sysid)?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(sysid))
    }

    fn ensure_param_state(&self, py: Python<'_>, key: (i64, i64)) -> PyResult<()> {
        let ps = self.param_state_dict(py);
        if !ps.contains(key)? {
            ps.set_item(key, Py::new(py, ParamState::new(py))?)?;
        }
        Ok(())
    }

    fn set_target_system_value(&self, py: Python<'_>, value: i64) -> PyResult<()> {
        let (param_sysid, changed) = {
            let mut s = lock(&self.state);
            s.sysid = value;
            let changed = value != s.param_sysid.0;
            if changed {
                s.param_sysid = (value, s.param_sysid.1);
            }
            (s.param_sysid, changed)
        };
        let states = self.sysid_state(py);
        if !states.contains(value)? {
            states.set_item(value, MavfileState::create(py)?)?;
        }
        if changed {
            self.ensure_param_state(py, param_sysid)?;
        }
        Ok(())
    }

    /// Non-blocking read of up to `n` bytes (a whole datagram on UDP).
    fn recv_bytes(&self, py: Python<'_>, n: Option<usize>) -> PyResult<Vec<u8>> {
        let n = match n {
            Some(n) => n,
            None => self.mav(py).get().bytes_needed_value(),
        };
        match &*self.link {
            Link::Udp(l) => l.recv().map_err(|e| os_error(&e)),
            Link::Mcast(l) => l.recv().map_err(|e| os_error(&e)),
            #[cfg(unix)]
            Link::Child(l) => Ok(l.recv()),
            Link::Log(f, _) => f.read(n).map_err(|e| os_error(&e)),
            #[cfg(unix)]
            Link::Uds(l) => {
                let (result, event) = l.recv(n);
                match event {
                    Some(TcpEvent::Disconnect) => {
                        py_print(py, "Connection reset or closed by peer on Unix domain socket");
                        self.reconnect_tcp(py)?;
                    }
                    Some(TcpEvent::Eof) => {
                        py_print(py, "EOF on Unix domain socket");
                        self.reconnect_tcp(py)?;
                    }
                    None => {}
                }
                result.map_err(|e| os_error(&e))
            }
            Link::TcpIn(l) => Ok(l.recv(n)),
            Link::Serial(l) => l.recv(n).map_err(|e| PyOSError::new_err(e.to_string())),
            Link::Tcp(l) => {
                let (result, event) = l.recv(n);
                match event {
                    Some(TcpEvent::Disconnect) => {
                        py_print(py, "Connection reset or closed by peer on TCP socket");
                        self.reconnect_tcp(py)?;
                    }
                    Some(TcpEvent::Eof) => {
                        py_print(py, "EOF on TCP socket");
                        self.reconnect_tcp(py)?;
                    }
                    None => {}
                }
                result.map_err(|e| os_error(&e))
            }
        }
    }

    fn reconnect_tcp(&self, py: Python<'_>) -> PyResult<()> {
        #[cfg(unix)]
        if let Link::Uds(uds) = &*self.link {
            if !uds.autoreconnect {
                return Ok(());
            }
            py_print(py, "Attempting reconnect");
            uds.close();
            let (path, retries) = (uds.path.clone(), uds.retries);
            let stream = py
                .allow_threads(|| Uds::connect(&path, retries, &mut |_| {}))
                .map_err(|e| os_error(&e))?;
            uds.replace(Some(stream));
            return Ok(());
        }
        let Link::Tcp(tcp) = &*self.link else {
            return Ok(());
        };
        if !tcp.autoreconnect {
            return Ok(());
        }
        py_print(py, "Attempting reconnect");
        tcp.close();
        let (host, port) = tcp.destination.clone();
        let (retries, delay) = (tcp.retries, tcp.reconnect_delay);
        let stream = py
            .allow_threads(|| Tcp::connect(&host, port, retries, delay, &mut |_| {}))
            .map_err(|e| os_error(&e))?;
        tcp.replace(Some(stream));
        Ok(())
    }

    fn write_bytes(&self, py: Python<'_>, buf: &[u8]) -> PyResult<Option<i64>> {
        match &*self.link {
            Link::Udp(l) => {
                py.allow_threads(|| l.write(buf));
                Ok(None)
            }
            Link::TcpIn(l) => {
                l.write(buf);
                Ok(None)
            }
            Link::Mcast(l) => {
                l.write(buf);
                Ok(None)
            }
            #[cfg(unix)]
            Link::Child(l) => {
                l.write(buf).map_err(|e| os_error(&e))?;
                Ok(None)
            }
            Link::Log(f, _) => {
                f.write(buf).map_err(|e| os_error(&e))?;
                Ok(None)
            }
            #[cfg(unix)]
            Link::Uds(l) => {
                if !l.is_open() {
                    let _ = self.reconnect_tcp(py);
                }
                if let Some(TcpEvent::Disconnect) = l.write(buf) {
                    py_print(py, "Connection reset or closed by peer on Unix domain socket");
                    let _ = self.reconnect_tcp(py);
                }
                Ok(None)
            }
            Link::Tcp(l) => {
                if !l.is_open() {
                    let _ = self.reconnect_tcp(py);
                }
                if let Some(TcpEvent::Disconnect) = l.write(buf) {
                    py_print(py, "Connection reset or closed by peer on TCP socket");
                    let _ = self.reconnect_tcp(py);
                }
                Ok(None)
            }
            Link::Serial(l) => match py.allow_threads(|| l.write(buf)) {
                Ok(n) => Ok(Some(n as i64)),
                Err(_) => {
                    let dead = std::mem::replace(&mut lock(&self.state).portdead, true);
                    if !dead {
                        py_print(py, &format!("Device {} is dead", l.device));
                    }
                    Ok(Some(-1))
                }
            },
        }
    }

    fn auto_version(&self, py: Python<'_>, buf: &[u8]) -> PyResult<()> {
        let Some(&magic) = buf.first() else {
            return Ok(());
        };
        if magic != 254 && magic != 253 {
            return Ok(());
        }
        let switch = {
            let mut s = lock(&self.state);
            s.first_byte = false;
            s.wire_version != "2.0" && magic == 253
        };
        if !switch {
            return Ok(());
        }
        lock(&self.state).wire_version = "2.0".to_string();
        dialect::switch_to_v2(py)?;
        let old = self.mav(py);
        let (src_system, src_component, robust) = {
            let s = lock(&self.state);
            (s.source_system, s.source_component, s.robust_parsing)
        };
        let file = old.get().file_obj(py);
        let new = Mavlink::create(py, Wire::V2, file, src_system, src_component)?;
        new.get().set_robust_parsing(robust);
        {
            let old_refs = lock(&old.get().refs);
            let mut new_refs = lock(&new.get().refs);
            new_refs.callback = old_refs.callback.as_ref().map(|c| c.clone_ref(py));
            new_refs.callback_args = old_refs.callback_args.as_ref().map(|c| c.clone_ref(py));
            new_refs.callback_kwargs = old_refs.callback_kwargs.as_ref().map(|c| c.clone_ref(py));
            new_refs.send_callback = old_refs.send_callback.as_ref().map(|c| c.clone_ref(py));
            new_refs.send_callback_args = old_refs.send_callback_args.as_ref().map(|c| c.clone_ref(py));
            new_refs.send_callback_kwargs = old_refs.send_callback_kwargs.as_ref().map(|c| c.clone_ref(py));
        }
        lock(&self.refs).mav = new;
        Ok(())
    }

    fn probably_vehicle(msg: &Bound<'_, PyAny>) -> PyResult<bool> {
        let comp: i64 = msg.call_method0("get_srcComponent")?.extract()?;
        if comp == constant_or("MAV_COMP_ID_GIMBAL", 154) {
            return Ok(false);
        }
        let t: i64 = msg.getattr("type")?.extract()?;
        let excluded = [
            constant_or("MAV_TYPE_GCS", 6),
            constant_or("MAV_TYPE_GIMBAL", 26),
            constant_or("MAV_TYPE_ADSB", 27),
            constant_or("MAV_TYPE_ONBOARD_CONTROLLER", 18),
        ];
        if excluded.contains(&t) {
            return Ok(false);
        }
        let autopilot: i64 = msg.getattr("autopilot")?.extract()?;
        Ok(autopilot != constant_or("MAV_AUTOPILOT_INVALID", 8))
    }

    fn add_message(messages: &Bound<'_, PyDict>, mtype: &str, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = msg.py();
        let field = msg.getattr("_instance_field")?;
        let value = if field.is_none() {
            None
        } else {
            msg.getattr(field.extract::<String>()?.as_str())
                .ok()
                .filter(|v| !v.is_none())
        };
        let Some(instance_value) = value else {
            messages.set_item(mtype, msg)?;
            return Ok(());
        };
        let copy = py.import("copy")?.getattr("copy")?;
        let key = format!("{}[{}]", mtype, instance_value.str()?);
        let existing = messages.get_item(mtype)?;
        let fresh = match &existing {
            None => true,
            Some(e) => e.getattr("_instances").map(|i| i.is_none()).unwrap_or(true),
        };
        if fresh {
            let head = copy.call1((msg,))?;
            let instances = PyDict::new(py);
            instances.set_item(&instance_value, msg)?;
            head.setattr("_instances", instances)?;
            messages.set_item(mtype, head)?;
            messages.set_item(key, copy.call1((msg,))?)?;
            return Ok(());
        }
        let prev = existing.unwrap().getattr("_instances")?;
        prev.set_item(&instance_value, msg)?;
        let head = copy.call1((msg,))?;
        head.setattr("_instances", prev)?;
        messages.set_item(mtype, head)?;
        messages.set_item(key, copy.call1((msg,))?)?;
        Ok(())
    }

    fn post(slf: &Bound<'_, Self>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        let dict = msg.getattr("__dict__")?;
        if dict.contains("_posted")? {
            return Ok(());
        }
        msg.setattr("_posted", true)?;
        let mut stamp = frame::now();
        if dict.contains("usec")? {
            let usec: f64 = msg.getattr("usec")?.extract()?;
            lock(&this.state).uptime = usec * 1.0e-6;
        }
        if dict.contains("time_boot_ms")? {
            let ms: f64 = msg.getattr("time_boot_ms")?.extract()?;
            lock(&this.state).uptime = ms * 1.0e-3;
        }
        {
            let s = lock(&this.state);
            if let Some(ts) = s.ts_override {
                stamp = if s.notimestamps { s.uptime } else { ts };
            }
        }
        msg.setattr("_timestamp", stamp)?;

        let src_system: i64 = msg.call_method0("get_srcSystem")?.extract()?;
        let src_component: i64 = msg.call_method0("get_srcComponent")?.extract()?;
        let states = this.sysid_state(py);
        if !states.contains(src_system)? {
            states.set_item(src_system, MavfileState::create(py)?)?;
        }
        let m_type: String = msg.call_method0("get_type")?.extract()?;
        let state = states.get_item(src_system)?.unwrap();
        let messages = state.getattr("messages")?.downcast_into::<PyDict>()?;
        Self::add_message(&messages, &m_type, msg)?;

        let radio = src_system == b'3' as i64 && src_component == b'D' as i64;
        if radio {
            for (_, s) in states.iter() {
                s.getattr("messages")?.set_item(&m_type, msg)?;
            }
        }
        let msg_id: i64 = msg.call_method0("get_msgId")?.extract()?;
        if !(radio || msg_id < 0) {
            let seq2: i64 = msg.call_method0("get_seq")?.extract()?;
            let mut s = lock(&this.state);
            let last = s.last_seq.get(&(src_system, src_component)).copied().unwrap_or(-1);
            let seq = (last + 1).rem_euclid(256);
            if seq != seq2 && last != -1 {
                s.mav_loss += (seq2 - seq).rem_euclid(256);
            }
            s.last_seq.insert((src_system, src_component), seq2);
            s.mav_count += 1;
        }
        lock(&this.state).timestamp = stamp;

        if m_type == "HEARTBEAT" && Self::probably_vehicle(msg)? {
            {
                let mut s = lock(&this.state);
                if s.sysid == 0 {
                    s.sysid = src_system;
                }
            }
            state.setattr("flightmode", modes::mode_string_v10(msg)?)?;
            let base_mode = msg.getattr("base_mode")?;
            state.setattr(
                "armed",
                base_mode.call_method1("__and__", (constant_or("MAV_MODE_FLAG_SAFETY_ARMED", 128),))?,
            )?;
            state.setattr("mav_type", msg.getattr("type")?)?;
            state.setattr("mav_autopilot", msg.getattr("autopilot")?)?;
        } else if m_type == "HIGH_LATENCY2" {
            {
                let mut s = lock(&this.state);
                if s.sysid == 0 {
                    s.sysid = src_system;
                }
            }
            let autopilot: i64 = msg.getattr("autopilot")?.extract()?;
            if autopilot == constant_or("MAV_AUTOPILOT_ARDUPILOTMEGA", 3) {
                let custom0 = msg.getattr("custom0")?;
                state.setattr("base_mode", &custom0)?;
                state.setattr(
                    "armed",
                    custom0.call_method1("__and__", (constant_or("MAV_MODE_FLAG_SAFETY_ARMED", 128),))?,
                )?;
            }
            state.setattr("flightmode", modes::mode_string_v10(msg)?)?;
            state.setattr("mav_type", msg.getattr("type")?)?;
            state.setattr("mav_autopilot", msg.getattr("autopilot")?)?;
        } else if m_type == "PARAM_VALUE" {
            this.ensure_param_state(py, (src_system, src_component))?;
            let ps = this
                .param_state_dict(py)
                .get_item((src_system, src_component))?
                .unwrap();
            ps.getattr("params")?
                .set_item(msg.getattr("param_id")?, msg.getattr("param_value")?)?;
        } else if m_type == "GPS_RAW_INT" {
            let home = messages.get_item("HOME")?;
            let replace = match home {
                Some(h) => h.getattr("fix_type")?.lt(3)?,
                None => true,
            };
            if replace {
                messages.set_item("HOME", msg)?;
            }
        }
        let hooks = lock(&this.refs).message_hooks.clone_ref(py);
        for hook in hooks.bind(py).iter() {
            hook.call1((slf.clone(), msg.clone()))?;
        }

        if msg.call_method0("get_signed")?.is_truthy()? {
            let signing = this.mav(py).get().signing(py);
            let link_id = lock(&signing.get().state).link_id;
            let incoming: Option<i64> = msg.call_method0("get_link_id")?.extract()?;
            let (target_system, target_component) = {
                let s = lock(&this.state);
                (s.sysid, s.param_sysid.1)
            };
            if link_id == 0
                && incoming.unwrap_or(0) != 0
                && target_system == src_system
                && target_component == src_component
            {
                lock(&signing.get().state).link_id = incoming.unwrap_or(0);
            }
        }
        Ok(())
    }

    fn recv_msg_inner<'py>(slf: &Bound<'py, Self>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let py = slf.py();
        let this = slf.get();
        this.log_pre_message(py)?;
        if this.link.is_datagram() {
            let s = this.recv_bytes(py, None)?;
            if !s.is_empty() && lock(&this.state).first_byte {
                this.auto_version(py, &s)?;
            }
            let mav = this.mav(py);
            let m = Mavlink::parse_bytes(&mav, &s)?;
            if let Some(m) = &m {
                Self::post(slf, m)?;
            }
            return Ok(m);
        }
        loop {
            let n = this.mav(py).get().bytes_needed_value();
            let s = this.recv_bytes(py, Some(n))?;
            if !s.is_empty() {
                let raw = lock(&this.refs).logfile_raw.as_ref().map(|f| f.clone_ref(py));
                if let Some(f) = raw {
                    f.call_method1(py, "write", (PyBytes::new(py, &s),))?;
                }
                if lock(&this.state).first_byte {
                    this.auto_version(py, &s)?;
                }
            }
            let mav = this.mav(py);
            let m = Mavlink::parse_bytes(&mav, &s)?;
            match m {
                Some(m) => {
                    let log = lock(&this.refs).logfile.as_ref().map(|f| f.clone_ref(py));
                    if let Some(f) = log {
                        let t: String = m.call_method0("get_type")?.extract()?;
                        if t != "BAD_DATA" {
                            let usec = ((frame::now() * 1.0e6) as u64) & !3u64;
                            let mut data = usec.to_be_bytes().to_vec();
                            data.extend(m.call_method0("get_msgbuf")?.extract::<Vec<u8>>()?);
                            f.call_method1(py, "write", (PyBytes::new(py, &data),))?;
                        }
                    }
                    Self::post(slf, &m)?;
                    this.log_post_message(py, &m)?;
                    return Ok(Some(m));
                }
                None if s.is_empty() => return Ok(None),
                None => {}
            }
        }
    }

    /// `mavlogfile.pre_message`: the stamp in front of the next frame.
    fn log_pre_message(&self, py: Python<'_>) -> PyResult<()> {
        let Link::Log(file, log) = &*self.link else {
            return Ok(());
        };
        let mut st = lock(log);
        if file.filesize != 0 {
            st.percent = (100.0 * file.tell() as f64) / file.filesize as f64;
        }
        if lock(&self.state).notimestamps {
            return Ok(());
        }
        let t;
        if st.planner_format {
            // pymavlink compares bytes with str here and so rejects every
            // planner stamp on Python 3; this reads the format as specified.
            let tbuf = file.read(21).map_err(|e| os_error(&e))?;
            if tbuf.len() != 21 || tbuf[0] != b'-' || tbuf[20] != b':' {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "bad planner timestamp {}",
                    PyBytes::new(py, &tbuf).repr()?
                )));
            }
            let text = String::from_utf8_lossy(&tbuf[0..20]).to_string();
            let value: f64 = text
                .trim()
                .parse()
                .map_err(|_| PyValueError::new_err(format!("could not convert string to float: '{text}'")))?;
            let hnsec = 2f64.powi(63) + value;
            t = hnsec * 1.0e-7 - 719163.0 * 24.0 * 60.0 * 60.0;
            st.link = 0;
        } else {
            let tbuf = file.read(8).map_err(|e| os_error(&e))?;
            if tbuf.len() != 8 {
                return Ok(());
            }
            let mut tusec = u64::from_be_bytes(tbuf[..8].try_into().unwrap());
            let mut stamp = tusec as f64 * 1.0e-6;
            if let Some(last) = st.last_timestamp {
                if st.last_was_bad && (stamp - last).abs() > 3.0 * 24.0 * 3600.0 {
                    // scan_timestamp: slide a byte at a time to a stamp in range.
                    let mut window = tbuf.clone();
                    loop {
                        tusec = u64::from_be_bytes(window[..8].try_into().unwrap());
                        stamp = tusec as f64 * 1.0e-6;
                        if (stamp - last).abs() <= 3.0 * 24.0 * 3600.0 {
                            break;
                        }
                        let c = file.read(1).map_err(|e| os_error(&e))?;
                        if c.len() != 1 {
                            break;
                        }
                        window.remove(0);
                        window.push(c[0]);
                    }
                }
            }
            t = stamp;
            st.link = (tusec & 0x3) as i64;
        }
        lock(&self.state).ts_override = Some(t);
        Ok(())
    }

    /// `mavlogfile.post_message`: remember the stamp and tag the link id.
    fn log_post_message(&self, py: Python<'_>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let Link::Log(file, log) = &*self.link else {
            return Ok(());
        };
        let mut st = lock(log);
        if st.planner_format {
            file.read(1).map_err(|e| os_error(&e))?;
        }
        let stamp: f64 = msg.getattr("_timestamp")?.extract()?;
        lock(&self.state).timestamp = stamp;
        let bad = msg.call_method0("get_type")?.extract::<String>()? == "BAD_DATA";
        st.last_was_bad = bad;
        if !bad {
            st.last_timestamp = Some(stamp);
        }
        msg.setattr("_link", st.link)?;
        let _ = py;
        Ok(())
    }

    fn select_inner(&self, py: Python<'_>, timeout: f64) -> bool {
        let link = Arc::clone(&self.link);
        py.allow_threads(move || link.wait_readable(timeout))
    }

    fn command_long(slf: &Bound<'_, Self>, command: &str, params: [f64; 7]) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        let (ts, tc) = {
            let s = lock(&this.state);
            (s.sysid, s.param_sysid.1)
        };
        let cmd = constant_or(command, 0);
        let mav = this.mav(py);
        let args = (
            ts, tc, cmd, 0, params[0], params[1], params[2], params[3], params[4], params[5], params[6],
        );
        mav.as_any().getattr("command_long_send")?.call1(args)?;
        Ok(())
    }
}

#[pymethods]
impl Connection {
    #[getter(sysid)]
    fn get_sysid(&self) -> i64 {
        lock(&self.state).sysid
    }

    #[setter(sysid)]
    fn set_sysid(&self, value: i64) {
        lock(&self.state).sysid = value;
    }
    #[getter(first_byte)]
    fn get_first_byte(&self) -> bool {
        lock(&self.state).first_byte
    }

    #[setter(first_byte)]
    fn set_first_byte(&self, value: bool) {
        lock(&self.state).first_byte = value;
    }
    #[getter(WIRE_PROTOCOL_VERSION)]
    fn get_wire(&self) -> String {
        lock(&self.state).wire_version.clone()
    }

    #[setter(WIRE_PROTOCOL_VERSION)]
    fn set_wire(&self, value: String) {
        lock(&self.state).wire_version = value;
    }
    #[getter(mav_loss)]
    fn get_mav_loss(&self) -> i64 {
        lock(&self.state).mav_loss
    }

    #[setter(mav_loss)]
    fn set_mav_loss(&self, value: i64) {
        lock(&self.state).mav_loss = value;
    }
    #[getter(mav_count)]
    fn get_mav_count(&self) -> i64 {
        lock(&self.state).mav_count
    }

    #[setter(mav_count)]
    fn set_mav_count(&self, value: i64) {
        lock(&self.state).mav_count = value;
    }
    #[getter(timestamp)]
    fn get_timestamp(&self) -> f64 {
        lock(&self.state).timestamp
    }

    #[setter(timestamp)]
    fn set_timestamp(&self, value: f64) {
        lock(&self.state).timestamp = value;
    }
    #[getter(uptime)]
    fn get_uptime(&self) -> f64 {
        lock(&self.state).uptime
    }

    #[setter(uptime)]
    fn set_uptime(&self, value: f64) {
        lock(&self.state).uptime = value;
    }
    #[getter(notimestamps)]
    fn get_notimestamps(&self) -> bool {
        lock(&self.state).notimestamps
    }

    #[setter(notimestamps)]
    fn set_notimestamps(&self, value: bool) {
        lock(&self.state).notimestamps = value;
    }
    #[getter(source_system)]
    fn get_source_system(&self) -> i64 {
        lock(&self.state).source_system
    }

    #[setter(source_system)]
    fn set_source_system(&self, value: i64) {
        lock(&self.state).source_system = value;
    }
    #[getter(source_component)]
    fn get_source_component(&self) -> i64 {
        lock(&self.state).source_component
    }

    #[setter(source_component)]
    fn set_source_component(&self, value: i64) {
        lock(&self.state).source_component = value;
    }
    #[getter(address)]
    fn get_address(&self) -> String {
        lock(&self.state).address.clone()
    }

    #[setter(address)]
    fn set_address(&self, value: String) {
        lock(&self.state).address = value;
    }
    #[getter(start_time)]
    fn get_start_time(&self) -> f64 {
        lock(&self.state).start_time
    }

    #[setter(start_time)]
    fn set_start_time(&self, value: f64) {
        lock(&self.state).start_time = value;
    }
    #[getter(stop_on_EOF)]
    fn get_stop_on_eof(&self) -> bool {
        lock(&self.state).stop_on_eof
    }

    #[setter(stop_on_EOF)]
    fn set_stop_on_eof(&self, value: bool) {
        lock(&self.state).stop_on_eof = value;
    }
    #[getter(portdead)]
    fn get_portdead(&self) -> bool {
        lock(&self.state).portdead
    }

    #[setter(portdead)]
    fn set_portdead(&self, value: bool) {
        lock(&self.state).portdead = value;
    }
    #[getter(param_fetch_start)]
    fn get_param_fetch_start(&self) -> f64 {
        lock(&self.state).param_fetch_start
    }

    #[setter(param_fetch_start)]
    fn set_param_fetch_start(&self, value: f64) {
        lock(&self.state).param_fetch_start = value;
    }

    #[getter(robust_parsing)]
    fn get_robust_parsing(&self) -> bool {
        lock(&self.state).robust_parsing
    }

    #[setter(robust_parsing)]
    fn set_robust_parsing(&self, py: Python<'_>, value: bool) {
        lock(&self.state).robust_parsing = value;
        self.mav(py).get().set_robust_parsing(value);
    }

    #[getter(_timestamp)]
    fn get_ts_override(&self) -> Option<f64> {
        lock(&self.state).ts_override
    }

    #[setter(_timestamp)]
    fn set_ts_override(&self, value: Option<f64>) {
        lock(&self.state).ts_override = value;
    }

    #[getter]
    fn target_system(&self) -> i64 {
        lock(&self.state).sysid
    }

    #[setter]
    fn set_target_system(&self, py: Python<'_>, value: i64) -> PyResult<()> {
        self.set_target_system_value(py, value)
    }

    #[getter]
    fn target_component(&self) -> i64 {
        lock(&self.state).param_sysid.1
    }

    #[setter]
    fn set_target_component(&self, py: Python<'_>, value: i64) -> PyResult<()> {
        let key = {
            let mut s = lock(&self.state);
            if value == s.param_sysid.1 {
                return Ok(());
            }
            s.param_sysid = (s.param_sysid.0, value);
            s.param_sysid
        };
        self.ensure_param_state(py, key)
    }

    #[getter]
    fn param_sysid(&self) -> (i64, i64) {
        lock(&self.state).param_sysid
    }

    #[setter]
    fn set_param_sysid(&self, value: (i64, i64)) {
        lock(&self.state).param_sysid = value;
    }

    #[getter]
    fn last_seq<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        for (k, v) in lock(&self.state).last_seq.iter() {
            d.set_item(*k, *v)?;
        }
        Ok(d)
    }

    #[getter(sysid_state)]
    fn get_sysid_state<'py>(&self, py: Python<'py>) -> Bound<'py, PyDict> {
        self.sysid_state(py)
    }

    #[setter(sysid_state)]
    fn set_sysid_state(&self, value: Py<PyDict>) {
        lock(&self.refs).sysid_state = value;
    }

    #[getter(param_state)]
    fn get_param_state<'py>(&self, py: Python<'py>) -> Bound<'py, PyDict> {
        self.param_state_dict(py)
    }

    #[setter(param_state)]
    fn set_param_state(&self, value: Py<PyDict>) {
        lock(&self.refs).param_state = value;
    }

    #[getter(mav)]
    fn get_mav<'py>(&self, py: Python<'py>) -> Bound<'py, Mavlink> {
        self.mav(py)
    }

    #[setter(mav)]
    fn set_mav(&self, value: Py<Mavlink>) {
        lock(&self.refs).mav = value;
    }

    #[getter]
    fn logfile(&self, py: Python<'_>) -> PyObject {
        lock(&self.refs)
            .logfile
            .as_ref()
            .map(|f| f.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }

    #[setter]
    fn set_logfile(&self, py: Python<'_>, value: PyObject) {
        lock(&self.refs).logfile = if value.is_none(py) { None } else { Some(value) };
    }

    #[getter]
    fn logfile_raw(&self, py: Python<'_>) -> PyObject {
        lock(&self.refs)
            .logfile_raw
            .as_ref()
            .map(|f| f.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }

    #[setter]
    fn set_logfile_raw(&self, py: Python<'_>, value: PyObject) {
        lock(&self.refs).logfile_raw = if value.is_none(py) { None } else { Some(value) };
    }

    #[getter]
    fn message_hooks<'py>(&self, py: Python<'py>) -> Bound<'py, PyList> {
        lock(&self.refs).message_hooks.bind(py).clone()
    }

    #[setter]
    fn set_message_hooks(&self, value: Py<PyList>) {
        lock(&self.refs).message_hooks = value;
    }

    #[getter]
    fn idle_hooks<'py>(&self, py: Python<'py>) -> Bound<'py, PyList> {
        lock(&self.refs).idle_hooks.bind(py).clone()
    }

    #[setter]
    fn set_idle_hooks(&self, value: Py<PyList>) {
        lock(&self.refs).idle_hooks = value;
    }

    #[getter]
    fn params<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let (sysid, param_sysid) = {
            let s = lock(&self.state);
            (s.sysid, s.param_sysid)
        };
        let ps = self.param_state_dict(py);
        if param_sysid.1 == 0 {
            if let Some(state) = ps.get_item((sysid, 1))? {
                return state.getattr("params");
            }
        }
        ps.get_item(param_sysid)?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(param_sysid))?
            .getattr("params")
    }

    #[getter]
    fn messages<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.current_state(py)?.getattr("messages")
    }

    #[getter]
    fn flightmode<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.current_state(py)?.getattr("flightmode")
    }

    #[setter]
    fn set_flightmode(&self, py: Python<'_>, value: PyObject) -> PyResult<()> {
        self.current_state(py)?.setattr("flightmode", value)
    }

    #[getter]
    fn vehicle_type<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.current_state(py)?.getattr("vehicle_type")
    }

    #[setter]
    fn set_vehicle_type(&self, py: Python<'_>, value: PyObject) -> PyResult<()> {
        self.current_state(py)?.setattr("vehicle_type", value)
    }

    #[getter]
    fn mav_type<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.current_state(py)?.getattr("mav_type")
    }

    #[setter]
    fn set_mav_type(&self, py: Python<'_>, value: PyObject) -> PyResult<()> {
        self.current_state(py)?.setattr("mav_type", value)
    }

    #[getter]
    fn base_mode<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.current_state(py)?.getattr("base_mode")
    }

    #[setter]
    fn set_base_mode(&self, py: Python<'_>, value: PyObject) -> PyResult<()> {
        self.current_state(py)?.setattr("base_mode", value)
    }

    #[getter]
    fn fd(&self) -> Option<i64> {
        self.link.fileno()
    }

    #[getter]
    fn port(&self, py: Python<'_>) -> PyResult<PyObject> {
        if let Link::Log(..) = &*self.link {
            return Err(PyAttributeError::new_err(format!(
                "'{}' object has no attribute 'port'",
                self.kind
            )));
        }
        if let Link::TcpIn(l) = &*self.link {
            if !l.has_client() {
                return Ok(py.None());
            }
        }
        Ok(Py::new(
            py,
            PortHandle {
                link: Arc::clone(&self.link),
            },
        )?
        .into_any())
    }

    #[getter]
    fn udp_server(&self) -> PyResult<bool> {
        match &*self.link {
            Link::Udp(l) => Ok(l.server),
            _ => Err(PyAttributeError::new_err(format!(
                "'{}' object has no attribute 'udp_server'",
                self.kind
            ))),
        }
    }

    #[getter]
    fn destination_addr(&self, py: Python<'_>) -> PyResult<PyObject> {
        match &*self.link {
            Link::Udp(l) => match l.destination() {
                Some(d) => Ok(d.into_pyobject(py)?.into_any().unbind()),
                None => Err(PyAttributeError::new_err(
                    "'mavudp' object has no attribute 'destination_addr'",
                )),
            },
            Link::Tcp(l) => Ok(l.destination.clone().into_pyobject(py)?.into_any().unbind()),
            _ => Err(PyAttributeError::new_err(format!(
                "'{}' object has no attribute 'destination_addr'",
                self.kind
            ))),
        }
    }

    #[getter]
    fn clients<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PySet>> {
        let Link::Udp(l) = &*self.link else {
            return Err(PyAttributeError::new_err(format!(
                "'{}' object has no attribute 'clients'",
                self.kind
            )));
        };
        let items: Vec<(String, u16)> = l.clients().iter().map(|a| (a.ip().to_string(), a.port())).collect();
        pyo3::types::PySet::new(py, items)
    }

    #[getter]
    fn device(&self) -> PyResult<String> {
        match &*self.link {
            Link::Serial(l) => Ok(l.device.clone()),
            _ => Err(PyAttributeError::new_err(format!(
                "'{}' object has no attribute 'device'",
                self.kind
            ))),
        }
    }

    #[getter]
    fn baud(&self) -> PyResult<u32> {
        match &*self.link {
            Link::Serial(l) => Ok(l.baud),
            _ => Err(PyAttributeError::new_err(format!(
                "'{}' object has no attribute 'baud'",
                self.kind
            ))),
        }
    }

    #[getter]
    fn autoreconnect(&self) -> bool {
        matches!(&*self.link, Link::Tcp(l) if l.autoreconnect)
    }

    #[getter]
    fn percent(&self) -> PyResult<f64> {
        match &*self.link {
            Link::Log(_, st) => Ok(lock(st).percent),
            _ => Err(PyAttributeError::new_err(format!(
                "'{}' object has no attribute 'percent'",
                self.kind
            ))),
        }
    }

    #[getter]
    fn filename(&self) -> PyResult<String> {
        match &*self.link {
            Link::Log(f, _) => Ok(f.filename.clone()),
            _ => Err(PyAttributeError::new_err(format!(
                "'{}' object has no attribute 'filename'",
                self.kind
            ))),
        }
    }

    #[getter]
    fn filesize(&self) -> PyResult<u64> {
        match &*self.link {
            Link::Log(f, _) => Ok(f.filesize),
            _ => Err(PyAttributeError::new_err(format!(
                "'{}' object has no attribute 'filesize'",
                self.kind
            ))),
        }
    }

    #[getter(__class_name__)]
    fn class_name(&self) -> &'static str {
        self.kind
    }

    #[pyo3(signature = (n=None))]
    fn recv<'py>(&self, py: Python<'py>, n: Option<usize>) -> PyResult<Bound<'py, PyBytes>> {
        Ok(PyBytes::new(py, &self.recv_bytes(py, n)?))
    }

    fn write(&self, py: Python<'_>, buf: &Bound<'_, PyAny>) -> PyResult<Option<i64>> {
        let data = frame::bytes_arg(buf)?;
        self.write_bytes(py, &data)
    }

    fn close(&self) {
        self.link.close();
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[pyo3(signature = (*_args))]
    fn __exit__(&self, _args: &Bound<'_, PyTuple>) -> bool {
        self.close();
        false
    }

    fn select(&self, py: Python<'_>, timeout: f64) -> bool {
        self.select_inner(py, timeout)
    }

    fn pre_message(&self) {}

    fn set_rtscts(&self, enable: bool) {
        if let Link::Serial(l) = &*self.link {
            l.set_rtscts(enable);
        }
    }

    fn set_baudrate(&self, baudrate: u32) -> PyResult<()> {
        if let Link::Serial(l) = &*self.link {
            l.set_baudrate(baudrate).map_err(|e| os_error(&e))?;
        }
        Ok(())
    }

    fn auto_mavlink_version(&self, py: Python<'_>, buf: &Bound<'_, PyAny>) -> PyResult<()> {
        let data = frame::bytes_arg(buf)?;
        self.auto_version(py, &data)
    }

    fn probably_vehicle_heartbeat(&self, msg: &Bound<'_, PyAny>) -> PyResult<bool> {
        Self::probably_vehicle(msg)
    }

    fn post_message(slf: &Bound<'_, Self>, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        Self::post(slf, msg)
    }

    fn packet_loss(&self) -> f64 {
        let s = lock(&self.state);
        if s.mav_count == 0 {
            return 0.0;
        }
        (100.0 * s.mav_loss as f64) / ((s.mav_count + s.mav_loss) as f64)
    }

    fn recv_msg<'py>(slf: &Bound<'py, Self>) -> PyResult<Option<Bound<'py, PyAny>>> {
        Self::recv_msg_inner(slf)
    }

    #[pyo3(signature = (condition=None, r#type=None, blocking=false, timeout=None))]
    fn recv_match<'py>(
        slf: &Bound<'py, Self>,
        condition: Option<&Bound<'py, PyAny>>,
        r#type: Option<&Bound<'py, PyAny>>,
        blocking: bool,
        timeout: Option<f64>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let py = slf.py();
        let this = slf.get();
        if condition.is_some_and(|c| !c.is_none()) {
            return Err(PyNotImplementedError::new_err(
                "corvus_link does not evaluate recv_match conditions",
            ));
        }
        let types: Option<Vec<String>> = match r#type {
            None => None,
            Some(t) if t.is_none() => None,
            Some(t) if t.is_instance_of::<PyString>() => Some(vec![t.extract()?]),
            Some(t) if t.is_instance_of::<PyList>() || t.is_instance_of::<pyo3::types::PySet>() => Some(
                t.try_iter()?
                    .map(|i| i.and_then(|v| v.extract::<String>()))
                    .collect::<PyResult<_>>()?,
            ),
            Some(t) => Some(vec![t.extract()?]),
        };
        let mut start = frame::now();
        loop {
            if let Some(timeout) = timeout {
                let now = frame::now();
                if now < start {
                    start = now;
                }
                if start + timeout < frame::now() {
                    return Ok(None);
                }
            }
            let m = Self::recv_msg_inner(slf)?;
            let Some(m) = m else {
                if blocking {
                    let hooks = lock(&this.refs).idle_hooks.clone_ref(py);
                    for hook in hooks.bind(py).iter() {
                        hook.call1((slf.clone(),))?;
                    }
                    let wait = match timeout {
                        None => 0.05,
                        Some(t) => t / 2.0,
                    };
                    this.select_inner(py, wait);
                    continue;
                }
                return Ok(None);
            };
            if let Some(types) = &types {
                let t: String = m.call_method0("get_type")?.extract()?;
                if !types.contains(&t) {
                    continue;
                }
            }
            let src: i64 = m.call_method0("get_srcSystem")?.extract()?;
            if !this.sysid_state(py).contains(src)? {
                continue;
            }
            return Ok(Some(m));
        }
    }

    #[pyo3(signature = (blocking=true, timeout=None))]
    fn wait_heartbeat<'py>(
        slf: &Bound<'py, Self>,
        blocking: bool,
        timeout: Option<f64>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let t = PyString::new(slf.py(), "HEARTBEAT");
        Self::recv_match(slf, None, Some(t.as_any()), blocking, timeout)
    }

    fn check_condition(&self, condition: Option<&Bound<'_, PyAny>>) -> PyResult<bool> {
        match condition {
            None => Ok(true),
            Some(c) if c.is_none() => Ok(true),
            Some(_) => Err(PyNotImplementedError::new_err(
                "corvus_link does not evaluate recv_match conditions",
            )),
        }
    }

    fn mavlink10(&self) -> bool {
        true
    }

    fn mavlink20(&self) -> bool {
        lock(&self.state).wire_version.parse::<f64>().unwrap_or(1.0) >= 2.0
    }

    #[pyo3(signature = (logfile, mode="wb"))]
    fn setup_logfile(&self, py: Python<'_>, logfile: &Bound<'_, PyAny>, mode: &str) -> PyResult<()> {
        let f = py.import("builtins")?.call_method1("open", (logfile, mode))?;
        lock(&self.refs).logfile = Some(f.unbind());
        Ok(())
    }

    #[pyo3(signature = (logfile, mode="wb"))]
    fn setup_logfile_raw(&self, py: Python<'_>, logfile: &Bound<'_, PyAny>, mode: &str) -> PyResult<()> {
        let f = py.import("builtins")?.call_method1("open", (logfile, mode))?;
        lock(&self.refs).logfile_raw = Some(f.unbind());
        Ok(())
    }

    fn mode_mapping(&self, py: Python<'_>) -> PyResult<PyObject> {
        let state = self.current_state(py)?;
        let autopilot = state.getattr("mav_autopilot")?;
        if !autopilot.is_none() && autopilot.extract::<i64>().ok() == Some(constant_or("MAV_AUTOPILOT_PX4", 12)) {
            return Ok(modes::px4_map(py)?.into_any().unbind());
        }
        let mav_type = state.getattr("mav_type")?;
        if mav_type.is_none() {
            return Ok(py.None());
        }
        mode_mapping_byname(py, mav_type.extract().unwrap_or(-1))
    }

    #[pyo3(signature = (secret_key, sign_outgoing=true, allow_unsigned_callback=None, initial_timestamp=None, link_id=None))]
    fn setup_signing(
        &self,
        py: Python<'_>,
        secret_key: &Bound<'_, PyAny>,
        sign_outgoing: bool,
        allow_unsigned_callback: Option<PyObject>,
        initial_timestamp: Option<u64>,
        link_id: Option<i64>,
    ) -> PyResult<()> {
        let signing = self.mav(py).get().signing(py);
        let key = if secret_key.is_none() {
            None
        } else {
            Some(crate::wire::crc::bytes_of(secret_key)?)
        };
        let link_id = match link_id {
            Some(v) => v,
            None => {
                let v = GLOBAL_LINK_ID.load(Ordering::SeqCst);
                GLOBAL_LINK_ID.store((v + 1).min(255), Ordering::SeqCst);
                v
            }
        };
        let timestamp = match initial_timestamp {
            Some(t) => t,
            None => {
                let epoch_offset = 1420070400.0;
                let now = frame::now().max(epoch_offset);
                ((now - epoch_offset) * 100.0 * 1000.0) as u64
            }
        };
        {
            let mut s = lock(&signing.get().state);
            s.secret_key = key;
            s.sign_outgoing = sign_outgoing;
            s.link_id = link_id;
            s.timestamp = timestamp;
        }
        *lock(&signing.get().allow_unsigned_callback) = allow_unsigned_callback.filter(|c| !c.is_none(py));
        Ok(())
    }

    fn disable_signing(&self, py: Python<'_>) {
        let signing = self.mav(py).get().signing(py);
        let mut s = lock(&signing.get().state);
        s.secret_key = None;
        s.sign_outgoing = false;
        s.link_id = 0;
        s.timestamp = 0;
        drop(s);
        *lock(&signing.get().allow_unsigned_callback) = None;
    }

    #[pyo3(signature = (r#type, field, default=None))]
    fn field<'py>(
        &self,
        py: Python<'py>,
        r#type: &str,
        field: &str,
        default: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let default = default.unwrap_or_else(|| py.None().into_bound(py));
        let messages = self.messages(py)?;
        match messages.get_item(r#type) {
            Ok(m) => Ok(m.getattr(field).unwrap_or(default)),
            Err(_) => Ok(default),
        }
    }

    #[pyo3(signature = (name, default=None))]
    fn param<'py>(
        &self,
        py: Python<'py>,
        name: &Bound<'py, PyAny>,
        default: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let params = self.params(py)?;
        if params.contains(name)? {
            return params.get_item(name);
        }
        Ok(default.unwrap_or_else(|| py.None().into_bound(py)))
    }

    fn time_since(&self, py: Python<'_>, mtype: &str) -> PyResult<f64> {
        let messages = self.messages(py)?;
        match messages.get_item(mtype) {
            Ok(m) => Ok(frame::now() - m.getattr("_timestamp")?.extract::<f64>()?),
            Err(_) => Ok(frame::now() - lock(&self.state).start_time),
        }
    }

    fn param_fetch_all(slf: &Bound<'_, Self>) -> PyResult<()> {
        let this = slf.get();
        let now = frame::now();
        {
            let mut s = lock(&this.state);
            if now - s.param_fetch_start < 2.0 {
                return Ok(());
            }
            s.param_fetch_start = now;
        }
        let (ts, tc) = (this.target_system(), this.target_component());
        this.mav(slf.py())
            .as_any()
            .getattr("param_request_list_send")?
            .call1((ts, tc))?;
        Ok(())
    }

    fn param_fetch_one(slf: &Bound<'_, Self>, name: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        let (ts, tc) = (this.target_system(), this.target_component());
        let mav = this.mav(py);
        let sender = mav.as_any().getattr("param_request_read_send")?;
        let index = py.import("builtins")?.getattr("int")?.call1((name,));
        if let Ok(idx) = index {
            if sender.call1((ts, tc, PyBytes::new(py, b""), idx)).is_ok() {
                return Ok(());
            }
        }
        let name = if name.is_instance_of::<PyBytes>() {
            name.clone()
        } else {
            PyBytes::new(py, name.str()?.to_str()?.as_bytes()).into_any()
        };
        sender.call1((ts, tc, name, -1))?;
        Ok(())
    }

    #[pyo3(signature = (parm_name, parm_value, parm_type=None))]
    fn param_set_send(
        slf: &Bound<'_, Self>,
        parm_name: &str,
        parm_value: &Bound<'_, PyAny>,
        parm_type: Option<i64>,
    ) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        let parm_type = parm_type.unwrap_or(9);
        let (ts, tc) = (this.target_system(), this.target_component());
        this.mav(py).as_any().getattr("param_set_send")?.call1((
            ts,
            tc,
            PyBytes::new(py, parm_name.as_bytes()),
            parm_value,
            parm_type,
        ))?;
        Ok(())
    }

    fn waypoint_request_list_send(slf: &Bound<'_, Self>) -> PyResult<()> {
        let this = slf.get();
        this.mav(slf.py())
            .as_any()
            .getattr("mission_request_list_send")?
            .call1((this.target_system(), this.target_component()))?;
        Ok(())
    }

    fn waypoint_clear_all_send(slf: &Bound<'_, Self>) -> PyResult<()> {
        let this = slf.get();
        this.mav(slf.py())
            .as_any()
            .getattr("mission_clear_all_send")?
            .call1((this.target_system(), this.target_component()))?;
        Ok(())
    }

    fn waypoint_request_send(slf: &Bound<'_, Self>, seq: i64) -> PyResult<()> {
        let this = slf.get();
        this.mav(slf.py()).as_any().getattr("mission_request_send")?.call1((
            this.target_system(),
            this.target_component(),
            seq,
        ))?;
        Ok(())
    }

    fn waypoint_set_current_send(slf: &Bound<'_, Self>, seq: i64) -> PyResult<()> {
        let this = slf.get();
        this.mav(slf.py())
            .as_any()
            .getattr("mission_set_current_send")?
            .call1((this.target_system(), this.target_component(), seq))?;
        Ok(())
    }

    fn waypoint_count_send(slf: &Bound<'_, Self>, seq: i64) -> PyResult<()> {
        let this = slf.get();
        this.mav(slf.py()).as_any().getattr("mission_count_send")?.call1((
            this.target_system(),
            this.target_component(),
            seq,
        ))?;
        Ok(())
    }

    fn set_mode_flag(slf: &Bound<'_, Self>, flag: i64, enable: bool) -> PyResult<()> {
        let base: i64 = slf.get().base_mode(slf.py())?.extract()?;
        let mode = if enable { base | flag } else { base & !flag };
        Self::command_long(slf, "MAV_CMD_DO_SET_MODE", [mode as f64, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])
    }

    fn set_mode_auto(slf: &Bound<'_, Self>) -> PyResult<()> {
        Self::command_long(slf, "MAV_CMD_MISSION_START", [0.0; 7])
    }

    fn set_mode_rtl(slf: &Bound<'_, Self>) -> PyResult<()> {
        Self::command_long(slf, "MAV_CMD_NAV_RETURN_TO_LAUNCH", [0.0; 7])
    }

    fn set_mode_manual(slf: &Bound<'_, Self>) -> PyResult<()> {
        Self::command_long(
            slf,
            "MAV_CMD_DO_SET_MODE",
            [
                constant_or("MAV_MODE_MANUAL_ARMED", 192) as f64,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
            ],
        )
    }

    fn set_mode_fbwa(slf: &Bound<'_, Self>) -> PyResult<()> {
        Self::command_long(
            slf,
            "MAV_CMD_DO_SET_MODE",
            [
                constant_or("MAV_MODE_STABILIZE_ARMED", 208) as f64,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
            ],
        )
    }

    fn set_mode_loiter(slf: &Bound<'_, Self>) -> PyResult<()> {
        Self::command_long(slf, "MAV_CMD_NAV_LOITER_UNLIM", [0.0; 7])
    }

    fn set_servo(slf: &Bound<'_, Self>, channel: f64, pwm: f64) -> PyResult<()> {
        Self::command_long(slf, "MAV_CMD_DO_SET_SERVO", [channel, pwm, 0.0, 0.0, 0.0, 0.0, 0.0])
    }

    #[pyo3(signature = (relay_pin=0.0, state=true))]
    fn set_relay(slf: &Bound<'_, Self>, relay_pin: f64, state: bool) -> PyResult<()> {
        Self::command_long(
            slf,
            "MAV_CMD_DO_SET_RELAY",
            [relay_pin, if state { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0, 0.0, 0.0],
        )
    }

    fn calibrate_level(slf: &Bound<'_, Self>) -> PyResult<()> {
        Self::command_long(
            slf,
            "MAV_CMD_PREFLIGHT_CALIBRATION",
            [1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        )
    }

    fn calibrate_pressure(slf: &Bound<'_, Self>) -> PyResult<()> {
        Self::command_long(
            slf,
            "MAV_CMD_PREFLIGHT_CALIBRATION",
            [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
        )
    }

    #[pyo3(signature = (hold_in_bootloader=false, force=false))]
    fn reboot_autopilot(slf: &Bound<'_, Self>, hold_in_bootloader: bool, force: bool) -> PyResult<()> {
        let p1 = if hold_in_bootloader { 3.0 } else { 1.0 };
        let p6 = if force { 20190226.0 } else { 0.0 };
        Self::command_long(
            slf,
            "MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN",
            [p1, 0.0, 0.0, 0.0, 0.0, p6, 0.0],
        )
    }

    fn arducopter_arm(slf: &Bound<'_, Self>) -> PyResult<()> {
        Self::command_long(slf, "MAV_CMD_COMPONENT_ARM_DISARM", [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])
    }

    fn arducopter_disarm(slf: &Bound<'_, Self>) -> PyResult<()> {
        Self::command_long(slf, "MAV_CMD_COMPONENT_ARM_DISARM", [0.0; 7])
    }

    fn motors_armed<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.current_state(py)?.getattr("armed")
    }

    fn set_mode_apm(slf: &Bound<'_, Self>, mode: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let mut number = mode.clone();
        if mode.is_instance_of::<PyString>() {
            let map = slf.get().mode_mapping(py)?;
            let map = map.bind(py);
            if map.is_none() || !map.contains(mode)? {
                py_print(py, &format!("Unknown mode '{}'", mode.str()?));
                return Ok(());
            }
            number = map.get_item(mode)?;
        }
        let this = slf.get();
        let args = (
            this.target_system(),
            this.target_component(),
            constant_or("MAV_CMD_DO_SET_MODE", 176),
            0,
            constant_or("MAV_MODE_FLAG_CUSTOM_MODE_ENABLED", 1),
            number,
            0,
            0,
            0,
            0,
            0,
        );
        this.mav(py).as_any().getattr("command_long_send")?.call1(args)?;
        Ok(())
    }

    fn set_mode_px4(
        slf: &Bound<'_, Self>,
        mode: &Bound<'_, PyAny>,
        custom_mode: &Bound<'_, PyAny>,
        custom_sub_mode: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = slf.py();
        let (mut base, mut custom, mut sub) = (mode.clone(), custom_mode.clone(), custom_sub_mode.clone());
        if mode.is_instance_of::<PyString>() {
            let map = slf.get().mode_mapping(py)?;
            let map = map.bind(py);
            if map.is_none() || !map.contains(mode)? {
                py_print(py, &format!("Unknown mode '{}'", mode.str()?));
                return Ok(());
            }
            let row = modes::px4_map(py)?
                .get_item(mode)?
                .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(mode.clone().unbind()))?;
            base = row.get_item(0)?;
            custom = row.get_item(1)?;
            sub = row.get_item(2)?;
        }
        let this = slf.get();
        let args = (
            this.target_system(),
            this.target_component(),
            constant_or("MAV_CMD_DO_SET_MODE", 176),
            0,
            base,
            custom,
            sub,
            0,
            0,
            0,
            0,
        );
        this.mav(py).as_any().getattr("command_long_send")?.call1(args)?;
        Ok(())
    }

    #[pyo3(signature = (mode, custom_mode=None, custom_sub_mode=None))]
    fn set_mode(
        slf: &Bound<'_, Self>,
        mode: &Bound<'_, PyAny>,
        custom_mode: Option<&Bound<'_, PyAny>>,
        custom_sub_mode: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let py = slf.py();
        let autopilot = slf.get().field(py, "HEARTBEAT", "autopilot", None)?;
        if !autopilot.is_none() && autopilot.extract::<i64>().ok() == Some(constant_or("MAV_AUTOPILOT_PX4", 12)) {
            let zero = 0i64.into_pyobject(py)?.into_any();
            Self::set_mode_px4(
                slf,
                mode,
                custom_mode.unwrap_or(&zero),
                custom_sub_mode.unwrap_or(&zero),
            )
        } else {
            Self::set_mode_apm(slf, mode)
        }
    }

    fn reconnect(&self, py: Python<'_>) -> PyResult<()> {
        self.reconnect_tcp(py)
    }

    fn __repr__(&self) -> String {
        format!("<corvus_link.{} {}>", self.kind, lock(&self.state).address)
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        if let Ok(refs) = self.refs.try_lock() {
            visit.call(&refs.mav)?;
            visit.call(&refs.sysid_state)?;
            visit.call(&refs.param_state)?;
            visit.call(&refs.message_hooks)?;
            visit.call(&refs.idle_hooks)?;
            if let Some(f) = &refs.logfile {
                visit.call(f)?;
            }
            if let Some(f) = &refs.logfile_raw {
                visit.call(f)?;
            }
        }
        Ok(())
    }

    fn __clear__(&self) {
        if let Ok(mut refs) = self.refs.try_lock() {
            refs.logfile = None;
            refs.logfile_raw = None;
        }
    }
}

pub fn mode_mapping_byname(py: Python<'_>, mav_type: i64) -> PyResult<PyObject> {
    match modes::ap_mode_table(mav_type) {
        Some(table) => {
            let d = PyDict::new(py);
            for (number, name) in table {
                d.set_item(name, number)?;
            }
            Ok(d.into_any().unbind())
        }
        None => Ok(py.None()),
    }
}

fn split_host_port(device: &str, what: &str) -> PyResult<(String, u16)> {
    let parts: Vec<&str> = device.split(':').collect();
    if parts.len() != 2 {
        return Err(PyValueError::new_err(format!(
            "{what} ports must be specified as host:port"
        )));
    }
    let port: i64 = parts[1]
        .trim()
        .parse()
        .map_err(|_| PyValueError::new_err(format!("invalid literal for int() with base 10: '{}'", parts[1])))?;
    let port =
        u16::try_from(port).map_err(|_| pyo3::exceptions::PyOverflowError::new_err("bind(): port must be 0-65535."))?;
    Ok((parts[0].to_string(), port))
}

/// `mavutil.mavlink_connection` for the transports Corvus offers.
#[pyfunction]
#[pyo3(signature = (
    device, baud=None, source_system=255, source_component=0, planner_format=None, write=false,
    append=false, robust_parsing=true, notimestamps=false, input=true, dialect=None, autoreconnect=false,
    zero_time_base=false, retries=3, reconnect_delay=1.0, use_native=false, force_connected=false,
    progress_callback=None, udp_timeout=0.0, **opts
))]
#[allow(clippy::too_many_arguments, unused_variables)]
pub fn mavlink_connection(
    py: Python<'_>,
    device: &str,
    baud: Option<&Bound<'_, PyAny>>,
    source_system: i64,
    source_component: i64,
    planner_format: Option<PyObject>,
    write: bool,
    append: bool,
    robust_parsing: bool,
    notimestamps: bool,
    input: bool,
    dialect: Option<PyObject>,
    autoreconnect: bool,
    zero_time_base: bool,
    retries: i64,
    reconnect_delay: f64,
    use_native: bool,
    force_connected: bool,
    progress_callback: Option<PyObject>,
    udp_timeout: f64,
    opts: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<Connection>> {
    let autoreconnect = autoreconnect || force_connected;
    let udp = |rest: &str, input: bool, broadcast: bool, timeout: f64| -> PyResult<Py<Connection>> {
        let (host, port) = split_host_port(rest, "UDP")?;
        let link = py
            .allow_threads(|| Udp::open(&host, port, input, broadcast, timeout))
            .map_err(|e| os_error(&e))?;
        Connection::new_object(
            py,
            Link::Udp(link),
            "mavudp",
            rest.to_string(),
            source_system,
            source_component,
        )
    };
    if let Some(rest) = device.strip_prefix("tcp:") {
        let (host, port) = split_host_port(rest, "TCP")?;
        let mut failures: Vec<String> = Vec::new();
        let result = py.allow_threads(|| {
            Tcp::connect(&host, port, retries, reconnect_delay, &mut |e| {
                failures.push(e.to_string())
            })
        });
        for failure in &failures {
            py_print(py, &format!("{failure} sleeping"));
        }
        let stream = result.map_err(|e| os_error(&e))?;
        let link = Tcp::new(stream, &host, port, autoreconnect, retries, reconnect_delay);
        return Connection::new_object(
            py,
            Link::Tcp(link),
            "mavtcp",
            format!("tcp:{rest}"),
            source_system,
            source_component,
        );
    }
    if let Some(rest) = device.strip_prefix("tcpin:") {
        let (host, port) = split_host_port(rest, "TCP")?;
        let link = TcpIn::open(&host, port).map_err(|e| os_error(&e))?;
        return Connection::new_object(
            py,
            Link::TcpIn(link),
            "mavtcpin",
            format!("tcpin:{rest}"),
            source_system,
            source_component,
        );
    }
    if let Some(rest) = device.strip_prefix("udpin:") {
        return udp(rest, true, false, udp_timeout);
    }
    if let Some(rest) = device.strip_prefix("udpout:") {
        return udp(rest, false, false, 0.0);
    }
    if let Some(rest) = device.strip_prefix("udpbcast:") {
        return udp(rest, false, true, 0.0);
    }
    if let Some(rest) = device.strip_prefix("udp:") {
        return udp(rest, input, false, 0.0);
    }
    #[cfg(unix)]
    for prefix in ["uds:", "unix:"] {
        if let Some(path) = device.strip_prefix(prefix) {
            if path.is_empty() {
                return Err(PyValueError::new_err("Unix domain socket path must be specified"));
            }
            let mut failures: Vec<String> = Vec::new();
            let result = py.allow_threads(|| Uds::connect(path, retries, &mut |e| failures.push(e.to_string())));
            for failure in &failures {
                py_print(py, &format!("{failure} sleeping"));
            }
            let stream = result.map_err(|e| os_error(&e))?;
            let link = Uds::new(stream, path, autoreconnect, retries);
            return Connection::new_object(
                py,
                Link::Uds(link),
                "mavuds",
                format!("uds:{path}"),
                source_system,
                source_component,
            );
        }
    }
    if let Some(rest) = device.strip_prefix("mcast:") {
        let parts: Vec<&str> = rest.split(':').collect();
        let (mut group, mut port) = ("239.255.145.50".to_string(), 14550u16);
        let parse_port = |p: &str| {
            p.trim()
                .parse::<u16>()
                .map_err(|_| PyValueError::new_err(format!("invalid literal for int() with base 10: '{p}'")))
        };
        if parts.len() == 1 && !parts[0].is_empty() {
            port = parse_port(parts[0])?;
        } else if parts.len() > 1 {
            group = parts[0].to_string();
            port = parse_port(parts[1])?;
        }
        let link = Mcast::open(&group, port).map_err(|e| os_error(&e))?;
        return Connection::new_object(
            py,
            Link::Mcast(link),
            "mavmcast",
            rest.to_string(),
            source_system,
            source_component,
        );
    }
    // Not built yet. pymavlink also opens these; each needs its own piece:
    //  * wsserver:, ws:, wss:  MAVLink over WebSocket (server and client).
    //    Needs a WebSocket codec and, for wss:, TLS (a rustls dependency).
    //  * csv:                  pymavlink's CSVReader for CSV exports.
    //  * *.bin, *.px4log, and text *.log: DataFlash logs (DFReader). Corvus
    //    reads ArduPilot .bin logs through its own log service instead.
    //  * mavmmaplog's per-type index: a plain tlog read gives the same
    //    messages, only without the seek-ahead recv_match acceleration.
    // Until then they fail here with a message that says so, rather than
    // falling through to the serial or UDP branch below.
    for prefix in ["wsserver:", "ws:", "wss:", "csv:"] {
        if device.starts_with(prefix) {
            return Err(PyNotImplementedError::new_err(format!(
                "corvus_link does not support {prefix} links yet"
            )));
        }
    }
    let lower = device.to_lowercase();
    if lower.ends_with(".bin") || lower.ends_with(".px4log") {
        return Err(PyNotImplementedError::new_err(
            "corvus_link does not read DataFlash logs yet",
        ));
    }
    if device.ends_with(".log") && is_dataflash_text(device) {
        return Err(PyNotImplementedError::new_err(
            "corvus_link does not read DataFlash logs yet",
        ));
    }
    let suffix = lower.rsplit('.').next().unwrap_or("");
    if device.contains(':') && !["mavlink", "log", "raw", "tlog"].contains(&suffix) {
        return udp(device, input, false, 0.0);
    }
    if std::path::Path::new(device).is_file() {
        #[cfg(unix)]
        if device.ends_with(".elf") || device.contains("/bin/") {
            py_print(py, &format!("executing '{device}'"));
            let child = ChildExec::spawn(device).map_err(|e| os_error(&e))?;
            return Connection::new_object(
                py,
                Link::Child(child),
                "mavchildexec",
                device.to_string(),
                source_system,
                source_component,
            );
        }
        let file = LogFile::open(device, write, append).map_err(|e| os_error(&e))?;
        let kind = if !write && !append && !notimestamps {
            "mavmmaplog"
        } else {
            "mavlogfile"
        };
        let state = LogState {
            planner_format: planner_format
                .as_ref()
                .is_some_and(|p| p.bind(py).is_truthy().unwrap_or(false)),
            last_timestamp: None,
            last_was_bad: false,
            link: 0,
            percent: 0.0,
        };
        let conn = Connection::new_object(
            py,
            Link::Log(file, Mutex::new(state)),
            kind,
            device.to_string(),
            source_system,
            source_component,
        )?;
        {
            let c = conn.get();
            let mut s = lock(&c.state);
            s.notimestamps = notimestamps;
            s.stop_on_eof = true;
            s.ts_override = Some(if notimestamps { 0.0 } else { frame::now() });
            s.robust_parsing = robust_parsing;
        }
        conn.get().mav(py).get().set_robust_parsing(robust_parsing);
        return Ok(conn);
    }
    let (mut device, mut baud_value): (String, u32) = (
        device.to_string(),
        match baud {
            Some(b) => b.extract()?,
            None => 115200,
        },
    );
    if device.contains(',') && !std::path::Path::new(&device).exists() {
        let (d, b) = device.split_once(',').unwrap();
        baud_value = b
            .trim()
            .parse()
            .map_err(|_| PyValueError::new_err(format!("invalid baud rate '{b}'")))?;
        device = d.to_string();
    }
    let opened = {
        let device = device.clone();
        py.allow_threads(move || Serial::open(&device, baud_value))
    };
    let link = match opened {
        Ok(link) => link,
        Err(e) => {
            return Err(match e.errno {
                Some(code) => PyOSError::new_err((code, e.message)),
                None => PyOSError::new_err(e.message),
            });
        }
    };
    Connection::new_object(
        py,
        Link::Serial(link),
        "mavserial",
        device,
        source_system,
        source_component,
    )
}

/// DFReader_is_text_log: a text DataFlash log starts with "FMT,".
fn is_dataflash_text(path: &str) -> bool {
    use std::io::Read;
    let mut head = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && &head == b"FMT,"
}
