//! Payload packing and unpacking from the generated field tables.
//!
//! Fields go on the wire in `MsgDef::order` (by size, extension fields last),
//! little endian. A MAVLink 2 payload has its trailing zero bytes cut (at
//! least one byte stays) and a shorter payload than the message needs is read
//! as if zero filled; MAVLink 1 sends every base field.

use super::message::{Message, Value};
use crate::wire::defs::{message_by_id, FieldDef, Ty, Wire};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// A value does not fit the field's type.
    OutOfRange(&'static str),
    /// A value of the wrong kind for the field.
    WrongType(&'static str),
    /// A payload longer than the message can have, or of the wrong length in MAVLink 1.
    Length {
        expected: usize,
        got: usize,
    },
    UnknownMessage(u32),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::OutOfRange(n) => write!(f, "value out of range for {n}"),
            CodecError::WrongType(n) => write!(f, "wrong type of value for {n}"),
            CodecError::Length { expected, got } => {
                write!(f, "invalid MAVLink payload length {got}, expected {expected}")
            }
            CodecError::UnknownMessage(id) => write!(f, "unknown MAVLink message id {id}"),
        }
    }
}

impl std::error::Error for CodecError {}

fn int_range(ty: Ty) -> (i128, i128) {
    match ty {
        Ty::U8 | Ty::Char => (0, u8::MAX as i128),
        Ty::I8 => (i8::MIN as i128, i8::MAX as i128),
        Ty::U16 => (0, u16::MAX as i128),
        Ty::I16 => (i16::MIN as i128, i16::MAX as i128),
        Ty::U32 => (0, u32::MAX as i128),
        Ty::I32 => (i32::MIN as i128, i32::MAX as i128),
        Ty::U64 => (0, u64::MAX as i128),
        Ty::I64 => (i64::MIN as i128, i64::MAX as i128),
        Ty::F32 | Ty::F64 => (0, 0),
    }
}

fn pack_scalar(out: &mut Vec<u8>, field: &FieldDef, v: &Value) -> Result<(), CodecError> {
    let ty = field.ty;
    match ty {
        Ty::F32 | Ty::F64 => {
            let f = v.as_f64().ok_or(CodecError::WrongType(field.name))?;
            if ty == Ty::F32 {
                let narrow = f as f32;
                if narrow.is_infinite() && f.is_finite() {
                    return Err(CodecError::OutOfRange(field.name));
                }
                out.extend_from_slice(&narrow.to_le_bytes());
            } else {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        _ => {
            let Value::Int(i) = v else {
                return Err(CodecError::WrongType(field.name));
            };
            let (lo, hi) = int_range(ty);
            if *i < lo || *i > hi {
                return Err(CodecError::OutOfRange(field.name));
            }
            out.extend_from_slice(&i.to_le_bytes()[..ty.size()]);
        }
    }
    Ok(())
}

fn pack_field(out: &mut Vec<u8>, field: &FieldDef, v: &Value) -> Result<(), CodecError> {
    if field.is_string() {
        let Value::Bytes(b) = v else {
            return Err(CodecError::WrongType(field.name));
        };
        let mut padded = b.clone();
        padded.resize(field.array_len, 0);
        out.extend_from_slice(&padded);
    } else if field.array_len != 0 {
        let Value::Array(items) = v else {
            return Err(CodecError::WrongType(field.name));
        };
        if items.len() > field.array_len {
            return Err(CodecError::OutOfRange(field.name));
        }
        for item in items {
            pack_scalar(out, field, item)?;
        }
        let zero = Value::zero_scalar(field.ty);
        for _ in items.len()..field.array_len {
            pack_scalar(out, field, &zero)?;
        }
    } else {
        pack_scalar(out, field, v)?;
    }
    Ok(())
}

/// The payload of `msg`. With `trim` (MAVLink 2) the trailing zero bytes are cut.
pub fn pack_payload(msg: &Message, trim: bool) -> Result<Vec<u8>, CodecError> {
    let def = msg.def();
    let mut out = Vec::with_capacity(def.payload_len(msg.wire()));
    for &i in def.order(msg.wire()) {
        pack_field(&mut out, &def.fields[i as usize], &msg.values()[i as usize])?;
    }
    if trim {
        let keep = out.iter().rposition(|&b| b != 0).map_or(1, |p| p + 1);
        out.truncate(keep);
    }
    Ok(out)
}

fn read_scalar(ty: Ty, b: &[u8]) -> Value {
    match ty {
        Ty::U8 | Ty::Char => Value::Int(b[0] as i128),
        Ty::I8 => Value::Int(b[0] as i8 as i128),
        Ty::U16 => Value::Int(u16::from_le_bytes([b[0], b[1]]) as i128),
        Ty::I16 => Value::Int(i16::from_le_bytes([b[0], b[1]]) as i128),
        Ty::U32 => Value::Int(u32::from_le_bytes(b[..4].try_into().unwrap_or([0; 4])) as i128),
        Ty::I32 => Value::Int(i32::from_le_bytes(b[..4].try_into().unwrap_or([0; 4])) as i128),
        Ty::U64 => Value::Int(u64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8])) as i128),
        Ty::I64 => Value::Int(i64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8])) as i128),
        Ty::F32 => Value::Float(f32::from_le_bytes(b[..4].try_into().unwrap_or([0; 4])) as f64),
        Ty::F64 => Value::Float(f64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8]))),
    }
}

/// The message with id `id` from `payload`. A MAVLink 2 payload may be
/// shorter than the message (trailing zeros were cut) and is read zero
/// filled; one longer than the message is refused. A MAVLink 1 payload must
/// be exactly as long as the base fields.
pub fn unpack_payload(id: u32, wire: Wire, payload: &[u8]) -> Result<Message, CodecError> {
    let def = message_by_id(id, wire).ok_or(CodecError::UnknownMessage(id))?;
    let full = def.payload_len(wire);
    if payload.len() > full || (wire == Wire::V1 && payload.len() != full) {
        return Err(CodecError::Length {
            expected: full,
            got: payload.len(),
        });
    }
    let mut buf = payload.to_vec();
    buf.resize(full, 0);
    let mut msg = Message::new(def, wire);
    let mut pos = 0;
    for &i in def.order(wire) {
        let field = &def.fields[i as usize];
        let size = field.ty.size();
        let value = if field.is_string() {
            Value::Bytes(buf[pos..pos + field.array_len].to_vec())
        } else if field.array_len != 0 {
            Value::Array(
                (0..field.array_len)
                    .map(|k| read_scalar(field.ty, &buf[pos + k * size..]))
                    .collect(),
            )
        } else {
            read_scalar(field.ty, &buf[pos..])
        };
        pos += field.wire_len();
        msg.set_index(i as usize, value);
    }
    Ok(msg)
}
