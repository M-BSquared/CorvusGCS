//! DataFlash logs: pymavlink's `DFReader_binary` and `DFReader_text`, an
//! ArduPilot `.bin` log (or a PX4 `.px4log`) and a text `.log` read as a
//! stream of messages.
//!
//! A log defines its own message formats (`FMT`), units (`UNIT`, `MULT`,
//! `FMTU`) and instance fields; every message becomes a `DFMessage` whose
//! fields are read by name and scaled as the format says. Four clocks turn a
//! log's own timestamps into wall-clock time: microsecond, millisecond and
//! PX4 style stamps, and one that interpolates between GPS messages.
//!
//! Numbers keep Python's types (a timestamp that was never set is the int
//! 0), text is decoded as pymavlink decodes it, and the odd arithmetic of
//! its struct handling (the native-alignment size of an instance field's
//! offset) is kept, by asking Python's `struct` where it matters. Not built:
//! the metadata download (`DFMetaData`), `dfindexer`'s fast index (the
//! reader indexes the way pymavlink's pure Python indexer does), and the
//! expression evaluator behind `recv_match(condition=...)`.

use std::collections::HashMap;

use indexmap::IndexMap;
use pyo3::exceptions::{PyAttributeError, PyException, PyIndexError, PyNotImplementedError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use pyo3::{PyTraverseError, PyVisit};

const MAV_TYPE_FIXED_WING: i64 = 1;
const MAV_TYPE_QUADROTOR: i64 = 2;
const MAV_TYPE_HELICOPTER: i64 = 4;
const MAV_TYPE_ANTENNA_TRACKER: i64 = 5;
const MAV_TYPE_AIRSHIP: i64 = 7;
const MAV_TYPE_GROUND_ROVER: i64 = 10;
const MAV_TYPE_SUBMARINE: i64 = 12;

// ------------------------------------------------------------------ helpers ----

fn print_to(py: Python<'_>, text: String, stderr: bool) -> PyResult<()> {
    let builtins = py.import("builtins")?;
    if stderr {
        let kwargs = PyDict::new(py);
        kwargs.set_item("file", py.import("sys")?.getattr("stderr")?)?;
        builtins.call_method("print", (text,), Some(&kwargs))?;
    } else {
        builtins.call_method1("print", (text,))?;
    }
    Ok(())
}

/// `values[i]`, with Python's error for a position past the end.
fn at<T>(values: &[T], i: usize) -> PyResult<&T> {
    values
        .get(i)
        .ok_or_else(|| PyIndexError::new_err("list index out of range"))
}

fn int_obj(py: Python<'_>, v: i64) -> PyObject {
    v.into_pyobject(py).expect("ints convert").into_any().unbind()
}

/// `string` up to its first NUL.
fn null_term(s: &str) -> String {
    match s.find('\0') {
        Some(i) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// pymavlink's `to_string`: text as it is, bytes decoded with backslashreplace.
fn to_string(obj: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(s) = obj.downcast::<PyString>() {
        return Ok(s.to_string_lossy().into_owned());
    }
    let kwargs = PyDict::new(obj.py());
    kwargs.set_item("errors", "backslashreplace")?;
    obj.call_method("decode", (), Some(&kwargs))?.extract()
}

/// pymavlink's `null_term` for a str or bytes.
fn null_term_obj(obj: &Bound<'_, PyAny>) -> PyResult<String> {
    if obj.is_instance_of::<PyBytes>() || obj.is_instance_of::<PyString>() {
        return Ok(null_term(&to_string(obj)?));
    }
    // Anything else fails (or not) in `string.find("\0")`, as it does in pymavlink.
    obj.call_method1("find", ("\0",))?;
    Ok(null_term(&obj.str()?.to_string_lossy()))
}

fn is_py_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Python slice `data[start:end]`.
fn py_slice(data: &[u8], start: i64, end: i64) -> &[u8] {
    let len = data.len() as i64;
    let clamp = |i: i64| if i < 0 { (i + len).max(0) } else { i.min(len) };
    let (s, e) = (clamp(start), clamp(end));
    if s >= e {
        &[]
    } else {
        &data[s as usize..e as usize]
    }
}

/// `data.find(needle, start)`.
fn py_find(data: &[u8], needle: &[u8], start: i64) -> i64 {
    let len = data.len() as i64;
    let s = if start < 0 { (start + len).max(0) } else { start };
    if s > len {
        return -1;
    }
    let hay = &data[s as usize..];
    if needle.is_empty() {
        return s;
    }
    hay.windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i as i64 + s)
        .unwrap_or(-1)
}

/// A number that is the int 0 until something sets it to a float, as the
/// clocks' start values are in pymavlink.
#[derive(Clone, Copy, PartialEq)]
enum Num {
    Zero,
    F(f64),
}

impl Num {
    fn f(self) -> f64 {
        match self {
            Num::Zero => 0.0,
            Num::F(v) => v,
        }
    }

    fn add_f(self, other: f64) -> Num {
        Num::F(self.f() + other)
    }

    fn add(self, other: Num) -> Num {
        match (self, other) {
            (Num::Zero, Num::Zero) => Num::Zero,
            _ => Num::F(self.f() + other.f()),
        }
    }

    fn to_py(self, py: Python<'_>) -> PyObject {
        match self {
            Num::Zero => int_obj(py, 0),
            Num::F(v) => PyFloat::new(py, v).into_any().unbind(),
        }
    }
}

// ------------------------------------------------------------------ formats ----

#[derive(Clone, Copy, PartialEq)]
enum Ty {
    Array,
    Str,
    Int,
    Float,
    Long,
}

struct CharInfo {
    code: &'static str,
    mult: Option<f64>,
    ty: Ty,
}

fn char_info(c: char) -> Option<CharInfo> {
    let (code, mult, ty) = match c {
        'a' => ("64s", None, Ty::Array),
        'b' => ("b", None, Ty::Int),
        'B' => ("B", None, Ty::Int),
        'g' => ("e", None, Ty::Float),
        'h' => ("h", None, Ty::Int),
        'H' => ("H", None, Ty::Int),
        'i' => ("i", None, Ty::Int),
        'I' => ("I", None, Ty::Int),
        'f' => ("f", None, Ty::Float),
        'n' => ("4s", None, Ty::Str),
        'N' => ("16s", None, Ty::Str),
        'Z' => ("64s", None, Ty::Str),
        'c' => ("h", Some(0.01), Ty::Float),
        'C' => ("H", Some(0.01), Ty::Float),
        'e' => ("i", Some(0.01), Ty::Float),
        'E' => ("I", Some(0.01), Ty::Float),
        'L' => ("i", Some(1.0e-7), Ty::Float),
        'd' => ("d", None, Ty::Float),
        'M' => ("b", None, Ty::Int),
        'q' => ("q", None, Ty::Long),
        'Q' => ("Q", None, Ty::Long),
        _ => return None,
    };
    Some(CharInfo { code, mult, ty })
}

fn prefix_for(mult: f64) -> Option<&'static str> {
    if mult == 0.0 || mult == 1.0 {
        Some("")
    } else if mult == 1.0e-1 {
        Some("d")
    } else if mult == 1.0e-2 {
        Some("c")
    } else if mult == 1.0e-3 {
        Some("m")
    } else if mult == 1.0e-6 {
        Some("\u{b5}")
    } else if mult == 1.0e-9 {
        Some("n")
    } else {
        None
    }
}

/// `"%.{digits}g" % value`, through Python so every spelling is Python's.
fn format_g(py: Python<'_>, digits: usize, value: &Bound<'_, PyAny>) -> PyResult<String> {
    PyString::new(py, &format!("%.{digits}g"))
        .call_method1("__mod__", (value,))?
        .extract()
}

/// A message format: its type number, name, struct, columns and units.
#[pyclass(name = "DFFormat", module = "corvus_link.mavutil")]
pub struct DfFormat {
    #[pyo3(get, set, name = "type")]
    ftype: PyObject,
    #[pyo3(get)]
    name: String,
    #[pyo3(get, name = "len")]
    flen: PyObject,
    #[pyo3(get)]
    format: String,
    #[pyo3(get)]
    columns: Py<PyList>,
    #[pyo3(get)]
    instance_field: Option<String>,
    units: Option<Vec<String>>,
    #[pyo3(get)]
    msg_struct: String,
    types: Vec<Ty>,
    mults: Vec<Option<f64>>,
    fmts: Vec<char>,
    cols: Vec<String>,
    colhash: HashMap<String, usize>,
    #[pyo3(get)]
    a_indexes: Vec<usize>,
    #[pyo3(get)]
    instance_ofs: Option<i64>,
    #[pyo3(get)]
    instance_len: Option<i64>,
}

impl DfFormat {
    fn build(
        py: Python<'_>,
        ftype: PyObject,
        name: &str,
        flen: PyObject,
        format: &str,
        columns: &str,
        oldfmt: Option<&Bound<'_, DfFormat>>,
    ) -> PyResult<DfFormat> {
        let mut cols: Vec<String> = columns.split(',').map(String::from).collect();
        if cols == [""] {
            cols.clear();
        }
        let mut msg_struct = String::from("<");
        let (mut types, mut mults, mut fmts) = (Vec::new(), Vec::new(), Vec::new());
        for c in format.chars() {
            fmts.push(c);
            match char_info(c) {
                Some(info) => {
                    msg_struct.push_str(info.code);
                    mults.push(info.mult);
                    types.push(info.ty);
                }
                None => {
                    print_to(
                        py,
                        format!("DFFormat: Unsupported format char: '{c}' in message {name}"),
                        false,
                    )?;
                    return Err(PyException::new_err(format!(
                        "Unsupported format char: '{c}' in message {name}"
                    )));
                }
            }
        }
        let mut colhash = HashMap::new();
        for (i, c) in cols.iter().enumerate() {
            colhash.insert(c.clone(), i);
        }
        let a_indexes = fmts
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == 'a')
            .map(|(i, _)| i)
            .collect();
        let mut out = DfFormat {
            ftype,
            name: null_term(name),
            flen,
            format: format.to_string(),
            columns: PyList::new(py, &cols)?.unbind(),
            instance_field: None,
            units: None,
            msg_struct,
            types,
            mults,
            fmts,
            cols,
            colhash,
            a_indexes,
            instance_ofs: None,
            instance_len: None,
        };
        if let Some(old) = oldfmt {
            let old = old.borrow();
            out.units = old.units.clone();
            if let Some(field) = &old.instance_field {
                let idx = *out
                    .colhash
                    .get(field)
                    .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(field.clone()))?;
                out.set_instance_field_inner(py, idx)?;
            }
        }
        Ok(out)
    }

    fn set_instance_field_inner(&mut self, py: Python<'_>, idx: usize) -> PyResult<()> {
        let field = self
            .cols
            .get(idx)
            .ok_or_else(|| PyIndexError::new_err("list index out of range"))?
            .clone();
        self.instance_field = Some(field);
        let chars: Vec<char> = self.format.chars().collect();
        let mut pre = String::new();
        for c in &chars[..idx.min(chars.len())] {
            let info = char_info(*c).ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(c.to_string()))?;
            pre.push_str(info.code);
        }
        let calcsize = py.import("struct")?.getattr("calcsize")?;
        self.instance_ofs = Some(calcsize.call1((pre,))?.extract()?);
        let ifmt = chars
            .get(idx)
            .ok_or_else(|| PyIndexError::new_err("string index out of range"))?;
        self.instance_len = Some(calcsize.call1((ifmt.to_string(),))?.extract()?);
        Ok(())
    }

    fn unit_of(&self, col: &str) -> PyResult<String> {
        match &self.units {
            None => Ok(String::new()),
            Some(units) => {
                let idx = *self
                    .colhash
                    .get(col)
                    .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(col.to_string()))?;
                units
                    .get(idx)
                    .cloned()
                    .ok_or_else(|| PyIndexError::new_err("list index out of range"))
            }
        }
    }
}

