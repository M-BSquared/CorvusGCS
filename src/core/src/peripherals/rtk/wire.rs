//! RTCM 3 framing and the UBX messages that make a u-blox receiver a base
//! station: pure functions over bytes (`rtk.py`, the wire half).
//!
//! The keys, scalings and the survey-in state machine's terms are those of the
//! PX4 GPS driver (`PX4-GPSDrivers/src/ubx.h`), named as there. The
//! GPS_RTCM_DATA fragmenting is `protocol::rtcm`.

use serde_json::{json, Value};

use crate::protocol::common::{py_g, py_round};

pub const RTCM_PREAMBLE: u8 = 0xD3;
pub const RTCM_MAX_PAYLOAD: usize = 1023;
pub const MAX_FRAME: usize = 3 + RTCM_MAX_PAYLOAD + 3;
const MAX_BUFFER: usize = MAX_FRAME * 2;

pub const UBX_CLASS_NAV: u8 = 0x01;
pub const UBX_CLASS_CFG: u8 = 0x06;
pub const UBX_CLASS_MON: u8 = 0x0A;
pub const UBX_CLASS_RTCM3: u8 = 0xF5;
pub const UBX_ID_NAV_SVIN: u8 = 0x3B;
pub const UBX_ID_CFG_MSG: u8 = 0x01;
pub const UBX_ID_CFG_TMODE3: u8 = 0x71;
pub const UBX_ID_CFG_VALSET: u8 = 0x8A;
pub const UBX_ID_MON_VER: u8 = 0x04;
pub const UBX_LAYER_RAM: u8 = 0x01;
const UBX_MAX_PAYLOAD: usize = 4096;

pub const KEY_TMODE_MODE: u32 = 0x20030001;
pub const KEY_TMODE_POS_TYPE: u32 = 0x20030002;
pub const KEY_TMODE_LAT: u32 = 0x40030009;
pub const KEY_TMODE_LON: u32 = 0x4003000A;
pub const KEY_TMODE_HEIGHT: u32 = 0x4003000B;
pub const KEY_TMODE_LAT_HP: u32 = 0x2003000C;
pub const KEY_TMODE_LON_HP: u32 = 0x2003000D;
pub const KEY_TMODE_HEIGHT_HP: u32 = 0x2003000E;
pub const KEY_TMODE_FIXED_POS_ACC: u32 = 0x4003000F;
pub const KEY_TMODE_SVIN_MIN_DUR: u32 = 0x40030010;
pub const KEY_TMODE_SVIN_ACC_LIMIT: u32 = 0x40030011;
pub const KEY_RATE_MEAS: u32 = 0x30210001;
pub const KEY_MSGOUT_NAV_SVIN_I2C: u32 = 0x20910088;
pub const KEY_MSGOUT_RTCM_1005_I2C: u32 = 0x209102BD;
pub const RTCM_MSM4_KEYS: [u32; 6] = [0x209102BD, 0x2091035E, 0x20910363, 0x20910368, 0x2091036D, 0x20910303];
pub const RTCM_MSM7_KEYS: [u32; 6] = [0x209102BD, 0x209102CC, 0x209102D1, 0x20910318, 0x209102D6, 0x20910303];
const LEGACY_RTCM_MSM4: [u8; 6] = [0x05, 0x4A, 0x54, 0x5E, 0x7C, 0xE6];
const LEGACY_RTCM_MSM7: [u8; 6] = [0x05, 0x4D, 0x57, 0x61, 0x7F, 0xE6];
pub const RTCM_1005_RATE: i64 = 5;

/// Baud rates a base station is found at, most likely first.
pub const BAUD_CANDIDATES: [u32; 6] = [38400, 9600, 115200, 57600, 19200, 460800];

const fn crc24q_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut byte = 0;
    while byte < 256 {
        let mut reg: u32 = (byte as u32) << 16;
        let mut i = 0;
        while i < 8 {
            reg = if reg & 0x800000 != 0 {
                (reg << 1) ^ 0x1864CFB
            } else {
                reg << 1
            };
            i += 1;
        }
        table[byte] = reg & 0xFFFFFF;
        byte += 1;
    }
    table
}

static CRC24Q_TABLE: [u32; 256] = crc24q_table();

/// The CRC-24Q of `data`.
pub fn crc24q(data: &[u8]) -> u32 {
    let mut reg: u32 = 0;
    for &byte in data {
        reg = ((reg << 8) & 0xFFFFFF) ^ CRC24Q_TABLE[(((reg >> 16) as u8) ^ byte) as usize];
    }
    reg
}

