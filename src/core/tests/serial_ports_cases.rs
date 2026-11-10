//! The Rust port classification answers what `corvus/serial_ports.py` answers
//! for the same device names and USB descriptors.

use corvus_core::serial_ports::*;
use serde_json::{json, Value as Json};

#[test]
fn the_serial_port_classification_answers_like_python() {
    let data: Json = serde_json::from_str(include_str!("data/serial_ports_cases.json")).unwrap();
    let mut failures: Vec<String> = Vec::new();
    let mut check = |what: String, got: Json, want: &Json| {
        if &got != want {
            failures.push(format!("{what}: got {got}, want {want}"));
        }
    };
    let s = |v: &Json| v.as_str().unwrap().to_string();
    for c in data["phantom"].as_array().unwrap() {
        check(format!("phantom {}", c[0]), json!(is_phantom_device(&s(&c[0]))), &c[1]);
    }
    for c in data["bootloader"].as_array().unwrap() {
        check(
            format!("bootloader {} {}", c[0], c[1]),
            json!(is_bootloader_port("/dev/x", &s(&c[0]), &s(&c[1]))),
            &c[2],
        );
    }
    for c in data["rtk"].as_array().unwrap() {
        check(
            format!("rtk {} {}", c[0], c[1]),
            json!(is_rtk_device("/dev/x", &s(&c[0]), &s(&c[1]))),
            &c[2],
        );
    }
    for c in data["hwid"].as_array().unwrap() {
        check(format!("hwid {}", c[0]), json!(classify_hwid(&s(&c[0]))), &c[1]);
    }
    for c in data["classify"].as_array().unwrap() {
        check(
            format!("classify {} {} {}", c[0], c[1], c[2]),
            json!(classify_serial_device(&s(&c[0]), &s(&c[1]), &s(&c[2]))),
            &c[3],
        );
    }
    for c in data["identity"].as_array().unwrap() {
        check(format!("identity {}", c[0]), json!(usb_identity(&s(&c[0]))), &c[1]);
    }
    for c in data["split"].as_array().unwrap() {
        let (device, baud) = split_serial_connection(&s(&c[0]));
        check(format!("split {}", c[0]), json!([device, baud]), &c[1]);
    }
    for c in data["same"].as_array().unwrap() {
        check(
            format!("same {} {}", c[0], c[1]),
            json!(same_port(&s(&c[0]), &s(&c[1]))),
            &c[2],
        );
    }
    for c in data["present"].as_array().unwrap() {
        let list: Vec<String> = c[1].as_array().unwrap().iter().map(s).collect();
        check(
            format!("present {} {}", c[0], c[1]),
            json!(port_present(&s(&c[0]), &list)),
            &c[2],
        );
    }
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.iter().take(15).cloned().collect::<Vec<_>>().join("\n")
    );
}
