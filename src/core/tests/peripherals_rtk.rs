//! The RTK base station against what Python answered (wire frames, settings,
//! progress, requests) and against simulated devices: a u-blox receiver, a
//! local NTRIP caster and a pseudo terminal. No hardware.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use corvus_core::peripherals::ports::PortRow;
use corvus_core::peripherals::rtk::config;
use corvus_core::peripherals::rtk::io::{ntrip_request, Port, Socket};
use corvus_core::peripherals::rtk::service::{RtkHost, RtkIo, RtkService, Timing};
use corvus_core::peripherals::rtk::wire::{self, RtcmFramer, UbxParser};
use serde_json::{json, Value};

fn load() -> Value {
    let path = format!("{}/tests/data/peripherals_rtk_cases.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn hex(s: &Value) -> Vec<u8> {
    let s = s.as_str().unwrap();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn frames(v: &Value) -> Vec<Vec<u8>> {
    v.as_array().unwrap().iter().map(hex).collect()
}

fn obj(v: &Value) -> serde_json::Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

#[test]
fn every_configuration_frame_is_the_pythons_byte_for_byte() {
    let d = load();
    for c in d["survey"].as_array().unwrap() {
        let (dur, acc, m) = (
            c["duration"].as_i64().unwrap(),
            c["accuracy"].as_f64().unwrap(),
            c["msm7"].as_bool().unwrap(),
        );
        assert_eq!(wire::survey_in_frames(dur, acc, m), frames(&c["modern"]), "{c}");
        assert_eq!(wire::legacy_survey_in_frames(dur, acc, m), frames(&c["legacy"]), "{c}");
    }
    for c in d["fixed"].as_array().unwrap() {
        let f = |k: &str| c[k].as_f64().unwrap();
        let m = c["msm7"].as_bool().unwrap();
        assert_eq!(
            wire::fixed_position_frames(f("lat"), f("lon"), f("alt"), f("acc"), m),
            frames(&c["modern"]),
            "{c}"
        );
        assert_eq!(
            wire::legacy_fixed_position_frames(f("lat"), f("lon"), f("alt"), f("acc"), m),
            frames(&c["legacy"]),
            "{c}"
        );
    }
    for c in d["output"].as_array().unwrap() {
        let m = c["msm7"].as_bool().unwrap();
        assert_eq!(wire::stop_output_frames(m), frames(&c["stop"]));
        assert_eq!(wire::activate_output_frames(m, true), frames(&c["activate"]));
        assert_eq!(wire::activate_output_frames(m, false), frames(&c["activate_full_rate"]));
        assert_eq!(wire::disable_base_frames(m), frames(&c["disable"]));
        assert_eq!(wire::legacy_stop_output_frames(m), frames(&c["legacy_stop"]));
        assert_eq!(wire::legacy_activate_output_frames(m), frames(&c["legacy_activate"]));
        assert_eq!(wire::legacy_disable_base_frames(m), frames(&c["legacy_disable"]));
    }
    for c in d["polls"].as_array().unwrap() {
        assert_eq!(
            wire::poll_frame(c["class"].as_u64().unwrap() as u8, c["id"].as_u64().unwrap() as u8),
            hex(&c["frame"])
        );
    }
    for c in d["valset"].as_array().unwrap() {
        let items: Vec<(u32, i64)> = c["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_u64().unwrap() as u32, p[1].as_i64().unwrap()))
            .collect();
        assert_eq!(wire::valset(&items), hex(&c["frame"]), "{c}");
    }
}

#[test]
fn what_a_receiver_says_is_read_as_python_reads_it() {
    let d = load();
    for c in d["nav_svin"].as_array().unwrap() {
        let got = wire::parse_nav_svin(&hex(&c["payload"])).unwrap_or(Value::Null);
        assert_eq!(got, c["result"], "{c}");
    }
    for c in d["mon_ver"].as_array().unwrap() {
        let v = wire::parse_mon_ver(&hex(&c["payload"]));
        assert_eq!(v, c["result"], "{c}");
        assert_eq!(wire::receiver_label(&v), c["label"].as_str().unwrap(), "{c}");
        assert_eq!(wire::supports_valset(&v), c["valset"].as_bool().unwrap(), "{c}");
    }
    for c in d["labels"].as_array().unwrap() {
        assert_eq!(wire::receiver_label(&c["version"]), c["label"].as_str().unwrap(), "{c}");
        assert_eq!(
            wire::supports_valset(&c["version"]),
            c["valset"].as_bool().unwrap(),
            "{c}"
        );
    }
    for c in d["crc"].as_array().unwrap() {
        assert_eq!(u64::from(wire::crc24q(&hex(&c["data"]))), c["crc"].as_u64().unwrap());
    }
}

#[test]
fn the_parsers_find_the_same_frames_however_the_stream_is_cut() {
    let d = load();
    for c in d["framer"].as_array().unwrap() {
        let stream = hex(&c["stream"]);
        let mut f = RtcmFramer::new();
        let mut got = Vec::new();
        for piece in stream.chunks(c["chunk"].as_u64().unwrap() as usize) {
            got.extend(f.feed(piece));
        }
        assert_eq!(got, frames(&c["frames"]), "{} {}", c["name"], c["chunk"]);
        assert_eq!(
            (f.crc_errors, f.discarded, f.frames),
            (
                c["crc_errors"].as_i64().unwrap(),
                c["discarded"].as_i64().unwrap(),
                c["count"].as_i64().unwrap()
            ),
            "{} {}",
            c["name"],
            c["chunk"]
        );
        let numbers: Vec<u64> = got.iter().map(|x| u64::from(wire::message_number(x))).collect();
        assert_eq!(json!(numbers), c["numbers"]);
    }
    for c in d["ubx_parser"].as_array().unwrap() {
        let stream = hex(&c["stream"]);
        let mut p = UbxParser::new();
        let mut got = Vec::new();
        for piece in stream.chunks(c["chunk"].as_u64().unwrap() as usize) {
            got.extend(p.feed(piece));
        }
        let got: Vec<Value> = got
            .into_iter()
            .map(|(a, b, pl)| json!([a, b, pl.iter().map(|x| format!("{x:02x}")).collect::<String>()]))
            .collect();
        assert_eq!(json!(got), c["messages"], "{c}");
    }
}

#[test]
fn settings_are_bounded_as_python_bounds_them() {
    let d = load();
    assert_eq!(Value::Object(config::defaults()), d["defaults"]);
    for c in d["settings"].as_array().unwrap() {
        let s = config::settings(&c["raw"]);
        assert_eq!(Value::Object(s.clone()), c["settings"], "{}", c["raw"]);
        assert_eq!(Value::Object(config::public_settings(&s)), c["public"], "{}", c["raw"]);
    }
    for c in d["fixed_problems"].as_array().unwrap() {
        assert_eq!(
            config::fixed_position_problem(&obj(&c["fixed"])),
            c["problem"].as_str().unwrap(),
            "{c}"
        );
    }
    for c in d["ntrip_problems"].as_array().unwrap() {
        assert_eq!(
            config::ntrip_problem(&obj(&c["ntrip"])),
            c["problem"].as_str().unwrap(),
            "{c}"
        );
    }
    for c in d["progress"].as_array().unwrap() {
        let got = config::survey_progress(
            Some(&c["survey"]),
            c["duration"].as_i64().unwrap(),
            c["accuracy"].as_f64().unwrap(),
        );
        assert_eq!(got, c["progress"].as_i64().unwrap(), "{c}");
    }
    for c in d["ntrip_requests"].as_array().unwrap() {
        let got = String::from_utf8(ntrip_request(
            c["host"].as_str().unwrap(),
            c["port"].as_i64().unwrap(),
            c["mount"].as_str().unwrap(),
            c["user"].as_str().unwrap(),
            c["password"].as_str().unwrap(),
        ))
        .unwrap();
        let want = c["request"]
            .as_str()
            .unwrap()
            .replace("{version}", corvus_core::version::version());
        // Python encodes with "replace" to ASCII and decodes as latin-1.
        assert_eq!(got, want, "{c}");
    }
}

// ----------------------------------------------------------- simulation ----

fn rtcm(number: u16, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![(number >> 4) as u8, ((number & 0x0F) << 4) as u8];
    payload.extend_from_slice(body);
    let mut out = vec![0xD3, (payload.len() >> 8) as u8 & 0x03, payload.len() as u8];
    out.extend(payload);
    let crc = wire::crc24q(&out);
    out.extend_from_slice(&[(crc >> 16) as u8, (crc >> 8) as u8, crc as u8]);
    out
}

fn mon_ver(protocol: &str, model: &str) -> Vec<u8> {
    let mut payload = vec![0u8; 40];
    payload[..13].copy_from_slice(b"EXT CORE 1.00");
    for ext in [format!("PROTVER={protocol}"), format!("MOD={model}")] {
        let mut e = ext.into_bytes();
        e.resize(30, 0);
        payload.extend(e);
    }
    wire::ubx_frame(wire::UBX_CLASS_MON, wire::UBX_ID_MON_VER, &payload)
}

#[derive(Default)]
struct RState {
    parser: UbxParser,
    out: VecDeque<u8>,
    written: Vec<(u8, u8, Vec<u8>)>,
    mode: u8,
    polls: u32,
    emitting: bool,
    closed: bool,
    last_emit: Option<Instant>,
}

/// A u-blox receiver (or a base that streams on its own) behind a `Port`.
struct Receiver {
    s: Mutex<RState>,
    cv: Condvar,
    answers_ver: bool,
    protocol: &'static str,
    streams_always: bool,
    polls_to_survey: u32,
}

impl Receiver {
    fn new(answers_ver: bool, protocol: &'static str, streams_always: bool) -> Arc<Receiver> {
        Arc::new(Receiver {
            s: Mutex::new(RState::default()),
            cv: Condvar::new(),
            answers_ver,
            protocol,
            streams_always,
            polls_to_survey: 3,
        })
    }

    fn written(&self) -> Vec<(u8, u8, Vec<u8>)> {
        self.s.lock().unwrap().written.clone()
    }

    fn mode(&self) -> u8 {
        self.s.lock().unwrap().mode
    }

    fn emitting(&self) -> bool {
        self.s.lock().unwrap().emitting
    }

    fn handle(&self, st: &mut RState, class: u8, id: u8, payload: &[u8]) {
        st.written.push((class, id, payload.to_vec()));
        if class == wire::UBX_CLASS_MON && id == wire::UBX_ID_MON_VER && payload.is_empty() && self.answers_ver {
            st.out.extend(mon_ver(self.protocol, "ZED-F9P"));
        }
        if class == wire::UBX_CLASS_NAV && id == wire::UBX_ID_NAV_SVIN && payload.is_empty() {
            if st.mode == 1 {
                st.polls += 1;
            }
            let done = st.mode == 1 && st.polls >= self.polls_to_survey;
            let mut p = vec![0u8; 40];
            p[8..12].copy_from_slice(&(st.polls * 10).to_le_bytes());
            p[28..32].copy_from_slice(&(if done { 9000u32 } else { 40000u32 }).to_le_bytes());
            p[36] = done as u8;
            p[37] = (st.mode == 1 && !done) as u8;
            st.out.extend(wire::ubx_frame(class, id, &p));
        }
        if class == wire::UBX_CLASS_CFG && id == wire::UBX_ID_CFG_VALSET {
            let mut at = 4;
            while at + 5 <= payload.len() {
                let key = u32::from_le_bytes(payload[at..at + 4].try_into().unwrap());
                let width = match (key >> 28) & 7 {
                    0..=2 => 1,
                    3 => 2,
                    _ => 4,
                };
                let mut v = [0u8; 8];
                v[..width].copy_from_slice(&payload[at + 4..at + 4 + width]);
                let value = u64::from_le_bytes(v);
                if key == wire::KEY_TMODE_MODE {
                    st.mode = value as u8;
                    if value == 1 {
                        st.polls = 0;
                    }
                }
                if key == wire::KEY_MSGOUT_RTCM_1005_I2C + 1 {
                    st.emitting = value > 0;
                }
                at += 4 + width;
            }
        }
    }
}

impl Port for Receiver {
    fn read(&self, max: usize) -> std::io::Result<Vec<u8>> {
        let mut st = self.s.lock().unwrap();
        if st.closed {
            return Err(std::io::Error::other("Attempting to use a port that is not open"));
        }
        if (st.emitting || self.streams_always) && st.last_emit.is_none_or(|t| t.elapsed() > Duration::from_millis(10))
        {
            st.out.extend(rtcm(1005, &[1; 19]));
            st.out.extend(rtcm(1074, &[2; 200]));
            st.last_emit = Some(Instant::now());
        }
        if st.out.is_empty() {
            st = self.cv.wait_timeout(st, Duration::from_millis(15)).unwrap().0;
        }
        let n = st.out.len().min(max);
        Ok(st.out.drain(..n).collect())
    }

    fn in_waiting(&self) -> std::io::Result<usize> {
        Ok(self.s.lock().unwrap().out.len())
    }

    fn write(&self, data: &[u8]) -> std::io::Result<()> {
        let mut st = self.s.lock().unwrap();
        if st.closed {
            return Err(std::io::Error::other("Attempting to use a port that is not open"));
        }
        let messages = st.parser.feed(data);
        for (c, i, p) in messages {
            self.handle(&mut st, c, i, &p);
        }
        self.cv.notify_all();
        Ok(())
    }

    fn close(&self) {
        self.s.lock().unwrap().closed = true;
    }
}

#[derive(Default)]
struct FakeHost {
    injected: Mutex<Vec<Vec<u8>>>,
    updates: Mutex<Vec<(String, String, f64, i64)>>,
    forgets: AtomicUsize,
    connection: Mutex<String>,
}

impl RtkHost for FakeHost {
    fn inject_rtcm(&self, frame: &[u8]) -> bool {
        self.injected.lock().unwrap().push(frame.to_vec());
        true
    }

    fn rtcm_stats(&self) -> Value {
        json!({"bytes": 0, "messages": self.injected.lock().unwrap().len(), "dropped": 0, "age": null})
    }

    fn forget_rtcm_session(&self) {
        self.forgets.fetch_add(1, Ordering::SeqCst);
    }

    fn connection_string(&self) -> String {
        self.connection.lock().unwrap().clone()
    }

    fn is_connected(&self) -> bool {
        true
    }

    fn store_update(&self, state: &str, source: &str, accuracy: f64, progress: i64) {
        self.updates
            .lock()
            .unwrap()
            .push((state.into(), source.into(), accuracy, progress));
    }

    fn vehicle(&self) -> (Value, Value, Value) {
        (json!("3D fix"), json!(12), json!(0.9))
    }
}

fn fast() -> Timing {
    Timing {
        search_interval: 0.03,
        retry_interval: 0.1,
        read_timeout: 0.02,
        identify_timeout: 0.25,
        listen_timeout: 0.6,
        survey_stop_timeout: 0.6,
        source_silence: 0.6,
        ntrip_connect_timeout: 2.0,
        frame_gap: 0.001,
    }
}

fn zed() -> PortRow {
    PortRow {
        device: "/dev/ttyACM9".into(),
        description: "u-blox GNSS receiver".into(),
        hwid: "USB VID:PID=1546:01A9 SER=".into(),
    }
}

fn io_for(rows: Vec<PortRow>, receiver: Arc<Receiver>, opened: Arc<Mutex<Vec<(String, u32)>>>) -> RtkIo {
    RtkIo {
        open_port: Arc::new(move |device, baud| {
            opened.lock().unwrap().push((device.to_string(), baud));
            receiver.s.lock().unwrap().closed = false;
            Ok(receiver.clone() as Arc<dyn Port>)
        }),
        list_ports: Arc::new(move || rows.clone()),
        open_socket: Arc::new(|_, _, _| Err(std::io::Error::other("no sockets here"))),
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if f() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn state_of(svc: &RtkService) -> String {
    svc.status()["state"].as_str().unwrap().to_string()
}

#[test]
fn a_base_is_found_surveyed_and_streamed_and_put_back_on_stop() {
    let host = Arc::new(FakeHost::default());
    let rx = Receiver::new(true, "27.31", false);
    let opened = Arc::new(Mutex::new(Vec::new()));
    let svc = RtkService::new(
        host.clone(),
        &json!({"enabled": true, "survey_duration": 10, "survey_accuracy": 2.0}),
        io_for(vec![zed()], rx.clone(), opened.clone()),
        fast(),
    );
    svc.start();
    wait_for("the base to be active", || state_of(&svc) == "active");
    let s = svc.status();
    assert_eq!(s["device"], "/dev/ttyACM9");
    assert_eq!(s["baud"], 38400);
    assert_eq!(
        s["receiver_label"],
        "u-blox ZED-F9P \u{b7} EXT CORE 1.00 \u{b7} protocol 27.31"
    );
    assert_eq!(s["survey_progress"], 100);
    assert_eq!(s["ports"][0]["device"], "/dev/ttyACM9");
    assert_eq!(s["vehicle"], json!({"fix": "3D fix", "satellites": 12, "hdop": 0.9}));
    assert_eq!(opened.lock().unwrap()[0], ("/dev/ttyACM9".to_string(), 38400));
    wait_for("corrections", || svc.status()["frames"].as_i64().unwrap() >= 4);
    let s = svc.status();
    assert!(s["messages"]["1005"].as_i64().unwrap() >= 2 && s["messages"]["1074"].as_i64().unwrap() >= 2);
    assert!(host.injected.lock().unwrap().len() >= 4);
    assert!(host.injected.lock().unwrap().iter().all(|f| f[0] == 0xD3));
    assert_eq!(rx.mode(), 1);
    assert!(rx.emitting());
    assert!(host
        .updates
        .lock()
        .unwrap()
        .iter()
        .any(|u| u.0 == "surveying" && u.1 == "usb"));
    assert!(host
        .updates
        .lock()
        .unwrap()
        .iter()
        .any(|u| u.0 == "active" && u.3 == 100));

    svc.stop(3.0);
    assert!(!svc.is_alive());
    assert_eq!(state_of(&svc), "off");
    assert_eq!(rx.mode(), 0, "the receiver is out of base mode");
    assert!(!rx.emitting());
    let last = rx
        .written()
        .into_iter()
        .rev()
        .find(|w| w.1 == wire::UBX_ID_CFG_VALSET)
        .unwrap();
    assert_eq!(
        wire::ubx_frame(last.0, last.1, &last.2),
        wire::disable_base_frames(false)[0]
    );
}

#[test]
fn a_fixed_base_is_configured_without_a_survey_and_an_unset_position_is_an_error() {
    let host = Arc::new(FakeHost::default());
    let rx = Receiver::new(true, "27.31", false);
    let svc = RtkService::new(
        host,
        &json!({"enabled": true, "mode": "fixed", "fixed": {"latitude": 47.5, "longitude": 8.5, "altitude": 500.0, "accuracy": 0.1}}),
        io_for(vec![zed()], rx.clone(), Arc::default()),
        fast(),
    );
    svc.start();
    wait_for("active", || state_of(&svc) == "active");
    assert_eq!(rx.mode(), 2);
    assert_eq!(svc.status()["message"], "Correcting from the fixed base position");
    svc.stop(3.0);

    let rx = Receiver::new(true, "27.31", false);
    let svc = RtkService::new(
        Arc::new(FakeHost::default()),
        &json!({"enabled": true, "mode": "fixed"}),
        io_for(vec![zed()], rx, Arc::default()),
        fast(),
    );
    svc.start();
    wait_for("the error", || state_of(&svc) == "error");
    assert!(svc.status()["error"]
        .as_str()
        .unwrap()
        .starts_with("the fixed base position is still 0, 0"));
    svc.stop(3.0);
}

#[test]
fn an_old_receiver_is_configured_the_old_way() {
    let rx = Receiver::new(true, "18.00", false);
    let svc = RtkService::new(
        Arc::new(FakeHost::default()),
        &json!({"enabled": true, "mode": "fixed", "fixed": {"latitude": 47.5, "longitude": 8.5}}),
        io_for(vec![zed()], rx.clone(), Arc::default()),
        fast(),
    );
    svc.start();
    wait_for("active", || state_of(&svc) == "active");
    assert!(
        rx.written().iter().any(|w| w.1 == wire::UBX_ID_CFG_TMODE3),
        "CFG-TMODE3 was written"
    );
    assert!(!rx.written().iter().any(|w| w.1 == wire::UBX_ID_CFG_VALSET));
    svc.stop(3.0);
}

#[test]
fn a_base_that_streams_by_itself_is_forwarded_and_never_written_to() {
    let host = Arc::new(FakeHost::default());
    let rx = Receiver::new(false, "27.31", true);
    let svc = RtkService::new(
        host.clone(),
        &json!({"enabled": true}),
        io_for(vec![zed()], rx.clone(), Arc::default()),
        fast(),
    );
    svc.start();
    wait_for("forwarding", || state_of(&svc) == "active");
    assert!(svc.status()["message"]
        .as_str()
        .unwrap()
        .starts_with("Forwarding corrections from /dev/ttyACM9 @ 38400"));
    wait_for("frames", || host.injected.lock().unwrap().len() >= 4);
    svc.stop(3.0);
    let written = rx.written();
    assert_eq!(written.len(), 1, "only the MON-VER poll was written: {written:?}");
    assert_eq!(
        (written[0].0, written[0].1),
        (wire::UBX_CLASS_MON, wire::UBX_ID_MON_VER)
    );
}

#[test]
fn a_silent_port_is_an_error_after_the_listen_time_and_is_tried_at_every_rate() {
    let rx = Receiver::new(false, "27.31", false);
    let opened = Arc::new(Mutex::new(Vec::new()));
    let svc = RtkService::new(
        Arc::new(FakeHost::default()),
        &json!({"enabled": true}),
        io_for(vec![zed()], rx, opened.clone()),
        fast(),
    );
    svc.start();
    wait_for("the error", || state_of(&svc) == "error");
    assert!(svc.status()["error"]
        .as_str()
        .unwrap()
        .contains("produced no corrections"));
    svc.stop(3.0);
    let bauds: Vec<u32> = opened.lock().unwrap().iter().map(|o| o.1).take(7).collect();
    assert_eq!(bauds, [38400, 9600, 115200, 57600, 19200, 460800, 38400]);
}

#[test]
fn only_a_gnss_receiver_is_a_candidate_and_never_the_telemetry_port() {
    let host = Arc::new(FakeHost::default());
    *host.connection.lock().unwrap() = "serial:/dev/ttyACM9:57600".into();
    let pixhawk = PortRow {
        device: "/dev/ttyACM0".into(),
        description: "PX4 FMU v5.x".into(),
        hwid: "USB VID:PID=26AC:0032".into(),
    };
    let svc = RtkService::new(
        host.clone(),
        &json!({"enabled": false}),
        io_for(vec![pixhawk, zed()], Receiver::new(false, "27", false), Arc::default()),
        fast(),
    );
    assert_eq!(svc.status()["ports"], json!([]), "the u-blox is the link's port");
    *host.connection.lock().unwrap() = "udp:127.0.0.1:14550".into();
    assert_eq!(svc.status()["ports"].as_array().unwrap().len(), 1);

    *host.connection.lock().unwrap() = "serial:/dev/ttyACM9:57600".into();
    let svc = RtkService::new(
        host,
        &json!({"enabled": true, "device": "/dev/ttyACM9"}),
        io_for(vec![zed()], Receiver::new(false, "27", false), Arc::default()),
        fast(),
    );
    svc.start();
    wait_for("the refusal", || state_of(&svc) == "error");
    assert!(svc.status()["error"]
        .as_str()
        .unwrap()
        .starts_with("that port is the vehicle's telemetry link"));
    svc.stop(3.0);
}

#[test]
fn switching_off_and_on_and_restarting_the_survey_start_over() {
    let host = Arc::new(FakeHost::default());
    let rx = Receiver::new(true, "27.31", false);
    let svc = RtkService::new(
        host.clone(),
        &json!({"enabled": false}),
        io_for(vec![zed()], rx.clone(), Arc::default()),
        fast(),
    );
    svc.start();
    wait_for("off", || {
        state_of(&svc) == "off" && svc.status()["message"] == "RTK corrections are switched off"
    });
    let resolved = svc.apply_settings(&json!({"enabled": true, "survey_duration": 10}));
    assert_eq!(resolved["enabled"], true);
    wait_for("active", || state_of(&svc) == "active");
    let before = host.forgets.load(Ordering::SeqCst);
    svc.restart_survey();
    wait_for("a second survey", || {
        host.forgets.load(Ordering::SeqCst) > before && state_of(&svc) == "active"
    });
    svc.apply_settings(&json!({"enabled": false}));
    wait_for("off again", || state_of(&svc) == "off");
    assert_eq!(rx.mode(), 0);
    svc.stop(3.0);
    assert!(!svc.is_alive());
}

#[test]
fn stopping_twice_and_starting_twice_leave_one_thread() {
    let svc = RtkService::new(
        Arc::new(FakeHost::default()),
        &json!({"enabled": true}),
        io_for(vec![], Receiver::new(false, "27", false), Arc::default()),
        fast(),
    );
    svc.start();
    svc.start();
    wait_for("searching", || state_of(&svc) == "searching");
    svc.stop(3.0);
    svc.stop(3.0);
    assert!(!svc.is_alive());
}

// ----------------------------------------------------------------- NTRIP ----

fn caster(reply: &'static [u8], then: Vec<Vec<u8>>) -> (u16, Arc<Mutex<String>>, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let request = Arc::new(Mutex::new(String::new()));
    let done = Arc::new(AtomicBool::new(false));
    let (r, d) = (request.clone(), done.clone());
    thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = [0u8; 2048];
        let n = s.read(&mut buf).unwrap();
        *r.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).into_owned();
        let _ = s.write_all(reply);
        for chunk in then {
            thread::sleep(Duration::from_millis(15));
            if s.write_all(&chunk).is_err() {
                break;
            }
        }
        while !d.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(10));
            if s.write_all(&rtcm(1230, &[9; 5])).is_err() {
                break;
            }
        }
    });
    (port, request, done)
}

