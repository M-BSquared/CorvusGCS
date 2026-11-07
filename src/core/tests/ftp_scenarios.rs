//! The Rust FTP engine plays the scenarios the Python one played
//! (`python/tools/dump_protocol_cases.py`) and must do the same: the frames
//! it sends, the bytes it returns, its errors and its progress.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use corvus_core::protocol::ftp::{decode, FtpEngine, FtpHost, FtpLimits};
use serde_json::{json, Value as Json};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct Sim {
    sc: Json,
    clock: Mutex<f64>,
    engine: Mutex<Option<Arc<FtpEngine>>>,
    frames: Mutex<Vec<Json>>,
    files: Vec<(String, Vec<u8>)>,
    opens: Mutex<u32>,
    bursts: Mutex<u32>,
    current: Mutex<Vec<u8>>,
}

impl Sim {
    fn veh(&self, key: &str) -> &Json {
        &self.sc["vehicle"][key]
    }

    #[allow(clippy::too_many_arguments)]
    fn deliver(&self, seq: u16, session: u8, req_op: u8, opcode: u8, offset: u32, data: &[u8], burst_complete: bool) {
        let mut buf = vec![0u8; 251];
        buf[0..2].copy_from_slice(&seq.wrapping_add(1).to_le_bytes());
        buf[2] = session;
        buf[3] = opcode;
        buf[4] = data.len() as u8;
        buf[5] = req_op;
        buf[6] = burst_complete as u8;
        buf[8..12].copy_from_slice(&offset.to_le_bytes());
        buf[12..12 + data.len()].copy_from_slice(data);
        let engine = self.engine.lock().unwrap().clone().unwrap();
        engine.handle_message(self, 255, 0, &buf);
    }

    fn nak(&self, seq: u16, session: u8, req_op: u8, code: u8, offset: u32) {
        self.deliver(seq, session, req_op, 129, offset, &[code], false);
    }

    fn vehicle(&self, seq: u16, session: u8, opcode: u8, size: usize, offset: u32, data: &[u8]) {
        let _ = size;
        if self.veh("silent").as_bool().unwrap_or(false) {
            return;
        }
        match opcode {
            4 => {
                let n = {
                    let mut o = self.opens.lock().unwrap();
                    *o += 1;
                    *o
                };
                let path = String::from_utf8_lossy(data).to_string();
                if self.veh("no_sessions_once").as_bool().unwrap_or(false) && n == 1 {
                    return self.nak(seq, 0, 4, 5, 0);
                }
                let Some((_, file)) = self.files.iter().find(|(p, _)| *p == path) else {
                    return self.nak(seq, 0, 4, 10, 0);
                };
                let report = self.veh("report_size").as_u64().unwrap_or(file.len() as u64) as u32;
                *self.current.lock().unwrap() = file.clone();
                self.deliver(seq, 1, 4, 128, 0, &report.to_le_bytes(), false);
            }
            1 | 2 => self.deliver(seq, session, opcode, 128, 0, &[], false),
            14 => {
                let path = String::from_utf8_lossy(data).to_string();
                let Some((_, file)) = self.files.iter().find(|(p, _)| *p == path) else {
                    return self.nak(seq, 0, 14, 10, 0);
                };
                if self.veh("short_crc").as_bool().unwrap_or(false) {
                    return self.deliver(seq, 0, 14, 128, 0, &[1], false);
                }
                self.deliver(seq, 0, 14, 128, 0, &crc32(file).to_le_bytes(), false);
            }
            15 => {
                let burst = {
                    let mut b = self.bursts.lock().unwrap();
                    *b += 1;
                    *b
                };
                let f = self.current.lock().unwrap().clone();
                let per_burst = self.veh("chunks_per_burst").as_u64().unwrap_or(1000);
                let drop: Vec<u64> = self
                    .veh("drop_first_burst")
                    .as_array()
                    .map(|a| a.iter().filter_map(Json::as_u64).collect())
                    .unwrap_or_default();
                let (mut pos, mut n) = (offset as usize, 0u64);
                loop {
                    if pos >= f.len() {
                        return self.nak(seq, session, 15, 6, f.len() as u32);
                    }
                    if n >= per_burst {
                        return;
                    }
                    let part = &f[pos..(pos + 239).min(f.len())];
                    let done = pos + part.len() >= f.len();
                    if !(burst == 1 && drop.contains(&n)) {
                        self.deliver(
                            seq,
                            session,
                            15,
                            128,
                            pos as u32,
                            part,
                            done && !self.veh("no_eof_flag").as_bool().unwrap_or(false),
                        );
                    }
                    pos += part.len();
                    n += 1;
                    if done {
                        if self.veh("eof_after").as_bool().unwrap_or(false) {
                            self.nak(seq, session, 15, 6, f.len() as u32);
                        }
                        return;
                    }
                }
            }
            _ => {}
        }
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

impl FtpHost for Sim {
    fn now(&self) -> f64 {
        let mut c = self.clock.lock().unwrap();
        *c += 0.05;
        *c
    }
    fn slow_link(&self) -> bool {
        self.sc["slow"].as_bool().unwrap_or(false)
    }
    fn stopping(&self) -> bool {
        self.sc["stopped"].as_bool().unwrap_or(false)
    }
    fn connected(&self) -> bool {
        !self.sc["not_connected"].as_bool().unwrap_or(false)
    }
    fn send_ftp(&self, p: &[u8]) -> Result<bool, String> {
        let seq = u16::from_le_bytes([p[0], p[1]]);
        let (session, opcode, size) = (p[2], p[3], p[4] as usize);
        let offset = u32::from_le_bytes([p[8], p[9], p[10], p[11]]);
        let data = p[12..12 + size].to_vec();
        self.frames
            .lock()
            .unwrap()
            .push(json!([seq, session, opcode, size, offset, hex(&data)]));
        self.vehicle(seq, session, opcode, size, offset, &data);
        Ok(true)
    }
    fn source(&self) -> (u8, u8) {
        (255, 0)
    }
}

#[test]
fn every_scenario_ends_where_the_python_engine_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/ftp_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 19);
    std::thread::scope(|scope| {
        let handles: Vec<_> = cases.iter().map(|case| scope.spawn(move || play(case))).collect();
        for h in handles {
            h.join().unwrap();
        }
    });
}