#[pymethods]
impl DfFormat {
    #[new]
    #[pyo3(signature = (r#type, name, flen, format, columns, oldfmt = None))]
    fn py_new(
        py: Python<'_>,
        r#type: PyObject,
        name: &Bound<'_, PyAny>,
        flen: PyObject,
        format: &str,
        columns: &str,
        oldfmt: Option<&Bound<'_, DfFormat>>,
    ) -> PyResult<Self> {
        DfFormat::build(py, r#type, &to_string(name)?, flen, format, columns, oldfmt)
    }

    #[getter]
    fn units(&self, py: Python<'_>) -> PyResult<PyObject> {
        match &self.units {
            None => Ok(py.None()),
            Some(u) => Ok(PyList::new(py, u)?.into_any().unbind()),
        }
    }

    #[getter]
    fn msg_types(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        let array = py.import("array")?.getattr("array")?;
        let out = PyList::empty(py);
        for t in &self.types {
            out.append(match t {
                Ty::Array => array.clone(),
                Ty::Str => py.get_type::<PyString>().into_any(),
                Ty::Int | Ty::Long => py.get_type::<PyInt>().into_any(),
                Ty::Float => py.get_type::<PyFloat>().into_any(),
            })?;
        }
        Ok(out.unbind())
    }

    #[getter]
    fn msg_mults(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        Ok(PyList::new(py, &self.mults)?.unbind())
    }

    #[getter]
    fn msg_fmts(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        let strings: Vec<String> = self.fmts.iter().map(|c| c.to_string()).collect();
        Ok(PyList::new(py, strings)?.unbind())
    }

    #[getter]
    fn colhash(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let d = PyDict::new(py);
        for (k, v) in &self.colhash {
            d.set_item(k, v)?;
        }
        Ok(d.unbind())
    }

    fn set_instance_field(&mut self, py: Python<'_>, instance_idx: usize) -> PyResult<()> {
        self.set_instance_field_inner(py, instance_idx)
    }

    /// The units from an FMTU's unit ids; a `#` marks the instance field.
    fn set_unit_ids(
        &mut self,
        py: Python<'_>,
        unit_ids: &Bound<'_, PyAny>,
        unit_lookup: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        if unit_ids.is_none() {
            return Ok(());
        }
        let found: i64 = unit_ids.call_method1("find", ("#",))?.extract()?;
        if found != -1 {
            self.set_instance_field_inner(py, found as usize)?;
        }
        let mut units = vec![String::new(); self.cols.len()];
        let ids_len = unit_ids.len()?;
        for (i, slot) in units.iter_mut().enumerate() {
            if i < ids_len {
                let id = unit_ids.get_item(i)?;
                if unit_lookup.contains(&id)? {
                    *slot = unit_lookup.get_item(&id)?.extract()?;
                }
            }
        }
        self.units = Some(units);
        Ok(())
    }

    /// Fold the multipliers of an FMTU into the units.
    #[allow(clippy::needless_range_loop)]
    fn set_mult_ids(
        &mut self,
        py: Python<'_>,
        mult_ids: &Bound<'_, PyAny>,
        mult_lookup: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let Some(units) = self.units.clone() else {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "object of type 'NoneType' has no len()",
            ));
        };
        let mut units = units;
        for i in 0..units.len() {
            if self
                .mults
                .get(i)
                .ok_or_else(|| PyIndexError::new_err("list index out of range"))?
                .is_some()
                || units[i].is_empty()
            {
                continue;
            }
            let id = mult_ids.get_item(i)?;
            if mult_lookup.contains(&id)? {
                let unitmult: f64 = mult_lookup.get_item(&id)?.extract()?;
                units[i] = match prefix_for(unitmult) {
                    Some(p) => format!("{p}{}", units[i]),
                    None => format!("{} {}", format_g(py, 4, &PyFloat::new(py, unitmult))?, units[i]),
                };
            }
        }
        self.units = Some(units);
        Ok(())
    }

    fn get_unit(&self, col: &str) -> PyResult<String> {
        self.unit_of(col)
    }

    fn __str__(&self, py: Python<'_>) -> PyResult<String> {
        let columns = self.columns.bind(py).repr()?;
        Ok(format!(
            "DFFormat({},{},{},{})",
            self.ftype.bind(py).str()?,
            self.name,
            self.format,
            columns
        ))
    }
}

// ------------------------------------------------------------------ messages ----

fn is_quiet_nan(val: &Bound<'_, PyAny>) -> PyResult<bool> {
    if let Ok(f) = val.downcast::<PyFloat>() {
        let v = f.value();
        if v.is_nan() {
            return Ok(v.to_bits() != 0x7ff8_0000_0000_0000);
        }
    }
    Ok(false)
}

/// One message of a log.
#[pyclass(name = "DFMessage", module = "corvus_link.mavutil", dict, frozen)]
pub struct DfMessage {
    fmt: Py<DfFormat>,
    elements: Py<PyList>,
    apply_multiplier: bool,
    parent: PyObject,
}

impl DfMessage {
    fn new(
        py: Python<'_>,
        fmt: &Py<DfFormat>,
        elements: Vec<PyObject>,
        apply_multiplier: bool,
        parent: &PyObject,
    ) -> PyResult<Py<DfMessage>> {
        Py::new(
            py,
            DfMessage {
                fmt: fmt.clone_ref(py),
                elements: PyList::new(py, elements)?.unbind(),
                apply_multiplier,
                parent: parent.clone_ref(py),
            },
        )
    }

    fn field(&self, py: Python<'_>, field: &str) -> PyResult<PyObject> {
        let fmt = self.fmt.bind(py).borrow();
        let i = *fmt
            .colhash
            .get(field)
            .ok_or_else(|| PyAttributeError::new_err(field.to_string()))?;
        let list_err = || PyIndexError::new_err("list index out of range");
        let fmt_char = *fmt.fmts.get(i).ok_or_else(list_err)?;
        let elements = self.elements.bind(py);
        if fmt_char == 'Z' && fmt.name == "FILE" {
            return Ok(elements.get_item(i).map_err(|_| list_err())?.unbind());
        }
        let element = elements.get_item(i).map_err(|_| list_err())?;
        let mut v: Bound<'_, PyAny> = if let Ok(b) = element.downcast::<PyBytes>() {
            match b.call_method1("decode", ("utf-8",)) {
                Ok(s) => s,
                Err(err) if err.is_instance_of::<pyo3::exceptions::PyUnicodeDecodeError>(py) => {
                    b.call_method1("decode", ("ISO-8859-1",))?
                }
                Err(err) => return Err(err),
            }
        } else {
            element
        };
        let format_i = fmt
            .format
            .chars()
            .nth(i)
            .ok_or_else(|| PyIndexError::new_err("string index out of range"))?;
        let ty = *fmt.types.get(i).ok_or_else(list_err)?;
        if format_i == 'a' {
        } else if format_i != 'M' || self.apply_multiplier {
            v = match ty {
                Ty::Int | Ty::Long => py.get_type::<PyInt>().call1((&v,))?,
                Ty::Float => py.get_type::<PyFloat>().call1((&v,))?,
                Ty::Str => v.str()?.into_any(),
                Ty::Array => py.import("array")?.getattr("array")?.call1((&v,))?,
            };
        }
        if ty == Ty::Str {
            v = PyString::new(py, &null_term_obj(&v)?).into_any();
        }
        if let Some(mult) = fmt.mults.get(i).ok_or_else(list_err)? {
            if self.apply_multiplier {
                v = if *mult > 0.0 && *mult < 1.0 {
                    let divisor = 1.0 / mult;
                    v.div(divisor)?
                } else {
                    v.mul(*mult)?
                };
            }
        }
        Ok(v.unbind())
    }
}

#[pymethods]
impl DfMessage {
    #[getter]
    fn fmt(&self, py: Python<'_>) -> Py<DfFormat> {
        self.fmt.clone_ref(py)
    }

    #[getter]
    fn _elements(&self, py: Python<'_>) -> Py<PyList> {
        self.elements.clone_ref(py)
    }

    #[getter]
    fn _apply_multiplier(&self) -> bool {
        self.apply_multiplier
    }

    #[getter]
    fn _fieldnames(&self, py: Python<'_>) -> Py<PyList> {
        self.fmt.bind(py).borrow().columns.clone_ref(py)
    }

    #[getter]
    fn _parent(&self, py: Python<'_>) -> PyObject {
        self.parent.clone_ref(py)
    }

    fn __getattr__(&self, py: Python<'_>, field: &str) -> PyResult<PyObject> {
        self.field(py, field)
    }

    fn __setattr__(slf: &Bound<'_, Self>, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        let first_upper = name
            .chars()
            .next()
            .ok_or_else(|| PyIndexError::new_err("string index out of range"))?
            .is_uppercase();
        let index = this.fmt.bind(py).borrow().colhash.get(name).copied();
        match index {
            Some(i) if first_upper => {
                let mult = this
                    .fmt
                    .bind(py)
                    .borrow()
                    .mults
                    .get(i)
                    .copied()
                    .ok_or_else(|| PyIndexError::new_err("list index out of range"))?;
                let value = match mult {
                    Some(m) if this.apply_multiplier => value.div(m)?,
                    _ => value.clone(),
                };
                this.elements.bind(py).set_item(i, value)
            }
            _ => slf.getattr("__dict__")?.set_item(name, value),
        }
    }

    fn __delattr__(slf: &Bound<'_, Self>, name: &str) -> PyResult<()> {
        slf.getattr("__dict__")?
            .del_item(name)
            .map_err(|_| PyAttributeError::new_err(name.to_string()))
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        let fmt = self.fmt.bind(py).borrow();
        d.set_item("mavpackettype", &fmt.name)?;
        let cols = fmt.cols.clone();
        drop(fmt);
        for c in cols {
            d.set_item(&c, self.field(py, &c)?)?;
        }
        Ok(d)
    }

    fn get_type(&self, py: Python<'_>) -> String {
        self.fmt.bind(py).borrow().name.clone()
    }

    fn get_fieldnames(&self, py: Python<'_>) -> Py<PyList> {
        self.fmt.bind(py).borrow().columns.clone_ref(py)
    }

    fn __str__(&self, py: Python<'_>) -> PyResult<String> {
        let fmt = self.fmt.bind(py).borrow();
        let cols = fmt.cols.clone();
        let mut ret = format!("{} {{", fmt.name);
        drop(fmt);
        for c in &cols {
            let val = self.field(py, c)?;
            let val = val.bind(py);
            let text = if is_quiet_nan(val)? {
                "qnan".to_string()
            } else {
                val.str()?.to_string_lossy().into_owned()
            };
            ret.push_str(&format!("{c} : {text}, "));
        }
        if !cols.is_empty() {
            ret.truncate(ret.len() - 2);
        }
        ret.push('}');
        Ok(ret)
    }