fn ntrip_io() -> RtkIo {
    let timing = fast();
    let sys = RtkIo::system(timing);
    RtkIo {
        open_port: Arc::new(|_, _| Err(std::io::Error::other("no serial"))),
        list_ports: Arc::new(Vec::new),
        open_socket: sys.open_socket,
    }
}

#[test]
fn an_ntrip_caster_is_read_from_the_bytes_after_its_header_on() {
    let host = Arc::new(FakeHost::default());
    let first = [b"ICY 200 OK\r\n\r\n".to_vec(), rtcm(1005, &[5; 19])].concat();
    let first: &'static [u8] = Box::leak(first.into_boxed_slice());
    let (port, request, done) = caster(first, vec![rtcm(1074, &[6; 100])]);
    let svc = RtkService::new(
        host.clone(),
        &json!({"enabled": true, "source": "ntrip", "ntrip": {"host": "127.0.0.1", "port": port, "mountpoint": "/MOUNT", "username": "u", "password": "p"}}),
        ntrip_io(),
        fast(),
    );
    svc.start();
    wait_for("active", || state_of(&svc) == "active");
    wait_for("frames", || host.injected.lock().unwrap().len() >= 3);
    let injected = host.injected.lock().unwrap().clone();
    assert_eq!(
        injected[0],
        rtcm(1005, &[5; 19]),
        "the correction in the header's packet is not lost"
    );
    assert_eq!(injected[1], rtcm(1074, &[6; 100]));
    let req = request.lock().unwrap().clone();
    assert!(
        req.starts_with("GET /MOUNT HTTP/1.1\r\n") && req.contains("Authorization: Basic dTpw\r\n"),
        "{req}"
    );
    assert_eq!(svc.status()["source"], "ntrip");
    assert_eq!(svc.status()["device"], format!("127.0.0.1:{port}/MOUNT"));
    done.store(true, Ordering::SeqCst);
    svc.stop(3.0);
    assert!(!svc.is_alive());
}

