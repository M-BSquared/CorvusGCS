//! Second station forwarding: every frame the vehicle link receives mirrored
//! to UDP endpoints (QGroundControl on 127.0.0.1:14550 by default), and, when
//! the operator allows it, whole frames from the configured endpoints put on
//! the vehicle's uplink.
//!
//! The receive loop only appends to a bounded drop-oldest queue that a sender
//! thread drains, so a wedged endpoint never stalls the aircraft's link.
//! Inbound datagrams are split into whole MAVLink frames before anything is
//! injected; an endpoint is learned only on a whole frame, may command only
//! when the operator named it, and the vehicle link's own socket is never
//! mirrored to (SITL and QGroundControl both default to 14550).
//!
//! The socket and the forwarder's state stay on
//! `corvus.mavlink_forwarder.MavlinkForwarder` (the socket is Python's, whose
//! I/O is C already, and which a test sends from); the logic and both
//! threads (`CoreThread`s) are here. Frame splitting is plain Rust.

use pyo3::exceptions::{PyException, PyOSError, PyTimeoutError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyModule, PySet, PyTuple};

use crate::bridge::receive::within;
use crate::bridge::thread;
use crate::protocol::host::obj;

const STX_V1: u8 = 0xFE;
const STX_V2: u8 = 0xFD;
const V1_OVERHEAD: usize = 8;
const V2_OVERHEAD: usize = 12;
const V2_SIGNATURE_LEN: usize = 13;
const IFLAG_SIGNED: u8 = 0x01;
const MAX_FRAME: usize = 280;

/// Whole MAVLink frames in *buf* by length and framing, and what is left.
pub fn split(buf: &[u8]) -> (Vec<&[u8]>, &[u8]) {
    let mut frames = Vec::new();
    let mut i = 0;
    let n = buf.len();
    while i < n {
        let total = match buf[i] {
            STX_V1 => {
                if n - i < 2 {
                    break;
                }
                buf[i + 1] as usize + V1_OVERHEAD
            }
            STX_V2 => {
                if n - i < 3 {
                    break;
                }
                let mut total = buf[i + 1] as usize + V2_OVERHEAD;
                if buf[i + 2] & IFLAG_SIGNED != 0 {
                    total += V2_SIGNATURE_LEN;
                }
                total
            }
            _ => {
                i += 1;
                continue;
            }
        };
        if total > MAX_FRAME {
            i += 1;
            continue;
        }
        if n - i < total {
            break;
        }
        frames.push(&buf[i..i + total]);
        i += total;
    }
    (frames, &buf[i..])
}

/// Split a byte run into whole MAVLink frames plus the trailing remainder.
/// Validated by length and framing only: enough that a truncated or garbage
/// datagram never puts a partial frame on the link to the aircraft.
#[pyfunction]
pub fn split_frames<'py>(py: Python<'py>, buf: &[u8]) -> PyResult<Bound<'py, PyTuple>> {
    let (frames, rest) = split(buf);
    let list = PyList::new(py, frames.iter().map(|f| PyBytes::new(py, f)))?;
    PyTuple::new(py, [list.into_any(), PyBytes::new(py, rest).into_any()])
}

/// The system id a whole frame was sent under, read positionally; 0 if
/// unreadable.
#[pyfunction]
pub fn frame_source_system(frame: &[u8]) -> u8 {
    if frame.len() >= 6 && frame[0] == STX_V1 {
        return frame[3];
    }
    if frame.len() >= 8 && frame[0] == STX_V2 {
        return frame[5];
    }
    0
}

