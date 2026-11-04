//! Where RTCM corrections come from: a base station's serial port, or a
//! socket held open to an NTRIP caster.
//!
//! `SerialPort` and `TcpSocket` are the default devices; both speak the
//! slice of pyserial's and the socket module's interface the transports
//! use, so a test can hand in a fake instead. `SerialTransport` and
//! `NtripTransport` are what the session reads, writes and closes.

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, SocketAddrV6, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pyo3::exceptions::{PyOSError, PyRuntimeError, PyTimeoutError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyModule, PyString};

use crate::link::transport::Serial;
use crate::lock;
use crate::rtk::wire::bytes_of;

pyo3::create_exception!(
    corvus_link.rtk,
    RtkError,
    PyRuntimeError,
    "A session could not be run, with the reason an operator can act on."
);

pub const NTRIP_READ_BYTES: usize = 4096;

/// An `RtkError` carrying *cause* as its `__cause__`, as `raise ... from`.
pub fn rtk_error(py: Python<'_>, text: String, cause: Option<&PyErr>) -> PyErr {
    let err = RtkError::new_err(text);
    if let Some(cause) = cause {
        err.set_cause(py, Some(cause.clone_ref(py)));
    }
    err
}

/// `OSError(errno, os.strerror(errno))` for an OS error, which Python turns
/// into the matching subclass; a timeout as `TimeoutError("timed out")`.
fn os_error(py: Python<'_>, err: &io::Error) -> PyErr {
    if matches!(err.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) {
        return PyTimeoutError::new_err("timed out");
    }
    match err.raw_os_error() {
        Some(code) => {
            let text = py
                .import("os")
                .and_then(|os| os.call_method1("strerror", (code,)))
                .and_then(|t| t.extract::<String>())
                .unwrap_or_else(|_| err.to_string());
            PyOSError::new_err((code, text))
        }
        None => PyOSError::new_err(err.to_string()),
    }
}

// ------------------------------------------------------------- serial ----

/// A serial port opened straight at its baud rate, with pyserial's
/// `timeout` and `write_timeout`.
#[pyclass(module = "corvus_link.rtk", frozen)]
pub struct SerialPort {
    port: Serial,
    timeout: f64,
    write_timeout: Option<f64>,
}

#[pymethods]
impl SerialPort {
    #[new]
    #[pyo3(signature = (port, baudrate, timeout = 0.5, write_timeout = None))]
    fn new(py: Python<'_>, port: String, baudrate: u32, timeout: f64, write_timeout: Option<f64>) -> PyResult<Self> {
        let device = port.clone();
        match py.allow_threads(move || Serial::open_direct(&device, baudrate)) {
            Ok(port) => Ok(SerialPort {
                port,
                timeout,
                write_timeout,
            }),
            Err(e) => Err(match e.errno {
                Some(code) => PyOSError::new_err((code, e.message)),
                None => PyOSError::new_err(e.message),
            }),
        }
    }

    #[getter]
    fn in_waiting(&self) -> PyResult<usize> {
        self.port.waiting().map_err(|e| PyOSError::new_err(e.to_string()))
    }

    #[pyo3(signature = (size = 1))]
    fn read<'py>(&self, py: Python<'py>, size: usize) -> PyResult<Bound<'py, PyBytes>> {
        let timeout = self.timeout;
        let data = py
            .allow_threads(|| self.port.read_timeout(size, timeout))
            .map_err(|e| PyOSError::new_err(e.to_string()))?;
        Ok(PyBytes::new(py, &data))
    }

    fn write(&self, py: Python<'_>, data: &Bound<'_, PyAny>) -> PyResult<usize> {
        let data = bytes_of(data)?;
        let timeout = self.write_timeout;
        py.allow_threads(|| match timeout {
            Some(t) => self.port.write_timeout(&data, t),
            None => self.port.write(&data),
        })
        .map_err(|e| PyOSError::new_err(e.to_string()))
    }

    fn reset_input_buffer(&self) -> PyResult<()> {
        self.port.reset_input().map_err(|e| PyOSError::new_err(e.to_string()))
    }

    fn flush(&self) {}

    fn close(&self) {
        self.port.close();
    }

    #[getter]
    fn port(&self) -> String {
        self.port.device.clone()
    }

    #[getter]
    fn baudrate(&self) -> u32 {
        self.port.baud
    }
}

// ---------------------------------------------------------------- TCP ----

