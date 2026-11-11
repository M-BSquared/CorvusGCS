//! The bootloader uploader and the flash service against a scripted bootloader,
//! held to what the Python of `tests/reference` does with the same fakes
//! (`python/tools/dump_logs_cases.py flash`, from `tests/test_flash_parity.py`).

mod logs_util;

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use corvus_core::firmware::flash::{DownloadError, FlashHost, FlashService, ImageSource};
use corvus_core::firmware::uploader::{bl_crc32, FirmwareUploader, Progress, SerialIo, UploaderEnv};
use corvus_core::protocol::common::Event;
use corvus_core::serial_ports::PortRow;
use logs_util::{diff, load};
use serde_json::{json, Value};

const INSYNC: u8 = 0x12;
const OK: u8 = 0x10;
const FAILED: u8 = 0x11;
const INVALID: u8 = 0x13;
const BAD_SILICON: u8 = 0x14;
const GET_SYNC: u8 = 0x21;
const GET_DEVICE: u8 = 0x22;
const CHIP_ERASE: u8 = 0x23;
const PROG_MULTI: u8 = 0x27;
const GET_CRC: u8 = 0x29;
const GET_CHIP: u8 = 0x2c;
const REBOOT: u8 = 0x30;

fn cases() -> Value {
    load("flash_cases.json")
}

fn same(what: &str, want: &Value, got: &Value) {
    let d = diff(want, got, 0.0);
    assert!(
        d.is_empty(),
        "{what}:\n  {}",
        d.iter().take(8).cloned().collect::<Vec<_>>().join("\n  ")
    );
}

// ------------------------------------------------------------ the bootloader

struct Options {
    fw_maxsize: i64,
    board_id: Option<i64>,
    sync: String,
    info: String,
    chip: bool,
    erase: String,
    prog: String,
    prog_at: u32,
    crc: String,
    reboot: String,
    write_error: Option<u8>,
    write_error_at: i64,
    cancel_at: String,
    read_error_after: Option<u32>,
}

impl Options {
    fn from(v: &Value) -> Options {
        let text = |k: &str, d: &str| v.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        Options {
            fw_maxsize: v.get("fw_maxsize").and_then(Value::as_i64).unwrap_or(1 << 20),
            board_id: match v.get("board_id") {
                Some(Value::Null) => None,
                Some(x) => x.as_i64(),
                None => Some(9),
            },
            sync: text("sync", "ok"),
            info: text("info", "ok"),
            chip: v.get("chip").and_then(Value::as_bool).unwrap_or(true),
            erase: text("erase", "ok"),
            prog: text("prog", "ok"),
            prog_at: v.get("prog_at").and_then(Value::as_u64).unwrap_or(1) as u32,
            crc: text("crc", "ok"),
            reboot: text("reboot", "ok"),
            write_error: v.get("write_error").and_then(Value::as_u64).map(|c| c as u8),
            write_error_at: v.get("write_error_at").and_then(Value::as_i64).unwrap_or(0),
            cancel_at: text("cancel_at", ""),
            read_error_after: v.get("read_error_after").and_then(Value::as_u64).map(|n| n as u32),
        }
    }
}

#[derive(Default)]
struct BootState {
    rbuf: VecDeque<u8>,
    programmed: Vec<u8>,
    writes: Vec<Vec<u8>>,
    block: u32,
    closed: bool,
    reads: u32,
    rebooted: bool,
    erased: u32,
    write_error_at: i64,
}

struct Bootloader {
    /// A real port waits out its read timeout when it has nothing; a spinning
    /// reader would starve the test thread that watches the fake.
    nap: AtomicBool,
    opts: Options,
    cancel: Option<Arc<Event>>,
    st: Mutex<BootState>,
}

