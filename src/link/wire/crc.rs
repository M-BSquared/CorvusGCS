//! CRC-16/MCRF4XX, the MAVLink frame checksum (plain in `crate::plain::crc`),
//! and pymavlink's `x25crc` object over it.

pub use crate::plain::crc::{accumulate, x25};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyString};

/// Bytes from anything pymavlink's CRC accepts: bytes-like, a str (encoded as
/// UTF-8) or an iterable of ints.
pub fn bytes_of(value: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(s) = value.downcast::<PyString>() {
        return Ok(s.to_str()?.as_bytes().to_vec());
    }
    if let Ok(b) = value.downcast::<PyBytes>() {
        return Ok(b.as_bytes().to_vec());
    }
    if let Ok(v) = value.extract::<Vec<u8>>() {
        return Ok(v);
    }
    let mut out = Vec::new();
    for item in value.try_iter()? {
        out.push(item?.extract::<u8>()?);
    }
    Ok(out)
}

/// pymavlink's `x25crc`: running CRC with `.crc`, `accumulate` and
/// `accumulate_str`.
#[pyclass(name = "x25crc", module = "corvus_link")]
pub struct X25Crc {
    #[pyo3(get, set)]
    crc: u32,
}

#[pymethods]
impl X25Crc {
    #[new]
    #[pyo3(signature = (buf=None))]
    fn new(buf: Option<&Bound<'_, PyAny>>) -> PyResult<Self> {
        let mut this = X25Crc { crc: 0xFFFF };
        if let Some(buf) = buf {
            this.accumulate(buf)?;
        }
        Ok(this)
    }

    fn accumulate(&mut self, buf: &Bound<'_, PyAny>) -> PyResult<()> {
        self.crc = accumulate(self.crc as u16, &bytes_of(buf)?) as u32;
        Ok(())
    }

    fn accumulate_str(&mut self, buf: &Bound<'_, PyAny>) -> PyResult<()> {
        self.accumulate(buf)
    }
}