/// `"host:port"` or `"host"` as `(host, port)`, or None if unusable: one bad
/// line of operator typed config costs that line, not the feature.
#[pyfunction]
#[pyo3(signature = (text, default_port=14550))]
pub fn parse_endpoint(py: Python<'_>, text: &Bound<'_, PyAny>, default_port: i64) -> PyResult<PyObject> {
    let raw = if text.is_truthy()? {
        text.str()?.to_string()
    } else {
        String::new()
    };
    let raw = py
        .import("builtins")?
        .getattr("str")?
        .call1((raw,))?
        .call_method0("strip")?;
    let raw: String = raw.extract()?;
    if raw.is_empty() {
        return Ok(py.None());
    }
    let (mut host, mut port_text) = match raw.rfind(':') {
        Some(at) => (raw[..at].to_string(), raw[at + 1..].to_string()),
        None => (String::new(), raw.clone()),
    };
    if host.is_empty() {
        host = raw.clone();
        port_text = String::new();
    }
    let port = if port_text.is_empty() {
        default_port
    } else {
        match py.get_type::<pyo3::types::PyInt>().call1((port_text,)) {
            Ok(v) => match v.extract::<i64>() {
                Ok(p) => p,
                Err(_) => return Ok(py.None()),
            },
            Err(err) if err.is_instance_of::<pyo3::exceptions::PyValueError>(py) => return Ok(py.None()),
            Err(err) => return Err(err),
        }
    };
    if !(0 < port && port < 65536) || host.is_empty() {
        return Ok(py.None());
    }
    Ok(PyTuple::new(py, [obj(py, host), obj(py, port)])?.into_any().unbind())
}

fn is_set(o: &Bound<'_, PyAny>, name: &str) -> PyResult<bool> {
    o.getattr(name)?.call_method0("is_set")?.is_truthy()
}

fn bump(fwd: &Bound<'_, PyAny>, name: &str) -> PyResult<()> {
    let value = fwd.getattr(name)?.add(1)?;
    fwd.setattr(name, value)
}

fn endpoint_text(addr: &Bound<'_, PyAny>) -> PyResult<String> {
    Ok(format!("{}:{}", addr.get_item(0)?.str()?, addr.get_item(1)?.str()?))
}

/// The forwarder's logic.
#[pyclass(name = "ForwarderEngine", module = "corvus_link.bridge", frozen)]
pub struct ForwarderEngine {
    env: PyObject,
}

impl ForwarderEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn monotonic(&self, py: Python<'_>) -> PyResult<f64> {
        self.env(py).getattr("time")?.call_method0("monotonic")?.extract()
    }

    fn logger<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr("logger")
    }

    fn tunable(&self, py: Python<'_>, name: &str) -> PyResult<f64> {
        self.env(py).getattr(name)?.extract()
    }
}

#[pymethods]
impl ForwarderEngine {
    /// *env* is ``corvus.mavlink_forwarder``.
    #[new]
    fn new(env: PyObject) -> Self {
        ForwarderEngine { env }
    }

    /// The static endpoints: the primary target first, then the configured
    /// ones; an unusable line, a duplicate and this forwarder's own address
    /// are left out (the last with a warning, since every mirrored frame
    /// would come straight back).
    fn static_endpoints<'py>(
        &self,
        fwd: &Bound<'py, PyAny>,
        endpoints: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyList>> {
        let py = fwd.py();
        let out = PyList::empty(py);
        let mut items = vec![obj(
            py,
            format!("{}:{}", fwd.getattr("_host")?.str()?, fwd.getattr("_port")?.str()?),
        )
        .into_bound(py)];
        for item in endpoints.try_iter()? {
            items.push(item?);
        }
        let own = PyTuple::new(py, [fwd.getattr("_listen_host")?, fwd.getattr("_listen_port")?])?;
        let parse = self.env(py).getattr("parse_endpoint")?;
        for item in items {
            let parsed = parse.call1((&item,))?;
            if parsed.is_none() {
                self.logger(py)?
                    .call_method1("warning", ("ignoring unusable forward endpoint %r", &item))?;
                continue;
            }
            if out.contains(&parsed)? {
                continue;
            }
            if parsed.eq(&own)? {
                self.logger(py)?.call_method1(
                    "warning",
                    (
                        "ignoring forward endpoint %r: it is this forwarder's own address",
                        &item,
                    ),
                )?;
                continue;
            }
            out.append(parsed)?;
        }
        Ok(out)
    }