/// The RTCM message type of a complete frame, or 0.
pub fn message_number(frame: &[u8]) -> u32 {
    if frame.len() < 6 {
        return 0;
    }
    ((frame[3] as u32) << 4) | ((frame[4] as u32) >> 4)
}

/// Byte stream in, complete CRC valid RTCM 3 frames out; resynchronises one
/// byte at a time and never holds more than two frames' worth.
#[derive(Default)]
pub struct RtcmFramer {
    buf: Vec<u8>,
    pub frames: i64,
    pub crc_errors: i64,
    pub discarded: i64,
}

impl RtcmFramer {
    pub fn new() -> RtcmFramer {
        RtcmFramer::default()
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            let Some(start) = self.buf.iter().position(|&b| b == RTCM_PREAMBLE) else {
                let keep = self.buf.len().min(2);
                self.discarded += (self.buf.len() - keep) as i64;
                self.buf.drain(..self.buf.len() - keep);
                break;
            };
            if start > 0 {
                self.discarded += start as i64;
                self.buf.drain(..start);
            }
            if self.buf.len() < 3 {
                break;
            }
            let length = (((self.buf[1] & 0x03) as usize) << 8) | self.buf[2] as usize;
            let total = 3 + length + 3;
            if self.buf.len() < total {
                break;
            }
            let expected =
                ((self.buf[total - 3] as u32) << 16) | ((self.buf[total - 2] as u32) << 8) | self.buf[total - 1] as u32;
            if crc24q(&self.buf[..total - 3]) == expected {
                out.push(self.buf.drain(..total).collect());
                self.frames += 1;
                continue;
            }
            self.crc_errors += 1;
            self.discarded += 1;
            self.buf.drain(..1);
        }
        if self.buf.len() > MAX_BUFFER {
            let over = self.buf.len() - MAX_BUFFER;
            self.discarded += over as i64;
            self.buf.drain(..over);
        }
        out
    }

    pub fn reset(&mut self) {
        self.buf.clear();
    }

    pub fn held(&self) -> usize {
        self.buf.len()
    }
}

fn checksum(payload: &[u8]) -> (u8, u8) {
    let (mut a, mut b) = (0u8, 0u8);
    for &byte in payload {
        a = a.wrapping_add(byte);
        b = b.wrapping_add(a);
    }
    (a, b)
}

/// One complete UBX message, ready to write to the port.
pub fn ubx_frame(class: u8, id: u8, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![class, id];
    body.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    body.extend_from_slice(payload);
    let (a, b) = checksum(&body);
    let mut out = vec![0xB5, 0x62];
    out.extend(body);
    out.extend([a, b]);
    out
}

/// A zero length UBX message, which is how one is polled for.
pub fn poll_frame(class: u8, id: u8) -> Vec<u8> {
    ubx_frame(class, id, &[])
}

/// Byte stream in, `(class, id, payload)` out, checksum valid only.
#[derive(Default)]
pub struct UbxParser {
    buf: Vec<u8>,
}

impl UbxParser {
    pub fn new() -> UbxParser {
        UbxParser::default()
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<(u8, u8, Vec<u8>)> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            let Some(start) = self.buf.windows(2).position(|w| w == [0xB5, 0x62]) else {
                let keep = self.buf.len().min(1);
                self.buf.drain(..self.buf.len() - keep);
                break;
            };
            if start > 0 {
                self.buf.drain(..start);
            }
            if self.buf.len() < 6 {
                break;
            }
            let length = u16::from_le_bytes([self.buf[4], self.buf[5]]) as usize;
            if length > UBX_MAX_PAYLOAD {
                self.buf.drain(..2);
                continue;
            }
            let total = 6 + length + 2;
            if self.buf.len() < total {
                break;
            }
            let (a, b) = checksum(&self.buf[2..6 + length]);
            if (a, b) == (self.buf[total - 2], self.buf[total - 1]) {
                out.push((self.buf[2], self.buf[3], self.buf[6..6 + length].to_vec()));
                self.buf.drain(..total);
                continue;
            }
            self.buf.drain(..2);
        }
        out
    }

    pub fn reset(&mut self) {
        self.buf.clear();
    }
}