#[test]
fn a_caster_that_refuses_says_why() {
    for (reply, want) in [
        (
            &b"HTTP/1.1 401 Unauthorized\r\n\r\n"[..],
            "the caster rejected the username or password",
        ),
        (
            &b"HTTP/1.1 404 Not Found\r\n\r\n"[..],
            "the caster has no such mountpoint",
        ),
        (&b"SOURCETABLE 200 OK\r\nSTR;x\r\nENDSOURCETABLE\r\n\r\n"[..], "200"),
        (
            &b"HTTP/1.1 503 Busy\r\n\r\n"[..],
            "the caster refused the stream: HTTP/1.1 503 Busy",
        ),
    ] {
        let (port, _, done) = caster(reply, vec![]);
        let svc = RtkService::new(
            Arc::new(FakeHost::default()),
            &json!({"enabled": true, "source": "ntrip", "ntrip": {"host": "127.0.0.1", "port": port, "mountpoint": "m"}}),
            ntrip_io(),
            fast(),
        );
        svc.start();
        if want == "200" {
            wait_for("a stream", || state_of(&svc) == "active");
        } else {
            wait_for("the error", || state_of(&svc) == "error");
            assert_eq!(svc.status()["error"], want);
        }
        done.store(true, Ordering::SeqCst);
        svc.stop(3.0);
    }
}

