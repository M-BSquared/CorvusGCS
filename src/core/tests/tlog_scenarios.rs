//! The Rust tlog session plays the scenarios the Python one played
//! (`python/tools/dump_protocol_cases.py`, run on `tests/reference`) on real
//! files: the names, the metadata line, the frames, rolling on a disarm and
//! what the forwarder's sink receives.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use corvus_core::tlog::{TlogHost, TlogSession};
use serde_json::{json, Value as Json};

struct Fake {
    sc: Json,
    dir: PathBuf,
    stamps: Mutex<Vec<String>>,
    log: Mutex<Vec<Json>>,
}

impl TlogHost for Fake {
    fn running(&self) -> bool {
        self.sc["running"].as_bool().unwrap_or(true)
    }
    fn conn_string(&self) -> String {
        self.sc["conn"].as_str().unwrap_or("udp:127.0.0.1:14550").into()
    }
    fn log_dir(&self) -> PathBuf {
        self.dir.join("logs")
    }
    fn stamp(&self) -> String {
        self.stamps.lock().unwrap().remove(0)
    }
    fn log(&self, level: &str, text: &str) {
        let text = if text.starts_with("tlog disabled") {
            "tlog disabled"
        } else {
            text
        };
        self.log.lock().unwrap().push(json!(["log", level, text]));
    }
    fn request_version(&self) {
        self.log.lock().unwrap().push(json!(["call", "request_version"]));
    }
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn files(dir: &Path) -> Json {
    let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    let mut out = serde_json::Map::new();
    for path in names {
        let data = std::fs::read(&path).unwrap();
        let magic = b"#CORVUS-TLOG ";
        let at = data.iter().position(|b| *b == b'\n').unwrap_or(data.len());
        let (head, body) = (&data[..at], data.get(at + 1..).unwrap_or(&[]));
        let entry = if head.starts_with(magic) {
            let mut meta: Json = serde_json::from_slice(&head[magic.len()..]).unwrap();
            let started = meta["started_at"].as_str().unwrap().to_string();
            let shaped = started.ends_with("+00:00") && started.len() >= 25 && &started[10..11] == "T";
            meta["started_at"] = json!(shaped);
            if meta["version"] == json!(corvus_core::version::version()) {
                meta["version"] = json!("VERSION");
            }
            json!({"meta": meta, "body": hex(body), "header": ""})
        } else {
            json!({"meta": Json::Null, "body": hex(body), "header": hex(head)})
        };
        out.insert(path.file_name().unwrap().to_string_lossy().into_owned(), entry);
    }
    Json::Object(out)
}

#[test]
fn every_scenario_ends_where_the_python_tlog_session_ends() {
    let doc: Json = serde_json::from_str(include_str!("data/tlog_cases.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 12);
    let mut failures = Vec::new();
    for (n, case) in cases.iter().enumerate() {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let dir = std::env::temp_dir().join(format!("corvus-tlog-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if sc["bad_dir"].as_bool().unwrap_or(false) {
            std::fs::write(dir.join("logs"), "not a folder").unwrap();
        }
        let host = Arc::new(Fake {
            sc: sc.clone(),
            dir: dir.clone(),
            stamps: Mutex::new(
                sc["stamps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| s.as_str().unwrap().into())
                    .collect(),
            ),
            log: Mutex::new(Vec::new()),
        });
        let dyn_host: Arc<dyn TlogHost> = host.clone();
        let session = TlogSession::new();
        let sink_log = Arc::new(Mutex::new(Vec::<String>::new()));
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = host.log.lock().unwrap().len();
            let result = match step["op"].as_str().unwrap() {
                "start" => {
                    session.start_tlog(&dyn_host);
                    json!(session.is_active())
                }
                "stop" => {
                    session.stop_tlog(host.as_ref());
                    Json::Null
                }
                "frame" => {
                    let raw = step["raw"].as_str().map(unhex);
                    session.write_tlog(raw.as_deref());
                    Json::Null
                }
                "roll" => {
                    session.roll_tlog_on_disarm(&dyn_host, step["armed"].as_bool().unwrap());
                    json!(session.is_active())
                }
                "sink" => {
                    let log = Arc::clone(&sink_log);
                    session.set_frame_sink(Some(Arc::new(move |raw: &[u8]| log.lock().unwrap().push(hex(raw)))));
                    Json::Null
                }
                "files" => {
                    let listing = if dir.join("logs").is_dir() {
                        files(&dir.join("logs"))
                    } else {
                        json!({})
                    };
                    json!({"files": listing, "sink": sink_log.lock().unwrap().clone()})
                }
                other => panic!("{other}"),
            };
            let events = host.log.lock().unwrap()[before..].to_vec();
            let want = &case["expected"]["steps"][i];
            if result != want["result"] || json!(events) != want["events"] {
                failures.push(format!(
                    "{name} step {i} ({}): got {result} {}\n  want {} {}",
                    step["op"],
                    json!(events),
                    want["result"],
                    want["events"]
                ));
                break;
            }
        }
        session.stop_tlog(host.as_ref());
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(
        failures.is_empty(),
        "{} scenario(s) differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