/// One TCP connection with the socket module's `recv`, `sendall`,
/// `settimeout` and `close`; closing it from another thread ends a read.
#[pyclass(module = "corvus_link.rtk", frozen)]
pub struct TcpSocket {
    stream: Mutex<Option<Arc<TcpStream>>>,
    timeout: Mutex<Option<f64>>,
}

impl TcpSocket {
    fn stream(&self) -> io::Result<Arc<TcpStream>> {
        lock(&self.stream)
            .clone()
            .ok_or_else(|| io::Error::from_raw_os_error(9))
    }

    fn apply_timeout(&self, stream: &TcpStream) -> io::Result<()> {
        let t = (*lock(&self.timeout)).map(|t| Duration::from_secs_f64(t.max(1e-6)));
        stream.set_read_timeout(t)?;
        stream.set_write_timeout(t)
    }
}

#[pymethods]
impl TcpSocket {
    fn recv<'py>(&self, py: Python<'py>, bufsize: usize) -> PyResult<Bound<'py, PyBytes>> {
        let stream = self.stream().map_err(|e| os_error(py, &e))?;
        self.apply_timeout(&stream).map_err(|e| os_error(py, &e))?;
        let mut buf = vec![0u8; bufsize.max(1)];
        match py.allow_threads(|| (&*stream).read(&mut buf)) {
            Ok(n) => Ok(PyBytes::new(py, &buf[..n])),
            Err(e) => Err(os_error(py, &e)),
        }
    }

    fn sendall(&self, py: Python<'_>, data: &Bound<'_, PyAny>) -> PyResult<()> {
        let data = bytes_of(data)?;
        let stream = self.stream().map_err(|e| os_error(py, &e))?;
        self.apply_timeout(&stream).map_err(|e| os_error(py, &e))?;
        py.allow_threads(|| (&*stream).write_all(&data))
            .map_err(|e| os_error(py, &e))
    }

    fn settimeout(&self, value: Option<f64>) {
        *lock(&self.timeout) = value;
    }

    fn gettimeout(&self) -> Option<f64> {
        *lock(&self.timeout)
    }

    fn close(&self) {
        if let Some(stream) = lock(&self.stream).take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __exit__(&self, _a: &Bound<'_, PyAny>, _b: &Bound<'_, PyAny>, _c: &Bound<'_, PyAny>) {
        self.close();
    }
}

fn address_of(sockaddr: &Bound<'_, PyAny>) -> PyResult<SocketAddr> {
    let host: String = sockaddr.get_item(0)?.extract()?;
    let port: u16 = sockaddr.get_item(1)?.extract()?;
    let host = host.split('%').next().unwrap_or("").to_string();
    let ip: IpAddr = host
        .parse()
        .map_err(|_| PyOSError::new_err(format!("unusable address {host}")))?;
    Ok(match ip {
        IpAddr::V6(v6) => {
            let scope: u32 = if sockaddr.len()? >= 4 {
                sockaddr.get_item(3)?.extract()?
            } else {
                0
            };
            SocketAddr::V6(SocketAddrV6::new(v6, port, 0, scope))
        }
        v4 => SocketAddr::new(v4, port),
    })
}

/// `socket.create_connection((host, port), timeout)`: every address the
/// name resolves to in turn, the last error if none answers.
#[pyfunction]
pub fn connect_tcp(
    py: Python<'_>,
    host: &Bound<'_, PyAny>,
    port: &Bound<'_, PyAny>,
    timeout: f64,
) -> PyResult<TcpSocket> {
    let socket = py.import("socket")?;
    let infos = socket.call_method1("getaddrinfo", (host, port, 0, socket.getattr("SOCK_STREAM")?))?;
    let mut last: Option<PyErr> = None;
    for info in infos.try_iter()? {
        let addr = address_of(&info?.get_item(4)?)?;
        let wait = Duration::from_secs_f64(timeout.max(1e-6));
        match py.allow_threads(|| TcpStream::connect_timeout(&addr, wait)) {
            Ok(stream) => {
                return Ok(TcpSocket {
                    stream: Mutex::new(Some(Arc::new(stream))),
                    timeout: Mutex::new(Some(timeout)),
                });
            }
            Err(e) => last = Some(os_error(py, &e)),
        }
    }
    Err(last.unwrap_or_else(|| PyOSError::new_err("getaddrinfo returns an empty list")))
}

// --------------------------------------------------------- transports ----

