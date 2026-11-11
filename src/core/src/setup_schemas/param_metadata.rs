//! Parameter metadata from the vehicle: defaults, and on PX4 the documentation
//! (`param_metadata.py`).
//!
//! The parameter protocol carries a name, a value and a type, and no field for
//! what the value was out of the box, so a ground station that shows defaults
//! reads them from a file the autopilot keeps on board, over MAVLink FTP:
//!
//! * PX4 builds `parameters.json.xz` into its ROMFS: every parameter's default,
//!   description, unit, range and value labels for exactly the firmware that is
//!   running.
//! * ArduPilot 4.1 and later generate `@PARAM/param.pck` on request; with
//!   `withdefaults=1` each entry carries its default beside its value.
//!
//! Which of the two the connected vehicle has is the dialect's to say; this
//! module reads the files.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::json::Map;
use crate::protocol::common::float_of_text;

pub const METADATA_PX4_JSON: &str = "px4-json";
pub const METADATA_ARDUPILOT_PCK: &str = "ardupilot-pck";

/// PX4's uncompressed metadata is a few MB; anything far past that is not it.
const MAX_DECOMPRESSED_BYTES: usize = 48 * 1024 * 1024;
const XZ_MAGIC: &[u8] = b"\xfd7zXZ\x00";

const PCK_MAGIC: u16 = 0x671B;
const PCK_MAGIC_DEFAULTS: u16 = 0x671C;

/// Why a metadata file could not be read, as a sentence for the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataError(pub String);

impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MetadataError {}

fn err<T>(text: &str) -> Result<T, MetadataError> {
    Err(MetadataError(text.to_string()))
}

/// Parameter name to its metadata, in the order the file lists them.
pub type ParamMeta = Map;

fn number_of(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => float_of_text(s),
        _ => None,
    }
}

fn text_of(value: Option<&Value>) -> &str {
    match value {
        Some(Value::String(s)) => s.trim(),
        _ => "",
    }
}

struct Limited {
    out: Vec<u8>,
}