    fn __getitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        let fmt = self.fmt.bind(py).borrow();
        if fmt.instance_field.is_none() {
            return Err(PyIndexError::new_err(()));
        }
        let k = format!("{}[{}]", fmt.name, key.str()?);
        let messages = self.parent.bind(py).getattr("messages")?;
        if !messages.contains(&k)? {
            return Err(PyIndexError::new_err(()));
        }
        Ok(messages.get_item(&k)?.unbind())
    }

    /// A binary message buffer for this message.
    fn get_msgbuf<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let fmt = self.fmt.bind(py).borrow();
        let mut values: Vec<Bound<'py, PyAny>> = Vec::new();
        for i in 0..fmt.cols.len() {
            if i >= fmt.mults.len() {
                continue;
            }
            let mul = fmt.mults[i];
            let mut name = fmt.cols[i].clone();
            if name == "Mode" && fmt.cols.iter().any(|c| c == "ModeNum") {
                name = "ModeNum".to_string();
            }
            let mut v = self.field(py, &name)?.into_bound(py);
            if v.is_instance_of::<PyString>() {
                v = match v.call_method1("encode", ("ascii",)) {
                    Ok(b) => b,
                    Err(err) if err.is_instance_of::<pyo3::exceptions::PyUnicodeEncodeError>(py) => {
                        v.call_method0("encode")?
                    }
                    Err(err) => return Err(err),
                };
            } else if v.get_type().name()?.to_string_lossy() == "array" {
                v = v.call_method0("tobytes")?;
            }
            if let Some(m) = mul {
                v = v.div(m)?;
                let rounded = py.import("builtins")?.getattr("round")?.call1((&v,))?;
                v = py.get_type::<PyInt>().call1((rounded,))?;
            }
            values.push(v);
        }
        let struct_mod = py.import("struct")?;
        let ftype = fmt.ftype.bind(py);
        let head = struct_mod.call_method1("pack", ("BBB", 0xA3, 0x95, ftype))?;
        let mut args: Vec<Bound<'py, PyAny>> = vec![PyString::new(py, &fmt.msg_struct).into_any()];
        args.extend(values);
        match struct_mod.call_method1("pack", PyTuple::new(py, args)?) {
            Ok(body) => Ok(Some(head.add(body)?)),
            Err(_) => Ok(None),
        }
    }

    /// The message, one field to a line, with its units.
    fn dump_verbose(slf: &Bound<'_, Self>, f: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        let ts: Bound<'_, PyAny> = slf.getattr("_timestamp")?;
        let time = py.import("time")?;
        let stamp = time.call_method1(
            "strftime",
            ("%Y-%m-%d %H:%M:%S", time.call_method1("localtime", (&ts,))?),
        )?;
        let millis = py
            .get_type::<PyInt>()
            .call1((ts.mul(1000.0)?,))?
            .call_method1("__mod__", (1000,))?;
        let name = this.fmt.bind(py).borrow().name.clone();
        f.call_method1(
            "write",
            (format!("{}.{:03}: {}\n", stamp, millis.extract::<i64>()?, name),),
        )?;
        let cols = this.fmt.bind(py).borrow().cols.clone();
        for c in cols {
            let val = this.field(py, &c)?.into_bound(py);
            let shown = if is_quiet_nan(&val)? {
                PyString::new(py, "qnan").into_any()
            } else {
                val.clone()
            };
            f.call_method1("write", (format!("    {c}: {}", shown.str()?),))?;
            let unit = this.fmt.bind(py).borrow().unit_of(&c)?;
            if unit.starts_with("rad") {
                let deg = py.import("math")?.call_method1("degrees", (&val,))?;
                f.call_method1(
                    "write",
                    (format!(" {unit} ({} {})", deg.str()?, unit.replace("rad", "deg")),),
                )?;
            } else {
                f.call_method1("write", (format!(" {unit}"),))?;
            }
            f.call_method1("write", ("\n",))?;
        }
        Ok(())
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.fmt)?;
        visit.call(&self.elements)?;
        visit.call(&self.parent)
    }
}

// ------------------------------------------------------------------- clocks ----

/// `m.<name> * factor`, with Python's own arithmetic.
fn msg_mul(py: Python<'_>, m: &Bound<'_, DfMessage>, name: &str, factor: f64) -> PyResult<f64> {
    m.get().field(py, name)?.bind(py).mul(factor)?.extract::<f64>()
}

/// pymavlink's `_gpsTimeToTime(week, msec)` on the raw field values.
fn gps_time(py: Python<'_>, week: &Bound<'_, PyAny>, msec: &Bound<'_, PyAny>) -> PyResult<f64> {
    let weeks = 604_800i64.into_pyobject(py)?.into_any().mul(week)?;
    let total = 315_964_800i64
        .into_pyobject(py)?
        .into_any()
        .add(weeks)?
        .add(msec.mul(0.001)?)?
        .sub(18)?;
    total.extract::<f64>()
}

fn field_gps_time(py: Python<'_>, m: &Bound<'_, DfMessage>, week: &str, msec: &str) -> PyResult<f64> {
    gps_time(py, m.get().field(py, week)?.bind(py), m.get().field(py, msec)?.bind(py))
}

/// `getattr(m, name, None)`: only a missing column is None.
fn try_field(py: Python<'_>, m: &Bound<'_, DfMessage>, name: &str) -> PyResult<Option<PyObject>> {
    match m.get().field(py, name) {
        Ok(v) => Ok(Some(v)),
        Err(err) if err.is_instance_of::<PyAttributeError>(py) => Ok(None),
        Err(err) => Err(err),
    }
}

fn first_fieldname(py: Python<'_>, m: &Bound<'_, DfMessage>) -> PyResult<String> {
    m.get()
        .fmt
        .bind(py)
        .borrow()
        .cols
        .first()
        .cloned()
        .ok_or_else(|| PyIndexError::new_err("list index out of range"))
}

fn set_timestamp(m: &Bound<'_, DfMessage>, value: Num) -> PyResult<()> {
    m.setattr("_timestamp", value.to_py(m.py()))
}

struct GpsClock {
    timebase: Num,
    msg_rate: HashMap<String, f64>,
    counts: HashMap<String, i64>,
    counts_since_gps: HashMap<String, i64>,
}

enum Clock {
    Gps(GpsClock),
    Usec {
        timebase: Num,
        timestamp: Num,
        first_us_stamp: Option<f64>,
    },
    Msec {
        timebase: Num,
        timestamp: Num,
        first_ms_stamp: Option<f64>,
    },
    Px4 {
        timebase: Num,
        px4_timebase: Num,
    },
}

impl Clock {
    fn gps() -> Clock {
        Clock::Gps(GpsClock {
            timebase: Num::Zero,
            msg_rate: HashMap::new(),
            counts: HashMap::new(),
            counts_since_gps: HashMap::new(),
        })
    }

    fn rewind_event(&mut self) {
        match self {
            Clock::Gps(g) => {
                g.counts.clear();
                g.counts_since_gps.clear();
            }
            Clock::Usec {
                timebase,
                timestamp,
                first_us_stamp,
            } => {
                *timestamp = *timebase;
                if let Some(first) = first_us_stamp {
                    *timestamp = timestamp.add_f(*first * 0.000001);
                }
            }
            Clock::Msec {
                timebase,
                timestamp,
                first_ms_stamp,
            } => {
                *timestamp = *timebase;
                if let Some(first) = first_ms_stamp {
                    *timestamp = timestamp.add_f(*first * 0.001);
                }
            }
            Clock::Px4 { .. } => {}
        }
    }

    fn message_arrived(&mut self, py: Python<'_>, m: &Bound<'_, DfMessage>) -> PyResult<()> {
        match self {
            Clock::Px4 { px4_timebase, .. } => {
                let fmt = m.get().fmt.bind(py).borrow();
                if fmt.name == "TIME" && fmt.cols.iter().any(|c| c == "StartTime") {
                    drop(fmt);
                    *px4_timebase = Num::F(msg_mul(py, m, "StartTime", 1.0e-6)?);
                }
                Ok(())
            }
            Clock::Gps(g) => {
                let kind = m.get().fmt.bind(py).borrow().name.clone();
                *g.counts.entry(kind.clone()).or_insert(0) += 1;
                *g.counts_since_gps.entry(kind.clone()).or_insert(0) += 1;
                if kind == "GPS" || kind == "GPS2" {
                    g.gps_message_arrived(py, m)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    fn set_message_timestamp(&mut self, py: Python<'_>, m: &Bound<'_, DfMessage>) -> PyResult<()> {
        match self {
            Clock::Usec {
                timebase, timestamp, ..
            } => {
                let first = first_fieldname(py, m)?;
                let name = m.get().fmt.bind(py).borrow().name.clone();
                let ts = if first == "TimeUS" {
                    timebase.add_f(msg_mul(py, m, "TimeUS", 0.000001)?)
                } else if !name.starts_with("ACC")
                    && !name.starts_with("GYR")
                    && first == "TimeMS"
                    && !(timebase.f() + msg_mul(py, m, "TimeMS", 0.001)? < timestamp.f())
                {
                    timebase.add_f(msg_mul(py, m, "TimeMS", 0.001)?)
                } else {
                    *timestamp
                };
                set_timestamp(m, ts)?;
                *timestamp = ts;
            }
            Clock::Msec {
                timebase, timestamp, ..
            } => {
                let first = first_fieldname(py, m)?;
                let name = m.get().fmt.bind(py).borrow().name.clone();
                let ts = if first == "TimeMS" {
                    timebase.add_f(msg_mul(py, m, "TimeMS", 0.001)?)
                } else if name == "GPS" || name == "GPS2" {
                    timebase.add_f(msg_mul(py, m, "T", 0.001)?)
                } else {
                    *timestamp
                };
                set_timestamp(m, ts)?;
                *timestamp = ts;
            }
            Clock::Px4 { timebase, px4_timebase } => {
                set_timestamp(m, timebase.add(*px4_timebase))?;
            }
            Clock::Gps(g) => {
                let name = m.get().fmt.bind(py).borrow().name.clone();
                let mut rate = g.msg_rate.get(&name).copied().unwrap_or(50.0);
                if rate as i64 == 0 {
                    rate = 50.0;
                }
                let count = g.counts_since_gps.get(&name).copied().unwrap_or(0);
                set_timestamp(m, g.timebase.add_f(count as f64 / rate))?;
            }
        }
        Ok(())
    }
}

impl GpsClock {
    fn gps_message_arrived(&mut self, py: Python<'_>, m: &Bound<'_, DfMessage>) -> PyResult<()> {
        let mut week = try_field(py, m, "Week")?;
        let mut timems = try_field(py, m, "TimeMS")?;
        if week.is_none() {
            week = try_field(py, m, "GWk")?;
            timems = try_field(py, m, "GMS")?;
            if week.is_none() && try_field(py, m, "GPSTime")?.is_some() {
                return Ok(());
            }
        }
        if week.is_none() && m.get().fmt.bind(py).borrow().colhash.contains_key("Wk") {
            week = Some(m.get().field(py, "Wk")?);
            timems = Some(m.get().field(py, "TWk")?);
        }
        let Some(week) = week else {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "unsupported operand type(s) for *: 'int' and 'NoneType'",
            ));
        };
        let Some(timems) = timems else {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "unsupported operand type(s) for *: 'NoneType' and 'float'",
            ));
        };
        let t = gps_time(py, week.bind(py), timems.bind(py))?;
        let deltat = t - self.timebase.f();
        if deltat <= 0.0 {
            return Ok(());
        }
        for (kind, count) in &self.counts_since_gps {
            let rate = *count as f64 / deltat;
            if rate > self.msg_rate.get(kind).copied().unwrap_or(0.0) {
                self.msg_rate.insert(kind.clone(), rate);
            }
        }
        self.msg_rate.insert("IMU".to_string(), 50.0);
        self.timebase = Num::F(t);
        self.counts_since_gps.clear();
        Ok(())
    }
}

// ------------------------------------------------------------------- reader ----

/// Reads a struct format's worth of bytes into Python values.
fn unpack_elements(py: Python<'_>, msg_struct: &str, body: &[u8]) -> PyResult<Vec<PyObject>> {
    let struct_mod = py.import("struct")?;
    let unpacked = struct_mod.call_method1("unpack", (msg_struct, PyBytes::new(py, body)))?;
    unpacked
        .try_iter()?
        .map(|v| v.map(|v| v.unbind()))
        .collect::<PyResult<Vec<_>>>()
}

fn array_h<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    py.import("array")?.getattr("array")?.call1(("h", value))
}

fn msg_types_in(types: &[String], name: &str) -> bool {
    types.iter().any(|t| t == name)
}

const KEY_TYPES: [&str; 6] = ["MODE", "MSG", "PARM", "STAT", "ORGN", "VER"];