impl Bootloader {
    fn new(opts: Options, cancel: Option<Arc<Event>>) -> Arc<Bootloader> {
        let st = BootState {
            write_error_at: opts.write_error_at,
            ..BootState::default()
        };
        Arc::new(Bootloader {
            nap: AtomicBool::new(false),
            opts,
            cancel,
            st: Mutex::new(st),
        })
    }

    fn st(&self) -> std::sync::MutexGuard<'_, BootState> {
        self.st.lock().unwrap()
    }

    fn summary(&self) -> Value {
        let st = self.st();
        Value::Array(
            st.writes
                .iter()
                .map(|w| {
                    let mut crc = flate2::Crc::new();
                    crc.update(w);
                    json!([w.first(), w.len(), crc.sum()])
                })
                .collect(),
        )
    }
}

fn reply(st: &mut BootState, chunks: &[&[u8]]) {
    for c in chunks {
        st.rbuf.extend(c.iter().copied());
    }
}

impl SerialIo for Bootloader {
    fn read(&self, n: usize) -> io::Result<Vec<u8>> {
        let mut st = self.st();
        st.reads += 1;
        if self.opts.read_error_after.is_some_and(|a| st.reads > a) {
            return Err(io::Error::from_raw_os_error(5));
        }
        let take = n.min(st.rbuf.len());
        let out: Vec<u8> = st.rbuf.drain(..take).collect();
        drop(st);
        if out.is_empty() && self.nap.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(out)
    }

