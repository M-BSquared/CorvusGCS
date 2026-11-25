//! A PX4 on a UDP port, simulated well enough to photograph: heartbeat and
//! telemetry out, and answers to the parameter, command, mission, fence, log,
//! FTP, shell and calibration protocols. Corvus reads all of it with the code
//! a flight uses. It is no flight dynamics and no SITL.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use corvus_link::plain::connection::{open, Connection, Options as LinkOptions};
use corvus_link::plain::defs::Wire;
use corvus_link::plain::{Message, Value as Wv};
use serde_json::{Map, Value};

use crate::data;
use crate::flight::Model;

/// Neubiberg stands at about this height.
const GROUND_AMSL: f64 = 570.0;
const PREARM: i64 = 0x1000_0000;
/// Gyro, accelerometer, compass, baro, GPS, the controllers, motors, receiver and the arming check.
const SENSORS: i64 =
    0x1 | 0x2 | 0x4 | 0x8 | 0x20 | 0x400 | 0x800 | 0x1000 | 0x2000 | 0x4000 | 0x8000 | 0x10000 | PREARM;
const SIDES: [&str; 6] = ["down", "front", "left", "right", "up", "back"];
const CALIBRATION_STEP_S: f64 = 2.2;
const ACCEPTED: i64 = 0;
const BUSY: i64 = 1;
const DENIED: i64 = 2;
const UNSUPPORTED: i64 = 3;

pub struct Options {
    pub params: Map<String, Value>,
    pub model: Model,
    pub armed: bool,
    pub mode: String,
    pub rover: bool,
    pub transmitter: Value,
    /// `(id, unix time, size, the file)`.
    pub logs: Vec<(i64, i64, usize, Option<Vec<u8>>)>,
    /// `(second, severity, text)`.
    pub chatter: Vec<(f64, i64, String)>,
    /// A calibration stops and waits after this many sides.
    pub pause_after: Option<usize>,
}

