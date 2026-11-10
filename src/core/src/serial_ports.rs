//! Which serial port is what (`serial_ports.py`): flight controller, radio, RTK
//! base, bootloader. Functions over a device name and its USB descriptor; none
//! of them opens a port. [`list_serial_ports`] enumerates without opening.

use std::collections::HashSet;

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
const USB_SERIAL_BRIDGE_TOKENS: [&str; 4] = ["vid:pid=0403", "vid:pid=10c4", "vid:pid=1a86", "vid:pid=067b"];
const RTK_TOKENS: [&str; 7] = [
    "vid:pid=1546",
    "vid:pid=152a",
    "u-blox",
    "ublox",
    "septentrio",
    "zed-f9p",
    "simplertk",
];

fn has_any(text: &str, tokens: &[&str]) -> bool {
    let lower = text.to_lowercase();
    tokens.iter().any(|t| lower.contains(t))
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `\bword\b` anywhere in `text` (already lower case).
fn has_word(text: &str, word: &str, leading: bool) -> bool {
    let mut from = 0;
    while let Some(i) = text[from..].find(word) {
        let start = from + i;
        let end = start + word.len();
        let before_ok = !leading || text[..start].chars().next_back().is_none_or(|c| !word_char(c));
        let after_ok = text[end..].chars().next().is_none_or(|c| !word_char(c));
        if before_ok && after_ok {
            return true;
        }
        from = start + text[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn digits_to_end(rest: &str) -> bool {
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

/// Is `device` a serial node that is never a vehicle link: the 8250 platform
/// ports, the two macOS entries that exist whether or not anything is plugged
/// in, and pseudo terminals.
pub fn is_phantom_device(device: &str) -> bool {
    let name = device.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c));
    if name.is_empty() {
        return true;
    }
    if name.strip_prefix("/dev/ttyS").is_some_and(digits_to_end) {
        return true;
    }
    for prefix in ["/dev/cu.", "/dev/tty."] {
        if let Some(rest) = name.strip_prefix(prefix) {
            if rest == "Bluetooth-Incoming-Port" || rest == "debug-console" {
                return true;
            }
        }
    }
    if let Some(rest) = name.strip_prefix("/dev/pts/") {
        return digits_to_end(rest);
    }
    let lower_alnum = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase());
    if let Some(rest) = name.strip_prefix("/dev/pty") {
        return lower_alnum(rest.strip_prefix('p').unwrap_or(rest)) || lower_alnum(rest);
    }
    name.strip_prefix("/dev/ttyp").is_some_and(lower_alnum)
}

/// Is this port a flight controller sitting in its USB bootloader? Closed
/// world on purpose: only a positive match counts, and only the descriptor is
/// read.
pub fn is_bootloader_port(_device: &str, hwid: &str, description: &str) -> bool {
    let text = format!("{hwid} {description}");
    if text.trim().is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    lower.contains("vid:pid=0483:df11")
        || has_word(&lower, "bl fmu", true)
        || has_word(&lower, "bootloader", true)
        || has_word(&lower, "dfu", true)
        || has_word(&lower, "-bl", false)
}

/// Is this port an RTK GNSS receiver rather than something to fly?
pub fn is_rtk_device(_device: &str, hwid: &str, description: &str) -> bool {
    let text = format!("{hwid} {description}");
    !text.trim().is_empty() && has_any(&text, &RTK_TOKENS)
}

/// `usb`, `sik`, `rtk` or `unknown` from a USB descriptor string.
pub fn classify_hwid(hwid: &str) -> &'static str {
    if hwid.trim().is_empty() {
        return "unknown";
    }
    if has_any(hwid, &RTK_TOKENS) {
        return "rtk";
    }
    if has_any(hwid, &USB_SERIAL_BRIDGE_TOKENS) {
        return "sik";
    }
    if has_any(hwid, &BYID_TOKENS) {
        return "usb";
    }
    "unknown"
}

/// `usb`, `sik`, `rtk` or `unknown` for an enumerated serial port.
pub fn classify_serial_device(device: &str, hwid: &str, description: &str) -> &'static str {
    let name = device.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c));
    if name.is_empty() {
        return "unknown";
    }
    if is_rtk_device(name, hwid, description) {
        return "rtk";
    }
    if name.strip_prefix("/dev/ttyACM").is_some_and(digits_to_end) {
        return "usb";
    }
    if name.starts_with("/dev/serial/by-id/") && has_any(name, &BYID_TOKENS) {
        return "usb";
    }
    if name.strip_prefix("/dev/ttyUSB").is_some_and(digits_to_end) {
        return "sik";
    }
    let macos = ["/dev/cu.", "/dev/tty."].iter().any(|p| {
        name.strip_prefix(p)
            .is_some_and(|rest| !rest.is_empty() && !rest.contains('\n'))
    });
    if is_windows_com_port(name) || macos {
        return classify_hwid(&format!("{hwid} {description}"));
    }
    "unknown"
}

/// `serial:<device>[:<baud>]` as `(device, baud)`. Never fails.
pub fn split_serial_connection(conn: &str) -> (String, u32) {
    crate::bridge::split_serial_connection(conn)
}

