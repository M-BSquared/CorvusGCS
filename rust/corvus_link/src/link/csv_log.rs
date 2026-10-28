//! `csv:` links: pymavlink's `CSVReader`, a CSV file read as a stream of
//! messages. The first row names the columns; every later row is one
//! message of type `CSV` (or the `message_type` option), its fields numbers
//! where they parse as numbers and text where they do not. The first column
//! is the timestamp in microseconds.
//!
//! Rows are cut exactly as Python's `csv.reader` cuts them with its default
//! dialect: quotes, doubled quotes, a quoted field that spans lines, blank
//! lines as empty rows, a field limit of 131072 characters, and the end of
//! the file inside a quote closing the field. The file is read through
//! Python's `open`, so its encoding and newline handling are Python's.
//!
//! `timestamp_expression` and `recv_match` conditions need pymavlink's
//! expression evaluator, which corvus_link does not have: they raise
//! `NotImplementedError`.

use pyo3::exceptions::{PyIndexError, PyKeyError, PyNotImplementedError, PyStopIteration, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyFloat, PyInt, PyList, PySet, PyString};
use pyo3::{PyTraverseError, PyVisit};

const FIELD_LIMIT: usize = 131072;

fn not_evaluated(what: &str) -> PyErr {
    PyNotImplementedError::new_err(format!("corvus_link does not evaluate {what}"))
}

// ------------------------------------------------------------ row parser ----

#[derive(Clone, Copy, PartialEq)]
enum State {
    StartRecord,
    StartField,
    InField,
    InQuoted,
    QuoteInQuoted,
    EatCrnl,
}

#[derive(Clone, Copy)]
enum Ch {
    Char(char),
    End,
}

struct Rows {
    text: String,
    pos: usize,
    delimiter: char,
}

struct Record {
    state: State,
    fields: Vec<String>,
    field: String,
    len: usize,
}

impl Record {
    fn save(&mut self) {
        self.fields.push(std::mem::take(&mut self.field));
        self.len = 0;
    }

    fn add(&mut self, py: Python<'_>, c: char) -> PyResult<()> {
        if self.len >= FIELD_LIMIT {
            return Err(csv_error(py, format!("field larger than field limit ({FIELD_LIMIT})")));
        }
        self.field.push(c);
        self.len += 1;
        Ok(())
    }
}

fn csv_error(py: Python<'_>, text: String) -> PyErr {
    match py.import("csv").and_then(|m| m.getattr("Error")).and_then(|e| e.call1((text,))) {
        Ok(err) => PyErr::from_value(err),
        Err(err) => err,
    }
}

impl Rows {
    /// One row, or None at the end of the file: `next(csv.reader(...))`.
    fn next_row(&mut self, py: Python<'_>) -> PyResult<Option<Vec<String>>> {
        let delimiter = self.delimiter;
        let mut r = Record { state: State::StartRecord, fields: Vec::new(), field: String::new(), len: 0 };
        loop {
            if self.pos >= self.text.len() {
                if r.len != 0 || r.state == State::InQuoted {
                    r.save();
                    return Ok(Some(r.fields));
                }
                return Ok(None);
            }
            let rest = &self.text[self.pos..];
            let end = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
            let line = &rest[..end];
            self.pos += end;
            for c in line.chars() {
                Self::process(py, &mut r, Ch::Char(c), delimiter)?;
            }
            Self::process(py, &mut r, Ch::End, delimiter)?;
            if r.state == State::StartRecord {
                return Ok(Some(r.fields));
            }
        }
    }

    fn process(py: Python<'_>, r: &mut Record, c: Ch, delimiter: char) -> PyResult<()> {
        let newline = matches!(c, Ch::Char('\n') | Ch::Char('\r'));
        let after_line = if matches!(c, Ch::End) { State::StartRecord } else { State::EatCrnl };
        match r.state {
            State::StartRecord => {
                if matches!(c, Ch::End) {
                    return Ok(());
                }
                if newline {
                    r.state = State::EatCrnl;
                    return Ok(());
                }
                r.state = State::StartField;
                Self::start_field(py, r, c, delimiter, newline, after_line)
            }
            State::StartField => Self::start_field(py, r, c, delimiter, newline, after_line),
            State::InField => {
                if newline || matches!(c, Ch::End) {
                    r.save();
                    r.state = after_line;
                } else if let Ch::Char(ch) = c {
                    if ch == delimiter {
                        r.save();
                        r.state = State::StartField;
                    } else {
                        r.add(py, ch)?;
                    }
                }
                Ok(())
            }
            State::InQuoted => {
                if let Ch::Char(ch) = c {
                    if ch == '"' {
                        r.state = State::QuoteInQuoted;
                    } else {
                        r.add(py, ch)?;
                    }
                }
                Ok(())
            }
            State::QuoteInQuoted => {
                match c {
                    Ch::Char('"') => {
                        r.add(py, '"')?;
                        r.state = State::InQuoted;
                    }
                    Ch::Char(ch) if ch == delimiter => {
                        r.save();
                        r.state = State::StartField;
                    }
                    _ if newline || matches!(c, Ch::End) => {
                        r.save();
                        r.state = after_line;
                    }
                    Ch::Char(ch) => {
                        r.add(py, ch)?;
                        r.state = State::InField;
                    }
                    Ch::End => {}
                }
                Ok(())
            }
            State::EatCrnl => {
                if newline {
                    Ok(())
                } else if matches!(c, Ch::End) {
                    r.state = State::StartRecord;
                    Ok(())
                } else {
                    Err(csv_error(
                        py,
                        "new-line character seen in unquoted field - do you need to open the file with newline=''?".into(),
                    ))
                }
            }
        }
    }