/// A DataFlash log. `DFReader_binary` and `DFReader_text` are this with the
/// file format they read.
#[pyclass(name = "DFReader", module = "corvus_link.mavutil", subclass)]
pub struct DfReader {
    binary: bool,
    me: Option<PyObject>,
    data: Vec<u8>,
    closed: bool,
    clock: Option<Clock>,
    mav_type: i64,
    verbose: bool,
    params: Py<PyDict>,
    param_defaults: Option<Py<PyDict>>,
    flightmodes: Option<Py<PyList>>,
    messages: Py<PyDict>,
    percent: PyObject,
    unit_lookup: Py<PyDict>,
    mult_lookup: Py<PyDict>,
    formats: Py<PyDict>,
    name_to_id: Py<PyDict>,
    id_to_name: Py<PyDict>,
    flightmode: PyObject,
    zero_time_base: bool,
    offset: i64,
    remaining: i64,
    prev_type: Option<u8>,
    offsets: Vec<Vec<usize>>,
    counts: Vec<i64>,
    count: i64,
    type_nums: Option<Vec<u8>>,
    indexes: Vec<usize>,
    delimiter: String,
    t_offsets: IndexMap<String, Vec<i64>>,
    t_counts: IndexMap<String, i64>,
    type_list: Option<Vec<String>>,
}

#[pyclass(name = "DFReader_binary", module = "corvus_link.mavutil", extends = DfReader)]
pub struct DfReaderBinary;

#[pyclass(name = "DFReader_text", module = "corvus_link.mavutil", extends = DfReader)]
pub struct DfReaderText;

impl DfReader {
    fn new(py: Python<'_>, binary: bool, data: Vec<u8>, zero_time_base: bool) -> PyResult<DfReader> {
        let fmt_fmt = Py::new(
            py,
            DfFormat::build(
                py,
                int_obj(py, 0x80),
                "FMT",
                int_obj(py, 89),
                "BBnNZ",
                "Type,Length,Name,Format,Columns",
                None,
            )?,
        )?;
        let formats = PyDict::new(py);
        if binary {
            formats.set_item(0x80, fmt_fmt)?;
        } else {
            formats.set_item("FMT", fmt_fmt)?;
        }
        let id_to_name = PyDict::new(py);
        if !binary {
            id_to_name.set_item(0x80, "FMT")?;
        }
        let remaining = data.len() as i64;
        Ok(DfReader {
            binary,
            me: None,
            data,
            closed: false,
            clock: None,
            mav_type: MAV_TYPE_FIXED_WING,
            verbose: false,
            params: PyDict::new(py).unbind(),
            param_defaults: None,
            flightmodes: None,
            messages: PyDict::new(py).unbind(),
            percent: int_obj(py, 0),
            unit_lookup: PyDict::new(py).unbind(),
            mult_lookup: PyDict::new(py).unbind(),
            formats: formats.unbind(),
            name_to_id: PyDict::new(py).unbind(),
            id_to_name: id_to_name.unbind(),
            flightmode: PyString::new(py, "UNKNOWN").into_any().unbind(),
            zero_time_base,
            offset: 0,
            remaining,
            prev_type: None,
            offsets: vec![Vec::new(); 256],
            counts: vec![0; 256],
            count: 0,
            type_nums: None,
            indexes: Vec::new(),
            delimiter: ", ".to_string(),
            t_offsets: IndexMap::new(),
            t_counts: IndexMap::new(),
            type_list: None,
        })
    }

    fn me(&self, py: Python<'_>) -> PyObject {
        self.me.as_ref().expect("reader is bound to its object").clone_ref(py)
    }

    fn dlen(&self) -> i64 {
        self.data.len() as i64
    }

    fn check_open(&self) -> PyResult<()> {
        if self.closed {
            return Err(PyValueError::new_err("mmap closed or invalid"));
        }
        Ok(())
    }

    fn reset_messages(&mut self, py: Python<'_>) -> PyResult<()> {
        let old = self.messages.bind(py);
        old.clear();
        let fresh = PyDict::new(py);
        fresh.set_item("MAV", self.me(py))?;
        fresh.set_item("__MAV__", self.me(py))?;
        self.messages = fresh.unbind();
        Ok(())
    }

    fn rewind_base(&mut self, py: Python<'_>, keep_messages: bool) -> PyResult<()> {
        if !keep_messages {
            self.reset_messages(py)?;
        }
        self.flightmode = match &self.flightmodes {
            Some(list) if !list.bind(py).is_empty() => list.bind(py).get_item(0)?.get_item(0)?.unbind(),
            _ => PyString::new(py, "UNKNOWN").into_any().unbind(),
        };
        self.percent = int_obj(py, 0);
        if let Some(clock) = &mut self.clock {
            clock.rewind_event();
        }
        Ok(())
    }

    fn rewind_inner(&mut self, py: Python<'_>, keep_messages: bool) -> PyResult<()> {
        self.rewind_base(py, keep_messages)?;
        if self.binary {
            self.offset = 0;
            self.remaining = self.dlen();
            self.type_nums = None;
        } else {
            self.offset = py_find(&self.data, b"FMT, ", 0);
            if self.offset == -1 {
                self.offset = py_find(&self.data, b"FMT,", 0);
                if self.offset != -1 {
                    self.delimiter = ",".to_string();
                }
            }
            self.type_list = None;
        }
        Ok(())
    }

    fn format_by_id(&self, py: Python<'_>, mtype: i64) -> PyResult<Option<Py<DfFormat>>> {
        match self.formats.bind(py).get_item(mtype)? {
            Some(f) => Ok(Some(f.extract()?)),
            None => Ok(None),
        }
    }

    fn format_by_name(&self, py: Python<'_>, name: &str) -> PyResult<Option<Py<DfFormat>>> {
        match self.formats.bind(py).get_item(name)? {
            Some(f) => Ok(Some(f.extract()?)),
            None => Ok(None),
        }
    }

    fn flen(py: Python<'_>, fmt: &Py<DfFormat>) -> PyResult<i64> {
        fmt.bind(py).borrow().flen.bind(py).extract()
    }

    // ---- shared logic

    fn add_msg(&mut self, py: Python<'_>, m: &Bound<'_, DfMessage>) -> PyResult<()> {
        let (kind, instance_field, fieldnames) = {
            let fmt = m.get().fmt.bind(py).borrow();
            (fmt.name.clone(), fmt.instance_field.clone(), fmt.cols.clone())
        };
        self.messages.bind(py).set_item(&kind, m)?;
        if let Some(field) = &instance_field {
            let i = m.get().field(py, field)?;
            self.messages
                .bind(py)
                .set_item(format!("{kind}[{}]", i.bind(py).str()?), m)?;
        }
        if let Some(clock) = &mut self.clock {
            clock.message_arrived(py, m)?;
        }
        let has = |name: &str| fieldnames.iter().any(|c| c == name);
        if kind == "MSG" && has("Message") {
            let message: String = m.get().field(py, "Message")?.bind(py).extract()?;
            if message.contains("Rover") {
                self.mav_type = MAV_TYPE_GROUND_ROVER;
            } else if message.contains("Plane") {
                self.mav_type = MAV_TYPE_FIXED_WING;
            } else if message.contains("Copter") {
                self.mav_type = MAV_TYPE_QUADROTOR;
            } else if message.starts_with("Antenna") {
                self.mav_type = MAV_TYPE_ANTENNA_TRACKER;
            } else if message.contains("ArduSub") {
                self.mav_type = MAV_TYPE_SUBMARINE;
            } else if message.contains("Blimp") {
                self.mav_type = MAV_TYPE_AIRSHIP;
            }
        }
        if kind == "VER" && has("BU") {
            let bu = m.get().field(py, "BU")?;
            let mapped = match bu.bind(py).extract::<i64>().ok() {
                Some(1) => Some(MAV_TYPE_GROUND_ROVER),
                Some(2) => Some(MAV_TYPE_QUADROTOR),
                Some(3) => Some(MAV_TYPE_FIXED_WING),
                Some(4) => Some(MAV_TYPE_ANTENNA_TRACKER),
                Some(7) => Some(MAV_TYPE_SUBMARINE),
                Some(13) => Some(MAV_TYPE_HELICOPTER),
                Some(12) => Some(MAV_TYPE_AIRSHIP),
                _ => None,
            };
            if let Some(t) = mapped {
                self.mav_type = t;
            }
        }
        if kind == "MODE" {
            let mavutil = py.import("corvus_link.mavutil")?;
            let mode_is_str = if has("Mode") {
                m.get().field(py, "Mode")?.bind(py).is_instance_of::<PyString>()
            } else {
                false
            };
            if mode_is_str {
                let text = m.get().field(py, "Mode")?;
                self.flightmode = text.bind(py).call_method0("upper")?.unbind();
            } else if has("ModeNum") {
                let mapping = mavutil.call_method1("mode_mapping_bynumber", (self.mav_type,))?;
                let num = m.get().field(py, "ModeNum")?;
                if !mapping.is_none() && mapping.contains(num.bind(py))? {
                    self.flightmode = mapping.get_item(num.bind(py))?.unbind();
                } else {
                    self.flightmode = PyString::new(py, "UNKNOWN").into_any().unbind();
                }
            } else if has("Mode") {
                let mode = m.get().field(py, "Mode")?;
                self.flightmode = mavutil.call_method1("mode_string_acm", (mode,))?.unbind();
            }
        }
        if kind == "STAT" && has("MainState") {
            let state = m.get().field(py, "MainState")?;
            self.flightmode = py
                .import("corvus_link.mavutil")?
                .call_method1("mode_string_px4", (state,))?
                .unbind();
        }
        if kind == "PARM" {
            if let Some(name) = try_field(py, m, "Name")? {
                if !name.bind(py).is_none() {
                    let value = m.get().field(py, "Value")?;
                    self.params.bind(py).set_item(name.bind(py), value)?;
                    if has("Default") {
                        let default = m.get().field(py, "Default")?;
                        let is_nan = py
                            .import("math")?
                            .call_method1("isnan", (default.bind(py),))?
                            .is_truthy()?;
                        if !is_nan {
                            let defaults = match &self.param_defaults {
                                Some(d) => d.clone_ref(py),
                                None => {
                                    let d = PyDict::new(py).unbind();
                                    self.param_defaults = Some(d.clone_ref(py));
                                    d
                                }
                            };
                            defaults.bind(py).set_item(name.bind(py), default)?;
                        }
                    }
                }
            }
        }
        self.set_time(py, m)
    }

    fn set_time(&mut self, py: Python<'_>, m: &Bound<'_, DfMessage>) -> PyResult<()> {
        m.setattr("_timestamp", int_obj(py, 0))?;
        if !m.get().fmt.bind(py).borrow().cols.is_empty() {
            if let Some(clock) = &mut self.clock {
                clock.set_message_timestamp(py, m)?;
            }
        }
        Ok(())
    }

    fn parse_next(&mut self, py: Python<'_>) -> PyResult<Option<Py<DfMessage>>> {
        self.check_open()?;
        if self.binary {
            self.parse_next_binary(py)
        } else {
            self.parse_next_text(py)
        }
    }

    fn skip_types(&mut self, py: Python<'_>, types: &[String], strict: bool) -> PyResult<()> {
        if self.binary {
            if self.type_nums.is_none() {
                let mut wanted: Vec<String> = types.to_vec();
                if !strict {
                    for k in KEY_TYPES {
                        if !msg_types_in(&wanted, k) {
                            wanted.push(k.to_string());
                        }
                    }
                }
                self.indexes = Vec::new();
                let mut nums = Vec::new();
                for t in &wanted {
                    if let Some(id) = self.name_to_id.bind(py).get_item(t)? {
                        nums.push(id.extract::<u8>()?);
                        self.indexes.push(0);
                    }
                }
                self.type_nums = Some(nums);
            }
            let nums = self.type_nums.clone().unwrap_or_default();
            let mut smallest_index: Option<usize> = None;
            let mut smallest_offset = self.data.len();
            for (i, mtype) in nums.iter().enumerate() {
                let mtype = *mtype as usize;
                if self.indexes[i] as i64 >= self.counts[mtype] {
                    continue;
                }
                let ofs = self.offsets[mtype][self.indexes[i]];
                if ofs < smallest_offset {
                    smallest_offset = ofs;
                    smallest_index = Some(i);
                }
            }
            match smallest_index {
                Some(i) => {
                    self.indexes[i] += 1;
                    self.offset = smallest_offset as i64;
                }
                None => self.offset = self.dlen(),
            }
        } else {
            if self.type_list.is_none() {
                let mut list: Vec<String> = types.to_vec();
                if !strict {
                    for k in KEY_TYPES {
                        if !msg_types_in(&list, k) {
                            list.push(k.to_string());
                        }
                    }
                }
                self.indexes = vec![0; list.len()];
                self.type_list = Some(list);
            }
            let list = self.type_list.clone().unwrap_or_default();
            let mut smallest_index: Option<usize> = None;
            let mut smallest_offset = self.dlen();
            for (i, mtype) in list.iter().enumerate() {
                let Some(count) = self.t_counts.get(mtype) else {
                    continue;
                };
                if self.indexes[i] as i64 >= *count {
                    continue;
                }
                let ofs = self.t_offsets[mtype][self.indexes[i]];
                if ofs < smallest_offset {
                    smallest_offset = ofs;
                    smallest_index = Some(i);
                }
            }
            if let Some(i) = smallest_index {
                self.indexes[i] += 1;
                self.offset = smallest_offset;
            }
        }
        Ok(())
    }

