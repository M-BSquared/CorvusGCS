//! Firmware flashing, as ``corvus_link.flash``: the PX4 bootloader protocol
//! (`UploaderEngine`: sync, identify, board check, erase, program, verify,
//! reboot) and the flash service around it (`FlashEngine`: the safety gates,
//! the download and flash workers on `CoreThread`s, the bridge hand-over).
//!
//! The image container (JSON, ZIP, raw) and the release catalogues stay in
//! Python. State stays on the Python objects under their own locks; every
//! step is called through them, so a step a caller replaces is the one that
//! runs. A failed or cancelled upload never sends REBOOT, so the board stays
//! in its bootloader for a retry.

use pyo3::exceptions::{PyException, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyCFunction, PyDict, PyList, PyModule, PyString, PyTuple};

use crate::bridge::receive::within;
use crate::bridge::thread;
use crate::protocol::host::caught_by;
use crate::rtk::wire::bytes_of;

const INSYNC: u8 = 0x12;
const EOC: u8 = 0x20;
const OK: u8 = 0x10;
const BAD_SILICON: u8 = 0x14;
const GET_SYNC: u8 = 0x21;
const GET_DEVICE: u8 = 0x22;
const CHIP_ERASE: u8 = 0x23;
const PROG_MULTI: u8 = 0x27;
const GET_CRC: u8 = 0x29;
const GET_CHIP: u8 = 0x2c;
const REBOOT: u8 = 0x30;
const INFO_BOARD_ID: u8 = 0x02;
const INFO_FLASH_SIZE: u8 = 0x04;

const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

static CRC32_TABLE: [u32; 256] = crc32_table();

fn fold(state: u32, data: &[u8]) -> u32 {
    data.iter().fold(state, |crc, &b| {
        CRC32_TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8)
    })
}

/// The bootloader's CRC-32: the reflected table, started at *state*, no
/// final XOR.
#[pyfunction]
#[pyo3(signature = (data, *state))]
fn bl_crc32(data: &Bound<'_, PyAny>, state: &Bound<'_, PyTuple>) -> PyResult<u32> {
    let masked: u32 = match state.len() {
        0 => 0,
        1 => state.get_item(0)?.bitand(0xFFFF_FFFFu32)?.extract()?,
        n => {
            return Err(pyo3::exceptions::PyTypeError::new_err(format!(
                "bl_crc32() takes at most 2 arguments ({} given)",
                n + 1
            )))
        }
    };
    let buffer: pyo3::buffer::PyBuffer<u8> = pyo3::buffer::PyBuffer::get(data).map_err(|_| {
        pyo3::exceptions::PyTypeError::new_err(format!(
            "a bytes-like object is required, not '{}'",
            data.get_type().name().map(|n| n.to_string()).unwrap_or_default()
        ))
    })?;
    Ok(fold(masked, &buffer.to_vec(data.py())?))
}

fn kw<'py>(py: Python<'py>, pairs: &[(&str, PyObject)]) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    for (k, v) in pairs {
        d.set_item(*k, v)?;
    }
    Ok(d)
}

fn fmt<'py, A: pyo3::call::PyCallArgs<'py>>(py: Python<'py>, template: &str, args: A) -> PyResult<Bound<'py, PyAny>> {
    PyString::new(py, template).call_method1("format", args)
}

fn is_exception(py: Python<'_>, err: &PyErr) -> bool {
    err.is_instance_of::<PyException>(py)
}

// -------------------------------------------------------------- uploader ----

/// The bootloader dialogue, run against a ``FirmwareUploader``.
#[pyclass(name = "UploaderEngine", module = "corvus_link.flash", frozen)]
pub struct UploaderEngine {
    env: PyObject,
}

impl UploaderEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn constant(&self, py: Python<'_>, name: &str) -> PyResult<f64> {
        self.env(py).getattr(name)?.extract()
    }

    fn now(&self, py: Python<'_>) -> PyResult<f64> {
        self.env(py).getattr("time")?.call_method0("monotonic")?.extract()
    }

    fn sleep(&self, py: Python<'_>, seconds: f64) -> PyResult<()> {
        self.env(py).getattr("time")?.call_method1("sleep", (seconds,))?;
        Ok(())
    }

    fn logger<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr("logger")
    }

    fn cancelled(&self, up: &Bound<'_, PyAny>) -> PyResult<bool> {
        up.getattr("_cancel")?.call_method0("is_set")?.is_truthy()
    }

    fn serial<'py>(&self, up: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        up.getattr("_serial")
    }

    /// `serial.write(bytes); serial.flush()`.
    fn send(&self, up: &Bound<'_, PyAny>, data: &[u8]) -> PyResult<()> {
        let port = self.serial(up)?;
        port.call_method1("write", (PyBytes::new(up.py(), data),))?;
        port.call_method0("flush")?;
        Ok(())
    }

    fn set(
        &self,
        up: &Bound<'_, PyAny>,
        state: &str,
        percent: &Bound<'_, PyAny>,
        message: Bound<'_, PyAny>,
    ) -> PyResult<()> {
        up.call_method1("_set", (state, percent, message))?;
        Ok(())
    }

    fn timed<'py>(
        &self,
        up: &Bound<'py, PyAny>,
        name: &str,
        args: impl pyo3::call::PyCallArgs<'py>,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = up.py();
        let kwargs = kw(py, &[("timeout_s", timeout.into_pyobject(py)?.into_any().unbind())])?;
        up.getattr(name)?.call(args, Some(&kwargs))
    }
}

#[pymethods]
impl UploaderEngine {
    /// *env* is ``corvus.firmware_uploader``.
    #[new]
    fn new(env: PyObject) -> Self {
        UploaderEngine { env }
    }

