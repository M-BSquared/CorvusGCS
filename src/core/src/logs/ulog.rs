//! A minimal ULog reader (`ulog.py`): the header, the definitions, the
//! topics a review asks for, the log messages and the dropouts.
//!
//! An ArduPilot DataFlash log reaches this reader too (it lands in the same
//! folder) and is refused with the sentence Python gives, because it is a
//! different container.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub const MAGIC: &[u8; 7] = b"ULog\x01\x12\x35";
pub const DATAFLASH_MAGIC: &[u8; 2] = b"\xa3\x95";
pub const HEADER_BYTES: usize = 16;
pub const MAX_FILE_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UlogError(pub String);

impl std::fmt::Display for UlogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Prim {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    Bool,
    Char,
}

impl Prim {
    fn of(name: &str) -> Option<Prim> {
        Some(match name {
            "int8_t" => Prim::I8,
            "uint8_t" => Prim::U8,
            "int16_t" => Prim::I16,
            "uint16_t" => Prim::U16,
            "int32_t" => Prim::I32,
            "uint32_t" => Prim::U32,
            "int64_t" => Prim::I64,
            "uint64_t" => Prim::U64,
            "float" => Prim::F32,
            "double" => Prim::F64,
            "bool" => Prim::Bool,
            "char" => Prim::Char,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Prim::I8 | Prim::U8 | Prim::Bool | Prim::Char => 1,
            Prim::I16 | Prim::U16 => 2,
            Prim::I32 | Prim::U32 | Prim::F32 => 4,
            Prim::I64 | Prim::U64 | Prim::F64 => 8,
        }
    }

    fn is_float(self) -> bool {
        matches!(self, Prim::F32 | Prim::F64)
    }
}

/// One decoded primitive, as Python holds it: an int, a float.
#[derive(Clone, Copy, Debug)]
enum Num {
    Int(i128),
    Float(f64),
}

impl Num {
    fn f64(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }
}

fn read_num(prim: Prim, b: &[u8]) -> Num {
    match prim {
        Prim::I8 => Num::Int(i128::from(b[0] as i8)),
        Prim::U8 => Num::Int(i128::from(b[0])),
        Prim::Bool => Num::Int(i128::from(b[0] != 0)),
        Prim::Char => Num::Int(i128::from(b[0])),
        Prim::I16 => Num::Int(i128::from(i16::from_le_bytes([b[0], b[1]]))),
        Prim::U16 => Num::Int(i128::from(u16::from_le_bytes([b[0], b[1]]))),
        Prim::I32 => Num::Int(i128::from(i32::from_le_bytes(b[..4].try_into().expect("4")))),
        Prim::U32 => Num::Int(i128::from(u32::from_le_bytes(b[..4].try_into().expect("4")))),
        Prim::I64 => Num::Int(i128::from(i64::from_le_bytes(b[..8].try_into().expect("8")))),
        Prim::U64 => Num::Int(i128::from(u64::from_le_bytes(b[..8].try_into().expect("8")))),
        Prim::F32 => Num::Float(f64::from(f32::from_le_bytes(b[..4].try_into().expect("4")))),
        Prim::F64 => Num::Float(f64::from_le_bytes(b[..8].try_into().expect("8"))),
    }
}

/// One column of a topic: what `log.series(...)` returns in Python, a list
/// of ints, floats, strings or arrays.
#[derive(Clone, Debug, PartialEq)]
pub enum Col {
    Int(Vec<i128>),
    Bool(Vec<bool>),
    Float(Vec<f64>),
    Str(Vec<String>),
    Arr(Vec<Vec<f64>>),
}

impl Col {
    pub fn len(&self) -> usize {
        match self {
            Col::Int(v) => v.len(),
            Col::Bool(v) => v.len(),
            Col::Float(v) => v.len(),
            Col::Str(v) => v.len(),
            Col::Arr(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `isinstance(values[0], list)`.
    pub fn is_list(&self) -> bool {
        matches!(self, Col::Arr(_))
    }

    /// `isinstance(values[0], int)`.
    pub fn is_int(&self) -> bool {
        matches!(self, Col::Int(_) | Col::Bool(_))
    }

    /// `float(values[i])`; None where Python raises.
    pub fn f64_at(&self, i: usize) -> Option<f64> {
        match self {
            Col::Int(v) => v.get(i).map(|x| *x as f64),
            Col::Bool(v) => v.get(i).map(|x| f64::from(u8::from(*x))),
            Col::Float(v) => v.get(i).copied(),
            Col::Str(v) => v.get(i).and_then(|s| float_of_text(s)),
            Col::Arr(_) => None,
        }
    }

    /// `int(values[i])`; None where Python raises.
    pub fn i128_at(&self, i: usize) -> Option<i128> {
        match self {
            Col::Int(v) => v.get(i).copied(),
            Col::Bool(v) => v.get(i).map(|x| i128::from(*x)),
            Col::Float(v) => v
                .get(i)
                .and_then(|f| if f.is_finite() { Some(f.trunc() as i128) } else { None }),
            Col::Str(v) => v.get(i).and_then(|s| s.trim().parse().ok()),
            Col::Arr(_) => None,
        }
    }

    /// Truthiness of `values[i]` (`not valid[index]`).
    pub fn truthy_at(&self, i: usize) -> Option<bool> {
        match self {
            Col::Int(v) => v.get(i).map(|x| *x != 0),
            Col::Bool(v) => v.get(i).copied(),
            Col::Float(v) => v.get(i).map(|x| *x != 0.0),
            Col::Str(v) => v.get(i).map(|s| !s.is_empty()),
            Col::Arr(v) => v.get(i).map(|r| !r.is_empty()),
        }
    }

    fn first_int(&self) -> Option<i128> {
        match self {
            Col::Int(v) => v.first().copied(),
            _ => None,
        }
    }
}

/// Python's `float(text)`.
fn float_of_text(text: &str) -> Option<f64> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let cleaned: String;
    let t = if t.contains('_') {
        cleaned = t.replace('_', "");
        cleaned.as_str()
    } else {
        t
    };
    t.parse().ok()
}

/// An info or parameter value.
#[derive(Clone, Debug, PartialEq)]
pub enum Scalar {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
    None,
}

impl Scalar {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Scalar::Int(i) => {
                if let Ok(v) = i64::try_from(*i) {
                    serde_json::Value::from(v)
                } else if let Ok(v) = u64::try_from(*i) {
                    serde_json::Value::from(v)
                } else {
                    serde_json::Value::from(*i as f64)
                }
            }
            Scalar::Float(f) => crate::json::num(*f),
            Scalar::Bool(b) => serde_json::Value::from(*b),
            Scalar::Str(s) => serde_json::Value::from(s.as_str()),
            Scalar::None => serde_json::Value::Null,
        }
    }

