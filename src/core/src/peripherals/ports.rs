//! Which serial port is what: flight controller, radio, RTK base, bootloader
//! (`serial_ports.py`), and the enumeration of the ports that exist
//! (`MavlinkBridge.list_serial_ports`). Nothing here opens a port.
//!
//! The patterns of the Python file are matched by hand: the standard library
//! has no regular expressions and these are small.

use std::path::Path;

pub use crate::serial_ports::{is_windows_com_port, same_port};

use super::util::py_strip;

/// SiK Telemetry Radio V3 factory default, and what a `serial:` string without
/// a baud rate means.
pub const DEFAULT_SERIAL_BAUD: u32 = 57600;

const BYID_TOKENS: [&str; 11] = [
    "px4",
    "pixhawk",
    "fmu",
    "3d_robotics",
    "3d robotics",
    "hex_proficnc",
    "arducy",
    "mro",
    "hex ",
    "usb vid:pid=26ac",
    "usb vid:pid=0483",
];

const BRIDGE_TOKENS: [&str; 4] = ["vid:pid=0403", "vid:pid=10c4", "vid:pid=1a86", "vid:pid=067b"];

const RTK_TOKENS: [&str; 7] = [
    "vid:pid=1546",
    "vid:pid=152a",
    "u-blox",
    "ublox",
    "septentrio",
    "zed-f9p",
    "simplertk",
];

fn contains_any(text: &str, tokens: &[&str]) -> bool {
    let lower = text.to_lowercase();
    tokens.iter().any(|t| lower.contains(t))
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn alnum_lower(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `\bword\b` anywhere in `lower` (already lower case).
fn has_word(lower: &str, word: &str) -> bool {
    let chars: Vec<char> = lower.chars().collect();
    let w: Vec<char> = word.chars().collect();
    if w.len() > chars.len() {
        return false;
    }
    (0..=chars.len() - w.len()).any(|i| {
        chars[i..i + w.len()] == w[..]
            && (i == 0 || !is_word(chars[i - 1]))
            && (i + w.len() == chars.len() || !is_word(chars[i + w.len()]))
    })
}

fn bootloader_text(text: &str) -> bool {
    let lower = text.to_lowercase();
    if lower.contains("vid:pid=0483:df11") {
        return true;
    }
    if has_word(&lower, "bl fmu") || has_word(&lower, "bootloader") || has_word(&lower, "dfu") {
        return true;
    }
    // `-bl\b`: no boundary needed in front.
    let chars: Vec<char> = lower.chars().collect();
    (0..chars.len().saturating_sub(2)).any(|i| {
        chars[i] == '-'
            && chars[i + 1] == 'b'
            && chars[i + 2] == 'l'
            && (i + 3 == chars.len() || !is_word(chars[i + 3]))
    })
}

fn phantom_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("/dev/ttyS") {
        if digits(rest) {
            return true;
        }
    }
    for kind in ["cu", "tty"] {
        for tail in ["Bluetooth-Incoming-Port", "debug-console"] {
            if name
                .strip_prefix("/dev/")
                .and_then(|r| r.strip_prefix(kind))
                .and_then(|r| r.strip_prefix('.'))
                == Some(tail)
            {
                return true;
            }
        }
    }
    false
}

fn pty_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("/dev/pts/") {
        return digits(rest);
    }
    if let Some(rest) = name.strip_prefix("/dev/pty") {
        return alnum_lower(rest);
    }
    if let Some(rest) = name.strip_prefix("/dev/ttyp") {
        return alnum_lower(rest);
    }
    false
}

/// Is `device` a serial node that is never a vehicle link: the 8250 platform
/// ports, the two macOS entries that exist whether or not anything is plugged
/// in, and pseudo-terminals.
pub fn is_phantom_device(device: &str) -> bool {
    let name = py_strip(device);
    name.is_empty() || phantom_name(name) || pty_name(name)
}

/// Is this port a flight controller sitting in its USB bootloader? Closed
/// world: only a positive match counts.
pub fn is_bootloader_port(_device: &str, hwid: &str, description: &str) -> bool {
    let text = format!("{hwid} {description}");
    !py_strip(&text).is_empty() && bootloader_text(&text)
}

/// Is this port an RTK GNSS receiver rather than something to fly? Reads the
/// descriptor only.
pub fn is_rtk_device(_device: &str, hwid: &str, description: &str) -> bool {
    let text = format!("{hwid} {description}");
    !py_strip(&text).is_empty() && contains_any(&text, &RTK_TOKENS)
}

/// `"usb"`, `"sik"`, `"rtk"` or `"unknown"` from a USB descriptor string.
pub fn classify_hwid(hwid: &str) -> &'static str {
    if py_strip(hwid).is_empty() {
        return "unknown";
    }
    if contains_any(hwid, &RTK_TOKENS) {
        return "rtk";
    }
    if contains_any(hwid, &BRIDGE_TOKENS) {
        return "sik";
    }
    if contains_any(hwid, &BYID_TOKENS) {
        return "usb";
    }
    "unknown"
}