    /// Read exactly *n* bytes within *timeout_s*, honouring cancel; None on
    /// timeout or cancel.
    fn recv_exact<'py>(
        &self,
        up: &Bound<'py, PyAny>,
        n: usize,
        timeout_s: f64,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let py = up.py();
        let deadline = self.now(py)? + timeout_s;
        let mut buf: Vec<u8> = Vec::new();
        while buf.len() < n {
            if self.cancelled(up)? {
                return Ok(None);
            }
            if self.now(py)? >= deadline {
                break;
            }
            let chunk = match self.serial(up)?.call_method1("read", (n - buf.len(),)) {
                Ok(c) => c,
                Err(err) if is_exception(py, &err) => return Ok(None),
                Err(err) => return Err(err),
            };
            if chunk.is_truthy()? {
                buf.extend(bytes_of(&chunk)?);
            }
        }
        Ok(if buf.len() == n {
            Some(PyBytes::new(py, &buf))
        } else {
            None
        })
    }

    /// Read a 2-byte ``INSYNC <status>`` reply; True on OK.
    fn expect_insync_ok(&self, up: &Bound<'_, PyAny>, timeout_s: f64) -> PyResult<bool> {
        let py = up.py();
        let reply = up.call_method1("_recv_exact", (2, timeout_s))?;
        if reply.is_none() {
            return Ok(false);
        }
        let b = bytes_of(&reply)?;
        if b.len() < 2 || b[0] != INSYNC {
            return Ok(false);
        }
        if b[1] == BAD_SILICON {
            self.logger(py)?
                .call_method1("error", ("bootloader reports bad silicon (F4 < Rev 3)",))?;
            return Ok(false);
        }
        Ok(b[1] == OK)
    }

    /// Discard pending input so the next command starts in sync.
    fn drain(&self, up: &Bound<'_, PyAny>) -> PyResult<()> {
        match self.serial(up)?.call_method0("reset_input_buffer") {
            Err(err) if !is_exception(up.py(), &err) => Err(err),
            _ => Ok(()),
        }
    }

    /// Poll for the device then open it, retrying on a transient busy.
    fn open_device(&self, up: &Bound<'_, PyAny>, device: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = up.py();
        let env = self.env(py);
        let serial = env.getattr("_serial")?;
        if serial.is_none() {
            return Err(PyRuntimeError::new_err(
                "pyserial is not available; cannot open the bootloader device",
            ));
        }
        let deadline = self.now(py)? + up.getattr("_connect_timeout_s")?.extract::<f64>()?;
        while self.now(py)? < deadline && !self.cancelled(up)? {
            if up.call_method1("_device_present", (device,))?.is_truthy()? {
                for _ in 0..5 {
                    if self.cancelled(up)? {
                        return Ok(false);
                    }
                    let kwargs = kw(
                        py,
                        &[
                            ("baudrate", up.getattr("_baud")?.unbind()),
                            (
                                "timeout",
                                self.constant(py, "READ_TIMEOUT_S")?
                                    .into_pyobject(py)?
                                    .into_any()
                                    .unbind(),
                            ),
                        ],
                    )?;
                    match serial.call_method("Serial", (device,), Some(&kwargs)) {
                        Ok(port) => {
                            up.setattr("_serial", port)?;
                            return Ok(true);
                        }
                        Err(err) if is_exception(py, &err) => self.sleep(py, 0.2)?,
                        Err(err) => return Err(err),
                    }
                }
                return Ok(false);
            }
            self.sleep(py, 0.1)?;
        }
        Ok(false)
    }

    /// Establish sync with the bootloader (GET_SYNC, retried a few times).
    fn sync(&self, up: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = up.py();
        for _ in 0..5 {
            if self.cancelled(up)? {
                return Ok(false);
            }
            up.call_method0("_drain")?;
            match self.send(up, &[GET_SYNC, EOC]) {
                Err(err) if is_exception(py, &err) => return Ok(false),
                Err(err) => return Err(err),
                Ok(()) => {}
            }
            if self.timed(up, "_expect_insync_ok", (), 1.0)?.is_truthy()? {
                return Ok(true);
            }
            self.sleep(py, 0.1)?;
        }
        Ok(false)
    }

    /// One GET_DEVICE query: the 4-byte value, or None when not answered.
    fn get_device_info<'py>(&self, up: &Bound<'py, PyAny>, info_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = up.py();
        let attempt = (|| -> PyResult<Option<Bound<'py, PyAny>>> {
            let id: u8 = info_id
                .extract()
                .map_err(|_| pyo3::exceptions::PyValueError::new_err("bytes must be in range(0, 256)"))?;
            self.send(up, &[GET_DEVICE, id, EOC])?;
            let raw = self.timed(up, "_recv_exact", (4,), self.constant(py, "INFO_TIMEOUT_S")?)?;
            if !raw.is_none() && self.timed(up, "_expect_insync_ok", (), 1.0)?.is_truthy()? {
                return Ok(Some(
                    py.get_type::<pyo3::types::PyInt>()
                        .call_method1("from_bytes", (raw, "little"))?,
                ));
            }
            Ok(None)
        })();
        match attempt {
            Ok(Some(value)) => return Ok(value),
            Ok(None) => {}
            Err(err) if is_exception(py, &err) => {}
            Err(err) => return Err(err),
        }
        up.call_method0("_drain")?;
        Ok(py.None().into_bound(py))
    }

    /// Fetch board info for the board check and the verify CRC; every
    /// failure is swallowed and the input drained afterwards.
    fn identify_best_effort(&self, up: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = up.py();
        let logger = self.logger(py)?;
        let board_id = up.call_method1("_get_device_info", (INFO_BOARD_ID,))?;
        if board_id.is_none() {
            logger.call_method1(
                "warning",
                ("bootloader did not report a board id; board check skipped",),
            )?;
        } else {
            up.setattr("_board_id", &board_id)?;
            logger.call_method1("info", ("bootloader board id: %d", &board_id))?;
        }
        let size = up.call_method1("_get_device_info", (INFO_FLASH_SIZE,))?;
        if !size.is_none() {
            if size.gt(0)? && size.le(self.env(py).getattr("MAX_BOOTLOADER_FLASH_BYTES")?)? {
                up.setattr("_fw_maxsize", &size)?;
            } else {
                logger.call_method1("error", ("bootloader reported invalid flash size: %d", &size))?;
            }
        }
        let chip = (|| -> PyResult<()> {
            self.send(up, &[GET_CHIP, EOC])?;
            let chip = self.timed(up, "_recv_exact", (4,), 1.0)?;
            if !chip.is_none() {
                self.timed(up, "_recv_exact", (2,), 0.5)?;
                logger.call_method1("info", ("bootloader chip id: %s", chip.call_method0("hex")?))?;
            }
            Ok(())
        })();
        match chip {
            Err(err) if !is_exception(py, &err) => return Err(err),
            _ => {}
        }
        up.call_method0("_drain")?;
        Ok(())
    }

    /// The operator message when the image is for another board, else None.
    fn board_refusal<'py>(&self, up: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = up.py();
        let (board, wanted) = (up.getattr("_board_id")?, up.getattr("_image_board_id")?);
        let none = py.None().into_bound(py);
        if board.is_none() || wanted.is_none() {
            return Ok(none);
        }
        let logger = self.logger(py)?;
        if self
            .env(py)
            .call_method1("board_ids_compatible", (&board, &wanted))?
            .is_truthy()?
        {
            if board.ne(&wanted)? {
                logger.call_method1(
                    "info",
                    ("board id %d accepts firmware for board id %d", &board, &wanted),
                )?;
            }
            return Ok(none);
        }
        logger.call_method1(
            "error",
            (
                "firmware board id %d does not match bootloader board id %d",
                &wanted,
                &board,
            ),
        )?;
        fmt(
            py,
            "This firmware is for board id {}, but the connected bootloader reports board id {}. Nothing was erased.",
            (&wanted, &board),
        )
    }

    /// Send CHIP_ERASE and wait for INSYNC OK.
    fn erase(&self, up: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = up.py();
        match self.send(up, &[CHIP_ERASE, EOC]) {
            Err(err) if is_exception(py, &err) => return Ok(false),
            Err(err) => return Err(err),
            Ok(()) => {}
        }
        self.timed(up, "_expect_insync_ok", (), self.constant(py, "ERASE_TIMEOUT_S")?)?
            .is_truthy()
    }

    /// Stream the image in PROG_MULTI blocks, checking cancel between each.
    fn program(&self, up: &Bound<'_, PyAny>, image: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = up.py();
        let data = bytes_of(image)?;
        let total = data.len();
        let block_max = self.env(py).getattr("PROG_MULTI_MAX")?.extract::<usize>()?;
        let (mut offset, mut last_pct) = (0usize, -1i64);
        while offset < total {
            if self.cancelled(up)? {
                self.set(
                    up,
                    "cancelled",
                    &up.getattr("_percent")?,
                    PyString::new(py, "Flash cancelled").into_any(),
                )?;
                return Ok(false);
            }
            let block = &data[offset..(offset + block_max).min(total)];
            let mut frame = vec![PROG_MULTI, block.len() as u8];
            frame.extend_from_slice(block);
            frame.push(EOC);
            let failed = |at: usize| -> PyResult<Bound<'_, PyAny>> { fmt(py, "program failed at offset {}", (at,)) };
            match self.send(up, &frame) {
                Err(err) if is_exception(py, &err) => {
                    self.set(up, "failed", &up.getattr("_percent")?, failed(offset)?)?;
                    return Ok(false);
                }
                Err(err) => return Err(err),
                Ok(()) => {}
            }
            if !self.timed(up, "_expect_insync_ok", (), 5.0)?.is_truthy()? {
                if self.cancelled(up)? {
                    self.set(
                        up,
                        "cancelled",
                        &up.getattr("_percent")?,
                        PyString::new(py, "Flash cancelled").into_any(),
                    )?;
                } else {
                    self.set(up, "failed", &up.getattr("_percent")?, failed(offset)?)?;
                }
                return Ok(false);
            }
            offset += block.len();
            let pct = (100.0 * offset as f64 / total as f64).round_ties_even() as i64;
            if pct != last_pct {
                last_pct = pct;
                self.set(
                    up,
                    "programming",
                    &pct.into_pyobject(py)?.into_any(),
                    fmt(py, "Programming\u{2026} {}%", (pct,))?,
                )?;
            }
        }
        Ok(true)
    }

    /// The bootloader CRC over the image padded with 0xFF to fw_maxsize.
    fn compute_expected_crc(&self, up: &Bound<'_, PyAny>, image: &Bound<'_, PyAny>) -> PyResult<u32> {
        let data = bytes_of(image)?;
        let mut state = fold(0, &data);
        let max = up.getattr("_fw_maxsize")?;
        if !max.is_none() && max.gt(data.len())? {
            let mut remaining = max.extract::<u64>()? - data.len() as u64;
            let padding = [0xFFu8; 65536];
            while remaining > 0 {
                let n = remaining.min(65536) as usize;
                state = fold(state, &padding[..n]);
                remaining -= n as u64;
            }
        }
        Ok(state)
    }

    /// Compare the bootloader's flash CRC to the expected one: True on a
    /// match, False on a mismatch, None when it could not be checked.
    fn verify<'py>(&self, up: &Bound<'py, PyAny>, image: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = up.py();
        let none = py.None().into_bound(py);
        let logger = self.logger(py)?;
        match self.send(up, &[GET_CRC, EOC]) {
            Err(err) if is_exception(py, &err) => return Ok(none),
            Err(err) => return Err(err),
            Ok(()) => {}
        }
        let crc = self.timed(up, "_recv_exact", (4,), 5.0)?;
        if crc.is_none() {
            logger.call_method1("warning", ("bootloader did not return a CRC; skipping verify",))?;
            return Ok(none);
        }
        if !self.timed(up, "_expect_insync_ok", (), 2.0)?.is_truthy()? {
            logger.call_method1("warning", ("GET_CRC missing INSYNC/OK; skipping verify",))?;
            return Ok(none);
        }
        let max = up.getattr("_fw_maxsize")?;
        if max.is_none() || max.le(0)? {
            logger.call_method1("warning", ("board fw_maxsize unknown; skipping CRC verify",))?;
            return Ok(none);
        }
        let reported = py
            .get_type::<pyo3::types::PyInt>()
            .call_method1("from_bytes", (crc, "little"))?;
        let expected = up.call_method1("_compute_expected_crc", (image,))?;
        if reported.eq(&expected)? {
            return Ok(true.into_pyobject(py)?.to_owned().into_any());
        }
        logger.call_method1(
            "error",
            (
                "firmware CRC mismatch: got 0x%08x expected 0x%08x",
                &reported,
                &expected,
            ),
        )?;
        Ok(false.into_pyobject(py)?.to_owned().into_any())
    }

    /// Send REBOOT; whether the command reached the serial link.
    fn reboot(&self, up: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = up.py();
        match self.send(up, &[REBOOT, EOC]) {
            Err(err) if is_exception(py, &err) => return Ok(false),
            Err(err) => return Err(err),
            Ok(()) => {}
        }
        self.timed(up, "_expect_insync_ok", (), 2.0)?;
        Ok(true)
    }

    /// The whole sequence; RESET only after a clean program and verify.
    fn run(&self, up: &Bound<'_, PyAny>, device: &Bound<'_, PyAny>, image: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = up.py();
        let zero = 0i64.into_pyobject(py)?.into_any();
        let text = |s: &str| PyString::new(py, s).into_any();
        let cancelled = |this: &Self| -> PyResult<bool> {
            if this.cancelled(up)? {
                this.set(up, "cancelled", &zero, text("Flash cancelled"))?;
                return Ok(true);
            }
            Ok(false)
        };
        if cancelled(self)? {
            return Ok(false);
        }
        self.set(
            up,
            "connecting",
            &zero,
            fmt(py, "Waiting for bootloader on {}\u{2026}", (device,))?,
        )?;
        if !up.call_method1("_open_device", (device,))?.is_truthy()? {
            if !cancelled(self)? {
                self.set(
                    up,
                    "failed",
                    &zero,
                    fmt(py, "bootloader device not available: {}", (device,))?,
                )?;
            }
            return Ok(false);
        }
        self.set(up, "syncing", &zero, text("Synchronising with bootloader\u{2026}"))?;
        if !up.call_method0("_sync")?.is_truthy()? {
            self.set(up, "failed", &zero, text("bootloader sync failed"))?;
            return Ok(false);
        }
        up.call_method0("_identify_best_effort")?;
        let refusal = up.call_method0("_board_refusal")?;
        if refusal.is_truthy()? {
            self.set(up, "failed", &zero, refusal)?;
            return Ok(false);
        }
        let max = up.getattr("_fw_maxsize")?;
        if max.is_none() {
            self.set(up, "failed", &zero, text("could not determine bootloader flash size"))?;
            return Ok(false);
        }
        if image.len()? > max.extract::<usize>()? {
            let message = fmt(
                py,
                "firmware image ({} bytes) exceeds board flash limit ({} bytes)",
                (image.len()?, &max),
            )?;
            self.set(up, "failed", &zero, message)?;
            return Ok(false);
        }
        if cancelled(self)? {
            return Ok(false);
        }
        self.set(up, "erasing", &zero, text("Erasing\u{2026}"))?;
        if !up.call_method0("_erase")?.is_truthy()? {
            if !cancelled(self)? {
                self.set(up, "failed", &zero, text("erase failed"))?;
            }
            return Ok(false);
        }
        self.set(up, "programming", &zero, text("Programming\u{2026}"))?;
        if !up.call_method1("_program", (image,))?.is_truthy()? {
            return Ok(false);
        }
        self.set(up, "verifying", &up.getattr("_percent")?, text("Verifying\u{2026}"))?;
        let verify = up.call_method1("_verify", (image,))?;
        if !verify.is(true.into_pyobject(py)?.to_owned().into_any()) {
            let message = if verify.is(false.into_pyobject(py)?.to_owned().into_any()) {
                "firmware CRC mismatch, not booting"
            } else {
                "firmware CRC verification unavailable, not booting"
            };
            self.set(up, "failed", &up.getattr("_percent")?, text(message))?;
            return Ok(false);
        }
        if self.cancelled(up)? {
            self.set(up, "cancelled", &up.getattr("_percent")?, text("Flash cancelled"))?;
            return Ok(false);
        }
        let hundred = 100i64.into_pyobject(py)?.into_any();
        self.set(up, "booting", &hundred, text("Booting new firmware\u{2026}"))?;
        if !up.call_method0("_reboot")?.is_truthy()? {
            self.set(
                up,
                "failed",
                &up.getattr("_percent")?,
                text("bootloader reboot command failed"),
            )?;
            return Ok(false);
        }
        self.set(up, "done", &hundred, text("Firmware flashed successfully"))?;
        Ok(true)
    }
}

