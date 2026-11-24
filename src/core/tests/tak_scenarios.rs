//! TAK inside a core: a scripted TAK server on a loopback socket, and the
//! routes an operator or an AI client uses. No hardware and no real network.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corvus_core::tak::cot::{Event, Framer};
use corvus_core::{Core, CoreBuilder, McpModule, Options as CoreOptions, TakModule, TakService, VehicleModule};
use serde_json::{json, Value};

fn dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("corvus-tak-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn core_in(dir: &std::path::Path) -> Core {
    CoreBuilder::new(CoreOptions::in_dir(dir))
        .module(VehicleModule::new())
        .module(McpModule::new())
        .module(TakModule::new())
        .build()
        .unwrap()
}

fn call(core: &Core, method: &str, path: &str, body: Value) -> (u16, Value) {
    core.internal_call(method, path, body.as_object().cloned())
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(8);
    while Instant::now() < end {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

/// A TAK server for one client: accepts it and reads its events.
struct Server {
    stream: TcpStream,
    framer: Framer,
    heard: Vec<Event>,
}

impl Server {
    fn accept(listener: &TcpListener) -> Server {
        listener.set_nonblocking(false).unwrap();
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        Server {
            stream,
            framer: Framer::default(),
            heard: Vec::new(),
        }
    }

    fn say(&mut self, xml: &str) {
        self.stream.write_all(xml.as_bytes()).unwrap();
    }

    /// Reads until an event of `kind` has come in.
    fn hear(&mut self, kind: &str) -> Event {
        let end = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(i) = self.heard.iter().position(|e| e.kind.starts_with(kind)) {
                return self.heard.remove(i);
            }
            assert!(Instant::now() < end, "no {kind} event arrived; heard {:?}", self.heard);
            let mut buf = [0u8; 4096];
            match self.stream.read(&mut buf) {
                Ok(0) => panic!("the client closed the connection"),
                Ok(n) => {
                    for text in self.framer.push(&buf[..n]).events {
                        self.heard.push(Event::parse(&text, 0.0).unwrap());
                    }
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(e) => panic!("{e}"),
            }
        }
    }

    /// Reads for `d` and says how many events came in.
    fn listen(&mut self, d: Duration) -> usize {
        let end = Instant::now() + d;
        let before = self.heard.len();
        while Instant::now() < end {
            let mut buf = [0u8; 4096];
            match self.stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for text in self.framer.push(&buf[..n]).events {
                        self.heard.push(Event::parse(&text, 0.0).unwrap());
                    }
                }
                Err(_) => {}
            }
        }
        self.heard.len() - before
    }

    /// Whether the client has closed its end.
    fn closed(&mut self) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            let mut buf = [0u8; 4096];
            match self.stream.read(&mut buf) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(_) => return true,
            }
        }
        false
    }
}

fn contact(uid: &str, callsign: &str, kind: &str, lat: f64, lon: f64) -> String {
    let now = corvus_core::tak::service::now();
    let t = corvus_core::tak::cot::format_time(now);
    let stale = corvus_core::tak::cot::format_time(now + 120.0);
    format!(
        r#"<event version="2.0" uid="{uid}" type="{kind}" how="m-g" time="{t}" start="{t}" stale="{stale}"><point lat="{lat}" lon="{lon}" hae="500" ce="9999999" le="9999999"/><detail><contact callsign="{callsign}"/><remarks>hello</remarks></detail></event>"#
    )
}