fn macos_serial(name: &str) -> bool {
    for kind in ["cu", "tty"] {
        if let Some(rest) = name
            .strip_prefix("/dev/")
            .and_then(|r| r.strip_prefix(kind))
            .and_then(|r| r.strip_prefix('.'))
        {
            if !rest.is_empty() && !rest.contains('\n') {
                return true;
            }
        }
    }
    false
}

/// `"usb"`, `"sik"`, `"rtk"` or `"unknown"` for an enumerated serial port.
/// Linux device names carry the answer; Windows COM numbers and macOS
/// `/dev/cu.*` nodes do not, so those fall through to the descriptor.
pub fn classify_serial_device(device: &str, hwid: &str, description: &str) -> &'static str {
    let name = py_strip(device);
    if name.is_empty() {
        return "unknown";
    }
    if is_rtk_device(name, hwid, description) {
        return "rtk";
    }
    if name.strip_prefix("/dev/ttyACM").is_some_and(digits) {
        return "usb";
    }
    if name.starts_with("/dev/serial/by-id/") && contains_any(name, &BYID_TOKENS) {
        return "usb";
    }
    if name.strip_prefix("/dev/ttyUSB").is_some_and(digits) {
        return "sik";
    }
    if is_windows_com_port(name) || macos_serial(name) {
        return classify_hwid(&format!("{hwid} {description}"));
    }
    "unknown"
}

/// `serial:<device>[:<baud>]` to `(device, baud)`. Never fails: the baud is the
/// last `:` field only when it is all digits.
pub fn split_serial_connection(conn: &str) -> (String, u32) {
    let conn = conn.split_once('?').map_or(conn, |(base, _)| base);
    let rest = conn.strip_prefix("serial:").unwrap_or(conn);
    if let Some((device, baud)) = rest.rsplit_once(':') {
        if digits(baud) {
            if let Ok(b) = baud.parse::<u32>() {
                return (device.to_string(), b);
            }
        }
    }
    (rest.to_string(), DEFAULT_SERIAL_BAUD)
}

/// `"26AC:0032/0001"` for a USB descriptor, or `None` when it has no ids: what
/// a port is, rather than where it is plugged in.
pub fn usb_identity(hwid: &str) -> Option<String> {
    let chars: Vec<char> = hwid.chars().collect();
    let key: Vec<char> = "vid:pid=".chars().collect();
    let hex4 = |s: &[char]| s.len() == 4 && s.iter().all(|c| c.is_ascii_hexdigit());
    for i in 0..chars.len() {
        if i + key.len() + 9 > chars.len() {
            break;
        }
        let head: String = chars[i..i + key.len()].iter().collect::<String>().to_lowercase();
        if head != "vid:pid=" {
            continue;
        }
        let at = i + key.len();
        let (vid, colon, pid) = (&chars[at..at + 4], chars[at + 4], &chars[at + 5..at + 9]);
        if !hex4(vid) || colon != ':' || !hex4(pid) {
            continue;
        }
        let mut serial = String::new();
        let rest = &chars[at + 9..];
        let ws = rest.iter().take_while(|c| c.is_whitespace()).count();
        if ws > 0 {
            let after = &rest[ws..];
            let tag: String = after.iter().take(4).collect::<String>().to_lowercase();
            if tag == "ser=" {
                let value: String = after[4..].iter().take_while(|c| !c.is_whitespace()).collect();
                if !value.is_empty() {
                    serial = value;
                }
            }
        }
        let up = |s: &[char]| s.iter().collect::<String>().to_uppercase();
        return Some(format!("{}:{}/{}", up(vid), up(pid), serial));
    }
    None
}

/// Is `device` there right now, either enumerated or as a device node?
pub fn port_present<S: AsRef<str>>(device: &str, enumerated: &[S]) -> bool {
    let name = py_strip(device);
    if name.is_empty() {
        return false;
    }
    if enumerated.iter().any(|other| same_port(name, other.as_ref())) {
        return true;
    }
    !is_windows_com_port(name) && Path::new(name).exists()
}

/// One row of the enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortRow {
    pub device: String,
    pub description: String,
    pub hwid: String,
}

/// The serial ports there are, without opening them: `{device, description,
/// hwid}` sorted by device, phantom nodes dropped. Falls back to globbing
/// `/dev` when the system's list is empty. Never fails.
pub fn list_serial_ports() -> Vec<PortRow> {
    let mut ports = system_ports();
    if ports.is_empty() {
        ports = glob_ports();
    }
    ports.retain(|p| !is_phantom_device(&p.device));
    ports.sort_by(|a, b| a.device.cmp(&b.device));
    ports
}