// --------------------------------------------------------------- service ----

enum PercentArg {
    Number(i64),
    Object(PyObject),
}

impl From<i64> for PercentArg {
    fn from(n: i64) -> Self {
        PercentArg::Number(n)
    }
}

impl From<i32> for PercentArg {
    fn from(n: i32) -> Self {
        PercentArg::Number(n as i64)
    }
}

impl From<PyObject> for PercentArg {
    fn from(o: PyObject) -> Self {
        PercentArg::Object(o)
    }
}

/// The flash service's gates and workers, run against a ``FlashService``.
#[pyclass(name = "FlashEngine", module = "corvus_link.flash", frozen)]
pub struct FlashEngine {
    env: PyObject,
}

impl FlashEngine {
    fn env<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.env.bind(py).clone()
    }

    fn logger<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.env(py).getattr("logger")
    }

    fn lock<T>(&self, svc: &Bound<'_, PyAny>, f: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
        within(&svc.getattr("_lock")?, f)
    }

    fn busy(&self, svc: &Bound<'_, PyAny>) -> PyResult<bool> {
        self.env(svc.py())
            .getattr("BUSY_STATES")?
            .contains(svc.getattr("_state")?)
    }

    fn log_error(&self, py: Python<'_>, text: &str, err: &PyErr) -> PyResult<()> {
        let kwargs = kw(py, &[("exc_info", err.value(py).clone().into_any().unbind())])?;
        self.logger(py)?.call_method("error", (text,), Some(&kwargs))?;
        Ok(())
    }

    fn log_debug(&self, py: Python<'_>, text: &str, err: &PyErr) -> PyResult<()> {
        let kwargs = kw(py, &[("exc_info", err.value(py).clone().into_any().unbind())])?;
        self.logger(py)?.call_method("debug", (text,), Some(&kwargs))?;
        Ok(())
    }

    fn set(
        &self,
        svc: &Bound<'_, PyAny>,
        state: &str,
        percent: impl Into<PercentArg>,
        message: Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = svc.py();
        let percent = match percent.into() {
            PercentArg::Number(n) => n.into_pyobject(py)?.into_any().unbind(),
            PercentArg::Object(o) => o,
        };
        svc.call_method1("_set", (state, percent, message))?;
        Ok(())
    }

    fn notify(&self, svc: &Bound<'_, PyAny>, state: &str, message: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        let entry = kw(
            py,
            &[
                ("state", state.into_pyobject(py)?.into_any().unbind()),
                ("percent", 0i64.into_pyobject(py)?.into_any().unbind()),
                ("message", message.clone().unbind()),
            ],
        )?;
        svc.call_method1("_notify_listeners", (entry,))?;
        Ok(())
    }

    fn gate_message<'py>(&self, svc: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = svc.py();
        let transport = svc.getattr("mavlink")?.call_method0("transport")?;
        let kwargs = kw(py, &[("transport", transport.unbind())])?;
        self.env(py)
            .getattr("NON_USB_GATE_MESSAGE")?
            .call_method("format", (), Some(&kwargs))
    }
}

