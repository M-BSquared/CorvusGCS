//! Generates the MAVLink message and enum tables from the XML definitions.
//!
//! This follows pymavlink's `mavparse` and `mavgen_python` step for step, so
//! the tables are the ones pymavlink would generate for the `all` dialect:
//! the same include order, the same field sort, the same CRC_EXTRA, the same
//! enum merging and the same constants. `tests/pymavlink_frozen.rs` compares
//! every table and constant with pymavlink's frozen answers.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const ROOT_DIALECT: &str = "all.xml";
const MAX_INCLUDE_NESTING: usize = 5;

#[derive(Clone)]
struct Field {
    name: String,
    base_type: String,
    array_length: usize,
    type_length: usize,
    enum_name: String,
    display: String,
    units: String,
    instance: bool,
    const_value: Option<i64>,
    description: String,
}

impl Field {
    fn wire_length(&self) -> usize {
        if self.array_length != 0 {
            self.array_length * self.type_length
        } else {
            self.type_length
        }
    }
}

struct Message {
    name: String,
    id: u32,
    fields: Vec<Field>,
    extensions_start: Option<usize>,
    description: String,
}

impl Message {
    fn base_fields(&self) -> usize {
        self.extensions_start.unwrap_or(self.fields.len())
    }
}

#[derive(Clone)]
struct EnumParam {
    index: u32,
    description: String,
    label: String,
}

#[derive(Clone)]
struct EnumEntry {
    name: String,
    value: i64,
    description: String,
    has_location: bool,
    autovalue: bool,
    params: Vec<EnumParam>,
}

struct Enum {
    name: String,
    bitmask: bool,
    entries: Vec<EnumEntry>,
    start_value: Option<i64>,
    highest_value: i64,
}

struct Xml {
    path: PathBuf,
    version: i64,
    includes: Vec<String>,
    messages: Vec<Message>,
    enums: Vec<Enum>,
}

fn type_length(name: &str) -> Option<usize> {
    Some(match name {
        "float" => 4,
        "double" => 8,
        "char" => 1,
        "int8_t" | "uint8_t" => 1,
        "int16_t" | "uint16_t" => 2,
        "int32_t" | "uint32_t" => 4,
        "int64_t" | "uint64_t" => 8,
        _ => return None,
    })
}

fn parse_value(text: &str) -> i64 {
    let text = text.trim();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest.trim()),
        None => (false, text),
    };
    let value = if let Some((base, exp)) = digits.split_once("**") {
        parse_value(base).pow(parse_value(exp) as u32)
    } else if let Some(hex) = digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).expect("enum value")
    } else if let Some(bin) = digits.strip_prefix("0b").or_else(|| digits.strip_prefix("0B")) {
        i64::from_str_radix(bin, 2).expect("enum value")
    } else {
        digits.parse::<i64>().expect("enum value")
    };
    if negative {
        -value
    } else {
        value
    }
}

/// Concatenated text of the element's direct text children, the way expat
/// hands character data to pymavlink's handler for that element.
fn own_text(node: roxmltree::Node) -> String {
    let mut out = String::new();
    for child in node.children() {
        if child.is_text() {
            out.push_str(child.text().unwrap_or(""));
        }
    }
    out
}

fn parse_field(node: roxmltree::Node, version: i64) -> Field {
    let name = node.attribute("name").expect("field name").to_string();
    let mut type_name = node.attribute("type").expect("field type").to_string();
    let mut const_value = None;
    if type_name == "uint8_t_mavlink_version" {
        type_name = "uint8_t".to_string();
        const_value = Some(version);
    }
    let mut array_length = 0usize;
    if let Some(open) = type_name.find('[') {
        assert!(type_name.ends_with(']'));
        array_length = type_name[open + 1..type_name.len() - 1].parse().expect("array length");
        type_name.truncate(open);
        if type_name == "array" {
            type_name = "int8_t".to_string();
        }
    }
    let (base_type, length) = if let Some(len) = type_length(&type_name) {
        (type_name.clone(), len)
    } else if let Some(len) = type_length(&format!("{type_name}_t")) {
        (format!("{type_name}_t"), len)
    } else {
        panic!("unknown type '{type_name}'");
    };
    let units = node.attribute("units").unwrap_or("");
    Field {
        name,
        base_type,
        array_length,
        type_length: length,
        enum_name: node.attribute("enum").unwrap_or("").to_string(),
        display: node.attribute("display").unwrap_or("").to_string(),
        units: units.to_string(),
        instance: node.attribute("instance").is_some_and(|v| !v.is_empty()),
        const_value,
        description: own_text(node),
    }
}