fn play(case: &Json) {
    {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let lim = &sc["limits"];
        let engine = Arc::new(FtpEngine::new(FtpLimits {
            max_file_bytes: lim["max_file"].as_u64().unwrap_or(8 * 1024 * 1024),
            request_retries: lim["retries"].as_i64().unwrap_or(3),
            ..FtpLimits::default()
        }));
        let files: Vec<(String, Vec<u8>)> = sc["files"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), unhex(v.as_str().unwrap()))).collect())
            .unwrap_or_default();
        let sim = Sim {
            sc: sc.clone(),
            clock: Mutex::new(1000.0),
            engine: Mutex::new(Some(engine.clone())),
            frames: Mutex::default(),
            files,
            opens: Mutex::new(0),
            bursts: Mutex::new(0),
            current: Mutex::default(),
        };
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = sim.frames.lock().unwrap().len();
            let progress: Mutex<Vec<Json>> = Mutex::default();
            let cb = |got: u64, total: u64| progress.lock().unwrap().push(json!([got, total]));
            let cancel = step["cancel"]
                .as_bool()
                .unwrap_or(false)
                .then(|| Arc::new(AtomicBool::new(true)));
            let path = step["path"].as_str().unwrap();
            let result = if step["op"] == "read" {
                match engine.read_file(&sim, path, Some(&cb), cancel) {
                    Ok(d) => json!({"ok": hex(&d)}),
                    Err(e) => json!({"error": [e.message, e.code]}),
                }
            } else {
                match engine.file_crc32(&sim, path, cancel) {
                    Ok(c) => json!({"ok": c}),
                    Err(e) => json!({"error": [e.message, e.code]}),
                }
            };
            let want = &case["expected"]["steps"][i];
            assert_eq!(result, want["result"], "{name} step {i}: result");
            assert_eq!(
                Json::Array(sim.frames.lock().unwrap()[before..].to_vec()),
                want["frames"],
                "{name} step {i}: frames"
            );
            assert_eq!(
                Json::Array(progress.lock().unwrap().clone()),
                want["progress"],
                "{name} step {i}: progress"
            );
        }
        let fin = &case["expected"]["final"];
        assert_eq!(json!(engine.listening()), fin["listening"], "{name}");
        assert_eq!(json!(engine.inbox_len()), fin["inbox"], "{name}");
        assert_eq!(json!(engine.busy()), fin["busy"], "{name}");
    }
}

#[test]
fn a_reply_for_another_station_or_a_message_nobody_listens_to_is_dropped() {
    let engine = FtpEngine::new(FtpLimits::default());
    let sim = Sim {
        sc: json!({}),
        clock: Mutex::new(0.0),
        engine: Mutex::new(None),
        frames: Mutex::default(),
        files: vec![],
        opens: Mutex::new(0),
        bursts: Mutex::new(0),
        current: Mutex::default(),
    };
    let reply = corvus_core::protocol::ftp::encode(2, 0, 128, 0, b"x", None);
    engine.handle_message(&sim, 255, 0, &reply);
    assert_eq!(engine.inbox_len(), 0, "nobody is listening");
    assert!(decode(&reply).is_some());
    let _ = Ordering::SeqCst;
}
