//! The Rust forwarder plays the scenarios the Python one played
//! (`python/tools/dump_protocol_cases.py`, run on `tests/reference`) over real
//! UDP sockets on loopback: what is mirrored to whom, who is learned, whose
//! frames reach the uplink, the loop guard, the busy listen port and the
//! status the LINK tab shows.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corvus_core::clock::Clock;
use corvus_core::forwarder::*;
use serde_json::{json, Value as Json};

struct OffsetClock {
    base: Instant,
    offset: Mutex<f64>,
}

impl Clock for OffsetClock {
    fn monotonic(&self) -> f64 {
        self.base.elapsed().as_secs_f64() + *self.offset.lock().unwrap()
    }

    fn local_hms(&self) -> String {
        "00:00:00".into()
    }
}

fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => x.as_f64() == y.as_f64(),
        (Json::Array(x), Json::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct Names(HashMap<String, u16>);

impl Names {
    /// `$A` is a port, `{A}` inside a string is replaced by it.
    fn tokens(&self, value: &Json) -> Json {
        match value {
            Json::String(s) => {
                if let Some(name) = s.strip_prefix('$') {
                    if let Some(port) = self.0.get(name) {
                        return json!(port);
                    }
                }
                let mut out = String::new();
                let mut rest = s.as_str();
                while let Some(open) = rest.find('{') {
                    out.push_str(&rest[..open]);
                    let close = rest[open..].find('}').map(|i| open + i);
                    match close {
                        Some(close) => {
                            out.push_str(&self.0[&rest[open + 1..close]].to_string());
                            rest = &rest[close + 1..];
                        }
                        None => {
                            out.push_str(&rest[open..]);
                            rest = "";
                        }
                    }
                }
                out.push_str(rest);
                Json::String(out)
            }
            Json::Array(items) => Json::Array(items.iter().map(|v| self.tokens(v)).collect()),
            Json::Object(map) => Json::Object(map.iter().map(|(k, v)| (k.clone(), self.tokens(v))).collect()),
            other => other.clone(),
        }
    }

    /// The name of a socket; the forwarder's own (`L`) wins when a port has two.
    fn name_of(&self, port: u64) -> Option<&String> {
        let own = self
            .0
            .get_key_value("L")
            .filter(|(_, p)| u64::from(**p) == port)
            .map(|(n, _)| n);
        own.or_else(|| self.0.iter().find(|(_, p)| u64::from(**p) == port).map(|(n, _)| n))
    }

    /// `:<port>` and `port <port>` become the name of the socket.
    fn text(&self, s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = String::new();
        let mut i = 0;
        while i < s.len() {
            let after_colon = bytes[i] == b':';
            let after_word = s[i..].starts_with("port ")
                && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
            if after_colon || after_word {
                let start = if after_colon { i + 1 } else { i + 5 };
                let end = start + s[start..].bytes().take_while(u8::is_ascii_digit).count();
                let boundary = end == s.len() || !(bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
                if end > start && boundary {
                    out.push_str(&s[i..start]);
                    let digits = &s[start..end];
                    match digits.parse::<u64>().ok().and_then(|p| self.name_of(p)) {
                        Some(name) => out.push_str(name),
                        None => out.push_str(digits),
                    }
                    i = end;
                    continue;
                }
            }
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }

    fn normalise(&self, value: &Json) -> Json {
        match value {
            Json::String(s) => Json::String(self.text(s)),
            Json::Array(items) => Json::Array(items.iter().map(|v| self.normalise(v)).collect()),
            Json::Object(map) => Json::Object(
                map.iter()
                    .map(|(k, v)| {
                        let named = matches!(k.as_str(), "port" | "listen_port" | "listen_port_requested")
                            .then(|| v.as_u64().filter(|p| *p != 0).and_then(|p| self.name_of(p)))
                            .flatten();
                        (k.clone(), named.map_or_else(|| self.normalise(v), |n| json!(n)))
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

fn udp() -> UdpSocket {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(150))).unwrap();
    sock
}

fn status_value(fwd: &Forwarder, key: &str) -> u64 {
    fwd.status()[key].as_u64().unwrap_or(0)
}

#[test]
fn the_forwarder_answers_like_python() {
    let data: Json = serde_json::from_str(include_str!("data/forwarder_cases.json")).unwrap();
    let mut failures = Vec::new();
    for case in data["cases"].as_array().unwrap() {
        let sc = &case["scenario"];
        let name = sc["name"].as_str().unwrap();
        let mut names = Names(HashMap::new());
        let mut socks: HashMap<String, UdpSocket> = HashMap::new();
        for peer in sc["peers"].as_array().unwrap() {
            let sock = udp();
            let port = sock.local_addr().unwrap().port();
            names.0.insert(peer.as_str().unwrap().to_string(), port);
            socks.insert(peer.as_str().unwrap().to_string(), sock);
        }
        if sc.get("busy").is_some_and(|b| !b.is_null()) {
            let busy = UdpSocket::bind("127.0.0.1:0").unwrap();
            names.0.insert("BUSY".into(), busy.local_addr().unwrap().port());
            socks.insert("BUSY".into(), busy);
        }
        if let Some(free) = sc.get("free").and_then(Json::as_str) {
            let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
            names.0.insert(free.to_string(), probe.local_addr().unwrap().port());
        }
        let cfg_json = names.tokens(&sc["config"]);
        let cfg = ForwarderConfig {
            host: cfg_json["host"].as_str().unwrap().into(),
            port: cfg_json["port"].as_u64().unwrap() as u16,
            listen_host: cfg_json["listen_host"].as_str().unwrap().into(),
            listen_port: cfg_json["listen_port"].as_u64().unwrap() as u16,
            endpoints: cfg_json["endpoints"]
                .as_array()
                .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
                .unwrap_or_default(),
            allow_commands: cfg_json["allow_commands"].as_bool().unwrap_or(false),
            ..ForwarderConfig::default()
        };
        let link = names.tokens(&sc["link"]);
        let link_address: Option<LinkAddress> = link.as_array().map(|pair| {
            let (host, port) = (pair[0].as_str().unwrap().to_string(), pair[1].as_u64().unwrap() as u16);
            Arc::new(move || Some((host.clone(), port))) as LinkAddress
        });
        let injected: Arc<Mutex<Vec<String>>> = Arc::default();
        let inject_result = sc.get("inject_result").and_then(Json::as_bool).unwrap_or(true);
        let inject: Inject = {
            let injected = Arc::clone(&injected);
            Arc::new(move |frame: &[u8]| {
                injected.lock().unwrap().push(to_hex(frame));
                inject_result
            })
        };
        let lines: Arc<Mutex<Vec<Json>>> = Arc::default();
        let logger: Logger = {
            let lines = Arc::clone(&lines);
            Arc::new(move |level: &str, text: &str| lines.lock().unwrap().push(json!([level, text])))
        };
        let clock = Arc::new(OffsetClock {
            base: Instant::now(),
            offset: Mutex::new(0.0),
        });
        let fwd = Forwarder::with_clock(cfg, Some(inject), link_address, logger, clock.clone());
        let mut failed = false;
        for (i, step) in sc["steps"].as_array().unwrap().iter().enumerate() {
            let before = lines.lock().unwrap().len();
            let op = step["op"].as_str().unwrap();
            let result = match op {
                "start" => {
                    let ok = fwd.start();
                    if ok {
                        names.0.insert("L".into(), fwd.listen_address().1);
                    }
                    json!(ok)
                }
                "stop" => {
                    fwd.stop();
                    Json::Null
                }
                "feed" => {
                    fwd.feed(&hex(step["raw"].as_str().unwrap()));
                    Json::Null
                }
                "status" => fwd.status(),
                "send" => {
                    let target = format!("127.0.0.1:{}", fwd.listen_address().1);
                    socks[step["peer"].as_str().unwrap()]
                        .send_to(&hex(step["data"].as_str().unwrap()), target)
                        .unwrap();
                    Json::Null
                }
                "recv" => {
                    let sock = &socks[step["peer"].as_str().unwrap()];
                    let mut got = Vec::new();
                    let mut buf = vec![0u8; 65535];
                    while let Ok((n, _)) = sock.recv_from(&mut buf) {
                        got.push(to_hex(&buf[..n]));
                    }
                    json!(got)
                }
                "wait" => {
                    let key = step["key"].as_str().unwrap();
                    let need = step["at_least"].as_u64().unwrap();
                    let deadline = Instant::now() + Duration::from_secs(3);
                    let mut ok = false;
                    while Instant::now() < deadline {
                        if status_value(&fwd, key) >= need {
                            ok = true;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    std::thread::sleep(Duration::from_millis(80));
                    json!(ok)
                }
                "settle" => {
                    std::thread::sleep(Duration::from_millis(300));
                    Json::Null
                }
                "injected" => json!(injected.lock().unwrap().clone()),
                "skip" => {
                    *clock.offset.lock().unwrap() += step["seconds"].as_f64().unwrap();
                    Json::Null
                }
                "parse" => parse_endpoint(step["text"].as_str().unwrap(), step["default"].as_u64().unwrap() as u16)
                    .map_or(Json::Null, |(h, p)| json!([h, p])),
                "split" => {
                    let bytes = hex(step["data"].as_str().unwrap());
                    let (frames, rest) = split_frames(&bytes);
                    json!({"frames": frames.iter().map(|f| to_hex(f)).collect::<Vec<_>>(), "rest": to_hex(rest)})
                }
                "source" => json!(frame_source_system(&hex(step["data"].as_str().unwrap()))),
                other => panic!("{other}"),
            };
            let want = &case["expected"]["steps"][i];
            let want_lines = want["log"].as_array().map_or(0, Vec::len);
            let deadline = Instant::now() + Duration::from_secs(3);
            while lines.lock().unwrap().len() < before + want_lines && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(20));
            let new_lines: Vec<Json> = lines.lock().unwrap()[before..].to_vec();
            let got = json!({"result": names.normalise(&result), "log": names.normalise(&Json::Array(new_lines))});
            if !same(&got, want) {
                failures.push(format!("{name} step {i} ({op}):\n  got  {got}\n  want {want}"));
                failed = true;
                break;
            }
        }
        fwd.stop();
        let _ = failed;
    }
    assert!(
        failures.is_empty(),
        "{} scenario(s) differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
