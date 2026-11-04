//! The `mavutil.mavlink` namespace: every constant, enum and message class
//! of the `all` dialect, once per wire version.
//!
//! pymavlink keeps one module object current and swaps it for the MAVLink 2
//! module the first time any link sees a MAVLink 2 frame. The same happens
//! here through [`switch_to_v2`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyDict, PyList, PyModule, PyString, PyTuple};
use pyo3::{PyTraverseError, PyVisit};

use crate::lock;
use crate::wire::defs::{self, MsgDef, Wire, ENUMS, MESSAGES};
use crate::wire::frame::{self, Mavlink, Signing};
use crate::wire::message::{self, Message, PyHeader};

static V2_ACTIVE: AtomicBool = AtomicBool::new(false);
static NAMESPACES: GILOnceCell<(Py<PyModule>, Py<PyModule>)> = GILOnceCell::new();
static TYPES: GILOnceCell<(Py<PyDict>, Py<PyDict>)> = GILOnceCell::new();
static SWITCH_HOOK: OnceLock<Mutex<Option<PyObject>>> = OnceLock::new();

pub fn current_wire() -> Wire {
    if V2_ACTIVE.load(Ordering::SeqCst) {
        Wire::V2
    } else {
        Wire::V1
    }
}

pub fn init_wire_from_env() {
    let v2 = std::env::var("MAVLINK20")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    V2_ACTIVE.store(v2, Ordering::SeqCst);
}

/// The MAVLink 2 switch pymavlink's `auto_mavlink_version` makes: process
/// wide, permanent, and visible to child processes through `MAVLINK20`.
pub fn switch_to_v2(py: Python<'_>) -> PyResult<()> {
    if V2_ACTIVE.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    std::env::set_var("MAVLINK20", "1");
    if let Ok(environ) = py.import("os").and_then(|os| os.getattr("environ")) {
        let _ = environ.set_item("MAVLINK20", "1");
    }
    let hook = SWITCH_HOOK.get_or_init(|| Mutex::new(None));
    let hook = lock(hook).as_ref().map(|h| h.clone_ref(py));
    if let Some(hook) = hook {
        hook.call0(py)?;
    }
    Ok(())
}

pub fn set_switch_hook(hook: PyObject) {
    *lock(SWITCH_HOOK.get_or_init(|| Mutex::new(None))) = Some(hook);
}

pub fn namespace(py: Python<'_>, wire: Wire) -> PyResult<Bound<'_, PyModule>> {
    let (v1, v2) = NAMESPACES.get_or_try_init(py, || -> PyResult<_> {
        Ok((
            build_namespace(py, Wire::V1)?.unbind(),
            build_namespace(py, Wire::V2)?.unbind(),
        ))
    })?;
    Ok(match wire {
        Wire::V1 => v1.bind(py).clone(),
        Wire::V2 => v2.bind(py).clone(),
    })
}

fn types(py: Python<'_>) -> &(Py<PyDict>, Py<PyDict>) {
    TYPES.get_or_init(py, || (PyDict::new(py).unbind(), PyDict::new(py).unbind()))
}

/// The class object of a message type, for class attribute lookups.
pub fn message_type<'py>(py: Python<'py>, def: &MsgDef, wire: Wire) -> Option<Bound<'py, PyAny>> {
    let _ = namespace(py, wire).ok()?;
    let (v1, v2) = types(py);
    let map = match wire {
        Wire::V1 => v1.bind(py),
        Wire::V2 => v2.bind(py),
    };
    map.get_item(def.id).ok().flatten()
}

/// A message class (`MAVLink_heartbeat_message` and friends). Calling it
/// builds a message; `isinstance` works against it; the class attributes
/// pymavlink defines are attributes of this object.
#[pyclass(name = "MAVLinkMessageType", module = "corvus_link", frozen, dict)]
pub struct MessageType {
    def: &'static MsgDef,
    wire: Wire,
}

#[pymethods]
impl MessageType {
    #[pyo3(signature = (*args, **kwargs))]
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, Message>> {
        let values = message::bind_arguments(py, self.def, self.wire, args, kwargs, false, "__init__")?;
        message::build(py, self.def, self.wire, &values)
    }