    fn write(&self, data: &[u8]) -> io::Result<()> {
        let o = &self.opts;
        let mut st = self.st();
        st.writes.push(data.to_vec());
        let cmd = data[0];
        if o.write_error == Some(cmd) {
            st.write_error_at -= 1;
            if st.write_error_at < 0 {
                return Err(io::Error::from_raw_os_error(5));
            }
        }
        let mapped = match cmd {
            GET_SYNC => "sync",
            CHIP_ERASE => "erase",
            GET_CRC => "crc",
            REBOOT => "reboot",
            _ => "\u{0}",
        };
        if let Some(c) = &self.cancel {
            if o.cancel_at == mapped {
                c.set();
            }
        }
        match cmd {
            GET_SYNC => match o.sync.as_str() {
                "ok" => reply(&mut st, &[&[INSYNC, OK]]),
                "bad-silicon" => reply(&mut st, &[&[INSYNC, BAD_SILICON]]),
                "wrong" => reply(&mut st, &[&[0x14, OK]]),
                "failed" => reply(&mut st, &[&[INSYNC, FAILED]]),
                _ => {}
            },
            GET_DEVICE => {
                let arg = data[1];
                if o.info == "silent" {
                    return Ok(());
                }
                if o.info == "invalid" || (arg == 0x02 && o.board_id.is_none()) {
                    reply(&mut st, &[&[INSYNC, INVALID]]);
                } else if o.info == "short" {
                    reply(&mut st, &[&[1, 2]]);
                } else {
                    let value = match arg {
                        0x02 => o.board_id.unwrap_or(0),
                        0x04 => o.fw_maxsize,
                        _ => 0,
                    };
                    reply(&mut st, &[&(value as u32).to_le_bytes(), &[INSYNC, OK]]);
                }
            }
            GET_CHIP => {
                if o.chip {
                    reply(&mut st, &[&0x1234_5678u32.to_le_bytes(), &[INSYNC, OK]]);
                }
            }
            CHIP_ERASE => {
                st.erased += 1;
                match o.erase.as_str() {
                    "ok" => reply(&mut st, &[&[INSYNC, OK]]),
                    "fail" => reply(&mut st, &[&[INSYNC, FAILED]]),
                    _ => {}
                }
            }
            PROG_MULTI => {
                let count = data[1] as usize;
                st.block += 1;
                if o.prog != "silent" || st.block != o.prog_at {
                    let end = (2 + count).min(data.len());
                    st.programmed.extend_from_slice(&data[2..end]);
                }
                if st.block == o.prog_at && o.prog == "fail" {
                    reply(&mut st, &[&[INSYNC, FAILED]]);
                } else if st.block == o.prog_at && o.prog == "silent" {
                } else {
                    reply(&mut st, &[&[INSYNC, OK]]);
                }
                if let Some(c) = &self.cancel {
                    if o.cancel_at == format!("block{}", st.block) {
                        c.set();
                    }
                }
            }
            GET_CRC => {
                let image = st.programmed.clone();
                let mut state = bl_crc32(&image, 0);
                if o.fw_maxsize > image.len() as i64 {
                    state = bl_crc32(&vec![0xFFu8; (o.fw_maxsize - image.len() as i64) as usize], state);
                }
                match o.crc.as_str() {
                    "ok" => reply(&mut st, &[&state.to_le_bytes(), &[INSYNC, OK]]),
                    "bad" => reply(&mut st, &[&state.wrapping_add(1).to_le_bytes(), &[INSYNC, OK]]),
                    "no-insync" => reply(&mut st, &[&state.to_le_bytes()]),
                    "failed" => reply(&mut st, &[&state.to_le_bytes(), &[INSYNC, FAILED]]),
                    _ => {}
                }
            }
            REBOOT => {
                st.rebooted = true;
                if o.reboot == "ok" {
                    reply(&mut st, &[&[INSYNC, OK]]);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        Ok(())
    }

    fn reset_input(&self) -> io::Result<()> {
        self.st().rbuf.clear();
        Ok(())
    }

    fn close(&self) {
        self.st().closed = true;
    }
}

/// The machine around it: a fake clock (or the real one), the log, and a port
/// that fails to open as often as it is told to.
struct Env {
    fake: Arc<Bootloader>,
    clock: Option<Mutex<f64>>,
    present: bool,
    failures: usize,
    opens: Mutex<Vec<Value>>,
    logs: Mutex<Vec<Value>>,
}

impl Env {
    fn new(fake: &Arc<Bootloader>, fake_clock: bool, present: bool, failures: usize) -> Arc<Env> {
        Arc::new(Env {
            fake: Arc::clone(fake),
            clock: fake_clock.then(|| Mutex::new(1000.0)),
            present,
            failures,
            opens: Mutex::default(),
            logs: Mutex::default(),
        })
    }

    fn time(&self) -> f64 {
        *self.clock.as_ref().unwrap().lock().unwrap()
    }
}

impl UploaderEnv for Env {
    fn monotonic(&self) -> f64 {
        match &self.clock {
            Some(c) => {
                let mut t = c.lock().unwrap();
                *t += 0.01;
                *t
            }
            None => corvus_core::clock::monotonic(),
        }
    }

    fn sleep(&self, seconds: f64) {
        match &self.clock {
            Some(c) => *c.lock().unwrap() += seconds,
            None => std::thread::sleep(Duration::from_secs_f64(seconds)),
        }
    }

    fn device_present(&self, _device: &str) -> bool {
        self.present
    }

    fn open(&self, device: &str, _baud: u32, _timeout: f64) -> io::Result<Arc<dyn SerialIo>> {
        let mut opens = self.opens.lock().unwrap();
        opens.push(json!([device, ["baudrate", "timeout"]]));
        if opens.len() <= self.failures {
            return Err(io::Error::from_raw_os_error(16));
        }
        self.fake.st().closed = false;
        Ok(self.fake.clone())
    }

    fn log(&self, level: &str, message: &str) {
        self.logs.lock().unwrap().push(json!([level, message, false]));
    }

    fn log_exc(&self, level: &str, message: &str, _detail: &str) {
        self.logs.lock().unwrap().push(json!([level, message, true]));
    }
}

fn progress_json(list: &[Progress]) -> Value {
    Value::Array(
        list.iter()
            .map(|p| json!({"state": p.state, "percent": p.percent, "message": p.message}))
            .collect(),
    )
}

fn status_json(p: &Progress) -> Value {
    json!({"state": p.state, "percent": p.percent, "message": p.message})
}

fn run_uploader(options: &Value, size: usize, board: Option<i64>) -> (Value, Arc<Env>) {
    let cancel = Arc::new(Event::new());
    let fake = Bootloader::new(Options::from(options), Some(Arc::clone(&cancel)));
    let env = Env::new(&fake, true, true, 0);
    let seen: Arc<Mutex<Vec<Progress>>> = Arc::default();
    let sink = Arc::clone(&seen);
    let up = FirmwareUploader::new(
        env.clone(),
        Some(Arc::new(move |p: &Progress| sink.lock().unwrap().push(p.clone()))),
        Some(cancel),
    );
    let image: Vec<u8> = (0..size).map(|i| ((i * 7 + 1) & 0xFF) as u8).collect();
    let result = up.run("/dev/ttyACM0", &image, board);
    let progress = progress_json(&seen.lock().unwrap());
    let st = fake.st();
    let got = json!({
        "result": result, "progress": progress, "status": status_json(&up.status()), "writes": Value::Null,
        "closed": st.closed, "rebooted": st.rebooted, "serial_open": up.is_open(), "fw_maxsize": up.fw_maxsize(),
        "board_id": up.board_id(), "opens": env.opens.lock().unwrap().clone(), "logs": env.logs.lock().unwrap().clone(),
        "clock": (env.time() * 1e6).round() / 1e6,
    });
    drop(st);
    let mut got = got;
    got["writes"] = fake.summary();
    (got, env)
}

#[test]
fn the_bootloader_crc_is_the_reflected_crc32_without_the_final_xor() {
    for case in load("firmware_cases.json")["crc"].as_array().unwrap() {
        let data = base64::engine::general_purpose::STANDARD
            .decode(case["data"].as_str().unwrap())
            .unwrap();
        let state = case["state"].as_u64().unwrap() as u32;
        assert_eq!(case["crc"].as_u64().unwrap() as u32, bl_crc32(&data, state));
    }
    assert_eq!(bl_crc32(b"123456789", 0), 0x2DFD_2D88);
    assert_eq!(!bl_crc32(b"123456789", 0xFFFF_FFFF), 0xCBF4_3926);
}

#[test]
fn every_upload_ends_as_python_ends_it() {
    let c = cases();
    let options: std::collections::HashMap<&str, &Value> = c["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["name"].as_str().unwrap(), &x["options"]))
        .collect();
    for case in c["uploads"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let (got, _) = run_uploader(
            options[name],
            case["size"].as_u64().unwrap() as usize,
            case["board"].as_i64(),
        );
        let mut want = case.clone();
        for k in ["name", "size", "board"] {
            want.as_object_mut().unwrap().remove(k);
        }
        same(
            &format!("{name} size {} board {}", case["size"], case["board"]),
            &want,
            &got,
        );
    }
}

#[test]
fn opening_the_device_goes_as_python_goes() {
    for case in cases()["opening"].as_array().unwrap() {
        let fake = Bootloader::new(Options::from(&json!({})), None);
        let env = Env::new(
            &fake,
            true,
            case["present"].as_bool().unwrap(),
            case["failures"].as_u64().unwrap() as usize,
        );
        let seen: Arc<Mutex<Vec<Progress>>> = Arc::default();
        let sink = Arc::clone(&seen);
        let up = FirmwareUploader::with_options(
            env.clone(),
            Some(Arc::new(move |p: &Progress| sink.lock().unwrap().push(p.clone()))),
            None,
            115_200,
            2.0,
        );
        let result = up.run("/dev/ttyACM0", &[1u8; 40], None);
        let got = json!({
            "failures": case["failures"], "present": case["present"], "result": result,
            "progress": progress_json(&seen.lock().unwrap()), "status": status_json(&up.status()),
            "opens": env.opens.lock().unwrap().clone(), "writes": fake.summary(), "logs": env.logs.lock().unwrap().clone(),
            "clock": (env.time() * 1e6).round() / 1e6,
        });
        same(
            &format!("failures {} present {}", case["failures"], case["present"]),
            case,
            &got,
        );
    }
}

#[test]
fn a_bad_image_is_refused_before_anything_opens() {
    for case in cases()["bad_inputs"].as_array().unwrap() {
        let fake = Bootloader::new(Options::from(&json!({})), None);
        let env = Env::new(&fake, true, true, 0);
        let up = FirmwareUploader::new(env.clone(), None, None);
        let image = if case["name"] == "empty" {
            Vec::new()
        } else {
            vec![b'x'; corvus_core::firmware::image::MAX_FIRMWARE_IMAGE_BYTES + 1]
        };
        let got = json!({"name": case["name"], "result": up.run("/dev/ttyACM0", &image, None), "status": status_json(&up.status())});
        same(case["name"].as_str().unwrap(), case, &got);
        assert!(env.opens.lock().unwrap().is_empty());
    }
}

#[test]
fn the_cases_reach_every_ending() {
    let mut states = std::collections::BTreeSet::new();
    for case in cases()["cases"].as_array().unwrap() {
        let (got, _) = run_uploader(&case["options"], 600, Some(9));
        states.insert(got["status"]["state"].as_str().unwrap().to_string());
    }
    assert!(
        ["done", "failed", "cancelled"].iter().all(|s| states.contains(*s)),
        "{states:?}"
    );
}

#[test]
fn a_shutdown_closes_the_port_that_is_open() {
    let fake = Bootloader::new(Options::from(&json!({"erase": "silent"})), None);
    fake.nap.store(true, Ordering::Relaxed);
    let env = Env::new(&fake, false, true, 0);
    let up = Arc::new(FirmwareUploader::new(env, None, None));
    let runner = {
        let up = Arc::clone(&up);
        std::thread::spawn(move || up.run("/dev/ttyACM0", &[1u8; 40], Some(9)))
    };
    let limit = Instant::now() + Duration::from_secs(8);
    while fake.st().erased == 0 && Instant::now() < limit {
        std::thread::sleep(Duration::from_millis(5));
    }
    up.shutdown();
    assert!(!runner.join().unwrap());
    assert!(fake.st().closed);
    assert!(!up.is_open());
}

// ------------------------------------------------------------------ service

struct World {
    connected: AtomicBool,
    armed: AtomicBool,
}

struct Mavlink {
    world: Arc<World>,
    usb: bool,
    reboot: bool,
    conn: String,
    device: String,
    rows: Vec<PortRow>,
    raises: String,
    events: Mutex<Vec<Value>>,
}

impl FlashHost for Mavlink {
    fn transport(&self) -> String {
        if self.usb { "usb" } else { "sik" }.into()
    }

    fn is_direct_usb(&self) -> bool {
        self.usb
    }

    fn connection_string(&self) -> String {
        self.conn.clone()
    }

    fn serial_device(&self) -> String {
        self.device.clone()
    }

    fn reboot_to_bootloader(&self) -> bool {
        self.events.lock().unwrap().push(json!("reboot"));
        self.reboot
    }

    fn stop(&self) -> Result<(), String> {
        self.events.lock().unwrap().push(json!("stop"));
        if self.raises == "stop" {
            Err("already down".into())
        } else {
            Ok(())
        }
    }

    fn set_connection(&self, conn: &str) -> Result<(), String> {
        self.events.lock().unwrap().push(json!(["set", conn]));
        if self.raises == "restart" {
            Err("cannot restart".into())
        } else {
            Ok(())
        }
    }

    fn start(&self) -> Result<(), String> {
        self.events.lock().unwrap().push(json!("start"));
        Ok(())
    }

    fn list_serial_ports(&self) -> Result<Vec<PortRow>, String> {
        if self.raises == "list" {
            Err("no ports".into())
        } else {
            Ok(self.rows.clone())
        }
    }

    fn connected(&self) -> bool {
        self.world.connected.load(Ordering::SeqCst)
    }

    fn armed(&self) -> bool {
        self.world.armed.load(Ordering::SeqCst)
    }
}

struct Catalog {
    world: Arc<World>,
    known: bool,
    cached: bool,
    fail: String,
    image: Vec<u8>,
    calls: Mutex<Vec<Value>>,
}

impl ImageSource for Catalog {
    fn resolve(&self, tag: &str, board: &str) -> Option<Value> {
        self.calls.lock().unwrap().push(json!(["resolve", tag, board]));
        self.known.then(|| json!({"name": format!("{board}.px4"), "tag": tag}))
    }

    fn cached_path(&self, _name: &str) -> Option<String> {
        self.cached.then(|| "/cache/x".to_string())
    }

    fn download(
        &self,
        entry: &Value,
        on_progress: Option<&dyn Fn(i64, i64)>,
        cancel: &Event,
    ) -> Result<Vec<u8>, DownloadError> {
        self.calls
            .lock()
            .unwrap()
            .push(json!(["download", entry["name"], on_progress.is_some()]));
        if let Some(report) = on_progress {
            for done in [0, 1500, 3000, 4096] {
                report(done, 4096);
            }
            report(10, 0);
        }
        match self.fail.as_str() {
            "value" => return Err(DownloadError::Value("bad release file".into())),
            "crash" => return Err(DownloadError::Crash("network on fire".into())),
            "cancel" => cancel.set(),
            "arm" => self.world.armed.store(true, Ordering::SeqCst),
            "unplug" => self.world.connected.store(true, Ordering::SeqCst),
            _ => {}
        }
        Ok(self.image.clone())
    }
}

fn unhex(text: &str) -> Vec<u8> {
    logs_util::unhex(text)
}

fn without_start_notice(events: &[Value], call: &str) -> Vec<Value> {
    let release = call.starts_with("release");
    let mut out = events.to_vec();
    for i in 0..out.len() {
        let message = out[i]["message"].as_str().unwrap_or("");
        let hit = if release {
            message.starts_with("Downloading ")
                && message.ends_with('\u{2026}')
                && out[i]["percent"] == 0
                && out[i]["state"] == "downloading"
        } else {
            message == "Preparing to flash\u{2026}" && out[i]["state"] == "flashing"
        };
        if hit {
            out.remove(i);
            break;
        }
    }
    out
}

fn wait_for(f: impl Fn() -> bool) -> bool {
    let limit = Instant::now() + Duration::from_secs(8);
    while Instant::now() < limit {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn the_service_does_what_python_does() {
    let c = cases();
    let rows = &c["port_rows"];
    let px4_image = unhex(c["px4_image"].as_str().unwrap());
    let defs = c["service_cases"].as_array().unwrap();
    for (def, want) in defs.iter().zip(c["services"].as_array().unwrap()) {
        let name = def["name"].as_str().unwrap();
        eprintln!("case {name}");
        let call = def["call"].as_str().unwrap();
        let mav = &def["mavlink"];
        let store = &def["store"];
        let world = Arc::new(World {
            connected: AtomicBool::new(store.get("connected").and_then(Value::as_bool).unwrap_or(true)),
            armed: AtomicBool::new(store.get("armed").and_then(Value::as_bool).unwrap_or(false)),
        });
        let port_rows: Vec<PortRow> = rows[def["ports"].as_str().unwrap()]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| PortRow {
                device: r["device"].as_str().unwrap().into(),
                description: r["description"].as_str().unwrap().into(),
                hwid: r["hwid"].as_str().unwrap().into(),
            })
            .collect();
        let host = Arc::new(Mavlink {
            world: Arc::clone(&world),
            usb: mav.get("usb").and_then(Value::as_bool).unwrap_or(true),
            reboot: mav.get("reboot").and_then(Value::as_bool).unwrap_or(true),
            conn: mav
                .get("conn")
                .and_then(Value::as_str)
                .unwrap_or("serial:/dev/ttyACM0:115200")
                .into(),
            device: mav
                .get("device")
                .and_then(Value::as_str)
                .unwrap_or("/dev/ttyACM0")
                .into(),
            rows: port_rows,
            raises: mav.get("raises").and_then(Value::as_str).unwrap_or("").into(),
            events: Mutex::default(),
        });
        let fake = Bootloader::new(Options::from(&def["bootloader"]), None);
        fake.nap.store(true, Ordering::Relaxed);
        let env = Env::new(&fake, false, true, 0);
        let cat = &def["catalog"];
        let catalog = (call != "release-none").then(|| {
            Arc::new(Catalog {
                world: Arc::clone(&world),
                known: cat.get("known").and_then(Value::as_bool).unwrap_or(true),
                cached: cat.get("cached").and_then(Value::as_bool).unwrap_or(false),
                fail: cat.get("fail").and_then(Value::as_str).unwrap_or("").into(),
                image: match cat.get("image").and_then(Value::as_str) {
                    Some(h) => unhex(h),
                    None => (0..=255u8).cycle().take(768).collect(),
                },
                calls: Mutex::default(),
            })
        });
        let svc = FlashService::new(
            host.clone(),
            catalog.clone().map(|c| c as Arc<dyn ImageSource>),
            env.clone(),
        );
        let events: Arc<Mutex<Vec<Value>>> = Arc::default();
        let sink = Arc::clone(&events);
        svc.add_listener(move |e| sink.lock().unwrap().push(e.clone()));
        let (mut first, mut second) = (Value::Null, Value::Null);
        match call {
            "upload" | "busy" => {
                first = json!(svc.start(&px4_image));
                if call == "busy" {
                    let a = svc.start(&px4_image);
                    let e1 = svc.last_error();
                    let b = svc.start_release("v1", "fmu-v6x");
                    let e2 = svc.last_error();
                    second = json!([a, e1, b, e2]);
                }
            }
            "upload-bad" => first = json!(svc.start(b"{not json")),
            c if c.starts_with("release") => first = json!(svc.start_release("v1.16.0", "fmu-v6x")),
            "cancel-idle" => first = json!(svc.cancel()),
            "cancel-erase" => {
                first = json!(svc.start(&px4_image));
                assert!(wait_for(|| fake.st().erased > 0), "{name}");
                second = json!(svc.cancel());
            }
            "shutdown-erase" => {
                first = json!(svc.start(&px4_image));
                assert!(wait_for(|| fake.st().erased > 0), "{name}");
                svc.shutdown();
            }
            "shutdown-idle" | "after-shutdown" => {
                svc.shutdown();
                svc.shutdown();
                if call == "after-shutdown" {
                    first = json!(svc.start(&px4_image));
                }
            }
            other => panic!("{other}"),
        }
        if let Some(worker) = svc.take_worker() {
            worker.join().unwrap();
        }
        svc.shutdown();
        let status = svc.status();
        let (worker_gone, uploader_gone) = svc.is_settled();
        let st = fake.st();
        let got = json!({
            "name": name, "call": call, "first": first, "second": second, "last_error": svc.last_error(), "status": status,
            "events": without_start_notice(&events.lock().unwrap(), call), "mavlink": host.events.lock().unwrap().clone(),
            "writes": Value::Null, "closed": st.closed, "rebooted": st.rebooted,
            "opens": env.opens.lock().unwrap().clone(), "logs": env.logs.lock().unwrap().clone(),
            "worker_gone": worker_gone, "uploader_gone": uploader_gone,
            "catalog_calls": catalog.as_ref().map_or(json!([]), |c| Value::Array(c.calls.lock().unwrap().clone())),
        });
        drop(st);
        let mut got = got;
        got["writes"] = fake.summary();
        same(name, want, &got);
    }
}
