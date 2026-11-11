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

#[test]
fn a_console_line_reaches_every_stream_and_a_closed_stream_is_let_go() {
    let d = dir("console");
    let core = core_in(&d);
    core.start().unwrap();
    let mut a = core.topic_stream(&[("console", "message")], None, &[]);
    let mut b = core.topic_stream(&[("console", "message")], None, &[]);
    let call = |cmd: &str| {
        core.internal_call(
            "POST",
            "/api/console/command",
            json!({"command": cmd}).as_object().cloned(),
        )
    };
    let (status, answer) = call("arm");
    assert_eq!(status, 503, "{answer}");
    for s in [&mut a, &mut b] {
        let events = corvus_core::events::collect(s.as_mut(), std::time::Duration::from_secs(3), 1);
        assert_eq!(events.len(), 1);
        let text = String::from_utf8(events[0].frame()).unwrap();
        assert!(
            text.starts_with("event: message\n") && text.contains("\"name\":\"ARM\""),
            "{text}"
        );
    }
    a.close();
    b.close();
    let (status, _) = call("arm");
    assert_eq!(status, 503);
    core.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[cfg(unix)]
#[test]
fn the_cores_stop_ends_every_terminal_and_background_program() {
    let d = dir("shell");
    let core = core_in(&d);
    core.start().unwrap();
    let call = |path: &str, body: Value| core.internal_call("POST", path, body.as_object().cloned());
    let (status, answer) = call("/api/local/connect", json!({"name": "life", "directory": "/"}));
    assert_eq!((status, &answer["connected"]), (200, &json!(true)), "{answer}");
    let (_, run) = call("/api/local/run", json!({"command": "exec sleep 300"}));
    let pid = run["pid"].as_i64().expect("it runs") as i32;
    let (_, status) = core.internal_call("GET", "/api/local/status", None);
    assert_eq!(status["running"], 1);
    let (_, sessions) = core.internal_call("GET", "/api/ssh/sessions", None);
    assert_eq!(sessions["sessions"][0]["name"], "life");
    let alive = |pid: i32| unsafe { libc::kill(pid, 0) == 0 };
    assert!(alive(pid));
    let t = std::time::Instant::now();
    core.stop();
    assert!(!alive(pid), "the background program is gone after stop");
    let (_, sessions) = core.internal_call("GET", "/api/ssh/sessions", None);
    assert_eq!(sessions["sessions"], json!([]));
    assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_video_routes_keep_the_list_hide_the_passwords_and_stop_joins_the_decoders() {
    let d = dir("video");
    let core = core_in(&d);
    core.start().unwrap();
    let video = core
        .service::<Peripherals>()
        .unwrap()
        .video()
        .expect("the video service is built at start");
    let call = |path: &str, body: Value| core.internal_call("POST", path, body.as_object().cloned());

    let (status, answer) = core.internal_call("GET", "/api/video/status", None);
    assert_eq!(status, 200);
    assert_eq!(answer["streams"], json!([]));
    assert_eq!(answer["max_streams"], 8);

    let (status, answer) = call(
        "/api/video/streams",
        json!({"name": "Front", "url": "rtsp://pilot:hunter2@10.1.2.3/live", "transport": "udp"}),
    );
    assert_eq!(status, 200, "{answer}");
    let id = answer["id"].as_str().unwrap().to_string();
    assert_eq!(answer["status"]["streams"][0]["address"], "10.1.2.3/live");
    assert_eq!(answer["status"]["streams"][0]["has_password"], true);
    assert!(!answer.to_string().contains("hunter2"), "no password in an answer");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
    assert_eq!(saved["video"]["streams"][0]["password"], "hunter2");
    assert_eq!(saved["video"]["streams"][0]["username"], "pilot");
    assert_eq!(saved["video"]["streams"][0]["url"], "rtsp://10.1.2.3/live");
    assert_eq!(
        video.settings()["streams"][0]["id"],
        json!(id),
        "the service has the list"
    );

    let (status, answer) = call(
        "/api/video/streams",
        json!({"id": id, "name": "Front", "url": "rtsp://other.example/live"}),
    );
    assert_eq!(status, 200);
    assert!(answer["warning"].as_str().unwrap().contains("moved to another host"));
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
    assert_eq!(saved["video"]["streams"][0]["password"], "");
    assert_eq!(
        call("/api/video/streams", json!({"id": "nope", "url": "rtsp://h/x"})).0,
        404
    );
    assert_eq!(call("/api/video/streams", json!({"url": "ftp://h/x"})).0, 400);
    assert_eq!(
        call("/api/video/streams", json!({"url": "rtsp://h/x", "kind": "x"})).0,
        400
    );
    assert_eq!(call("/api/video/streams", json!({"url": 5})).0, 400);
    assert_eq!(call("/api/video/settings", json!({"ffmpeg": "/no/such/ffmpeg"})).0, 400);
    assert_eq!(call("/api/video/settings", json!({"ffmpeg": 5})).0, 400);

    assert_eq!(core.internal_call("GET", "/api/video/frame?id=zz", None).0, 404);
    assert_eq!(call("/api/video/webrtc/offer", json!({"id": id, "sdp": "v=0"})).0, 409);
    assert_eq!(
        call("/api/video/webrtc/offer", json!({"id": "zz", "sdp": "v=0"})).0,
        404
    );
    assert_eq!(call("/api/video/webrtc/offer", json!({"id": "zz"})).0, 400);
    assert_eq!(
        call("/api/video/webrtc/close", json!({"session": "unknown"})).1["closed"],
        false
    );
    assert_eq!(call("/api/video/webrtc/close", json!({})).0, 400);

    let (status, answer) = call("/api/video/streams/remove", json!({"id": id}));
    assert_eq!((status, answer["status"]["streams"].clone()), (200, json!([])));
    assert_eq!(
        call("/api/video/streams/remove", json!({"id": id})).0,
        200,
        "removing twice is harmless"
    );
    assert_eq!(call("/api/video/streams/remove", json!({})).0, 400);

    core.stop();
    assert!(!video.any_alive());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_betaflight_routes_list_ports_and_refuse_a_bad_read() {
    let d = dir("betaflight");
    let core = core_in(&d);
    core.start().unwrap();
    let (status, answer) = core.internal_call("GET", "/api/betaflight/status", None);
    assert_eq!(status, 200);
    assert_eq!(answer["link_device"], "");
    assert_eq!(answer["default_baud"], 115_200);
    assert_eq!(answer["bauds"][1], 57_600);
    assert!(answer["ports"].is_array());

    let call = |body: Value| core.internal_call("POST", "/api/betaflight/read", body.as_object().cloned());
    let (status, answer) = call(json!({}));
    assert_eq!((status, answer["error"].as_str()), (400, Some("device is required")));
    let (status, _) = call(json!({"device": "  "}));
    assert_eq!(status, 400);
    let (status, answer) = call(json!({"device": "/dev/ttyACM0", "baud": "fast"}));
    assert_eq!(
        (status, answer["error"].as_str()),
        (400, Some("baud must be a positive integer"))
    );
    let (status, _) = call(json!({"device": "/dev/ttyACM0", "baud": true}));
    assert_eq!(status, 400);
    let (status, _) = call(json!({"device": "/dev/ttyACM0", "baud": 0}));
    assert_eq!(status, 400);
    let (status, answer) = call(json!({"device": "/definitely/not/a/port"}));
    assert_eq!(status, 409);
    assert!(answer["error"]
        .as_str()
        .unwrap()
        .starts_with("could not open /definitely/not/a/port: "));
    core.stop();
}
