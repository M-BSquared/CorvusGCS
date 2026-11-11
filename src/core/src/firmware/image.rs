//! The firmware container (`firmware_uploader.parse_firmware_image`): the JSON
//! `.px4` and `.apj` file, a ZIP that wraps a binary, or a raw binary.

use std::io::Read;

use base64::Engine;
use flate2::{Decompress, FlushDecompress, Status};
use serde_json::Value;

use super::pyjson;

/// Current flight controller images are a few MiB. 32 MiB leaves room for
/// future boards while stopping a compressed upload from expanding until it
/// exhausts the field laptop's memory.
pub const MAX_FIRMWARE_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// Bootloader board id -> the one other firmware board id it runs
/// (ArduPilot `Tools/scripts/uploader.py` `compatible_IDs`).
const COMPATIBLE_BOARD_IDS: [(i64, i64); 1] = [(33, 9)];

/// A decoded firmware file: the bytes to flash and the board they are for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareImage {
    pub image: Vec<u8>,
    /// From the container; `None` for a raw binary, which names no board.
    pub board_id: Option<i64>,
}

/// May firmware built for `firmware_id` run on a board reporting `bootloader_id`?
pub fn board_ids_compatible(bootloader_id: i64, firmware_id: i64) -> bool {
    bootloader_id == firmware_id
        || COMPATIBLE_BOARD_IDS
            .iter()
            .any(|(b, f)| *b == bootloader_id && *f == firmware_id)
}

fn plain(image: Vec<u8>) -> Result<FirmwareImage, String> {
    Ok(FirmwareImage { image, board_id: None })
}

/// Decodes `data` into the firmware binary and its target board id. The error
/// text is what `ValueError` carries in Python.
pub fn parse_firmware_image(data: &[u8]) -> Result<FirmwareImage, String> {
    if data.is_empty() {
        return Err("empty firmware data".into());
    }
    let start = data
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
        .unwrap_or(data.len());
    let stripped = &data[start..];
    if stripped.first() == Some(&b'{') {
        return parse_json(stripped);
    }
    if data.starts_with(b"PK") {
        return parse_zip(data);
    }
    if data.len() > MAX_FIRMWARE_IMAGE_BYTES {
        return Err("firmware image exceeds size limit".into());
    }
    plain(data.to_vec())
}

fn parse_json(stripped: &[u8]) -> Result<FirmwareImage, String> {
    let text = String::from_utf8_lossy(stripped);
    if let Some(problem) = pyjson::check(&text) {
        return Err(format!("invalid firmware JSON: {problem}"));
    }
    let desc: Value = serde_json::from_str(&text).map_err(|e| format!("invalid firmware JSON: {e}"))?;
    let Some(object) = desc.as_object().filter(|o| o.contains_key("image")) else {
        return Err("firmware JSON has no 'image' field".into());
    };
    let Some(encoded) = object["image"].as_str() else {
        return Err("firmware JSON 'image' must be a base64 string".into());
    };
    let board_id = match object.get("board_id") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => match n.as_u64().filter(|v| *v <= 0xFFFF_FFFF) {
            Some(v) => Some(v as i64),
            None => return Err("firmware JSON 'board_id' must be an unsigned integer".into()),
        },
        Some(_) => return Err("firmware JSON 'board_id' must be an unsigned integer".into()),
    };
    let raw = b64decode(encoded)?;
    let image = inflate(&raw)?;
    if image.is_empty() {
        return Err("firmware image is empty".into());
    }
    Ok(FirmwareImage { image, board_id })
}

