//! The parts of `serial_ports.py` the vehicle list and the second link need:
//! which names are one serial node. The enumeration and the classification of
//! ports come with the peripherals area.

use std::collections::HashSet;

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