impl Write for Limited {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.out.len() + buf.len() > MAX_DECOMPRESSED_BYTES {
            return Err(io::Error::other("too large"));
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn decompress(raw: &[u8]) -> Result<Vec<u8>, MetadataError> {
    if !raw.starts_with(XZ_MAGIC) {
        return Ok(raw.to_vec());
    }
    let mut out = Limited { out: Vec::new() };
    let mut input = raw;
    match lzma_rs::xz_decompress(&mut input, &mut out) {
        Ok(()) => Ok(out.out),
        Err(_) => err("the metadata file is truncated or too large"),
    }
}

fn float_value(n: f64) -> Value {
    crate::json::num(n)
}

/// PX4 `parameters.json(.xz)` to `{name: {default, short_desc, ...}}`. Only the
/// keys a parameter carries are set, so the editor can tell "no unit" from an
/// empty one.
pub fn parse_px4_metadata(raw: &[u8]) -> Result<ParamMeta, MetadataError> {
    let text = decompress(raw)?;
    let text = std::str::from_utf8(&text).map_err(|e| MetadataError(format!("{e}")))?;
    let doc: Value = serde_json::from_str(text).map_err(|e| MetadataError(format!("{e}")))?;
    let Some(entries) = doc.get("parameters").and_then(Value::as_array) else {
        return err("not a PX4 parameter metadata file");
    };
    let mut out = ParamMeta::new();
    for entry in entries {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let name = text_of(entry.get("name"));
        if name.is_empty() {
            continue;
        }
        let mut meta = Map::new();
        for key in ["default", "min", "max", "increment"] {
            if let Some(n) = number_of(entry.get(key)) {
                meta.insert(key.into(), float_value(n));
            }
        }
        for (src, dst) in [
            ("shortDesc", "short_desc"),
            ("longDesc", "long_desc"),
            ("units", "units"),
            ("group", "group"),
        ] {
            let t = text_of(entry.get(src));
            if !t.is_empty() {
                meta.insert(dst.into(), Value::from(t));
            }
        }
        if let Some(d) = entry.get("decimalPlaces").and_then(Value::as_i64) {
            if (0..=9).contains(&d) {
                meta.insert("decimals".into(), Value::from(d));
            }
        }
        if entry.get("rebootRequired") == Some(&Value::Bool(true)) {
            meta.insert("reboot_required".into(), Value::from(true));
        }
        let mut values = Vec::new();
        if let Some(items) = entry.get("values").and_then(Value::as_array) {
            for item in items {
                if let Some(item) = item.as_object() {
                    if let Some(n) = number_of(item.get("value")) {
                        values.push(Value::Array(vec![
                            float_value(n),
                            Value::from(text_of(item.get("description"))),
                        ]));
                    }
                }
            }
        }
        if !values.is_empty() {
            meta.insert("values".into(), Value::Array(values));
        }
        let mut bits = Vec::new();
        if let Some(items) = entry.get("bitmask").and_then(Value::as_array) {
            for item in items {
                if let Some(item) = item.as_object() {
                    // isinstance(True, int) holds in Python, so a boolean index passes there too.
                    if let Some(index) = item.get("index").filter(|v| v.is_i64() || v.is_u64() || v.is_boolean()) {
                        bits.push(Value::Array(vec![
                            index.clone(),
                            Value::from(text_of(item.get("description"))),
                        ]));
                    }
                }
            }
        }
        if !bits.is_empty() {
            meta.insert("bitmask".into(), Value::Array(bits));
        }
        out.insert(name.to_string(), Value::Object(meta));
    }
    Ok(out)
}

fn pck_width(kind: u8) -> Option<usize> {
    match kind {
        1 => Some(1),
        2 => Some(2),
        3 | 4 => Some(4),
        _ => None,
    }
}

fn pck_read(kind: u8, bytes: &[u8]) -> f64 {
    match kind {
        1 => f64::from(bytes[0] as i8),
        2 => f64::from(i16::from_le_bytes([bytes[0], bytes[1]])),
        3 => f64::from(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        _ => f64::from(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
    }
}

/// ArduPilot `@PARAM/param.pck` to `{name: {"default": value}}`.
///
/// The pack is a 6 byte header (magic, parameters in this file, total) and then
/// one entry per parameter: a byte of type (low nibble) and flags (high
/// nibble), a byte of name length and the prefix it shares with the previous
/// name, the rest of the name, the value, and the default when flag 1 is set.
/// An entry without that flag is at its default. Zero bytes between entries are
/// padding. A pack without the defaults magic carries no defaults, which is an
/// empty answer, not an error.
pub fn parse_ardupilot_pck(raw: &[u8]) -> Result<ParamMeta, MetadataError> {
    if raw.len() < 6 {
        return err("the parameter pack is truncated");
    }
    let magic = u16::from_le_bytes([raw[0], raw[1]]);
    let count = u16::from_le_bytes([raw[2], raw[3]]) as usize;
    if magic != PCK_MAGIC && magic != PCK_MAGIC_DEFAULTS {
        return err("not an ArduPilot parameter pack");
    }
    let with_defaults = magic == PCK_MAGIC_DEFAULTS;
    let mut out = ParamMeta::new();
    let mut pos = 6usize;
    let mut last: Vec<u8> = Vec::new();
    let mut seen = 0usize;
    while pos < raw.len() {
        if raw[pos] == 0 {
            pos += 1;
            continue;
        }
        if pos + 2 > raw.len() {
            return err("the parameter pack is truncated");
        }
        let (type_byte, len_byte) = (raw[pos], raw[pos + 1]);
        let kind = type_byte & 0x0F;
        let Some(width) = pck_width(kind) else {
            return Err(MetadataError(format!("unknown parameter type {kind} in the pack")));
        };
        let name_len = usize::from(len_byte >> 4) + 1;
        let common = usize::from(len_byte & 0x0F);
        let has_default = with_defaults && (type_byte >> 4) & 1 == 1;
        let end = pos + 2 + name_len + width * if has_default { 2 } else { 1 };
        if end > raw.len() || common > last.len() {
            return err("the parameter pack is truncated");
        }
        let mut name_bytes = last[..common].to_vec();
        name_bytes.extend_from_slice(&raw[pos + 2..pos + 2 + name_len]);
        let value_at = pos + 2 + name_len;
        let value = pck_read(kind, &raw[value_at..value_at + width]);
        let default = if has_default {
            pck_read(kind, &raw[value_at + width..value_at + 2 * width])
        } else {
            value
        };
        last = name_bytes.clone();
        pos = end;
        seen += 1;
        let name: String = name_bytes
            .iter()
            .map(|b| if b.is_ascii() { *b as char } else { '\u{fffd}' })
            .collect();
        let mut entry = Map::new();
        if with_defaults {
            entry.insert("default".into(), float_value(default));
        }
        out.insert(name, Value::Object(entry));
    }
    if seen != count {
        return Err(MetadataError(format!(
            "the parameter pack lists {count} parameters but holds {seen}"
        )));
    }
    Ok(out)
}

/// Parses a metadata file in the format the dialect named.
pub fn parse_param_metadata(fmt: &str, raw: &[u8]) -> Result<ParamMeta, MetadataError> {
    match fmt {
        METADATA_PX4_JSON => parse_px4_metadata(raw),
        METADATA_ARDUPILOT_PCK => parse_ardupilot_pck(raw),
        other => Err(MetadataError(format!(
            "unknown metadata format {}",
            crate::protocol::common::py_repr(&Value::from(other))
        ))),
    }
}

/// What an operator is told when the file transfer failed.
pub fn fetch_error_text(code: u8, message: &str) -> String {
    use crate::protocol::ftp::{FTP_ERR_FILE_NOT_FOUND, FTP_ERR_UNKNOWN_COMMAND};
    if code == FTP_ERR_FILE_NOT_FOUND {
        "this firmware was built without parameter metadata".to_string()
    } else if code == FTP_ERR_UNKNOWN_COMMAND {
        "the autopilot does not support MAVLink FTP".to_string()
    } else {
        message.to_string()
    }
}

fn mtime(path: &Path) -> SystemTime {
    path.metadata()
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Metadata files kept on the ground station, one per vehicle checksum.
///
/// Opt-in (`parameters.cache_defaults` in the config), and only for a stack
/// whose file is fixed when the firmware is built (PX4). A copy is used only
/// when the vehicle reports, over MAVLink FTP, the same CRC32 for its file that
/// it reported when the copy was made; that check costs one round trip where
/// the download costs the whole file.
///
/// What this cannot know is written on the switch that turns it on: a CRC32 is
/// 32 bits, so two different files can share one, and a copy that matches is
/// then the wrong firmware's. The copies are raw files as the vehicle sent them
/// and are parsed exactly like a download; a copy that no longer parses is
/// deleted.
pub struct ParamMetadataCache {
    pub directory: PathBuf,
}

impl ParamMetadataCache {
    pub const MAX_FILES: usize = 12;

    pub fn new(directory: impl Into<PathBuf>) -> ParamMetadataCache {
        ParamMetadataCache {
            directory: directory.into(),
        }
    }

    /// `^[a-z0-9-]+-[0-9a-f]{8}\.bin$`: a file this cache wrote.
    pub fn is_cache_file(name: &str) -> bool {
        // `$` also matches before one trailing newline.
        let name = name.strip_suffix('\n').unwrap_or(name);
        let Some(stem) = name.strip_suffix(".bin") else {
            return false;
        };
        let Some((tag, crc)) = stem.rsplit_once('-') else {
            return false;
        };
        !tag.is_empty()
            && tag
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && crc.len() == 8
            && crc.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
    }

    /// The name of the copy for a format and a checksum.
    pub fn file_name(fmt: &str, crc: u32) -> String {
        let tag: String = fmt
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
            .collect();
        let tag = if tag.is_empty() { "metadata".to_string() } else { tag };
        format!("{tag}-{crc:08x}.bin")
    }

    fn path(&self, fmt: &str, crc: u32) -> PathBuf {
        self.directory.join(Self::file_name(fmt, crc))
    }

    /// The copy for `crc`, or `None`.
    pub fn load(&self, fmt: &str, crc: u32) -> Option<Vec<u8>> {
        let path = self.path(fmt, crc);
        let raw = std::fs::read(&path).ok()?;
        if let Ok(f) = std::fs::File::options().write(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
        (!raw.is_empty()).then_some(raw)
    }

    /// Keeps `raw` for `crc`, atomically; the oldest copies beyond `MAX_FILES` go.
    pub fn store(&self, fmt: &str, crc: u32, raw: &[u8]) {
        let path = self.path(fmt, crc);
        if self.write_atomic(&path, raw).is_err() {
            return;
        }
        let mut files = self.files();
        files.sort_by_key(|p| std::cmp::Reverse(mtime(p)));
        for old in files.into_iter().skip(Self::MAX_FILES) {
            let _ = std::fs::remove_file(old);
        }
    }

    fn write_atomic(&self, path: &Path, raw: &[u8]) -> io::Result<()> {
        std::fs::create_dir_all(&self.directory)?;
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = self.directory.join(format!(
            "{}.{}{nanos}.tmp",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("metadata"),
            std::process::id()
        ));
        let result = std::fs::write(&tmp, raw).and_then(|()| std::fs::rename(&tmp, path));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    pub fn discard(&self, fmt: &str, crc: u32) {
        let _ = std::fs::remove_file(self.path(fmt, crc));
    }

    fn files(&self) -> Vec<PathBuf> {
        let Ok(dir) = std::fs::read_dir(&self.directory) else {
            return Vec::new();
        };
        dir.filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.file_name().and_then(|n| n.to_str()).is_some_and(Self::is_cache_file))
            .collect()
    }

    /// `{"dir", "files", "bytes"}` for the settings page.
    pub fn info(&self) -> Map {
        let files = self.files();
        let size: u64 = files.iter().filter_map(|p| p.metadata().ok()).map(|m| m.len()).sum();
        let mut m = Map::new();
        m.insert("dir".into(), Value::from(self.directory.to_string_lossy().to_string()));
        m.insert("files".into(), Value::from(files.len()));
        m.insert("bytes".into(), Value::from(size));
        m
    }

    /// Deletes every copy; returns how many went.
    pub fn clear(&self) -> usize {
        self.files()
            .into_iter()
            .filter(|p| std::fs::remove_file(p).is_ok())
            .count()
    }
}
