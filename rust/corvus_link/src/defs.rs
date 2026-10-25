//! The message and enum tables generated from the XML definitions, and the
//! two views pymavlink offers of them.
//!
//! pymavlink generates the `all` dialect twice: a MAVLink 1 module (message
//! ids up to 255, no extension fields) and a MAVLink 2 module. A link starts
//! on the first and moves to the second when it sees its first MAVLink 2
//! frame. Both are one table here, read through [`Wire`].

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    Char,
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
}

impl Ty {
    pub fn size(self) -> usize {
        match self {
            Ty::Char | Ty::U8 | Ty::I8 => 1,
            Ty::U16 | Ty::I16 => 2,
            Ty::U32 | Ty::I32 | Ty::F32 => 4,
            Ty::U64 | Ty::I64 | Ty::F64 => 8,
        }
    }

    /// The `struct` format character pymavlink packs this type with.
    pub fn struct_code(self) -> char {
        match self {
            Ty::Char => 'c',
            Ty::U8 => 'B',
            Ty::I8 => 'b',
            Ty::U16 => 'H',
            Ty::I16 => 'h',
            Ty::U32 => 'I',
            Ty::I32 => 'i',
            Ty::U64 => 'Q',
            Ty::I64 => 'q',
            Ty::F32 => 'f',
            Ty::F64 => 'd',
        }
    }
}

pub struct FieldDef {
    pub name: &'static str,
    pub ty: Ty,
    pub type_name: &'static str,
    pub array_len: usize,
    pub wire_offset: usize,
    pub enum_name: &'static str,
    pub display: &'static str,
    pub units: &'static str,
    pub const_value: Option<i64>,
    #[allow(dead_code)]
    pub description: &'static str,
}

impl FieldDef {
    pub fn wire_len(&self) -> usize {
        if self.array_len != 0 {
            self.array_len * self.ty.size()
        } else {
            self.ty.size()
        }
    }

    /// A char array, which pymavlink handles as one `bytes` value.
    pub fn is_string(&self) -> bool {
        self.ty == Ty::Char && self.array_len != 0
    }

    /// pymavlink's `len_map` entry: 1 for scalars and strings, the element
    /// count for every other array.
    pub fn length(&self) -> usize {
        if self.array_len == 0 || self.ty == Ty::Char {
            1
        } else {
            self.array_len
        }
    }
}

pub struct MsgDef {
    pub id: u32,
    pub name: &'static str,
    pub lower: &'static str,
    pub crc_extra: u8,
    pub base_fields: usize,
    #[allow(dead_code)]
    pub wire_len: usize,
    #[allow(dead_code)]
    pub base_wire_len: usize,
    pub order: &'static [u8],
    pub instance_field: Option<u8>,
    #[allow(dead_code)]
    pub description: &'static str,
    pub fields: &'static [FieldDef],
}

pub struct EnumEntryDef {
    pub name: &'static str,
    pub value: i64,
    pub description: &'static str,
    pub has_location: bool,
    pub params: &'static [(u32, &'static str, &'static str)],
}

pub struct EnumDef {
    pub name: &'static str,
    pub bitmask: bool,
    pub entries: &'static [EnumEntryDef],
}

include!(concat!(env!("OUT_DIR"), "/definitions.rs"));

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Wire {
    V1,
    V2,
}

impl Wire {
    pub fn version_text(self) -> &'static str {
        match self {
            Wire::V1 => "1.0",
            Wire::V2 => "2.0",
        }
    }
}

impl MsgDef {
    /// Declared fields in this view: MAVLink 1 never sees an extension.
    pub fn fields(&self, wire: Wire) -> &'static [FieldDef] {
        match wire {
            Wire::V1 => &self.fields[..self.base_fields],
            Wire::V2 => self.fields,
        }
    }

    /// Field indices in wire order for this view.
    pub fn order(&self, wire: Wire) -> &'static [u8] {
        match wire {
            Wire::V1 => &self.order[..self.base_fields],
            Wire::V2 => self.order,
        }
    }

    /// Payload length with every field present.
    pub fn payload_len(&self, wire: Wire) -> usize {
        self.order(wire)
            .iter()
            .map(|&i| self.fields[i as usize].wire_len())
            .sum()
    }

    pub fn class_name(&self) -> String {
        format!("MAVLink_{}_message", self.lower)
    }

    pub fn in_view(&self, wire: Wire) -> bool {
        wire == Wire::V2 || self.id <= 255
    }

    pub fn field_index(&self, wire: Wire, name: &str) -> Option<usize> {
        self.fields(wire).iter().position(|f| f.name == name)
    }
}

fn by_id() -> &'static HashMap<u32, &'static MsgDef> {
    static MAP: OnceLock<HashMap<u32, &'static MsgDef>> = OnceLock::new();
    MAP.get_or_init(|| MESSAGES.iter().map(|m| (m.id, m)).collect())
}

fn by_lower() -> &'static HashMap<&'static str, &'static MsgDef> {
    static MAP: OnceLock<HashMap<&'static str, &'static MsgDef>> = OnceLock::new();
    MAP.get_or_init(|| MESSAGES.iter().map(|m| (m.lower, m)).collect())
}

pub fn message_by_id(id: u32, wire: Wire) -> Option<&'static MsgDef> {
    by_id().get(&id).copied().filter(|m| m.in_view(wire))
}

pub fn message_by_lower(name: &str, wire: Wire) -> Option<&'static MsgDef> {
    by_lower().get(name).copied().filter(|m| m.in_view(wire))
}

pub fn message_by_name(name: &str, wire: Wire) -> Option<&'static MsgDef> {
    message_by_lower(&name.to_ascii_lowercase(), wire).filter(|m| m.name == name)
}

/// The value of an enum constant by name, as the generated module binds it:
/// a name that appears in more than one enum keeps the last definition.
pub fn constant(name: &str) -> Option<i64> {
    static MAP: OnceLock<HashMap<&'static str, i64>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for e in ENUMS {
            for entry in e.entries {
                map.insert(entry.name, entry.value);
            }
        }
        map
    })
    .get(name)
    .copied()
}

pub fn constant_or(name: &str, fallback: i64) -> i64 {
    constant(name).unwrap_or(fallback)
}