    /// Python truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Scalar::Int(i) => *i != 0,
            Scalar::Float(f) => *f != 0.0,
            Scalar::Bool(b) => *b,
            Scalar::Str(s) => !s.is_empty(),
            Scalar::None => false,
        }
    }

    /// `str(value)`.
    pub fn py_str(&self) -> String {
        match self {
            Scalar::Int(i) => i.to_string(),
            Scalar::Float(f) => super::pytext::py_float_repr(*f),
            Scalar::Bool(b) => if *b { "True" } else { "False" }.into(),
            Scalar::Str(s) => s.clone(),
            Scalar::None => "None".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogMessage {
    pub t: u64,
    pub level: &'static str,
    pub text: String,
}

type Format = Vec<(String, i64, String)>;

/// A parsed ULog.
#[derive(Default)]
pub struct ULog {
    pub info: HashMap<String, Scalar>,
    pub params: HashMap<String, Scalar>,
    pub formats: HashMap<String, Format>,
    pub messages: Vec<LogMessage>,
    pub dropout_ms: Vec<u16>,
    pub start_timestamp: u64,
    /// topic -> multi id -> field -> values
    pub data: BTreeMap<String, BTreeMap<u8, HashMap<String, Col>>>,
    pub truncated: bool,
}

impl ULog {
    /// One field of one topic instance; `None` when it is not in the log.
    pub fn series(&self, topic: &str, field: &str, multi_id: u8) -> Option<&Col> {
        self.data.get(topic)?.get(&multi_id)?.get(field)
    }

    /// The columns of one instance, when it has any.
    pub fn fields(&self, topic: &str, multi_id: u8) -> Option<&HashMap<String, Col>> {
        self.data.get(topic)?.get(&multi_id)
    }

    /// Multi-instance ids present for `topic`.
    pub fn instances(&self, topic: &str) -> Vec<u8> {
        self.data
            .get(topic)
            .map(|m| m.keys().copied().collect())
            .unwrap_or_default()
    }

    /// `bool(self.data.get(topic))`: a topic that was announced counts, even
    /// before it logged a record.
    pub fn has(&self, topic: &str) -> bool {
        self.data.get(topic).is_some_and(|m| !m.is_empty())
    }
}

fn level_name(raw: u8) -> &'static str {
    let mut raw = raw;
    if (b'0'..=b'7').contains(&raw) {
        raw -= b'0';
    }
    match raw {
        0 => "emergency",
        1 => "alert",
        2 => "critical",
        3 => "error",
        4 => "warning",
        5 => "notice",
        6 => "info",
        7 => "debug",
        _ => "info",
    }
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn until_nul(bytes: &[u8]) -> &[u8] {
    match bytes.iter().position(|b| *b == 0) {
        Some(p) => &bytes[..p],
        None => bytes,
    }
}

fn parse_format(definition: &str) -> (String, Format) {
    let (name, rest) = match definition.split_once(':') {
        Some((n, r)) => (n, r),
        None => (definition, ""),
    };
    let mut fields = Vec::new();
    for chunk in rest.split(';') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let (type_part, field_name) = match chunk.split_once(' ') {
            Some((t, f)) => (t, f),
            None => (chunk, ""),
        };
        let mut count = 1;
        let mut type_part = type_part;
        if let Some((head, tail)) = type_part.split_once('[') {
            count = tail.trim_end_matches(']').trim().parse().unwrap_or(1);
            type_part = head;
        }
        fields.push((type_part.to_string(), count, field_name.trim().to_string()));
    }
    (name.trim().to_string(), fields)
}

struct Field {
    name: String,
    prim: Prim,
    count: usize,
    offset: usize,
}

struct Layout {
    fields: Vec<Field>,
    /// The kept fields (`plan`): index into `fields`.
    kept: Vec<usize>,
    /// Shortest record that holds every kept field.
    compiled_size: usize,
}

fn is_padding(name: &str) -> bool {
    name.starts_with("_padding") || name.contains(".._padding")
}

fn build_layout(
    fields: &Format,
    formats: &HashMap<String, Format>,
    prefix: &str,
    depth: usize,
) -> Result<(Vec<Field>, usize), UlogError> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    if depth > 6 {
        return Ok((out, offset));
    }
    for (type_, count, name) in fields {
        let full = format!("{prefix}{name}");
        let count_u = usize::try_from(*count).unwrap_or(0);
        if let Some(prim) = Prim::of(type_) {
            out.push(Field {
                name: full,
                prim,
                count: count_u,
                offset,
            });
            offset += prim.size() * count_u;
            continue;
        }
        let Some(nested) = formats.get(type_) else {
            return Err(UlogError(format!("unknown field type {type_:?}")));
        };
        for index in 0..count_u {
            let sub_prefix = if *count == 1 {
                format!("{full}.")
            } else {
                format!("{full}[{index}].")
            };
            let (sub, size) = build_layout(nested, formats, &sub_prefix, depth + 1)?;
            for mut f in sub {
                f.offset += offset;
                out.push(f);
            }
            offset += size;
        }
    }
    Ok((out, offset))
}