fn glob_ports() -> Vec<PortRow> {
    let mut devices: Vec<String> = Vec::new();
    for (dir, prefixes) in [
        (
            "/dev",
            &[
                "ttyUSB",
                "ttyACM",
                "cu.usbmodem",
                "cu.usbserial",
                "cu.SLAB_",
                "rfcomm",
                "cu.HC-",
            ][..],
        ),
        ("/dev/serial/by-id", &[""][..]),
    ] {
        let Ok(read) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if prefixes.iter().any(|p| name.starts_with(p)) {
                devices.push(format!("{dir}/{name}"));
            }
        }
    }
    devices.sort();
    devices.dedup();
    devices
        .into_iter()
        .map(|device| PortRow {
            device,
            description: String::new(),
            hwid: String::new(),
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn system_ports() -> Vec<PortRow> {
    let mut devices: Vec<String> = Vec::new();
    let Ok(read) = std::fs::read_dir("/dev") else {
        return Vec::new();
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if ["ttyS", "ttyUSB", "ttyXRUSB", "ttyACM", "ttyAMA", "rfcomm", "ttyAP"]
            .iter()
            .any(|p| name.starts_with(p))
        {
            devices.push(format!("/dev/{name}"));
        }
    }
    devices.into_iter().filter_map(|d| sysfs_row(&d)).collect()
}

#[cfg(target_os = "linux")]
fn read_line(dir: &Path, file: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(file)).ok()?;
    Some(py_strip(text.lines().next().unwrap_or("")).to_string())
}

/// pyserial's `list_ports_linux.SysFS`: what sysfs says about one node.
#[cfg(target_os = "linux")]
fn sysfs_row(device: &str) -> Option<PortRow> {
    let name = Path::new(device).file_name()?.to_string_lossy().into_owned();
    let link = Path::new("/sys/class/tty").join(&name).join("device");
    let mut description = "n/a".to_string();
    let mut hwid = "n/a".to_string();
    if link.exists() {
        let device_path = std::fs::canonicalize(&link).ok()?;
        let subsystem = std::fs::canonicalize(device_path.join("subsystem"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
        match subsystem.as_deref() {
            Some("platform") => return None,
            Some(kind @ ("usb" | "usb-serial")) => {
                let interface_path = if kind == "usb-serial" {
                    device_path.parent()?.to_path_buf()
                } else {
                    device_path.clone()
                };
                let usb_device = interface_path.parent()?.to_path_buf();
                let num_if: u32 = read_line(&usb_device, "bNumInterfaces")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1);
                let vid = u32::from_str_radix(&read_line(&usb_device, "idVendor")?, 16).ok()?;
                let pid = u32::from_str_radix(&read_line(&usb_device, "idProduct")?, 16).ok()?;
                let serial = read_line(&usb_device, "serial");
                let location = if num_if > 1 { &interface_path } else { &usb_device }
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned());
                let product = read_line(&usb_device, "product");
                let interface = read_line(&interface_path, "interface");
                description = match (&interface, &product) {
                    (Some(i), p) => format!("{} - {i}", p.clone().unwrap_or_else(|| "None".into())),
                    (None, Some(p)) => p.clone(),
                    (None, None) => name.clone(),
                };
                hwid = format!(
                    "USB VID:PID={vid:04X}:{pid:04X}{}{}",
                    serial.map(|s| format!(" SER={s}")).unwrap_or_default(),
                    location.map(|l| format!(" LOCATION={l}")).unwrap_or_default()
                );
            }
            Some("pnp") => {
                description = name.clone();
                hwid = read_line(&device_path, "id").unwrap_or_default();
            }
            Some("amba") => {
                description = name.clone();
                hwid = device_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
            _ => {}
        }
    }
    Some(PortRow {
        device: device.to_string(),
        description,
        hwid,
    })
}

#[cfg(any(target_os = "macos", windows))]
fn system_ports() -> Vec<PortRow> {
    let Ok(list) = serialport::available_ports() else {
        return Vec::new();
    };
    list.into_iter()
        .map(|p| {
            let (description, hwid) = match &p.port_type {
                serialport::SerialPortType::UsbPort(u) => (
                    u.product.clone().unwrap_or_else(|| p.port_name.clone()),
                    format!(
                        "USB VID:PID={:04X}:{:04X}{}",
                        u.vid,
                        u.pid,
                        u.serial_number
                            .as_ref()
                            .map(|s| format!(" SER={s}"))
                            .unwrap_or_default()
                    ),
                ),
                _ => ("n/a".to_string(), "n/a".to_string()),
            };
            PortRow {
                device: p.port_name,
                description,
                hwid,
            }
        })
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn system_ports() -> Vec<PortRow> {
    Vec::new()
}