    fn recv_match_inner(
        &mut self,
        py: Python<'_>,
        types: Option<&[String]>,
        strict: bool,
    ) -> PyResult<Option<Py<DfMessage>>> {
        loop {
            if let Some(t) = types {
                self.skip_types(py, t, strict)?;
            }
            let Some(m) = self.parse_next(py)? else {
                return Ok(None);
            };
            if let Some(t) = types {
                if !msg_types_in(t, &m.bind(py).get().fmt.bind(py).borrow().name) {
                    continue;
                }
            }
            return Ok(Some(m));
        }
    }

    fn last_timestamp_inner(&mut self, py: Python<'_>) -> PyResult<PyObject> {
        if self.binary {
            let (mut highest, mut second) = (0usize, 0usize);
            for i in 0..256 {
                if self.counts[i] == -1 || self.offsets[i].is_empty() {
                    continue;
                }
                let ofs = *self.offsets[i].last().unwrap_or(&0);
                if ofs > highest {
                    second = highest;
                    highest = ofs;
                } else if ofs > second {
                    second = ofs;
                }
            }
            self.offset = highest as i64;
            let mut m = self.parse_next(py)?;
            if m.is_none() {
                self.offset = second as i64;
                m = self.parse_next(py)?;
            }
            return timestamp_of(py, m);
        }
        let mut highest = 0i64;
        for offsets in self.t_offsets.values() {
            if let Some(ofs) = offsets.last() {
                if *ofs > highest {
                    highest = *ofs;
                }
            }
        }
        self.offset = highest;
        let m = self.parse_next(py)?;
        timestamp_of(py, m)
    }

    fn init_clock(&mut self, py: Python<'_>) -> PyResult<()> {
        self.rewind_inner(py, true)?;
        self.clock = Some(Clock::gps());
        let mut px4_msg_time: Option<Py<DfMessage>> = None;
        let mut px4_msg_gps: Option<Py<DfMessage>> = None;
        let mut gps_interp_msg_gps1: Option<Py<DfMessage>> = None;
        let mut first_us_stamp: Option<PyObject> = None;
        let mut first_ms_stamp: Option<PyObject> = None;
        let mut have_good_clock = false;
        let mut first_modern_gps_message: Option<Py<DfMessage>> = None;
        let gps_parm = ["GPS".to_string(), "PARM".to_string()];

        loop {
            let Some(m) = self.recv_match_inner(py, Some(&gps_parm), true)? else {
                break;
            };
            let mb = m.bind(py);
            if mb.get().fmt.bind(py).borrow().name == "PARM" {
                continue;
            }
            if try_field(py, mb, "TimeUS")?.is_none()
                || try_field(py, mb, "GWk")?.is_none()
                || try_field(py, mb, "GMS")?.is_none()
            {
                break;
            }
            first_modern_gps_message = Some(m.clone_ref(py));
            if mb.get().field(py, "GWk")?.bind(py).gt(0)? {
                break;
            }
        }

        self.rewind_inner(py, true)?;
        loop {
            let Some(m) = self.parse_next(py)? else { break };
            let mb = m.bind(py);
            let kind = mb.get().fmt.bind(py).borrow().name.clone();

            if first_us_stamp.is_none() {
                first_us_stamp = try_field(py, mb, "TimeUS")?;
                if let (Some(gps), Some(us)) = (&first_modern_gps_message, &first_us_stamp) {
                    if gps.bind(py).get().field(py, "GWk")?.bind(py).le(0)? {
                        break;
                    }
                    let mut clock = Clock::Usec {
                        timebase: Num::Zero,
                        timestamp: Num::Zero,
                        first_us_stamp: None,
                    };
                    if !self.zero_time_base {
                        let g = gps.bind(py);
                        let t = field_gps_time(py, g, "GWk", "GMS")?;
                        clock = Clock::Usec {
                            timebase: Num::F(t - msg_mul(py, g, "TimeUS", 0.000001)?),
                            timestamp: Num::Zero,
                            first_us_stamp: Some(us.bind(py).extract()?),
                        };
                    }
                    self.clock = Some(clock);
                    have_good_clock = true;
                    break;
                }
            }

            if first_ms_stamp.is_none() && kind != "GPS" && kind != "GPS2" {
                first_ms_stamp = try_field(py, mb, "TimeMS")?;
            }

            if kind == "GPS" || kind == "GPS2" {
                let nonzero = |name: &str| -> PyResult<bool> {
                    match try_field(py, mb, name)? {
                        None => Ok(false),
                        Some(v) => v.bind(py).ne(0),
                    }
                };
                if nonzero("TimeUS")? && nonzero("GWk")? {
                    let mut clock = Clock::Usec {
                        timebase: Num::Zero,
                        timestamp: Num::Zero,
                        first_us_stamp: None,
                    };
                    if !self.zero_time_base {
                        let t = field_gps_time(py, mb, "GWk", "GMS")?;
                        let first = match &first_us_stamp {
                            Some(v) => Some(v.bind(py).extract::<f64>()?),
                            None => None,
                        };
                        clock = Clock::Usec {
                            timebase: Num::F(t - msg_mul(py, mb, "TimeUS", 0.000001)?),
                            timestamp: Num::Zero,
                            first_us_stamp: first,
                        };
                    }
                    self.clock = Some(clock);
                    have_good_clock = true;
                    break;
                }
                if nonzero("T")? && nonzero("Week")? {
                    if first_ms_stamp.is_none() {
                        first_ms_stamp = Some(mb.get().field(py, "T")?);
                    }
                    let mut clock = Clock::Msec {
                        timebase: Num::Zero,
                        timestamp: Num::Zero,
                        first_ms_stamp: None,
                    };
                    if !self.zero_time_base {
                        let t = field_gps_time(py, mb, "Week", "TimeMS")?;
                        let first = match &first_ms_stamp {
                            Some(v) => Some(v.bind(py).extract::<f64>()?),
                            None => None,
                        };
                        clock = Clock::Msec {
                            timebase: Num::F(t - msg_mul(py, mb, "T", 0.001)?),
                            timestamp: Num::Zero,
                            first_ms_stamp: first,
                        };
                    }
                    self.clock = Some(clock);
                    have_good_clock = true;
                    break;
                }
                if nonzero("GPSTime")? {
                    px4_msg_gps = Some(m.clone_ref(py));
                }
                if nonzero("Week")? {
                    if let Some(prev) = &gps_interp_msg_gps1 {
                        let p = prev.bind(py);
                        if p.get()
                            .field(py, "TimeMS")?
                            .bind(py)
                            .ne(mb.get().field(py, "TimeMS")?.bind(py))?
                            || p.get()
                                .field(py, "Week")?
                                .bind(py)
                                .ne(mb.get().field(py, "Week")?.bind(py))?
                        {
                            have_good_clock = true;
                            break;
                        }
                    }
                    gps_interp_msg_gps1 = Some(m.clone_ref(py));
                }
            } else if kind == "TIME" {
                if let Some(v) = try_field(py, mb, "StartTime")? {
                    let _ = v;
                    px4_msg_time = Some(m.clone_ref(py));
                }
            }

            if let (Some(t), Some(g)) = (&px4_msg_time, &px4_msg_gps) {
                let mut timebase = Num::Zero;
                let mut px4_timebase = Num::Zero;
                if !self.zero_time_base {
                    px4_timebase = Num::F(msg_mul(py, t.bind(py), "StartTime", 1.0e-6)?);
                    timebase = Num::F(msg_mul(py, g.bind(py), "GPSTime", 1.0e-6)? - px4_timebase.f());
                }
                self.clock = Some(Clock::Px4 { timebase, px4_timebase });
                have_good_clock = true;
                break;
            }
        }

        if !have_good_clock {
            if first_us_stamp.is_some() {
                self.clock = Some(Clock::Usec {
                    timebase: Num::Zero,
                    timestamp: Num::Zero,
                    first_us_stamp: None,
                });
            } else if first_ms_stamp.is_some() {
                self.clock = Some(Clock::Msec {
                    timebase: Num::Zero,
                    timestamp: Num::Zero,
                    first_ms_stamp: None,
                });
            }
        }
        self.rewind_inner(py, true)
    }
}

fn timestamp_of(py: Python<'_>, m: Option<Py<DfMessage>>) -> PyResult<PyObject> {
    match m {
        Some(m) => Ok(m.bind(py).getattr("_timestamp")?.unbind()),
        None => Err(PyAttributeError::new_err(
            "'NoneType' object has no attribute '_timestamp'",
        )),
    }
}

// ---------------------------------------------------- binary and text logs ----

impl DfReader {
    fn new_message(
        &self,
        py: Python<'_>,
        fmt: &Py<DfFormat>,
        elements: Vec<PyObject>,
        apply_multiplier: bool,
    ) -> PyResult<Py<DfMessage>> {
        DfMessage::new(py, fmt, elements, apply_multiplier, &self.me(py))
    }

    /// pymavlink builds a format from the six strings of an `FMT` message.
    fn format_from_elements(&self, py: Python<'_>, elements: &[PyObject]) -> PyResult<Py<DfFormat>> {
        let ftype = elements
            .first()
            .ok_or_else(|| PyIndexError::new_err("list index out of range"))?;
        let get = |i: usize| {
            elements
                .get(i)
                .ok_or_else(|| PyIndexError::new_err("list index out of range"))
        };
        let name = null_term_obj(get(2)?.bind(py))?;
        let format = null_term_obj(get(3)?.bind(py))?;
        let columns = null_term_obj(get(4)?.bind(py))?;
        let old = self.formats.bind(py).get_item(ftype.bind(py))?;
        let old = match &old {
            Some(o) => Some(o.downcast::<DfFormat>()?),
            None => None,
        };
        Py::new(
            py,
            DfFormat::build(
                py,
                ftype.clone_ref(py),
                &name,
                get(1)?.clone_ref(py),
                &format,
                &columns,
                old,
            )?,
        )
    }