#[pymethods]
impl FlashEngine {
    /// *env* is ``corvus.flash_service``.
    #[new]
    fn new(env: PyObject) -> Self {
        FlashEngine { env }
    }

    /// Serial ports whose descriptor says "flight controller in its bootloader".
    fn bootloader_ports<'py>(&self, svc: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
        let py = svc.py();
        let out = PyList::empty(py);
        let mut lister = svc.getattr("_list_ports")?;
        if !lister.is_truthy()? {
            lister = match svc.getattr("mavlink")?.getattr("list_serial_ports") {
                Ok(l) => l,
                Err(err) if caught_by(py, &err, &["AttributeError"]) => return Ok(out),
                Err(err) => return Err(err),
            };
        }
        let rows = match lister.call0() {
            Ok(r) => r,
            Err(err) if is_exception(py, &err) => {
                self.log_debug(py, "bootloader port enumeration failed", &err)?;
                return Ok(out);
            }
            Err(err) => return Err(err),
        };
        let rows = if rows.is_truthy()? {
            rows
        } else {
            PyList::empty(py).into_any()
        };
        let is_bootloader = self.env(py).getattr("is_bootloader_port")?;
        for row in rows.try_iter()? {
            let row = row?;
            let field = |name: &str| -> PyResult<Bound<'py, PyAny>> {
                let v = row.call_method1("get", (name, ""))?;
                let v = if v.is_truthy()? {
                    v
                } else {
                    PyString::new(py, "").into_any()
                };
                Ok(v.str()?.into_any())
            };
            if is_bootloader
                .call1((field("device")?, field("hwid")?, field("description")?))?
                .is_truthy()?
            {
                out.append(row)?;
            }
        }
        Ok(out)
    }

    /// The one bootloader port that may be flashed without a link, or "".
    fn bootloader_device<'py>(&self, svc: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = svc.py();
        let empty = PyString::new(py, "").into_any();
        if svc
            .getattr("_store")?
            .call_method0("get_snapshot")?
            .call_method1("get", ("connected",))?
            .is_truthy()?
        {
            return Ok(empty);
        }
        let ports = svc.call_method0("bootloader_ports")?;
        if ports.len()? != 1 {
            return Ok(empty);
        }
        let device = ports.get_item(0)?.call_method1("get", ("device", ""))?;
        let device = if device.is_truthy()? { device } else { empty };
        Ok(device.str()?.into_any())
    }

    /// The refusal reason, or None when a flash may start.
    fn gate<'py>(&self, svc: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = svc.py();
        let none = py.None().into_bound(py);
        let mavlink = svc.getattr("mavlink")?;
        if mavlink.is_none() {
            return Ok(PyString::new(py, "no autopilot connection").into_any());
        }
        if !svc.call_method0("bootloader_device")?.is_truthy()? {
            if !mavlink.call_method0("is_direct_usb")?.is_truthy()? {
                return self.gate_message(svc);
            }
            if svc
                .getattr("_store")?
                .call_method0("get_snapshot")?
                .call_method1("get", ("armed",))?
                .is_truthy()?
            {
                return Ok(PyString::new(py, "cannot flash while armed").into_any());
            }
        }
        if self.lock(svc, || self.busy(svc))? {
            return Ok(PyString::new(py, "flash already in progress").into_any());
        }
        Ok(none)
    }

    /// The gate minus the busy check: this job is the busy one.
    #[pyo3(signature = (svc, bootloader = None))]
    fn gate_for_resume<'py>(
        &self,
        svc: &Bound<'py, PyAny>,
        bootloader: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = svc.py();
        let none = py.None().into_bound(py);
        let mavlink = svc.getattr("mavlink")?;
        if mavlink.is_none() {
            return Ok(PyString::new(py, "no autopilot connection").into_any());
        }
        if let Some(b) = bootloader.filter(|b| b.is_truthy().unwrap_or(false)) {
            if svc.call_method0("bootloader_device")?.ne(b)? {
                return Ok(PyString::new(py, "the controller left its bootloader").into_any());
            }
            return Ok(none);
        }
        if !mavlink.call_method0("is_direct_usb")?.is_truthy()? {
            return self.gate_message(svc);
        }
        if svc
            .getattr("_store")?
            .call_method0("get_snapshot")?
            .call_method1("get", ("armed",))?
            .is_truthy()?
        {
            return Ok(PyString::new(py, "cannot flash while armed").into_any());
        }
        Ok(none)
    }

    /// Gate, parse the image, then start the flash worker.
    fn start(&self, svc: &Bound<'_, PyAny>, firmware_bytes: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = svc.py();
        let refusal = svc.call_method0("_gate")?;
        if refusal.is_truthy()? {
            svc.setattr("last_error", refusal)?;
            return Ok(false);
        }
        let parsed = match self.env(py).call_method1("parse_firmware_image", (firmware_bytes,)) {
            Ok(p) => p,
            Err(err) if caught_by(py, &err, &["ValueError"]) => {
                svc.setattr("last_error", err.value(py).str()?)?;
                return Ok(false);
            }
            Err(err) => return Err(err),
        };
        let (image, board_id) = (parsed.get_item(0)?, parsed.get_item(1)?);
        if !image.is_truthy()? {
            svc.setattr("last_error", "empty firmware image")?;
            return Ok(false);
        }
        let preparing = PyString::new(py, "Preparing to flash\u{2026}").into_any();
        let started = self.lock(svc, || {
            if svc.getattr("_shutting_down")?.is_truthy()? {
                svc.setattr("last_error", "flash service is shutting down")?;
                return Ok(false);
            }
            if self.busy(svc)? {
                svc.setattr("last_error", "flash already in progress")?;
                return Ok(false);
            }
            let conn = svc.getattr("mavlink")?.call_method0("connection_string")?;
            let bootloader = svc.call_method0("bootloader_device")?;
            svc.getattr("_cancel")?.call_method0("clear")?;
            svc.setattr("_state", "flashing")?;
            svc.setattr("_percent", 0)?;
            svc.setattr("_message", &preparing)?;
            let target = svc.clone().unbind();
            let args = (
                conn.unbind(),
                image.clone().unbind(),
                board_id.clone().unbind(),
                bootloader.unbind(),
            );
            let handle = thread::spawn(py, "firmware-flash", move |py| {
                target.bind(py).call_method1(
                    "_run",
                    (args.0.bind(py), args.1.bind(py), args.2.bind(py), args.3.bind(py)),
                )?;
                Ok(())
            })?;
            svc.setattr("_worker", handle)?;
            Ok(true)
        })?;
        if !started {
            return Ok(false);
        }
        self.notify(svc, "flashing", &preparing)?;
        Ok(true)
    }

    /// Download one catalogue image, then flash it.
    fn start_release(
        &self,
        svc: &Bound<'_, PyAny>,
        release_tag: &Bound<'_, PyAny>,
        board_name: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        let py = svc.py();
        let catalog = svc.getattr("catalog")?;
        if catalog.is_none() {
            svc.setattr("last_error", "firmware catalogue unavailable")?;
            return Ok(false);
        }
        let refusal = svc.call_method0("_gate")?;
        if refusal.is_truthy()? {
            svc.setattr("last_error", refusal)?;
            return Ok(false);
        }
        let entry = catalog.call_method1("resolve", (release_tag, board_name))?;
        if entry.is_none() {
            svc.setattr("last_error", "unknown firmware release or board")?;
            return Ok(false);
        }
        let name = entry.call_method1("get", ("name", ""))?;
        let message = fmt(py, "Downloading {}\u{2026}", (&name,))?;
        let started = self.lock(svc, || {
            if svc.getattr("_shutting_down")?.is_truthy()? {
                svc.setattr("last_error", "flash service is shutting down")?;
                return Ok(false);
            }
            if self.busy(svc)? {
                svc.setattr("last_error", "flash already in progress")?;
                return Ok(false);
            }
            let conn = svc.getattr("mavlink")?.call_method0("connection_string")?;
            let bootloader = svc.call_method0("bootloader_device")?;
            svc.getattr("_cancel")?.call_method0("clear")?;
            svc.setattr("_state", "downloading")?;
            svc.setattr("_percent", 0)?;
            svc.setattr("_message", &message)?;
            let target = svc.clone().unbind();
            let args = (conn.unbind(), entry.clone().unbind(), bootloader.unbind());
            let handle = thread::spawn(py, "firmware-download", move |py| {
                target
                    .bind(py)
                    .call_method1("_run_release", (args.0.bind(py), args.1.bind(py), args.2.bind(py)))?;
                Ok(())
            })?;
            svc.setattr("_worker", handle)?;
            Ok(true)
        })?;
        if !started {
            return Ok(false);
        }
        self.notify(svc, "downloading", &message)?;
        Ok(true)
    }

    /// Download worker: fetch (or reuse the cache), then hand off to `_run`.
    #[pyo3(signature = (svc, conn, entry, bootloader = None))]
    fn run_release(
        &self,
        svc: &Bound<'_, PyAny>,
        conn: &Bound<'_, PyAny>,
        entry: &Bound<'_, PyAny>,
        bootloader: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let py = svc.py();
        let bootloader = match bootloader {
            Some(b) => b.clone(),
            None => PyString::new(py, "").into_any(),
        };
        let name = entry.call_method1("get", ("name", ""))?.str()?;
        let fetched = (|| -> PyResult<Option<(Bound<'_, PyAny>, Bound<'_, PyAny>)>> {
            let catalog = svc.getattr("catalog")?;
            let cached = !catalog.call_method1("cached_path", (&name,))?.is_none();
            if cached {
                self.set(svc, "downloading", 100, fmt(py, "Using cached {}", (&name,))?)?;
            }
            let callback = {
                let target = svc.clone().unbind();
                let label = name.clone().unbind();
                PyCFunction::new_closure(
                    py,
                    None,
                    None,
                    move |args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
                        let py = args.py();
                        let (done, total): (i64, i64) = (args.get_item(0)?.extract()?, args.get_item(1)?.extract()?);
                        let percent = if total > 0 {
                            ((done * 100) as f64 / total as f64).trunc() as i64
                        } else {
                            0
                        };
                        let svc = target.bind(py);
                        let message = fmt(py, "Downloading {}: {} KB", (label.bind(py), done.div_euclid(1024)))?;
                        svc.call_method1("_set", ("downloading", percent.min(99), message))?;
                        Ok(())
                    },
                )?
            };
            let kwargs = kw(
                py,
                &[
                    (
                        "on_progress",
                        if cached {
                            py.None()
                        } else {
                            callback.into_any().unbind()
                        },
                    ),
                    ("cancel", svc.getattr("_cancel")?.unbind()),
                ],
            )?;
            let raw = catalog.call_method("download", (entry,), Some(&kwargs))?;
            if svc.getattr("_cancel")?.call_method0("is_set")?.is_truthy()? {
                self.set(svc, "cancelled", 0, PyString::new(py, "Download cancelled").into_any())?;
                return Ok(None);
            }
            let parsed = self.env(py).call_method1("parse_firmware_image", (raw,))?;
            let (image, board_id) = (parsed.get_item(0)?, parsed.get_item(1)?);
            if !image.is_truthy()? {
                self.set(svc, "failed", 0, PyString::new(py, "empty firmware image").into_any())?;
                return Ok(None);
            }
            Ok(Some((image, board_id)))
        })();
        let (image, board_id) = match fetched {
            Ok(Some(pair)) => pair,
            Ok(None) => return Ok(()),
            Err(err) if caught_by(py, &err, &["ValueError"]) => {
                return self.set(svc, "failed", 0, err.value(py).str()?.into_any());
            }
            Err(err) if is_exception(py, &err) => {
                self.log_error(py, "firmware download worker crashed", &err)?;
                return self.set(
                    svc,
                    "failed",
                    0,
                    PyString::new(py, "firmware download failed unexpectedly").into_any(),
                );
            }
            Err(err) => return Err(err),
        };
        let refusal = svc.call_method1("_gate_for_resume", (&bootloader,))?;
        if refusal.is_truthy()? {
            return self.set(svc, "failed", 0, refusal);
        }
        self.set(
            svc,
            "flashing",
            0,
            PyString::new(py, "Preparing to flash\u{2026}").into_any(),
        )?;
        svc.call_method1("_run", (conn, image, board_id, bootloader))?;
        Ok(())
    }

    /// Upload worker: reboot, stop the bridge, flash, reconnect the bridge.
    #[pyo3(signature = (svc, conn, image, board_id = None, bootloader = None))]
    fn run(
        &self,
        svc: &Bound<'_, PyAny>,
        conn: &Bound<'_, PyAny>,
        image: &Bound<'_, PyAny>,
        board_id: Option<&Bound<'_, PyAny>>,
        bootloader: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let py = svc.py();
        let env = self.env(py);
        let board_id = match board_id {
            Some(b) => b.clone(),
            None => py.None().into_bound(py),
        };
        let bootloader = match bootloader {
            Some(b) => b.clone(),
            None => PyString::new(py, "").into_any(),
        };
        let mut uploader: Option<Bound<'_, PyAny>> = None;
        let body = (|| -> PyResult<()> {
            let mavlink = svc.getattr("mavlink")?;
            if bootloader.is_truthy()? {
                self.set(
                    svc,
                    "flashing",
                    0,
                    PyString::new(py, "Controller found in its bootloader\u{2026}").into_any(),
                )?;
            } else {
                self.set(
                    svc,
                    "flashing",
                    0,
                    PyString::new(py, "Rebooting autopilot into bootloader\u{2026}").into_any(),
                )?;
                if !mavlink.call_method0("reboot_to_bootloader")?.is_truthy()? {
                    return self.set(
                        svc,
                        "failed",
                        0,
                        PyString::new(py, "Reboot to bootloader rejected. Is the vehicle armed?").into_any(),
                    );
                }
            }
            match mavlink.call_method0("stop") {
                Err(err) if is_exception(py, &err) => self.log_debug(py, "mavlink.stop() during flash raised", &err)?,
                Err(err) => return Err(err),
                Ok(_) => {}
            }
            let device = if bootloader.is_truthy()? {
                bootloader.clone()
            } else {
                mavlink.call_method0("serial_device")?
            };
            if !device.is_truthy()? {
                return self.set(
                    svc,
                    "failed",
                    0,
                    PyString::new(py, "no serial device for bootloader").into_any(),
                );
            }
            let kwargs = kw(
                py,
                &[
                    ("on_progress", svc.getattr("_on_uploader_progress")?.unbind()),
                    ("cancel", svc.getattr("_cancel")?.unbind()),
                ],
            )?;
            let up = env.getattr("FirmwareUploader")?.call((), Some(&kwargs))?;
            uploader = Some(up.clone());
            self.lock(svc, || svc.setattr("_uploader", &up))?;
            let kwargs = kw(py, &[("board_id", board_id.clone().unbind())])?;
            let ok = up.call_method("run", (&device, image), Some(&kwargs))?;
            if svc.getattr("_cancel")?.call_method0("is_set")?.is_truthy()? {
                self.set(
                    svc,
                    "cancelled",
                    svc.getattr("_percent")?.unbind(),
                    PyString::new(py, "Flash cancelled").into_any(),
                )?;
            } else if ok.is_truthy()? {
                self.set(
                    svc,
                    "done",
                    100,
                    PyString::new(py, "Firmware flashed successfully").into_any(),
                )?;
            } else {
                let message = svc.getattr("_message")?;
                let message = if message.is_truthy()? {
                    message
                } else {
                    PyString::new(py, "flash failed").into_any()
                };
                self.set(svc, "failed", svc.getattr("_percent")?.unbind(), message)?;
            }
            Ok(())
        })();
        let body = match body {
            Err(err) if is_exception(py, &err) => {
                self.log_error(py, "firmware flash worker crashed", &err)?;
                self.set(
                    svc,
                    "failed",
                    svc.getattr("_percent")?.unbind(),
                    PyString::new(py, "flash failed unexpectedly").into_any(),
                )
            }
            other => other,
        };
        if let Some(up) = &uploader {
            match up.call_method0("shutdown") {
                Err(err) if !is_exception(py, &err) => return Err(err),
                _ => {}
            }
            self.lock(svc, || {
                if svc.getattr("_uploader")?.is(up) {
                    svc.setattr("_uploader", py.None())?;
                }
                Ok(())
            })?;
        }
        svc.call_method1("_reconnect", (conn,))?;
        body
    }

    /// Restart the MAVLink bridge so the GCS reconnects to the flashed FC.
    fn reconnect(&self, svc: &Bound<'_, PyAny>, conn: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        let mavlink = svc.getattr("mavlink")?;
        if svc.getattr("_shutting_down")?.is_truthy()? || !conn.is_truthy()? || mavlink.is_none() {
            return Ok(());
        }
        let restarted = (|| -> PyResult<()> {
            mavlink.call_method1("set_connection", (conn,))?;
            mavlink.call_method0("start")?;
            Ok(())
        })();
        match restarted {
            Err(err) if is_exception(py, &err) => self.log_debug(py, "mavlink reconnect after flash failed", &err),
            other => other,
        }
    }

    /// Cancel a running download or flash; False when nothing is running.
    fn cancel(&self, svc: &Bound<'_, PyAny>) -> PyResult<bool> {
        if !self.lock(svc, || self.busy(svc))? {
            return Ok(false);
        }
        svc.getattr("_cancel")?.call_method0("set")?;
        Ok(true)
    }

    /// Idempotent teardown: cancel the worker, join it, reset the state.
    fn shutdown(&self, svc: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = svc.py();
        let (uploader, worker) = self.lock(svc, || {
            svc.setattr("_shutting_down", true)?;
            svc.getattr("_cancel")?.call_method0("set")?;
            Ok((svc.getattr("_uploader")?, svc.getattr("_worker")?))
        })?;
        if !uploader.is_none() {
            match uploader.call_method0("shutdown") {
                Err(err) if !is_exception(py, &err) => return Err(err),
                _ => {}
            }
        }
        if !worker.is_none() {
            let kwargs = kw(py, &[("timeout", 3.0f64.into_pyobject(py)?.into_any().unbind())])?;
            match worker.call_method("join", (), Some(&kwargs)) {
                Err(err) if !is_exception(py, &err) => return Err(err),
                _ => {}
            }
            svc.setattr("_worker", py.None())?;
        }
        self.lock(svc, || svc.setattr("_state", "idle"))
    }
}

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "corvus_link.flash")?;
    m.add_class::<UploaderEngine>()?;
    m.add_class::<FlashEngine>()?;
    m.add_function(wrap_pyfunction!(bl_crc32, &m)?)?;
    parent.add_submodule(&m)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("corvus_link.flash", &m)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bootloader_crc_is_the_reflected_crc32_without_the_final_xor() {
        assert_eq!(fold(0, b"123456789"), 0x2DFD_2D88);
        assert_eq!(fold(0, b""), 0);
        // The standard CRC-32 is this one started at 0xFFFFFFFF and inverted.
        assert_eq!(!fold(0xFFFF_FFFF, b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn a_crc_can_be_continued_over_pieces_and_over_padding() {
        let whole = fold(0, &[b"abcdef".as_slice(), &[0xFF; 1000]].concat());
        let piecewise = fold(fold(fold(0, b"abc"), b"def"), &[0xFF; 1000]);
        assert_eq!(whole, piecewise);
    }
}