    fn __instancecheck__(&self, obj: &Bound<'_, PyAny>) -> bool {
        match obj.downcast::<Message>() {
            Ok(m) => {
                let meta = m.get().meta();
                matches!(meta.kind, message::Kind::Def(d) if d.id == self.def.id) && meta.wire == self.wire
            }
            Err(_) => false,
        }
    }

    fn __repr__(&self) -> String {
        format!("<class 'corvus_link.{}'>", self.def.class_name())
    }
}

/// A protocol object factory bound to one wire version, published as
/// `mavutil.mavlink.MAVLink`.
#[pyclass(name = "MAVLinkFactory", module = "corvus_link", frozen)]
pub struct MavlinkFactory {
    wire: Wire,
}

#[pymethods]
impl MavlinkFactory {
    #[pyo3(signature = (file, srcSystem=0, srcComponent=0, use_native=false))]
    #[allow(non_snake_case, unused_variables)]
    fn __call__(
        &self,
        py: Python<'_>,
        file: PyObject,
        srcSystem: i64,
        srcComponent: i64,
        use_native: bool,
    ) -> PyResult<Py<Mavlink>> {
        Mavlink::create(py, self.wire, Some(file), srcSystem, srcComponent)
    }

    fn __instancecheck__(&self, obj: &Bound<'_, PyAny>) -> bool {
        obj.downcast::<Mavlink>().is_ok_and(|m| m.get().wire == self.wire)
    }
}

/// `MAVLink_bad_data(data, reason)` and `MAVLink_unknown(msgid, data)`.
#[pyclass(module = "corvus_link", frozen)]
pub struct SpecialFactory {
    unknown: bool,
    wire: Wire,
}

#[pymethods]
impl SpecialFactory {
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        a: &Bound<'py, PyAny>,
        b: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, Message>> {
        if self.unknown {
            let id: u32 = a.extract()?;
            message::unknown(py, id, &frame::bytes_arg(b)?, self.wire)
        } else {
            message::bad_data(py, &frame::bytes_arg(a)?, &b.str()?.to_string(), self.wire)
        }
    }

    fn __instancecheck__(&self, obj: &Bound<'_, PyAny>) -> bool {
        obj.downcast::<Message>().is_ok_and(|m| {
            let kind = m.get().meta().kind;
            if self.unknown {
                matches!(kind, message::Kind::Unknown)
            } else {
                matches!(kind, message::Kind::BadData)
            }
        })
    }
}

/// `EnumEntry`.
#[pyclass(name = "EnumEntry", module = "corvus_link")]
pub struct EnumEntry {
    #[pyo3(get, set)]
    name: String,
    #[pyo3(get, set)]
    description: String,
    #[pyo3(get, set)]
    param: Py<PyDict>,
    #[pyo3(get, set)]
    label: Py<PyDict>,
    #[pyo3(get, set)]
    has_location: bool,
}

#[pymethods]
impl EnumEntry {
    #[new]
    fn new(py: Python<'_>, name: String, description: String) -> Self {
        EnumEntry {
            name,
            description,
            param: PyDict::new(py).unbind(),
            label: PyDict::new(py).unbind(),
            has_location: false,
        }
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.param)?;
        visit.call(&self.label)
    }
}

fn enum_class(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    static CLASS: GILOnceCell<PyObject> = GILOnceCell::new();
    let class = CLASS.get_or_try_init(py, || -> PyResult<PyObject> {
        let builtins = py.import("builtins")?;
        let ns = PyDict::new(py);
        ns.set_item("__module__", "corvus_link")?;
        ns.set_item("bitmask", false)?;
        let bases = PyTuple::new(py, [builtins.getattr("dict")?])?;
        Ok(builtins.getattr("type")?.call1(("Enum", bases, ns))?.unbind())
    })?;
    Ok(class.bind(py).clone())
}

fn enums_dict(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    static ENUMS_DICT: GILOnceCell<Py<PyDict>> = GILOnceCell::new();
    let d = ENUMS_DICT.get_or_try_init(py, || -> PyResult<Py<PyDict>> {
        let class = enum_class(py)?;
        let all = PyDict::new(py);
        for e in ENUMS {
            let obj = class.call0()?;
            obj.setattr("bitmask", e.bitmask)?;
            for entry in e.entries {
                let item = Py::new(
                    py,
                    EnumEntry::new(py, entry.name.to_string(), entry.description.to_string()),
                )?;
                {
                    let item_ref = item.borrow_mut(py);
                    let mut item_ref = item_ref;
                    item_ref.has_location = entry.has_location;
                    for (index, description, label) in entry.params {
                        item_ref.param.bind(py).set_item(*index, *description)?;
                        if !label.is_empty() {
                            item_ref.label.bind(py).set_item(*index, *label)?;
                        }
                    }
                }
                obj.set_item(entry.value, item)?;
            }
            all.set_item(e.name, obj)?;
        }
        Ok(all.unbind())
    })?;
    Ok(d.bind(py).clone())
}