    /// Bind and run both threads. False (with the error set) only if no
    /// socket could be bound at all; a busy listen port falls back to an
    /// ephemeral one and says so.
    fn start(&self, fwd: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = fwd.py();
        if !fwd.getattr("_sock")?.is_none() {
            return Ok(true);
        }
        let bound = fwd.call_method0("_bind")?;
        let (sock, address, note): (Bound<'_, PyAny>, Bound<'_, PyAny>, Bound<'_, PyAny>) = bound.extract()?;
        if sock.is_none() {
            self.logger(py)?
                .call_method1("warning", ("MAVLink forwarding disabled: %s", fwd.getattr("_error")?))?;
            return Ok(false);
        }
        fwd.setattr("_sock", &sock)?;
        fwd.setattr("_bound", &address)?;
        fwd.setattr("_error", "")?;
        fwd.setattr("_notice", &note)?;
        fwd.setattr("_sysid_conflict", false)?;
        fwd.setattr("_refused", 0)?;
        fwd.setattr("_refused_peers", PySet::empty(py)?)?;
        fwd.setattr("_loop_blocked", py.None())?;
        fwd.setattr("_local_hosts", self.env(py).getattr("_local_host_addresses")?.call0()?)?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("force", true)?;
        fwd.call_method("_resolve_static", (), Some(&kwargs))?;
        fwd.getattr("_stop")?.call_method0("clear")?;
        let (rx, tx) = (fwd.clone().unbind(), fwd.clone().unbind());
        fwd.setattr(
            "_rx_thread",
            thread::spawn(py, "mav-forward-rx", move |py| {
                rx.bind(py).call_method0("_rx_loop").map(|_| ())
            })?,
        )?;
        fwd.setattr(
            "_tx_thread",
            thread::spawn(py, "mav-forward-tx", move |py| {
                tx.bind(py).call_method0("_tx_loop").map(|_| ())
            })?,
        )?;
        let mut targets = Vec::new();
        for addr in fwd.getattr("_static")?.try_iter()? {
            targets.push(endpoint_text(&addr?)?);
        }
        let targets = if targets.is_empty() {
            "nothing".to_string()
        } else {
            targets.join(", ")
        };
        let commands = if fwd.getattr("_allow_commands")?.is_truthy()? {
            "allowed"
        } else {
            "blocked"
        };
        self.logger(py)?.call_method1(
            "info",
            (
                "MAVLink forwarding: mirroring to %s, listening on %s:%d (commands from other stations: %s)",
                targets,
                address.get_item(0)?,
                address.get_item(1)?,
                commands,
            ),
        )?;
        Ok(true)
    }

    /// The listen socket, falling back to an ephemeral port: `(socket,
    /// bound address, note)`, the socket None when nothing would bind.
    fn bind<'py>(&self, fwd: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
        let py = fwd.py();
        let socket = py.import("socket")?;
        let host = fwd.getattr("_listen_host")?;
        let requested = fwd.getattr("_listen_port")?;
        for (port, fallback) in [(requested.clone(), false), (obj(py, 0).into_bound(py), true)] {
            if fallback && requested.eq(0)? {
                break;
            }
            let mut sock: Option<Bound<'_, PyAny>> = None;
            let opened = (|| -> PyResult<()> {
                let s = socket.call_method1("socket", (socket.getattr("AF_INET")?, socket.getattr("SOCK_DGRAM")?))?;
                sock = Some(s.clone());
                s.call_method1(
                    "setsockopt",
                    (socket.getattr("SOL_SOCKET")?, socket.getattr("SO_REUSEADDR")?, 1),
                )?;
                s.call_method1("bind", (PyTuple::new(py, [&host, &port])?,))?;
                s.call_method1("settimeout", (self.env(py).getattr("_DRAIN_TIMEOUT_S")?,))?;
                Ok(())
            })();
            if let Err(err) = opened {
                if !err.is_instance_of::<PyOSError>(py) {
                    return Err(err);
                }
                if let Some(s) = &sock {
                    let _ = s.call_method0("close");
                }
                let value = err.value(py);
                let reason = value.getattr("strerror")?;
                let reason = if reason.is_truthy()? {
                    reason.str()?.to_string()
                } else {
                    value.str()?.to_string()
                };
                fwd.setattr(
                    "_error",
                    format!("cannot listen on {}:{} ({reason})", host.str()?, port.str()?),
                )?;
                continue;
            }
            let sock = sock.expect("opened");
            let bound = match sock.call_method0("getsockname") {
                Ok(b) => b,
                Err(err) if err.is_instance_of::<PyOSError>(py) => PyTuple::new(py, [&host, &port])?.into_any(),
                Err(err) => return Err(err),
            };
            let mut note = String::new();
            if fallback {
                note = format!(
                    "port {} was busy, listening on {}:{} instead",
                    requested.str()?,
                    bound.get_item(0)?.str()?,
                    bound.get_item(1)?.str()?
                );
                self.logger(py)?
                    .call_method1("warning", ("MAVLink forwarding: %s", &note))?;
            }
            let address = PyTuple::new(
                py,
                [
                    bound.get_item(0)?,
                    py.get_type::<pyo3::types::PyInt>().call1((bound.get_item(1)?,))?,
                ],
            )?;
            return PyTuple::new(py, [sock, address.into_any(), obj(py, note).into_bound(py)]);
        }
        PyTuple::new(
            py,
            [
                py.None().into_bound(py),
                PyTuple::new(py, [&host, &requested])?.into_any(),
                fwd.getattr("_error")?,
            ],
        )
    }