/// `"26AC:0032/0001"` for a USB descriptor, None when it has no ids: what a
/// port is, rather than where it is plugged in.
pub fn usb_identity(hwid: &str) -> Option<String> {
    let upper = hwid.to_ascii_uppercase();
    let at = upper.find("VID:PID=")?;
    let rest = &upper[at + 8..];
    let hex4 = |s: &str| s.len() >= 4 && s.as_bytes()[..4].iter().all(u8::is_ascii_hexdigit);
    if !hex4(rest) || rest.as_bytes().get(4) != Some(&b':') || !hex4(&rest[5..]) {
        return None;
    }
    let (vid, pid) = (&rest[..4], &rest[5..9]);
    let after = &rest[9..];
    let trimmed = after.trim_start();
    let serial = if trimmed.len() < after.len() && trimmed.starts_with("SER=") {
        let from = hwid.len() - trimmed.len() + 4;
        hwid[from..].split_whitespace().next().unwrap_or("")
    } else {
        ""
    };
    Some(format!("{vid}:{pid}/{serial}"))
}

/// Is `device` present right now, either enumerated or as a device node?
pub fn port_present(device: &str, enumerated: &[String]) -> bool {
    let name = device.trim();
    if name.is_empty() {
        return false;
    }
    if enumerated.iter().any(|other| same_port(name, other)) {
        return true;
    }
    !is_windows_com_port(name) && std::path::Path::new(name).exists()
}

/// One row of the enumeration.
#[derive(Debug, Clone, PartialEq)]
pub struct PortRow {
    pub device: String,
    pub description: String,
    pub hwid: String,
}

/// The serial ports there are, without opening any: sorted by device, phantom
/// nodes dropped. The system's own list first (USB descriptors included), a
/// `/dev` glob when that finds nothing.
pub fn list_serial_ports() -> Vec<PortRow> {
    let mut rows: Vec<PortRow> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let (description, hwid) = match &p.port_type {
                serialport::SerialPortType::UsbPort(u) => {
                    let mut hwid = format!("USB VID:PID={:04X}:{:04X}", u.vid, u.pid);
                    if let Some(serial) = &u.serial_number {
                        hwid.push_str(&format!(" SER={serial}"));
                    }
                    let description = u.product.clone().unwrap_or_else(|| p.port_name.clone());
                    (description, hwid)
                }
                _ => (String::new(), String::new()),
            };
            PortRow {
                device: p.port_name,
                description,
                hwid,
            }
        })
        .collect();
    if rows.is_empty() {
        rows = glob_devices();
    }
    rows.retain(|r| !is_phantom_device(&r.device));
    rows.sort_by(|a, b| a.device.cmp(&b.device));
    rows
}

fn glob_devices() -> Vec<PortRow> {
    let mut devices: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let scan = |dir: &str, prefixes: &[&str], out: &mut std::collections::BTreeSet<String>| {
        if let Ok(read) = std::fs::read_dir(dir) {
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if prefixes.is_empty() || prefixes.iter().any(|p| name.starts_with(p)) {
                    out.insert(format!("{dir}/{name}"));
                }
            }
        }
    };
    scan(
        "/dev",
        &["ttyUSB", "ttyACM", "cu.usbmodem", "cu.usbserial", "cu.SLAB_"],
        &mut devices,
    );
    scan("/dev/serial/by-id", &[], &mut devices);
    devices
        .into_iter()
        .map(|device| PortRow {
            device,
            description: String::new(),
            hwid: String::new(),
        })
        .collect()
}

/// `COM7` or `\\.\COM12`, in any case.
pub fn is_windows_com_port(device: &str) -> bool {
    let text = device.trim();
    let rest = text.strip_prefix("\\\\.\\").unwrap_or(text);
    let upper = rest.to_ascii_uppercase();
    match upper.strip_prefix("COM") {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// Every spelling of `device` that names the same node, lowercased.
fn port_aliases(device: &str) -> HashSet<String> {
    let name = device.trim();
    let mut aliases = HashSet::new();
    if name.is_empty() {
        return aliases;
    }
    aliases.insert(name.to_lowercase());
    for (this, other) in [("/dev/tty.", "/dev/cu."), ("/dev/cu.", "/dev/tty.")] {
        if let Some(rest) = name.strip_prefix(this) {
            aliases.insert(format!("{other}{rest}").to_lowercase());
        }
    }
    if is_windows_com_port(name) {
        let upper = name.to_uppercase();
        aliases.insert(upper.strip_prefix("\\\\.\\").unwrap_or(&upper).to_lowercase());
    } else if name.starts_with('/') {
        if let Ok(real) = std::fs::canonicalize(name) {
            aliases.insert(real.to_string_lossy().to_lowercase());
        }
    }
    aliases
}

/// Do `a` and `b` name the same serial node? A by-id symlink and the node it
/// points at, the two halves of a macOS cu/tty pair, and `COM3` against
/// `\\.\COM3` all count as one port.
pub fn same_port(a: &str, b: &str) -> bool {
    let left = port_aliases(a);
    port_aliases(b).iter().any(|x| left.contains(x))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_of_one_node_are_one_port() {
        assert!(same_port("/dev/tty.usbmodem1", "/dev/cu.usbmodem1"));
        assert!(same_port("COM3", "\\\\.\\com3"));
        assert!(same_port("/dev/ttyACM0", "/DEV/TTYACM0"));
        assert!(!same_port("/dev/ttyACM0", "/dev/ttyACM1"));
        assert!(!same_port("", ""));
    }

    #[test]
    fn com_names_are_recognised_by_shape() {
        assert!(is_windows_com_port("COM7"));
        assert!(is_windows_com_port(" \\\\.\\COM12 "));
        assert!(!is_windows_com_port("COM"));
        assert!(!is_windows_com_port("/dev/ttyUSB0"));
    }
}