#[test]
fn an_unreachable_caster_and_a_missing_address_are_errors() {
    let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let svc = RtkService::new(
        Arc::new(FakeHost::default()),
        &json!({"enabled": true, "source": "ntrip", "ntrip": {"host": "127.0.0.1", "port": free, "mountpoint": "m"}}),
        ntrip_io(),
        fast(),
    );
    svc.start();
    wait_for("the error", || state_of(&svc) == "error");
    // Windows retries a refused connection until the connect timeout runs out,
    // so what it reports is a timeout, which carries no errno.
    let prefix = if cfg!(windows) {
        "could not open the correction source: "
    } else {
        "could not open the correction source: [Errno"
    };
    assert!(svc.status()["error"].as_str().unwrap().starts_with(prefix));
    svc.stop(3.0);
    let svc = RtkService::new(
        Arc::new(FakeHost::default()),
        &json!({"enabled": true, "source": "ntrip"}),
        ntrip_io(),
        fast(),
    );
    svc.start();
    wait_for("the error", || state_of(&svc) == "error");
    assert_eq!(svc.status()["error"], "no NTRIP caster address is set");
    svc.stop(3.0);
}

#[test]
fn a_caster_that_goes_quiet_ends_the_session() {
    let first: &'static [u8] = Box::leak(
        [b"ICY 200 OK\r\n\r\n".to_vec(), rtcm(1005, &[5; 19])]
            .concat()
            .into_boxed_slice(),
    );
    let (port, _, done) = caster(first, vec![]);
    done.store(true, Ordering::SeqCst);
    let svc = RtkService::new(
        Arc::new(FakeHost::default()),
        &json!({"enabled": true, "source": "ntrip", "ntrip": {"host": "127.0.0.1", "port": port, "mountpoint": "m"}}),
        ntrip_io(),
        fast(),
    );
    svc.start();
    wait_for("the silence error", || {
        svc.status()["error"]
            .as_str()
            .unwrap_or("")
            .ends_with("stopped sending corrections")
    });
    svc.stop(3.0);
}

