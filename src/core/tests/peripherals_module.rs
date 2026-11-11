//! The peripherals module inside a core: its routes through `internal_call`,
//! its services published, and every thread it started gone after `stop`.

use corvus_core::{CoreBuilder, Options as CoreOptions, Peripherals, PeripheralsModule, VehicleModule};
use serde_json::{json, Value};

fn dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("corvus-peripherals-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn core_in(dir: &std::path::Path) -> corvus_core::Core {
    CoreBuilder::new(CoreOptions::in_dir(dir))
        .module(VehicleModule::new())
        .module(PeripheralsModule::new())
        .build()
        .unwrap()
}

#[test]
fn the_rtk_routes_answer_through_the_core_and_stop_joins_the_session() {
    let d = dir("rtk");
    std::fs::write(
        d.join("config.json"),
        json!({"rtk": {"enabled": true, "device": "/definitely/not/a/port", "baud": 38400}}).to_string(),
    )
    .unwrap();
    let core = core_in(&d);
    core.start().unwrap();
    let rtk = core
        .service::<Peripherals>()
        .unwrap()
        .rtk()
        .expect("the RTK service is built at start");
    assert!(rtk.is_alive(), "switched on in the config, so it runs");

    let (status, answer) = core.internal_call("GET", "/api/rtk/status", None);
    assert_eq!(status, 200);
    assert_eq!(answer["enabled"], true);
    assert_eq!(answer["source"], "usb");
    assert_eq!(answer["link_ready"], false);

    let call = |path: &str, body: Value| core.internal_call("POST", path, body.as_object().cloned());
    let (status, answer) = call("/api/rtk/settings", json!({"source": "ntrip", "ntrip": {"host": "h"}}));
    assert_eq!(
        (status, answer["error"].as_str()),
        (400, Some("no NTRIP mountpoint is set"))
    );
    let (status, answer) = call(
        "/api/rtk/settings",
        json!({"enabled": false, "ntrip": {"host": "h", "mountpoint": "m", "password": "secret"}}),
    );
    assert_eq!(status, 200);
    assert_eq!(answer["status"]["settings"]["ntrip"]["has_password"], true);
    assert_eq!(answer["status"]["settings"]["ntrip"]["password"], "");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
    assert_eq!(
        saved["rtk"]["ntrip"]["password"], "secret",
        "the stored password is kept"
    );
    let (status, answer) = call(
        "/api/rtk/settings",
        json!({"ntrip": {"host": "h", "mountpoint": "m", "password": ""}}),
    );
    assert_eq!(status, 200);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
    assert_eq!(
        saved["rtk"]["ntrip"]["password"], "secret",
        "an empty password keeps the stored one"
    );
    assert_eq!(answer["status"]["settings"]["ntrip"]["has_password"], true);

    let (status, answer) = call("/api/rtk/restart", json!({}));
    assert_eq!((status, answer["ok"].clone()), (200, json!(true)));

    core.stop();
    assert!(!rtk.is_alive(), "the session thread was joined on the core's stop");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_switched_off_base_runs_no_thread_and_a_ping_is_refused_from_another_machine() {
    let d = dir("off");
    let core = core_in(&d);
    core.start().unwrap();
    let rtk = core.service::<Peripherals>().unwrap().rtk().unwrap();
    assert!(!rtk.is_alive());
    assert_eq!(rtk.status()["state"], "off");

    let mut req = corvus_core::Request::new(corvus_core::Method::Post, "/api/local/ping");
    req.peer = Some("192.168.1.9".parse().unwrap());
    req.set_payload(json!({"host": "127.0.0.1"}).as_object().cloned().unwrap());
    let r = core.dispatch(req);
    assert_eq!(r.status, 403);
    assert_eq!(
        r.json_body().unwrap()["error"],
        "Programs on this computer can only be started from this computer."
    );
    core.stop();
    let _ = std::fs::remove_dir_all(d);
}
