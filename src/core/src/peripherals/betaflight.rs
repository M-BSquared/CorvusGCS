//! Betaflight: read a flight controller over MSP (`routes/betaflight.py`).
//!
//! Read only: the page lists the serial ports and reads the controller on one
//! of them (see `corvus_link::plain::msp`), and nothing it can do changes the
//! controller. A read is one short session that holds nothing afterwards, so
//! there is nothing for the shutdown path to end.

use corvus_link::plain::msp;
use serde_json::{json, Map, Value};

use super::ports::{is_bootloader_port, is_rtk_device, list_serial_ports};
use crate::core::Core;
use crate::module::Registry;
use crate::request::Request;
use crate::response::Response;
use crate::serial_ports::same_port;

pub fn register(reg: &mut Registry) {
    reg.get("/api/betaflight/status", api_status);
    reg.post("/api/betaflight/read", api_read);
}

/// The serial port the MAVLink link holds, "" when it is not on a serial port.
fn link_device(core: &Core) -> String {
    core.service::<crate::vehicle::VehicleService>()
        .map(|svc| svc.active().bridge.serial_device())
        .unwrap_or_default()
}

/// The ports a controller could be on, and which one the MAVLink link holds.
///
/// Always 200 so the page can poll it. The link's own port is listed and
/// flagged, not hidden: an operator who picked the wrong cable should be told
/// which it is.
fn api_status(core: &Core, _req: &Request) -> Response {
    let link = link_device(core);
    let ports: Vec<Value> = if core.service::<crate::vehicle::VehicleService>().is_some() {
        list_serial_ports()
            .into_iter()
            .filter(|p| {
                !is_bootloader_port(&p.device, &p.hwid, &p.description)
                    && !is_rtk_device(&p.device, &p.hwid, &p.description)
            })
            .map(|p| {
                let on_link = !link.is_empty() && same_port(&p.device, &link);
                json!({"device": p.device, "description": p.description, "hwid": p.hwid, "link": on_link})
            })
            .collect()
    } else {
        Vec::new()
    };
    Response::json(json!({
        "ports": ports,
        "link_device": link,
        "default_baud": msp::DEFAULT_BAUD,
        "bauds": msp::BAUD_CANDIDATES,
    }))
}

/// Read the controller on `device`: firmware, board, sensors, PIDs, rates.
///
/// 409 for every condition the operator can do something about (a port that is
/// the MAVLink link, one that will not open, one that does not answer), 400
/// for a request that is malformed. A synchronous read: it takes a second or
/// two and holds nothing afterwards.
fn api_read(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let device = match payload.get("device") {
        Some(Value::String(s)) if !crate::peripherals::util::py_strip(s).is_empty() => {
            crate::peripherals::util::py_strip(s).to_string()
        }
        _ => return Response::not_ok(400, "device is required"),
    };
    let baud = match payload.get("baud") {
        None => msp::DEFAULT_BAUD,
        Some(Value::Number(n)) => match n.as_i64().and_then(|b| u32::try_from(b).ok()) {
            Some(b) if b > 0 => b,
            _ => return Response::not_ok(400, "baud must be a positive integer"),
        },
        Some(_) => return Response::not_ok(400, "baud must be a positive integer"),
    };
    let link = link_device(core);
    if !link.is_empty() && same_port(&device, &link) {
        return Response::not_ok(
            409,
            "This port carries the MAVLink link. Read the flight controller on its USB port instead.",
        );
    }
    match msp::read_flight_controller_system(&device, baud) {
        Ok(mut result) => {
            finish(&mut result, &device, baud);
            Response::json(Value::Object(result))
        }
        Err(msp::MspError::Msp(why)) => Response::not_ok(409, &why),
        Err(msp::MspError::Other(why)) => Response::not_ok(500, &format!("read failed: {why}")),
    }
}

fn finish(result: &mut Map<String, Value>, device: &str, baud: u32) {
    result.insert("ok".into(), json!(true));
    result.insert("device".into(), json!(device));
    result.insert("baud".into(), json!(baud));
}