    fn parse_next_binary(&mut self, py: Python<'_>) -> PyResult<Option<Py<DfMessage>>> {
        loop {
            let mut skip_type: Option<(u8, u8, u8)> = None;
            let mut skip_start: i64 = 0;
            let msg_type: u8;
            loop {
                if self.dlen() - self.offset < 3 {
                    return Ok(None);
                }
                let o = self.offset as usize;
                let hdr = (self.data[o], self.data[o + 1], self.data[o + 2]);
                if hdr.0 == 0xA3 && hdr.1 == 0x95 {
                    if let Some(st) = skip_type.take() {
                        if self.remaining >= 528 {
                            let prev = match self.prev_type {
                                Some(p) => p.to_string(),
                                None => "None".to_string(),
                            };
                            print_to(
                                py,
                                format!(
                                    "Skipped {} bad bytes in log at offset {skip_start}, type=({}, {}, {}) (prev={prev})",
                                    self.offset - skip_start,
                                    st.0,
                                    st.1,
                                    st.2
                                ),
                                true,
                            )?;
                        }
                    }
                    if self.format_by_id(py, hdr.2 as i64)?.is_some() {
                        self.prev_type = Some(hdr.2);
                        msg_type = hdr.2;
                        break;
                    }
                }
                if skip_type.is_none() {
                    skip_type = Some(hdr);
                    skip_start = self.offset;
                }
                self.offset += 1;
                self.remaining -= 1;
            }

            self.offset += 3;
            self.remaining = self.dlen() - self.offset;
            let fmt = self.format_by_id(py, msg_type as i64)?.expect("checked above");
            let flen = Self::flen(py, &fmt)?;
            if self.remaining < flen - 3 {
                if self.verbose {
                    print_to(py, "out of data".to_string(), true)?;
                }
                return Ok(None);
            }
            let body = py_slice(&self.data, self.offset, self.offset + flen - 3).to_vec();
            let (msg_struct, name, a_indexes) = {
                let f = fmt.bind(py).borrow();
                (f.msg_struct.clone(), f.name.clone(), f.a_indexes.clone())
            };
            let mut elements = match unpack_elements(py, &msg_struct, &body) {
                Ok(e) => e,
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    print_to(py, err.value(py).str()?.to_string_lossy().into_owned(), false)?;
                    if self.remaining < 528 {
                        return Ok(None);
                    }
                    print_to(
                        py,
                        format!(
                            "Failed to parse {name}/{msg_struct} with len {} (remaining {})",
                            body.len(),
                            self.remaining
                        ),
                        true,
                    )?;
                    continue;
                }
                Err(err) => return Err(err),
            };
            for a in a_indexes {
                let converted = array_h(py, elements[a].bind(py));
                match converted {
                    Ok(arr) => elements[a] = arr.unbind(),
                    Err(err) if err.is_instance_of::<PyException>(py) => {
                        print_to(py, format!("Failed to transform array: {}", err.value(py).str()?), true)?;
                    }
                    Err(err) => return Err(err),
                }
            }
            if name == "FMT" {
                let made = self.format_from_elements(py, &elements).and_then(|mfmt| {
                    self.formats.bind(py).set_item(at(&elements, 0)?.bind(py), mfmt)?;
                    Ok(())
                });
                match made {
                    Ok(()) => {}
                    Err(err) if err.is_instance_of::<PyException>(py) => continue,
                    Err(err) => return Err(err),
                }
            }
            self.offset += flen - 3;
            self.remaining = self.dlen() - self.offset;
            let m = self.new_message(py, &fmt, elements.iter().map(|e| e.clone_ref(py)).collect(), true)?;
            if name == "FMTU" {
                let fmt_type = py.get_type::<PyInt>().call1((elements
                    .first()
                    .ok_or_else(|| PyIndexError::new_err("list index out of range"))?,))?;
                let unit_ids = elements
                    .get(1)
                    .ok_or_else(|| PyIndexError::new_err("list index out of range"))?;
                let mult_ids = elements
                    .get(2)
                    .ok_or_else(|| PyIndexError::new_err("list index out of range"))?;
                if let Some(f2) = self.formats.bind(py).get_item(&fmt_type)? {
                    f2.call_method1("set_unit_ids", (unit_ids, self.unit_lookup.bind(py)))?;
                    f2.call_method1("set_mult_ids", (mult_ids, self.mult_lookup.bind(py)))?;
                }
            }
            match self.add_msg(py, m.bind(py)) {
                Ok(()) => {}
                Err(err) if err.is_instance_of::<PyException>(py) => {
                    print_to(
                        py,
                        format!("bad msg at offset {} {}", self.offset, err.value(py).str()?),
                        false,
                    )?;
                }
                Err(err) => return Err(err),
            }
            self.percent = PyFloat::new(py, 100.0 * (self.offset as f64 / self.dlen() as f64))
                .into_any()
                .unbind();
            return Ok(Some(m));
        }
    }

    fn parse_next_text(&mut self, py: Python<'_>) -> PyResult<Option<Py<DfMessage>>> {
        loop {
            let mut elements: Vec<String> = Vec::new();
            loop {
                let mut endline = py_find(&self.data, b"\n", self.offset);
                if endline == -1 {
                    endline = self.dlen();
                    if endline < self.offset {
                        break;
                    }
                }
                let raw = py_slice(&self.data, self.offset, endline);
                let end = raw.iter().rposition(|b| !is_py_space(*b)).map(|i| i + 1).unwrap_or(0);
                let line = PyBytes::new(py, &raw[..end])
                    .call_method0("decode")?
                    .extract::<String>()?;
                elements = line.split(self.delimiter.as_str()).map(String::from).collect();
                self.offset = endline + 1;
                if elements.len() >= 2 {
                    break;
                }
            }
            if self.offset > self.dlen() {
                return Ok(None);
            }
            if elements.len() == 5 && elements[4] == "," {
                elements[4] = String::new();
                elements.push(String::new());
            }
            self.percent = PyFloat::new(py, 100.0 * (self.offset as f64 / self.dlen() as f64))
                .into_any()
                .unbind();
            let msg_type = elements[0].clone();
            let Some(fmt) = self.format_by_name(py, &msg_type)? else {
                continue;
            };
            let (format_len, name) = {
                let f = fmt.bind(py).borrow();
                (f.format.chars().count(), f.name.trim_end_matches('\0').to_string())
            };
            if elements.len() < format_len + 1 {
                continue;
            }
            elements.remove(0);

            if name == "FMT" {
                let int = py.get_type::<PyInt>();
                let ftype = int.call1((at(&elements, 0)?,))?;
                let fname = elements
                    .get(2)
                    .cloned()
                    .ok_or_else(|| PyIndexError::new_err("list index out of range"))?;
                if self.delimiter == "," {
                    let joined = elements.get(4..).unwrap_or(&[]).join(",");
                    elements.truncate(4);
                    elements.push(joined);
                }
                let mut columns = elements
                    .get(4)
                    .cloned()
                    .ok_or_else(|| PyIndexError::new_err("list index out of range"))?;
                if fname == "FMT" && columns == "Type,Length,Name,Format" {
                    columns = "Type,Length,Name,Format,Columns".to_string();
                }
                let flen = int.call1((at(&elements, 1)?,))?;
                let new_fmt = Py::new(
                    py,
                    DfFormat::build(
                        py,
                        ftype.clone().unbind(),
                        &fname,
                        flen.unbind(),
                        at(&elements, 3)?,
                        &columns,
                        None,
                    )?,
                )?;
                self.formats.bind(py).set_item(&fname, new_fmt)?;
                self.id_to_name.bind(py).set_item(&ftype, &fname)?;
                self.name_to_id.bind(py).set_item(&fname, &ftype)?;
            }

            let objects: Vec<PyObject> = elements
                .iter()
                .map(|e| PyString::new(py, e).into_any().unbind())
                .collect();
            let m = self.new_message(py, &fmt, objects, false)?;
            let mb = m.bind(py);
            let kind = fmt.bind(py).borrow().name.clone();
            if kind == "FMTU" {
                let fmtid = try_field(py, mb, "FmtType")?;
                if let Some(fmtid) = fmtid {
                    if self.id_to_name.bind(py).contains(fmtid.bind(py))? {
                        let target = self.id_to_name.bind(py).get_item(fmtid.bind(py))?.expect("checked");
                        let fmtu = self
                            .formats
                            .bind(py)
                            .get_item(&target)?
                            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(target.unbind()))?;
                        let none = py.None();
                        let unit_ids = try_field(py, mb, "UnitIds")?.unwrap_or_else(|| none.clone_ref(py));
                        let mult_ids = try_field(py, mb, "MultIds")?.unwrap_or_else(|| none.clone_ref(py));
                        fmtu.call_method1("set_unit_ids", (unit_ids, self.unit_lookup.bind(py)))?;
                        fmtu.call_method1("set_mult_ids", (mult_ids, self.mult_lookup.bind(py)))?;
                    }
                }
            }
            if kind == "UNIT" {
                let unitid = try_field(py, mb, "Id")?.unwrap_or_else(|| py.None());
                let label = try_field(py, mb, "Label")?.unwrap_or_else(|| py.None());
                let value = null_term_obj(label.bind(py))?;
                let ch = py.import("builtins")?.getattr("chr")?.call1((unitid,))?;
                self.unit_lookup.bind(py).set_item(ch, value)?;
            }
            if kind == "MULT" {
                let multid = try_field(py, mb, "Id")?.unwrap_or_else(|| py.None());
                let mult = try_field(py, mb, "Mult")?.unwrap_or_else(|| py.None());
                let rounded = py.get_type::<PyFloat>().call1((format_g(py, 7, mult.bind(py))?,))?;
                let ch = py.import("builtins")?.getattr("chr")?.call1((multid,))?;
                self.mult_lookup.bind(py).set_item(ch, rounded)?;
            }
            self.add_msg(py, mb)?;
            return Ok(Some(m));
        }
    }

    fn init_arrays_binary(&mut self, py: Python<'_>, progress: &PyObject) -> PyResult<()> {
        use std::collections::HashSet;
        self.count = 0;
        let mut offsets: Vec<Vec<usize>> = vec![Vec::new(); 256];
        let mut counts: Vec<i64> = vec![0; 256];
        let mut type_instances: HashMap<usize, HashSet<Vec<u8>>> = HashMap::new();
        let fmt_type: i64 = 0x80;
        let (mut fmtu_type, mut unit_type, mut mult_type): (Option<i64>, Option<i64>, Option<i64>) = (None, None, None);
        let mut ofs: i64 = 0;
        let mut pct: i64 = 0;
        let mut lengths: Vec<i64> = vec![-1; 256];
        let data_len = self.dlen();
        let noisy = |ofs: i64| data_len - ofs >= 528 || data_len < 528;
        let int_type = py.get_type::<PyInt>();

        while ofs + 3 < data_len {
            let o = ofs as usize;
            let (h1, h2, mtype) = (self.data[o], self.data[o + 1], self.data[o + 2] as usize);
            if h1 != 0xA3 || h2 != 0x95 {
                if noisy(ofs) {
                    print_to(py, format!("bad header 0x{h1:02x} 0x{h2:02x} at {ofs}"), true)?;
                }
                ofs += 1;
                continue;
            }
            offsets[mtype].push(o);

            if lengths[mtype] == -1 {
                if self.format_by_id(py, mtype as i64)?.is_none() {
                    if noisy(ofs) {
                        print_to(py, format!("unknown msg type 0x{mtype:02x} ({mtype}) at {ofs}"), true)?;
                    }
                    break;
                }
                self.offset = ofs;
                self.parse_next(py)?;
                let fmt = self.format_by_id(py, mtype as i64)?.expect("known");
                lengths[mtype] = Self::flen(py, &fmt)?;
            } else if let Some(fmt) = self.format_by_id(py, mtype as i64)? {
                let (has_instance, iofs, ilen) = {
                    let f = fmt.bind(py).borrow();
                    (
                        f.instance_field.is_some(),
                        f.instance_ofs.unwrap_or(0),
                        f.instance_len.unwrap_or(0),
                    )
                };
                if has_instance {
                    let idata = py_slice(&self.data, ofs + 3 + iofs, ofs + 3 + iofs + ilen).to_vec();
                    let seen = type_instances.entry(mtype).or_default();
                    if seen.insert(idata) {
                        self.offset = ofs;
                        self.parse_next(py)?;
                    }
                }
            } else {
                return Err(pyo3::exceptions::PyKeyError::new_err(mtype as i64));
            }

            counts[mtype] += 1;
            let mlen = lengths[mtype];
            // A format of no length would be indexed forever (pymavlink never
            // returns from it); a damaged log ends the index here instead.
            if mlen <= 0 {
                break;
            }

            let read_body = |this: &Self, fmt: &Py<DfFormat>| -> PyResult<Option<Vec<PyObject>>> {
                let body = py_slice(&this.data, ofs + 3, ofs + mlen);
                if (body.len() as i64) + 3 < mlen {
                    return Ok(None);
                }
                let msg_struct = fmt.bind(py).borrow().msg_struct.clone();
                Ok(Some(unpack_elements(py, &msg_struct, body)?))
            };

            if mtype as i64 == fmt_type {
                let fmt = self.format_by_id(py, fmt_type)?.expect("FMT is always known");
                let Some(elements) = read_body(self, &fmt)? else { break };
                let mfmt = self.format_from_elements(py, &elements)?;
                let (mname, mid) = {
                    let f = mfmt.bind(py).borrow();
                    (f.name.clone(), f.ftype.clone_ref(py))
                };
                self.formats.bind(py).set_item(at(&elements, 0)?.bind(py), &mfmt)?;
                self.name_to_id.bind(py).set_item(&mname, mid.bind(py))?;
                self.id_to_name.bind(py).set_item(mid.bind(py), &mname)?;
                let id: i64 = mid.bind(py).extract()?;
                match mname.as_str() {
                    "FMTU" => fmtu_type = Some(id),
                    "UNIT" => unit_type = Some(id),
                    "MULT" => mult_type = Some(id),
                    _ => {}
                }
            }
            if Some(mtype as i64) == fmtu_type {
                let fmt = self.format_by_id(py, mtype as i64)?.expect("known");
                let Some(elements) = read_body(self, &fmt)? else { break };
                let ftype = int_type.call1((elements
                    .get(1)
                    .ok_or_else(|| PyIndexError::new_err("list index out of range"))?,))?;
                if let Some(fmt2) = self.formats.bind(py).get_item(&ftype)? {
                    let colhash = fmt.bind(py).borrow().colhash.clone();
                    if let Some(i) = colhash.get("UnitIds") {
                        fmt2.call_method1(
                            "set_unit_ids",
                            (null_term_obj(at(&elements, *i)?.bind(py))?, self.unit_lookup.bind(py)),
                        )?;
                    }
                    if let Some(i) = colhash.get("MultIds") {
                        fmt2.call_method1(
                            "set_mult_ids",
                            (null_term_obj(at(&elements, *i)?.bind(py))?, self.mult_lookup.bind(py)),
                        )?;
                    }
                }
            }
            if Some(mtype as i64) == unit_type {
                let fmt = self.format_by_id(py, mtype as i64)?.expect("known");
                let Some(elements) = read_body(self, &fmt)? else { break };
                let value = null_term_obj(at(&elements, 2)?.bind(py))?;
                let ch = py.import("builtins")?.getattr("chr")?.call1((at(&elements, 1)?,))?;
                self.unit_lookup.bind(py).set_item(ch, value)?;
            }
            if Some(mtype as i64) == mult_type {
                let fmt = self.format_by_id(py, mtype as i64)?.expect("known");
                let Some(elements) = read_body(self, &fmt)? else { break };
                let mult = py
                    .get_type::<PyFloat>()
                    .call1((format_g(py, 7, at(&elements, 2)?.bind(py))?,))?;
                let ch = py.import("builtins")?.getattr("chr")?.call1((elements
                    .get(1)
                    .ok_or_else(|| PyIndexError::new_err("list index out of range"))?,))?;
                self.mult_lookup.bind(py).set_item(ch, mult)?;
            }

            ofs += mlen;
            if !progress.bind(py).is_none() {
                let new_pct = (100 * ofs) / data_len;
                if new_pct != pct {
                    progress.call1(py, (new_pct,))?;
                    pct = new_pct;
                }
            }
        }
        self.offsets = offsets;
        self.counts = counts;
        self.count = self.counts.iter().sum();
        self.offset = 0;
        Ok(())
    }

    fn init_arrays_text(&mut self, py: Python<'_>, progress: &PyObject) -> PyResult<()> {
        self.t_offsets = IndexMap::new();
        self.t_counts = IndexMap::new();
        self.count = 0;
        let mut ofs = self.offset;
        let mut pct = 0;
        let data_len = self.dlen();
        while ofs + 16 < data_len {
            let slice = py_slice(&self.data, ofs, ofs + 4).to_vec();
            let decoded: String = PyBytes::new(py, &slice).call_method0("decode")?.extract()?;
            let mtype = decoded.split(',').next().unwrap_or("").to_string();
            if !self.t_offsets.contains_key(&mtype) {
                self.t_counts.insert(mtype.clone(), 0);
                self.t_offsets.insert(mtype.clone(), Vec::new());
                self.offset = ofs;
                self.parse_next(py)?;
            }
            self.t_offsets.get_mut(&mtype).expect("inserted").push(ofs);
            *self.t_counts.get_mut(&mtype).expect("inserted") += 1;
            if mtype == "FMT" || mtype == "FMTU" {
                self.offset = ofs;
                self.parse_next(py)?;
            }
            ofs = py_find(&self.data, b"\n", ofs);
            if ofs == -1 {
                break;
            }
            ofs += 1;
            let new_pct = (100 * ofs) / data_len;
            if !progress.bind(py).is_none() && new_pct != pct {
                progress.call1(py, (new_pct,))?;
                pct = new_pct;
            }
        }
        self.count = self.t_counts.values().sum();
        self.offset = 0;
        Ok(())
    }
}