    fn start_field(py: Python<'_>, r: &mut Record, c: Ch, delimiter: char, newline: bool, after_line: State) -> PyResult<()> {
        if newline || matches!(c, Ch::End) {
            r.save();
            r.state = after_line;
        } else if let Ch::Char(ch) = c {
            if ch == '"' {
                r.state = State::InQuoted;
            } else if ch == delimiter {
                r.save();
            } else {
                r.add(py, ch)?;
                r.state = State::InField;
            }
        }
        Ok(())
    }
}

// -------------------------------------------------------- the Python side ----

/// The columns of the file and where each one is.
#[pyclass(name = "CSVFormat", module = "corvus_link.mavutil", frozen)]
pub struct CsvFormat {
    #[pyo3(get)]
    headings: Py<PyList>,
    #[pyo3(get)]
    messages: Py<PyDict>,
    #[pyo3(get)]
    timestamp_expression: PyObject,
    #[pyo3(get)]
    field_offset: Py<PyDict>,
}

#[pymethods]
impl CsvFormat {
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.headings)?;
        visit.call(&self.messages)?;
        visit.call(&self.timestamp_expression)?;
        visit.call(&self.field_offset)
    }
}

/// One row of the file.
#[pyclass(name = "CSVMessage", module = "corvus_link.mavutil")]
pub struct CsvMessage {
    #[pyo3(get)]
    message_type: PyObject,
    #[pyo3(get)]
    fmt: Py<CsvFormat>,
    line: Py<PyList>,
}

#[pymethods]
impl CsvMessage {
    #[getter]
    fn line(&self, py: Python<'_>) -> Py<PyList> {
        self.line.clone_ref(py)
    }

    fn get_type(&self, py: Python<'_>) -> PyObject {
        self.message_type.clone_ref(py)
    }

    fn get_fieldnames(&self, py: Python<'_>) -> Py<PyList> {
        self.fmt.get().headings.clone_ref(py)
    }

    fn __str__(&self, py: Python<'_>) -> PyResult<String> {
        let mut out = format!("{} {{", self.message_type.bind(py).str()?);
        let headings = self.fmt.get().headings.bind(py);
        for (c, val) in headings.iter().zip(self.line.bind(py).iter()) {
            out.push_str(&format!("{} : {}, ", c.str()?, val.str()?));
        }
        out.push('}');
        Ok(out)
    }

    fn __getattr__(&self, py: Python<'_>, field: &str) -> PyResult<PyObject> {
        let fmt = self.fmt.get();
        let line = self.line.bind(py);
        if field == "_timestamp" {
            if !fmt.timestamp_expression.bind(py).is_none() {
                return Err(not_evaluated("timestamp expressions"));
            }
            let first = line.get_item(0).map_err(|_| PyIndexError::new_err("list index out of range"))?;
            return Ok(py.get_type::<PyInt>().call1((first,))?.unbind());
        }
        let index = fmt.field_offset.bind(py).get_item(field)?.ok_or_else(|| PyKeyError::new_err(field.to_string()))?;
        let index: isize = index.extract()?;
        line.get_item(index as usize).map(|v| v.unbind()).map_err(|_| PyIndexError::new_err("list index out of range"))
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.message_type)?;
        visit.call(&self.fmt)?;
        visit.call(&self.line)
    }

    fn __clear__(&mut self, py: Python<'_>) {
        self.line = PyList::empty(py).unbind();
    }
}

