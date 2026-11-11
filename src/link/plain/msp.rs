//! MSP: what a Betaflight flight controller tells a configurator, read only.
//!
//! Betaflight takes no parameters or commands over MAVLink. It is configured
//! over MSP, the MultiWii Serial Protocol, on the controller's USB port, which
//! is not the UART that carries the MAVLink telemetry. This module speaks
//! enough of it to *read* a controller: firmware and board, sensors, the PID
//! and rate profile in use, the battery. Nothing here writes to it.
//!
//! A read is one short session on one port: open, ask a handful of questions,
//! close. There is no thread and no handle that outlives the call.
//!
//! Wire format (MSP v1, which every Betaflight 4.x answers): a request is
//! `$M<` + length + command + payload + checksum, the answer `$M>` (or `$M!`
//! for a command the firmware does not know), and the checksum is the XOR of
//! the length, command and payload bytes. Multi-byte fields are little-endian.
//! Held to `python/corvus/msp.py` by `src/core/tests/data/msp_cases.json`.

use std::io;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

pub const DEFAULT_BAUD: u32 = 115_200;
/// A USB serial port ignores the baud rate. A UART does not, and 115200 is the
/// Betaflight default for an MSP port.
pub const BAUD_CANDIDATES: [u32; 8] = [115_200, 57_600, 230_400, 460_800, 921_600, 38_400, 19_200, 9_600];

pub const MSP_API_VERSION: u8 = 1;
pub const MSP_FC_VARIANT: u8 = 2;
pub const MSP_FC_VERSION: u8 = 3;
pub const MSP_BOARD_INFO: u8 = 4;
pub const MSP_BUILD_INFO: u8 = 5;
pub const MSP_NAME: u8 = 10;
pub const MSP_STATUS_EX: u8 = 150;
pub const MSP_RC_TUNING: u8 = 111;
pub const MSP_PID: u8 = 112;
pub const MSP_BATTERY_STATE: u8 = 130;

pub const MAX_PAYLOAD: usize = 255;
pub const REPLY_TIMEOUT_S: f64 = 0.8;
pub const FIRST_REPLY_TIMEOUT_S: f64 = 1.5;
pub const READ_CHUNK: usize = 256;
pub const RETRIES: usize = 1;

const VARIANTS: [(&str, &str); 6] = [
    ("BTFL", "Betaflight"),
    ("CLFL", "Cleanflight"),
    ("EMUF", "EmuFlight"),
    ("INAV", "iNav"),
    ("KISS", "KISS"),
    ("RACE", "RaceFlight"),
];

const SENSOR_BITS: [(u16, &str); 6] = [
    (1 << 5, "gyro"),
    (1, "accelerometer"),
    (1 << 1, "barometer"),
    (1 << 2, "magnetometer"),
    (1 << 3, "gps"),
    (1 << 4, "rangefinder"),
];

const PID_AXES: [&str; 3] = ["roll", "pitch", "yaw"];
/// Betaflight orders MSP_PID roll, pitch, yaw, altitude, position, position
/// rate, navigation rate, level, mag, velocity. Level carries the angle mode
/// strength, the one besides the axes a pilot tunes.
const PID_LEVEL_INDEX: usize = 7;

/// A read could not be completed, with the reason an operator can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MspError {
    /// The controller, the port or the answer is the problem (a 409 upstream).
    Msp(String),
    /// Anything else the serial layer raised (a 500 upstream).
    Other(String),
}

impl MspError {
    fn msp(text: &str) -> MspError {
        MspError::Msp(text.to_string())
    }
}

impl std::fmt::Display for MspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MspError::Msp(t) | MspError::Other(t) => f.write_str(t),
        }
    }
}

impl std::error::Error for MspError {}

/// One checked reply. `error` is true for `$M!`, a command not understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MspFrame {
    pub command: u8,
    pub payload: Vec<u8>,
    pub error: bool,
}

/// Encode an MSP v1 request.
pub fn build_request(command: i64, payload: &[u8]) -> Result<Vec<u8>, String> {
    if !(0..=0xFF).contains(&command) {
        return Err("MSP v1 commands are one byte".into());
    }
    if payload.len() > MAX_PAYLOAD {
        return Err("MSP v1 payload is at most 255 bytes".into());
    }
    let mut body = vec![payload.len() as u8, command as u8];
    body.extend_from_slice(payload);
    let checksum = body.iter().fold(0u8, |a, b| a ^ b);
    let mut out = b"$M<".to_vec();
    out.extend(body);
    out.push(checksum);
    Ok(out)
}

