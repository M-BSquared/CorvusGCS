//! The update check answers what `update_check.py` answers, against a local
//! stub of the release API (`python/tools/dump_ai_cases.py update`).

mod common_ai;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use common_ai::Tmp;
use corvus_core::plugins::update_check::*;
use serde_json::{json, Value};

fn fixture() -> Value {
    serde_json::from_str(include_str!("data/ai_update.json")).unwrap()
}

/// A server that answers every request with `status` and `body`, and keeps the
/// request heads it saw. Stops when the returned flag is dropped.
struct Stub {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Stub {
    fn new(status: u16, body: String, chunked: bool) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/releases?per_page=10", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, st) = (Arc::clone(&seen), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            while !st.load(std::sync::atomic::Ordering::SeqCst) {
                let Ok((mut conn, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                conn.set_nonblocking(false).unwrap();
                let mut buf = [0u8; 4096];
                let n = conn.read(&mut buf).unwrap_or(0);
                s.lock().unwrap().push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let head = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nConnection: close\r\n");
                let reply = if chunked {
                    let mut out = format!("{head}Transfer-Encoding: chunked\r\n\r\n").into_bytes();
                    for part in body.as_bytes().chunks(1000) {
                        out.extend(format!("{:x}\r\n", part.len()).bytes());
                        out.extend(part);
                        out.extend(b"\r\n");
                    }
                    out.extend(b"0\r\n\r\n");
                    out
                } else {
                    let mut out = format!("{head}Content-Length: {}\r\n\r\n", body.len()).into_bytes();
                    out.extend(body.as_bytes());
                    out
                };
                let _ = conn.write_all(&reply);
            }
        });
        Stub {
            url,
            seen,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn normalized(mut m: serde_json::Map<String, Value>) -> Value {
    let at = m["checked_at"].as_f64().unwrap_or(0.0) > 0.0;
    m.insert("checked_at".into(), json!(at));
    Value::Object(m)
}

fn error_class(v: &mut Value) {
    if v.get("current").is_some() {
        v["current"] = json!("");
    }
    // The words after "could not reach the release server" are the HTTP library's.
    if let Some(e) = v["error"].as_str() {
        if e.starts_with("could not reach the release server (") && !e.contains("HTTP Error") {
            v["error"] = json!("could not reach the release server (…)");
        }
    }
}

#[test]
fn versions_pages_and_picks_are_python_s() {
    let f = fixture();
    for pair in f["parse"].as_array().unwrap() {
        let got = match &pair[0] {
            Value::String(s) => parse_version(s),
            Value::Null => parse_version(""),
            other => parse_version(&other.to_string()),
        };
        assert_eq!(json!(got), pair[1], "parse {}", pair[0]);
    }
    for t in f["newer"].as_array().unwrap() {
        assert_eq!(
            json!(is_newer(t[0].as_str().unwrap(), t[1].as_str().unwrap())),
            t[2],
            "{t}"
        );
    }
    for p in f["pages"].as_array().unwrap() {
        assert_eq!(json!(release_page(p[0].as_str().unwrap())), p[1]);
    }
    let releases: Value = {
        // the payloads are inside the scenarios' bodies, in the same order as the picks
        let mut m = serde_json::Map::new();
        for s in f["scenarios"].as_array().unwrap() {
            if let Some(body) = s["scenario"]["body"].as_str() {
                if let Ok(v) = serde_json::from_str::<Value>(body) {
                    if s["scenario"]["status"] == 200 {
                        m.insert(s["scenario"]["name"].as_str().unwrap().into(), v);
                    }
                }
            }
        }
        Value::Object(m)
    };
    for pick in f["picks"].as_array().unwrap() {
        let name = pick[0].as_str().unwrap();
        let Some(payload) = releases.get(name) else { continue };
        assert_eq!(json!(pick_latest(payload)), pick[1], "pick {name}");
    }
    assert_eq!(json!([CHECK_INTERVAL_S, RETRY_INTERVAL_S]), f["intervals"]);
}

#[test]
fn every_scenario_ends_where_the_python_check_ends() {
    let f = fixture();
    for sc in f["scenarios"].as_array().unwrap() {
        let s = &sc["scenario"];
        let Some(body) = s["body"].as_str() else { continue };
        let name = s["name"].as_str().unwrap();
        let stub = Stub::new(s["status"].as_u64().unwrap() as u16, body.to_string(), name == "mixed");
        let tmp = Tmp::new("update");
        let checker = UpdateChecker::with_url(&tmp.0, &stub.url);
        let mut first = normalized(checker.check(true));
        error_class(&mut first);
        let mut want = sc["first"].clone();
        error_class(&mut want);
        assert_eq!(first, want, "{name}: first");
        let mut cache: Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.0.join("update.json")).unwrap()).unwrap();
        cache["checked_at"] = json!(cache["checked_at"].as_f64().unwrap() > 0.0);
        error_class(&mut cache);
        let mut want_cache = sc["cache"].clone();
        error_class(&mut want_cache);
        assert_eq!(cache, want_cache, "{name}: cache");
        let before = stub.seen.lock().unwrap().len();
        let mut second = normalized(checker.check(false));
        error_class(&mut second);
        let mut want = sc["second"].clone();
        error_class(&mut want);
        assert_eq!(second, want, "{name}: second");
        assert_eq!(
            stub.seen.lock().unwrap().len(),
            before,
            "{name}: a fresh answer is not fetched again"
        );
        let mut cached = normalized(checker.cached_status());
        error_class(&mut cached);
        let mut want = sc["cached"].clone();
        error_class(&mut want);
        assert_eq!(cached, want, "{name}: cached");
        let head = stub.seen.lock().unwrap()[0].clone();
        assert!(head.starts_with("GET /releases?per_page=10 HTTP/1.1\r\n"), "{head}");
        assert!(head.contains(&format!("User-Agent: CorvusGCS/{}", corvus_core::version::version())));
        assert!(head.to_lowercase().contains("accept: application/vnd.github+json"));
    }
}

