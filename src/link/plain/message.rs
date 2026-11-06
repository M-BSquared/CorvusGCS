//! A message as plain data: the definition it follows and one value per
//! declared field.

use crate::wire::defs::{message_by_id, message_by_name, FieldDef, MsgDef, Ty, Wire};

/// A field value. Every integer type is an `i128`, which holds all of them;
/// a char array is its bytes, padded with zeros to the declared length.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i128),
    Float(f64),
    Bytes(Vec<u8>),
    Array(Vec<Value>),
}

impl Value {
    pub fn zero_scalar(ty: Ty) -> Value {
        match ty {
            Ty::F32 | Ty::F64 => Value::Float(0.0),
            _ => Value::Int(0),
        }
    }

    pub fn zero(field: &FieldDef) -> Value {
        if field.is_string() {
            Value::Bytes(vec![0; field.array_len])
        } else if field.array_len != 0 {
            Value::Array((0..field.array_len).map(|_| Value::zero_scalar(field.ty)).collect())
        } else {
            Value::zero_scalar(field.ty)
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => i64::try_from(*i).ok(),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// A char array as text, up to its first zero byte.
    pub fn as_text(&self) -> Option<String> {
        match self {
            Value::Bytes(b) => {
                let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                Some(String::from_utf8_lossy(&b[..end]).into_owned())
            }
            _ => None,
        }
    }
}

/// A message of the `all` dialect in one of its two views.
#[derive(Clone)]
pub struct Message {
    def: &'static MsgDef,
    wire: Wire,
    values: Vec<Value>,
}

impl PartialEq for Message {
    fn eq(&self, other: &Message) -> bool {
        self.def.id == other.def.id && self.wire == other.wire && self.values == other.values
    }
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct(self.def.name);
        for (field, value) in self.def.fields(self.wire).iter().zip(&self.values) {
            d.field(field.name, value);
        }
        d.finish()
    }
}

impl Message {
    /// Every field zero.
    pub fn new(def: &'static MsgDef, wire: Wire) -> Message {
        Message {
            def,
            wire,
            values: def.fields(wire).iter().map(Value::zero).collect(),
        }
    }

    /// By name (`HEARTBEAT`); `None` for a name not in this view.
    pub fn named(name: &str, wire: Wire) -> Option<Message> {
        message_by_name(name, wire).map(|d| Message::new(d, wire))
    }

    pub fn by_id(id: u32, wire: Wire) -> Option<Message> {
        message_by_id(id, wire).map(|d| Message::new(d, wire))
    }

    pub fn def(&self) -> &'static MsgDef {
        self.def
    }

    pub fn wire(&self) -> Wire {
        self.wire
    }

    pub fn id(&self) -> u32 {
        self.def.id
    }

    pub fn name(&self) -> &'static str {
        self.def.name
    }

    /// The values in declared field order.
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    pub fn get(&self, field: &str) -> Option<&Value> {
        self.def.field_index(self.wire, field).map(|i| &self.values[i])
    }

    /// Sets a field; `Err` names a field the message does not have.
    pub fn set(&mut self, field: &str, value: Value) -> Result<(), String> {
        let i = self
            .def
            .field_index(self.wire, field)
            .ok_or_else(|| format!("{} has no field {field}", self.def.name))?;
        self.values[i] = value;
        Ok(())
    }

    pub fn with(mut self, field: &str, value: Value) -> Message {
        if let Err(e) = self.set(field, value) {
            panic!("{e}");
        }
        self
    }

    pub(crate) fn set_index(&mut self, i: usize, value: Value) {
        self.values[i] = value;
    }
}