/// Turns a byte stream into reply frames, resynchronising on garbage.
///
/// A port may hand over the tail of an earlier frame, a CLI banner or a
/// MAVLink burst when it is the wrong port. Anything that is not a well formed
/// `$M>` or `$M!` frame with a matching checksum costs one byte of resync,
/// never an error.
#[derive(Debug, Default)]
pub struct MspParser {
    buf: Vec<u8>,
    pub frames_ok: u64,
    pub checksum_errors: u64,
}

impl MspParser {
    pub fn new() -> MspParser {
        MspParser::default()
    }

    /// Add `data` and return every complete frame it finished.
    pub fn feed(&mut self, data: &[u8]) -> Vec<MspFrame> {
        self.buf.extend_from_slice(data);
        let mut frames = Vec::new();
        loop {
            let Some(start) = self.buf.windows(2).position(|w| w == b"$M") else {
                let keep = self.buf.len().saturating_sub(1);
                self.buf.drain(..keep);
                break;
            };
            if start > 0 {
                self.buf.drain(..start);
            }
            if self.buf.len() < 3 {
                break;
            }
            if self.buf[2] != b'>' && self.buf[2] != b'!' {
                self.buf.remove(0);
                continue;
            }
            if self.buf.len() < 5 {
                break;
            }
            let total = 6 + self.buf[3] as usize;
            if self.buf.len() < total {
                break;
            }
            let checksum = self.buf[3..total - 1].iter().fold(0u8, |a, b| a ^ b);
            if checksum != self.buf[total - 1] {
                self.checksum_errors += 1;
                self.buf.remove(0);
                continue;
            }
            frames.push(MspFrame {
                command: self.buf[4],
                payload: self.buf[5..total - 1].to_vec(),
                error: self.buf[2] == b'!',
            });
            self.frames_ok += 1;
            self.buf.drain(..total);
        }
        frames
    }
}

/// Python's `str.strip` for the ASCII it sees here.
fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_ascii_whitespace() || ('\x1c'..='\x1f').contains(&c) || c == '\x0b')
}

fn ascii_replace(data: &[u8]) -> String {
    data.iter()
        .map(|&b| if b < 0x80 { b as char } else { '\u{FFFD}' })
        .collect()
}

fn text(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    py_strip(&ascii_replace(&data[..end])).to_string()
}

fn slice(data: &[u8], from: usize, to: usize) -> &[u8] {
    let to = to.min(data.len());
    if from >= to {
        &[]
    } else {
        &data[from..to]
    }
}

fn u16le(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

type Decoded = Result<Map<String, Value>, MspError>;

fn one(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.to_string(), value);
    m
}

fn decode_api_version(p: &[u8]) -> Decoded {
    if p.len() < 3 {
        return Err(MspError::msp("API version reply is too short"));
    }
    let mut m = Map::new();
    m.insert("protocol".into(), json!(p[0]));
    m.insert("api".into(), json!(format!("{}.{}", p[1], p[2])));
    Ok(m)
}

fn decode_variant(p: &[u8]) -> Decoded {
    let code = text(slice(p, 0, 4));
    let name = VARIANTS
        .iter()
        .find(|(c, _)| *c == code)
        .map_or(code.clone(), |(_, n)| n.to_string());
    let mut m = Map::new();
    m.insert("variant".into(), json!(code));
    m.insert("variant_name".into(), json!(name));
    Ok(m)
}

fn decode_version(p: &[u8]) -> Decoded {
    if p.len() < 3 {
        return Err(MspError::msp("firmware version reply is too short"));
    }
    Ok(one("version", json!(format!("{}.{}.{}", p[0], p[1], p[2]))))
}

fn decode_board_info(p: &[u8]) -> Decoded {
    if p.len() < 6 {
        return Err(MspError::msp("board info reply is too short"));
    }
    let mut m = Map::new();
    m.insert("board".into(), json!(text(slice(p, 0, 4))));
    m.insert("hardware_revision".into(), json!(u16le(p, 4)));
    let mut target = String::new();
    if p.len() >= 9 {
        let name_len = p[8] as usize;
        target = text(slice(p, 9, 9 + name_len));
    }
    m.insert("target".into(), json!(target));
    Ok(m)
}

fn decode_build_info(p: &[u8]) -> Decoded {
    if p.len() < 19 {
        return Err(MspError::msp("build info reply is too short"));
    }
    Ok(one(
        "build",
        json!({
            "date": text(slice(p, 0, 11)),
            "time": text(slice(p, 11, 19)),
            "revision": text(slice(p, 19, 26)),
        }),
    ))
}

fn decode_name(p: &[u8]) -> Decoded {
    Ok(one("name", json!(py_strip(&ascii_replace(p)))))
}