fn compile(fields: Vec<Field>) -> Layout {
    let mut order: Vec<usize> = (0..fields.len()).collect();
    order.sort_by_key(|i| fields[*i].offset);
    let mut kept = Vec::new();
    let mut compiled_size = 0usize;
    let mut cursor = 0usize;
    for i in order {
        let f = &fields[i];
        let span = f.prim.size() * f.count;
        cursor = f.offset + span;
        if is_padding(&f.name) {
            continue;
        }
        compiled_size = cursor;
        kept.push(i);
    }
    let _ = cursor;
    Layout {
        fields,
        kept,
        compiled_size,
    }
}

fn new_col(field: &Field) -> Col {
    if field.prim == Prim::Char {
        Col::Str(Vec::new())
    } else if field.count == 1 {
        if field.prim.is_float() {
            Col::Float(Vec::new())
        } else if field.prim == Prim::Bool {
            Col::Bool(Vec::new())
        } else {
            Col::Int(Vec::new())
        }
    } else {
        Col::Arr(Vec::new())
    }
}

fn push_field(col: &mut Col, field: &Field, raw: &[u8]) {
    let size = field.prim.size();
    match col {
        Col::Str(v) => v.push(lossy(until_nul(&raw[field.offset..field.offset + field.count]))),
        Col::Int(v) => {
            if let Num::Int(i) = read_num(field.prim, &raw[field.offset..]) {
                v.push(i);
            }
        }
        Col::Bool(v) => v.push(raw[field.offset] != 0),
        Col::Float(v) => v.push(read_num(field.prim, &raw[field.offset..]).f64()),
        Col::Arr(v) => {
            let mut row = Vec::with_capacity(field.count);
            for k in 0..field.count {
                row.push(read_num(field.prim, &raw[field.offset + k * size..]).f64());
            }
            v.push(row);
        }
    }
}

