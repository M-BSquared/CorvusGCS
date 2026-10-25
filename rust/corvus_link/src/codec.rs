//! Payload packing and unpacking with pymavlink's `struct` semantics: the
//! same accepted Python types, the same range checks and the same values
//! back out.

use pyo3::exceptions::PyOverflowError;
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyFloat, PyList, PyString};

use crate::defs::{FieldDef, MsgDef, Ty, Wire};

pub fn struct_error(py: Python<'_>, message: impl Into<String>) -> PyErr {
    match py.import("struct").and_then(|m| m.getattr("error")) {
        Ok(kind) => PyErr::from_type(kind.downcast_into().expect("struct.error is a type"), message.into()),
        Err(err) => err,
    }
}

fn int_range(ty: Ty) -> (i128, i128, &'static str) {
    match ty {
        Ty::U8 => (0, u8::MAX as i128, "ubyte format requires 0 <= number <= 255"),
        Ty::I8 => (
            i8::MIN as i128,
            i8::MAX as i128,
            "byte format requires -128 <= number <= 127",
        ),
        Ty::U16 => (0, u16::MAX as i128, "ushort format requires 0 <= number <= 65535"),
        Ty::I16 => (
            i16::MIN as i128,
            i16::MAX as i128,
            "short format requires -32768 <= number <= 32767",
        ),
        Ty::U32 => (0, u32::MAX as i128, "'I' format requires 0 <= number <= 4294967295"),
        Ty::I32 => (
            i32::MIN as i128,
            i32::MAX as i128,
            "'i' format requires -2147483648 <= number <= 2147483647",
        ),
        Ty::U64 => (0, u64::MAX as i128, "argument out of range"),
        Ty::I64 => (i64::MIN as i128, i64::MAX as i128, "argument out of range"),
        _ => unreachable!(),
    }
}

fn index_of(value: &Bound<'_, PyAny>) -> PyResult<Option<i128>> {
    let py = value.py();
    if value.is_instance_of::<PyFloat>() || value.is_instance_of::<PyString>() {
        return Ok(None);
    }
    let index = if value.hasattr("__index__")? {
        value.call_method0("__index__")?
    } else {
        return Ok(None);
    };
    match index.extract::<i128>() {
        Ok(v) => Ok(Some(v)),
        Err(_) => {
            // Wider than 128 bits: out of range for every MAVLink type.
            let negative = index.lt(0i64.into_pyobject(py)?)?;
            Ok(Some(if negative { i128::MIN } else { i128::MAX }))
        }
    }
}

