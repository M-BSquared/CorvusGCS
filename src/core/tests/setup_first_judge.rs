//! When the first setup dialog may interrupt the operator: only for a board
//! that is certainly new. The frozen answers of `assess` are held by
//! `setup_first_cases.rs`; this is the policy on top of them.

use corvus_core::setup_schemas::fields::Values;
use corvus_core::setup_schemas::first_setup::*;
use serde_json::Value;

fn values(list: &[(&str, f64)]) -> Values {
    list.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

/// Every parameter PX4 answers for, all empty: a board fresh from the factory.
fn px4_blank() -> Values {
    read_names("px4").into_iter().map(|n| (n, 0.0)).collect()
}

fn with(mut base: Values, set: &[(&str, f64)]) -> Values {
    for (name, value) in set {
        base.insert(name.to_string(), *value);
    }
    base
}

fn prompt(stack: &str, transport: &str, v: &Values) -> bool {
    let judged = judge(assess(stack, "Copter", transport, v), stack, v);
    judged["prompt"] == Value::Bool(true)
}

#[test]
fn a_blank_px4_board_on_usb_is_offered_the_wizard() {
    let v = with(px4_blank(), &[("SYS_HAS_MAG", 1.0)]);
    assert!(prompt("px4", "usb", &v));
}

#[test]
fn a_px4_board_with_an_airframe_is_never_interrupted_for_its_calibration() {
    let v = with(px4_blank(), &[("SYS_AUTOSTART", 4001.0), ("SYS_HAS_MAG", 1.0)]);
    let judged = judge(assess("px4", "Copter", "usb", &v), "px4", &v);
    assert_eq!(judged["needs_setup"], Value::Bool(true), "the fact is still reported");
    assert_eq!(judged["prompt"], Value::Bool(false));
}

#[test]
fn a_calibration_in_a_later_slot_counts() {
    let v = with(
        px4_blank(),
        &[
            ("CAL_ACC1_ID", 1_114_378.0),
            ("CAL_GYRO2_ID", 2_228_234.0),
            ("CAL_MAG1_ID", 197_388.0),
            ("SYS_HAS_MAG", 1.0),
            ("SYS_AUTOSTART", 4001.0),
        ],
    );
    let found = assess("px4", "Copter", "usb", &v);
    for sensor in ["accel", "gyro", "mag"] {
        assert_eq!(found["facts"][sensor], "done", "{sensor}");
    }
    assert!(!prompt("px4", "usb", &v));
    assert_eq!(found["needs_setup"], Value::Bool(false));
}

#[test]
fn a_board_that_has_flown_is_not_new() {
    let v = with(px4_blank(), &[("COM_FLIGHT_UUID", 12.0), ("SYS_HAS_MAG", 1.0)]);
    let judged = judge(assess("px4", "Copter", "usb", &v), "px4", &v);
    assert_eq!(judged["flown"], Value::Bool(true));
    assert_eq!(judged["prompt"], Value::Bool(false));
}

#[test]
fn nothing_is_decided_while_a_parameter_is_still_missing() {
    let mut v = with(px4_blank(), &[("SYS_HAS_MAG", 1.0)]);
    v.remove("CAL_ACC1_ID");
    let judged = judge(assess("px4", "Copter", "usb", &v), "px4", &v);
    assert_eq!(judged["settled"], Value::Bool(false));
    assert_eq!(judged["prompt"], Value::Bool(false));
}

#[test]
fn a_simulator_is_never_prompted() {
    let v = with(px4_blank(), &[("SYS_HAS_MAG", 1.0)]);
    assert!(!prompt("px4", "udp", &v));
}

#[test]
fn an_ardupilot_board_is_new_only_when_nothing_was_calibrated_and_it_never_flew() {
    let base = [
        ("FRAME_CLASS", 1.0),
        ("INS_ACCOFFS_X", 0.0),
        ("INS_ACCOFFS_Y", 0.0),
        ("INS_ACCOFFS_Z", 0.0),
        ("INS_ACCSCAL_X", 1.0),
        ("INS_ACCSCAL_Y", 1.0),
        ("INS_ACCSCAL_Z", 1.0),
        ("STAT_FLTTIME", 0.0),
        ("COMPASS_DEV_ID", 0.0),
    ];
    assert!(prompt("ardupilot", "usb", &values(&base)));

    let mut scaled = base.to_vec();
    scaled[4].1 = 1.002;
    assert!(
        !prompt("ardupilot", "usb", &values(&scaled)),
        "a six position calibration"
    );

    let mut flown = base.to_vec();
    flown[7].1 = 3600.0;
    assert!(!prompt("ardupilot", "usb", &values(&flown)));

    let mut late = base.to_vec();
    late.retain(|(n, _)| *n != "STAT_FLTTIME");
    assert!(
        !prompt("ardupilot", "usb", &values(&late)),
        "flight time not answered yet"
    );
}