    /// Idempotent teardown: threads joined, socket closed, queue dropped.
    fn stop(&self, fwd: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = fwd.py();
        fwd.getattr("_stop")?.call_method0("set")?;
        let cond = fwd.getattr("_cond")?;
        within(&cond, || {
            fwd.getattr("_out")?.call_method0("clear")?;
            cond.call_method0("notify_all").map(|_| ())
        })?;
        let sock = fwd.getattr("_sock")?;
        fwd.setattr("_sock", py.None())?;
        if !sock.is_none() {
            let _ = sock.call_method0("close");
        }
        for name in ["_rx_thread", "_tx_thread"] {
            let handle = fwd.getattr(name)?;
            if !handle.is_none() {
                let kwargs = PyDict::new(py);
                kwargs.set_item("timeout", 2.0)?;
                let _ = handle.call_method("join", (), Some(&kwargs));
            }
        }
        fwd.setattr("_rx_thread", py.None())?;
        fwd.setattr("_tx_thread", py.None())?;
        within(&fwd.getattr("_peer_lock")?, || {
            fwd.getattr("_peers")?.call_method0("clear").map(|_| ())
        })?;
        fwd.getattr("_rx_buf")?.call_method0("clear")?;
        fwd.setattr("_bound", py.None())
    }

    /// Queue one raw frame from the aircraft; never blocks, never raises.
    fn feed(&self, fwd: &Bound<'_, PyAny>, raw: &Bound<'_, PyAny>) -> PyResult<()> {
        if is_set(fwd, "_stop")? || fwd.getattr("_sock")?.is_none() || !raw.is_truthy()? {
            return Ok(());
        }
        let cond = fwd.getattr("_cond")?;
        within(&cond, || {
            let out = fwd.getattr("_out")?;
            if out.len()?.eq(&out.getattr("maxlen")?.extract::<usize>()?) {
                bump(fwd, "_dropped")?;
            }
            out.call_method1("append", (raw,))?;
            cond.call_method0("notify").map(|_| ())
        })
    }

    /// Resolve the static endpoints' names once: at start, and afterwards
    /// only the names that failed, at most every _RESOLVE_RETRY_S, so the
    /// sender never waits on a resolver.
    #[pyo3(signature = (fwd, force=false))]
    fn resolve_static(&self, fwd: &Bound<'_, PyAny>, force: bool) -> PyResult<()> {
        let py = fwd.py();
        let now = self.monotonic(py)?;
        let unresolved_now = fwd.getattr("_unresolved")?;
        if !force
            && (!unresolved_now.is_truthy()?
                || now - fwd.getattr("_resolved_at")?.extract::<f64>()? < self.tunable(py, "_RESOLVE_RETRY_S")?)
        {
            return Ok(());
        }
        let builtins = py.import("builtins")?;
        let pending =
            builtins
                .getattr("list")?
                .call1((if force { fwd.getattr("_static")? } else { unresolved_now },))?;
        let keep = if force {
            PyList::empty(py)
        } else {
            builtins
                .getattr("list")?
                .call1((fwd.getattr("_resolved")?,))?
                .downcast_into::<PyList>()?
        };
        let unresolved = PyList::empty(py);
        let socket = py.import("socket")?;
        for item in pending.try_iter()? {
            let item = item?;
            let (host, port): (Bound<'_, PyAny>, Bound<'_, PyAny>) = item.extract()?;
            let info = match socket.call_method1(
                "getaddrinfo",
                (&host, &port, socket.getattr("AF_INET")?, socket.getattr("SOCK_DGRAM")?),
            ) {
                Ok(info) => info,
                Err(err) if err.is_instance_of::<PyOSError>(py) => {
                    unresolved.append(PyTuple::new(py, [&host, &port])?)?;
                    self.logger(py)?.call_method1(
                        "warning",
                        (
                            "forward endpoint %s:%d does not resolve (%s)",
                            &host,
                            &port,
                            err.value(py),
                        ),
                    )?;
                    continue;
                }
                Err(err) => return Err(err),
            };
            let addr = info.get_item(0)?.get_item(4)?;
            let entry = PyTuple::new(
                py,
                [
                    addr.get_item(0)?.str()?.into_any(),
                    py.get_type::<pyo3::types::PyInt>().call1((addr.get_item(1)?,))?,
                ],
            )?;
            if !keep.contains(&entry)? {
                keep.append(entry)?;
            }
        }
        fwd.setattr("_resolved", keep)?;
        fwd.setattr("_unresolved", unresolved)?;
        fwd.setattr("_resolved_at", now)
    }