/// `base64.b64decode(text.encode("ascii"), validate=True)`.
fn b64decode(text: &str) -> Result<Vec<u8>, String> {
    if let Some((at, c)) = text.chars().enumerate().find(|(_, c)| !c.is_ascii()) {
        let run = text.chars().skip(at).take_while(|c| !c.is_ascii()).count();
        let esc = |c: char| match c as u32 {
            v if v <= 0xFF => format!("\\x{v:02x}"),
            v if v <= 0xFFFF => format!("\\u{v:04x}"),
            v => format!("\\U{v:08x}"),
        };
        return Err(if run == 1 {
            format!(
                "'ascii' codec can't encode character '{}' in position {at}: ordinal not in range(128)",
                esc(c)
            )
        } else {
            format!(
                "'ascii' codec can't encode characters in position {at}-{}: ordinal not in range(128)",
                at + run - 1
            )
        });
    }
    let bytes = text.as_bytes();
    let data_len = bytes
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/'))
        .unwrap_or(bytes.len());
    let pad = bytes.len() - data_len;
    if bytes[data_len..].iter().any(|b| *b != b'=') || pad > 2 {
        return Err("Only base64 data is allowed".into());
    }
    if data_len == 0 && pad > 0 {
        return Err("Leading padding not allowed".into());
    }
    let wanted = match data_len % 4 {
        0 => 0,
        1 => {
            return Err(format!(
                "Invalid base64-encoded string: number of data characters ({data_len}) cannot be 1 more than a multiple of 4"
            ))
        }
        2 => 2,
        _ => 1,
    };
    if pad > wanted {
        return Err("Excess data after padding".into());
    }
    if pad < wanted {
        return Err("Incorrect padding".into());
    }
    let engine = base64::engine::general_purpose::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    engine.decode(text).map_err(|e| e.to_string())
}

/// `zlib.decompressobj().decompress` with the size cap and the checks around it.
fn inflate(raw: &[u8]) -> Result<Vec<u8>, String> {
    let bad = |why: &str| format!("could not decode firmware image: Error -3 while decompressing data: {why}");
    if raw.len() < 2 {
        return Err("firmware image has an incomplete zlib stream".into());
    }
    let (cmf, flg) = (raw[0] as u32, raw[1] as u32);
    if (cmf * 256 + flg) % 31 != 0 {
        return Err(bad("incorrect header check"));
    }
    if cmf & 0x0f != 8 {
        return Err(bad("unknown compression method"));
    }
    if cmf >> 4 > 7 {
        return Err(bad("invalid window size"));
    }
    let mut d = Decompress::new(true);
    let mut out: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let (before_in, before_out) = (d.total_in(), d.total_out());
        let input = &raw[before_in as usize..];
        let status = d
            .decompress(input, &mut buf, FlushDecompress::None)
            .map_err(|_| bad("invalid data"))?;
        let produced = (d.total_out() - before_out) as usize;
        out.extend_from_slice(&buf[..produced]);
        if out.len() > MAX_FIRMWARE_IMAGE_BYTES {
            return Err("decoded firmware image exceeds size limit".into());
        }
        match status {
            Status::StreamEnd => return Ok(out),
            Status::Ok | Status::BufError => {
                if d.total_in() == before_in && produced == 0 {
                    return Err("firmware image has an incomplete zlib stream".into());
                }
            }
        }
    }
}