/// `value` at the width the key's size bits declare, little endian.
fn value_bytes(key: u32, value: i64) -> Vec<u8> {
    let width = match (key >> 28) & 0x07 {
        0..=2 => 1,
        3 => 2,
        _ => 4,
    };
    let v = (value as u64) & ((1u64 << (8 * width)) - 1);
    v.to_le_bytes()[..width].to_vec()
}

/// A CFG-VALSET message carrying `items` as `(key, value)` pairs, into RAM only.
pub fn valset(items: &[(u32, i64)]) -> Vec<u8> {
    let mut payload = vec![0x00, UBX_LAYER_RAM, 0x00, 0x00];
    for &(key, value) in items {
        payload.extend_from_slice(&key.to_le_bytes());
        payload.extend(value_bytes(key, value));
    }
    ubx_frame(UBX_CLASS_CFG, UBX_ID_CFG_VALSET, &payload)
}

fn msgout(keys: &[u32], rate: i64) -> Vec<(u32, i64)> {
    // UART1 (+1) and USB (+3): the ports a base is plugged in by.
    keys.iter().flat_map(|&k| [k + 1, k + 3]).map(|k| (k, rate)).collect()
}

fn key_set(msm7: bool) -> &'static [u32; 6] {
    if msm7 {
        &RTCM_MSM7_KEYS
    } else {
        &RTCM_MSM4_KEYS
    }
}

/// Does this receiver take CFG-VALSET rather than the CFG-* structs?
pub fn supports_valset(version: &Value) -> bool {
    let Some(obj) = version.as_object().filter(|o| !o.is_empty()) else {
        return false;
    };
    match obj.get("protocol").and_then(Value::as_f64) {
        Some(p) => p >= 27.0,
        None => false,
    }
}

/// Silence the RTCM output before the time mode is changed.
pub fn stop_output_frames(msm7: bool) -> Vec<Vec<u8>> {
    vec![valset(&msgout(key_set(msm7), 0))]
}

/// `max(0, int(round(x * 10000.0)))`: metres to 0.1 mm.
fn tenth_mm(x: f64) -> i64 {
    ((x * 10000.0).round_ties_even() as i64).max(0)
}

/// Configure survey in: stop the old one, then start the new one.
pub fn survey_in_frames(duration_s: i64, accuracy_m: f64, msm7: bool) -> Vec<Vec<u8>> {
    let mut frames = stop_output_frames(msm7);
    frames.push(valset(&[(KEY_TMODE_MODE, 0)]));
    let mut items = vec![
        (KEY_TMODE_MODE, 1),
        (KEY_TMODE_SVIN_MIN_DUR, duration_s.max(0)),
        (KEY_TMODE_SVIN_ACC_LIMIT, tenth_mm(accuracy_m)),
    ];
    items.extend(msgout(&[KEY_MSGOUT_NAV_SVIN_I2C], 5));
    frames.push(valset(&items));
    frames
}

/// A coarse count and a [-99, 99] high precision remainder.
pub fn hp_split(value: f64, scale: f64) -> (i64, i64) {
    let fine = (value * scale).round_ties_even() as i64;
    let coarse = ((fine as f64) / 100.0).trunc() as i64;
    (coarse, fine - coarse * 100)
}

/// Configure a base at a position the operator already knows (CFG-VALSET).
/// `altitude` is metres above the ellipsoid, not AMSL.
pub fn fixed_position_frames(lat: f64, lon: f64, alt: f64, accuracy_m: f64, msm7: bool) -> Vec<Vec<u8>> {
    let (la, la_hp) = hp_split(lat, 1e9);
    let (lo, lo_hp) = hp_split(lon, 1e9);
    let (h, h_hp) = hp_split(alt, 1e4);
    let mut frames = stop_output_frames(msm7);
    frames.push(valset(&[
        (KEY_TMODE_MODE, 2),
        (KEY_TMODE_POS_TYPE, 1),
        (KEY_TMODE_LAT, la),
        (KEY_TMODE_LAT_HP, la_hp),
        (KEY_TMODE_LON, lo),
        (KEY_TMODE_LON_HP, lo_hp),
        (KEY_TMODE_HEIGHT, h),
        (KEY_TMODE_HEIGHT_HP, h_hp),
        (KEY_TMODE_FIXED_POS_ACC, tenth_mm(accuracy_m)),
    ]));
    frames
}