/// A serial port, opened read/write at one baud rate.
#[pyclass(module = "corvus_link.rtk")]
pub struct SerialTransport {
    port: PyObject,
    #[pyo3(get)]
    device: PyObject,
    #[pyo3(get)]
    baud: PyObject,
    #[pyo3(get, set)]
    label: String,
    #[pyo3(get, set)]
    pending: PyObject,
}

#[pymethods]
impl SerialTransport {
    #[new]
    fn new(py: Python<'_>, port: PyObject, device: PyObject, baud: PyObject) -> PyResult<Self> {
        let label = format!("{} @ {}", device.bind(py).str()?, baud.bind(py).str()?);
        Ok(SerialTransport {
            port,
            device,
            baud,
            label,
            pending: PyBytes::new(py, b"").into_any().unbind(),
        })
    }

    #[classattr]
    fn configurable() -> bool {
        true
    }

    #[getter]
    fn _port(&self, py: Python<'_>) -> PyObject {
        self.port.clone_ref(py)
    }

    fn read<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let port = self.port.bind(py);
        let waiting = match port.getattr("in_waiting") {
            Ok(v) if !v.is_truthy().unwrap_or(false) => 0,
            Ok(v) => py
                .get_type::<pyo3::types::PyInt>()
                .call1((v,))
                .and_then(|i| i.extract::<i64>())
                .unwrap_or(0),
            Err(err) if err.is_instance_of::<pyo3::exceptions::PyAttributeError>(py) => 0,
            Err(err) if err.is_instance_of::<pyo3::exceptions::PyException>(py) => 0,
            Err(err) => return Err(err),
        };
        match port.call_method1("read", (waiting.clamp(1, 4096),)) {
            Ok(data) => Ok(if data.is_truthy()? {
                data
            } else {
                PyBytes::new(py, b"").into_any()
            }),
            Err(err) if err.is_instance_of::<pyo3::exceptions::PyException>(py) => {
                Err(rtk_error(py, format!("read failed: {}", err.value(py)), Some(&err)))
            }
            Err(err) => Err(err),
        }
    }

    fn write(&self, py: Python<'_>, data: &Bound<'_, PyAny>) -> PyResult<()> {
        let port = self.port.bind(py);
        let result = (|| -> PyResult<()> {
            port.call_method1("write", (data,))?;
            let flush = match port.getattr("flush") {
                Ok(f) => f,
                Err(err) if err.is_instance_of::<pyo3::exceptions::PyAttributeError>(py) => return Ok(()),
                Err(err) => return Err(err),
            };
            if flush.is_callable() {
                flush.call0()?;
            }
            Ok(())
        })();
        match result {
            Err(err) if err.is_instance_of::<pyo3::exceptions::PyException>(py) => {
                Err(rtk_error(py, format!("write failed: {}", err.value(py)), Some(&err)))
            }
            other => other,
        }
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        match self.port.bind(py).call_method0("close") {
            Err(err) if !err.is_instance_of::<pyo3::exceptions::PyException>(py) => Err(err),
            _ => Ok(()),
        }
    }
}

/// A socket holding one NTRIP caster stream open.
#[pyclass(module = "corvus_link.rtk")]
pub struct NtripTransport {
    sock: PyObject,
    #[pyo3(get, set)]
    label: PyObject,
    #[pyo3(get, set)]
    pending: PyObject,
}

#[pymethods]
impl NtripTransport {
    #[new]
    fn new(py: Python<'_>, sock: PyObject, label: PyObject) -> Self {
        NtripTransport {
            sock,
            label,
            pending: PyBytes::new(py, b"").into_any().unbind(),
        }
    }

    #[classattr]
    fn configurable() -> bool {
        false
    }

    #[getter]
    fn _sock(&self, py: Python<'_>) -> PyObject {
        self.sock.clone_ref(py)
    }

    fn read<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self.sock.bind(py).call_method1("recv", (NTRIP_READ_BYTES,)) {
            Ok(data) => Ok(if data.is_truthy()? {
                data
            } else {
                PyBytes::new(py, b"").into_any()
            }),
            Err(err) if err.is_instance_of::<PyTimeoutError>(py) => Ok(PyBytes::new(py, b"").into_any()),
            Err(err) if err.is_instance_of::<PyOSError>(py) => Err(rtk_error(
                py,
                format!("caster read failed: {}", err.value(py)),
                Some(&err),
            )),
            Err(err) => Err(err),
        }
    }

    /// A caster takes nothing from a ground station relaying to a rover.
    fn write(&self, _data: &Bound<'_, PyAny>) {}

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        match self.sock.bind(py).call_method0("close") {
            Err(err) if !err.is_instance_of::<pyo3::exceptions::PyException>(py) => Err(err),
            _ => Ok(()),
        }
    }
}