struct Entry {
    name: String,
    method: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    offset: u64,
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn zip_entries(data: &[u8]) -> Result<Vec<Entry>, String> {
    let not_zip = || "invalid firmware archive: File is not a zip file".to_string();
    let from = data.len().saturating_sub(22 + 65535);
    let tail = &data[from..];
    let eocd = tail
        .windows(4)
        .rposition(|w| w == [0x50, 0x4b, 0x05, 0x06])
        .map(|p| p + from)
        .ok_or_else(not_zip)?;
    let count = le16(data, eocd + 10).ok_or_else(not_zip)? as usize;
    let size = le32(data, eocd + 12).ok_or_else(not_zip)? as usize;
    let offset = le32(data, eocd + 16).ok_or_else(not_zip)? as usize;
    // Bytes in front of the archive shift every offset (a self extracting stub).
    let concat = eocd.saturating_sub(size + offset);
    let mut at = offset + concat;
    let mut out = Vec::new();
    for _ in 0..count {
        let bad = || "invalid firmware archive: Truncated central directory".to_string();
        if le32(data, at).ok_or_else(bad)? != 0x0201_4b50 {
            return Err("invalid firmware archive: Bad magic number for central directory".into());
        }
        let method = le16(data, at + 10).ok_or_else(bad)?;
        let crc = le32(data, at + 16).ok_or_else(bad)?;
        let compressed = le32(data, at + 20).ok_or_else(bad)? as u64;
        let usize_ = le32(data, at + 24).ok_or_else(bad)? as u64;
        let (n, m, k) = (
            le16(data, at + 28).ok_or_else(bad)? as usize,
            le16(data, at + 30).ok_or_else(bad)? as usize,
            le16(data, at + 32).ok_or_else(bad)? as usize,
        );
        let header = le32(data, at + 42).ok_or_else(bad)? as u64;
        let name = data.get(at + 46..at + 46 + n).ok_or_else(bad)?;
        out.push(Entry {
            name: String::from_utf8_lossy(name).into_owned(),
            method,
            crc,
            compressed,
            size: usize_,
            offset: header + concat as u64,
        });
        at += 46 + n + m + k;
    }
    Ok(out)
}

fn zip_read(data: &[u8], e: &Entry) -> Result<Vec<u8>, String> {
    let bad = || "invalid firmware archive: Bad magic number for file header".to_string();
    let at = e.offset as usize;
    if le32(data, at).ok_or_else(bad)? != 0x0403_4b50 {
        return Err(bad());
    }
    let (n, m) = (
        le16(data, at + 26).ok_or_else(bad)? as usize,
        le16(data, at + 28).ok_or_else(bad)? as usize,
    );
    let start = at + 30 + n + m;
    let raw = data.get(start..start + e.compressed as usize).ok_or_else(bad)?;
    let body = match e.method {
        0 => raw.to_vec(),
        8 => {
            let mut out = Vec::new();
            flate2::read::DeflateDecoder::new(raw)
                .take(e.size + 1)
                .read_to_end(&mut out)
                .map_err(|e| format!("Error -3 while decompressing data: {e}"))?;
            out
        }
        _ => return Err("That compression method is not supported".into()),
    };
    let mut crc = flate2::Crc::new();
    crc.update(&body);
    if crc.sum() != e.crc {
        return Err(format!("Bad CRC-32 for file '{}'", e.name));
    }
    Ok(body)
}

fn parse_zip(data: &[u8]) -> Result<FirmwareImage, String> {
    let entries = zip_entries(data)?;
    if entries.is_empty() {
        return Err("firmware archive is empty".into());
    }
    // `ZipFile.getinfo`: the last entry of a name wins.
    let by_name = |name: &str| entries.iter().rfind(|e| e.name == name);
    for preferred in ["firmware.px4", "image.px4"] {
        if let Some(e) = by_name(preferred) {
            if e.size > MAX_FIRMWARE_IMAGE_BYTES as u64 {
                return Err("firmware archive image exceeds size limit".into());
            }
            let blob = zip_read(data, e)?;
            if blob.is_empty() {
                return Err(format!("firmware archive entry '{preferred}' is empty"));
            }
            return plain(blob);
        }
    }
    let wanted: Vec<&Entry> = entries
        .iter()
        .filter(|e| {
            let n = e.name.to_lowercase();
            n.ends_with(".px4") || n.ends_with(".bin")
        })
        .collect();
    let pool: Vec<&Entry> = if wanted.is_empty() {
        entries.iter().collect()
    } else {
        wanted
    };
    let mut best = pool[0];
    for e in &pool {
        let e = by_name(&e.name).unwrap_or(e);
        if e.size > best.size {
            best = e;
        }
    }
    let best = by_name(&best.name).unwrap_or(best);
    if best.size > MAX_FIRMWARE_IMAGE_BYTES as u64 {
        return Err("firmware archive image exceeds size limit".into());
    }
    let blob = zip_read(data, best)?;
    if blob.is_empty() {
        return Err("firmware archive contains an empty image".into());
    }
    plain(blob)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_one_compatible_pair() {
        assert!(board_ids_compatible(33, 9));
        assert!(!board_ids_compatible(9, 33));
        assert!(board_ids_compatible(50, 50));
    }
}