/// Start the RTCM stream and stop the survey reports.
pub fn activate_output_frames(msm7: bool, reduce_rate: bool) -> Vec<Vec<u8>> {
    let mut items = Vec::new();
    if reduce_rate {
        items.push((KEY_RATE_MEAS, 1000));
    }
    items.extend(msgout(&[KEY_MSGOUT_RTCM_1005_I2C], RTCM_1005_RATE));
    let observations: Vec<u32> = key_set(msm7)
        .iter()
        .copied()
        .filter(|&k| k != KEY_MSGOUT_RTCM_1005_I2C)
        .collect();
    items.extend(msgout(&observations, 1));
    items.extend(msgout(&[KEY_MSGOUT_NAV_SVIN_I2C], 0));
    vec![valset(&items)]
}

/// Put the receiver back the way it was found.
pub fn disable_base_frames(msm7: bool) -> Vec<Vec<u8>> {
    let mut items = msgout(key_set(msm7), 0);
    items.extend(msgout(&[KEY_MSGOUT_NAV_SVIN_I2C], 0));
    items.push((KEY_TMODE_MODE, 0));
    vec![valset(&items)]
}

fn cfg_msg(class: u8, id: u8, rate: u8) -> Vec<u8> {
    ubx_frame(UBX_CLASS_CFG, UBX_ID_CFG_MSG, &[class, id, rate])
}

fn legacy_set(msm7: bool) -> &'static [u8; 6] {
    if msm7 {
        &LEGACY_RTCM_MSM7
    } else {
        &LEGACY_RTCM_MSM4
    }
}

fn legacy_silence(msm7: bool) -> Vec<Vec<u8>> {
    legacy_set(msm7)
        .iter()
        .map(|&id| cfg_msg(UBX_CLASS_RTCM3, id, 0))
        .collect()
}

/// Silence an M8P generation receiver's RTCM output.
pub fn legacy_stop_output_frames(msm7: bool) -> Vec<Vec<u8>> {
    legacy_silence(msm7)
}

/// One CFG-TMODE3 message: 40 bytes in the order the receiver reads them.
#[allow(clippy::too_many_arguments)]
fn tmode3(
    flags: u16,
    lat: i32,
    lon: i32,
    alt: i32,
    lat_hp: i8,
    lon_hp: i8,
    alt_hp: i8,
    fixed_pos_acc: u32,
    svin_min_dur: u32,
    svin_acc_limit: u32,
) -> Vec<u8> {
    let mut payload = vec![0u8, 0u8];
    payload.extend_from_slice(&flags.to_le_bytes());
    for v in [lat, lon, alt] {
        payload.extend_from_slice(&v.to_le_bytes());
    }
    for v in [lat_hp, lon_hp, alt_hp] {
        payload.push(v as u8);
    }
    payload.push(0);
    for v in [fixed_pos_acc, svin_min_dur, svin_acc_limit] {
        payload.extend_from_slice(&v.to_le_bytes());
    }
    payload.extend_from_slice(&[0u8; 8]);
    ubx_frame(UBX_CLASS_CFG, UBX_ID_CFG_TMODE3, &payload)
}

fn tmode3_off() -> Vec<u8> {
    tmode3(0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
}

/// CFG-TMODE3 survey in, for an M8P generation receiver.
pub fn legacy_survey_in_frames(duration_s: i64, accuracy_m: f64, msm7: bool) -> Vec<Vec<u8>> {
    let mut frames = legacy_silence(msm7);
    frames.push(tmode3_off());
    frames.push(tmode3(
        1,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        duration_s.max(0) as u32,
        tenth_mm(accuracy_m) as u32,
    ));
    frames.push(cfg_msg(UBX_CLASS_NAV, UBX_ID_NAV_SVIN, 5));
    frames
}

/// CFG-TMODE3 fixed position, for an M8P generation receiver.
pub fn legacy_fixed_position_frames(lat: f64, lon: f64, alt: f64, accuracy_m: f64, msm7: bool) -> Vec<Vec<u8>> {
    let (la, la_hp) = hp_split(lat, 1e9);
    let (lo, lo_hp) = hp_split(lon, 1e9);
    let (h, h_hp) = hp_split(alt, 1e4);
    let mut frames = legacy_silence(msm7);
    frames.push(tmode3_off());
    frames.push(tmode3(
        2 | (1 << 8),
        la as i32,
        lo as i32,
        h as i32,
        la_hp as i8,
        lo_hp as i8,
        h_hp as i8,
        tenth_mm(accuracy_m) as u32,
        0,
        0,
    ));
    frames
}

/// Start the RTCM stream on an M8P generation receiver.
pub fn legacy_activate_output_frames(msm7: bool) -> Vec<Vec<u8>> {
    let mut frames = vec![cfg_msg(UBX_CLASS_NAV, UBX_ID_NAV_SVIN, 0)];
    for &id in legacy_set(msm7) {
        let rate = if id == 0x05 { RTCM_1005_RATE as u8 } else { 1 };
        frames.push(cfg_msg(UBX_CLASS_RTCM3, id, rate));
    }
    frames
}

/// Take an M8P generation receiver back out of base mode.
pub fn legacy_disable_base_frames(msm7: bool) -> Vec<Vec<u8>> {
    let mut frames = legacy_silence(msm7);
    frames.push(cfg_msg(UBX_CLASS_NAV, UBX_ID_NAV_SVIN, 0));
    frames.push(tmode3_off());
    frames
}

/// NAV-SVIN: how the survey is going, in the operator's units.
pub fn parse_nav_svin(p: &[u8]) -> Option<Value> {
    if p.len() < 40 {
        return None;
    }
    let u32_at = |o: usize| u32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]]);
    let i32_at = |o: usize| i32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]]);
    Some(json!({
        "duration": u32_at(8),
        "accuracy": py_round(u32_at(28) as f64 / 10000.0, 4),
        "observations": u32_at(32),
        "valid": p[36] != 0,
        "active": p[37] != 0,
        "ecef": [i32_at(12), i32_at(16), i32_at(20)],
    }))
}