fn types_arg(type_: Option<&Bound<'_, PyAny>>) -> PyResult<Option<Vec<String>>> {
    let Some(t) = type_ else { return Ok(None) };
    if t.is_none() {
        return Ok(None);
    }
    if let Ok(s) = t.downcast::<PyString>() {
        return Ok(Some(vec![s.to_string_lossy().into_owned()]));
    }
    let mut out: Vec<String> = Vec::new();
    for item in t.try_iter()? {
        let name: String = item?.extract()?;
        if !out.contains(&name) {
            out.push(name);
        }
    }
    Ok(Some(out))
}

fn not_evaluated() -> PyErr {
    PyNotImplementedError::new_err("corvus_link does not evaluate recv_match conditions")
}

#[pymethods]
impl DfReader {
    #[getter]
    fn timestamp(&self, py: Python<'_>) -> PyObject {
        int_obj(py, 0)
    }

    #[getter]
    fn mav_type(&self) -> i64 {
        self.mav_type
    }

    #[setter]
    fn set_mav_type(&mut self, v: i64) {
        self.mav_type = v;
    }

    #[getter]
    fn verbose(&self) -> bool {
        self.verbose
    }

    #[setter]
    fn set_verbose(&mut self, v: bool) {
        self.verbose = v;
    }

    #[getter]
    fn params(&self, py: Python<'_>) -> Py<PyDict> {
        self.params.clone_ref(py)
    }

    #[getter]
    fn param_defaults(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        self.param_defaults.as_ref().map(|d| d.clone_ref(py)).ok_or_else(|| {
            PyAttributeError::new_err(format!(
                "'{}' object has no attribute 'param_defaults'",
                if self.binary {
                    "DFReader_binary"
                } else {
                    "DFReader_text"
                }
            ))
        })
    }

    #[getter]
    fn _flightmodes(&self, py: Python<'_>) -> PyObject {
        match &self.flightmodes {
            Some(l) => l.clone_ref(py).into_any(),
            None => py.None(),
        }
    }

    #[getter]
    fn messages(&self, py: Python<'_>) -> Py<PyDict> {
        self.messages.clone_ref(py)
    }

    #[getter]
    fn percent(&self, py: Python<'_>) -> PyObject {
        self.percent.clone_ref(py)
    }

    #[getter]
    fn unit_lookup(&self, py: Python<'_>) -> Py<PyDict> {
        self.unit_lookup.clone_ref(py)
    }

    #[getter]
    fn mult_lookup(&self, py: Python<'_>) -> Py<PyDict> {
        self.mult_lookup.clone_ref(py)
    }

    #[getter]
    fn formats(&self, py: Python<'_>) -> Py<PyDict> {
        self.formats.clone_ref(py)
    }

    #[getter]
    fn name_to_id(&self, py: Python<'_>) -> Py<PyDict> {
        self.name_to_id.clone_ref(py)
    }

    #[getter]
    fn id_to_name(&self, py: Python<'_>) -> Py<PyDict> {
        self.id_to_name.clone_ref(py)
    }

    #[getter]
    fn flightmode(&self, py: Python<'_>) -> PyObject {
        self.flightmode.clone_ref(py)
    }

    #[getter]
    fn offset(&self) -> i64 {
        self.offset
    }

    #[setter]
    fn set_offset(&mut self, v: i64) {
        self.offset = v;
    }

    #[getter]
    fn remaining(&self) -> i64 {
        self.remaining
    }

    #[getter]
    fn data_len(&self) -> i64 {
        self.data.len() as i64
    }

    #[getter]
    fn _count(&self) -> i64 {
        self.count
    }

    #[getter]
    fn delimiter(&self) -> String {
        self.delimiter.clone()
    }

    #[getter]
    fn counts(&self, py: Python<'_>) -> PyResult<PyObject> {
        if self.binary {
            Ok(PyList::new(py, &self.counts)?.into_any().unbind())
        } else {
            let d = PyDict::new(py);
            for (k, v) in &self.t_counts {
                d.set_item(k, v)?;
            }
            Ok(d.into_any().unbind())
        }
    }

    #[getter]
    fn offsets(&self, py: Python<'_>) -> PyResult<PyObject> {
        if self.binary {
            let out = PyList::empty(py);
            for o in &self.offsets {
                out.append(PyList::new(py, o)?)?;
            }
            Ok(out.into_any().unbind())
        } else {
            let d = PyDict::new(py);
            for (k, v) in &self.t_offsets {
                d.set_item(k, PyList::new(py, v)?)?;
            }
            Ok(d.into_any().unbind())
        }
    }

    #[getter]
    fn type_nums(&self, py: Python<'_>) -> PyResult<PyObject> {
        match &self.type_nums {
            Some(n) => Ok(PyList::new(py, n)?.into_any().unbind()),
            None => Ok(py.None()),
        }
    }

    fn rewind(&mut self, py: Python<'_>) -> PyResult<()> {
        self.rewind_inner(py, false)
    }

    #[pyo3(signature = (keep_messages = false))]
    fn _rewind(&mut self, py: Python<'_>, keep_messages: bool) -> PyResult<()> {
        self.rewind_inner(py, keep_messages)
    }

    fn recv_msg(&mut self, py: Python<'_>) -> PyResult<Option<Py<DfMessage>>> {
        self.parse_next(py)
    }

    fn _parse_next(&mut self, py: Python<'_>) -> PyResult<Option<Py<DfMessage>>> {
        self.parse_next(py)
    }