/// A CSV file read as messages.
#[pyclass(name = "CSVReader", module = "corvus_link.mavutil")]
pub struct CsvReader {
    #[pyo3(get)]
    filename: PyObject,
    #[pyo3(get)]
    separator: String,
    #[pyo3(get)]
    message_type: PyObject,
    #[pyo3(get)]
    progress_callback: PyObject,
    #[pyo3(get)]
    timestamp_expression: PyObject,
    #[pyo3(get, set)]
    timestamp: PyObject,
    #[pyo3(get, set)]
    verbose: bool,
    #[pyo3(get, set)]
    linecount: Option<i64>,
    #[pyo3(get, set)]
    params: Py<PyDict>,
    #[pyo3(get, set)]
    percent: i64,
    #[pyo3(get)]
    messages: Py<PyDict>,
    #[pyo3(get, set)]
    _flightmodes: Py<PyList>,
    #[pyo3(get)]
    fmt: Option<Py<CsvFormat>>,
    rows: Rows,
}

impl CsvReader {
    fn rewind_inner(&mut self, py: Python<'_>) -> PyResult<()> {
        self.percent = 0;
        let builtins = py.import("builtins")?;
        let file = builtins.call_method1("open", (self.filename.bind(py),))?;
        let text = file.call_method0("read");
        let _ = file.call_method0("close");
        let text: String = text?.extract()?;
        let mut chars = self.separator.chars();
        let delimiter = match (chars.next(), chars.next()) {
            (Some(c), None) => c,
            _ => return Err(PyTypeError::new_err("\"delimiter\" must be a 1-character string")),
        };
        self.rows = Rows { text, pos: 0, delimiter };
        let headings = self.rows.next_row(py)?.ok_or_else(|| PyStopIteration::new_err(()))?;
        let list = PyList::new(py, &headings)?;
        let offsets = PyDict::new(py);
        for (i, h) in headings.iter().enumerate() {
            offsets.set_item(h, i)?;
        }
        self.fmt = Some(Py::new(
            py,
            CsvFormat {
                headings: list.unbind(),
                messages: self.messages.clone_ref(py),
                timestamp_expression: self.timestamp_expression.clone_ref(py),
                field_offset: offsets.unbind(),
            },
        )?);
        Ok(())
    }

    fn parse_next(&mut self, py: Python<'_>) -> PyResult<Option<Py<CsvMessage>>> {
        let Some(row) = self.rows.next_row(py)? else {
            return Ok(None);
        };
        let line = PyList::empty(py);
        let float = py.get_type::<PyFloat>();
        for entry in &row {
            match float.call1((entry,)) {
                Ok(v) => line.append(v)?,
                Err(err) if err.is_instance_of::<PyValueError>(py) => line.append(entry)?,
                Err(err) => return Err(err),
            }
        }
        let fmt = self.fmt.as_ref().expect("format is read on open").clone_ref(py);
        let message = Py::new(py, CsvMessage { message_type: self.message_type.clone_ref(py), fmt, line: line.unbind() })?;
        self.messages.bind(py).set_item(self.message_type.bind(py), &message)?;
        Ok(Some(message))
    }
}

#[pymethods]
impl CsvReader {
    fn rewind(&mut self, py: Python<'_>) -> PyResult<()> {
        self.rewind_inner(py)
    }

    fn _rewind(&mut self, py: Python<'_>) -> PyResult<()> {
        self.rewind_inner(py)
    }

    fn recv_msg(&mut self, py: Python<'_>) -> PyResult<Option<Py<CsvMessage>>> {
        self.parse_next(py)
    }

    fn _parse_next(&mut self, py: Python<'_>) -> PyResult<Option<Py<CsvMessage>>> {
        self.parse_next(py)
    }