fn parse_xml(path: &Path, allow_extensions: bool) -> Xml {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let doc = roxmltree::Document::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let root = doc.root_element();
    assert_eq!(root.tag_name().name(), "mavlink");
    let mut xml = Xml {
        path: path.to_path_buf(),
        version: 2,
        includes: Vec::new(),
        messages: Vec::new(),
        enums: Vec::new(),
    };
    // <version> must be known before a mavlink_version field is read, and in
    // every definition file it precedes <messages>, as expat sees it.
    for child in root.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "version" => xml.version = own_text(child).trim().parse().expect("version"),
            "include" => xml.includes.push(own_text(child)),
            _ => {}
        }
    }
    for section in root.children().filter(|n| n.is_element()) {
        match section.tag_name().name() {
            "enums" => {
                for node in section
                    .children()
                    .filter(|n| n.is_element() && n.tag_name().name() == "enum")
                {
                    xml.enums.push(parse_enum(node));
                }
            }
            "messages" => {
                for node in section
                    .children()
                    .filter(|n| n.is_element() && n.tag_name().name() == "message")
                {
                    xml.messages.push(parse_message(node, xml.version, allow_extensions));
                }
            }
            _ => {}
        }
    }
    xml
}

fn parse_enum(node: roxmltree::Node) -> Enum {
    let mut e = Enum {
        name: node.attribute("name").expect("enum name").to_string(),
        bitmask: node.attribute("bitmask") == Some("true"),
        entries: Vec::new(),
        start_value: None,
        highest_value: 0,
    };
    for entry in node
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "entry")
    {
        let (value, autovalue) = match entry.attribute("value") {
            Some(v) => (parse_value(v), false),
            None => (e.highest_value + 1, true),
        };
        if e.start_value.is_none_or(|s| value < s) {
            e.start_value = Some(value);
        }
        if value > e.highest_value {
            e.highest_value = value;
        }
        let has_location = match entry.attribute("hasLocation") {
            None | Some("false") => false,
            Some("true") => true,
            Some(other) => panic!("invalid has_location value {other}"),
        };
        let mut description = String::new();
        let mut params = Vec::new();
        for child in entry.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "description" => description.push_str(&own_text(child)),
                "param" => {
                    let reserved = child.attribute("reserved").is_some_and(|v| !v.is_empty());
                    let default = child.attribute("default").unwrap_or("0");
                    let mut text = if reserved {
                        format!("Reserved (default:{default})")
                    } else {
                        String::new()
                    };
                    text.push_str(&own_text(child));
                    params.push(EnumParam {
                        index: child
                            .attribute("index")
                            .expect("param index")
                            .parse()
                            .expect("param index"),
                        description: text,
                        label: child.attribute("label").unwrap_or("").to_string(),
                    });
                }
                _ => {}
            }
        }
        e.entries.push(EnumEntry {
            name: entry.attribute("name").expect("entry name").to_string(),
            value,
            description,
            has_location,
            autovalue,
            params,
        });
    }
    if e.name.contains("MAV_CMD") {
        for entry in &mut e.entries {
            if entry.params.len() == 7 {
                continue;
            }
            let mut slots: Vec<EnumParam> = (1..=7)
                .map(|index| EnumParam {
                    index,
                    description: "Reserved (default:0)".to_string(),
                    label: String::new(),
                })
                .collect();
            let mut extra = Vec::new();
            for p in entry.params.drain(..) {
                match slots.iter_mut().find(|s| s.index == p.index) {
                    Some(slot) => *slot = p,
                    None => extra.push(p),
                }
            }
            slots.extend(extra);
            entry.params = slots;
        }
    }
    e
}

fn parse_message(node: roxmltree::Node, version: i64, allow_extensions: bool) -> Message {
    let mut m = Message {
        name: node.attribute("name").expect("message name").to_string(),
        id: node
            .attribute("id")
            .expect("message id")
            .trim()
            .parse()
            .expect("message id"),
        fields: Vec::new(),
        extensions_start: None,
        description: String::new(),
    };
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "description" => m.description.push_str(&own_text(child)),
            "extensions" => m.extensions_start = Some(m.fields.len()),
            "field" if m.extensions_start.is_none() || allow_extensions => {
                m.fields.push(parse_field(child, version));
            }
            _ => {}
        }
    }
    m
}

