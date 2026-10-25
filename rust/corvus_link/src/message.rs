//! Message objects with pymavlink's interface.
//!
//! A decoded or constructed message keeps its field values as ordinary
//! instance attributes, exactly as pymavlink's generated classes do, so
//! `msg.custom_mode`, `getattr(msg, "x", default)` and assigning a field all
//! behave the same. The frame bookkeeping (header, raw bytes, CRC, signature)
//! lives on the Rust side behind the `get_*` methods.

use std::sync::Mutex;

use pyo3::exceptions::{PyAttributeError, PyIndexError, PyNotImplementedError, PyTypeError};
use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::types::{PyByteArray, PyBytes, PyDict, PyList, PyString, PyTuple};

use crate::codec;
use crate::defs::{MsgDef, Ty, Wire};
use crate::dialect;
use crate::lock;

pub const MSG_ID_BAD_DATA: i64 = -1;
pub const MSG_ID_UNKNOWN: i64 = -2;

#[derive(Clone, Copy)]
pub enum Kind {
    Def(&'static MsgDef),
    BadData,
    Unknown,
}

#[derive(Clone, Default)]
pub struct Header {
    pub msg_id: i64,
    pub incompat_flags: i64,
    pub compat_flags: i64,
    pub mlen: i64,
    pub seq: i64,
    pub src_system: i64,
    pub src_component: i64,
}

impl Header {
    pub fn new(msg_id: i64) -> Self {
        Header {
            msg_id,
            ..Default::default()
        }
    }

    /// The packed header, as `MAVLink_header.pack` writes it.
    pub fn pack(&self, py: Python<'_>, v2: bool) -> PyResult<Vec<u8>> {
        let byte = |v: i64| -> PyResult<u8> {
            u8::try_from(v).map_err(|_| codec::struct_error(py, "ubyte format requires 0 <= number <= 255"))
        };
        if v2 {
            let id = self.msg_id;
            if !(0..=0xFF_FFFF).contains(&id) {
                return Err(codec::struct_error(py, "ushort format requires 0 <= number <= 65535"));
            }
            Ok(vec![
                253,
                byte(self.mlen)?,
                byte(self.incompat_flags)?,
                byte(self.compat_flags)?,
                byte(self.seq)?,
                byte(self.src_system)?,
                byte(self.src_component)?,
                (id & 0xFF) as u8,
                ((id >> 8) & 0xFF) as u8,
                (id >> 16) as u8,
            ])
        } else {
            Ok(vec![
                254,
                byte(self.mlen)?,
                byte(self.seq)?,
                byte(self.src_system)?,
                byte(self.src_component)?,
                byte(self.msg_id)?,
            ])
        }
    }
}

#[derive(Clone)]
pub struct Meta {
    pub kind: Kind,
    pub wire: Wire,
    pub type_name: String,
    pub header: Header,
    pub msgbuf: Vec<u8>,
    pub payload: Option<Vec<u8>>,
    pub crc: Option<u16>,
    pub signed: bool,
    pub link_id: Option<u8>,
}

/// `MAVLink_message`. One Rust class for every message type; the per-type
/// class objects pymavlink exposes are [`crate::dialect::MessageType`].
#[pyclass(name = "MAVLink_message", module = "corvus_link", subclass, dict, frozen)]
pub struct Message {
    pub meta: Mutex<Meta>,
}

/// `MAVLink_header`.
#[pyclass(name = "MAVLink_header", module = "corvus_link")]
#[derive(Clone)]
pub struct PyHeader {
    #[pyo3(get, set, name = "msgId")]
    msg_id: i64,
    #[pyo3(get, set)]
    incompat_flags: i64,
    #[pyo3(get, set)]
    compat_flags: i64,
    #[pyo3(get, set)]
    mlen: i64,
    #[pyo3(get, set)]
    seq: i64,
    #[pyo3(get, set, name = "srcSystem")]
    src_system: i64,
    #[pyo3(get, set, name = "srcComponent")]
    src_component: i64,
}

impl PyHeader {
    fn header(&self) -> Header {
        Header {
            msg_id: self.msg_id,
            incompat_flags: self.incompat_flags,
            compat_flags: self.compat_flags,
            mlen: self.mlen,
            seq: self.seq,
            src_system: self.src_system,
            src_component: self.src_component,
        }
    }
}

#[pymethods]
impl PyHeader {
    #[new]
    #[pyo3(signature = (msgId, incompat_flags=0, compat_flags=0, mlen=0, seq=0, srcSystem=0, srcComponent=0))]
    #[allow(non_snake_case)]
    fn new(
        msgId: i64,
        incompat_flags: i64,
        compat_flags: i64,
        mlen: i64,
        seq: i64,
        srcSystem: i64,
        srcComponent: i64,
    ) -> Self {
        PyHeader {
            msg_id: msgId,
            incompat_flags,
            compat_flags,
            mlen,
            seq,
            src_system: srcSystem,
            src_component: srcComponent,
        }
    }