    /// The endpoints that spoke MAVLink to us inside the TTL; the rest swept.
    fn live_peers<'py>(&self, fwd: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
        let py = fwd.py();
        let now = self.monotonic(py)?;
        let ttl = self.tunable(py, "_PEER_TTL_S")?;
        within(&fwd.getattr("_peer_lock")?, || {
            let peers = fwd.getattr("_peers")?;
            let live = PyList::empty(py);
            for (addr, seen) in peers.downcast::<PyDict>()?.iter() {
                if now - seen.extract::<f64>()? <= ttl {
                    live.append(addr)?;
                }
            }
            if live.len() != peers.len()? {
                let kept = PyDict::new(py);
                for addr in live.iter() {
                    kept.set_item(&addr, peers.get_item(&addr)?)?;
                }
                fwd.setattr("_peers", kept)?;
            }
            Ok(live)
        })
    }

    /// Everywhere one mirrored frame goes: the configured endpoints and
    /// whoever dialled in, minus ourselves and the vehicle link.
    fn targets<'py>(&self, fwd: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
        let py = fwd.py();
        let bound = fwd.getattr("_bound")?;
        let out = PyList::empty(py);
        let mut all: Vec<Bound<'_, PyAny>> = fwd.getattr("_resolved")?.try_iter()?.collect::<PyResult<_>>()?;
        all.extend(
            fwd.call_method0("_live_peers")?
                .try_iter()?
                .collect::<PyResult<Vec<_>>>()?,
        );
        for addr in all {
            if out.contains(&addr)? || addr.eq(&bound)? {
                continue;
            }
            if fwd.call_method1("_is_vehicle_link", (&addr,))?.is_truthy()? {
                fwd.call_method1("_note_loop", (&addr,))?;
                continue;
            }
            out.append(addr)?;
        }
        Ok(out)
    }

    /// Is *addr* the socket Corvus receives the aircraft on?
    fn is_vehicle_link(&self, fwd: &Bound<'_, PyAny>, addr: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = fwd.py();
        let getter = fwd.getattr("_link_address")?;
        if getter.is_none() {
            return Ok(false);
        }
        let own = match getter.call0() {
            Ok(own) => own,
            Err(err) if err.is_instance_of::<PyException>(py) => return Ok(false),
            Err(err) => return Err(err),
        };
        if !own.is_truthy()? {
            return Ok(false);
        }
        let host = own.get_item(0)?.str()?.to_string();
        let int = py.get_type::<pyo3::types::PyInt>();
        let port = int.call1((own.get_item(1)?,))?;
        if int.call1((addr.get_item(1)?,))?.ne(&port)? {
            return Ok(false);
        }
        let peer = addr.get_item(0)?.str()?.to_string();
        if host.is_empty() || host == "0.0.0.0" {
            return Ok(peer.starts_with("127.") || fwd.getattr("_local_hosts")?.contains(peer.as_str())?);
        }
        if peer == host {
            return Ok(true);
        }
        Ok(peer.starts_with("127.") && host.starts_with("127."))
    }

    /// Latch, and log once, an endpoint skipped as the vehicle link.
    fn note_loop(&self, fwd: &Bound<'_, PyAny>, addr: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = fwd.py();
        if fwd.getattr("_loop_blocked")?.eq(addr)? {
            return Ok(());
        }
        fwd.setattr("_loop_blocked", addr)?;
        self.logger(py)?.call_method1(
            "warning",
            (
                "not forwarding to %s:%d: that is the port Corvus receives the vehicle on, so every frame would come straight back. Set forwarding to another port (for example 14560) and point the other station's UDP link at it.",
                addr.get_item(0)?,
                addr.get_item(1)?,
            ),
        )?;
        Ok(())
    }

    fn loop_text(&self, fwd: &Bound<'_, PyAny>) -> PyResult<String> {
        let addr = fwd.getattr("_loop_blocked")?;
        if addr.is_none() {
            return Ok(String::new());
        }
        let at = endpoint_text(&addr)?;
        Ok(format!(
            "Not mirroring to {at}: Corvus receives the vehicle on that port, so every frame would come straight back. Choose another forwarding port, for example 14560, and set the other station's UDP link to it."
        ))
    }

    /// The sender thread: batches out of the queue to every target; an
    /// endpoint that has gone away costs a drop, never the link.
    fn tx_loop(&self, fwd: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = fwd.py();
        let cond = fwd.getattr("_cond")?;
        let wait = self.env(py).getattr("_DRAIN_TIMEOUT_S")?;
        while !thread::shutting_down() && !is_set(fwd, "_stop")? {
            let batch = within(&cond, || -> PyResult<Option<Vec<Bound<'_, PyAny>>>> {
                let out = fwd.getattr("_out")?;
                if !out.is_truthy()? {
                    cond.call_method1("wait", (&wait,))?;
                    return Ok(None);
                }
                let batch: Vec<Bound<'_, PyAny>> = out.try_iter()?.collect::<PyResult<_>>()?;
                out.call_method0("clear")?;
                Ok(Some(batch))
            })?;
            let Some(batch) = batch else {
                continue;
            };
            let sock = fwd.getattr("_sock")?;
            if sock.is_none() {
                continue;
            }
            if fwd.getattr("_unresolved")?.is_truthy()? {
                fwd.call_method0("_resolve_static")?;
            }
            let targets = fwd.call_method0("_targets")?;
            if !targets.is_truthy()? {
                continue;
            }
            let targets: Vec<Bound<'_, PyAny>> = targets.try_iter()?.collect::<PyResult<_>>()?;
            for frame in &batch {
                for addr in &targets {
                    match sock.call_method1("sendto", (frame, addr)) {
                        Ok(_) => bump(fwd, "_sent")?,
                        Err(err) if err.is_instance_of::<PyException>(py) => bump(fwd, "_dropped")?,
                        Err(err) => return Err(err),
                    }
                }
            }
        }
        Ok(())
    }

    /// The receiver thread: datagrams reassembled per endpoint into whole
    /// frames; a whole frame learns its sender as a peer, and only a
    /// configured endpoint may command when commanding is on. Never a frame
    /// under Corvus' own system id.
    fn rx_loop(&self, fwd: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = fwd.py();
        let max_frame = MAX_FRAME;
        let max_buffers: usize = self.env(py).getattr("_MAX_RX_BUFFERS")?.extract()?;
        let corvus_id: u8 = self.env(py).getattr("_CORVUS_SYSTEM_ID")?.extract()?;
        while !thread::shutting_down() && !is_set(fwd, "_stop")? {
            let sock = fwd.getattr("_sock")?;
            if sock.is_none() {
                break;
            }
            let received = match sock.call_method1("recvfrom", (65535,)) {
                Ok(r) => r,
                Err(err) if err.is_instance_of::<PyTimeoutError>(py) => continue,
                Err(err) if err.is_instance_of::<PyOSError>(py) => {
                    if is_set(fwd, "_stop")? {
                        break;
                    }
                    self.env(py).getattr("time")?.call_method1("sleep", (0.05,))?;
                    continue;
                }
                Err(err) => return Err(err),
            };
            let (data, addr): (Bound<'_, PyAny>, Bound<'_, PyAny>) = received.extract()?;
            if !data.is_truthy()? {
                continue;
            }
            if addr.eq(fwd.getattr("_bound")?)? {
                continue;
            }
            if fwd.call_method1("_is_vehicle_link", (&addr,))?.is_truthy()? {
                fwd.call_method1("_note_loop", (&addr,))?;
                continue;
            }
            bump(fwd, "_received")?;
            let buffers = fwd.getattr("_rx_buf")?;
            let mut buffered: Vec<u8> = buffers.call_method1("get", (&addr, PyBytes::new(py, b"")))?.extract()?;
            buffered.extend(data.extract::<Vec<u8>>()?);
            let (frames, rest) = split(&buffered);
            if !rest.is_empty() && rest.len() < max_frame {
                buffers.set_item(&addr, PyBytes::new(py, rest))?;
            } else {
                buffers.call_method1("pop", (&addr, py.None()))?;
            }
            if buffers.len()? > max_buffers {
                fwd.call_method0("_drop_stale_rx_buffers")?;
            }
            if frames.is_empty() {
                continue;
            }
            let now = self.monotonic(py)?;
            within(&fwd.getattr("_peer_lock")?, || {
                fwd.getattr("_peers")?.set_item(&addr, now)
            })?;
            let allowed = fwd.getattr("_allow_commands")?.is_truthy()?;
            let may_command = allowed && fwd.getattr("_resolved")?.contains(&addr)?;
            if allowed && !may_command {
                fwd.call_method1("_note_command_refused", (&addr,))?;
            }
            let inject = fwd.getattr("_inject")?;
            for frame in frames {
                if frame_source_system(frame) == corvus_id {
                    fwd.call_method0("_note_sysid_conflict")?;
                    continue;
                }
                if !may_command || inject.is_none() {
                    continue;
                }
                match inject.call1((PyBytes::new(py, frame),)) {
                    Ok(ok) => {
                        if ok.is_truthy()? {
                            bump(fwd, "_injected")?;
                        }
                    }
                    Err(err) if err.is_instance_of::<PyException>(py) => {
                        let kwargs = PyDict::new(py);
                        kwargs.set_item("exc_info", err.value(py))?;
                        self.logger(py)?
                            .call_method("debug", ("forward inject failed",), Some(&kwargs))?;
                    }
                    Err(err) => return Err(err),
                }
            }
        }
        Ok(())
    }

    /// Forget the half frames of endpoints that stopped talking.
    fn drop_stale_rx_buffers(&self, fwd: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = fwd.py();
        let now = self.monotonic(py)?;
        let ttl = self.tunable(py, "_PEER_TTL_S")?;
        let live = within(&fwd.getattr("_peer_lock")?, || {
            let live = PySet::empty(py)?;
            for (addr, seen) in fwd.getattr("_peers")?.downcast::<PyDict>()?.iter() {
                if now - seen.extract::<f64>()? <= ttl {
                    live.add(addr)?;
                }
            }
            Ok(live)
        })?;
        let buffers = fwd.getattr("_rx_buf")?;
        let stale: Vec<Bound<'_, PyAny>> = buffers
            .try_iter()?
            .collect::<PyResult<Vec<_>>>()?
            .into_iter()
            .filter(|a| !live.contains(a).unwrap_or(false))
            .collect();
        for addr in stale {
            buffers.call_method1("pop", (addr, py.None()))?;
        }
        Ok(())
    }

    /// Count, and log once per station, frames kept off the uplink because
    /// their sender is not a configured endpoint.
    fn note_command_refused(&self, fwd: &Bound<'_, PyAny>, addr: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = fwd.py();
        bump(fwd, "_refused")?;
        let refused = fwd.getattr("_refused_peers")?;
        if refused.contains(addr)? {
            return Ok(());
        }
        refused.call_method1("add", (addr,))?;
        let mut configured = Vec::new();
        for item in fwd.getattr("_static")?.try_iter()? {
            configured.push(endpoint_text(&item?)?);
        }
        let configured = if configured.is_empty() {
            "none".to_string()
        } else {
            configured.join(", ")
        };
        self.logger(py)?.call_method1(
            "warning",
            (
                "refusing to forward commands from %s:%d. Commanding is allowed only from the configured endpoints (%s). Add it to the forwarding endpoint list if this station should be able to command the aircraft.",
                addr.get_item(0)?,
                addr.get_item(1)?,
                configured,
            ),
        )?;
        Ok(())
    }

    /// Latch, and log once, a second station sending under Corvus' own
    /// system id.
    fn note_sysid_conflict(&self, fwd: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = fwd.py();
        if fwd.getattr("_sysid_conflict")?.is_truthy()? {
            return Ok(());
        }
        fwd.setattr("_sysid_conflict", true)?;
        self.logger(py)?.call_method1(
            "warning",
            (
                "the other station is transmitting as MAVLink system %d, the same id Corvus uses. Give it its own (QGroundControl: Application Settings → MAVLink → Ground Station system ID)",
                self.env(py).getattr("_CORVUS_SYSTEM_ID")?,
            ),
        )?;
        Ok(())
    }

    /// What the LINK tab shows.
    fn status<'py>(&self, fwd: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
        let py = fwd.py();
        let listen = fwd.call_method0("listen_address")?;
        let texts = |items: Bound<'_, PyAny>| -> PyResult<Vec<String>> {
            items.try_iter()?.map(|a| endpoint_text(&a?)).collect()
        };
        let out = PyDict::new(py);
        out.set_item("running", !fwd.getattr("_sock")?.is_none() && !is_set(fwd, "_stop")?)?;
        out.set_item("host", fwd.getattr("_host")?)?;
        out.set_item("port", fwd.getattr("_port")?)?;
        out.set_item("listen_host", listen.get_item(0)?)?;
        out.set_item("listen_port", listen.get_item(1)?)?;
        out.set_item("listen_port_requested", fwd.getattr("_listen_port")?)?;
        out.set_item("allow_commands", fwd.getattr("_allow_commands")?)?;
        out.set_item("endpoints", texts(fwd.getattr("_static")?)?)?;
        out.set_item("peers", texts(fwd.call_method0("_live_peers")?)?)?;
        out.set_item("targets", texts(fwd.call_method0("_targets")?)?)?;
        out.set_item("frames_sent", fwd.getattr("_sent")?)?;
        out.set_item("datagrams_received", fwd.getattr("_received")?)?;
        out.set_item("frames_injected", fwd.getattr("_injected")?)?;
        out.set_item("dropped", fwd.getattr("_dropped")?)?;
        out.set_item("sysid_conflict", fwd.getattr("_sysid_conflict")?)?;
        out.set_item("commands_refused", fwd.getattr("_refused")?)?;
        let mut refused = texts(fwd.getattr("_refused_peers")?)?;
        refused.sort();
        out.set_item("commands_refused_from", refused)?;
        let blocked = fwd.getattr("_loop_blocked")?;
        out.set_item(
            "loop_blocked",
            if blocked.is_truthy()? {
                endpoint_text(&blocked)?
            } else {
                String::new()
            },
        )?;
        let notice = fwd.getattr("_notice")?;
        let notice = if notice.is_truthy()? {
            notice.str()?.to_string()
        } else {
            fwd.call_method0("_loop_text")?.str()?.to_string()
        };
        out.set_item("notice", notice)?;
        out.set_item("error", fwd.getattr("_error")?)?;
        Ok(out)
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<ForwarderEngine>()?;
    m.add_function(wrap_pyfunction!(split_frames, m)?)?;
    m.add_function(wrap_pyfunction!(frame_source_system, m)?)?;
    m.add_function(wrap_pyfunction!(parse_endpoint, m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_by_length_and_resync_on_junk() {
        let v1 = [0xFE, 1, 0, 1, 1, 0, 7, 0xAA, 0xBB];
        let mut buf = vec![0x00, 0x13];
        buf.extend_from_slice(&v1);
        buf.extend_from_slice(&v1[..4]);
        let (frames, rest) = split(&buf);
        assert_eq!(frames, vec![&v1[..]]);
        assert_eq!(rest, &v1[..4]);
    }

    #[test]
    fn a_signed_v2_frame_carries_its_signature() {
        let mut frame = vec![0xFD, 0, 0x01];
        frame.resize(12 + 13, 0);
        let (frames, rest) = split(&frame);
        assert_eq!(frames.len(), 1);
        assert!(rest.is_empty());
        assert_eq!(frame_source_system(&frame), 0);
    }
}
