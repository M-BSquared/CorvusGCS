//! The ArduPilot safety and sensor schema of Setup, Safety and Sensors
//! (`ardupilot_safety.py`): the twin of [`super::safety`] with the same output
//! shape, so the interface renders an ArduPilot aircraft and a PX4 one with the
//! same code. A field whose parameter the connected firmware did not answer for
//! is dropped, so one schema serves Copter, Plane, Rover and Sub, and ArduPilot
//! 4.3 through 4.6, with no version switch.
//!
//! Two things shape the tables: bitmasks (`FENCE_TYPE`, `FS_OPTIONS`,
//! `ARMING_CHECK` come out as `kind = "bitmask"`) and centimetres (the Copter
//! return profile is in centimetres; the unit travels with the field because the
//! value the page writes has to be the value the parameter holds). A driver on
//! ArduPilot is more than its TYPE value: a serial sensor needs a SERIALn port
//! on the right protocol at the right baud rate, a DroneCAN sensor needs a CAN
//! port running DroneCAN, some I2C drivers need an address. Every driver entry
//! carries those writes and the state is read back from all of it.

use serde_json::{json, Value};

use super::fields::*;
use super::tables::ardupilot_safety as tbl;
use crate::json::Map;

pub const EXTRA_PARAM_LIMIT: usize = 24;
const BUS_CAN: &str = "can";
const BUS_SERIAL: &str = "serial";
const BUS_ANALOG: &str = "analog";
const BUS_PWM: &str = "pwm";
const SERIAL_PROTOCOL_NONE: i64 = -1;
const SERIAL_PROTOCOL_RANGEFINDER: i64 = 9;
const SERIAL_PROTOCOL_OPTICAL_FLOW: i64 = 18;
const CAN_PROTOCOL_DRONECAN: i64 = 1;
const EK3_VELXY_FLOW: i64 = 5;
const EK3_FUSE_ALL_VELOCITIES: i64 = 1;
const FENCE_TYPE_POLYGON: i64 = 1 << 2;

/// Protocols whose driver takes the first port set to them.
fn exclusive(protocol: i64) -> bool {
    protocol == SERIAL_PROTOCOL_RANGEFINDER || protocol == SERIAL_PROTOCOL_OPTICAL_FLOW
}

fn type_param(kind: &str) -> &'static str {
    if kind == "range" {
        "RNGFND1_TYPE"
    } else {
        "FLOW_TYPE"
    }
}

fn types_of(kind: &str) -> &'static [Value] {
    tbl().array(if kind == "range" {
        "RANGEFINDER_TYPES"
    } else {
        "FLOW_TYPES"
    })
}

fn rounded(values: &Values, name: &str) -> Option<i64> {
    values.get(name).map(|v| round_int(*v))
}

fn rounded_or(values: &Values, name: &str, default: i64) -> i64 {
    rounded(values, name).unwrap_or(default)
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(t)) => !t.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Null) | None => false,
        Some(_) => true,
    }
}

fn int_of(v: Option<&Value>) -> i64 {
    v.and_then(Value::as_f64).map_or(0, |f| f as i64)
}

fn float_of(v: Option<&Value>) -> f64 {
    v.and_then(Value::as_f64).unwrap_or(0.0)
}

fn table_options(name: &str) -> Vec<Value> {
    tbl().options(name)
}

// ---------------------------------------------------------------------------
// Operator-added parameters
// ---------------------------------------------------------------------------

/// Cleans the operator's extra parameter list: upper case, unique, bounded. The
/// rules are the PX4 module's, because they are about what a MAVLink parameter
/// name can be, not about which stack owns it.
pub fn normalise_extra(names: &[String]) -> Vec<String> {
    super::safety::normalise_extra(names)
}