fn decode_status(p: &[u8]) -> Decoded {
    if p.len() < 11 {
        return Err(MspError::msp("status reply is too short"));
    }
    let sensors = u16le(p, 4);
    let mut m = Map::new();
    m.insert("cycle_time_us".into(), json!(u16le(p, 0)));
    m.insert("i2c_errors".into(), json!(u16le(p, 2)));
    m.insert(
        "sensors".into(),
        json!(SENSOR_BITS
            .iter()
            .filter(|(bit, _)| sensors & bit != 0)
            .map(|(_, n)| *n)
            .collect::<Vec<_>>()),
    );
    m.insert("pid_profile".into(), json!(u32::from(p[10]) + 1));
    if p.len() >= 13 {
        m.insert("cpu_load".into(), json!(u16le(p, 11)));
    }
    if p.len() >= 15 {
        m.insert("pid_profile_count".into(), json!(p[13]));
        m.insert("rate_profile".into(), json!(u32::from(p[14]) + 1));
    }
    Ok(m)
}

fn decode_pid(p: &[u8]) -> Decoded {
    if p.len() < 9 {
        return Err(MspError::msp("PID reply is too short"));
    }
    let mut pids = Map::new();
    for (i, axis) in PID_AXES.iter().enumerate() {
        pids.insert(
            (*axis).to_string(),
            json!({"p": p[i * 3], "i": p[i * 3 + 1], "d": p[i * 3 + 2]}),
        );
    }
    if p.len() >= (PID_LEVEL_INDEX + 1) * 3 {
        let base = PID_LEVEL_INDEX * 3;
        pids.insert(
            "level".into(),
            json!({"p": p[base], "i": p[base + 1], "d": p[base + 2]}),
        );
    }
    Ok(one("pids", Value::Object(pids)))
}

fn decode_rc_tuning(p: &[u8]) -> Decoded {
    if p.len() < 8 {
        return Err(MspError::msp("rates reply is too short"));
    }
    let mut rates = Map::new();
    for (key, i) in [
        ("rc_rate", 0),
        ("rc_expo", 1),
        ("roll", 2),
        ("pitch", 3),
        ("yaw", 4),
        ("tpa_rate", 5),
        ("throttle_mid", 6),
        ("throttle_expo", 7),
    ] {
        rates.insert(key.into(), json!(p[i]));
    }
    if p.len() >= 10 {
        rates.insert("tpa_breakpoint".into(), json!(u16le(p, 8)));
    }
    Ok(one("rates", Value::Object(rates)))
}

fn decode_battery_state(p: &[u8]) -> Decoded {
    if p.len() < 11 {
        return Err(MspError::msp("battery reply is too short"));
    }
    let amps = i16::from_le_bytes([p[6], p[7]]);
    let volts = u16le(p, 9);
    Ok(one(
        "battery",
        json!({
            "cells": p[0],
            "capacity_mah": u16le(p, 1),
            "voltage": f64::from(volts) / 100.0,
            "used_mah": u16le(p, 4),
            "current": f64::from(amps) / 100.0,
        }),
    ))
}

type Decoder = fn(&[u8]) -> Decoded;

/// The questions, in the order they are asked. The first three decide whether
/// there is a flight controller on the port at all; the rest are optional, and
/// a firmware that answers one with `$M!` simply leaves that part out.
const REQUIRED: [(u8, Decoder); 3] = [
    (MSP_API_VERSION, decode_api_version),
    (MSP_FC_VARIANT, decode_variant),
    (MSP_FC_VERSION, decode_version),
];
const OPTIONAL: [(u8, Decoder); 7] = [
    (MSP_BOARD_INFO, decode_board_info),
    (MSP_BUILD_INFO, decode_build_info),
    (MSP_NAME, decode_name),
    (MSP_STATUS_EX, decode_status),
    (MSP_PID, decode_pid),
    (MSP_RC_TUNING, decode_rc_tuning),
    (MSP_BATTERY_STATE, decode_battery_state),
];

/// Decode one reply payload the way the session does, for the fixtures.
pub fn decode(command: u8, payload: &[u8]) -> Option<Decoded> {
    REQUIRED
        .iter()
        .chain(OPTIONAL.iter())
        .find(|(c, _)| *c == command)
        .map(|(_, d)| d(payload))
}

/// The part of a serial port a session uses.
pub trait ByteLink {
    /// Up to `size` bytes, empty when nothing came within the port's short timeout.
    fn read(&mut self, size: usize) -> io::Result<Vec<u8>>;
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
    fn reset_input_buffer(&mut self) -> io::Result<()>;
    fn close(&mut self);
}

/// Asks questions on an open port, one at a time, until each is answered.
pub struct MspSession<'a> {
    link: &'a mut dyn ByteLink,
    clock: &'a dyn Fn() -> f64,
    parser: MspParser,
}

fn other(e: io::Error) -> MspError {
    MspError::Other(e.to_string())
}