    #[pyo3(signature = (force_mavlink1=false))]
    fn pack<'py>(&self, py: Python<'py>, force_mavlink1: bool) -> PyResult<Bound<'py, PyBytes>> {
        let v2 = dialect::current_wire() == Wire::V2 && !force_mavlink1;
        Ok(PyBytes::new(py, &self.header().pack(py, v2)?))
    }
}

pub(crate) fn header_object(py: Python<'_>, h: &Header) -> PyResult<Py<PyHeader>> {
    Py::new(
        py,
        PyHeader {
            msg_id: h.msg_id,
            incompat_flags: h.incompat_flags,
            compat_flags: h.compat_flags,
            mlen: h.mlen,
            seq: h.seq,
            src_system: h.src_system,
            src_component: h.src_component,
        },
    )
}

impl Message {
    pub fn new_meta(kind: Kind, wire: Wire) -> Meta {
        let (type_name, msg_id) = match kind {
            Kind::Def(def) => (def.name.to_string(), def.id as i64),
            Kind::BadData => ("BAD_DATA".to_string(), MSG_ID_BAD_DATA),
            Kind::Unknown => (String::new(), MSG_ID_UNKNOWN),
        };
        Meta {
            kind,
            wire,
            type_name,
            header: Header::new(msg_id),
            msgbuf: Vec::new(),
            payload: None,
            crc: None,
            signed: false,
            link_id: None,
        }
    }

    pub fn meta(&self) -> std::sync::MutexGuard<'_, Meta> {
        lock(&self.meta)
    }

    fn fieldnames(&self) -> Vec<&'static str> {
        let meta = self.meta();
        match meta.kind {
            Kind::Def(def) => def.fields(meta.wire).iter().map(|f| f.name).collect(),
            Kind::BadData => vec!["data", "reason"],
            Kind::Unknown => vec!["data"],
        }
    }

    fn type_object<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyAny>> {
        let meta = self.meta();
        match meta.kind {
            Kind::Def(def) => dialect::message_type(py, def, meta.wire),
            _ => None,
        }
    }
}

/// Build a message of `def`'s type from field values in declaration order.
/// Char fields go through pymavlink's constructor expression; everything
/// else is stored as given.
pub fn build<'py>(
    py: Python<'py>,
    def: &'static MsgDef,
    wire: Wire,
    values: &[Bound<'py, PyAny>],
) -> PyResult<Bound<'py, Message>> {
    let obj = Bound::new(
        py,
        Message {
            meta: Mutex::new(Message::new_meta(Kind::Def(def), wire)),
        },
    )?;
    let any = obj.as_any();
    for (field, value) in def.fields(wire).iter().zip(values) {
        if field.ty == Ty::Char {
            any.setattr(format!("_{}_raw", field.name).as_str(), value)?;
            any.setattr(field.name, codec::decode_char_arg(value)?)?;
        } else {
            any.setattr(field.name, value)?;
        }
    }
    Ok(obj)
}