/// The operator's own parameters, each flagged present or absent.
pub fn extra_fields(names: &[String], values: &Values) -> Vec<Value> {
    names
        .iter()
        .map(|name| {
            let known = values.contains_key(name);
            json!({
                "param": name, "label": name, "kind": "number",
                "value": crate::json::num(values.get(name).copied().unwrap_or(0.0)),
                "present": known,
                "hint": if known { "" } else { "This firmware did not answer for this parameter." },
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Sensors
// ---------------------------------------------------------------------------

fn can_ports(values: &Values) -> Vec<i64> {
    let mut ports = Vec::new();
    for k in [1, 2] {
        let Some(driver) = rounded(values, &format!("CAN_P{k}_DRIVER")) else {
            continue;
        };
        if driver <= 0 {
            continue;
        }
        if rounded(values, &format!("CAN_D{driver}_PROTOCOL")) == Some(CAN_PROTOCOL_DRONECAN) {
            ports.push(k);
        }
    }
    ports
}

/// Which CAN connector the sensor belongs on: the one DroneCAN runs on, CAN1
/// when none does yet because that is the port the switch starts.
fn can_wiring(values: &Values) -> String {
    let ports = can_ports(values);
    let where_ = if ports.is_empty() {
        "CAN1".to_string()
    } else {
        ports.iter().map(|k| format!("CAN{k}")).collect::<Vec<_>>().join(" or ")
    };
    format!(
        "Plug it into the {where_} connector, the one that runs DroneCAN. Further CAN devices can be chained behind it, and the last one on the bus needs its termination on."
    )
}

fn wiring(values: &Values, bus: &str) -> String {
    if bus == BUS_CAN {
        can_wiring(values)
    } else {
        tbl().text("WIRING", bus).to_string()
    }
}

fn spec_of(kind: &str, value: Option<f64>) -> Option<&'static Value> {
    let ivalue = round_int(value?);
    types_of(kind).iter().find(|t| t["value"].as_i64() == Some(ivalue))
}

fn baud_rate(code: f64) -> i64 {
    let ivalue = round_int(code);
    tbl()
        .get("BAUD_CODES")
        .get(ivalue.to_string())
        .and_then(Value::as_i64)
        .unwrap_or(ivalue)
}

#[derive(Clone)]
struct Port {
    value: i64,
    label: String,
    protocol: i64,
}

/// Every SERIALn port the vehicle answered for, labelled with what it does now.
fn serial_ports(values: &Values) -> Vec<Port> {
    let mut ports = Vec::new();
    for n in 1..=8 {
        let Some(protocol) = rounded(values, &format!("SERIAL{n}_PROTOCOL")) else {
            continue;
        };
        let use_ = tbl()
            .get("SERIAL_PROTOCOL_NAMES")
            .get(protocol.to_string())
            .and_then(Value::as_str)
            .map_or(format!("protocol {protocol}"), str::to_string);
        ports.push(Port {
            value: n,
            label: format!("SERIAL{n} ({use_})"),
            protocol,
        });
    }
    ports
}

fn ports_with(ports: &[Port], protocol: i64) -> Vec<i64> {
    ports
        .iter()
        .filter(|p| p.protocol == protocol)
        .map(|p| p.value)
        .collect()
}

/// Whether a GPS is set up: `GPS1_TYPE` from 4.6, `GPS_TYPE` before.
fn gps_configured(values: &Values) -> Option<bool> {
    ["GPS1_TYPE", "GPS_TYPE"]
        .iter()
        .find_map(|n| rounded(values, n))
        .map(|v| v != 0)
}

struct DroneCan {
    running: bool,
    writes: Vec<Value>,
    blocked: String,
}

/// Is DroneCAN running, and if not, which writes start it. Otherwise the first
/// CAN port is started, but only when it is idle and its driver is not serving
/// the second port with something else; anything else is `blocked` with the
/// reason, because rewriting a CAN port that already runs another protocol could
/// stop whatever is on it.
fn dronecan(values: &Values) -> DroneCan {
    let blocked = |text: &str| DroneCan {
        running: false,
        writes: Vec::new(),
        blocked: text.to_string(),
    };
    if !can_ports(values).is_empty() {
        return DroneCan {
            running: true,
            writes: Vec::new(),
            blocked: String::new(),
        };
    }
    let Some(p1) = rounded(values, "CAN_P1_DRIVER") else {
        return blocked("This flight controller reports no CAN port (CAN_P1_DRIVER).");
    };
    if p1 != 0 {
        return blocked("CAN port 1 already runs another protocol. Set the CAN ports on the Parameters page.");
    }
    let protocol = rounded(values, "CAN_D1_PROTOCOL");
    if protocol.is_some_and(|p| p != CAN_PROTOCOL_DRONECAN) && rounded_or(values, "CAN_P2_DRIVER", 0) == 1 {
        return blocked("CAN driver 1 runs another protocol for CAN port 2. Set the CAN ports on the Parameters page.");
    }
    let mut writes = vec![json!({"param": "CAN_P1_DRIVER", "value": 1.0})];
    if protocol.is_some_and(|p| p != CAN_PROTOCOL_DRONECAN) {
        writes.push(json!({"param": "CAN_D1_PROTOCOL", "value": CAN_PROTOCOL_DRONECAN as f64}));
    }
    DroneCan {
        running: false,
        writes,
        blocked: String::new(),
    }
}

fn can_write_label(param: &str) -> String {
    tbl().text("CAN_WRITE_LABELS", param).to_string()
}

/// Why RNGFND1's serial port cannot be set from here, or "".
fn serial_rangefinder_conflict(values: &Values) -> &'static str {
    match spec_of("range", values.get("RNGFND2_TYPE").copied()) {
        Some(spec) if spec["bus"].as_str() == Some(BUS_SERIAL) => {
            "RNGFND2 is a serial rangefinder too, and ArduPilot hands serial ports to rangefinders in order. Set the ports on the Parameters page."
        }
        _ => "",
    }
}

fn protocol_of_bus(bus: &str) -> Option<i64> {
    tbl().get("SERIAL_PROTOCOL_BY_BUS").get(bus).and_then(Value::as_i64)
}

/// The picker entries for one sensor, each carrying everything it needs to
/// start. A serial entry carries `port_writes` (`{port}` is the SERIAL number
/// the operator picks) and `port_clear` (the other ports on the same exclusive
/// protocol, which would otherwise take the sensor's place). A DroneCAN entry
/// carries the CAN writes as `extra`, or `blocked` when the CAN ports cannot be
/// changed safely. An I2C entry whose driver needs an address carries it too,
/// unless the sensor is already that type and has one.
fn driver_entries(values: &Values, kind: &str, ports: &[Port]) -> Vec<Value> {
    let tp = type_param(kind);
    let Some(current) = rounded(values, tp) else {
        return Vec::new();
    };
    let addr_param = (kind == "range").then_some("RNGFND1_ADDR");
    let serial_blocked = if kind == "range" {
        serial_rangefinder_conflict(values)
    } else {
        ""
    };
    let mut can: Option<DroneCan> = None;

    let mut entries = Vec::new();
    for spec in types_of(kind) {
        let value = spec["value"].as_i64().unwrap_or(0);
        let bus = spec["bus"].as_str().unwrap_or("");
        let mut entry = Map::new();
        entry.insert("id".into(), json!(format!("{tp}:{value}")));
        entry.insert("label".into(), spec["label"].clone());
        entry.insert("param".into(), json!(tp));
        entry.insert("value".into(), json!(value));
        entry.insert("serial".into(), json!(false));
        entry.insert("wiring".into(), json!(wiring(values, bus)));
        let mut extra: Vec<Value> = Vec::new();
        if let Some(protocol) = protocol_of_bus(bus) {
            entry.insert("serial".into(), json!(true));
            let mut writes = vec![json!({"param": "SERIAL{port}_PROTOCOL", "value": protocol as f64})];
            if truthy(spec.get("baud")) {
                writes.push(json!({"param": "SERIAL{port}_BAUD", "value": float_of(spec.get("baud"))}));
            }
            entry.insert("port_writes".into(), Value::Array(writes));
            let clear: Vec<Value> = if exclusive(protocol) {
                ports_with(ports, protocol)
                    .into_iter()
                    .map(|n| json!({"port": n, "param": format!("SERIAL{n}_PROTOCOL"), "value": SERIAL_PROTOCOL_NONE as f64}))
                    .collect()
            } else {
                Vec::new()
            };
            entry.insert("port_clear".into(), Value::Array(clear));
            if ports.is_empty() {
                entry.insert(
                    "blocked".into(),
                    json!("This firmware reports no SERIALn_PROTOCOL parameters, so the port cannot be set from here."),
                );
            } else if protocol == SERIAL_PROTOCOL_RANGEFINDER && !serial_blocked.is_empty() {
                entry.insert("blocked".into(), json!(serial_blocked));
            }
        }
        if bus == BUS_CAN {
            let c = can.get_or_insert_with(|| dronecan(values));
            if !c.blocked.is_empty() {
                entry.insert("blocked".into(), json!(c.blocked));
            }
            extra.extend(c.writes.iter().cloned());
        }
        if let Some(addr_param) = addr_param {
            if spec.get("addr").is_some() && values.contains_key(addr_param) {
                let have = rounded_or(values, addr_param, 0);
                if current != value || (truthy(spec.get("addr")) && have == 0) {
                    extra.push(json!({"param": addr_param, "value": float_of(spec.get("addr"))}));
                }
            }
        }
        if !extra.is_empty() {
            entry.insert("extra".into(), Value::Array(extra));
        }
        entries.push(Value::Object(entry));
    }

    if current != 0 && spec_of(kind, Some(current as f64)).is_none() {
        entries.push(json!({
            "id": format!("{tp}:{current}"),
            "label": format!("{tp} = {current}, not in this list"),
            "param": tp, "value": current, "serial": false,
        }));
    }
    entries
}

/// What the running driver still lacks, read back from the vehicle.
fn driver_problems(values: &Values, kind: &str, spec: &Value, ports: &[Port]) -> Vec<String> {
    let mut problems = Vec::new();
    let bus = spec["bus"].as_str().unwrap_or("");
    if let Some(protocol) = protocol_of_bus(bus) {
        let on = ports_with(ports, protocol);
        if on.is_empty() {
            let name = tbl()
                .get("SERIAL_PROTOCOL_NAMES")
                .get(protocol.to_string())
                .and_then(Value::as_str)
                .unwrap_or("");
            problems.push(format!(
                "No serial port is set to {name} (SERIALn_PROTOCOL = {protocol}), so the driver has nothing to read."
            ));
        } else if truthy(spec.get("baud")) {
            // An exclusive protocol is read on its first port only; MSP on any.
            let read: &[i64] = if exclusive(protocol) { &on[..1] } else { &on };
            let want = baud_rate(float_of(spec.get("baud")));
            let rates: Vec<(i64, i64)> = read
                .iter()
                .filter_map(|n| values.get(&format!("SERIAL{n}_BAUD")).map(|v| (*n, baud_rate(*v))))
                .collect();
            if !rates.is_empty() && rates.iter().all(|(_, rate)| *rate != want) {
                let (n, rate) = rates[0];
                problems.push(format!(
                    "SERIAL{n} runs at {rate} baud, and this sensor talks at {want}."
                ));
            }
        }
    }
    if bus == BUS_CAN && !dronecan(values).running {
        problems.push("No CAN port runs DroneCAN, so nothing reaches the driver.".to_string());
    }
    if kind == "range" {
        let addr = int_of(spec.get("addr"));
        if addr != 0 {
            let have = values.get("RNGFND1_ADDR").map_or(addr, |v| round_int(*v));
            if have == 0 {
                problems.push(format!(
                    "RNGFND1_ADDR is 0, and this driver needs the sensor's I2C address ({addr})."
                ));
            }
        }
        if (bus == BUS_ANALOG || bus == BUS_PWM) && rounded_or(values, "RNGFND1_PIN", 0) < 0 {
            problems.push("RNGFND1_PIN is not set, so the driver has no input pin to read.".to_string());
        }
    }
    problems
}

/// The port the picker opens on: the running sensor's, else the first free one.
fn default_port(ports: &[Port], spec: Option<&Value>) -> Option<i64> {
    let protocol = spec.and_then(|s| s["bus"].as_str()).and_then(protocol_of_bus);
    if let Some(protocol) = protocol {
        if let Some(first) = ports_with(ports, protocol).first() {
            return Some(*first);
        }
    }
    ports_with(ports, SERIAL_PROTOCOL_NONE).first().copied()
}

struct FlowEstimator {
    using: Vec<i64>,
    writes: Vec<Value>,
    note: String,
}

/// Which EKF3 source set uses optical flow, and the writes that give it one.
/// With no GPS configured the primary set becomes the flow set, which is the
/// wiki's setup for flight without GPS. With a GPS, set 1 stays on it and flow
/// goes into the first free one of sets 2 and 3, to be selected in flight with an
/// RC switch. A set counts as free while it has no horizontal position, velocity
/// or vertical velocity source; one that has is somebody's configuration and is
/// not overwritten.
fn flow_estimator(values: &Values) -> FlowEstimator {
    let using: Vec<i64> = [1, 2, 3]
        .into_iter()
        .filter(|k| rounded_or(values, &format!("EK3_SRC{k}_VELXY"), -1) == EK3_VELXY_FLOW)
        .collect();
    let mut target: Option<i64> = using.first().copied();
    let mut writes: Vec<Value> = Vec::new();
    let mut blocked = String::new();
    if target.is_none() {
        if !values.contains_key("EK3_SRC1_VELXY") {
            blocked =
                "This firmware has no EK3_SRC source sets, so the estimator half cannot be set from here.".to_string();
        } else if gps_configured(values) == Some(false) {
            target = Some(1);
        } else {
            target = [2, 3].into_iter().find(|k| {
                ["POSXY", "VELXY", "VELZ"]
                    .iter()
                    .all(|suffix| rounded(values, &format!("EK3_SRC{k}_{suffix}")) == Some(0))
            });
            if target.is_none() {
                blocked = "EKF source sets 2 and 3 are both in use. Point one of them at optical flow (EK3_SRCn_VELXY = 5) on the Parameters page.".to_string();
            }
        }
        if let Some(t) = target {
            for item in tbl().array("FLOW_SOURCE_SET") {
                let suffix = item[0].as_str().unwrap_or("");
                let value = item[1].as_i64().unwrap_or(0);
                let name = format!("EK3_SRC{t}_{suffix}");
                if rounded(values, &name).is_some_and(|v| v != value) {
                    writes.push(json!({"param": name, "value": value as f64, "label": item[2]}));
                }
            }
        }
    }
    if let Some(options) = rounded(values, "EK3_SRC_OPTIONS") {
        if options & EK3_FUSE_ALL_VELOCITIES != 0 {
            writes.push(json!({
                "param": "EK3_SRC_OPTIONS",
                "value": (options & !EK3_FUSE_ALL_VELOCITIES) as f64,
                "label": "Fuse only the selected set's velocities, as the wiki sets it for flow",
            }));
        }
    }

    let note = if let Some(first) = using.first() {
        format!("Flow is the horizontal velocity source of EKF source set {first}.")
    } else if target == Some(1) {
        "No GPS is configured, so On makes flow the primary source: no position, flow for velocity, the barometer for height and the compass for heading, as the ArduPilot wiki sets it for flight without GPS.".to_string()
    } else if let Some(t) = target {
        let position = if t == 2 { "middle" } else { "high" };
        format!(
            "GPS stays EKF source set 1, and On sets flow up as source set {t}. Switch to it in flight with an RC switch set to EKF source (RCx_OPTION = 90), {position} position."
        )
    } else {
        blocked
    };
    FlowEstimator { using, writes, note }
}

/// Puts back every source set that reads optical flow. Set 1 returns to its GPS
/// defaults where flow had emptied it; sets 2 and 3 lose their horizontal
/// velocity source, which is what they had before.
fn flow_estimator_off(values: &Values, using: &[i64]) -> Vec<Value> {
    let mut writes = Vec::new();
    for &k in using {
        if k == 1 {
            writes.push(json!({"param": "EK3_SRC1_VELXY", "value": 3.0}));
            for suffix in ["POSXY", "VELZ"] {
                let name = format!("EK3_SRC1_{suffix}");
                if rounded(values, &name) == Some(0) {
                    writes.push(json!({"param": name, "value": 3.0}));
                }
            }
        } else {
            writes.push(json!({"param": format!("EK3_SRC{k}_VELXY"), "value": 0.0}));
        }
    }
    writes
}

/// The shared toggle shape: the picker, the state, and the write lists.
#[allow(clippy::too_many_arguments)]
fn toggle(
    values: &Values,
    kind: &str,
    label: &str,
    ports: &[Port],
    enable: Vec<Value>,
    disable: Vec<Value>,
    problems: Vec<String>,
    detail_suffix: &str,
) -> Option<Value> {
    let tp = type_param(kind);
    let current = rounded(values, tp)?;
    let spec = spec_of(kind, Some(current as f64));
    let drivers = driver_entries(values, kind, ports);

    let (mut detail, state, problems) = if current == 0 {
        ("No driver enabled".to_string(), "off", Vec::new())
    } else if spec.is_none() {
        (format!("{tp} = {current}, not in this list"), "on", problems)
    } else {
        let state = if problems.is_empty() { "on" } else { "partial" };
        (s(spec.unwrap(), "label"), state, problems)
    };
    detail += detail_suffix;

    let selected = if current != 0 {
        format!("{tp}:{current}")
    } else {
        drivers.first().and_then(|d| d["id"].as_str()).unwrap_or("").to_string()
    };
    let mut out = json!({
        "label": label,
        "enabled": state == "on",
        "state": state,
        "problems": problems,
        "detail": detail,
        "drivers": drivers,
        "selected": selected,
        "enable": enable,
        "disable": disable,
        "clear": if current != 0 { json!([tp]) } else { json!([]) },
        "reboot": true,
    });
    if drivers.iter().any(|d| d["serial"].as_bool() == Some(true)) {
        out["ports"] = Value::Array(
            ports
                .iter()
                .map(|p| json!({"value": p.value, "label": p.label}))
                .collect(),
        );
        out["port"] = default_port(ports, spec).map_or(Value::Null, |p| json!(p));
    }
    Some(out)
}

fn rangefinder_section(values: &Values, ports: &[Port]) -> Option<Value> {
    let on_off = table_options("ON_OFF_OPTIONS");
    let spec = spec_of("range", values.get("RNGFND1_TYPE").copied());
    let problems = spec.map_or_else(Vec::new, |sp| driver_problems(values, "range", sp, ports));

    let mut enable = Vec::new();
    if values.contains_key("RNGFND_LANDING") {
        enable.push(json!({"param": "RNGFND_LANDING", "value": 1.0}));
    }
    // Nothing turns the rangefinder into the height source, but a set that was
    // pointed at it (by hand, or by an older Corvus) must not be left pointing at
    // a sensor that is gone: that is an estimator with no height at all.
    let disable: Vec<Value> = [1, 2, 3]
        .into_iter()
        .filter(|k| rounded_or(values, &format!("EK3_SRC{k}_POSZ"), 1) == 2)
        .map(|k| json!({"param": format!("EK3_SRC{k}_POSZ"), "value": 1.0}))
        .collect();

    let suffix = values.get("EK3_SRC1_POSZ").map_or(String::new(), |posz| {
        format!(
            " \u{b7} height source: {}",
            option_label(&table_options("EK3_POSZ_OPTIONS"), *posz)
        )
    });
    let toggle = toggle(
        values,
        "range",
        "Distance sensor",
        ports,
        enable,
        disable,
        problems,
        &suffix,
    )?;
    let fields = present(vec![
        enum_("RNGFND1_ORIENT", "Orientation", values, &table_options("RANGEFINDER_ORIENT_OPTIONS"))
            .hint("Down is what a landing or terrain-following sensor wants."),
        number("RNGFND1_MIN_CM", "Shortest reading", values).unit("cm").step(1).min(0),
        number("RNGFND1_MAX_CM", "Longest reading", values).unit("cm").step(10).min(0),
        number("RNGFND1_MIN", "Shortest reading", values).unit("m").step(0.01).min(0),
        number("RNGFND1_MAX", "Longest reading", values).unit("m").step(0.1).min(0),
        number("RNGFND1_GNDCLEAR", "Ground clearance", values)
            .unit("cm")
            .step(1)
            .min(0)
            .hint("Distance the sensor reads with the vehicle on the ground."),
        number("RNGFND1_GNDCLR", "Ground clearance", values)
            .unit("m")
            .step(0.01)
            .min(0)
            .hint("Distance the sensor reads with the vehicle on the ground."),
        number("RNGFND1_ADDR", "I2C address", values).step(1).min(0),
        number("RNGFND1_PIN", "Analog or PWM pin", values).step(1),
        number("RNGFND1_SCALING", "Scaling", values).step(0.01),
        number("RNGFND1_OFFSET", "Offset", values).step(0.01),
        number("RNGFND1_POS_X", "Mounting offset, X", values).unit("m").step(0.01),
        number("RNGFND1_POS_Y", "Mounting offset, Y", values).unit("m").step(0.01),
        number("RNGFND1_POS_Z", "Mounting offset, Z", values).unit("m").step(0.01),
        enum_("RNGFND_LANDING", "Use for landing", values, &on_off),
        enum_(
            "SURFTRAK_MODE",
            "Surface tracking",
            values,
            &opts(&[(0, "Disabled"), (1, "Track the ground")]),
        ),
        enum_("EK3_SRC1_POSZ", "Primary height source", values, &table_options("EK3_POSZ_OPTIONS")).hint(
            "Leave on Barometer. ArduPilot uses a downward rangefinder for surface tracking and landing on its own; Rangefinder here is only for indoor flight over a flat floor.",
        ),
    ]);
    Some(json!({
        "id": "rangefinder", "title": "Distance sensor", "kind": "toggle",
        "group": "sensors", "icon": "radar",
        "short": "A downward lidar or sonar: height above the ground under the aircraft.",
        "toggle": toggle,
        "presets": presets_for("range", values, ports),
        "fields": fields,
        "hint": "Switching it on starts the driver with everything it needs: the serial port and its baud rate, the CAN port, or the I2C address. ArduPilot then uses a downward rangefinder on its own for surface tracking and landing, so the estimator's height source stays on the barometer.",
    }))
}

fn flow_section(values: &Values, ports: &[Port]) -> Option<Value> {
    let spec = spec_of("flow", values.get("FLOW_TYPE").copied());
    let estimator = flow_estimator(values);
    let mut problems = spec.map_or_else(Vec::new, |sp| driver_problems(values, "flow", sp, ports));
    if estimator.using.is_empty() {
        problems
            .push("No EKF source set uses optical flow (EK3_SRCn_VELXY = 5), so the estimator ignores it.".to_string());
    }

    let enable: Vec<Value> = estimator
        .writes
        .iter()
        .map(|w| json!({"param": w["param"], "value": w["value"]}))
        .collect();
    let disable = flow_estimator_off(values, &estimator.using);
    let suffix = match estimator.using.first() {
        Some(first) => format!(" \u{b7} estimator: source set {first}"),
        None => " \u{b7} estimator: not used".to_string(),
    };
    let mut toggle = toggle(
        values,
        "flow",
        "Optical flow",
        ports,
        enable,
        disable,
        problems,
        &suffix,
    )?;
    toggle["note"] = json!(estimator.note);
    let posxy = table_options("EK3_POSXY_OPTIONS");
    let velxy = table_options("EK3_VELXY_OPTIONS");
    let fields = present(vec![
        number("FLOW_ORIENT_YAW", "Mounting yaw", values)
            .unit("cdeg")
            .step(100)
            .hint("Centidegrees, ArduPilot's own unit. 0 is the sensor's forward axis aligned with the aircraft's."),
        number("FLOW_POS_X", "Mounting offset, X", values).unit("m").step(0.01),
        number("FLOW_POS_Y", "Mounting offset, Y", values).unit("m").step(0.01),
        number("FLOW_POS_Z", "Mounting offset, Z", values)
            .unit("m")
            .step(0.01)
            .hint("Positive down from the centre of gravity."),
        number("FLOW_FXSCALER", "X scale correction", values).step(1),
        number("FLOW_FYSCALER", "Y scale correction", values).step(1),
        number("FLOW_ADDR", "I2C address", values).step(1).min(0),
        number("EK3_FLOW_DELAY", "Measurement delay", values)
            .unit("ms")
            .step(1)
            .min(0),
        enum_("EK3_SRC1_POSXY", "Position source, set 1", values, &posxy),
        enum_("EK3_SRC1_VELXY", "Velocity source, set 1", values, &velxy),
        enum_("EK3_SRC2_VELXY", "Velocity source, set 2", values, &velxy)
            .hint("Set 2 and 3 are selected in flight with an RC switch set to EKF source (RCx_OPTION = 90)."),
    ]);
    Some(json!({
        "id": "flow", "title": "Optical flow", "kind": "toggle",
        "group": "sensors", "icon": "scan-line",
        "short": "A downward camera: horizontal velocity without GPS. Needs a distance sensor.",
        "toggle": toggle,
        "presets": presets_for("flow", values, ports),
        "fields": fields,
        "hint": "A downward camera for horizontal velocity. It needs a distance sensor to scale what it sees, so switch that on first; a copter flying on flow will not climb above that sensor's longest reading. Calibrate it in flight with an RC switch set to optical flow calibration (RCx_OPTION = 158).",
    }))
}

// ---------------------------------------------------------------------------
// Hardware presets
// ---------------------------------------------------------------------------

/// The parameter this firmware stores `name` in, with the value in its unit
/// (4.6 moved the rangefinder distances to metres; older firmware has them in
/// centimetres).
fn param_for(name: &str, value: f64, values: &Values) -> Option<(String, f64)> {
    if values.contains_key(name) {
        return Some((name.to_string(), value));
    }
    let alias = tbl().get("LENGTH_ALIASES").get(name)?;
    let target = alias[0].as_str()?;
    if values.contains_key(target) {
        let factor = alias[1].as_f64()?;
        return Some((target.to_string(), (value * factor).round_ties_even()));
    }
    None
}

fn preset_type(preset: &Value, kind: &str) -> Option<i64> {
    let tp = type_param(kind);
    preset
        .get("params")?
        .as_array()?
        .iter()
        .find(|p| p[0].as_str() == Some(tp))
        .map(|p| p[1].as_f64().unwrap_or(0.0) as i64)
}

/// One catalogue entry, turned into what this vehicle can do with it: every write
/// is listed with its reason, a parameter this firmware lacks is named under
/// `missing`, and a module whose driver cannot be started from here is offered
/// with the reason and nothing to apply.
fn resolve_preset(preset: &Value, kind: &str, values: &Values, ports: &[Port]) -> Value {
    let t = tbl();
    let mut reason = String::new();
    let provides: &[Value] = preset
        .get("provides")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    for covered in provides {
        let covered = covered.as_str().unwrap_or("");
        if !values.contains_key(type_param(covered)) {
            reason = format!(
                "This firmware does not carry {}, so this module cannot be started from here.",
                type_param(covered)
            );
        }
    }
    let protocol = preset.get("serial").and_then(Value::as_i64);
    if reason.is_empty() {
        if let Some(protocol) = protocol {
            if ports.is_empty() {
                reason = "This firmware reports no SERIALn_PROTOCOL parameters, so the port cannot be set from here."
                    .to_string();
            } else if protocol == SERIAL_PROTOCOL_RANGEFINDER {
                reason = serial_rangefinder_conflict(values).to_string();
            }
        }
    }
    let can = truthy(preset.get("can")).then(|| dronecan(values));
    if reason.is_empty() {
        if let Some(c) = &can {
            if !c.blocked.is_empty() {
                reason = c.blocked.clone();
            }
        }
    }
    let supported = reason.is_empty();

    let mut writes: Vec<Value> = Vec::new();
    let mut missing: Vec<Value> = Vec::new();
    let mut clear: Vec<Value> = Vec::new();
    let mut note = s(preset, "note");
    if supported {
        if let Some(protocol) = protocol {
            let name = t
                .get("SERIAL_PROTOCOL_NAMES")
                .get(protocol.to_string())
                .and_then(Value::as_str)
                .unwrap_or("");
            writes.push(json!({
                "param": "SERIAL{port}_PROTOCOL", "value": protocol as f64, "port_param": true,
                "label": format!("The port the module is wired to speaks {name}"),
            }));
            if truthy(preset.get("baud")) {
                let baud = float_of(preset.get("baud"));
                writes.push(json!({
                    "param": "SERIAL{port}_BAUD", "value": baud, "port_param": true,
                    "label": format!("{} baud", baud_rate(baud)),
                }));
            }
            if exclusive(protocol) {
                clear = ports_with(ports, protocol)
                    .into_iter()
                    .map(|n| json!({"port": n, "param": format!("SERIAL{n}_PROTOCOL"), "value": SERIAL_PROTOCOL_NONE as f64}))
                    .collect();
            }
        }
        if let Some(c) = &can {
            for w in &c.writes {
                let mut w = w.clone();
                w["label"] = json!(can_write_label(w["param"].as_str().unwrap_or("")));
                writes.push(w);
            }
        }
        for item in preset
            .get("params")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
        {
            let param = item[0].as_str().unwrap_or("");
            match param_for(param, float_of(item.get(1)), values) {
                None => missing.push(json!(param)),
                Some((p, v)) => writes.push(json!({"param": p, "value": v, "label": item[2]})),
            }
        }
        if truthy(preset.get("flow_estimator")) {
            let estimator = flow_estimator(values);
            writes.extend(estimator.writes.iter().cloned());
            if !estimator.note.is_empty() {
                note = format!("{note} {}", estimator.note).trim().to_string();
            }
        }
    }

    let type_value = preset_type(preset, kind);
    let current = values.get(type_param(kind)).copied();
    let wiring_text = if truthy(preset.get("can")) {
        can_wiring(values)
    } else if protocol.is_some() {
        t.get("SERIAL_WIRING").as_str().unwrap_or("").to_string()
    } else {
        String::new()
    };
    json!({
        "id": preset["id"],
        "label": preset["label"],
        "vendor": s(preset, "vendor"),
        "model": s(preset, "model"),
        "bus": s(preset, "bus"),
        "summary": s(preset, "summary"),
        "note": note,
        "wiring": wiring_text,
        "serial": protocol.is_some(),
        "supported": supported,
        "unsupported": reason,
        "driver": type_value.map_or(String::new(), |v| format!("{}:{v}", type_param(kind))),
        "writes": writes,
        "kept": [],
        "clear": clear,
        "missing": missing,
        "active": type_value.is_some() && current.is_some_and(|c| Some(round_int(c)) == type_value),
        "reboot": truthy(preset.get("reboot")) && supported,
    })
}

fn presets_for(kind: &str, values: &Values, ports: &[Port]) -> Vec<Value> {
    tbl()
        .array("SENSOR_PRESETS")
        .iter()
        .filter(|p| {
            p.get("provides")
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().any(|k| k.as_str() == Some(kind)))
        })
        .map(|p| resolve_preset(p, kind, values, ports))
        .collect()
}

// ---------------------------------------------------------------------------
// The parameter superset
// ---------------------------------------------------------------------------

const RANGEFINDER_DRIVER_PARAMS: [&str; 18] = [
    "RNGFND1_TYPE",
    "RNGFND1_ORIENT",
    "RNGFND1_MIN_CM",
    "RNGFND1_MAX_CM",
    "RNGFND1_MIN",
    "RNGFND1_MAX",
    "RNGFND1_GNDCLEAR",
    "RNGFND1_GNDCLR",
    "RNGFND1_ADDR",
    "RNGFND1_PIN",
    "RNGFND1_SCALING",
    "RNGFND1_OFFSET",
    "RNGFND1_POS_X",
    "RNGFND1_POS_Y",
    "RNGFND1_POS_Z",
    "RNGFND_LANDING",
    "SURFTRAK_MODE",
    "RNGFND2_TYPE",
];

const FLOW_DRIVER_PARAMS: [&str; 9] = [
    "FLOW_TYPE",
    "FLOW_ORIENT_YAW",
    "FLOW_POS_X",
    "FLOW_POS_Y",
    "FLOW_POS_Z",
    "FLOW_FXSCALER",
    "FLOW_FYSCALER",
    "FLOW_ADDR",
    "EK3_FLOW_DELAY",
];

/// Every parameter this page may need, across all four vehicle families, for one
/// batched read. A copter answers for `FS_THR_ENABLE` and not `FS_SHORT_ACTN`, a
/// plane the other way round, and neither is an error.
pub fn param_names() -> Vec<String> {
    let mut names: Vec<String> = [
        "FENCE_ENABLE",
        "FENCE_TYPE",
        "FENCE_ACTION",
        "FENCE_ALT_MAX",
        "FENCE_ALT_MIN",
        "FENCE_RADIUS",
        "FENCE_MARGIN",
        "FENCE_TOTAL",
        "FENCE_AUTOENABLE",
        "FENCE_OPTIONS",
        "AVOID_ENABLE",
        "AVOID_MARGIN",
        "RTL_ALT",
        "RTL_ALT_FINAL",
        "RTL_CLIMB_MIN",
        "RTL_CONE_SLOPE",
        "RTL_LOIT_TIME",
        "RTL_SPEED",
        "RTL_ALT_TYPE",
        "RTL_RADIUS",
        "RTL_AUTOLAND",
        "LAND_SPEED",
        "LAND_ALT_LOW",
        "FS_THR_ENABLE",
        "FS_THR_VALUE",
        "FS_GCS_ENABLE",
        "FS_GCS_TIMEOUT",
        "FS_OPTIONS",
        "FS_EKF_ACTION",
        "FS_EKF_THRESH",
        "FS_CRASH_CHECK",
        "FS_VIBE_ENABLE",
        "FS_DR_ENABLE",
        "FS_DR_TIMEOUT",
        "FS_SHORT_ACTN",
        "FS_SHORT_TIMEOUT",
        "FS_LONG_ACTN",
        "FS_LONG_TIMEOUT",
        "THR_FAILSAFE",
        "THR_FS_VALUE",
        "FS_GCS_ENABL",
        "FS_ACTION",
        "FS_TIMEOUT",
        "FS_CRASH_CHECK",
        "ARMING_CHECK",
        "ARMING_RUDDER",
        "DISARM_DELAY",
        "ARMING_REQUIRE",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    names.extend(RANGEFINDER_DRIVER_PARAMS.iter().map(|s| s.to_string()));
    names.extend(FLOW_DRIVER_PARAMS.iter().map(|s| s.to_string()));
    for k in 1..=3 {
        for suffix in ["POSXY", "VELXY", "POSZ", "VELZ", "YAW"] {
            names.push(format!("EK3_SRC{k}_{suffix}"));
        }
    }
    names.push("EK3_SRC_OPTIONS".to_string());
    for n in 1..=8 {
        for p in ["PROTOCOL", "BAUD"] {
            names.push(format!("SERIAL{n}_{p}"));
        }
    }
    names.extend(
        [
            "CAN_P1_DRIVER",
            "CAN_P2_DRIVER",
            "CAN_D1_PROTOCOL",
            "CAN_D2_PROTOCOL",
            "GPS_TYPE",
            "GPS1_TYPE",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    // FS_CRASH_CHECK is on both the Copter and the Rover list, and a duplicate
    // would be fetched twice on every page load.
    let mut seen = std::collections::HashSet::new();
    names.retain(|n| seen.insert(n.clone()));
    names
}

/// The parameters [`return_profile`] reads for `vehicle`: a copter keeps its
/// heights in centimetres; a plane had `ALT_HOLD_RTL` (centimetres, -1 for
/// "where it is") and has `RTL_ALTITUDE` in metres on the releases that renamed
/// it, so both are asked for and the one that answers is used.
pub fn return_param_names(vehicle: &str) -> Vec<String> {
    let names: &[&str] = if vehicle == "plane" {
        &["RTL_ALTITUDE", "ALT_HOLD_RTL"]
    } else {
        &["RTL_ALT", "RTL_ALT_FINAL"]
    };
    names.iter().map(|s| s.to_string()).collect()
}

/// A positive height in centimetres as metres, else null ("where it is").
fn centimetres(value: Option<f64>) -> Value {
    match value {
        Some(v) if !v.is_nan() && v > 0.0 => crate::json::num(v / 100.0),
        _ => Value::Null,
    }
}

/// How ArduPilot flies a Return, in the shape the PX4 profile has. A copter
/// climbs to `RTL_ALT` when it is lower (0: it returns where it is), and over
/// home goes to `RTL_ALT_FINAL`, where 0 is a landing and anything else a hover
/// it stays in. A plane flies home at its return altitude and circles there; it
/// lands only through a landing sequence this planner does not write.
pub fn return_profile(values: &Values, vehicle: &str) -> Value {
    if vehicle == "plane" {
        let climb = match values.get("RTL_ALTITUDE") {
            Some(&a) if !a.is_nan() && a > 0.0 => crate::json::num(a),
            _ => centimetres(values.get("ALT_HOLD_RTL").copied()),
        };
        return json!({"known": !values.is_empty(), "climb_to": climb, "arrive_at": Value::Null, "hold": false});
    }
    let final_alt = centimetres(values.get("RTL_ALT_FINAL").copied());
    json!({
        "known": !values.is_empty(),
        "climb_to": centimetres(values.get("RTL_ALT").copied()),
        "hold": !final_alt.is_null(),
        "arrive_at": final_alt,
    })
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn fence_section(values: &Values) -> Option<Value> {
    let on_off = table_options("ON_OFF_OPTIONS");
    let fields = present(vec![
        enum_("FENCE_ENABLE", "Fence", values, &on_off),
        bitmask(
            "FENCE_TYPE",
            "What the fence limits",
            values,
            table_options("FENCE_TYPE_BITS"),
        )
        .hint("A fence with no type selected limits nothing, whatever FENCE_ENABLE says."),
        number("FENCE_ALT_MAX", "Maximum altitude", values)
            .unit("m")
            .step(1)
            .min(0),
        number("FENCE_ALT_MIN", "Minimum altitude", values).unit("m").step(1),
        number("FENCE_RADIUS", "Circle radius", values)
            .unit("m")
            .step(1)
            .min(0)
            .hint("Distance from home the vehicle may reach."),
        number("FENCE_MARGIN", "Margin", values)
            .unit("m")
            .step(0.5)
            .min(0)
            .hint("How far inside the fence the vehicle starts to be pushed back."),
        enum_("FENCE_AUTOENABLE", "Enable automatically", values, &on_off),
        enum_("AVOID_ENABLE", "Obstacle avoidance", values, &on_off),
        number("AVOID_MARGIN", "Avoidance margin", values)
            .unit("m")
            .step(0.5)
            .min(0),
    ]);
    section(
        "limits",
        "Flight limits",
        fields,
        "The envelope the vehicle is not allowed to leave, measured from the home position. What it does at the fence is set on the Geofence card.",
    )
    .map(Value::Object)
}

/// The writes that make an uploaded polygon act, in order. ArduPilot stores the
/// polygon whatever its parameters say and ignores it until `FENCE_TYPE` has the
/// polygon bit and `FENCE_ENABLE` is on. A parameter this firmware did not
/// answer for is left alone.
pub fn geofence_enable_writes(values: &Values) -> Vec<Value> {
    let mut writes = Vec::new();
    if let Some(&kind) = values.get("FENCE_TYPE") {
        if !kind.is_nan() && (kind as i64) & FENCE_TYPE_POLYGON == 0 {
            writes.push(json!({"name": "FENCE_TYPE", "value": (kind as i64) | FENCE_TYPE_POLYGON}));
        }
    }
    if let Some(&enabled) = values.get("FENCE_ENABLE") {
        if enabled != 1.0 {
            writes.push(json!({"name": "FENCE_ENABLE", "value": 1}));
        }
    }
    writes
}

/// The Geofence card, in the shape the PX4 schema has. Present whenever the
/// vehicle answered at all; the action labels differ per vehicle class.
fn geofence_section(values: &Values, vehicle: &str) -> Option<Value> {
    if values.is_empty() {
        return None;
    }
    let actions_table = tbl().get("FENCE_ACTION_OPTIONS");
    let actions = actions_table
        .get(vehicle)
        .or_else(|| actions_table.get("copter"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let fields = present(vec![enum_(
        "FENCE_ACTION",
        "Action when leaving the area",
        values,
        &actions,
    )
    .hint("Also applies to the altitude and circle limits under Flight limits.")]);
    let writes = geofence_enable_writes(values);
    let inactive = if !writes.is_empty() {
        "The fence is switched off or does not include polygons. Uploading the area switches it on."
    } else if values.get("FENCE_ACTION") == Some(&0.0) {
        "The action is Report only, so leaving the area only warns."
    } else {
        ""
    };
    Some(json!({
        "id": "geofence", "title": "Geofence", "kind": "geofence", "fields": fields,
        "hint": "Draw the area the vehicle may fly in. Leaving it triggers the action below.",
        "enable_writes": writes,
        "inactive": inactive,
    }))
}

fn rtl_section(values: &Values) -> Option<Value> {
    let fields = present(vec![
        number("RTL_ALT", "Return altitude", values)
            .unit("cm")
            .step(100)
            .min(0)
            .hint("Centimetres above home, ArduPilot's own unit. 1500 is 15 m. 0 returns at the current altitude."),
        number("RTL_ALT_FINAL", "Final altitude", values)
            .unit("cm")
            .step(100)
            .hint("Altitude it holds at home before landing. 0 lands."),
        number("RTL_CLIMB_MIN", "Minimum climb", values)
            .unit("cm")
            .step(100)
            .min(0)
            .hint("Climbed before turning for home, whatever the return altitude is."),
        number("RTL_CONE_SLOPE", "Return cone slope", values)
            .step(0.1)
            .min(0)
            .hint("Close to home the return altitude is reduced along this slope, so a vehicle hovering overhead does not climb first."),
        number("RTL_LOIT_TIME", "Hold before landing", values).unit("ms").step(100).min(0),
        number("RTL_SPEED", "Return speed", values)
            .unit("cm/s")
            .step(50)
            .min(0)
            .hint("0 uses the normal waypoint speed."),
        number("RTL_RADIUS", "Loiter radius", values).unit("m").step(1).hint("Fixed wing only."),
        enum_("RTL_AUTOLAND", "Automatic landing sequence", values, &table_options("ON_OFF_OPTIONS")),
        number("LAND_SPEED", "Landing descent rate", values).unit("cm/s").step(10).min(0),
        number("LAND_ALT_LOW", "Slow-descent altitude", values).unit("cm").step(50).min(0),
    ]);
    section(
        "rtl",
        "Return to Launch",
        fields,
        "The path home, flown on an RTL command and by every failsafe whose action is Return.",
    )
    .map(Value::Object)
}

fn failsafe_section(values: &Values, vehicle: &str) -> Option<Value> {
    let on_off = table_options("ON_OFF_OPTIONS");
    let copter = table_options("COPTER_FAILSAFE_ACTION_OPTIONS");
    let fields = present(vec![
        enum_("FS_THR_ENABLE", "RC loss", values, &copter),
        number("FS_THR_VALUE", "RC-loss throttle threshold", values)
            .unit("PWM")
            .step(1)
            .hint("A throttle channel below this counts as a lost receiver."),
        enum_("FS_GCS_ENABLE", "Ground station link loss", values, &copter),
        number("FS_GCS_TIMEOUT", "Link-loss timeout", values)
            .unit("s")
            .step(0.5)
            .min(0),
        enum_("THR_FAILSAFE", "Throttle failsafe", values, &on_off),
        number("THR_FS_VALUE", "Throttle failsafe threshold", values)
            .unit("PWM")
            .step(1),
        enum_(
            "FS_SHORT_ACTN",
            "Short failsafe action",
            values,
            &table_options("PLANE_SHORT_FAILSAFE_OPTIONS"),
        ),
        number("FS_SHORT_TIMEOUT", "Short failsafe timeout", values)
            .unit("s")
            .step(0.5)
            .min(0),
        enum_(
            "FS_LONG_ACTN",
            "Long failsafe action",
            values,
            &table_options("PLANE_LONG_FAILSAFE_OPTIONS"),
        ),
        number("FS_LONG_TIMEOUT", "Long failsafe timeout", values)
            .unit("s")
            .step(0.5)
            .min(0),
        enum_(
            "FS_ACTION",
            "Failsafe action",
            values,
            &table_options("ROVER_FAILSAFE_ACTION_OPTIONS"),
        ),
        number("FS_TIMEOUT", "Failsafe timeout", values)
            .unit("s")
            .step(0.5)
            .min(0),
        enum_(
            "FS_EKF_ACTION",
            "Estimator failure",
            values,
            &table_options("EKF_FAILSAFE_ACTION_OPTIONS"),
        ),
        enum_(
            "FS_EKF_THRESH",
            "Estimator failure threshold",
            values,
            &table_options("EKF_FAILSAFE_THRESHOLD_OPTIONS"),
        ),
        enum_("FS_CRASH_CHECK", "Crash check", values, &on_off),
        enum_("FS_VIBE_ENABLE", "Vibration failsafe", values, &on_off),
        enum_("FS_DR_ENABLE", "Dead reckoning on GPS loss", values, &on_off),
        number("FS_DR_TIMEOUT", "Dead-reckoning timeout", values)
            .unit("s")
            .step(1)
            .min(0),
        bitmask(
            "FS_OPTIONS",
            "Failsafe exceptions",
            values,
            table_options("FS_OPTIONS_BITS"),
        )
        .hint("Each bit lets one activity carry on through a failsafe that would otherwise interrupt it."),
    ]);
    section(
        "failsafe",
        "Failsafes",
        fields,
        &format!(
            "What the {vehicle} does when it loses an input it was relying on. The battery failsafes are on the Battery & Power page, with the levels that trigger them. Every BATT_ parameter lives there."
        ),
    )
    .map(Value::Object)
}

fn arming_section(values: &Values) -> Option<Value> {
    let fields =
        present(vec![
        bitmask("ARMING_CHECK", "Pre-arm checks", values, table_options("ARMING_CHECK_BITS")).hint(
            "Turning a check off does not fix what it was catching. Bit 0 enables every check and overrides the rest.",
        ),
        enum_(
            "ARMING_RUDDER",
            "Arm with the rudder stick",
            values,
            &table_options("ARMING_RUDDER_OPTIONS"),
        ),
        number("DISARM_DELAY", "Auto-disarm delay", values)
            .unit("s")
            .step(1)
            .min(0)
            .hint("Seconds on the ground at zero throttle before the vehicle disarms itself. 0 never does."),
    ]);
    section(
        "arming",
        "Arming",
        fields,
        "What the vehicle checks before it will spin a motor, and when it gives up and disarms again.",
    )
    .map(Value::Object)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Turns the raw `{param: value}` into the Safety and Sensors page description,
/// the same shape as the PX4 one: `sections` of `{id, title, hint, kind, fields[,
/// toggle]}`, `extra` for the parameters the operator added by name (already
/// normalised), and `received`. `vehicle` picks the enum labels that differ per
/// firmware family; an unrecognised class falls back to Copter's.
pub fn build(values: &Values, extra: &[String], vehicle: &str) -> Value {
    let ports = serial_ports(values);
    let mut sections: Vec<Value> = [
        fence_section(values),
        geofence_section(values, vehicle),
        rtl_section(values),
        failsafe_section(values, vehicle),
        arming_section(values),
        rangefinder_section(values, &ports),
        flow_section(values, &ports),
    ]
    .into_iter()
    .flatten()
    .collect();
    flag_flow_without_range(&mut sections);

    // A parameter this build already renders is not offered a second time.
    let rendered: Vec<&str> = sections
        .iter()
        .flat_map(|e| e.get("fields").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|f| f.get("param").and_then(Value::as_str))
        .filter(|p| !p.is_empty())
        .collect();
    let names: Vec<String> = extra
        .iter()
        .filter(|n| !rendered.contains(&n.as_str()))
        .cloned()
        .collect();
    json!({
        "stack": "ardupilot",
        "vehicle": vehicle,
        "sections": sections,
        "extra": extra_fields(&names, values),
        "extra_limit": EXTRA_PARAM_LIMIT,
        "received": values.len(),
    })
}
