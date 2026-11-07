//! The Rust dialect answers what `python/corvus/autopilot.py` answers on a
//! matrix of vehicles, firmwares, modes, texts and calibrations
//! (`python/tools/dump_core_cases.py dialect`).

use corvus_core::dialect::*;
use serde_json::{json, Map, Value};

fn cases() -> Value {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/dialect_cases.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn f(x: f64) -> Value {
    if x.is_nan() {
        json!("nan")
    } else {
        json!(x)
    }
}

fn plan(p: &CommandPlan) -> Value {
    json!({
        "command": p.command,
        "params": p.params.iter().map(|x| f(*x)).collect::<Vec<_>>(),
        "ok": p.ok(),
        "unsupported": p.unsupported,
    })
}

fn opt_plan(p: Option<CommandPlan>) -> Value {
    p.map_or(Value::Null, |p| plan(&p))
}

fn event(e: Option<VehicleEvent>) -> Value {
    e.map_or(Value::Null, |e| json!([e.kind, e.level, e.message]))
}

fn firmware(text: &str) -> Option<Vec<i64>> {
    if text == "none" {
        None
    } else {
        Some(
            text.split('.')
                .filter(|n| !n.is_empty())
                .map(|n| n.parse().unwrap())
                .collect(),
        )
    }
}

fn commands(table: Vec<(String, ModeCommand)>) -> Value {
    Value::Array(
        table
            .into_iter()
            .map(|(n, c)| json!([n, c.base_mode, c.custom_mode, c.custom_sub]))
            .collect(),
    )
}

fn stack_of(key: &str) -> Dialect {
    dialect_for_stack(Stack::from_name(key))
}

fn static_attrs(d: &Dialect) -> Value {
    json!({
        "stack": d.stack().as_str(),
        "label": d.label(),
        "supports_shell": d.supports_shell(),
        "supports_esc_calibration": d.supports_esc_calibration(),
        "accel_cal_positions_are_prompted": d.accel_cal_positions_are_prompted(),
        "autotune_style": d.autotune_style(),
        "log_format": d.log_format(),
        "log_suffix": d.log_suffix(),
        "firmware_vendor": d.firmware_vendor(),
        "mission_seq0_is_home": d.mission_seq0_is_home(),
        "mission_mode": d.mission_mode(),
        "mission_circles_as_waypoints": d.mission_circles_as_waypoints(),
        "mission_validity_reported": d.mission_validity_reported(),
        "mission_rejection_hint": d.mission_rejection_hint(),
        "param_encoding": d.param_encoding(),
        "param_metadata_path": d.param_metadata_path(),
        "param_metadata_format": d.param_metadata_format(),
        "param_metadata_cacheable": d.param_metadata_cacheable(),
        "param_file_format": d.param_file_format(),
        "prearm_report_complete": d.prearm_report_complete(),
        "prearm_prefixes": d.prearm_prefixes(),
        "routine_statustext_prefixes": d.routine_statustext_prefixes(),
    })
}

fn describe(d: &Dialect, mav: i64) -> Value {
    let t = d.takeoff_plan(mav);
    json!({
        "takeoff": [t.order, t.altitude_frame, t.guided_mode, f(t.min_pitch_deg)],
        "guided_mode": d.guided_mode(mav),
        "stick_mode": d.stick_mode(mav),
        "stick_modes": d.stick_modes(mav),
        "reposition": d.reposition_altitude_frame(mav),
        "hold_mode": d.hold_mode(mav),
        "velocity_mode": d.velocity_mode(mav),
        "plugin_commands": d.plugin_commands(mav).into_iter().map(|(i, n)| json!([i, n])).collect::<Vec<_>>(),
        "mission_arm_mode": d.mission_arm_mode(mav),
        "autotune_mode": d.autotune_mode(mav),
        "autotune_on": plan(&d.autotune_plan(mav, true)),
        "autotune_off": plan(&d.autotune_plan(mav, false)),
        "mission_refusals": d.mission_refusals(mav).into_iter().map(|(i, n)| json!([i, n])).collect::<Vec<_>>(),
        "capabilities": Value::Object(d.capabilities(mav)),
    })
}

fn live_mapping(spec: &Value) -> Vec<(String, LiveMode)> {
    spec.as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let name = e[0].as_str().unwrap().to_string();
            let v = &e[1];
            let mode = if let Some(t) = v.get("t") {
                let t: Vec<i64> = t.as_array().unwrap().iter().map(|n| n.as_i64().unwrap()).collect();
                if t.len() >= 3 {
                    LiveMode::Triple(t[0], t[1], t[2])
                } else {
                    LiveMode::Other
                }
            } else if let Some(i) = v.get("i") {
                LiveMode::Flat(i.as_i64().unwrap())
            } else {
                LiveMode::Other
            };
            (name, mode)
        })
        .collect()
}

fn check(what: &str, got: Value, want: &Value) {
    assert_eq!(&got, want, "{what}");
}

#[test]
fn stacks_follow_the_autopilot_id_and_name() {
    let c = cases();
    for row in c["stack_ids"].as_array().unwrap() {
        let want = row[1].as_str().unwrap();
        let got = match &row[0] {
            Value::Number(n) => stack_for_autopilot_id(n.as_i64().unwrap()),
            Value::String(s) => stack_for_autopilot_name(s),
            other => panic!("unexpected input {other}"),
        };
        assert_eq!(got.as_str(), want, "{}", row[0]);
    }
}