fn str_list<'py>(py: Python<'py>, items: impl IntoIterator<Item = &'py str>) -> PyResult<Bound<'py, PyList>> {
    PyList::new(py, items.into_iter().collect::<Vec<_>>())
}

fn message_type_object<'py>(py: Python<'py>, def: &'static MsgDef, wire: Wire) -> PyResult<Bound<'py, MessageType>> {
    let obj = Bound::new(py, MessageType { def, wire })?;
    let fields = def.fields(wire);
    let order = def.order(wire);
    let ordered: Vec<&str> = order.iter().map(|&i| def.fields[i as usize].name).collect();
    let set = |name: &str, value: Bound<'_, PyAny>| obj.as_any().setattr(name, value);
    set("id", def.id.into_pyobject(py)?.into_any())?;
    set("msgname", PyString::new(py, def.name).into_any())?;
    set("__name__", PyString::new(py, &def.class_name()).into_any())?;
    set("__qualname__", PyString::new(py, &def.class_name()).into_any())?;
    set("fieldnames", str_list(py, fields.iter().map(|f| f.name))?.into_any())?;
    set("ordered_fieldnames", str_list(py, ordered.iter().copied())?.into_any())?;
    set(
        "fieldtypes",
        str_list(py, fields.iter().map(|f| f.type_name))?.into_any(),
    )?;
    let displays = PyDict::new(py);
    let enums = PyDict::new(py);
    let units = PyDict::new(py);
    for f in fields {
        if !f.display.is_empty() {
            displays.set_item(f.name, f.display)?;
        }
        if !f.enum_name.is_empty() {
            enums.set_item(f.name, f.enum_name)?;
        }
        if !f.units.is_empty() {
            units.set_item(f.name, f.units)?;
        }
    }
    set("fielddisplays_by_name", displays.into_any())?;
    set("fieldenums_by_name", enums.into_any())?;
    set("fieldunits_by_name", units.into_any())?;
    // pymavlink's order_map / len_map / array_len_map, including the quirk
    // that array_len_map is indexed by declaration position into the wire
    // ordered list.
    let n = fields.len();
    let orders: Vec<usize> = fields
        .iter()
        .map(|f| ordered.iter().position(|o| *o == f.name).unwrap())
        .collect();
    let mut lengths = vec![0usize; n];
    for (i, f) in fields.iter().enumerate() {
        lengths[orders[i]] = f.length();
    }
    let array_lengths: Vec<usize> = (0..n).map(|i| def.fields[order[i] as usize].array_len).collect();
    set("orders", PyList::new(py, orders)?.into_any())?;
    set("lengths", PyList::new(py, lengths)?.into_any())?;
    set("array_lengths", PyList::new(py, array_lengths)?.into_any())?;
    set("crc_extra", def.crc_extra.into_pyobject(py)?.into_any())?;
    let mut fmt = String::from("<");
    let mut native = String::from("<");
    for &i in order {
        let f = &def.fields[i as usize];
        let code = f.ty.struct_code();
        if f.array_len != 0 {
            fmt.push_str(&f.array_len.to_string());
            fmt.push(if f.ty == defs::Ty::Char { 's' } else { code });
        } else {
            fmt.push(code);
        }
        native.push(if f.const_value.is_some() { 'v' } else { code });
    }
    set("format", PyString::new(py, &fmt).into_any())?;
    set(
        "native_format",
        pyo3::types::PyByteArray::new(py, native.as_bytes()).into_any(),
    )?;
    let instance = def.instance_field.filter(|&i| (i as usize) < fields.len());
    match instance {
        Some(i) => {
            let f = &def.fields[i as usize];
            set("instance_field", PyString::new(py, f.name).into_any())?;
            set("instance_offset", f.wire_offset.into_pyobject(py)?.into_any())?;
        }
        None => {
            set("instance_field", py.None().into_bound(py))?;
            set("instance_offset", (-1i64).into_pyobject(py)?.into_any())?;
        }
    }
    Ok(obj)
}