fn is_py_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\x1c'..='\x1f' | ' ')
}

/// `raw.split(b"\0", 1)[0].decode("ascii", errors="replace").strip()`.
fn ascii_text(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let text: String = raw[..end]
        .iter()
        .map(|&b| if b < 0x80 { b as char } else { '\u{FFFD}' })
        .collect();
    text.trim_matches(is_py_space).to_string()
}

/// MON-VER: who answered, and which protocol they speak.
pub fn parse_mon_ver(payload: &[u8]) -> Value {
    if payload.len() < 40 {
        return json!({"software": "", "hardware": "", "extensions": [], "protocol": 0.0, "model": ""});
    }
    let software = ascii_text(&payload[0..30]);
    let hardware = ascii_text(&payload[30..40]);
    let mut extensions = Vec::new();
    let mut offset = 40;
    while offset + 30 <= payload.len() {
        extensions.push(ascii_text(&payload[offset..offset + 30]));
        offset += 30;
    }
    let mut protocol = 0.0f64;
    let mut model = String::new();
    for line in &extensions {
        if line.starts_with("PROTVER") {
            let after = line.split_once('=').map(|(_, r)| r).unwrap_or(line.as_str());
            let mut text = after.trim_matches(is_py_space).to_string();
            if text.is_empty() {
                text = line
                    .chars()
                    .skip(7)
                    .collect::<String>()
                    .trim_matches(is_py_space)
                    .to_string();
            }
            protocol = text.parse().unwrap_or(0.0);
        } else if let Some(rest) = line.strip_prefix("MOD=") {
            model = rest.trim_matches(is_py_space).to_string();
        }
    }
    json!({
        "software": software,
        "hardware": hardware,
        "extensions": extensions,
        "protocol": protocol,
        "model": model,
    })
}

