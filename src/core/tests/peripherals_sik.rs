//! The SiK radio schema against what Python answered, and the AT sessions
//! against a simulated radio pair (no hardware).

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use corvus_core::peripherals::ports::PortRow;
use corvus_core::peripherals::sik::config;
use corvus_core::peripherals::sik::service::{
    OpenSikPort, SikFailure, SikHost, SikPort, SikService, SikTiming, ARMED_MESSAGE, BUSY_MESSAGE,
};
use serde_json::{json, Map, Value};

fn load() -> Value {
    let path = format!("{}/tests/data/peripherals_sik_cases.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn obj(v: &Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

#[test]
fn the_register_table_and_every_reading_are_the_pythons() {
    let d = load();
    assert_eq!(config::schema(), d["schema"]);
    for c in d["registers"].as_array().unwrap() {
        assert_eq!(
            Value::Object(config::parse_registers(c["text"].as_str().unwrap())),
            c["result"],
            "{c}"
        );
    }
    for c in d["rssi"].as_array().unwrap() {
        assert_eq!(
            config::parse_rssi(c["text"].as_str().unwrap()).unwrap_or(Value::Null),
            c["result"],
            "{c}"
        );
    }
    for c in d["versions"].as_array().unwrap() {
        assert_eq!(
            config::parse_version(c["text"].as_str().unwrap()),
            c["result"].as_str().unwrap(),
            "{c}"
        );
    }
    for c in d["describe"].as_array().unwrap() {
        let got = config::describe_radio(
            &obj(&c["registers"]),
            c["version"].as_str().unwrap(),
            c["board"].as_str().unwrap(),
        );
        assert_eq!(got, c["result"], "{c}");
    }
    for c in d["pair"].as_array().unwrap() {
        let remote = if c["remote"].is_null() {
            None
        } else {
            Some(&c["remote"])
        };
        assert_eq!(json!(config::compare(&c["local"], remote)), c["compare"], "{c}");
        assert_eq!(
            Value::Object(config::copy_to_remote(&c["local"], remote)),
            c["copy"],
            "{c}"
        );
    }
    for c in d["baud"].as_array().unwrap() {
        assert_eq!(
            config::baud_from_register(c["value"].as_i64().unwrap()),
            c["baud"].as_i64().unwrap()
        );
    }
    for c in d["register"].as_array().unwrap() {
        assert_eq!(
            config::register_from_baud(c["baud"].as_i64().unwrap()),
            c["register"].as_i64().unwrap()
        );
    }
    for c in d["dbm"].as_array().unwrap() {
        assert_eq!(
            config::rssi_to_dbm(c["rssi"].as_f64().unwrap()),
            c["dbm"].as_f64().unwrap()
        );
    }
}

#[test]
fn every_write_is_accepted_or_refused_with_the_pythons_words() {
    let d = load();
    let present = obj(&d["present"]);
    for c in d["writes"].as_array().unwrap() {
        let got = config::validate_writes(&obj(&c["settings"]), &present);
        match (&got, c.get("error")) {
            (Err(e), Some(want)) => assert_eq!(&e.0, want.as_str().unwrap(), "{c}"),
            (Ok(w), None) => {
                assert_eq!(json!(w), c["writes"], "{c}");
                assert_eq!(json!(config::write_plan(w, false)), c["plan_local"], "{c}");
                assert_eq!(json!(config::write_plan(w, true)), c["plan_remote"], "{c}");
            }
            other => panic!("{c}: {other:?}"),
        }
    }
}

// ----------------------------------------------------------- simulation ----

struct Radio {
    baud: u32,
    registers: Mutex<Vec<(String, i64, i64)>>,
    reachable_remote: bool,
    reject: Option<String>,
}

fn pair_registers(netid: i64) -> Vec<(String, i64, i64)> {
    [
        ("FORMAT", 25),
        ("SERIAL_SPEED", 57),
        ("AIR_SPEED", 64),
        ("NETID", netid),
        ("TXPOWER", 20),
        ("ECC", 0),
        ("MAVLINK", 1),
        ("MIN_FREQ", 915000),
        ("MAX_FREQ", 928000),
        ("NUM_CHANNELS", 20),
        ("MAX_WINDOW", 131),
    ]
    .iter()
    .enumerate()
    .map(|(i, (n, v))| (n.to_string(), i as i64, *v))
    .collect()
}

struct World {
    local: Radio,
    remote: Mutex<Vec<(String, i64, i64)>>,
    sent: Mutex<Vec<String>>,
    opened: Mutex<Vec<u32>>,
    closed: AtomicBool,
}

struct Wire {
    world: Arc<World>,
    baud: u32,
    command_mode: Mutex<bool>,
    out: Mutex<VecDeque<u8>>,
    pending: Mutex<Vec<u8>>,
}

impl Wire {
    fn say(&self, text: &str) {
        self.out.lock().unwrap().extend(text.bytes());
    }

    fn registers_text(regs: &[(String, i64, i64)]) -> String {
        regs.iter().map(|(n, num, v)| format!("S{num}: {n}={v}\r\n")).collect()
    }

    fn handle(&self, line: &str) {
        self.world.sent.lock().unwrap().push(line.to_string());
        self.say(&format!("{line}\r\n"));
        let remote = line.starts_with("RT");
        if remote && !self.world.local.reachable_remote {
            return;
        }
        let body = &line[2..];
        let regs: &Mutex<Vec<(String, i64, i64)>> = if remote {
            &self.world.remote
        } else {
            &self.world.local.registers
        };
        match body {
            "I5" => {
                let text = Self::registers_text(&regs.lock().unwrap());
                self.say(&text);
                self.say("OK\r\n");
            }
            "I" => self.say("SiK 2.0 on HM-TRP\r\nOK\r\n"),
            "I2" => self.say("26\r\nOK\r\n"),
            "I7" => self.say("L/R RSSI: 190/180  L/R noise: 60/55 pkts: 10\r\nOK\r\n"),
            "&W" | "Z" | "&F" => self.say("OK\r\n"),
            "O" => {
                *self.command_mode.lock().unwrap() = false;
                self.say("OK\r\n");
            }
            _ if body.starts_with('S') && body.contains('=') => {
                let (n, v) = body[1..].split_once('=').unwrap();
                if self.world.local.reject.as_deref() == Some(line) {
                    self.say("ERROR\r\n");
                    return;
                }
                let (n, v): (i64, i64) = (n.parse().unwrap(), v.parse().unwrap());
                for r in regs.lock().unwrap().iter_mut() {
                    if r.1 == n {
                        r.2 = v;
                    }
                }
                self.say("OK\r\n");
            }
            _ => self.say("ERROR\r\n"),
        }
    }
}

impl SikPort for Wire {
    fn in_waiting(&self) -> io::Result<usize> {
        Ok(self.out.lock().unwrap().len())
    }

    fn read(&self, n: usize) -> io::Result<Vec<u8>> {
        let mut out = self.out.lock().unwrap();
        let k = out.len().min(n);
        Ok(out.drain(..k).collect())
    }

    fn write(&self, data: &[u8]) -> io::Result<()> {
        if self.baud != self.world.local.baud {
            return Ok(());
        }
        let mut cm = self.command_mode.lock().unwrap();
        if data == b"+++" {
            *cm = true;
            drop(cm);
            self.say("OK\r\n");
            return Ok(());
        }
        if !*cm {
            return Ok(());
        }
        drop(cm);
        let mut pending = self.pending.lock().unwrap();
        pending.extend_from_slice(data);
        while let Some(end) = pending.windows(2).position(|w| w == b"\r\n") {
            let line = String::from_utf8_lossy(&pending[..end]).into_owned();
            pending.drain(..end + 2);
            drop(pending);
            self.handle(&line);
            pending = self.pending.lock().unwrap();
        }
        Ok(())
    }

    fn reset_input(&self) -> io::Result<()> {
        self.out.lock().unwrap().clear();
        Ok(())
    }

    fn close(&self) {
        self.world.closed.store(true, Ordering::SeqCst);
    }
}

fn world(baud: u32, reachable: bool, reject: Option<&str>) -> Arc<World> {
    Arc::new(World {
        local: Radio {
            baud,
            registers: Mutex::new(pair_registers(25)),
            reachable_remote: reachable,
            reject: reject.map(str::to_string),
        },
        remote: Mutex::new(pair_registers(26)),
        sent: Mutex::new(Vec::new()),
        opened: Mutex::new(Vec::new()),
        closed: AtomicBool::new(false),
    })
}

fn opener(w: &Arc<World>) -> OpenSikPort {
    let w = Arc::clone(w);
    Arc::new(move |_device, baud| {
        w.opened.lock().unwrap().push(baud);
        Ok(Arc::new(Wire {
            world: Arc::clone(&w),
            baud,
            command_mode: Mutex::new(false),
            out: Mutex::new(VecDeque::new()),
            pending: Mutex::new(Vec::new()),
        }) as Arc<dyn SikPort>)
    })
}

#[derive(Default)]
struct Host {
    armed: AtomicBool,
    conn: Mutex<String>,
    running: AtomicBool,
    calls: Mutex<Vec<String>>,
}

impl SikHost for Host {
    fn armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    fn connection_string(&self) -> String {
        self.conn.lock().unwrap().clone()
    }

    fn link_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn stop_link(&self) {
        self.calls.lock().unwrap().push("stop".into());
    }

    fn restart_link(&self, conn: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("restart {conn}"));
        Ok(())
    }

    fn list_ports(&self) -> Vec<PortRow> {
        vec![
            PortRow {
                device: "/dev/ttyUSB0".into(),
                description: "FT230X".into(),
                hwid: "USB VID:PID=0403:6015".into(),
            },
            PortRow {
                device: "/dev/cu.usbmodem1".into(),
                description: "PX4 FMU".into(),
                hwid: "USB VID:PID=26AC:0032".into(),
            },
        ]
    }
}

fn quick() -> SikTiming {
    SikTiming {
        guard: 0.02,
        idle: 0.05,
        local_timeout: 0.4,
        remote_timeout: 0.6,
        reboot: 0.02,
    }
}

fn service(w: &Arc<World>, host: &Arc<Host>) -> SikService {
    SikService::new(host.clone(), opener(w), quick())
}

#[test]
fn both_radios_are_read_and_the_mismatch_is_named() {
    let w = world(57600, true, None);
    let host = Arc::new(Host::default());
    let r = service(&w, &host).load("/dev/ttyUSB0", Some(57600), true).unwrap();
    assert_eq!(r["remote_reachable"], true);
    assert_eq!(
        r["mismatches"],
        json!([{"name": "NETID", "label": "Network ID", "local": 25, "remote": 26}])
    );
    assert_eq!(r["local"]["version"], "SiK 2.0 on HM-TRP");
    assert_eq!(r["local"]["board"], "26");
    assert_eq!(r["link"]["local_margin_db"], 65.0);
    assert_eq!(
        (r["device"].as_str(), r["baud"].as_i64()),
        (Some("/dev/ttyUSB0"), Some(57600))
    );
    assert!(w.closed.load(Ordering::SeqCst), "the port is closed");
    assert_eq!(
        w.sent.lock().unwrap().last().map(String::as_str),
        Some("ATO"),
        "the radio is handed back"
    );
    assert!(
        host.calls.lock().unwrap().is_empty(),
        "another port: the bridge is not touched"
    );
}

#[test]
fn a_remote_that_does_not_answer_is_not_a_mismatch_and_may_be_skipped() {
    let w = world(57600, false, None);
    let r = service(&w, &Arc::default())
        .load("/dev/ttyUSB0", Some(57600), true)
        .unwrap();
    assert_eq!(
        (
            r["remote_reachable"].clone(),
            r["remote"].clone(),
            r["mismatches"].clone()
        ),
        (json!(false), Value::Null, json!([]))
    );
    let w = world(57600, true, None);
    let r = service(&w, &Arc::default())
        .load("/dev/ttyUSB0", Some(57600), false)
        .unwrap();
    assert_eq!(r["remote_reachable"], false);
    assert!(!w.sent.lock().unwrap().iter().any(|c| c.starts_with("RT")));
}

#[test]
fn the_far_radio_is_written_before_the_near_one_and_each_is_committed_and_rebooted() {
    let w = world(57600, true, None);
    let host = Arc::new(Host::default());
    let local = obj(&json!({"NETID": 30}));
    let remote = obj(&json!({"netid": 30, "TXPOWER": 11}));
    let r = service(&w, &host)
        .save("/dev/ttyUSB0", Some(57600), Some(&local), Some(&remote))
        .unwrap();
    let sent = w.sent.lock().unwrap().clone();
    let writes: Vec<&str> = sent
        .iter()
        .map(String::as_str)
        .filter(|c| c.contains('=') || c.ends_with("&W") || c.ends_with('Z'))
        .collect();
    assert_eq!(writes, ["RTS3=30", "RTS4=11", "RT&W", "RTZ", "ATS3=30", "AT&W", "ATZ"]);
    assert_eq!(r["applied"]["remote"].as_array().unwrap().len(), 2);
    assert_eq!(
        r["applied"]["local"],
        json!([{"name": "NETID", "register": 3, "value": 30}])
    );
    assert_eq!(r["warnings"], json!([]));
    assert_eq!(r["new_serial_speed"], 0);
}

#[test]
fn a_rejected_write_and_an_unknown_register_are_refused_in_the_operators_words() {
    let w = world(57600, true, Some("ATS3=30"));
    let local = obj(&json!({"NETID": 30}));
    let e = service(&w, &Arc::default())
        .save("/dev/ttyUSB0", Some(57600), Some(&local), None)
        .unwrap_err();
    assert_eq!(e, SikFailure::Sik("the radio rejected ATS3=30".into()));
    let w = world(57600, true, None);
    let local = obj(&json!({"NOPE": 1}));
    let e = service(&w, &Arc::default())
        .save("/dev/ttyUSB0", Some(57600), Some(&local), None)
        .unwrap_err();
    assert_eq!(e, SikFailure::Config("this radio has no NOPE setting".into()));
    assert!(w.closed.load(Ordering::SeqCst), "the port is closed after a refusal");
    let w = world(57600, false, None);
    let remote = obj(&json!({"NETID": 30}));
    let e = service(&w, &Arc::default())
        .save("/dev/ttyUSB0", Some(57600), None, Some(&remote))
        .unwrap_err();
    assert!(
        matches!(&e, SikFailure::Sik(t) if t.starts_with("the remote radio is not answering, so its settings cannot be written"))
    );
}

#[test]
fn the_link_is_stopped_for_the_session_and_put_back_and_moved_to_a_new_baud() {
    let w = world(57600, true, None);
    let host = Arc::new(Host::default());
    *host.conn.lock().unwrap() = "serial:/dev/ttyUSB0:57600".into();
    host.running.store(true, Ordering::SeqCst);
    let svc = service(&w, &host);
    svc.load("/dev/ttyUSB0", Some(57600), false).unwrap();
    assert_eq!(
        *host.calls.lock().unwrap(),
        ["stop", "restart serial:/dev/ttyUSB0:57600"]
    );

    host.calls.lock().unwrap().clear();
    let local = obj(&json!({"SERIAL_SPEED": 115}));
    let r = svc.save("/dev/ttyUSB0", Some(57600), Some(&local), None).unwrap();
    assert_eq!(r["new_serial_speed"], 115200);
    assert!(r["warnings"][0]
        .as_str()
        .unwrap()
        .starts_with("This radio now talks to its cable at 115200 baud."));
    assert_eq!(
        *host.calls.lock().unwrap(),
        [
            "stop",
            "restart serial:/dev/ttyUSB0:57600",
            "stop",
            "restart serial:/dev/ttyUSB0:115200"
        ]
    );

    host.calls.lock().unwrap().clear();
    host.running.store(false, Ordering::SeqCst);
    svc.load("/dev/ttyUSB0", Some(57600), false).unwrap();
    assert!(
        host.calls.lock().unwrap().is_empty(),
        "a link that was not running is not started"
    );
    svc.shutdown();
    host.running.store(true, Ordering::SeqCst);
    svc.load("/dev/ttyUSB0", Some(57600), false).unwrap();
    assert_eq!(
        *host.calls.lock().unwrap(),
        ["stop"],
        "a shutting down service does not respawn the link"
    );
}

#[test]
fn the_baud_is_scanned_for_and_a_silent_port_says_what_was_tried() {
    let w = world(115200, true, None);
    let r = service(&w, &Arc::default())
        .load("/dev/ttyUSB0", Some(9600), false)
        .unwrap();
    assert_eq!(r["baud"], 115200);
    assert_eq!(*w.opened.lock().unwrap(), [9600, 57600, 115200]);

    let w = world(1200, true, None);
    let e = service(&w, &Arc::default())
        .load("/dev/ttyUSB0", Some(57600), false)
        .unwrap_err();
    let SikFailure::Sik(text) = e else { panic!() };
    assert!(
        text.starts_with("could not reach a radio on /dev/ttyUSB0 (no answer at 4800 baud). Check that"),
        "{text}"
    );
    assert_eq!(
        *w.opened.lock().unwrap(),
        [57600, 115200, 38400, 19200, 9600, 230400, 2400, 4800],
        "each distinct baud is opened once"
    );
}

#[test]
fn an_armed_vehicle_a_busy_service_and_a_missing_port_are_refused() {
    let w = world(57600, true, None);
    let host = Arc::new(Host::default());
    let svc = Arc::new(service(&w, &host));
    host.armed.store(true, Ordering::SeqCst);
    assert_eq!(
        svc.load("/dev/ttyUSB0", None, true).unwrap_err(),
        SikFailure::Sik(ARMED_MESSAGE.into())
    );
    let status = svc.status();
    assert_eq!(
        (status["can_configure"].clone(), status["blocked_reason"].clone()),
        (json!(false), json!(ARMED_MESSAGE))
    );
    host.armed.store(false, Ordering::SeqCst);
    assert_eq!(
        svc.load("  ", None, true).unwrap_err(),
        SikFailure::Sik("no serial port was selected".into())
    );
    assert_eq!(
        svc.reset("/dev/ttyUSB0", None, "both").unwrap_err(),
        SikFailure::Sik("target must be 'local' or 'remote'".into())
    );

    let slow = {
        let svc = Arc::clone(&svc);
        thread::spawn(move || svc.load("/dev/ttyUSB0", Some(57600), true))
    };
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while !svc.status()["busy"].as_bool().unwrap() && std::time::Instant::now() < end {
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(svc.status()["blocked_reason"], BUSY_MESSAGE);
    assert_eq!(
        svc.load("/dev/ttyUSB0", None, true).unwrap_err(),
        SikFailure::Sik(BUSY_MESSAGE.into())
    );
    slow.join().unwrap().unwrap();
    assert_eq!(svc.status()["busy"], false);
}

#[test]
fn a_factory_reset_runs_the_three_commands_on_the_chosen_end() {
    let w = world(57600, true, None);
    let r = service(&w, &Arc::default())
        .reset("/dev/ttyUSB0", Some(57600), "remote")
        .unwrap();
    assert_eq!(r["target"], "remote");
    let sent = w.sent.lock().unwrap().clone();
    assert!(sent.windows(3).any(|s| s == ["RT&F", "RT&W", "RTZ"]), "{sent:?}");
    let w = world(57600, false, None);
    let e = service(&w, &Arc::default())
        .reset("/dev/ttyUSB0", Some(57600), "remote")
        .unwrap_err();
    assert_eq!(e, SikFailure::Sik("the remote radio is not answering".into()));
}

#[test]
fn the_status_lists_every_port_with_its_kind_and_the_links_own() {
    let w = world(57600, true, None);
    let host = Arc::new(Host::default());
    *host.conn.lock().unwrap() = "serial:/dev/ttyUSB0:115200".into();
    let s = service(&w, &host).status();
    assert_eq!(
        (s["link_device"].clone(), s["link_baud"].clone(), s["transport"].clone()),
        (json!("/dev/ttyUSB0"), json!(115200), json!("sik"))
    );
    assert_eq!(
        s["ports"][0],
        json!({"device": "/dev/ttyUSB0", "description": "FT230X", "kind": "sik", "is_link": true})
    );
    assert_eq!(s["ports"][1]["kind"], "usb");
    assert_eq!(s["ports"][1]["is_link"], false);
    *host.conn.lock().unwrap() = "udp:127.0.0.1:14550".into();
    assert_eq!(service(&w, &host).status()["transport"], "udp");
    *host.conn.lock().unwrap() = "serial:/dev/cu.usbserial-1:57600".into();
    assert_eq!(service(&w, &host).status()["transport"], "unknown");
}