#[test]
fn px4_modes_wait_for_the_firmware_that_flies_them() {
    let c = cases();
    for (key, want) in c["px4_supported"].as_object().unwrap() {
        let (name, fw) = key.split_once('|').unwrap();
        let fw = firmware(fw);
        assert_eq!(
            px4_mode_supported(name, fw.as_deref()),
            want.as_bool().unwrap(),
            "{key}"
        );
    }
}

#[test]
fn vehicle_classes_match() {
    let c = cases();
    for (mav, want) in c["classes"].as_object().unwrap() {
        let mav: i64 = mav.parse().unwrap();
        check(
            &format!("class {mav}"),
            json!([vehicle_class(mav).as_str(), is_ground_vehicle(mav)]),
            want,
        );
    }
    let mut ids = plugin_position_commands();
    ids.sort_unstable();
    check("position commands", json!(ids), &c["position_commands"]);
}

#[test]
fn every_dialect_answers_like_the_python_one() {
    let c = cases();
    for (key, entry) in c["dialects"].as_object().unwrap() {
        let d = stack_of(key);
        check(&format!("{key} static"), static_attrs(&d), &entry["static"]);
        for (mav, want) in entry["types"].as_object().unwrap() {
            check(&format!("{key} type {mav}"), describe(&d, mav.parse().unwrap()), want);
        }
        for (count, want) in entry["start_params"].as_object().unwrap() {
            let (a, b) = d.mission_start_params(count.parse().unwrap());
            check(&format!("{key} start {count}"), json!([f(a), f(b)]), want);
        }
        for (text, want) in entry["events"].as_object().unwrap() {
            let got = json!([
                event(d.vehicle_event(text)),
                d.prearm_failure(text).map_or(Value::Null, Value::from),
                d.is_routine_statustext(text)
            ]);
            check(&format!("{key} event {text:?}"), got, want);
        }
        for (reason, want) in entry["prearm_events"].as_object().unwrap() {
            check(&format!("{key} prearm {reason:?}"), event(d.prearm_event(reason)), want);
        }
        for (name, want) in entry["labels"].as_object().unwrap() {
            check(&format!("{key} label {name:?}"), json!(d.mode_label(name)), want);
        }
        for (sensor, want) in entry["calibration"].as_object().unwrap() {
            check(&format!("{key} cal {sensor:?}"), opt_plan(d.calibration(sensor)), want);
        }
        for (sensor, want) in entry["cancel"].as_object().unwrap() {
            check(
                &format!("{key} cancel {sensor:?}"),
                plan(&d.cancel_calibration(sensor)),
                want,
            );
        }
        for (pos, want) in entry["accel_position"].as_object().unwrap() {
            check(&format!("{key} accel {pos:?}"), plan(&d.accel_position(pos)), want);
        }
        for row in entry["motor_test"].as_array().unwrap() {
            let got = plan(&d.motor_test(
                row[0].as_i64().unwrap(),
                row[1].as_f64().unwrap(),
                row[2].as_f64().unwrap(),
            ));
            check(&format!("{key} motor test {row}"), got, &row[3]);
        }
        for (motor, want) in entry["motor_stop"].as_object().unwrap() {
            check(
                &format!("{key} motor stop {motor}"),
                plan(&d.motor_stop(motor.parse().unwrap())),
                want,
            );
        }
    }
}

#[test]
fn mode_tables_decoding_and_adoption_match() {
    let c = cases();
    let specs = c["live_specs"].as_object().unwrap();
    for (key, entry) in c["dialects"].as_object().unwrap() {
        let d = stack_of(key);
        for (k, want) in entry["tables"].as_object().unwrap() {
            let (mav, fw) = k.split_once('|').unwrap();
            let (mav, fw) = (mav.parse::<i64>().unwrap(), firmware(fw));
            let got = json!([
                commands(d.mode_table(mav, fw.as_deref())),
                d.available_modes(mav, fw.as_deref())
            ]);
            check(&format!("{key} table {k}"), got, want);
        }
        for (k, want) in entry["decode"].as_object().unwrap() {
            let mut p = k.split('|');
            let custom: i64 = p.next().unwrap().parse().unwrap();
            let base: i64 = p.next().unwrap().parse().unwrap();
            let mav: i64 = p.next().unwrap().parse().unwrap();
            check(
                &format!("{key} decode {k}"),
                json!(d.decode_mode(custom, base, mav)),
                want,
            );
        }
        for (k, want) in entry["live"].as_object().unwrap() {
            let mut p = k.split('|');
            let spec = live_mapping(&specs[p.next().unwrap()]);
            let mav: i64 = p.next().unwrap().parse().unwrap();
            let fw = firmware(p.next().unwrap());
            check(
                &format!("{key} live {k}"),
                commands(d.adopt_live_mapping(&spec, mav, fw.as_deref())),
                want,
            );
        }
    }
}

#[test]
fn the_matrix_is_not_empty() {
    let c = cases();
    let mut n = 0;
    for entry in c["dialects"].as_object().unwrap().values() {
        for key in ["types", "events", "tables", "decode", "live"] {
            n += entry[key].as_object().map_or(0, Map::len);
        }
    }
    assert!(n > 5000, "{n}");
}