pub struct Vehicle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Vehicle {
    /// Flies at `127.0.0.1:port`, from now on.
    pub fn start(port: u16, options: Options) -> Result<Vehicle, String> {
        let link = LinkOptions {
            source_system: 1,
            source_component: 1,
            input: false,
            ..LinkOptions::default()
        };
        let conn = open(&format!("udpout:127.0.0.1:{port}"), &link).map_err(|e| format!("{e:?}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || Sim::new(conn, options).run(&flag));
        Ok(Vehicle {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Vehicle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A calibration as the lines PX4 says, each at its time.
#[derive(Default)]
struct Calibration {
    lines: VecDeque<(f64, String)>,
    /// It waits for the operator for as long as the picture takes.
    held: bool,
}

/// How often a stream is sent, when it is next, and what sends it.
type Due = (f64, f64, fn(&mut Sim));

struct Sim {
    conn: Connection,
    o: Options,
    names: Vec<String>,
    began: Instant,
    /// Who spoke last: the station that is answered.
    gcs: (u8, u8),
    due: Vec<Due>,
    chatter: usize,
    noise: u64,
    /// Parameters still to send, from the front.
    burst: VecDeque<usize>,
    out: VecDeque<Message>,
    calibration: Option<Calibration>,
    fence_count: i64,
    upload: i64,
    items: Vec<Vec<(String, Wv)>>,
    shell: Option<String>,
    file: Option<Vec<u8>>,
}

fn int(v: i64) -> Wv {
    Wv::Int(i128::from(v))
}

fn float(v: f64) -> Wv {
    Wv::Float(v)
}

fn msg(name: &str) -> Message {
    Message::named(name, Wire::V2).unwrap_or_else(|| panic!("no message {name}"))
}

fn unix_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}

/// PX4's custom mode: the main mode in bits 16 to 23, the sub mode above it.
fn custom_mode(mode: &str) -> i64 {
    let (main, sub) = match mode {
        "MANUAL" => (1, 0),
        "ALTCTL" => (2, 0),
        "ACRO" => (5, 0),
        "OFFBOARD" => (6, 0),
        "STABILIZED" => (7, 0),
        "TAKEOFF" => (4, 2),
        "HOLD" | "LOITER" => (4, 3),
        "MISSION" => (4, 4),
        "RTL" => (4, 5),
        "LAND" => (4, 6),
        _ => (3, 0),
    };
    main << 16 | sub << 24
}

fn mode_of(main: i64, sub: i64) -> String {
    [
        "MANUAL",
        "ALTCTL",
        "ACRO",
        "OFFBOARD",
        "STABILIZED",
        "TAKEOFF",
        "HOLD",
        "MISSION",
        "RTL",
        "LAND",
    ]
    .iter()
    .find(|m| custom_mode(m) == (main << 16 | sub << 24))
    .map_or_else(|| "POSCTL".to_string(), |m| m.to_string())
}

impl Sim {
    fn new(conn: Connection, o: Options) -> Sim {
        Sim {
            conn,
            names: o.params.keys().cloned().collect(),
            o,
            began: Instant::now(),
            gcs: (255, 190),
            due: [
                (1.0, Sim::heartbeat as fn(&mut Sim)),
                (10.0, Sim::position),
                (20.0, Sim::attitude),
                (10.0, Sim::hud),
                (5.0, Sim::gps),
                (2.0, Sim::status),
                (2.0, Sim::battery),
                (2.0, Sim::landed),
                (10.0, Sim::channels),
                (2.0, Sim::vibration),
                (1.0, Sim::time),
                (0.2, Sim::home),
                (1.0, Sim::mission_current),
            ]
            .into_iter()
            .map(|(hz, f)| (1.0 / hz, 0.0, f))
            .collect(),
            chatter: 0,
            noise: 20_260_916,
            burst: VecDeque::new(),
            out: VecDeque::new(),
            calibration: None,
            fence_count: 0,
            upload: 0,
            items: Vec::new(),
            shell: None,
            file: None,
        }
    }

    fn t(&self) -> f64 {
        self.began.elapsed().as_secs_f64()
    }

    fn ms(&self) -> Wv {
        int((self.t() * 1000.0) as i64)
    }

    /// From 0 to 1, the same at every run.
    fn random(&mut self) -> f64 {
        self.noise = self
            .noise
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.noise >> 33) as f64 / (1u64 << 31) as f64
    }

    fn gauss(&mut self, sigma: f64) -> f64 {
        ((0..6).map(|_| self.random()).sum::<f64>() - 3.0) * sigma * 1.4
    }

    fn send(&self, m: &Message) {
        let _ = self.conn.send(m);
    }

    fn say(&self, severity: i64, text: &str) {
        let mut t = text.as_bytes().to_vec();
        t.truncate(50);
        t.resize(50, 0);
        self.send(
            &msg("STATUSTEXT")
                .with("severity", int(severity))
                .with("text", Wv::Bytes(t)),
        );
    }

    fn run(&mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::SeqCst) {
            let now = self.t();
            for i in 0..self.due.len() {
                let (every, at, f) = self.due[i];
                if now >= at {
                    self.due[i].1 = now + every;
                    f(self);
                }
            }
            while let Some((at, severity, text)) = self.o.chatter.get(self.chatter).cloned() {
                if at > now {
                    break;
                }
                self.say(severity, &text);
                self.chatter += 1;
            }
            self.calibrate(now);
            // A whole table at once would overrun the socket, and a bar that fills is worth seeing.
            for _ in 0..12 {
                match self.burst.pop_front() {
                    Some(i) => self.param(i),
                    None => break,
                }
            }
            for _ in 0..40 {
                match self.out.pop_front() {
                    Some(m) => self.send(&m),
                    None => break,
                }
            }
            match self.conn.recv(Duration::from_millis(8)) {
                Ok(frames) => {
                    for r in frames {
                        self.gcs = (r.frame.sysid, r.frame.compid);
                        if let Ok(m) = r.message {
                            self.handle(&m);
                        }
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    fn to_gcs(&self, m: Message) -> Message {
        m.with("target_system", int(i64::from(self.gcs.0)))
            .with("target_component", int(i64::from(self.gcs.1)))
    }

    // ---- telemetry --------------------------------------------------------

    fn heartbeat(&mut self) {
        let armed = if self.o.armed { 128 } else { 0 };
        self.send(
            &msg("HEARTBEAT")
                .with("type", int(if self.o.rover { 10 } else { 2 }))
                .with("autopilot", int(12))
                .with("base_mode", int(1 | 16 | 8 | 4 | armed))
                .with("custom_mode", int(custom_mode(&self.o.mode)))
                .with("system_status", int(if self.o.armed { 4 } else { 3 }))
                .with("mavlink_version", int(3)),
        );
    }

    fn position(&mut self) {
        let f = self.o.model.sample(self.t());
        self.send(
            &msg("GLOBAL_POSITION_INT")
                .with("time_boot_ms", self.ms())
                .with("lat", int((f.lat * 1e7) as i64))
                .with("lon", int((f.lon * 1e7) as i64))
                .with("alt", int(((f.alt_agl + GROUND_AMSL) * 1000.0) as i64))
                .with("relative_alt", int((f.alt_agl * 1000.0) as i64))
                .with("vx", int((f.vx * 100.0) as i64))
                .with("vy", int((f.vy * 100.0) as i64))
                .with("vz", int((-f.climb * 100.0) as i64))
                .with("hdg", int((f.heading * 100.0) as i64 % 36000)),
        );
    }

    fn attitude(&mut self) {
        let t = self.t();
        let (a, b) = (self.o.model.sample(t), self.o.model.sample(t + 0.1));
        let wrap = |d: f64| (d + 180.0).rem_euclid(360.0) - 180.0;
        let rates = [
            (b.roll - a.roll) * 10.0,
            (b.pitch - a.pitch) * 10.0,
            wrap(b.heading - a.heading).to_radians() * 10.0,
        ];
        let rates = rates.map(|r| r + self.gauss(0.004));
        self.send(
            &msg("ATTITUDE")
                .with("time_boot_ms", self.ms())
                .with("roll", float(a.roll))
                .with("pitch", float(a.pitch))
                .with("yaw", float(wrap(a.heading).to_radians()))
                .with("rollspeed", float(rates[0]))
                .with("pitchspeed", float(rates[1]))
                .with("yawspeed", float(rates[2])),
        );
    }

    fn hud(&mut self) {
        let f = self.o.model.sample(self.t());
        self.send(
            &msg("VFR_HUD")
                .with("airspeed", float(f.ground_speed * 1.04))
                .with("groundspeed", float(f.ground_speed))
                .with("heading", int(f.heading as i64 % 360))
                .with(
                    "throttle",
                    int((42.0 + f.ground_speed * 2.0 + f.climb * 6.0).clamp(0.0, 100.0) as i64),
                )
                .with("alt", float(f.alt_agl + GROUND_AMSL))
                .with("climb", float(f.climb)),
        );
    }

    fn gps(&mut self) {
        let f = self.o.model.sample(self.t());
        let (eph, epv) = (60.0 + self.random() * 20.0, 80.0 + self.random() * 30.0);
        self.send(
            &msg("GPS_RAW_INT")
                .with("time_usec", int(unix_us()))
                .with("fix_type", int(6))
                .with("lat", int((f.lat * 1e7) as i64))
                .with("lon", int((f.lon * 1e7) as i64))
                .with("alt", int(((f.alt_agl + GROUND_AMSL) * 1000.0) as i64))
                .with("eph", int(eph as i64))
                .with("epv", int(epv as i64))
                .with("vel", int((f.ground_speed * 100.0) as i64))
                .with("cog", int((f.heading * 100.0) as i64 % 36000))
                .with("satellites_visible", int(18)),
        );
    }

    fn status(&mut self) {
        let (volts, amps, left) = self.o.model.battery(self.t());
        // A calibration under way refuses arming.
        let health = if self.calibration.is_some() {
            SENSORS & !PREARM
        } else {
            SENSORS
        };
        let load = 180.0 + self.random() * 60.0;
        self.send(
            &msg("SYS_STATUS")
                .with("onboard_control_sensors_present", int(SENSORS))
                .with("onboard_control_sensors_enabled", int(SENSORS))
                .with("onboard_control_sensors_health", int(health))
                .with("load", int(load as i64))
                .with("voltage_battery", int((volts * 1000.0) as i64))
                .with("current_battery", int((amps * 100.0) as i64))
                .with("battery_remaining", int(left)),
        );
    }

    fn battery(&mut self) {
        let (volts, amps, left) = self.o.model.battery(self.t());
        let cells = self
            .o
            .params
            .get("BAT1_N_CELLS")
            .and_then(Value::as_i64)
            .unwrap_or(6)
            .clamp(1, 10);
        let each = (volts / cells as f64 * 1000.0) as i64;
        let list = (0..10).map(|i| int(if i < cells { each } else { 65535 })).collect();
        let warm = 2850.0 + self.random() * 100.0;
        self.send(
            &msg("BATTERY_STATUS")
                .with("battery_function", int(1))
                .with("type", int(1))
                .with("temperature", int(warm as i64))
                .with("voltages", Wv::Array(list))
                .with("current_battery", int((amps * 100.0) as i64))
                .with("current_consumed", int((amps * self.t() / 3.6) as i64))
                .with("energy_consumed", int(-1))
                .with("battery_remaining", int(left)),
        );
    }

    fn landed(&mut self) {
        let f = self.o.model.sample(self.t());
        let state = match (f.landed, f.takeoff) {
            (true, _) => 1,
            (_, true) => 3,
            _ => 2,
        };
        self.send(&msg("EXTENDED_SYS_STATE").with("landed_state", int(state)));
    }

    fn channels(&mut self) {
        let t = self.o.transmitter.clone();
        if t.is_null() {
            return;
        }
        let wobble = self.o.model.sample(self.t()).roll;
        let n = |k: &str, d: f64| t[k].as_f64().unwrap_or(d);
        let mut list = [0.0f64; 16];
        list[0] = n("roll", 0.0) + wobble * 0.15;
        list[1] = n("pitch", 0.0) + wobble * 0.1;
        list[2] = n("throttle", 0.55) * 2.0 - 1.0;
        list[3] = n("yaw", 0.0);
        // Channel, position, of how many.
        for s in t["switches"].as_array().into_iter().flatten() {
            let at = |i: usize| s[i].as_f64().unwrap_or(0.0);
            if let Some(c) = list.get_mut((at(0) as usize).wrapping_sub(1)) {
                *c = -1.0 + 2.0 * at(1) / (at(2) - 1.0).max(1.0);
            }
        }
        for k in t["knobs"].as_array().into_iter().flatten() {
            if let Some(c) = list.get_mut((k[0].as_f64().unwrap_or(0.0) as usize).wrapping_sub(1)) {
                *c = k[1].as_f64().unwrap_or(0.5) * 2.0 - 1.0;
            }
        }
        let mut m = msg("RC_CHANNELS")
            .with("time_boot_ms", self.ms())
            .with("chancount", int(16))
            .with("rssi", int(n("rssi", 96.0) as i64));
        for (i, v) in list.into_iter().enumerate() {
            let us = (1500.0 + v * 500.0 + self.gauss(2.5)).round().clamp(1000.0, 2000.0);
            m = m.with(&format!("chan{}_raw", i + 1), int(us as i64));
        }
        for i in 17..=18 {
            m = m.with(&format!("chan{i}_raw"), int(65535));
        }
        self.send(&m);
    }

    fn vibration(&mut self) {
        let base = 2.4 + self.o.model.sample(self.t()).ground_speed * 0.08;
        let (x, y, z) = (
            base + self.random() * 0.4,
            base + self.random() * 0.4,
            base * 1.3 + self.random() * 0.5,
        );
        self.send(
            &msg("VIBRATION")
                .with("time_usec", int(unix_us()))
                .with("vibration_x", float(x))
                .with("vibration_y", float(y))
                .with("vibration_z", float(z)),
        );
    }

    fn time(&mut self) {
        self.send(
            &msg("SYSTEM_TIME")
                .with("time_unix_usec", int(unix_us()))
                .with("time_boot_ms", self.ms()),
        );
    }

    fn home(&mut self) {
        let (lat, lon) = self.o.model.home;
        self.send(
            &msg("HOME_POSITION")
                .with("latitude", int((lat * 1e7) as i64))
                .with("longitude", int((lon * 1e7) as i64))
                .with("altitude", int((GROUND_AMSL * 1000.0) as i64)),
        );
    }

    fn mission_current(&mut self) {
        let n = self.items.len() as i64;
        let state = match (n, self.o.armed && self.o.mode == "MISSION") {
            (0, _) => 1,
            (_, true) => 3,
            _ => 2,
        };
        self.send(
            &msg("MISSION_CURRENT")
                .with("total", int(if n == 0 { 65535 } else { n }))
                .with("mission_state", int(state))
                .with("mission_mode", int(i64::from(state == 3))),
        );
    }

    fn version(&mut self) {
        let sw = 1 << 24 | 18 << 16 | 255;
        let hash = [0xc9, 0xb8, 0xa7, 0xf6, 0xe5, 0xd4, 0xc3, 0xb2].map(int).to_vec();
        self.send(
            &msg("AUTOPILOT_VERSION")
                .with("capabilities", int(4 | 2 | 8 | 8192 | 32))
                .with("flight_sw_version", int(sw))
                .with("middleware_sw_version", int(sw))
                .with("flight_custom_version", Wv::Array(hash))
                .with("vendor_id", int(0x3162))
                .with("product_id", int(0x0047))
                .with("uid", int(0x0123_4567_89AB_CDEF)),
        );
    }

    // ---- answers ----------------------------------------------------------

    fn handle(&mut self, m: &Message) {
        let n = |k: &str| m.get(k).and_then(Wv::as_i64).unwrap_or(0);
        match m.name() {
            "PARAM_REQUEST_LIST" => self.burst = (0..self.names.len()).collect(),
            "PARAM_REQUEST_READ" => {
                let id = text(m, "param_id");
                let index = match self.names.iter().position(|p| *p == id) {
                    Some(i) => Some(i),
                    None => usize::try_from(n("param_index"))
                        .ok()
                        .filter(|i| *i < self.names.len() && id.is_empty()),
                };
                // A name this firmware does not have goes unanswered, as on a real board.
                if let Some(i) = index {
                    self.param(i);
                }
            }
            "PARAM_SET" => self.set_param(m),
            "COMMAND_LONG" | "COMMAND_INT" => self.command(m),
            "SET_MODE" => {
                let mode = n("custom_mode");
                self.o.mode = mode_of(mode >> 16 & 0xFF, mode >> 24 & 0xFF);
            }
            "MISSION_COUNT" => {
                self.upload = n("mission_type");
                self.fence_count = n("count");
                if self.upload == 0 {
                    self.items.clear();
                }
                if self.fence_count <= 0 {
                    self.mission_ack();
                } else {
                    self.request(0);
                }
            }
            "MISSION_ITEM_INT" => {
                if self.upload == 0 {
                    let keep = m.def().fields.iter().filter(|f| !f.name.starts_with("target_"));
                    self.items.push(
                        keep.filter_map(|f| Some((f.name.to_string(), m.get(f.name)?.clone())))
                            .collect(),
                    );
                }
                if n("seq") + 1 < self.fence_count {
                    self.request(n("seq") + 1);
                } else {
                    self.mission_ack();
                }
            }
            "MISSION_REQUEST_LIST" => {
                let count = if n("mission_type") == 0 {
                    self.items.len() as i64
                } else {
                    0
                };
                self.send(
                    &self
                        .to_gcs(msg("MISSION_COUNT"))
                        .with("count", int(count))
                        .with("mission_type", int(n("mission_type"))),
                );
            }
            "MISSION_REQUEST_INT" | "MISSION_REQUEST" => {
                if let Some(item) = usize::try_from(n("seq")).ok().and_then(|i| self.items.get(i)) {
                    let mut out = self.to_gcs(msg("MISSION_ITEM_INT"));
                    for (k, v) in item {
                        out = out.with(k, v.clone());
                    }
                    self.send(&out);
                }
            }
            "MISSION_CLEAR_ALL" => {
                self.upload = n("mission_type");
                self.items.clear();
                self.mission_ack();
            }
            "LOG_REQUEST_LIST" => {
                let last = self.o.logs.last().map_or(0, |l| l.0);
                for (id, utc, size, _) in &self.o.logs {
                    self.send(
                        &msg("LOG_ENTRY")
                            .with("id", int(*id))
                            .with("num_logs", int(self.o.logs.len() as i64))
                            .with("last_log_num", int(last))
                            .with("time_utc", int(*utc))
                            .with("size", int(*size as i64)),
                    );
                }
            }
            "FILE_TRANSFER_PROTOCOL" => self.ftp(m),
            "SERIAL_CONTROL" if n("device") == 10 => self.serial(m),
            _ => {}
        }
    }

    fn param(&self, i: usize) {
        let name = &self.names[i];
        let v = &self.o.params[name];
        // An INT32 travels as its own four bytes in the float field.
        let (value, ty) = match v.as_i64() {
            Some(n) => (f64::from(f32::from_bits(n as i32 as u32)), 6),
            None => (v.as_f64().unwrap_or(0.0), 9),
        };
        let mut id = name.as_bytes().to_vec();
        id.resize(16, 0);
        self.send(
            &msg("PARAM_VALUE")
                .with("param_id", Wv::Bytes(id))
                .with("param_value", float(value))
                .with("param_type", int(ty))
                .with("param_count", int(self.names.len() as i64))
                .with("param_index", int(i as i64)),
        );
    }

    fn set_param(&mut self, m: &Message) {
        let id = text(m, "param_id");
        let Some(i) = self.names.iter().position(|p| *p == id) else {
            return;
        };
        // PX4 changes no geometry in the air.
        if !(self.o.armed && ["CA_", "SYS_", "PWM_"].iter().any(|p| id.starts_with(p))) {
            let v = m.get("param_value").and_then(Wv::as_f64).unwrap_or(0.0);
            let old = &self.o.params[&id];
            let new = if old.is_i64() {
                Value::from(i64::from((v as f32).to_bits() as i32))
            } else {
                Value::from(v)
            };
            self.o.params.insert(id, new);
        }
        self.param(i);
    }

    fn ack(&self, command: i64, result: i64) {
        self.send(
            &msg("COMMAND_ACK")
                .with("command", int(command))
                .with("result", int(result)),
        );
    }

    fn command(&mut self, m: &Message) {
        let p = |i: usize| m.get(&format!("param{i}")).and_then(Wv::as_f64).unwrap_or(0.0);
        let command = m.get("command").and_then(Wv::as_i64).unwrap_or(0);
        let mut result = ACCEPTED;
        match command {
            // PREFLIGHT_CALIBRATION
            241 => {
                self.ack(command, ACCEPTED);
                let all: Vec<f64> = (1..=7).map(p).collect();
                return self.start_calibration(&all);
            }
            // COMPONENT_ARM_DISARM
            400 => {
                let want = p(1) >= 0.5;
                if want && self.calibration.is_some() {
                    result = BUSY;
                    self.say(2, "Arming denied: calibration in progress");
                } else {
                    self.o.armed = want;
                    self.say(6, if want { "Armed by external command" } else { "Disarmed" });
                }
            }
            176 => self.o.mode = mode_of(p(2) as i64, p(3) as i64),
            22 => {
                self.o.mode = "TAKEOFF".into();
                self.o.armed = true;
            }
            21 => self.o.mode = "LAND".into(),
            20 => self.o.mode = "RTL".into(),
            192 => self.o.mode = "HOLD".into(),
            300 if self.items.is_empty() => {
                result = DENIED;
                self.say(4, "No valid mission available");
            }
            300 => {
                self.o.armed = true;
                self.o.mode = "MISSION".into();
            }
            512 | 520 | 410 => {
                self.ack(command, ACCEPTED);
                match (command, p(1) as i64) {
                    (520, _) | (512, 148) => self.version(),
                    (410, _) | (512, 242) => self.home(),
                    _ => {}
                }
                return;
            }
            179 | 511 | 519 => {}
            209 if self.o.armed => result = BUSY,
            209 => {}
            _ => result = UNSUPPORTED,
        }
        self.ack(command, result);
    }

    fn request(&self, seq: i64) {
        self.send(
            &self
                .to_gcs(msg("MISSION_REQUEST_INT"))
                .with("seq", int(seq))
                .with("mission_type", int(self.upload)),
        );
    }

    fn mission_ack(&self) {
        self.send(
            &self
                .to_gcs(msg("MISSION_ACK"))
                .with("type", int(0))
                .with("mission_type", int(self.upload)),
        );
    }

    // ---- calibration -------------------------------------------------------

    fn start_calibration(&mut self, p: &[f64]) {
        if p.iter().all(|v| *v == 0.0) {
            if self.calibration.take().is_some() {
                self.say(4, "[cal] calibration cancelled");
            }
            return;
        }
        if self.calibration.is_some() {
            return self.say(4, "[cal] calibration already in progress");
        }
        let on = |i: usize, v: f64| (p[i] - v).abs() < 0.01;
        let sensor = match () {
            () if on(0, 1.0) => "gyro",
            () if on(1, 1.0) => "compass",
            () if on(2, 1.0) => "baro",
            () if on(4, 1.0) || on(4, 4.0) => "accel",
            () if on(4, 2.0) => "level",
            () if on(5, 1.0) => "airspeed",
            () => return,
        };
        let mut c = Calibration::default();
        let mut at = self.t();
        let mut say = |wait: f64, text: String| {
            c.lines.push_back((at, text));
            at += wait * CALIBRATION_STEP_S;
        };
        say(1.0, format!("[cal] calibration started: 2 {sensor}"));
        if sensor == "accel" || sensor == "compass" {
            for done in 0..SIDES.len() {
                say(
                    1.0,
                    format!("[cal] Rotate to a pending side: {}", SIDES[done..].join(", ")),
                );
                if self.o.pause_after == Some(done) {
                    c.held = true;
                    break;
                }
                let side = SIDES[done];
                say(0.6, format!("[cal] {side} orientation detected"));
                say(1.0, format!("[cal] Hold still, measuring {side} side"));
                if sensor == "compass" {
                    say(1.4, "[cal] Rotate vehicle around the detected orientation".into());
                }
                say(0.3, format!("[cal] progress <{}>", (done + 1) * 100 / SIDES.len()));
                say(0.5, format!("[cal] {side} side done"));
            }
        } else {
            say(1.0, format!("[cal] Hold still, measuring {sensor}"));
            for percent in [30, 70, 100] {
                say(0.34, format!("[cal] progress <{percent}>"));
            }
        }
        if !c.held {
            say(0.0, format!("[cal] calibration done: {sensor}"));
        }
        self.calibration = Some(c);
    }

    fn calibrate(&mut self, now: f64) {
        let Some(mut c) = self.calibration.take() else { return };
        while c.lines.front().is_some_and(|(at, _)| *at <= now) {
            if let Some((_, text)) = c.lines.pop_front() {
                self.say(6, &text);
            }
        }
        if c.held || !c.lines.is_empty() {
            self.calibration = Some(c);
        }
    }

    // ---- shell and files ----------------------------------------------------

    fn serial(&mut self, m: &Message) {
        if m.get("flags").and_then(Wv::as_i64).unwrap_or(0) == 0 {
            self.shell = None;
            return;
        }
        let count = m.get("count").and_then(Wv::as_i64).unwrap_or(0).clamp(0, 70) as usize;
        let typed: Vec<u8> = bytes(m, "data").into_iter().take(count).collect();
        let mut line = self.shell.take().unwrap_or_default();
        let mut out = String::new();
        for c in String::from_utf8_lossy(&typed).chars() {
            match c {
                '\r' | '\n' => {
                    out += "\r\n";
                    out += &data::shell(&line);
                    out += "nsh> ";
                    line.clear();
                }
                '\x03' => {
                    line.clear();
                    out += "^C\r\nnsh> ";
                }
                '\x08' | '\x7f' => {
                    if line.pop().is_some() {
                        out += "\x08 \x08";
                    }
                }
                c if c >= ' ' => {
                    line.push(c);
                    out.push(c);
                }
                _ => {}
            }
        }
        self.shell = Some(line);
        for chunk in out.as_bytes().chunks(70) {
            let mut d: Vec<Wv> = chunk.iter().map(|b| int(i64::from(*b))).collect();
            d.resize(70, int(0));
            self.out.push_back(
                msg("SERIAL_CONTROL")
                    .with("device", int(10))
                    .with("flags", int(1))
                    .with("count", int(chunk.len() as i64))
                    .with("data", Wv::Array(d)),
            );
        }
    }

    /// Open, burst read, checksum and close: what the parameter descriptions are read with.
    fn ftp(&mut self, m: &Message) {
        let raw = bytes(m, "payload");
        if raw.len() < 12 {
            return;
        }
        let seq = u16::from_le_bytes([raw[0], raw[1]]).wrapping_add(1);
        let (op, size) = (raw[3], raw[4] as usize);
        let at = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]) as usize;
        let body: Vec<u8> = raw.iter().skip(12).take(size).copied().collect();
        let gcs = self.gcs;
        let reply = move |seq: u16, code: u8, data: &[u8], at: usize, last: bool| {
            let mut b = vec![0u8; 251];
            b[..2].copy_from_slice(&seq.to_le_bytes());
            b[3] = code;
            b[4] = data.len() as u8;
            b[5] = op;
            b[6] = u8::from(last);
            b[8..12].copy_from_slice(&(at as u32).to_le_bytes());
            b[12..12 + data.len()].copy_from_slice(data);
            msg("FILE_TRANSFER_PROTOCOL")
                .with("target_system", int(i64::from(gcs.0)))
                .with("target_component", int(i64::from(gcs.1)))
                .with("payload", Wv::Array(b.into_iter().map(|b| int(i64::from(b))).collect()))
        };
        let path = String::from_utf8_lossy(&body).trim_end_matches('\0').to_string();
        let known = path.trim_start_matches('/') == "etc/extras/parameters.json.xz";
        match op {
            4 if known => {
                let file = data::metadata(&self.o.params);
                self.out
                    .push_back(reply(seq, 128, &(file.len() as u32).to_le_bytes(), 0, false));
                self.file = Some(file);
            }
            14 if known => {
                let crc = crc32(&data::metadata(&self.o.params));
                self.out.push_back(reply(seq, 128, &crc.to_le_bytes(), 0, false));
            }
            4 | 14 => self.out.push_back(reply(seq, 129, &[10], 0, false)),
            15 => {
                let Some(file) = &self.file else {
                    return self.out.push_back(reply(seq, 129, &[4], 0, false));
                };
                let mut n = seq;
                for (i, chunk) in file.get(at..).unwrap_or(&[]).chunks(239).enumerate() {
                    let pos = at + i * 239;
                    self.out
                        .push_back(reply(n, 128, chunk, pos, pos + chunk.len() >= file.len()));
                    n = n.wrapping_add(1);
                }
                self.out.push_back(reply(n, 129, &[6], file.len(), false));
            }
            1 | 2 => {
                self.file = None;
                self.out.push_back(reply(seq, 128, &[], 0, false));
            }
            _ => self.out.push_back(reply(seq, 129, &[7], 0, false)),
        }
    }
}

fn bytes(m: &Message, field: &str) -> Vec<u8> {
    match m.get(field) {
        Some(Wv::Bytes(b)) => b.clone(),
        Some(Wv::Array(a)) => a.iter().map(|v| v.as_i64().unwrap_or(0) as u8).collect(),
        _ => Vec::new(),
    }
}

fn text(m: &Message, field: &str) -> String {
    String::from_utf8_lossy(&bytes(m, field))
        .trim_end_matches('\0')
        .trim()
        .to_string()
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for b in data {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { crc >> 1 ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_go_out_and_come_back() {
        for m in ["MISSION", "POSCTL", "LAND", "RTL", "HOLD"] {
            let c = custom_mode(m);
            assert_eq!(mode_of(c >> 16 & 0xFF, c >> 24 & 0xFF), m);
        }
    }

    #[test]
    fn a_checksum_is_the_usual_one() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