    #[pyo3(signature = (condition = None, r#type = None, blocking = false, strict = false))]
    fn recv_match(
        &mut self,
        py: Python<'_>,
        condition: Option<&Bound<'_, PyAny>>,
        r#type: Option<&Bound<'_, PyAny>>,
        blocking: bool,
        strict: bool,
    ) -> PyResult<Option<Py<DfMessage>>> {
        let _ = blocking;
        let types = types_arg(r#type)?;
        let Some(m) = self.recv_match_inner(py, types.as_deref(), strict)? else {
            return Ok(None);
        };
        if condition.is_some_and(|c| !c.is_none()) {
            return Err(not_evaluated());
        }
        Ok(Some(m))
    }

    fn check_condition(&self, condition: Option<&Bound<'_, PyAny>>) -> PyResult<bool> {
        match condition {
            None => Ok(true),
            Some(c) if c.is_none() => Ok(true),
            Some(_) => Err(not_evaluated()),
        }
    }

    #[pyo3(signature = (name, default = None))]
    fn param(&self, py: Python<'_>, name: &Bound<'_, PyAny>, default: Option<PyObject>) -> PyResult<PyObject> {
        let params = self.params.bind(py);
        if !params.contains(name)? {
            return Ok(default.unwrap_or_else(|| py.None()));
        }
        Ok(params.get_item(name)?.expect("checked").unbind())
    }

    fn flightmode_list(&mut self, py: Python<'_>) -> PyResult<PyObject> {
        let mut tstamp: Option<PyObject> = None;
        let mut fmode: Option<PyObject> = None;
        if self.flightmodes.is_none() {
            self.rewind_inner(py, false)?;
            let list = PyList::empty(py);
            self.flightmodes = Some(list.clone().unbind());
            let wanted = ["MODE".to_string(), "PARM".to_string()];
            loop {
                let Some(m) = self.recv_match_inner(py, Some(&wanted), true)? else {
                    break;
                };
                let ts = m.bind(py).getattr("_timestamp")?;
                tstamp = Some(ts.clone().unbind());
                let same = match &fmode {
                    Some(f) => self.flightmode.bind(py).eq(f.bind(py))?,
                    None => false,
                };
                if same {
                    continue;
                }
                if !list.is_empty() {
                    let last = list.get_item(list.len() - 1)?;
                    list.set_item(list.len() - 1, (last.get_item(0)?, last.get_item(1)?, &ts))?;
                }
                list.append((self.flightmode.bind(py), &ts, py.None()))?;
                fmode = Some(self.flightmode.clone_ref(py));
            }
            if tstamp.is_some() {
                let last = list.get_item(list.len() - 1)?;
                let end = self.last_timestamp_inner(py)?;
                list.set_item(list.len() - 1, (last.get_item(0)?, last.get_item(1)?, end))?;
            }
        }
        self.rewind_inner(py, false)?;
        Ok(self.flightmodes.as_ref().expect("set above").clone_ref(py).into_any())
    }

    fn last_timestamp(&mut self, py: Python<'_>) -> PyResult<PyObject> {
        self.last_timestamp_inner(py)
    }

    #[pyo3(signature = (r#type, strict = false))]
    fn skip_to_type(&mut self, py: Python<'_>, r#type: &Bound<'_, PyAny>, strict: bool) -> PyResult<()> {
        let types = types_arg(Some(r#type))?.unwrap_or_default();
        self.skip_types(py, &types, strict)
    }

    fn close(&mut self) {
        self.closed = true;
        self.data = Vec::new();
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __exit__(&mut self, _a: &Bound<'_, PyAny>, _b: &Bound<'_, PyAny>, _c: &Bound<'_, PyAny>) {
        self.close();
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        if let Some(me) = &self.me {
            visit.call(me)?;
        }
        visit.call(&self.params)?;
        if let Some(d) = &self.param_defaults {
            visit.call(d)?;
        }
        if let Some(l) = &self.flightmodes {
            visit.call(l)?;
        }
        visit.call(&self.messages)?;
        visit.call(&self.percent)?;
        visit.call(&self.unit_lookup)?;
        visit.call(&self.mult_lookup)?;
        visit.call(&self.formats)?;
        visit.call(&self.name_to_id)?;
        visit.call(&self.id_to_name)?;
        visit.call(&self.flightmode)
    }

    fn __clear__(&mut self, py: Python<'_>) {
        self.me = None;
        self.messages = PyDict::new(py).unbind();
        self.formats = PyDict::new(py).unbind();
    }
}

#[pymethods]
impl DfReaderBinary {
    fn find_unused_format(slf: PyRef<'_, Self>, py: Python<'_>) -> PyResult<Option<i64>> {
        let base = slf.as_super();
        for i in (2..=254).rev() {
            if !base.formats.bind(py).contains(i)? {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    fn add_format(slf: PyRef<'_, Self>, py: Python<'_>, fmt: &Bound<'_, DfFormat>) -> PyResult<Option<PyObject>> {
        let base = slf.as_super();
        for i in (2..=254).rev() {
            if !base.formats.bind(py).contains(i)? {
                fmt.borrow_mut().ftype = int_obj(py, i);
                base.formats.bind(py).set_item(i, fmt)?;
                return Ok(Some(fmt.clone().into_any().unbind()));
            }
        }
        Ok(None)
    }

    fn make_msgbuf<'py>(
        slf: PyRef<'_, Self>,
        py: Python<'py>,
        fmt: &Bound<'py, DfFormat>,
        values: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let _ = slf;
        let struct_mod = py.import("struct")?;
        let head = struct_mod.call_method1("pack", ("BBB", 0xA3, 0x95, fmt.borrow().ftype.bind(py)))?;
        let mut args: Vec<Bound<'py, PyAny>> = vec![PyString::new(py, &fmt.borrow().msg_struct).into_any()];
        for v in values.try_iter()? {
            args.push(v?);
        }
        head.add(struct_mod.call_method1("pack", PyTuple::new(py, args)?)?)
    }

    fn make_format_msgbuf<'py>(
        slf: PyRef<'_, Self>,
        py: Python<'py>,
        fmt: &Bound<'py, DfFormat>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let base = slf.as_super();
        let fmt_fmt = base
            .formats
            .bind(py)
            .get_item(0x80)?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(0x80))?;
        let fmt_fmt = fmt_fmt.downcast::<DfFormat>()?.borrow();
        let struct_mod = py.import("struct")?;
        let head = struct_mod.call_method1("pack", ("BBB", 0xA3, 0x95, 0x80))?;
        let f = fmt.borrow();
        let size = struct_mod
            .call_method1("calcsize", (&f.msg_struct,))?
            .extract::<i64>()?
            + 3;
        let columns = f.cols.join(",");
        let args = PyTuple::new(
            py,
            [
                PyString::new(py, &fmt_fmt.msg_struct).into_any(),
                f.ftype.bind(py).clone(),
                size.into_pyobject(py)?.into_any(),
                PyBytes::new(py, f.name.as_bytes()).into_any(),
                PyBytes::new(py, f.format.as_bytes()).into_any(),
                PyBytes::new(py, columns.as_bytes()).into_any(),
            ],
        )?;
        head.add(struct_mod.call_method1("pack", args)?)
    }
}

// ------------------------------------------------------------ constructors ----

fn read_log(py: Python<'_>, filename: &str, text: bool) -> PyResult<Vec<u8>> {
    let builtins = py.import("builtins")?;
    let file = builtins.call_method1("open", (filename, if text { "r" } else { "rb" }))?;
    let result = (|| -> PyResult<Vec<u8>> {
        if text {
            file.call_method1("seek", (0, 2))?;
            let len: usize = file.call_method0("tell")?.extract()?;
            file.call_method0("close")?;
            let raw = builtins.call_method1("open", (filename, "rb"))?;
            let bytes: Vec<u8> = raw.call_method0("read")?.extract()?;
            raw.call_method0("close")?;
            let _ = len;
            Ok(bytes)
        } else {
            Ok(file.call_method0("read")?.extract()?)
        }
    })();
    let _ = file.call_method0("close");
    let data = result?;
    if data.is_empty() {
        return Err(PyValueError::new_err("cannot mmap an empty file"));
    }
    Ok(data)
}

/// `DFReader_binary(filename, zero_time_base, progress_callback)`.
pub fn open_binary(
    py: Python<'_>,
    filename: &str,
    zero_time_base: bool,
    progress_callback: Option<PyObject>,
) -> PyResult<Py<DfReaderBinary>> {
    let data = read_log(py, filename, false)?;
    let reader = DfReader::new(py, true, data, zero_time_base)?;
    let object = Py::new(py, PyClassInitializer::from(reader).add_subclass(DfReaderBinary))?;
    {
        let mut base = object.bind(py).as_super().borrow_mut();
        base.me = Some(object.clone_ref(py).into_any());
        base.reset_messages(py)?;
        let progress = progress_callback.unwrap_or_else(|| py.None());
        base.init_arrays_binary(py, &progress)?;
        base.init_clock(py)?;
        base.prev_type = None;
        base.rewind_inner(py, true)?;
    }
    Ok(object)
}

/// `DFReader_text(filename, zero_time_base, progress_callback)`.
pub fn open_text(
    py: Python<'_>,
    filename: &str,
    zero_time_base: bool,
    progress_callback: Option<PyObject>,
) -> PyResult<Py<DfReaderText>> {
    let data = read_log(py, filename, true)?;
    let reader = DfReader::new(py, false, data, zero_time_base)?;
    let object = Py::new(py, PyClassInitializer::from(reader).add_subclass(DfReaderText))?;
    {
        let mut base = object.bind(py).as_super().borrow_mut();
        base.me = Some(object.clone_ref(py).into_any());
        base.reset_messages(py)?;
        base.rewind_inner(py, false)?;
        let progress = progress_callback.unwrap_or_else(|| py.None());
        base.init_arrays_text(py, &progress)?;
        base.init_clock(py)?;
        base.rewind_inner(py, true)?;
    }
    Ok(object)
}

/// True when a file's first 8000 characters hold `FMT,`.
#[pyfunction]
#[pyo3(name = "DFReader_is_text_log")]
pub fn is_text_log(py: Python<'_>, filename: &str) -> PyResult<bool> {
    let file = py.import("builtins")?.call_method1("open", (filename, "r"))?;
    let found = file
        .call_method1("read", (8000,))
        .and_then(|t| t.call_method1("find", ("FMT,",)))
        .and_then(|i| i.extract::<i64>());
    let _ = file.call_method0("close");
    Ok(found? != -1)
}

pub fn register(m: &Bound<'_, pyo3::types::PyModule>) -> PyResult<()> {
    m.add_class::<DfFormat>()?;
    m.add_class::<DfMessage>()?;
    m.add_class::<DfReader>()?;
    m.add_class::<DfReaderBinary>()?;
    m.add_class::<DfReaderText>()?;
    m.add_function(pyo3::wrap_pyfunction!(is_text_log, m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_clamp_and_count_from_the_end_like_python() {
        let data = b"abcdefgh";
        assert_eq!(py_slice(data, 2, 5), b"cde");
        assert_eq!(py_slice(data, -3, 100), b"fgh");
        assert_eq!(py_slice(data, 5, 2), b"");
        assert_eq!(py_slice(data, -100, 2), b"ab");
        assert_eq!(py_slice(data, -1, 3), b"");
        assert_eq!(py_slice(b"", 0, 4), b"");
    }

    #[test]
    fn find_starts_where_asked_and_counts_a_negative_start_from_the_end() {
        let data = b"FMT, a\nFMT,b\n";
        assert_eq!(py_find(data, b"FMT, ", 0), 0);
        assert_eq!(py_find(data, b"FMT,", 1), 7);
        assert_eq!(py_find(data, b"\n", -4), 12);
        assert_eq!(py_find(data, b"zz", 0), -1);
        assert_eq!(py_find(data, b"a", 100), -1);
        assert_eq!(py_find(data, b"", 3), 3);
    }

    #[test]
    fn an_unset_timestamp_stays_the_int_zero_until_a_float_touches_it() {
        assert!(Num::Zero.add(Num::Zero) == Num::Zero);
        assert!(Num::Zero.add_f(1.5) == Num::F(1.5));
        assert!(Num::F(2.0).add(Num::Zero) == Num::F(2.0));
        assert_eq!(Num::Zero.f(), 0.0);
    }

    #[test]
    fn names_end_at_the_first_nul() {
        assert_eq!(null_term("GPS\0\0\0"), "GPS");
        assert_eq!(null_term("no nul"), "no nul");
        assert_eq!(null_term("\0x"), "");
    }

    #[test]
    fn unit_multipliers_have_their_metric_prefix() {
        assert_eq!(prefix_for(1.0), Some(""));
        assert_eq!(prefix_for(0.0), Some(""));
        assert_eq!(prefix_for(1.0e-3), Some("m"));
        assert_eq!(prefix_for(1.0e-6), Some("\u{b5}"));
        assert_eq!(prefix_for(3.5), None);
    }

    #[test]
    fn every_format_character_has_a_struct_code_and_the_scaled_ones_a_multiplier() {
        for c in "abBhHiIfdnNZcCeELMqQg".chars() {
            assert!(char_info(c).is_some(), "{c}");
        }
        assert!(char_info('?').is_none());
        assert_eq!(char_info('L').and_then(|i| i.mult), Some(1.0e-7));
        assert_eq!(char_info('c').and_then(|i| i.mult), Some(0.01));
        assert_eq!(char_info('f').and_then(|i| i.mult), None);
    }
}
