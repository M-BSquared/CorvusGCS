//! The PX4 safety and sensor schema of Setup, Safety and Sensors
//! (`safety_config.py`).
//!
//! The module owns the knowledge of which PX4 parameters bound a flight and
//! which ones bring a rangefinder or an optical flow sensor online; the bridge
//! only fetches raw values and the route only serialises. Two kinds of section
//! come out of [`build`]: a plain form (`"fields"`) and a sensor that is off
//! until the operator turns it on (`"toggle"`), which carries the exact
//! ordered write list for both halves of the chain (driver, then estimator).
//!
//! Every field and every driver is a candidate only: a parameter the firmware
//! did not answer for is one field fewer, never an error, and an enum value
//! the schema does not know is kept as an extra `Unknown (n)` option.

use serde_json::{json, Value};

use super::fields::*;
use super::tables::{safety, Table};
use crate::json::Map;
use crate::protocol::common::py_g;

/// EKF2_AID_MASK bit 1, the pre v1.14 way to switch optical flow fusion on.
pub const AID_MASK_FLOW_BIT: i64 = 2;
pub const EXTRA_PARAM_LIMIT: usize = 24;
pub const CAN_ENABLE_PARAM: &str = "UAVCAN_ENABLE";
pub const CAN_ENABLE_VALUE: f64 = 2.0;
const HEIGHT_REF_RANGE: i64 = 2;
const HEIGHT_REF_BARO: f64 = 0.0;
const EXTERNAL_DRIVER_ID: &str = "external";
const BUS_SERIAL: &str = "serial";
const BUS_CAN: &str = "can";
const BUS_EXTERNAL: &str = "external";

const LANDING_PARAMS: [&str; 7] = [
    "MPC_Z_V_AUTO_DN",
    "MPC_Z_VEL_MAX_DN",
    "MPC_LAND_ALT1",
    "MPC_LAND_ALT2",
    "MPC_LAND_SPEED",
    "MPC_LAND_ALT3",
    "MPC_LAND_CRWL",
];

/// Parameters a preset may raise but must never lower.
fn raise_only(param: &str) -> bool {
    param == CAN_ENABLE_PARAM
}

fn wiring(t: &Table, bus: &str) -> &'static str {
    t.text("WIRING", bus)
}

fn str_at(v: &Value, i: usize) -> &str {
    v.get(i).and_then(Value::as_str).unwrap_or("")
}

fn int_at(v: &Value, i: usize) -> i64 {
    v.get(i).and_then(Value::as_i64).unwrap_or(0)
}

fn rounded(values: &Values, name: &str) -> Option<i64> {
    values.get(name).map(|v| round_int(*v))
}