struct Sink {
    layout: Arc<Layout>,
    topic: String,
    multi_id: u8,
    bound: bool,
}

fn read_value(type_: &str, raw: &[u8]) -> Option<Scalar> {
    if type_.starts_with("char[") {
        return Some(Scalar::Str(lossy(until_nul(raw))));
    }
    let prim = Prim::of(type_)?;
    if raw.len() < prim.size() {
        return Some(Scalar::None);
    }
    Some(match (prim, read_num(prim, raw)) {
        (Prim::Bool, _) => Scalar::Bool(raw[0] != 0),
        (Prim::Char, _) => Scalar::Str(lossy(&raw[..1])),
        (_, Num::Int(i)) => Scalar::Int(i),
        (_, Num::Float(f)) => Scalar::Float(f),
    })
}

/// `b[from:to]`: Python slices clamp instead of failing.
fn slice(b: &[u8], from: usize, to: usize) -> &[u8] {
    let to = to.min(b.len());
    let from = from.min(to);
    &b[from..to]
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Parses a ULog. `topics` limits which are decoded (`None` decodes all).
///
/// A file cut short (power lost mid write) is not an error: everything up to
/// the cut is returned with `truncated` set.
pub fn read(blob: &[u8], topics: Option<&[&str]>) -> Result<ULog, UlogError> {
    if blob.len() > MAX_FILE_BYTES {
        return Err(UlogError("log is too large to review".into()));
    }
    if blob.len() < HEADER_BYTES || &blob[..7] != MAGIC {
        if blob.len() >= 2 && &blob[..2] == DATAFLASH_MAGIC {
            return Err(UlogError(
                "this is an ArduPilot DataFlash log (.bin), not a PX4 ULog. \
                 Corvus can download it but cannot yet analyse it. Open it in \
                 Mission Planner or at plot.ardupilot.org."
                    .into(),
            ));
        }
        return Err(UlogError("not a ULog file".into()));
    }
    let mut log = ULog {
        start_timestamp: u64_at(blob, 8).unwrap_or(0),
        ..ULog::default()
    };
    let wanted: Option<std::collections::HashSet<&str>> = topics.map(|t| t.iter().copied().collect());

    let mut layouts: HashMap<String, Arc<Layout>> = HashMap::new();
    let mut subscriptions: HashMap<u16, Sink> = HashMap::new();
    let total = blob.len();
    let mut offset = HEADER_BYTES;

    while offset + 3 <= total {
        let size = usize::from(u16::from_le_bytes([blob[offset], blob[offset + 1]]));
        let msg_type = blob[offset + 2];
        let body_start = offset + 3;
        let body_end = body_start + size;
        if body_end > total {
            log.truncated = true;
            break;
        }
        let body = &blob[body_start..body_end];
        offset = body_end;
        let _ = handle(
            &mut log,
            &mut layouts,
            &mut subscriptions,
            wanted.as_ref(),
            msg_type,
            body,
        );
    }
    Ok(log)
}

fn handle(
    log: &mut ULog,
    layouts: &mut HashMap<String, Arc<Layout>>,
    subscriptions: &mut HashMap<u16, Sink>,
    wanted: Option<&std::collections::HashSet<&str>>,
    msg_type: u8,
    body: &[u8],
) -> Option<()> {
    match msg_type {
        b'F' => {
            let (name, fields) = parse_format(&lossy(body));
            if !name.is_empty() {
                log.formats.insert(name, fields);
            }
        }
        b'A' => {
            let multi_id = *body.first()?;
            let msg_id = u16_at(body, 1)?;
            let topic = lossy(slice(body, 3, usize::MAX));
            if wanted.is_some_and(|w| !w.contains(topic.as_str())) {
                return Some(());
            }
            if !layouts.contains_key(&topic) {
                let fields = log.formats.get(&topic)?;
                let (flat, _) = build_layout(fields, &log.formats, "", 0).ok()?;
                layouts.insert(topic.clone(), Arc::new(compile(flat)));
            }
            log.data.entry(topic.clone()).or_default().entry(multi_id).or_default();
            subscriptions.insert(
                msg_id,
                Sink {
                    layout: Arc::clone(&layouts[&topic]),
                    topic,
                    multi_id,
                    bound: false,
                },
            );
        }
        b'R' => {
            subscriptions.remove(&u16_at(body, 0)?);
        }
        b'D' => {
            let msg_id = u16_at(body, 0)?;
            let sink = subscriptions.get_mut(&msg_id)?;
            let raw = &body[2..];
            let layout = Arc::clone(&sink.layout);
            let bucket = log.data.get_mut(&sink.topic)?.get_mut(&sink.multi_id)?;
            if raw.len() >= layout.compiled_size {
                if !sink.bound {
                    for &i in &layout.kept {
                        let f = &layout.fields[i];
                        bucket.entry(f.name.clone()).or_insert_with(|| new_col(f));
                    }
                    sink.bound = true;
                }
                for &i in &layout.kept {
                    let f = &layout.fields[i];
                    let col = bucket.get_mut(&f.name)?;
                    push_field(col, f, raw);
                }
            } else {
                for f in &layout.fields {
                    if is_padding(&f.name) {
                        continue;
                    }
                    let end = f.offset + f.prim.size() * f.count;
                    if end > raw.len() {
                        continue;
                    }
                    let col = bucket.entry(f.name.clone()).or_insert_with(|| new_col(f));
                    push_field(col, f, raw);
                }
            }
        }
        b'I' | b'P' | b'Q' => {
            let (key_len, key_start) = if msg_type == b'Q' {
                (usize::from(*body.get(1)?), 2)
            } else {
                (usize::from(*body.first()?), 1)
            };
            let key = lossy(slice(body, key_start, key_start + key_len));
            let (type_, field_name) = key.split_once(' ').unwrap_or((key.as_str(), ""));
            let value = read_value(type_, slice(body, key_start + key_len, usize::MAX)).unwrap_or(Scalar::None);
            let target = if msg_type == b'I' {
                &mut log.info
            } else {
                &mut log.params
            };
            if !field_name.is_empty() {
                target.insert(field_name.to_string(), value);
            }
        }
        b'M' => {
            let key_len = usize::from(*body.get(1)?);
            let key = lossy(slice(body, 2, 2 + key_len));
            let (type_, field_name) = key.split_once(' ').unwrap_or((key.as_str(), ""));
            let value = read_value(type_, slice(body, 2 + key_len, usize::MAX)).unwrap_or(Scalar::None);
            if !field_name.is_empty() {
                log.info.entry(field_name.to_string()).or_insert(value);
            }
        }
        b'L' | b'C' => {
            let head = if msg_type == b'L' { 9 } else { 11 };
            let stamp = u64_at(body, if msg_type == b'L' { 1 } else { 3 })?;
            let level = level_name(*body.first()?);
            let text = lossy(slice(body, head, usize::MAX)).trim_end_matches('\0').to_string();
            log.messages.push(LogMessage { t: stamp, level, text });
        }
        b'O' => log.dropout_ms.push(u16_at(body, 0)?),
        _ => {}
    }
    Some(())
}

impl ULog {
    /// `log.params.get("SYS_AUTOSTART")` and friends: the first element of an int column.
    pub fn first_int(&self, topic: &str, field: &str, multi_id: u8) -> Option<i128> {
        self.series(topic, field, multi_id)?.first_int()
    }
}
