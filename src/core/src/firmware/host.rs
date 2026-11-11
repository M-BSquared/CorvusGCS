//! The bridge and the state store as the flash service's host.

use std::sync::Arc;

use serde_json::{json, Value};

use super::flash::FlashHost;
use crate::bridge::Bridge;
use crate::serial_ports::{
    classify_hwid, classify_serial_device, is_windows_com_port, list_serial_ports, split_serial_connection, PortRow,
};
use crate::store::VehicleStateStore;

/// `^/dev/(?:cu|tty)\..+$`
fn macos_serial(device: &str) -> bool {
    ["/dev/cu.", "/dev/tty."].iter().any(|p| {
        device
            .strip_prefix(p)
            .is_some_and(|rest| !rest.is_empty() && !rest.contains('\n'))
    })
}

/// The names one physical port can be spelled as, lower case: macOS gives every
/// serial device a call-out and a dial-in node, and only the first is enumerated.
fn descriptor_aliases(device: &str) -> Vec<String> {
    let name = device.trim().to_lowercase();
    let mut out = vec![name.clone()];
    for (this, other) in [("/dev/tty.", "/dev/cu."), ("/dev/cu.", "/dev/tty.")] {
        if let Some(rest) = name.strip_prefix(this) {
            out.push(format!("{other}{rest}"));
        }
    }
    out
}

/// `MavlinkBridge.transport()`: `usb` (a direct USB link to the flight
/// controller, flashable), `sik`, `udp`, `tcp` or `unknown`. On Windows and
/// macOS the device name says nothing about what is on the other end, so the
/// answer comes from the port's USB descriptor; unknown wins ties, because
/// guessing permissively would offer to flash firmware down a telemetry radio.
pub fn transport_of(conn: &str, ports: &dyn Fn() -> Vec<PortRow>) -> &'static str {
    if ["udp:", "udpin:", "udpout:", "udpbcast:"]
        .iter()
        .any(|p| conn.starts_with(p))
    {
        return "udp";
    }
    if conn.starts_with("tcp:") || conn.starts_with("tcpin:") {
        return "tcp";
    }
    if !conn.starts_with("serial:") {
        return "unknown";
    }
    let (device, _) = split_serial_connection(conn);
    if is_windows_com_port(&device) || macos_serial(&device) {
        let rows = ports();
        for candidate in descriptor_aliases(&device) {
            for port in &rows {
                if port.device.trim().to_lowercase() == candidate {
                    let hwid = format!("{} {}", port.hwid, port.description);
                    if !hwid.trim().is_empty() {
                        return classify_hwid(&hwid);
                    }
                }
            }
        }
        return classify_hwid("");
    }
    match classify_serial_device(&device, "", "") {
        "usb" => "usb",
        "sik" => "sik",
        _ => "unknown",
    }
}

pub struct BridgeFlashHost {
    pub bridge: Bridge,
    pub store: Arc<VehicleStateStore>,
}

impl BridgeFlashHost {
    fn flag(&self, key: &str) -> bool {
        super::util::truthy(self.store.snapshot().get(key))
    }
}

impl FlashHost for BridgeFlashHost {
    fn transport(&self) -> String {
        transport_of(&self.bridge.connection_string(), &list_serial_ports).into()
    }

    fn is_direct_usb(&self) -> bool {
        self.transport() == "usb"
    }

    fn connection_string(&self) -> String {
        self.bridge.connection_string()
    }

    fn serial_device(&self) -> String {
        self.bridge.serial_device()
    }

    fn reboot_to_bootloader(&self) -> bool {
        self.bridge.reboot_to_bootloader()
    }

    fn stop(&self) -> Result<(), String> {
        self.bridge.stop();
        Ok(())
    }

    fn set_connection(&self, conn: &str) -> Result<(), String> {
        self.bridge.set_connection(conn)
    }

    fn start(&self) -> Result<(), String> {
        self.bridge.start();
        Ok(())
    }

    fn list_serial_ports(&self) -> Result<Vec<PortRow>, String> {
        Ok(list_serial_ports())
    }

    fn connected(&self) -> bool {
        self.flag("connected")
    }

    fn armed(&self) -> bool {
        self.flag("armed")
    }

    fn board_identity(&self) -> Value {
        let (vendor, product) = self.bridge.board_ids();
        let mut identity = json!({
            "device": "", "description": "", "hwid": "",
            "vendor_id": vendor, "product_id": product,
        });
        let device = self.bridge.serial_device();
        if device.is_empty() {
            return identity;
        }
        identity["device"] = json!(device);
        if let Some(port) = list_serial_ports().into_iter().find(|p| p.device == device) {
            identity["description"] = json!(port.description);
            identity["hwid"] = json!(port.hwid);
        }
        identity
    }

    fn stack(&self) -> String {
        self.bridge
            .capabilities()
            .get("stack")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    fn vehicle_type_id(&self) -> i64 {
        self.bridge.vehicle_type_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Vec<PortRow> {
        Vec::new()
    }

    #[test]
    fn a_connection_is_classified_as_python_classifies_it() {
        for (conn, want) in [
            ("serial:/dev/ttyACM0:115200", "usb"),
            ("serial:/dev/ttyACM0", "usb"),
            ("serial:/dev/ttyACM3", "usb"),
            ("serial:/dev/serial/by-id/usb-3D_Robotics_PX4_FMU-v5_x7_0-if00", "usb"),
            ("serial:/dev/serial/by-id/usb-Hex_ProfiCNC_Pixhawk4_x4-if00", "usb"),
            ("serial:/dev/serial/by-id/usb-mRo_Pixhawk_v2-if00", "usb"),
            ("serial:/dev/ttyUSB0:57600", "sik"),
            ("serial:/dev/ttyUSB1", "sik"),
            ("udp:0.0.0.0:14540", "udp"),
            ("udpin:0.0.0.0:14540", "udp"),
            ("udpbcast:10.0.0.255:14550", "udp"),
            ("tcp:1.2.3.4:5760", "tcp"),
            ("serial:/dev/ttyAMA0", "unknown"),
            ("serial:/dev/ttyTHS0", "unknown"),
            ("", "unknown"),
            ("garbage", "unknown"),
        ] {
            assert_eq!(transport_of(conn, &none), want, "{conn}");
        }
    }

    #[test]
    fn a_port_with_no_name_to_go_by_is_read_from_its_descriptor() {
        let rows = || {
            vec![
                PortRow {
                    device: "/dev/cu.usbmodem1101".into(),
                    description: "PX4 FMU v6X.x".into(),
                    hwid: "USB VID:PID=26AC:0032".into(),
                },
                PortRow {
                    device: "COM7".into(),
                    description: "USB Serial Port".into(),
                    hwid: "USB VID:PID=0403:6001".into(),
                },
            ]
        };
        assert_eq!(transport_of("serial:/dev/cu.usbmodem1101:115200", &rows), "usb");
        assert_eq!(transport_of("serial:/dev/tty.usbmodem1101", &rows), "usb");
        assert_eq!(transport_of("serial:COM7:57600", &rows), "sik");
        assert_eq!(transport_of("serial:COM9", &rows), "unknown");
        assert_eq!(transport_of("serial:/dev/cu.usbmodem1101", &none), "unknown");
    }
}