fn build_namespace(py: Python<'_>, wire: Wire) -> PyResult<Bound<'_, PyModule>> {
    let name = match wire {
        Wire::V1 => "corvus_link.dialects.v10.all",
        Wire::V2 => "corvus_link.dialects.v20.all",
    };
    let m = PyModule::new(py, name)?;
    m.add("WIRE_PROTOCOL_VERSION", wire.version_text())?;
    m.add("DIALECT", "all")?;
    match std::env::var("MAV_IGNORE_CRC") {
        Ok(v) => m.add("MAVLINK_IGNORE_CRC", v)?,
        Err(_) => m.add("MAVLINK_IGNORE_CRC", 0)?,
    }
    m.add("PROTOCOL_MARKER_V1", frame::PROTOCOL_MARKER_V1)?;
    m.add("PROTOCOL_MARKER_V2", frame::PROTOCOL_MARKER_V2)?;
    m.add("HEADER_LEN_V1", frame::HEADER_LEN_V1)?;
    m.add("HEADER_LEN_V2", frame::HEADER_LEN_V2)?;
    m.add("MAVLINK_SIGNATURE_BLOCK_LEN", frame::SIGNATURE_BLOCK_LEN)?;
    m.add("MAVLINK_IFLAG_SIGNED", frame::IFLAG_SIGNED)?;
    for (i, name) in [
        "MAVLINK_TYPE_CHAR",
        "MAVLINK_TYPE_UINT8_T",
        "MAVLINK_TYPE_INT8_T",
        "MAVLINK_TYPE_UINT16_T",
        "MAVLINK_TYPE_INT16_T",
        "MAVLINK_TYPE_UINT32_T",
        "MAVLINK_TYPE_INT32_T",
        "MAVLINK_TYPE_UINT64_T",
        "MAVLINK_TYPE_INT64_T",
        "MAVLINK_TYPE_FLOAT",
        "MAVLINK_TYPE_DOUBLE",
    ]
    .iter()
    .enumerate()
    {
        m.add(*name, i)?;
    }
    m.add("x25crc", py.get_type::<crate::wire::crc::X25Crc>())?;
    m.add("MAVLink_header", py.get_type::<PyHeader>())?;
    m.add("MAVLink_message", py.get_type::<Message>())?;
    m.add("MAVLinkSigning", py.get_type::<Signing>())?;
    m.add("MAVError", py.get_type::<frame::MAVError>())?;
    m.add("MAVLink", Py::new(py, MavlinkFactory { wire })?)?;
    m.add(
        "MAVLink_bad_data",
        Py::new(py, SpecialFactory { unknown: false, wire })?,
    )?;
    m.add("MAVLink_unknown", Py::new(py, SpecialFactory { unknown: true, wire })?)?;
    m.add("EnumEntry", py.get_type::<EnumEntry>())?;
    m.add("Enum", enum_class(py)?)?;
    m.add("enums", enums_dict(py)?)?;

    for e in ENUMS {
        for entry in e.entries {
            m.add(entry.name, entry.value)?;
        }
    }
    m.add("MAVLINK_MSG_ID_BAD_DATA", message::MSG_ID_BAD_DATA)?;
    m.add("MAVLINK_MSG_ID_UNKNOWN", message::MSG_ID_UNKNOWN)?;

    let (v1_types, v2_types) = types(py);
    let registry = match wire {
        Wire::V1 => v1_types.bind(py),
        Wire::V2 => v2_types.bind(py),
    };
    let map = PyDict::new(py);
    for def in MESSAGES.iter().filter(|d| d.in_view(wire)) {
        m.add(format!("MAVLINK_MSG_ID_{}", def.name).as_str(), def.id)?;
        let t = message_type_object(py, def, wire)?;
        m.add(def.class_name().as_str(), &t)?;
        map.set_item(def.id, &t)?;
        registry.set_item(def.id, &t)?;
    }
    if let Some(mission_item) = defs::message_by_name("MISSION_ITEM", wire) {
        m.add("MAVLink_waypoint_message", registry.get_item(mission_item.id)?)?;
    }
    m.add("mavlink_map", map)?;
    Ok(m)
}