/// Pack one scalar the way `struct.pack` does for this type.
pub fn pack_scalar(out: &mut Vec<u8>, ty: Ty, value: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = value.py();
    match ty {
        Ty::F32 | Ty::F64 => {
            let v: f64 = match value.extract::<f64>() {
                Ok(v) => v,
                Err(_) => return Err(struct_error(py, "required argument is not a float")),
            };
            if ty == Ty::F32 {
                let f = v as f32;
                if f.is_infinite() && v.is_finite() {
                    return Err(PyOverflowError::new_err("float too large to pack with f format"));
                }
                out.extend_from_slice(&f.to_le_bytes());
            } else {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        Ty::Char => {
            let ok = value
                .downcast::<PyBytes>()
                .map(|b| b.as_bytes().to_vec())
                .ok()
                .or_else(|| value.downcast::<PyByteArray>().ok().map(|b| b.to_vec()));
            match ok {
                Some(bytes) if bytes.len() == 1 => out.push(bytes[0]),
                _ => return Err(struct_error(py, "char format requires a bytes object of length 1")),
            }
        }
        _ => {
            let Some(v) = index_of(value)? else {
                return Err(struct_error(py, "required argument is not an integer"));
            };
            let (lo, hi, message) = int_range(ty);
            if v < lo || v > hi {
                return Err(struct_error(py, message));
            }
            match ty {
                Ty::U8 => out.push(v as u8),
                Ty::I8 => out.push(v as i8 as u8),
                Ty::U16 => out.extend_from_slice(&(v as u16).to_le_bytes()),
                Ty::I16 => out.extend_from_slice(&(v as i16).to_le_bytes()),
                Ty::U32 => out.extend_from_slice(&(v as u32).to_le_bytes()),
                Ty::I32 => out.extend_from_slice(&(v as i32).to_le_bytes()),
                Ty::U64 => out.extend_from_slice(&(v as u64).to_le_bytes()),
                Ty::I64 => out.extend_from_slice(&(v as i64).to_le_bytes()),
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}

fn pack_string(out: &mut Vec<u8>, field: &FieldDef, value: &Bound<'_, PyAny>) -> PyResult<()> {
    let bytes = if let Ok(b) = value.downcast::<PyBytes>() {
        b.as_bytes().to_vec()
    } else if let Ok(b) = value.downcast::<PyByteArray>() {
        b.to_vec()
    } else {
        return Err(struct_error(value.py(), "argument for 's' must be a bytes object"));
    };
    let n = field.array_len;
    let take = bytes.len().min(n);
    out.extend_from_slice(&bytes[..take]);
    out.resize(out.len() + (n - take), 0);
    Ok(())
}

/// The full (untrimmed) payload of a message object, read from its
/// attributes exactly as the generated `pack` reads them: char fields from
/// `_<name>_raw`, arrays element by element.
pub fn pack_payload(obj: &Bound<'_, PyAny>, def: &MsgDef, wire: Wire) -> PyResult<Vec<u8>> {
    let mut out = Vec::with_capacity(def.payload_len(wire));
    for &i in def.order(wire) {
        let field = &def.fields[i as usize];
        if field.ty == Ty::Char {
            let raw = obj.getattr(format!("_{}_raw", field.name).as_str())?;
            if field.array_len == 0 {
                pack_scalar(&mut out, Ty::Char, &raw)?;
            } else {
                pack_string(&mut out, field, &raw)?;
            }
        } else if field.array_len == 0 {
            pack_scalar(&mut out, field.ty, &obj.getattr(field.name)?)?;
        } else {
            let seq = obj.getattr(field.name)?;
            for k in 0..field.array_len {
                pack_scalar(&mut out, field.ty, &seq.get_item(k)?)?;
            }
        }
    }
    Ok(out)
}

fn read_scalar<'py>(py: Python<'py>, ty: Ty, b: &[u8]) -> PyResult<Bound<'py, PyAny>> {
    Ok(match ty {
        Ty::U8 => b[0].into_pyobject(py)?.into_any(),
        Ty::I8 => (b[0] as i8).into_pyobject(py)?.into_any(),
        Ty::U16 => u16::from_le_bytes([b[0], b[1]]).into_pyobject(py)?.into_any(),
        Ty::I16 => i16::from_le_bytes([b[0], b[1]]).into_pyobject(py)?.into_any(),
        Ty::U32 => u32::from_le_bytes(b[..4].try_into().unwrap())
            .into_pyobject(py)?
            .into_any(),
        Ty::I32 => i32::from_le_bytes(b[..4].try_into().unwrap())
            .into_pyobject(py)?
            .into_any(),
        Ty::U64 => u64::from_le_bytes(b[..8].try_into().unwrap())
            .into_pyobject(py)?
            .into_any(),
        Ty::I64 => i64::from_le_bytes(b[..8].try_into().unwrap())
            .into_pyobject(py)?
            .into_any(),
        Ty::F32 => (f32::from_le_bytes(b[..4].try_into().unwrap()) as f64)
            .into_pyobject(py)?
            .into_any(),
        Ty::F64 => f64::from_le_bytes(b[..8].try_into().unwrap())
            .into_pyobject(py)?
            .into_any(),
        Ty::Char => PyBytes::new(py, &b[..1]).into_any(),
    })
}

fn rstrip_nul(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == 0 {
        end -= 1;
    }
    &bytes[..end]
}

/// Values for every field of the view, in declaration order, from a payload
/// already padded to the view's full length.
pub fn unpack_payload<'py>(
    py: Python<'py>,
    def: &MsgDef,
    wire: Wire,
    payload: &[u8],
) -> PyResult<Vec<Bound<'py, PyAny>>> {
    let fields = def.fields(wire);
    let mut values = Vec::with_capacity(fields.len());
    for field in fields {
        let at = field.wire_offset;
        let raw = &payload[at..at + field.wire_len()];
        let value = if field.ty == Ty::Char {
            PyBytes::new(py, rstrip_nul(raw)).into_any()
        } else if field.length() == 1 {
            read_scalar(py, field.ty, raw)?
        } else {
            let size = field.ty.size();
            let items = (0..field.array_len)
                .map(|k| read_scalar(py, field.ty, &raw[k * size..(k + 1) * size]))
                .collect::<PyResult<Vec<_>>>()?;
            PyList::new(py, items)?.into_any()
        };
        values.push(value);
    }
    Ok(values)
}

/// `value.split(b"\x00", 1)[0].decode("ascii", errors="replace")`, done
/// natively for the bytes the decoder produced.
pub fn ascii_replace(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end]
        .iter()
        .map(|&b| if b < 0x80 { b as char } else { '\u{FFFD}' })
        .collect()
}

/// The same expression for an arbitrary constructor argument, evaluated by
/// Python so a wrong type fails exactly as it does in pymavlink.
pub fn decode_char_arg<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = value.py();
    if let Ok(b) = value.downcast::<PyBytes>() {
        return Ok(PyString::new(py, &ascii_replace(b.as_bytes())).into_any());
    }
    let nul = PyBytes::new(py, b"\x00");
    let head = value.call_method1("split", (nul, 1))?.get_item(0)?;
    let kwargs = pyo3::types::PyDict::new(py);
    kwargs.set_item("errors", "replace")?;
    head.call_method("decode", ("ascii",), Some(&kwargs))
}