/// One line naming the receiver, for the page's status row.
pub fn receiver_label(version: &Value) -> String {
    let Some(obj) = version.as_object().filter(|o| !o.is_empty()) else {
        return String::new();
    };
    let field = |name: &str| -> String {
        match obj.get(name) {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string().trim().to_string(),
        }
    };
    let (model, software) = (field("model"), field("software"));
    let mut parts = vec![if model.is_empty() {
        "u-blox receiver".to_string()
    } else {
        format!("u-blox {model}")
    }];
    if !software.is_empty() {
        parts.push(software);
    }
    if let Some(p) = obj.get("protocol").and_then(Value::as_f64).filter(|p| *p != 0.0) {
        parts.push(format!("protocol {}", py_g(p)));
    }
    parts.join(" \u{b7} ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(number: u16, body: &[u8]) -> Vec<u8> {
        let mut payload = vec![(number >> 4) as u8, ((number & 0x0F) << 4) as u8];
        payload.extend_from_slice(body);
        let mut out = vec![0xD3, (payload.len() >> 8) as u8 & 0x03, payload.len() as u8];
        out.extend(payload);
        let crc = crc24q(&out);
        out.extend_from_slice(&[(crc >> 16) as u8, (crc >> 8) as u8, crc as u8]);
        out
    }

    #[test]
    fn crc24q_matches_the_published_check_value() {
        assert_eq!(crc24q(b"123456789"), 0xCDE703);
        assert_eq!(crc24q(b""), 0);
    }

    #[test]
    fn the_message_number_is_the_first_twelve_bits() {
        assert_eq!(message_number(&frame(1005, &[0; 4])), 1005);
        assert_eq!(message_number(&frame(1230, &[0; 4])), 1230);
        assert_eq!(message_number(&[0xD3, 0, 1]), 0);
    }

    #[test]
    fn the_framer_finds_a_frame_however_the_stream_is_cut() {
        let wire: Vec<u8> = [frame(1005, &[1; 19]), frame(1074, &[2; 300]), frame(1230, &[3; 5])].concat();
        for chunk in [1usize, 2, 3, 7, 64, 500, wire.len()] {
            let mut framer = RtcmFramer::new();
            let mut got = Vec::new();
            for piece in wire.chunks(chunk) {
                got.extend(framer.feed(piece));
            }
            assert_eq!(got.len(), 3, "chunk size {chunk}");
            assert_eq!(framer.frames, 3);
            assert_eq!(got.concat(), wire);
        }
    }

    #[test]
    fn the_framer_resynchronises_past_noise_and_a_corrupt_frame() {
        let good = frame(1074, &[9; 40]);
        let mut bad = frame(1005, &[7; 19]);
        let last = bad.len() - 1;
        bad[last] ^= 0xFF;
        let wire: Vec<u8> = [b"junk, not a frame".to_vec(), bad, good.clone()].concat();
        let mut framer = RtcmFramer::new();
        assert_eq!(framer.feed(&wire), vec![good]);
        assert!(framer.crc_errors >= 1 && framer.discarded > 0);
        let mut framer = RtcmFramer::new();
        framer.feed(&vec![0xD3u8; 20_000]);
        assert!(framer.held() <= MAX_BUFFER);
    }

    #[test]
    fn ubx_messages_carry_their_checksum_and_split_reads_parse() {
        assert_eq!(
            poll_frame(UBX_CLASS_MON, UBX_ID_MON_VER),
            [0xB5, 0x62, 0x0A, 0x04, 0x00, 0x00, 0x0E, 0x34]
        );
        let good = ubx_frame(UBX_CLASS_NAV, UBX_ID_NAV_SVIN, &[1, 2, 3]);
        let mut bad = ubx_frame(UBX_CLASS_CFG, 0x8A, &[9; 4]);
        let last = bad.len() - 1;
        bad[last] ^= 0x01;
        let wire: Vec<u8> = [vec![0x00, 0xB5], bad, good].concat();
        let mut parser = UbxParser::new();
        let mut got = Vec::new();
        for piece in wire.chunks(3) {
            got.extend(parser.feed(piece));
        }
        assert_eq!(got, vec![(UBX_CLASS_NAV, UBX_ID_NAV_SVIN, vec![1, 2, 3])]);
    }

    #[test]
    fn a_value_has_the_width_its_key_declares() {
        assert_eq!(value_bytes(KEY_TMODE_MODE, 1), [1]);
        assert_eq!(value_bytes(KEY_RATE_MEAS, 1000), [0xE8, 0x03]);
        assert_eq!(value_bytes(KEY_TMODE_LAT, -1), [0xFF; 4]);
        assert_eq!(hp_split(47.123456789, 1e9), (471234567, 89));
        assert_eq!(hp_split(-47.123456789, 1e9), (-471234567, -89));
    }

    #[test]
    fn the_receiver_label_reads_as_the_page_wants_it() {
        let mut payload = vec![0u8; 40];
        payload[..9].copy_from_slice(b"EXT CORE ");
        for ext in ["PROTVER=27.31", "MOD=ZED-F9P"] {
            let mut e = ext.as_bytes().to_vec();
            e.resize(30, 0);
            payload.extend(e);
        }
        let v = parse_mon_ver(&payload);
        assert_eq!(
            receiver_label(&v),
            "u-blox ZED-F9P \u{b7} EXT CORE \u{b7} protocol 27.31"
        );
        assert!(supports_valset(&v));
        assert!(!supports_valset(&json!({"protocol": 15.0})));
        assert_eq!(receiver_label(&json!(null)), "");
    }
}