fn x25(crc: u16, bytes: &[u8]) -> u16 {
    let mut accum = crc;
    for &b in bytes {
        let mut tmp = b ^ (accum & 0xFF) as u8;
        tmp ^= tmp << 4;
        accum = (accum >> 8) ^ ((tmp as u16) << 8) ^ ((tmp as u16) << 3) ^ ((tmp as u16) >> 4);
    }
    accum
}

/// Field indices in wire order: base fields stably sorted by descending type
/// length, extension fields after them in declaration order.
fn wire_order(m: &Message) -> Vec<usize> {
    let base = m.base_fields();
    let mut order: Vec<usize> = (0..base).collect();
    order.sort_by(|a, b| m.fields[*b].type_length.cmp(&m.fields[*a].type_length));
    order.extend(base..m.fields.len());
    order
}

fn crc_extra(m: &Message, order: &[usize]) -> u8 {
    let mut crc = x25(0xFFFF, format!("{} ", m.name).as_bytes());
    for &i in &order[..m.base_fields()] {
        let f = &m.fields[i];
        crc = x25(crc, format!("{} ", f.base_type).as_bytes());
        crc = x25(crc, format!("{} ", f.name).as_bytes());
        if f.array_length != 0 {
            crc = x25(crc, &[f.array_length as u8]);
        }
    }
    ((crc & 0xFF) ^ (crc >> 8)) as u8
}

fn load_dialect(dir: &Path, allow_extensions: bool) -> Vec<Xml> {
    let mut xml = vec![parse_xml(&dir.join(ROOT_DIALECT), allow_extensions)];
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for _ in 0..MAX_INCLUDE_NESTING {
        let mut added = false;
        let count = xml.len();
        for i in 0..count {
            let includes = xml[i].includes.clone();
            let base = xml[i].path.parent().unwrap().to_path_buf();
            for include in includes {
                let path = base.join(include.trim());
                if seen.contains(&path) {
                    continue;
                }
                xml.push(parse_xml(&path, allow_extensions));
                seen.insert(path);
                added = true;
            }
        }
        if !added {
            break;
        }
    }
    xml
}

/// pymavlink's merge_enums: an enum defined again in a later file has its
/// entries appended to the first definition, auto-assigned values that would
/// collide are moved past the newcomer's range, entries are sorted by value
/// and an ENUM_END is appended.
fn merge_enums(xml: &mut [Xml]) -> Vec<Enum> {
    let mut merged: Vec<Enum> = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for x in xml.iter_mut() {
        for mut e in std::mem::take(&mut x.enums) {
            if let Some(&at) = by_name.get(&e.name) {
                let first = &mut merged[at];
                if first.start_value.unwrap_or(0) <= e.highest_value
                    && first.highest_value >= e.start_value.unwrap_or(0)
                {
                    for entry in first.entries.iter_mut() {
                        if entry.value <= e.highest_value && entry.autovalue {
                            entry.value = e.highest_value + 1;
                            e.highest_value = entry.value;
                        }
                    }
                }
                first.entries.extend(e.entries);
            } else {
                by_name.insert(e.name.clone(), merged.len());
                merged.push(e);
            }
        }
    }
    for e in merged.iter_mut() {
        e.entries.sort_by_key(|entry| entry.value);
        let end = e.entries.last().map(|entry| entry.value + 1).unwrap_or(1);
        e.entries.push(EnumEntry {
            name: format!("{}_ENUM_END", e.name),
            value: end,
            description: String::new(),
            has_location: false,
            autovalue: false,
            params: Vec::new(),
        });
    }
    merged
}