impl<'a> MspSession<'a> {
    pub fn new(link: &'a mut dyn ByteLink, clock: &'a dyn Fn() -> f64) -> MspSession<'a> {
        MspSession {
            link,
            clock,
            parser: MspParser::new(),
        }
    }

    /// The reply to `command`, or None when nothing answered in time.
    pub fn ask(&mut self, command: u8, timeout: f64) -> Result<Option<MspFrame>, MspError> {
        for _ in 0..=RETRIES {
            self.link.reset_input_buffer().map_err(other)?;
            let request = build_request(i64::from(command), &[]).map_err(MspError::Other)?;
            self.link.write(&request).map_err(other)?;
            let deadline = (self.clock)() + timeout;
            while (self.clock)() < deadline {
                let data = self.link.read(READ_CHUNK).map_err(other)?;
                if data.is_empty() {
                    continue;
                }
                for frame in self.parser.feed(&data) {
                    if frame.command == command {
                        return Ok(Some(frame));
                    }
                }
            }
        }
        Ok(None)
    }
}

/// Run the questions on `session` and fold the answers into one document.
pub fn read_with_session(session: &mut MspSession<'_>) -> Result<Map<String, Value>, MspError> {
    let mut result = Map::new();
    let mut unanswered: Vec<Value> = Vec::new();
    for (index, (command, decode)) in REQUIRED.iter().enumerate() {
        let timeout = if index == 0 {
            FIRST_REPLY_TIMEOUT_S
        } else {
            REPLY_TIMEOUT_S
        };
        let frame = match session.ask(*command, timeout)? {
            Some(f) if !f.error => f,
            _ => {
                return Err(MspError::msp(
                    "No flight controller answered. Use the USB port of the flight \
                     controller, not the telemetry port, with MSP on that port.",
                ))
            }
        };
        result.extend(decode(&frame.payload)?);
    }
    for (command, decode) in OPTIONAL.iter() {
        match session.ask(*command, REPLY_TIMEOUT_S)? {
            Some(f) if !f.error => match decode(&f.payload) {
                Ok(part) => result.extend(part),
                Err(MspError::Msp(_)) => unanswered.push(json!(command)),
                Err(e) => return Err(e),
            },
            _ => unanswered.push(json!(command)),
        }
    }
    let is_betaflight = result.get("variant").and_then(Value::as_str) == Some("BTFL");
    result.insert("unanswered".into(), Value::Array(unanswered));
    result.insert("is_betaflight".into(), json!(is_betaflight));
    Ok(result)
}

static LOCK: Mutex<()> = Mutex::new(());

/// The port a read uses: short timeouts, so a silent port costs a second.
pub struct SerialLink(crate::link::transport::Serial);

impl SerialLink {
    pub fn open(device: &str, baud: u32) -> Result<SerialLink, String> {
        crate::link::transport::Serial::open_direct(device, baud)
            .map(SerialLink)
            .map_err(|e| e.message)
    }
}

impl ByteLink for SerialLink {
    fn read(&mut self, size: usize) -> io::Result<Vec<u8>> {
        self.0.read_timeout(size, 0.05)
    }
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.0.write_timeout(data, 1.0).map(|_| ())
    }
    fn reset_input_buffer(&mut self) -> io::Result<()> {
        self.0.reset_input()
    }
    fn close(&mut self) {
        self.0.close();
    }
}

/// How a read opens its port.
pub type Opener = dyn Fn(&str, u32) -> Result<Box<dyn ByteLink>, String>;

/// Open `device`, read the flight controller on it and close the port again.
///
/// Serialised, so two requests cannot talk over one port. Always closes the
/// port, whatever happened.
pub fn read_flight_controller(
    device: &str,
    baud: u32,
    opener: &Opener,
    clock: &dyn Fn() -> f64,
) -> Result<Map<String, Value>, MspError> {
    let started = Instant::now();
    let _guard = loop {
        match LOCK.try_lock() {
            Ok(g) => break g,
            Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                if started.elapsed() >= Duration::from_secs(2) {
                    return Err(MspError::msp("another read is in progress"));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    let mut link = opener(device, baud).map_err(|e| MspError::Msp(format!("could not open {device}: {e}")))?;
    let result = {
        let mut session = MspSession::new(link.as_mut(), clock);
        read_with_session(&mut session)
    };
    link.close();
    result
}

/// The system opener and clock: a real serial port, the monotonic clock.
pub fn read_flight_controller_system(device: &str, baud: u32) -> Result<Map<String, Value>, MspError> {
    let origin = Instant::now();
    let clock = move || origin.elapsed().as_secs_f64();
    read_flight_controller(
        device,
        baud,
        &|d, b| SerialLink::open(d, b).map(|l| Box::new(l) as Box<dyn ByteLink>),
        &clock,
    )
}