/// Every parameter the page may need, in one flat list for one batched read.
pub fn param_names() -> Vec<String> {
    let t = safety();
    let mut names: Vec<String> = [
        "GF_ACTION",
        "GF_MAX_HOR_DIST",
        "GF_MAX_VER_DIST",
        "GF_SOURCE",
        "GF_PREDICT",
        "LNDMC_ALT_MAX",
        "RTL_TYPE",
        "RTL_RETURN_ALT",
        "RTL_DESCEND_ALT",
        "RTL_CONE_ANG",
        "RTL_MIN_DIST",
        "RTL_LAND_DELAY",
        "RTL_LOITER_RAD",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    names.extend(LANDING_PARAMS.iter().map(|s| s.to_string()));
    names.extend(
        [
            "NAV_RCL_ACT",
            "COM_RC_LOSS_T",
            "NAV_DLL_ACT",
            "COM_DL_LOSS_T",
            "COM_LOW_BAT_ACT",
            "COM_POSCTL_NAVL",
            "COM_ACT_FAIL_ACT",
            "COM_QC_ACT",
            "COM_FAIL_ACT_T",
            "COM_DISARM_LAND",
            "COM_DISARM_PRFLT",
            "EKF2_RNG_CTRL",
            "EKF2_RNG_AID",
            "EKF2_HGT_REF",
            "EKF2_RNG_A_HMAX",
            "EKF2_RNG_A_VMAX",
            "EKF2_RNG_POS_X",
            "EKF2_RNG_POS_Y",
            "EKF2_RNG_POS_Z",
            "EKF2_RNG_PITCH",
            "EKF2_RNG_DELAY",
            "EKF2_RNG_NOISE",
            "EKF2_RNG_SFE",
            "EKF2_RNG_QLTY_T",
            "EKF2_MIN_RNG",
            "MPC_ALT_MODE",
            "EKF2_OF_CTRL",
            "EKF2_AID_MASK",
            "EKF2_OF_DELAY",
            "EKF2_OF_QMIN",
            "EKF2_OF_QMIN_GND",
            "EKF2_OF_N_MIN",
            "EKF2_OF_N_MAX",
            "EKF2_OF_POS_X",
            "EKF2_OF_POS_Y",
            "EKF2_OF_POS_Z",
            "SENS_FLOW_ROT",
            "SENS_FLOW_MINHGT",
            "SENS_FLOW_MAXHGT",
            "SENS_FLOW_MAXR",
            CAN_ENABLE_PARAM,
            "UAVCAN_RNG_MIN",
            "UAVCAN_RNG_MAX",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    for user in t.array("PORT_USERS") {
        names.push(str_at(user, 0).to_string());
    }
    for table in ["RANGEFINDER_BUS_DRIVERS", "FLOW_BUS_DRIVERS"] {
        for entry in t.array(table) {
            names.push(str_at(entry, 0).to_string());
        }
    }
    for table in ["RANGEFINDER_SERIAL_DRIVERS", "FLOW_SERIAL_DRIVERS"] {
        for entry in t.array(table) {
            names.push(str_at(entry, 0).to_string());
            if let Some(model) = entry.get(1).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                names.push(model.to_string());
            }
        }
    }
    for table in ["RANGEFINDER_CAN_DRIVERS", "FLOW_CAN_DRIVERS"] {
        for entry in t.array(table) {
            names.push(str_at(entry, 0).to_string());
        }
    }
    names
}

/// The parameters [`return_profile`] reads, the same on every PX4 airframe.
pub fn return_param_names() -> Vec<String> {
    ["RTL_RETURN_ALT", "RTL_DESCEND_ALT", "RTL_LAND_DELAY"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn height(value: Option<f64>) -> Value {
    match value {
        Some(v) if !v.is_nan() && v >= 0.0 => crate::json::num(v),
        _ => Value::Null,
    }
}

/// How PX4 flies a Return, in metres above home: the height it climbs to
/// before it turns for home (`climb_to`), the height it goes to once overhead
/// (`arrive_at`), and a multicopter that never lands on its own (`hold`, a
/// `RTL_LAND_DELAY` of -1). The return cone is left out.
pub fn return_profile(values: &Values) -> Value {
    let delay = values.get("RTL_LAND_DELAY");
    json!({
        "known": !values.is_empty(),
        "climb_to": height(values.get("RTL_RETURN_ALT").copied()),
        "arrive_at": height(values.get("RTL_DESCEND_ALT").copied()),
        "hold": delay.is_some_and(|d| *d < 0.0),
    })
}

/// The operator-added names this module will actually read, in order: shaped
/// like a PX4 parameter id, no duplicates, capped. Being in [`param_names`] is
/// deliberately not a reason to drop one; two controls writing one parameter
/// is settled in [`build`].
pub fn normalise_extra(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in names {
        let name = raw.trim().to_uppercase();
        let mut chars = name.chars();
        let ok = name.chars().count() <= 16
            && chars.next().is_some_and(|c| c.is_ascii_uppercase())
            && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !ok || out.contains(&name) {
            continue;
        }
        out.push(name);
        if out.len() >= EXTRA_PARAM_LIMIT {
            break;
        }
    }
    out
}

/// One field description per requested name, present on this firmware or not.
/// A name the vehicle never answered for still comes back, flagged, so the
/// operator can tell a typo from a parameter their PX4 version does not have.
pub fn extra_fields(names: &[String], values: &Values) -> Vec<Value> {
    names
        .iter()
        .map(|name| {
            json!({
                "param": name, "label": name, "kind": "number",
                "present": values.contains_key(name),
                "value": crate::json::num(values.get(name).copied().unwrap_or(0.0)),
            })
        })
        .collect()
}

fn low_battery_options(t: &Table, values: &Values) -> Vec<Value> {
    let options = t.options("LOW_BATTERY_ACTION_OPTIONS");
    let deprecated = t.get("LOW_BATTERY_ACTION_DEPRECATED").clone();
    let held = rounded(values, "COM_LOW_BAT_ACT");
    if held == deprecated.get("value").and_then(Value::as_i64) && held.is_some() {
        let mut out = vec![options[0].clone(), deprecated];
        out.extend(options[1..].iter().cloned());
        return out;
    }
    options
}

fn limits_section(values: &Values) -> Option<Value> {
    let t = safety();
    let fields = present(vec![
        number("GF_MAX_HOR_DIST", "Maximum distance", values)
            .unit("m")
            .step(1)
            .min(0)
            .hint("Horizontal distance from home the vehicle may reach. 0 disables the limit."),
        number("GF_MAX_VER_DIST", "Maximum height", values)
            .unit("m")
            .step(1)
            .min(0)
            .hint("Height above home the vehicle may reach. 0 disables the limit."),
        number("LNDMC_ALT_MAX", "Multicopter altitude ceiling", values)
            .unit("m")
            .step(1)
            .hint("Hard altitude limit enforced by the multicopter land detector."),
        enum_(
            "GF_SOURCE",
            "Position source",
            values,
            &t.options("GEOFENCE_SOURCE_OPTIONS"),
        ),
        enum_("GF_PREDICT", "Predict the breach", values, &t.options("ON_OFF_OPTIONS"))
            .hint("Act on where the current velocity is taking the vehicle, not only on where it already is."),
    ]);
    section(
        "limits",
        "Flight limits",
        fields,
        "The envelope the vehicle is not allowed to leave, measured from the home position. What it does at the limit is set on the Geofence card.",
    )
    .map(Value::Object)
}

/// The Geofence card: the area drawn on the map and the action on leaving it.
/// Present whenever the vehicle answered at all, even without `GF_ACTION`: the
/// area is kept on this station and only the action needs the vehicle.
fn geofence_section(values: &Values) -> Option<Value> {
    if values.is_empty() {
        return None;
    }
    let t = safety();
    let fields = present(vec![enum_(
        "GF_ACTION",
        "Action when leaving the area",
        values,
        &t.options("GEOFENCE_ACTION_OPTIONS"),
    )
    .hint("Also applies to the maximum distance and height under Flight limits.")]);
    let inactive = if values.get("GF_ACTION") == Some(&0.0) {
        "The action is None, so leaving the area does nothing."
    } else {
        ""
    };
    Some(json!({
        "id": "geofence", "title": "Geofence", "kind": "geofence", "fields": fields,
        "hint": "Draw the area the vehicle may fly in. Leaving it triggers the action below.",
        "enable_writes": [],
        "inactive": inactive,
    }))
}

fn rtl_section(values: &Values) -> Option<Value> {
    let t = safety();
    let fields = present(vec![
        enum_("RTL_TYPE", "Return type", values, &t.options("RTL_TYPE_OPTIONS")),
        number("RTL_RETURN_ALT", "Return height", values)
            .unit("m")
            .step(1)
            .min(0)
            .hint("Height above home the vehicle climbs to before flying back."),
        number("RTL_DESCEND_ALT", "Descend height", values)
            .unit("m")
            .step(1)
            .min(0)
            .hint("Height above home it descends to once it has arrived."),
        enum_("RTL_CONE_ANG", "Return cone", values, &t.options("RTL_CONE_OPTIONS")).hint(
            "Close to home the return height is reduced along this cone, so a vehicle hovering overhead does not climb first.",
        ),
        number("RTL_MIN_DIST", "Minimum cone distance", values).unit("m").step(1).min(0),
        number("RTL_LAND_DELAY", "Loiter before landing", values)
            .unit("s")
            .step(1)
            .hint("Seconds to hold at the descend height before landing. -1 loiters indefinitely."),
        number("RTL_LOITER_RAD", "Loiter radius", values)
            .unit("m")
            .step(1)
            .hint("Fixed-wing only."),
    ]);
    section(
        "rtl",
        "Return to Launch",
        fields,
        "The path home, flown on an RTL command and by every failsafe whose action is Return.",
    )
    .map(Value::Object)
}

/// The Landing card: how fast the vehicle descends and where it slows down.
/// The `actions` list carries the Fast landing preset restricted to the
/// parameters this firmware answered for, so a build that lacks one never
/// receives a write for it.
fn landing_section(values: &Values) -> Option<Value> {
    let t = safety();
    let fields = present(vec![
        number("MPC_Z_V_AUTO_DN", "Descent speed", values)
            .unit("m/s")
            .step(0.1)
            .min(0.5)
            .max(4)
            .hint("Speed of automatic descents before the landing itself, such as the descent after a return. Firmware default 1.5."),
        number("MPC_Z_VEL_MAX_DN", "Maximum descent speed", values)
            .unit("m/s")
            .step(0.1)
            .min(0.5)
            .max(4)
            .hint("Speed of the landing above the slowdown height. Also caps every other descent, manual ones too. Firmware default 1.5."),
        number("MPC_LAND_ALT1", "Slowdown starts at", values)
            .unit("m")
            .step(1)
            .min(0)
            .max(122)
            .hint("Height where the descent starts to slow down. Must be above the next height. Firmware default 10."),
        number("MPC_LAND_ALT2", "Landing speed from", values)
            .unit("m")
            .step(1)
            .min(0)
            .max(122)
            .hint("Height below which it descends at the landing speed. Firmware default 5."),
        number("MPC_LAND_SPEED", "Landing speed", values)
            .unit("m/s")
            .step(0.1)
            .min(0.6)
            .hint("Descent speed down to the ground. Firmware default 0.7."),
        number("MPC_LAND_ALT3", "Crawl below", values)
            .unit("m")
            .step(0.5)
            .min(0)
            .max(122)
            .hint("Height of the last, slowest step. Used only with a distance sensor. Firmware default 1."),
        number("MPC_LAND_CRWL", "Crawl speed", values)
            .unit("m/s")
            .step(0.1)
            .min(0.1)
            .hint("Descent speed in the last step. Firmware default 0.3."),
    ]);
    if fields.is_empty() {
        return None;
    }
    let fast = t.get("FAST_LANDING");
    let writes: Vec<Value> = fields
        .iter()
        .filter_map(|f| {
            let param = f.get("param")?.as_str()?;
            Some(json!({"param": param, "value": fast.get(param)?}))
        })
        .collect();
    Some(json!({
        "id": "landing", "title": "Landing", "kind": "fields", "fields": fields,
        "hint": "How fast the vehicle may descend when it lands on its own (Land, Return and a mission landing), and the heights where it slows down. Below the slowdown it holds the landing speed until touchdown.",
        "actions": [{
            "id": "fast", "label": "Fast landing", "writes": writes,
            "hint": "Faster descent that still brakes early. Tuned for the Holybro X500 and X650. The values are written to the vehicle and stay editable below.",
        }],
    }))
}

fn failsafe_section(values: &Values) -> Option<Value> {
    let t = safety();
    let link = t.options("LINK_LOSS_ACTION_OPTIONS");
    let fields = present(vec![
        enum_("NAV_RCL_ACT", "RC loss", values, &link),
        number("COM_RC_LOSS_T", "RC loss timeout", values).unit("s").step(0.1).min(0),
        enum_("NAV_DLL_ACT", "Data link loss", values, &link),
        number("COM_DL_LOSS_T", "Data link loss timeout", values).unit("s").step(1).min(0),
        enum_("COM_LOW_BAT_ACT", "Low battery", values, &low_battery_options(&t, values)),
        enum_("COM_POSCTL_NAVL", "Position loss", values, &t.options("POSITION_LOSS_ACTION_OPTIONS"))
            .hint("Fallback when the position estimate is lost in Position mode."),
        enum_("COM_ACT_FAIL_ACT", "Actuator failure", values, &t.options("ACTUATOR_FAILURE_ACTION_OPTIONS")),
        enum_("COM_QC_ACT", "VTOL quad-chute", values, &t.options("QUADCHUTE_ACTION_OPTIONS")),
        number("COM_FAIL_ACT_T", "Reaction delay", values)
            .unit("s")
            .step(0.1)
            .min(0)
            .hint("Grace period between the failsafe condition and the action, so a brief dropout does not trigger a return."),
        number("COM_DISARM_LAND", "Auto-disarm after landing", values)
            .unit("s")
            .step(0.1)
            .hint("Seconds after touchdown before the vehicle disarms itself. -1 disables it."),
        number("COM_DISARM_PRFLT", "Auto-disarm if not taking off", values)
            .unit("s")
            .step(1)
            .hint("Seconds armed on the ground before disarming again."),
    ]);
    section(
        "failsafe",
        "Failsafe actions",
        fields,
        "What the autopilot does on its own when something is lost. Every action named Return flies the Return to Launch profile. The levels the low-battery action reacts to are on the Battery & Power page. Every BAT_ parameter lives there.",
    )
    .map(Value::Object)
}

// ---------------------------------------------------------------------------
// Sensor toggles
// ---------------------------------------------------------------------------

/// Driver entries for every enable parameter the firmware answered for. A value
/// the table does not know is offered as its own entry, because without it the
/// picker would fall back to the first model and the next press of the switch
/// would replace a working driver with that one.
fn bus_drivers(values: &Values, table: &[Value]) -> Vec<Value> {
    let t = safety();
    let mut drivers = Vec::new();
    for entry in table {
        let param = str_at(entry, 0);
        let Some(&held) = values.get(param) else {
            continue;
        };
        let models = entry.get(1).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
        for model in models {
            let value = int_at(model, 0);
            drivers.push(json!({
                "id": format!("{param}:{value}"), "label": str_at(model, 1),
                "param": param, "value": value as f64, "serial": false,
                "wiring": wiring(&t, str_at(model, 2)),
            }));
        }
        let current = round_int(held);
        if current != 0 && models.iter().all(|m| int_at(m, 0) != current) {
            let bus = models.first().map_or("", |m| str_at(m, 2));
            drivers.push(json!({
                "id": format!("{param}:{current}"),
                "label": format!("{param} = {current}, not in this list"),
                "param": param, "value": current as f64, "serial": false,
                "wiring": wiring(&t, bus),
            }));
        }
    }
    drivers
}

fn serial_entry(port_param: &str, label: &str, entry_id: &str, model: Option<(&str, f64)>) -> Value {
    let t = safety();
    let mut entry = Map::new();
    entry.insert("id".into(), json!(entry_id));
    entry.insert("label".into(), json!(label));
    entry.insert("param".into(), json!(port_param));
    entry.insert("value".into(), Value::Null);
    entry.insert("serial".into(), json!(true));
    entry.insert("wiring".into(), json!(wiring(&t, BUS_SERIAL)));
    if let Some((param, value)) = model {
        entry.insert("model".into(), json!({"param": param, "value": value}));
        entry.insert("extra".into(), json!([{"param": param, "value": value}]));
    }
    Value::Object(entry)
}

/// Driver entries whose "enable" is the serial port the sensor is wired to.
/// Where the driver also reads a model parameter each model is an entry of its
/// own and carries that write, so the port and the model are always set
/// together; a model number this build does not know is kept for the same
/// reason an unknown bus driver is.
fn serial_drivers(values: &Values, table: &[Value]) -> Vec<Value> {
    let mut drivers = Vec::new();
    for entry in table {
        let port_param = str_at(entry, 0);
        let model_param = entry.get(1).and_then(Value::as_str).unwrap_or("");
        let models = entry.get(2).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
        let label = str_at(entry, 3);
        if !values.contains_key(port_param) {
            continue;
        }
        if model_param.is_empty() || !values.contains_key(model_param) {
            drivers.push(serial_entry(port_param, label, port_param, None));
            continue;
        }
        for model in models {
            let value = int_at(model, 0);
            drivers.push(serial_entry(
                port_param,
                str_at(model, 1),
                &format!("{port_param}:{value}"),
                Some((model_param, value as f64)),
            ));
        }
        let current = round_int(values[model_param]);
        let running = round_int(values[port_param]) != 0;
        if running && models.iter().all(|m| int_at(m, 0) != current) {
            drivers.push(serial_entry(
                port_param,
                &format!("{label}, {model_param} = {current}"),
                &format!("{port_param}:{current}"),
                Some((model_param, current as f64)),
            ));
        }
    }
    drivers
}

/// Driver entries for a DroneCAN subscription. A subscription on its own is
/// silent if the CAN stack is not running, so the entry carries the
/// `UAVCAN_ENABLE` write alongside it, but only when the board is not already
/// past it: a vehicle on 3 runs DroneCAN ESCs, and dropping it to 2 would stop
/// the motors answering.
fn can_drivers(values: &Values, table: &[Value]) -> Vec<Value> {
    let t = safety();
    let mut drivers = Vec::new();
    for entry in table {
        let param = str_at(entry, 0);
        if !values.contains_key(param) {
            continue;
        }
        let mut driver = json!({
            "id": format!("{param}:1"), "label": str_at(entry, 1),
            "param": param, "value": 1.0, "serial": false,
            "wiring": wiring(&t, BUS_CAN),
        });
        if let Some(&current) = values.get(CAN_ENABLE_PARAM) {
            if current < CAN_ENABLE_VALUE {
                driver["extra"] = json!([{"param": CAN_ENABLE_PARAM, "value": CAN_ENABLE_VALUE}]);
                driver["needs"] = json!(format!(
                    "{CAN_ENABLE_PARAM} is {}, so the DroneCAN stack is off and the subscription hears nothing.",
                    round_int(current)
                ));
            }
        }
        drivers.push(driver);
    }
    drivers
}

/// The driver currently switched on (its index) and its port when it is a
/// serial one. A bus driver matches on its exact model value, because
/// `SENS_EN_SF1XX` = 6 is a different rangefinder from 3; a serial driver
/// matches on "not 0", its value is the port, and where it also reads a model
/// parameter that has to match too.
fn active_driver(values: &Values, drivers: &[Value]) -> (Option<usize>, Option<i64>) {
    for (i, driver) in drivers.iter().enumerate() {
        let Some(param) = driver.get("param").and_then(Value::as_str).filter(|p| !p.is_empty()) else {
            continue;
        };
        let Some(&held) = values.get(param) else {
            continue;
        };
        let current = round_int(held);
        if current == 0 {
            continue;
        }
        if driver.get("serial").and_then(Value::as_bool) == Some(true) {
            if let Some(model) = driver.get("model").filter(|m| !m.is_null()) {
                let want = round_int(model["value"].as_f64().unwrap_or(0.0));
                let have = values
                    .get(model["param"].as_str().unwrap_or(""))
                    .copied()
                    .unwrap_or_else(|| model["value"].as_f64().unwrap_or(0.0));
                if round_int(have) != want {
                    continue;
                }
            }
            return (Some(i), Some(current));
        }
        if current == round_int(driver["value"].as_f64().unwrap_or(0.0)) {
            return (Some(i), None);
        }
    }
    (None, None)
}

/// The distinct enable parameters behind a driver list, order preserved.
fn driver_params(drivers: &[Value]) -> Vec<String> {
    let mut params: Vec<String> = Vec::new();
    for driver in drivers {
        if let Some(p) = driver.get("param").and_then(Value::as_str).filter(|p| !p.is_empty()) {
            if !params.iter().any(|x| x == p) {
                params.push(p.to_string());
            }
        }
    }
    params
}

/// The enable parameters in `drivers` that are currently switched on.
fn running_params(values: &Values, drivers: &[Value]) -> Vec<String> {
    driver_params(drivers)
        .into_iter()
        .filter(|p| values.get(p).is_some_and(|v| round_int(*v) != 0))
        .collect()
}

struct Fusion {
    param: &'static str,
    options: Vec<Value>,
    on: f64,
    current: f64,
}

/// The first estimator control the firmware carries.
fn fusion(values: &Values, candidates: &[(&'static str, Vec<Value>, f64)]) -> Option<Fusion> {
    for (param, options, on) in candidates {
        if let Some(&current) = values.get(*param) {
            return Some(Fusion {
                param,
                options: options.clone(),
                on: *on,
                current,
            });
        }
    }
    None
}

#[derive(Default)]
struct ToggleExtras {
    on: Vec<Value>,
    off: Vec<Value>,
    enabled: bool,
    detail: String,
    fusion_off: String,
}

/// Assembles the toggle description for one sensor. `state` is what the
/// overview reports: `on` only when every half is in place, `partial` when
/// some of it is, and `problems` says what is missing. The switch follows
/// `enabled` (`state == "on"`), so a half-configured sensor shows an off switch
/// and one press completes it.
fn sensor_toggle(
    values: &Values,
    label: &str,
    drivers: Vec<Value>,
    fusion: Option<Fusion>,
    extras: ToggleExtras,
) -> Option<Value> {
    let t = safety();
    if drivers.is_empty() && fusion.is_none() && extras.on.is_empty() {
        return None;
    }
    let (active, port) = active_driver(values, &drivers);
    let active_driver = active.map(|i| &drivers[i]);
    let fusion_on = fusion.as_ref().is_some_and(|f| round_int(f.current) != 0) || extras.enabled;

    let mut enable: Vec<Value> = Vec::new();
    let mut disable: Vec<Value> = extras.off.clone();
    if let Some(f) = &fusion {
        enable.push(json!({"param": f.param, "value": f.on}));
        disable.push(json!({"param": f.param, "value": 0.0}));
    }
    enable.extend(extras.on.iter().cloned());

    let clear = running_params(values, &drivers);

    let mut offered = drivers.clone();
    if fusion.is_some() || !extras.on.is_empty() {
        offered.push(json!({
            "id": EXTERNAL_DRIVER_ID, "label": "External / MAVLink",
            "param": Value::Null, "value": Value::Null, "serial": false,
            "wiring": wiring(&t, BUS_EXTERNAL),
        }));
    }

    let id_of = |d: &Value| d.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let selected = if let Some(d) = active_driver {
        id_of(d)
    } else if fusion_on {
        EXTERNAL_DRIVER_ID.to_string()
    } else {
        offered.first().map(id_of).unwrap_or_default()
    };

    let mut detail = if let Some(d) = active_driver {
        d.get("label").and_then(Value::as_str).unwrap_or("").to_string()
    } else if fusion_on {
        "No local driver, expecting the sensor over MAVLink".to_string()
    } else {
        "No driver enabled".to_string()
    };
    if let Some(f) = &fusion {
        detail += &format!(" \u{b7} fusion: {}", option_label(&f.options, f.current));
    } else if !extras.detail.is_empty() {
        detail += &format!(" \u{b7} {}", extras.detail);
    }

    let mut problems: Vec<String> = Vec::new();
    if let Some(needs) = active_driver
        .and_then(|d| d.get("needs"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        problems.push(needs.to_string());
    }
    if active_driver.is_some() && !fusion_on {
        if let Some(f) = &fusion {
            problems.push(format!(
                "The driver runs, but the estimator ignores it: {} is {}.",
                f.param,
                option_label(&f.options, f.current)
            ));
        } else if !extras.fusion_off.is_empty() {
            problems.push(extras.fusion_off.clone());
        } else {
            problems.push("The driver runs, but the estimator ignores it.".to_string());
        }
    }
    if !clear.is_empty() && active_driver.is_none() {
        problems.push(format!(
            "A driver is running that this page cannot match to a model: {}.",
            clear.join(", ")
        ));
    }
    let ports = port_options(values, &driver_params(&drivers));
    if active_driver.is_some() {
        if let Some(port) = port {
            let shared = ports.iter().find(|p| {
                p.get("value").and_then(Value::as_i64) == Some(port)
                    && p.get("blocked").is_some_and(|b| !b.is_null() && b != "")
            });
            if let Some(blocked) = shared.and_then(|p| p.get("blocked")).and_then(Value::as_str) {
                problems.push(blocked.to_string());
            }
        }
    }

    let configured = active_driver.is_some() || fusion_on || !clear.is_empty();
    let state = if !configured {
        "off"
    } else if !problems.is_empty() {
        "partial"
    } else {
        "on"
    };

    let reboot = !clear.is_empty()
        || offered
            .iter()
            .any(|d| d.get("param").and_then(Value::as_str).is_some_and(|p| !p.is_empty()));
    let any_serial = offered
        .iter()
        .any(|d| d.get("serial").and_then(Value::as_bool) == Some(true));
    let mut toggle = json!({
        "label": label,
        "enabled": state == "on",
        "state": state,
        "problems": problems,
        "detail": detail,
        "drivers": offered,
        "selected": selected,
        "enable": enable,
        "disable": disable,
        "clear": clear,
        "reboot": reboot,
    });
    if any_serial {
        let chosen = match port {
            Some(p) => json!(p),
            None => ports
                .iter()
                .find(|p| p.get("used").and_then(Value::as_bool) != Some(true))
                .and_then(|p| p.get("value").cloned())
                .unwrap_or(Value::Null),
        };
        toggle["ports"] = Value::Array(ports);
        toggle["port"] = chosen;
    }
    Some(toggle)
}

/// PX4's serial ports, each labelled with what already runs on it. `own` are
/// the driver parameters of the sensor this picker belongs to: a port one of
/// them holds is where that sensor already is. Any other user makes the port
/// `blocked` for this sensor, with the reason, because PX4 would start both
/// drivers on one port and neither would work.
fn port_options(values: &Values, own: &[String]) -> Vec<Value> {
    let t = safety();
    let mut named: Vec<(String, String)> = t
        .array("PORT_USERS")
        .iter()
        .map(|u| (str_at(u, 0).to_string(), str_at(u, 1).to_string()))
        .collect();
    for table in ["RANGEFINDER_SERIAL_DRIVERS", "FLOW_SERIAL_DRIVERS"] {
        for entry in t.array(table) {
            named.push((str_at(entry, 0).to_string(), str_at(entry, 3).replace(" (serial)", "")));
        }
    }
    let mut users: Vec<(i64, Vec<(String, String)>)> = Vec::new();
    for (param, name) in named {
        let Some(&v) = values.get(&param) else {
            continue;
        };
        let port = round_int(v);
        if port == 0 {
            continue;
        }
        match users.iter_mut().find(|(p, _)| *p == port) {
            Some((_, list)) => list.push((param, name)),
            None => users.push((port, vec![(param, name)])),
        }
    }
    let mut out = Vec::new();
    for option in t.array("SERIAL_PORT_OPTIONS") {
        let value = option["value"].as_i64().unwrap_or(0);
        let label = str_at(option, 0);
        let label = option["label"].as_str().unwrap_or(label);
        let taken: &[(String, String)] = users
            .iter()
            .find(|(p, _)| *p == value)
            .map_or(&[], |(_, l)| l.as_slice());
        let mut names: Vec<&str> = Vec::new();
        for (_, name) in taken {
            if !names.contains(&name.as_str()) {
                names.push(name);
            }
        }
        let mut entry = Map::new();
        entry.insert("value".into(), json!(value));
        let shown = if names.is_empty() {
            label.to_string()
        } else {
            format!("{label} ({})", names.join(", "))
        };
        entry.insert("label".into(), json!(shown));
        entry.insert("used".into(), json!(!taken.is_empty()));
        if let Some((param, name)) = taken.iter().find(|(param, _)| !own.contains(param)) {
            entry.insert(
                "blocked".into(),
                json!(format!(
                    "{label} is already set up for {name} ({param}). One serial port cannot serve two drivers; pick a free port, or free this one on the Parameters page first."
                )),
            );
        }
        out.push(Value::Object(entry));
    }
    out
}

fn preset_driver_param(preset: &Value, kind: &str) -> Option<(String, i64)> {
    let id = preset.get("driver_id")?.get(kind)?.as_str().filter(|s| !s.is_empty())?;
    match id.split_once(':') {
        Some((param, raw)) => Some((param.to_string(), raw.trim().parse().unwrap_or(0))),
        None => Some((id.to_string(), 0)),
    }
}

/// True when the vehicle already runs the driver half this preset sets.
/// Deliberately not "this preset is selected": the whole Benewake family shares
/// one driver parameter, so a configured `SENS_TFMINI_CFG` says a Benewake lidar
/// is running and cannot say which one.
fn preset_is_active(preset: &Value, kind: &str, values: &Values) -> bool {
    let Some((param, value)) = preset_driver_param(preset, kind) else {
        return false;
    };
    let Some(&held) = values.get(&param) else {
        return false;
    };
    let current = round_int(held);
    if preset.get("serial").and_then(Value::as_bool) == Some(true) {
        current != 0
    } else {
        current == value
    }
}

fn format_value(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() {
        format!("{}", value as i64)
    } else {
        py_g(value)
    }
}

fn drivers_of(kind: &str, values: &Values) -> Vec<Value> {
    if kind == "range" {
        range_drivers(values)
    } else {
        flow_drivers(values)
    }
}

/// Turns one catalogue entry into what this firmware can actually do with it.
/// The write list is filtered against the parameters the vehicle answered for,
/// and what fell out is reported rather than dropped silently. A parameter that
/// may only be raised and already holds the value or more is not written and
/// comes back under `kept`. `clear` is every driver on these pages that is
/// running now and that the preset does not set itself.
fn resolve_preset(preset: &Value, kind: &str, values: &Values) -> Value {
    let driver = preset_driver_param(preset, kind);
    let mut supported = preset.get("supported").and_then(Value::as_bool).unwrap_or(true);
    let mut reason = if supported {
        String::new()
    } else {
        preset
            .get("unsupported")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    if supported {
        if let Some((param, _)) = &driver {
            if !values.contains_key(param) {
                supported = false;
                reason = format!(
                    "This firmware does not carry {param}, so the driver half of this module cannot be started from here."
                );
            }
        }
    }

    let mut writes: Vec<Value> = Vec::new();
    let mut kept: Vec<Value> = Vec::new();
    let mut missing: Vec<Value> = Vec::new();
    if supported {
        if let (Some((param, _)), true) = (&driver, preset.get("serial").and_then(Value::as_bool) == Some(true)) {
            writes.push(json!({
                "param": param, "value": Value::Null, "port": true,
                "label": "The serial port the module is wired to",
            }));
        }
        for item in preset
            .get("params")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let param = str_at(item, 0);
            let value = item.get(1).and_then(Value::as_f64).unwrap_or(0.0);
            match values.get(param) {
                None => missing.push(json!(param)),
                Some(&held) if raise_only(param) && held >= value => kept.push(json!({
                    "param": param, "value": held,
                    "label": format!("Already {} on this vehicle, which covers this. It is not lowered", format_value(held)),
                })),
                Some(_) => writes.push(json!({"param": param, "value": value, "label": str_at(item, 2)})),
            }
        }
    }

    let own: Vec<&str> = writes
        .iter()
        .filter_map(|w| w.get("param").and_then(Value::as_str))
        .collect();
    let mut clear: Vec<String> = Vec::new();
    if supported {
        for covered in preset
            .get("provides")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let covered = covered.as_str().unwrap_or("");
            for param in running_params(values, &drivers_of(covered, values)) {
                if !own.contains(&param.as_str()) && !clear.contains(&param) {
                    clear.push(param);
                }
            }
        }
    }

    let text = |key: &str| preset.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    let driver_id = preset
        .get("driver_id")
        .and_then(|d| d.get(kind))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let reboot = preset.get("reboot").is_some_and(|v| v.as_bool().unwrap_or(false)) && supported;
    json!({
        "id": preset["id"],
        "label": preset["label"],
        "vendor": text("vendor"),
        "model": text("model"),
        "bus": text("bus"),
        "summary": text("summary"),
        "note": text("note"),
        "wiring": text("wiring"),
        "serial": preset.get("serial").and_then(Value::as_bool) == Some(true),
        "supported": supported,
        "unsupported": reason,
        "driver": driver_id,
        "writes": writes,
        "kept": kept,
        "clear": clear,
        "missing": missing,
        "active": preset_is_active(preset, kind, values),
        "reboot": reboot,
    })
}

/// Every catalogue entry that claims to cover `kind`, resolved for this board.
fn presets_for(kind: &str, values: &Values) -> Vec<Value> {
    safety()
        .array("SENSOR_PRESETS")
        .iter()
        .filter(|p| {
            p.get("provides")
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().any(|k| k.as_str() == Some(kind)))
        })
        .map(|p| resolve_preset(p, kind, values))
        .collect()
}

fn range_drivers(values: &Values) -> Vec<Value> {
    let t = safety();
    let mut d = bus_drivers(values, t.array("RANGEFINDER_BUS_DRIVERS"));
    d.extend(serial_drivers(values, t.array("RANGEFINDER_SERIAL_DRIVERS")));
    d.extend(can_drivers(values, t.array("RANGEFINDER_CAN_DRIVERS")));
    d
}

fn flow_drivers(values: &Values) -> Vec<Value> {
    let t = safety();
    let mut d = bus_drivers(values, t.array("FLOW_BUS_DRIVERS"));
    d.extend(serial_drivers(values, t.array("FLOW_SERIAL_DRIVERS")));
    d.extend(can_drivers(values, t.array("FLOW_CAN_DRIVERS")));
    d
}

fn rangefinder_section(values: &Values) -> Option<Value> {
    let t = safety();
    let on_off = t.options("ON_OFF_OPTIONS");
    let range_ctrl = t.options("RANGE_CTRL_OPTIONS");
    let mut extras = ToggleExtras::default();
    if rounded(values, "EKF2_HGT_REF") == Some(HEIGHT_REF_RANGE) {
        extras.off = vec![json!({"param": "EKF2_HGT_REF", "value": HEIGHT_REF_BARO})];
    }
    let candidates = [
        ("EKF2_RNG_CTRL", range_ctrl.clone(), 1.0),
        ("EKF2_RNG_AID", on_off.clone(), 1.0),
    ];
    let toggle = sensor_toggle(
        values,
        "Distance sensor",
        range_drivers(values),
        fusion(values, &candidates),
        extras,
    )?;
    let fields = present(vec![
        enum_("EKF2_RNG_CTRL", "Fusion mode", values, &range_ctrl).hint(
            "Conditional uses the rangefinder for height only while the vehicle is low and slow; Always trusts it throughout the flight.",
        ),
        enum_("EKF2_RNG_AID", "Range aid", values, &on_off),
        enum_("EKF2_HGT_REF", "Primary height source", values, &t.options("HEIGHT_REF_OPTIONS")).hint(
            "Leave on Barometer or GPS for outdoor flight; Range sensor is for indoor and low-altitude work over a flat floor. Switching the sensor off moves this back to Barometer.",
        ),
        enum_("MPC_ALT_MODE", "Altitude control mode", values, &t.options("ALT_MODE_OPTIONS")).hint(
            "Terrain following holds a height above the ground the sensor sees, not above home.",
        ),
        number("EKF2_RNG_A_HMAX", "Range aid maximum height", values).unit("m").step(0.1),
        number("EKF2_RNG_A_VMAX", "Range aid maximum speed", values).unit("m/s").step(0.1),
        number("EKF2_MIN_RNG", "Reading on the ground", values)
            .unit("m")
            .step(0.01)
            .min(0)
            .hint("What the sensor reads with the vehicle standing on the ground."),
        number("EKF2_RNG_POS_X", "Mounting offset, X", values)
            .unit("m")
            .step(0.01)
            .hint("Sensor position ahead of the centre of gravity."),
        number("EKF2_RNG_POS_Y", "Mounting offset, Y", values)
            .unit("m")
            .step(0.01)
            .hint("Sensor position right of the centre of gravity."),
        number("EKF2_RNG_POS_Z", "Mounting offset, Z", values)
            .unit("m")
            .step(0.01)
            .hint("Sensor position below the centre of gravity, positive down."),
        number("EKF2_RNG_PITCH", "Mounting pitch offset", values).unit("rad").step(0.01),
        number("EKF2_RNG_DELAY", "Measurement delay", values).unit("ms").step(1),
        number("EKF2_RNG_NOISE", "Measurement noise", values).unit("m").step(0.01),
        number("EKF2_RNG_SFE", "Range-dependent noise", values).unit("m/m").step(0.01),
    ]);
    Some(json!({
        "id": "rangefinder", "title": "Distance sensor", "kind": "toggle",
        "group": "sensors", "icon": "radar",
        "short": "A downward lidar or sonar: height above the ground under the aircraft.",
        "toggle": toggle, "fields": fields,
        "presets": presets_for("range", values),
        "hint": "A downward lidar or sonar. Switching it on starts the driver and tells the estimator to fuse it. Doing only one of the two is the usual reason a rangefinder reads perfectly and changes nothing.",
    }))
}

fn flow_section(values: &Values) -> Option<Value> {
    let t = safety();
    let on_off = t.options("ON_OFF_OPTIONS");
    let candidates = [("EKF2_OF_CTRL", on_off, 1.0)];
    let flow_fusion = fusion(values, &candidates);
    let mut extras = ToggleExtras::default();
    // Pre v1.14 firmware has no EKF2_OF_CTRL: flow fusion is bit 1 of the
    // aiding mask. The target values come from the mask the vehicle holds now,
    // so the write never clears an unrelated aiding source.
    if flow_fusion.is_none() {
        if let Some(&raw) = values.get("EKF2_AID_MASK") {
            let mask = round_int(raw);
            extras
                .on
                .push(json!({"param": "EKF2_AID_MASK", "value": (mask | AID_MASK_FLOW_BIT) as f64}));
            extras
                .off
                .push(json!({"param": "EKF2_AID_MASK", "value": (mask & !AID_MASK_FLOW_BIT) as f64}));
        }
    }
    let mask_on = !extras.on.is_empty() && round_int(values["EKF2_AID_MASK"]) & AID_MASK_FLOW_BIT != 0;
    extras.enabled = mask_on;
    if !extras.on.is_empty() {
        extras.detail = format!("fusion: {} (EKF2_AID_MASK bit 1)", if mask_on { "On" } else { "Off" });
    }
    extras.fusion_off =
        "The driver runs, but the estimator ignores it: EKF2_AID_MASK does not have the optical flow bit set."
            .to_string();

    let toggle = sensor_toggle(values, "Optical flow", flow_drivers(values), flow_fusion, extras)?;

    let fields = present(vec![
        enum_(
            "SENS_FLOW_ROT",
            "Sensor rotation",
            values,
            &t.options("FLOW_ROTATION_OPTIONS"),
        )
        .hint("Yaw of the sensor relative to the airframe's forward axis."),
        number("SENS_FLOW_MINHGT", "Minimum height", values)
            .unit("m")
            .step(0.01),
        number("SENS_FLOW_MAXHGT", "Maximum height", values).unit("m").step(0.1),
        number("SENS_FLOW_MAXR", "Maximum angular rate", values)
            .unit("rad/s")
            .step(0.1),
        number("EKF2_OF_QMIN", "Minimum quality, in air", values).step(1),
        number("EKF2_OF_QMIN_GND", "Minimum quality, on ground", values).step(1),
        number("EKF2_OF_DELAY", "Measurement delay", values).unit("ms").step(1),
        number("EKF2_OF_N_MIN", "Measurement noise, best case", values)
            .unit("rad/s")
            .step(0.01),
        number("EKF2_OF_N_MAX", "Measurement noise, worst case", values)
            .unit("rad/s")
            .step(0.01),
        number("EKF2_OF_POS_X", "Mounting offset, X", values)
            .unit("m")
            .step(0.01),
        number("EKF2_OF_POS_Y", "Mounting offset, Y", values)
            .unit("m")
            .step(0.01),
        number("EKF2_OF_POS_Z", "Mounting offset, Z", values)
            .unit("m")
            .step(0.01),
    ]);
    Some(json!({
        "id": "flow", "title": "Optical flow", "kind": "toggle",
        "group": "sensors", "icon": "scan-line",
        "short": "A downward camera: horizontal speed without GPS. Needs a distance sensor.",
        "toggle": toggle, "fields": fields,
        "presets": presets_for("flow", values),
        "hint": "Horizontal velocity from a downward camera, for position hold without GPS. It needs a distance sensor: without a height above ground the flow rate cannot be turned into a speed.",
    }))
}

/// Turns the raw `{param: value}` the vehicle answered with into the Safety and
/// Sensors page description (`GET /api/safety`): `sections` (an ordered list of
/// `{id, title, hint, kind, fields[, toggle]}`), `extra` (the parameters the
/// operator added by name, flagged with whether this firmware answered for
/// them) and `received`.
pub fn build(values: &Values, extra: &[String]) -> Value {
    let mut sections: Vec<Value> = [
        limits_section(values),
        geofence_section(values),
        rtl_section(values),
        landing_section(values),
        failsafe_section(values),
        rangefinder_section(values),
        flow_section(values),
    ]
    .into_iter()
    .flatten()
    .collect();
    flag_flow_without_range(&mut sections);

    // A parameter this build already renders is not offered a second time: two
    // controls over one number can disagree until the next read.
    let rendered: Vec<&str> = sections
        .iter()
        .flat_map(|s| s.get("fields").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|f| f.get("param").and_then(Value::as_str))
        .collect();
    let names: Vec<String> = normalise_extra(extra)
        .into_iter()
        .filter(|n| !rendered.contains(&n.as_str()))
        .collect();
    json!({
        "sections": sections,
        "extra": extra_fields(&names, values),
        "received": values.len(),
    })
}