/// The constructor and `<name>_encode`/`<name>_send` argument handling:
/// positional or keyword, extension fields default to zero, and (for the
/// encode and send helpers only) `mavlink_version` defaults to the XML's.
pub fn bind_arguments<'py>(
    py: Python<'py>,
    def: &'static MsgDef,
    wire: Wire,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
    const_defaults: bool,
    what: &str,
) -> PyResult<Vec<Bound<'py, PyAny>>> {
    let fields = def.fields(wire);
    if args.len() > fields.len() {
        return Err(PyTypeError::new_err(format!(
            "{what}() takes {} positional arguments but {} were given",
            fields.len() + 1,
            args.len() + 1
        )));
    }
    let mut values: Vec<Option<Bound<'py, PyAny>>> = vec![None; fields.len()];
    for (i, arg) in args.iter().enumerate() {
        values[i] = Some(arg);
    }
    if let Some(kwargs) = kwargs {
        for (key, value) in kwargs.iter() {
            let name: String = key.extract()?;
            match fields.iter().position(|f| f.name == name) {
                Some(i) if values[i].is_some() => {
                    return Err(PyTypeError::new_err(format!(
                        "{what}() got multiple values for argument '{name}'"
                    )));
                }
                Some(i) => values[i] = Some(value),
                None => {
                    return Err(PyTypeError::new_err(format!(
                        "{what}() got an unexpected keyword argument '{name}'"
                    )));
                }
            }
        }
    }
    let mut out = Vec::with_capacity(fields.len());
    let mut missing = Vec::new();
    for (i, (field, value)) in fields.iter().zip(values).enumerate() {
        match value {
            Some(v) => out.push(v),
            None if const_defaults && field.const_value.is_some() => {
                out.push(field.const_value.unwrap().into_pyobject(py)?.into_any());
            }
            None if i >= def.base_fields => out.push(default_value(py, field)?),
            None => missing.push(field.name),
        }
    }
    if !missing.is_empty() {
        let names: Vec<String> = missing.iter().map(|n| format!("'{n}'")).collect();
        return Err(PyTypeError::new_err(format!(
            "{what}() missing {} required positional argument{}: {}",
            missing.len(),
            if missing.len() == 1 { "" } else { "s" },
            names.join(" and ")
        )));
    }
    Ok(out)
}

fn default_value<'py>(py: Python<'py>, field: &crate::defs::FieldDef) -> PyResult<Bound<'py, PyAny>> {
    Ok(if field.ty == Ty::Char {
        PyBytes::new(py, b"").into_any()
    } else if field.array_len == 0 {
        0i64.into_pyobject(py)?.into_any()
    } else {
        let zeros: Vec<i64> = vec![0; field.array_len];
        PyTuple::new(py, zeros)?.into_any()
    })
}

pub fn bad_data<'py>(py: Python<'py>, data: &[u8], reason: &str, wire: Wire) -> PyResult<Bound<'py, Message>> {
    let mut meta = Message::new_meta(Kind::BadData, wire);
    meta.msgbuf = data.to_vec();
    let obj = Bound::new(py, Message { meta: Mutex::new(meta) })?;
    obj.as_any().setattr("data", PyByteArray::new(py, data))?;
    obj.as_any().setattr("reason", reason)?;
    Ok(obj)
}

pub fn unknown<'py>(py: Python<'py>, msg_id: u32, data: &[u8], wire: Wire) -> PyResult<Bound<'py, Message>> {
    let mut meta = Message::new_meta(Kind::Unknown, wire);
    meta.type_name = format!("UNKNOWN_{msg_id}");
    meta.msgbuf = data.to_vec();
    let obj = Bound::new(py, Message { meta: Mutex::new(meta) })?;
    obj.as_any().setattr("data", PyByteArray::new(py, data))?;
    Ok(obj)
}