#[test]
fn the_cache_file_is_written_in_pythons_spelling_and_staleness_follows_the_rules() {
    let tmp = Tmp::new("update-cache");
    let stub = Stub::new(
        200,
        json!([{"tag_name": "v2999.01.01", "name": "Né"}]).to_string(),
        false,
    );
    let checker = UpdateChecker::with_url(&tmp.0, &stub.url);
    checker.check_at(true, 1000.5);
    let text = std::fs::read_to_string(tmp.0.join("update.json")).unwrap();
    assert!(
        text.starts_with("{\"checked_at\": 1000.5, \"latest\": {\"tag\": \"v2999.01.01\""),
        "{text}"
    );
    assert!(text.contains("\"name\": \"N\\u00e9\""), "{text}");
    // fresh for six hours, then fetched again
    checker.check_at(false, 1000.5 + CHECK_INTERVAL_S - 1.0);
    assert_eq!(stub.seen.lock().unwrap().len(), 1);
    checker.check_at(false, 1000.5 + CHECK_INTERVAL_S);
    assert_eq!(stub.seen.lock().unwrap().len(), 2);
    // a clock that went backwards is stale
    checker.check_at(false, 10.0);
    assert_eq!(stub.seen.lock().unwrap().len(), 3);
}

#[test]
fn a_server_that_is_not_there_gives_the_cached_release_and_a_message() {
    let tmp = Tmp::new("update-offline");
    std::fs::write(
        tmp.0.join("update.json"),
        r#"{"checked_at": 1.0, "latest": {"tag": "v2999.01.01", "version": "2999.01.01", "name": "N", "url": "https://evil.example/", "assets": []}, "error": ""}"#,
    )
    .unwrap();
    let checker = UpdateChecker::with_url(&tmp.0, "http://127.0.0.1:9/releases");
    let got = checker.check(false);
    assert_eq!(got["latest"], "2999.01.01");
    assert_eq!(got["update_available"], true);
    assert!(got["error"]
        .as_str()
        .unwrap()
        .starts_with("could not reach the release server ("));
    assert_eq!(
        got["url"], "https://github.com/M-BSquared/CorvusGCS/releases/tag/v2999.01.01",
        "the page is built from the tag"
    );
}

/// Reaches GitHub for real; run by hand: `cargo test -p corvus_core --test ai_update -- --ignored`.
#[test]
#[ignore]
fn the_real_release_list_is_read_over_https() {
    let tmp = Tmp::new("update-real");
    let got = UpdateChecker::new(&tmp.0).check(true);
    assert_eq!(got["error"], "", "{got:?}");
}