fn connect(core: &Core, listener: &TcpListener) -> (Server, String) {
    let port = listener.local_addr().unwrap().port();
    let (status, answer) = call(
        core,
        "POST",
        "/api/tak/connection",
        json!({"name": "Scripted server", "kind": "tcp", "host": "127.0.0.1", "port": port, "enabled": true}),
    );
    assert_eq!(status, 200, "{answer}");
    let id = answer["connection"]["id"].as_str().unwrap().to_string();
    let server = Server::accept(listener);
    wait("the connection to say it is connected", || {
        let (_, s) = call(core, "GET", "/api/tak", json!({}));
        s["connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == json!(id) && c["state"] == "connected")
    });
    (server, id)
}

#[test]
fn nothing_is_on_until_the_operator_switches_it_on() {
    let d = dir("off");
    let core = core_in(&d);
    core.start().unwrap();
    let (status, s) = call(&core, "GET", "/api/tak", json!({}));
    assert_eq!(status, 200);
    assert_eq!(s["share_position"], false);
    assert!(s["uid"].as_str().unwrap().starts_with("CORVUS-"));
    assert!(s["connections"].as_array().unwrap().iter().all(|c| c["state"] == "off"));
    let (status, e) = call(&core, "POST", "/api/tak/marker", json!({"lat": 48.0, "lon": 11.0}));
    assert_eq!(status, 409, "{e}");
    assert!(e["error"].as_str().unwrap().contains("switched on"), "{e}");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(d.join("config.json")).unwrap()).unwrap();
    assert_eq!(saved["tak"]["uid"], s["uid"], "the identity is kept");
    core.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn contacts_arrive_markers_leave_and_stop_closes_the_socket() {
    let d = dir("tcp");
    let core = core_in(&d);
    core.start().unwrap();
    let svc = core.service::<TakService>().unwrap();
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = Arc::clone(&seen);
    svc.add_listener(Arc::new(move |v: &Value| sink.lock().unwrap().push(v.clone())));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (mut server, id) = connect(&core, &listener);

    // What ATAK says arrives on the map.
    server.say(&contact("ANDROID-1", "ALPHA-1", "a-f-G-U-C", 48.0, 11.0));
    server.say(&contact("ANDROID-2", "BRAVO-2", "a-h-G", 48.001, 11.001));
    server.say(&contact("SPOT-1", "Rally", "b-m-p-s-p-i", 48.002, 11.002));
    wait("three contacts", || {
        call(&core, "GET", "/api/tak/tracks", json!({})).1["count"] == 3
    });
    let (_, list) = call(&core, "POST", "/api/tak/tracks", json!({"affiliation": "hostile"}));
    assert_eq!(list["count"], 1);
    assert_eq!(list["contacts"][0]["callsign"], "BRAVO-2");
    assert_eq!(list["contacts"][0]["source"], "Scripted server");
    let (_, near) = call(
        &core,
        "POST",
        "/api/tak/tracks",
        json!({"near_lat": 48.0, "near_lon": 11.0, "within_m": 150}),
    );
    let names: Vec<&str> = near["contacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["callsign"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["ALPHA-1", "BRAVO-2"], "nearest first, the far one left out");
    let (_, found) = call(&core, "GET", "/api/tak/tracks?query=rally", json!({}));
    assert_eq!(found["count"], 1);
    wait("a listener told of the contacts", || {
        seen.lock()
            .unwrap()
            .iter()
            .any(|u| u["upsert"].as_array().is_some_and(|a| !a.is_empty()))
    });

    // Chat, and a delete order.
    let chat = r#"<event version="2.0" uid="GeoChat.ANDROID-1.All Chat Rooms.7" type="b-t-f" how="h-g-i-g-o" time="2026-11-17T10:00:00Z" start="2026-11-17T10:00:00Z" stale="2026-11-17T10:01:00Z"><point lat="0" lon="0"/><detail><__chat chatroom="All Chat Rooms" senderCallsign="ALPHA-1"/><remarks>need a ride</remarks></detail></event>"#;
    server.say(chat);
    server.say(chat);
    wait("the chat line", || {
        call(&core, "GET", "/api/tak/chat", json!({})).1["messages"]
            .as_array()
            .unwrap()
            .len()
            == 1
    });
    let (_, m) = call(&core, "GET", "/api/tak/chat", json!({}));
    assert_eq!(m["messages"][0]["text"], "need a ride");
    assert_eq!(m["messages"][0]["from"], "ALPHA-1");
    server.say(r#"<event version="2.0" uid="del-1" type="t-x-d-d" how="m-g" time="2026-11-17T10:00:00Z" start="2026-11-17T10:00:00Z" stale="2026-11-17T10:01:00Z"><point lat="0" lon="0"/><detail><link uid="SPOT-1" type="none" relation="none"/></detail></event>"#);
    wait("the delete order to be obeyed", || {
        call(&core, "GET", "/api/tak/tracks", json!({})).1["count"] == 2
    });

    // Garbage costs a count and nothing else.
    server.say("<event uid=\"x\"><point lat=\"99\" lon=\"0\"/></event>");
    server.say("\u{0}\u{1}not xml at all </event>");
    server.say(&contact("ANDROID-3", "CHARLIE-3", "a-n-A", 48.1, 11.1));
    wait("the good one after the bad ones", || {
        call(&core, "GET", "/api/tak/tracks", json!({})).1["count"] == 3
    });
    let (_, s) = call(&core, "GET", "/api/tak", json!({}));
    let c = s["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == json!(id))
        .unwrap();
    assert_eq!(c["rejected"], 2);
    assert_eq!(c["received"], 7);

    // A marker goes out as ATAK expects it, and so does a chat line and a delete.
    let (status, sent) = call(
        &core,
        "POST",
        "/api/tak/marker",
        json!({"lat": 48.2, "lon": 11.2, "callsign": "LZ <1>", "remarks": "land & wait", "type": "b-m-p-s-p-i"}),
    );
    assert_eq!(status, 200, "{sent}");
    assert_eq!(sent["sent_to"], 1);
    let m = server.hear("b-m-p").clone();
    assert_eq!(m.uid, sent["uid"].as_str().unwrap());
    assert_eq!(m.callsign().as_deref(), Some("LZ <1>"));
    assert_eq!(m.remarks(), "land & wait");
    assert!((m.point.lat - 48.2).abs() < 1e-6);
    assert_eq!(call(&core, "GET", "/api/tak/tracks?query=LZ", json!({})).1["count"], 1);
    for bad in [
        json!({"lat": 91, "lon": 0}),
        json!({"lat": 1, "lon": 1, "type": "t-x-d-d"}),
        json!({"lat": 1, "lon": 1, "uid": "a\"b"}),
        json!({"lat": "x", "lon": 1}),
    ] {
        assert_eq!(call(&core, "POST", "/api/tak/marker", bad.clone()).0, 400, "{bad}");
    }
    assert_eq!(
        call(&core, "POST", "/api/tak/chat", json!({"text": "on my way"})).0,
        200
    );
    let c = server.hear("b-t-f");
    assert_eq!(c.chat().unwrap().2, "on my way");
    let uid = sent["uid"].as_str().unwrap();
    let (status, gone) = call(&core, "POST", "/api/tak/delete", json!({"uid": uid}));
    assert_eq!((status, gone["removed"].clone()), (200, json!(true)));
    assert_eq!(server.hear("t-x-d-d").deletes(), vec![uid.to_string()]);

    // Switching it off closes the socket; so does a stop.
    let (status, _) = call(
        &core,
        "POST",
        "/api/tak/connection",
        json!({"id": id, "enabled": false}),
    );
    assert_eq!(status, 200);
    assert!(server.closed(), "the connection was closed when it was switched off");
    let (_, again) = call(&core, "POST", "/api/tak/connection", json!({"id": id, "enabled": true}));
    assert_eq!(again["connection"]["enabled"], true);
    let mut server = Server::accept(&listener);
    core.stop();
    assert!(server.closed(), "stop closed the socket");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_aircraft_is_shared_only_when_allowed_and_as_a_friendly_drone() {
    let d = dir("share");
    let core = core_in(&d);
    core.start().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (mut server, _) = connect(&core, &listener);
    core.store().update([
        ("connected", json!(true)),
        ("position", json!([11.6385, 48.0795])),
        ("altitude_amsl", json!(560.0)),
        ("heading", json!(90.0)),
        ("groundspeed", json!(12.5)),
        ("battery_percent", json!(81.0)),
    ]);
    assert_eq!(
        server.listen(Duration::from_millis(1500)),
        0,
        "nothing is shared by default"
    );
    let (status, s) = call(
        &core,
        "POST",
        "/api/tak/settings",
        json!({"share_position": true, "share_interval_s": 1, "callsign": "Raven", "team": "Green"}),
    );
    assert_eq!(status, 200, "{s}");
    let own = server.hear("a-f-A-M-F-Q");
    assert_eq!(own.callsign().as_deref(), Some("Raven"));
    assert_eq!(own.group().unwrap().0, "Green");
    assert_eq!(own.track(), Some((90.0, 12.5)));
    assert_eq!(own.battery(), Some(81.0));
    assert!((own.point.lat - 48.0795).abs() < 1e-6 && (own.point.lon - 11.6385).abs() < 1e-6);
    let (_, st) = call(&core, "GET", "/api/tak", json!({}));
    assert_eq!(own.uid, st["uid"].as_str().unwrap());

    // Its own echo is not a contact.
    server.say(&own.to_xml());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(call(&core, "GET", "/api/tak/tracks", json!({})).1["count"], 0);

    // Switched off again, it stops.
    call(&core, "POST", "/api/tak/settings", json!({"share_position": false}));
    server.listen(Duration::from_millis(1500));
    assert_eq!(
        server.listen(Duration::from_millis(2500)),
        0,
        "it stopped when it was switched off"
    );
    core.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_udp_connection_hears_datagrams() {
    let d = dir("udp");
    let core = core_in(&d);
    core.start().unwrap();
    let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = {
        let p = probe.local_addr().unwrap().port();
        drop(probe);
        p
    };
    let (status, a) = call(
        &core,
        "POST",
        "/api/tak/connection",
        json!({"name": "Direct", "kind": "udp", "host": "127.0.0.1", "port": port, "enabled": true, "send": false}),
    );
    assert_eq!(status, 200, "{a}");
    wait("the socket to listen", || {
        call(&core, "GET", "/api/tak", json!({})).1["connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["state"] == "listening")
    });
    let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
    tx.send_to(
        contact("W-1", "WINTAK-1", "a-f-G-U-C", 48.0, 11.0).as_bytes(),
        ("127.0.0.1", port),
    )
    .unwrap();
    wait("the datagram", || {
        call(&core, "GET", "/api/tak/tracks", json!({})).1["count"] == 1
    });
    assert_eq!(
        call(&core, "POST", "/api/tak/marker", json!({"lat": 1, "lon": 1})).0,
        409,
        "it only listens"
    );
    core.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn connections_are_checked_and_removed() {
    let d = dir("conn");
    let core = core_in(&d);
    core.start().unwrap();
    for bad in [
        json!({"kind": "multicast", "host": "192.168.0.1", "port": 6969}),
        json!({"kind": "tcp", "host": "x y", "port": 80}),
        json!({"kind": "tcp", "host": "h", "port": 0}),
        json!({"id": "nope", "enabled": true}),
    ] {
        assert_eq!(call(&core, "POST", "/api/tak/connection", bad.clone()).0, 400, "{bad}");
    }
    let (status, added) = call(
        &core,
        "POST",
        "/api/tak/connection",
        json!({"name": "TLS", "kind": "tls", "host": "tak.example", "port": 8089}),
    );
    assert_eq!(status, 200);
    let id = added["connection"]["id"].as_str().unwrap();
    assert_eq!(added["connection"]["enabled"], false);
    assert_eq!(
        call(&core, "POST", "/api/tak/connection/remove", json!({"id": id})).0,
        200
    );
    assert_eq!(
        call(&core, "POST", "/api/tak/connection/remove", json!({"id": id})).0,
        404
    );
    assert_eq!(
        call(&core, "POST", "/api/tak/settings", json!({"team": "Plaid"})).0,
        400
    );
    core.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_tls_connection_to_a_plain_server_fails_with_a_reason_and_retries() {
    let d = dir("tls");
    let core = core_in(&d);
    core.start().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    call(
        &core,
        "POST",
        "/api/tak/connection",
        json!({"name": "TLS", "kind": "tls", "host": "127.0.0.1", "port": port, "enabled": true}),
    );
    let (mut plain, _) = listener.accept().unwrap();
    plain.write_all(b"this is not TLS\r\n").unwrap();
    wait("a reason", || {
        call(&core, "GET", "/api/tak", json!({})).1["connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["state"] == "error" && c["detail"].as_str().is_some_and(|t| !t.is_empty()))
    });
    core.stop();
    let _ = std::fs::remove_dir_all(d);
}

struct Ai {
    svc: Arc<corvus_core::mcp::McpService>,
    session: String,
    next: i64,
}

impl Ai {
    fn start(core: &Core) -> Ai {
        let svc = core.service::<corvus_core::mcp::McpService>().unwrap();
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "test client", "version": "1"}}});
        let (status, _, headers) = svc.handle_post(&init, None);
        assert_eq!(status, 200);
        let session = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("mcp-session-id"))
            .map(|(_, v)| v.clone())
            .expect("a session id");
        Ai { svc, session, next: 1 }
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
        let (status, reply, _) = self.svc.handle_post(&msg, Some(&self.session));
        assert_eq!(status, 200);
        reply.unwrap()
    }

    fn listed(&mut self) -> Vec<String> {
        let r = self.rpc("tools/list", json!({}));
        r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .filter(|n| n.starts_with("tak_"))
            .map(str::to_string)
            .collect()
    }

    /// `(is an error, the tool's answer)`.
    fn call(&mut self, name: &str, args: Value) -> (bool, Value) {
        let r = self.rpc("tools/call", json!({"name": name, "arguments": args}));
        let result = &r["result"];
        assert!(result.is_object(), "{name}: {r}");
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        (
            result["isError"] == true,
            serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
        )
    }

    fn ok(&mut self, name: &str, args: Value) -> Value {
        let (err, v) = self.call(name, args.clone());
        assert!(!err, "{name} {args} was refused: {v}");
        v
    }
}

#[test]
fn an_ai_client_gets_only_the_tak_tools_it_was_given_and_they_do_what_the_buttons_do() {
    let d = dir("mcp");
    let core = core_in(&d);
    core.start().unwrap();
    let mut ai = Ai::start(&core);
    let (status, _) = call(&core, "POST", "/api/mcp/settings", json!({"enabled": true}));
    assert_eq!(status, 200);
    assert!(ai.listed().is_empty(), "by default an agent sees no TAK tool");
    let (err, why) = ai.call("tak_status", json!({}));
    assert!(err, "{why}");

    let (status, _) = call(
        &core,
        "POST",
        "/api/mcp/settings",
        json!({"permissions": {"tak_read": true}}),
    );
    assert_eq!(status, 200);
    assert_eq!(ai.listed(), ["tak_status", "tak_list_contacts", "tak_get_chat"]);
    assert_eq!(ai.ok("tak_status", json!({}))["share_position"], false);
    assert!(
        ai.call("tak_send_marker", json!({"lat": 1, "lon": 1})).0,
        "sending is a switch of its own"
    );

    let (status, _) = call(
        &core,
        "POST",
        "/api/mcp/settings",
        json!({"permissions": {"tak_send": true, "tak_setup": true}, "confirm": {"tak_send": false, "tak_setup": false}}),
    );
    assert_eq!(status, 200);
    assert_eq!(ai.listed().len(), 10);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let added = ai.ok(
        "tak_add_connection",
        json!({"name": "Server", "kind": "tcp", "host": "127.0.0.1", "port": port, "enabled": true,
               "ca_file": "/etc/passwd", "key_file": "/etc/shadow"}),
    );
    assert_eq!(added["connection"]["ca_file"], "", "an agent cannot name files");
    let mut server = Server::accept(&listener);
    wait("connected", || {
        ai.ok("tak_status", json!({}))["connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["state"] == "connected")
    });

    server.say(&contact("A-1", "ALPHA", "a-f-G-U-C", 48.0, 11.0));
    server.say(&contact("H-1", "HOSTILE-7", "a-h-G", 48.01, 11.0));
    wait("two contacts", || ai.ok("tak_list_contacts", json!({}))["count"] == 2);
    let hostile = ai.ok("tak_list_contacts", json!({"affiliation": "hostile"}));
    assert_eq!(hostile["contacts"][0]["callsign"], "HOSTILE-7");
    let near = ai.ok(
        "tak_list_contacts",
        json!({"near_lat": 48.0, "near_lon": 11.0, "within_m": 500}),
    );
    assert_eq!(near["count"], 1);

    let sent = ai.ok(
        "tak_send_marker",
        json!({"lat": 48.5, "lon": 11.5, "callsign": "LZ", "type": "a-h-G"}),
    );
    let heard = server.hear("a-h-G");
    assert_eq!(heard.uid, sent["uid"].as_str().unwrap());
    assert!(
        ai.call("tak_send_marker", json!({"lat": 48.5, "lon": 11.5, "type": "t-x-d-d"}))
            .0
    );
    ai.ok("tak_send_chat", json!({"text": "contact north"}));
    assert_eq!(server.hear("b-t-f").chat().unwrap().2, "contact north");
    ai.ok("tak_delete_contact", json!({"uid": sent["uid"]}));
    assert_eq!(
        server.hear("t-x-d-d").deletes(),
        vec![sent["uid"].as_str().unwrap().to_string()]
    );

    let id = added["connection"]["id"].as_str().unwrap();
    ai.ok("tak_configure", json!({"share_position": true, "callsign": "Agent"}));
    assert_eq!(ai.ok("tak_status", json!({}))["callsign"], "Agent");
    ai.ok("tak_set_connection", json!({"id": id, "enabled": false}));
    assert!(server.closed());
    ai.ok("tak_remove_connection", json!({"id": id}));

    // Taking the switch away takes the tools away, and Stop takes the writing ones.
    let (status, _) = call(&core, "POST", "/api/mcp/stop", json!({}));
    assert_eq!(status, 200);
    assert_eq!(ai.listed(), ["tak_status", "tak_list_contacts", "tak_get_chat"]);
    core.stop();
    let _ = std::fs::remove_dir_all(d);
}