fn format_attr_value(value: Bound<'_, PyAny>) -> PyResult<Bound<'_, PyAny>> {
    if let Ok(b) = value.downcast::<PyBytes>() {
        let kwargs = PyDict::new(value.py());
        kwargs.set_item("errors", "backslashreplace")?;
        let text = b.call_method("decode", (), Some(&kwargs))?;
        return text.call_method1("rstrip", ("\x00",));
    }
    Ok(value)
}

#[pymethods]
impl Message {
    fn get_type(&self) -> String {
        self.meta().type_name.clone()
    }

    fn get_msgbuf<'py>(&self, py: Python<'py>) -> Bound<'py, PyByteArray> {
        PyByteArray::new(py, &self.meta().msgbuf)
    }

    fn get_header(&self, py: Python<'_>) -> PyResult<Py<PyHeader>> {
        let header = self.meta().header.clone();
        header_object(py, &header)
    }

    fn get_payload<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyByteArray>> {
        self.meta().payload.as_ref().map(|p| PyByteArray::new(py, p))
    }

    fn get_crc(&self) -> Option<u16> {
        self.meta().crc
    }

    fn get_fieldnames(&self) -> Vec<&'static str> {
        self.fieldnames()
    }

    #[allow(non_snake_case)]
    fn get_msgId(&self) -> i64 {
        self.meta().header.msg_id
    }

    #[allow(non_snake_case)]
    fn get_srcSystem(&self) -> i64 {
        self.meta().header.src_system
    }

    #[allow(non_snake_case)]
    fn get_srcComponent(&self) -> i64 {
        self.meta().header.src_component
    }

    fn get_seq(&self) -> i64 {
        self.meta().header.seq
    }

    fn get_signed(&self) -> bool {
        self.meta().signed
    }

    fn get_link_id(&self) -> Option<u8> {
        self.meta().link_id
    }

    fn format_attr<'py>(slf: &Bound<'py, Self>, field: &str) -> PyResult<Bound<'py, PyAny>> {
        format_attr_value(slf.as_any().getattr(field)?)
    }

    fn to_dict<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        let py = slf.py();
        let d = PyDict::new(py);
        d.set_item("mavpackettype", slf.get().get_type())?;
        for name in slf.get().fieldnames() {
            d.set_item(name, Self::format_attr(slf, name)?)?;
        }
        Ok(d)
    }

    fn to_json(slf: &Bound<'_, Self>) -> PyResult<String> {
        let json = slf.py().import("json")?;
        json.call_method1("dumps", (Self::to_dict(slf)?,))?.extract()
    }

    fn __str__(slf: &Bound<'_, Self>) -> PyResult<String> {
        let this = slf.get();
        let (kind, type_name) = {
            let meta = this.meta();
            (meta.kind, meta.type_name.clone())
        };
        match kind {
            Kind::BadData | Kind::Unknown => {
                let data: Vec<u8> = slf.as_any().getattr("data")?.extract()?;
                let hex: Vec<String> = data.iter().map(|b| format!("'{b:x}'")).collect();
                let hex = format!("[{}]", hex.join(", "));
                if matches!(kind, Kind::BadData) {
                    let reason = slf.as_any().getattr("reason")?.str()?.to_string();
                    Ok(format!("{type_name} {{{reason}, data:{hex}}}"))
                } else {
                    Ok(format!("{type_name} {{data:{hex}}}"))
                }
            }
            Kind::Def(_) => {
                let mut ret = format!("{type_name} {{");
                for name in this.fieldnames() {
                    let v = Self::format_attr(slf, name)?;
                    ret.push_str(&format!("{name} : {}, ", v.str()?));
                }
                ret.truncate(ret.len().saturating_sub(2));
                ret.push('}');
                Ok(ret)
            }
        }
    }

    fn __richcmp__(slf: &Bound<'_, Self>, other: &Bound<'_, PyAny>, op: CompareOp) -> PyResult<PyObject> {
        let py = slf.py();
        let equal = match op {
            CompareOp::Eq | CompareOp::Ne => Self::equals(slf, other)?,
            _ => return Ok(py.NotImplemented()),
        };
        let result = if matches!(op, CompareOp::Eq) { equal } else { !equal };
        Ok(result.into_pyobject(py)?.to_owned().into_any().unbind())
    }

    #[pyo3(signature = (mav, force_mavlink1=false))]
    fn pack<'py>(
        slf: &Bound<'py, Self>,
        mav: &Bound<'py, PyAny>,
        force_mavlink1: bool,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let py = slf.py();
        let (kind, wire) = {
            let meta = slf.get().meta();
            (meta.kind, meta.wire)
        };
        let Kind::Def(def) = kind else {
            return Err(PyNotImplementedError::new_err(
                "MAVLink_message cannot be serialized directly",
            ));
        };
        let payload = codec::pack_payload(slf.as_any(), def, wire)?;
        let frame = crate::mavlink::frame_message(py, slf.get(), mav, def, wire, payload, force_mavlink1)?;
        Ok(PyBytes::new(py, &frame))
    }

    fn __getitem__<'py>(slf: &Bound<'py, Self>, key: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let instances = slf.as_any().getattr("_instances")?;
        if instances.is_none() {
            return Err(PyIndexError::new_err(()));
        }
        match instances.get_item(key) {
            Ok(v) => Ok(v),
            Err(_) => Err(PyIndexError::new_err(())),
        }
    }

    fn __copy__<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, Message>> {
        let py = slf.py();
        let meta = slf.get().meta().clone();
        let copy = Bound::new(py, Message { meta: Mutex::new(meta) })?;
        let source = slf.as_any().getattr("__dict__")?;
        let target = copy.as_any().getattr("__dict__")?;
        target.call_method1("update", (source,))?;
        Ok(copy)
    }

    /// Class-level attributes and pymavlink's private ones, for code that
    /// reads them through an instance.
    fn __getattr__<'py>(slf: &Bound<'py, Self>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.get();
        let meta = this.meta().clone();
        match name {
            "_type" => return Ok(PyString::new(py, &meta.type_name).into_any()),
            "_fieldnames" => return Ok(PyList::new(py, this.fieldnames())?.into_any()),
            "_signed" => return Ok(meta.signed.into_pyobject(py)?.to_owned().into_any()),
            "_link_id" => return Ok(meta.link_id.into_pyobject(py)?.into_any()),
            "_crc" => return Ok(meta.crc.into_pyobject(py)?.into_any()),
            "_msgbuf" => return Ok(PyByteArray::new(py, &meta.msgbuf).into_any()),
            "_payload" => {
                return Ok(meta
                    .payload
                    .as_ref()
                    .map(|p| PyByteArray::new(py, p).into_any())
                    .unwrap_or_else(|| py.None().into_bound(py)))
            }
            "_header" => return Ok(header_object(py, &meta.header)?.into_bound(py).into_any()),
            "_instances" => return Ok(py.None().into_bound(py)),
            "_instance_field" | "_instance_offset" => {
                let attr = &name[1..];
                return match this.type_object(py) {
                    Some(t) => t.getattr(attr),
                    None if attr == "instance_field" => Ok(py.None().into_bound(py)),
                    None => Ok((-1i64).into_pyobject(py)?.into_any()),
                };
            }
            _ => {}
        }
        if !name.starts_with('_') {
            if let Some(t) = this.type_object(py) {
                if let Ok(value) = t.getattr(name) {
                    return Ok(value);
                }
            } else if name == "id" {
                return Ok(meta.header.msg_id.into_pyobject(py)?.into_any());
            }
        }
        let class = match meta.kind {
            Kind::Def(def) => def.class_name(),
            Kind::BadData => "MAVLink_bad_data".to_string(),
            Kind::Unknown => "MAVLink_unknown".to_string(),
        };
        Err(PyAttributeError::new_err(format!(
            "'{class}' object has no attribute '{name}'"
        )))
    }

    fn __repr__(&self) -> String {
        let meta = self.meta();
        let class = match meta.kind {
            Kind::Def(def) => def.class_name(),
            Kind::BadData => "MAVLink_bad_data".to_string(),
            Kind::Unknown => "MAVLink_unknown".to_string(),
        };
        format!("<corvus_link.{class} object>")
    }
}

impl Message {
    fn equals(slf: &Bound<'_, Self>, other: &Bound<'_, PyAny>) -> PyResult<bool> {
        if other.is_none() {
            return Ok(false);
        }
        let Ok(other) = other.downcast::<Message>() else {
            return Ok(false);
        };
        let (a, b) = (slf.get().meta().clone(), other.get().meta().clone());
        if a.type_name != b.type_name || a.crc != b.crc || a.header.seq != b.header.seq {
            return Ok(false);
        }
        if a.header.src_system != b.header.src_system || a.header.src_component != b.header.src_component {
            return Ok(false);
        }
        for name in slf.get().fieldnames() {
            let x = Self::format_attr(slf, name)?;
            let y = Self::format_attr(other, name)?;
            if !x.eq(y)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