fn ty_variant(base: &str) -> &'static str {
    match base {
        "char" => "Ty::Char",
        "uint8_t" => "Ty::U8",
        "int8_t" => "Ty::I8",
        "uint16_t" => "Ty::U16",
        "int16_t" => "Ty::I16",
        "uint32_t" => "Ty::U32",
        "int32_t" => "Ty::I32",
        "uint64_t" => "Ty::U64",
        "int64_t" => "Ty::I64",
        "float" => "Ty::F32",
        "double" => "Ty::F64",
        other => panic!("type {other}"),
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = manifest.join("message_definitions").join("v1.0");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", dir.display());
    for entry in std::fs::read_dir(&dir).unwrap() {
        println!("cargo:rerun-if-changed={}", entry.unwrap().path().display());
    }

    let mut xml = load_dialect(&dir, true);
    let enums = merge_enums(&mut xml);
    let bitmask: HashMap<&str, bool> = enums.iter().map(|e| (e.name.as_str(), e.bitmask)).collect();

    let mut messages: Vec<&Message> = xml.iter().flat_map(|x| x.messages.iter()).collect();
    let mut ids = HashSet::new();
    for m in &messages {
        assert!(ids.insert(m.id), "duplicate message id {}", m.id);
    }
    messages.sort_by_key(|m| m.id);

    let mut out = String::new();
    out.push_str("// Generated by build.rs from message_definitions/v1.0. Do not edit.\n\n");

    let _ = writeln!(out, "pub static MESSAGES: &[MsgDef] = &[");
    for m in &messages {
        let order = wire_order(m);
        let crc = crc_extra(m, &order);
        let mut offsets = vec![0usize; m.fields.len()];
        let mut offset = 0;
        for &i in &order {
            offsets[i] = offset;
            offset += m.fields[i].wire_length();
        }
        let base_len: usize = order[..m.base_fields()]
            .iter()
            .map(|&i| m.fields[i].wire_length())
            .sum();
        let instance = m.fields.iter().position(|f| f.instance);
        let _ = writeln!(out, "    MsgDef {{");
        let _ = writeln!(out, "        id: {},", m.id);
        let _ = writeln!(out, "        name: {:?},", m.name);
        let _ = writeln!(out, "        lower: {:?},", m.name.to_lowercase());
        let _ = writeln!(out, "        crc_extra: {crc},");
        let _ = writeln!(out, "        base_fields: {},", m.base_fields());
        let _ = writeln!(out, "        wire_len: {offset},");
        let _ = writeln!(out, "        base_wire_len: {base_len},");
        let _ = writeln!(
            out,
            "        order: &{:?},",
            order.iter().map(|&i| i as u8).collect::<Vec<_>>()
        );
        let _ = writeln!(out, "        instance_field: {:?},", instance.map(|i| i as u8));
        let _ = writeln!(out, "        description: {:?},", m.description);
        let _ = writeln!(out, "        fields: &[");
        for (i, f) in m.fields.iter().enumerate() {
            let mut display = f.display.clone();
            if display.is_empty() && !f.enum_name.is_empty() && bitmask.get(f.enum_name.as_str()) == Some(&true) {
                display = "bitmask".to_string();
            }
            let _ = writeln!(
                out,
                "            FieldDef {{ name: {:?}, ty: {}, type_name: {:?}, array_len: {}, wire_offset: {}, enum_name: {:?}, display: {:?}, units: {:?}, const_value: {:?}, description: {:?} }},",
                f.name,
                ty_variant(&f.base_type),
                f.base_type,
                f.array_length,
                offsets[i],
                f.enum_name,
                display,
                f.units,
                f.const_value,
                f.description,
            );
        }
        let _ = writeln!(out, "        ],");
        let _ = writeln!(out, "    }},");
    }
    let _ = writeln!(out, "];\n");

    let _ = writeln!(out, "pub static ENUMS: &[EnumDef] = &[");
    for e in &enums {
        let _ = writeln!(
            out,
            "    EnumDef {{ name: {:?}, bitmask: {}, entries: &[",
            e.name, e.bitmask
        );
        for entry in &e.entries {
            let params: Vec<String> = entry
                .params
                .iter()
                .map(|p| {
                    format!(
                        "({}, {:?}, {:?})",
                        p.index,
                        p.description.replace('\t', "    "),
                        p.label
                    )
                })
                .collect();
            let _ = writeln!(
                out,
                "        EnumEntryDef {{ name: {:?}, value: {}, description: {:?}, has_location: {}, params: &[{}] }},",
                entry.name,
                entry.value,
                entry.description.replace('\t', "    "),
                entry.has_location,
                params.join(", "),
            );
        }
        let _ = writeln!(out, "    ] }},");
    }
    let _ = writeln!(out, "];");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(out_dir.join("definitions.rs"), out).unwrap();
}