    #[pyo3(signature = (condition=None, r#type=None, blocking=false))]
    fn recv_match(
        &mut self,
        py: Python<'_>,
        condition: Option<&Bound<'_, PyAny>>,
        r#type: Option<&Bound<'_, PyAny>>,
        blocking: bool,
    ) -> PyResult<Option<Py<CsvMessage>>> {
        let _ = blocking;
        let wanted: Option<Bound<'_, PyAny>> = match r#type {
            None => None,
            Some(t) if t.is_none() => None,
            Some(t) if t.is_instance_of::<PyList>() || t.is_instance_of::<PySet>() => Some(t.clone()),
            Some(t) => Some(PyList::new(py, [t])?.into_any()),
        };
        loop {
            let Some(m) = self.parse_next(py)? else {
                return Ok(None);
            };
            if let Some(types) = &wanted {
                if !types.contains(m.bind(py).call_method0("get_type")?)? {
                    continue;
                }
            }
            if condition.is_some_and(|c| !c.is_none()) {
                return Err(not_evaluated("recv_match conditions"));
            }
            return Ok(Some(m));
        }
    }

    fn check_condition(&self, condition: Option<&Bound<'_, PyAny>>) -> PyResult<bool> {
        match condition {
            None => Ok(true),
            Some(c) if c.is_none() => Ok(true),
            Some(_) => Err(not_evaluated("recv_match conditions")),
        }
    }

    fn count_lines(&mut self, py: Python<'_>) -> PyResult<i64> {
        self.rewind_inner(py)?;
        let mut count = 0;
        loop {
            let more = if count == 0 { self.parse_next(py)?.is_some() } else { self.rows.next_row(py)?.is_some() };
            if !more {
                self.rewind_inner(py)?;
                return Ok(count);
            }
            count += 1;
        }
    }

    #[getter]
    fn _count(&mut self, py: Python<'_>) -> PyResult<i64> {
        if self.linecount.is_none() {
            self.linecount = Some(self.count_lines(py)?);
        }
        Ok(self.linecount.unwrap_or(0))
    }

    fn flightmode_list(&self, py: Python<'_>) -> Py<PyList> {
        self._flightmodes.clone_ref(py)
    }

    fn name_to_id(&self, _name: &Bound<'_, PyAny>) -> i64 {
        1
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.filename)?;
        visit.call(&self.message_type)?;
        visit.call(&self.progress_callback)?;
        visit.call(&self.timestamp_expression)?;
        visit.call(&self.timestamp)?;
        visit.call(&self.params)?;
        visit.call(&self.messages)?;
        visit.call(&self._flightmodes)?;
        if let Some(fmt) = &self.fmt {
            visit.call(fmt)?;
        }
        Ok(())
    }

    fn __clear__(&mut self, py: Python<'_>) {
        self.messages = PyDict::new(py).unbind();
        self.fmt = None;
    }
}

/// Open a `csv:` device the way `mavutil.mavlink_connection` does.
pub fn open(
    py: Python<'_>,
    device: &str,
    progress_callback: Option<PyObject>,
    opts: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<CsvReader>> {
    let options = PyDict::new(py);
    if let Some(o) = opts {
        for (k, v) in o.iter() {
            options.set_item(k, v)?;
        }
    }
    let mut device = device.to_string();
    if device.contains("separator=:") {
        options.set_item("separator", ":")?;
        device = device.replace(":separator=:", "");
    }
    let components: Vec<&str> = device.split(':').collect();
    let filename = components.get(1).ok_or_else(|| PyIndexError::new_err("list index out of range"))?;
    for nv in &components[2..] {
        let parts: Vec<&str> = nv.split('=').collect();
        match parts.len() {
            2 => options.set_item(parts[0], parts[1])?,
            1 => return Err(PyValueError::new_err("not enough values to unpack (expected 2, got 1)")),
            _ => return Err(PyValueError::new_err("too many values to unpack (expected 2)")),
        }
    }
    let mut separator = ";".to_string();
    let mut message_type = PyString::new(py, "CSV").into_any().unbind();
    let mut expression = py.None();
    for (k, v) in options.iter() {
        match k.extract::<String>()?.as_str() {
            "separator" => separator = v.extract()?,
            "message_type" => message_type = v.unbind(),
            "timestamp_expression" => expression = v.unbind(),
            other => {
                return Err(PyTypeError::new_err(format!(
                    "CSVReader.__init__() got an unexpected keyword argument '{other}'"
                )));
            }
        }
    }
    if let Some(env) = py.import("os")?.getattr("environ")?.call_method1("get", ("CSV_SEPARATOR",))?.extract::<Option<String>>()? {
        separator = env;
    }
    let messages = PyDict::new(py);
    let reader = Py::new(
        py,
        CsvReader {
            filename: PyString::new(py, filename).into_any().unbind(),
            separator,
            message_type,
            progress_callback: progress_callback.unwrap_or_else(|| py.None()),
            timestamp_expression: expression,
            timestamp: 0i64.into_pyobject(py)?.into_any().unbind(),
            verbose: false,
            linecount: None,
            params: PyDict::new(py).unbind(),
            percent: 0,
            messages: messages.clone().unbind(),
            _flightmodes: PyList::empty(py).unbind(),
            fmt: None,
            rows: Rows { text: String::new(), pos: 0, delimiter: ';' },
        },
    )?;
    messages.set_item("MAV", &reader)?;
    {
        let mut r = reader.borrow_mut(py);
        r.rewind_inner(py)?;
        r.parse_next(py)?;
        r.rewind_inner(py)?;
    }
    Ok(reader)
}
