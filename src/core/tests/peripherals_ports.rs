//! `serial_ports.py` and `net_probe.py` against what the Python answered
//! (`python/tools/dump_peripherals_cases.py ports netprobe`).

use corvus_core::peripherals::net_probe::{self, Pinger};
use corvus_core::peripherals::ports;
use serde_json::{json, Value};

fn load(name: &str) -> Value {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

#[test]
fn every_port_is_classified_as_python_classifies_it() {
    let doc = load("peripherals_ports_cases.json");
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() > 700);
    for c in cases {
        let (d, h, t) = (s(&c["device"]), s(&c["hwid"]), s(&c["description"]));
        assert_eq!(ports::is_phantom_device(d), c["phantom"], "phantom {c}");
        assert_eq!(ports::is_bootloader_port(d, h, t), c["bootloader"], "bootloader {c}");
        assert_eq!(ports::is_rtk_device(d, h, t), c["rtk"], "rtk {c}");
        assert_eq!(ports::classify_serial_device(d, h, t), s(&c["class"]), "class {c}");
    }
    for c in doc["hwid_class"].as_array().unwrap() {
        assert_eq!(ports::classify_hwid(s(&c["hwid"])), s(&c["class"]), "{c}");
    }
    for c in doc["com"].as_array().unwrap() {
        assert_eq!(ports::is_windows_com_port(s(&c["device"])), c["com"], "{c}");
    }
}

#[test]
fn connection_strings_and_identities_read_as_python_reads_them() {
    let doc = load("peripherals_ports_cases.json");
    assert_eq!(doc["default_baud"], ports::DEFAULT_SERIAL_BAUD);
    for c in doc["split"].as_array().unwrap() {
        let (device, baud) = ports::split_serial_connection(s(&c["conn"]));
        assert_eq!(json!([device, baud]), c["result"], "{c}");
    }
    for c in doc["identity"].as_array().unwrap() {
        let got = ports::usb_identity(s(&c["hwid"]));
        assert_eq!(got.map(Value::from).unwrap_or(Value::Null), c["result"], "{c}");
    }
}

#[test]
fn one_node_is_one_port_and_a_present_port_is_found() {
    let doc = load("peripherals_ports_cases.json");
    for c in doc["same"].as_array().unwrap() {
        assert_eq!(ports::same_port(s(&c["a"]), s(&c["b"])), c["same"], "{c}");
    }
    for c in doc["present"].as_array().unwrap() {
        // A device node of Unix, which a Windows computer does not have.
        if cfg!(windows) && s(&c["device"]).starts_with("/dev/") {
            continue;
        }
        let list: Vec<&str> = c["enumerated"].as_array().unwrap().iter().map(s).collect();
        assert_eq!(ports::port_present(s(&c["device"]), &list), c["present"], "{c}");
    }
}

#[test]
fn the_enumeration_is_sorted_and_has_no_phantom_node() {
    let list = ports::list_serial_ports();
    let names: Vec<&str> = list.iter().map(|p| p.device.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert!(list.iter().all(|p| !ports::is_phantom_device(&p.device)));
}

#[test]
fn hosts_commands_and_replies_read_as_python_reads_them() {
    let doc = load("peripherals_netprobe_cases.json");
    assert_eq!(doc["limits"]["max_host"], net_probe::MAX_HOST);
    assert_eq!(doc["limits"]["min_wait"], net_probe::MIN_WAIT_S);
    assert_eq!(doc["limits"]["max_wait"], net_probe::MAX_WAIT_S);
    assert_eq!(doc["limits"]["grace"], net_probe::GRACE_S);
    assert_eq!(doc["limits"]["concurrent"], net_probe::MAX_CONCURRENT);
    for c in doc["hosts"].as_array().unwrap() {
        let (host, problem) = net_probe::clean_host_value(&c["host"]);
        assert_eq!(json!([host, problem]), c["result"], "{c}");
    }
    for c in doc["commands"].as_array().unwrap() {
        let argv = net_probe::ping_command(
            "/bin/ping",
            s(&c["host"]),
            c["wait"].as_f64().unwrap(),
            s(&c["platform"]),
        );
        assert_eq!(json!(argv), c["argv"], "{c}");
    }
    for c in doc["replies"].as_array().unwrap() {
        let (reachable, rtt) = net_probe::parse_reply(
            c["code"].as_i64().unwrap() as i32,
            s(&c["text"]),
            s(&c["host"]),
            s(&c["platform"]),
        );
        assert_eq!(reachable, c["reachable"], "{c}");
        assert_eq!(rtt.map(Value::from).unwrap_or(Value::Null), c["rtt"], "{c}");
    }
}

#[cfg(unix)]
mod simulated {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("corvus-ping-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_reply_is_reachable_and_a_silent_host_is_not() {
        let dir = tmp("reply");
        let ok = script(&dir, "ok", "echo '64 bytes from x: icmp_seq=1 ttl=64 time=1.5 ms'");
        let fail = script(&dir, "fail", "echo 'Request timeout'; exit 1");
        let p = Pinger::new(Some(&ok));
        let a = p.ping(&json!("10.0.0.1"), 1.0);
        assert_eq!(
            a,
            json!({"ok": true, "host": "10.0.0.1", "reachable": true, "rtt_ms": 1.5})
        );
        let p = Pinger::new(Some(&fail));
        let a = p.ping(&json!("10.0.0.1"), 1.0);
        assert_eq!(
            a,
            json!({"ok": true, "host": "10.0.0.1", "reachable": false, "rtt_ms": null})
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bad_host_a_missing_program_and_a_closed_pinger_say_why() {
        let p = Pinger::new(Some("/definitely/not/ping"));
        assert_eq!(
            p.ping(&json!("-oX"), 1.0)["error"],
            "That is not an IP address or host name."
        );
        assert_eq!(p.ping(&json!(""), 1.0)["error"], "Enter an IP address or host name.");
        assert_eq!(
            p.ping(&json!("10.0.0.1"), 1.0)["error"],
            "ping could not be started: No such file or directory"
        );
        assert!(Pinger::new(Some("")).ping(&json!("10.0.0.1"), 1.0)["error"]
            .as_str()
            .unwrap()
            .contains("no ping program"));
        p.shutdown();
        assert_eq!(p.ping(&json!("10.0.0.1"), 1.0)["error"], "Corvus is shutting down.");
    }

    #[test]
    fn a_probe_that_hangs_is_killed_at_its_deadline() {
        let dir = tmp("hang");
        let hang = script(&dir, "hang", "exec sleep 30");
        let p = Pinger::new(Some(&hang));
        let t = Instant::now();
        let a = p.ping(&json!("10.0.0.1"), 0.1);
        assert_eq!(a["reachable"], false);
        assert!(t.elapsed() < Duration::from_secs(6), "{:?}", t.elapsed());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn shutdown_ends_a_probe_in_flight_and_only_four_run_at_once() {
        let dir = tmp("flight");
        let hang = script(&dir, "hang", "exec sleep 30");
        let p = Arc::new(Pinger::new(Some(&hang)));
        let mut running = Vec::new();
        for _ in 0..4 {
            let p = Arc::clone(&p);
            running.push(std::thread::spawn(move || p.ping(&json!("10.0.0.1"), 10.0)));
        }
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(p.ping(&json!("10.0.0.1"), 1.0)["error"], "Too many pings at once.");
        let t = Instant::now();
        p.shutdown();
        for r in running {
            assert_eq!(r.join().unwrap()["reachable"], false);
        }
        assert!(t.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(dir);
    }
}