#[test]
fn the_socket_trait_closes_under_a_blocked_reader() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let t = thread::spawn(move || listener.accept().map(|(s, _)| s));
    let sock = Arc::new(corvus_core::peripherals::rtk::io::TcpSocket::connect("127.0.0.1", port, 2.0, 5.0).unwrap());
    let _peer = t.join().unwrap().unwrap();
    let s2 = sock.clone();
    let reader = thread::spawn(move || s2.recv(10));
    thread::sleep(Duration::from_millis(100));
    let t0 = Instant::now();
    sock.close();
    let r = reader.join().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert!(r.is_err() || r.unwrap().is_empty());
}

// ------------------------------------------------------------------- pty ----

#[cfg(unix)]
#[test]
fn a_real_serial_port_on_a_pty_is_identified_and_configured() {
    use corvus_core::peripherals::rtk::io::SerialPort;
    use std::os::fd::FromRawFd;

    let mut master = -1;
    let mut slave = -1;
    let mut name = [0 as libc::c_char; 128];
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            name.as_mut_ptr(),
            std::ptr::null_mut::<libc::termios>() as _,
            std::ptr::null_mut::<libc::winsize>() as _,
        )
    };
    assert_eq!(rc, 0);
    let path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    unsafe { libc::close(slave) };
    // Raw mode on the master side so the line discipline leaves bytes alone.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        libc::tcgetattr(master, &mut t);
        libc::cfmakeraw(&mut t);
        libc::tcsetattr(master, libc::TCSANOW, &t);
    }
    let mut m = unsafe { std::fs::File::from_raw_fd(master) };
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::<(u8, u8)>::new()));
    let (s2, seen2) = (stop.clone(), seen.clone());
    let m2 = m.try_clone().unwrap();
    let sim = thread::spawn(move || {
        let mut m = m2;
        let mut parser = UbxParser::new();
        let mut buf = [0u8; 512];
        let mut mode = 0u8;
        let mut polls = 0u32;
        unsafe {
            let fl = libc::fcntl(std::os::fd::AsRawFd::as_raw_fd(&m), libc::F_GETFL);
            libc::fcntl(
                std::os::fd::AsRawFd::as_raw_fd(&m),
                libc::F_SETFL,
                fl | libc::O_NONBLOCK,
            );
        }
        while !s2.load(Ordering::SeqCst) {
            match m.read(&mut buf) {
                Ok(n) if n > 0 => {
                    for (c, i, p) in parser.feed(&buf[..n]) {
                        seen2.lock().unwrap().push((c, i));
                        if (c, i) == (wire::UBX_CLASS_MON, wire::UBX_ID_MON_VER) {
                            let _ = m.write_all(&mon_ver("27.31", "ZED-F9P"));
                        }
                        if (c, i) == (wire::UBX_CLASS_NAV, wire::UBX_ID_NAV_SVIN) {
                            polls += (mode == 1) as u32;
                            let mut q = vec![0u8; 40];
                            q[36] = (polls >= 2) as u8;
                            q[37] = (mode == 1 && polls < 2) as u8;
                            let _ = m.write_all(&wire::ubx_frame(c, i, &q));
                        }
                        if (c, i) == (wire::UBX_CLASS_CFG, wire::UBX_ID_CFG_VALSET)
                            && p.len() >= 9
                            && p[4..8] == wire::KEY_TMODE_MODE.to_le_bytes()
                        {
                            mode = p[8];
                        }
                    }
                }
                _ => thread::sleep(Duration::from_millis(5)),
            }
            if mode == 1 && polls >= 2 {
                let _ = m.write_all(&rtcm(1005, &[1; 19]));
            }
        }
    });
    let host = Arc::new(FakeHost::default());
    // Linux names a pty /dev/pts/N, which the service rightly never takes for
    // a receiver, so the receiver is reached through a link with a plain name.
    let dir = std::env::temp_dir().join(format!("corvus-pty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let link = dir.join("ttyACM9");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let rows = vec![PortRow {
        device: link.to_string_lossy().into_owned(),
        description: "u-blox GNSS receiver".into(),
        hwid: "USB VID:PID=1546:01A9".into(),
    }];
    let timing = Timing {
        identify_timeout: 1.0,
        ..fast()
    };
    let mut io = RtkIo::system(timing);
    io.list_ports = Arc::new(move || rows.clone());
    let svc = RtkService::new(
        host.clone(),
        &json!({"enabled": true, "baud": 38400, "survey_duration": 10}),
        io,
        timing,
    );
    svc.start();
    wait_for("active over the pty", || state_of(&svc) == "active");
    wait_for("a correction", || !host.injected.lock().unwrap().is_empty());
    svc.stop(3.0);
    stop.store(true, Ordering::SeqCst);
    sim.join().unwrap();
    assert!(seen
        .lock()
        .unwrap()
        .contains(&(wire::UBX_CLASS_MON, wire::UBX_ID_MON_VER)));
    drop(m.write(b""));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = SerialPort::open("/definitely/not/a/port", 9600, 0.1, 0.1)
        .err()
        .expect("a missing port does not open");
}