// -------------------------------------------------------------- NTRIP ----

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((*chunk.get(1).unwrap_or(&0) as u32) << 8)
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The GET a caster expects, as NTRIP 2.0 with the 1.0 answer accepted.
#[pyfunction]
pub fn ntrip_request<'py>(
    host: &Bound<'py, PyAny>,
    port: &Bound<'py, PyAny>,
    mountpoint: &Bound<'py, PyAny>,
    username: &Bound<'py, PyAny>,
    password: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyBytes>> {
    let py = host.py();
    let version = py.import("corvus.version")?.call_method0("get_version")?;
    let template = "GET /{2} HTTP/1.1\r\nHost: {0}:{1}\r\nNtrip-Version: Ntrip/2.0\r\nUser-Agent: NTRIP CorvusGCS/{3}\r\nAccept: */*\r\nConnection: close";
    let mut text = PyString::new(py, template).call_method1("format", (host, port, mountpoint, version))?;
    if username.is_truthy()? || password.is_truthy()? {
        let raw = PyString::new(py, "{}:{}")
            .call_method1("format", (username, password))?
            .call_method0("encode")?;
        let line = format!(
            "\r\nAuthorization: Basic {}",
            base64(raw.downcast::<PyBytes>()?.as_bytes())
        );
        text = text.add(line)?;
    }
    let text = text.add("\r\n\r\n")?;
    Ok(text
        .call_method1("encode", ("ascii", "replace"))?
        .downcast_into::<PyBytes>()?)
}

fn is_py_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\x1c'..='\x1f' | ' ' | '\u{85}' | '\u{a0}'
    )
}

/// Consume the caster's response header and return the bytes after it;
/// *clock* is the `time` module the deadline is read from.
#[pyfunction]
pub fn read_ntrip_header<'py>(
    sock: &Bound<'py, PyAny>,
    clock: &Bound<'py, PyAny>,
    timeout: f64,
) -> PyResult<Bound<'py, PyBytes>> {
    let py = sock.py();
    let mut buf: Vec<u8> = Vec::new();
    let deadline = clock.call_method0("monotonic")?.extract::<f64>()? + timeout;
    while clock.call_method0("monotonic")?.extract::<f64>()? < deadline {
        let chunk = match sock.call_method1("recv", (NTRIP_READ_BYTES,)) {
            Ok(c) => c,
            Err(err) if err.is_instance_of::<PyTimeoutError>(py) => continue,
            Err(err) if err.is_instance_of::<PyOSError>(py) => {
                return Err(rtk_error(
                    py,
                    format!("the caster closed the connection: {}", err.value(py)),
                    Some(&err),
                ));
            }
            Err(err) => return Err(err),
        };
        if !chunk.is_truthy()? {
            return Err(RtkError::new_err("the caster closed the connection without answering"));
        }
        let joined = PyBytes::new(py, &buf).add(&chunk)?;
        buf = bytes_of(&joined)?;
        let Some(split) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            if buf.len() > 8192 {
                return Err(RtkError::new_err("the caster sent no usable response"));
            }
            continue;
        };
        let head: String = buf[..split].iter().map(|&b| b as char).collect();
        let rest = &buf[split + 4..];
        let first = head.split("\r\n").next().unwrap_or("");
        if first.contains("200") || first.to_uppercase().starts_with("ICY 200") {
            return Ok(PyBytes::new(py, rest));
        }
        if first.contains("401") {
            return Err(RtkError::new_err("the caster rejected the username or password"));
        }
        if first.contains("404") {
            return Err(RtkError::new_err("the caster has no such mountpoint"));
        }
        let shown = first.trim_matches(is_py_space);
        let shown = if shown.is_empty() { "no response" } else { shown };
        return Err(RtkError::new_err(format!("the caster refused the stream: {shown}")));
    }
    Err(RtkError::new_err("the caster did not answer in time"))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("RtkError", m.py().get_type::<RtkError>())?;
    m.add_class::<SerialPort>()?;
    m.add_class::<TcpSocket>()?;
    m.add_class::<SerialTransport>()?;
    m.add_class::<NtripTransport>()?;
    m.add_function(wrap_pyfunction!(connect_tcp, m)?)?;
    m.add_function(wrap_pyfunction!(ntrip_request, m)?)?;
    m.add_function(wrap_pyfunction!(read_ntrip_header, m)?)?;
    Ok(())
}
