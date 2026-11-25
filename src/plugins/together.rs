//! Map Entities and TAK, the real plugins, through the real registry, over HTTP
//! and SSE into a real core, out to a scripted TAK server on a loopback socket. TAK is
//! both modules as the store installs them.
//! No window, no hardware, no ATAK: what ATAK would receive is what the server
//! here reads.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use corvus_core::tak::cot::{Event, Framer};
use eframe::egui;
use serde_json::{json, Value};

use super::wasm::testlib;
use super::Registry;
use crate::net::http::{self, Server};

struct Peer {
    stream: TcpStream,
    framer: Framer,
    heard: Vec<Event>,
}

impl Peer {
    fn accept(listener: &TcpListener) -> Peer {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_millis(40))).unwrap();
        Peer {
            stream,
            framer: Framer::default(),
            heard: Vec::new(),
        }
    }

    fn read(&mut self) {
        let mut buf = [0u8; 8192];
        if let Ok(n) = self.stream.read(&mut buf) {
            for text in self.framer.push(&buf[..n]).events {
                if let Ok(e) = Event::parse(&text, 0.0) {
                    self.heard.push(e);
                }
            }
        }
    }

    fn markers(&self) -> Vec<&Event> {
        self.heard.iter().filter(|e| e.uid.starts_with("corvus.me.")).collect()
    }
}

struct Rig {
    ctx: egui::Context,
    reg: Registry,
    peer: Peer,
    _core: corvus_core::Core,
    dir: std::path::PathBuf,
}

impl Rig {
    /// Runs the registry and the server's reading until `ok` or a timeout.
    fn pump(&mut self, what: &str, mut ok: impl FnMut(&Peer) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            self.reg.update(&self.ctx, &json!({}));
            self.peer.read();
            if ok(&self.peer) {
                return;
            }
        }
        panic!("timed out waiting for {what}; heard {:?}", self.peer.heard);
    }

    /// Runs for `secs` and returns what was heard meanwhile.
    fn quiet(&mut self, secs: f64) -> usize {
        let before = self.peer.heard.len();
        let end = Instant::now() + Duration::from_secs_f64(secs);
        while Instant::now() < end {
            self.reg.update(&self.ctx, &json!({}));
            self.peer.read();
        }
        self.peer.heard.len() - before
    }

    fn option(&mut self, plugin: &str, key: &str, on: bool) {
        let i = self.reg.index_of(plugin).unwrap();
        self.reg.set_option(&self.ctx, &json!({}), i, key, &json!(on)).unwrap();
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const ENTITIES: &str = r##"[
    {"id": "g1", "kind": "gate", "name": "Gate 1", "color": "#FFD60A", "lng": 11.6432, "lat": 48.0762,
     "lng2": 11.6435, "lat2": 48.0762, "base": 0, "height": 5, "note": "Fly through"},
    {"id": "t1", "kind": "tree", "name": "Old oak", "color": "#7BD389", "lng": 11.644, "lat": 48.077,
     "radius": 4, "alert": true, "height": 14, "note": ""},
    {"id": "p1", "kind": "person", "name": "Pilot", "color": "#1E8E3E", "lng": 11.645, "lat": 48.078,
     "radius": 0, "alert": false, "angle": 360, "bearing": 0, "note": ""}
]"##;

/// A core with the TAK module and a connection to a scripted server, and the
/// registry of `plugins` (all bundled ones when `None`) on top of it. `shared`
/// and `sends` are the two options.
fn rig(tag: &str, only_tak: bool, shared: bool, sends: bool) -> Rig {
    let dir = std::env::temp_dir().join(format!("corvus-together-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let core = corvus_core::CoreBuilder::new(corvus_core::Options::in_dir(&dir))
        .module(corvus_core::PluginsModule::new())
        .module(corvus_core::TakModule::new())
        .module(corvus_core::Http::new(corvus_core::HttpOptions {
            port: 0,
            ..corvus_core::HttpOptions::default()
        }))
        .build()
        .unwrap();
    core.start().unwrap();
    let port = core
        .service::<corvus_core::HttpInfo>()
        .and_then(|i| i.addr())
        .map(|a| a.port())
        .unwrap();
    let mut server = Server::local(port);
    server.key = core
        .service::<corvus_core::ApiKey>()
        .and_then(|k| http::Key::new(k.secret()));
    let save = |id: &str, settings: Value| {
        http::post_json(
            &server,
            "/api/plugins/settings",
            &json!({"id": id, "settings": settings}),
        )
        .unwrap();
    };
    save(
        "map-entities",
        json!({"entities": serde_json::from_str::<Value>(ENTITIES).unwrap(), "_options": {"share": shared}}),
    );
    save("tak", json!({"_options": {"entities": sends}}));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let answer = http::post_json(
        &server,
        "/api/tak/connection",
        &json!({"name": "Scripted", "kind": "tcp", "host": "127.0.0.1",
            "port": listener.local_addr().unwrap().port(), "enabled": true}),
    )
    .unwrap();
    assert!(answer["connection"]["id"].is_string(), "{answer}");
    let peer = Peer::accept(&listener);
    let ctx = egui::Context::default();
    let packages = dir.join("plugins-wasm");
    std::fs::create_dir_all(&packages).unwrap();
    testlib::package(&packages, "tak", "corvus_plugin_tak");
    if !only_tak {
        testlib::package(&packages, "map-entities", "corvus_plugin_map_entities");
    }
    let mut reg = Registry::with_packages(&testlib::sandbox(), server, &ctx, &json!({}), Vec::new(), &packages);
    reg.finish_preparing();
    assert_eq!(reg.refused, []);
    Rig {
        ctx,
        reg,
        peer,
        _core: core,
        dir,
    }
}

fn uids(p: &Peer) -> Vec<String> {
    let mut u: Vec<String> = p.markers().iter().map(|e| e.uid.clone()).collect();
    u.sort();
    u.dedup();
    u
}

#[test]
fn both_plugins_on_the_entities_reach_the_network_and_leave_it_again() {
    let mut r = rig("both", false, true, true);
    r.pump("three markers", |p| uids(p).len() == 3);
    assert_eq!(uids(&r.peer), ["corvus.me.g1", "corvus.me.p1", "corvus.me.t1"]);
    let find = |r: &Rig, uid: &str| r.peer.markers().into_iter().find(|e| e.uid == uid).cloned().unwrap();
    let gate = find(&r, "corvus.me.g1");
    assert_eq!(gate.kind, "b-m-p-s-p-i");
    assert_eq!(gate.callsign().as_deref(), Some("Gate 1"));
    assert!((gate.point.lon - 11.64335).abs() < 1e-6, "{}", gate.point.lon);
    assert!(
        gate.remarks().starts_with("Gate, 22 m wide, 5.0 m high"),
        "{}",
        gate.remarks()
    );
    assert!(gate.remarks().ends_with("Fly through"));
    assert_eq!(find(&r, "corvus.me.p1").kind, "a-f-G-U-C");
    let tree = find(&r, "corvus.me.t1");
    assert_eq!(tree.remarks(), "Tree, crown radius 4.0 m, 14 m high");

    // Nothing is sent twice while nothing changes.
    let n = r.peer.heard.len();
    assert_eq!(
        r.quiet(1.0),
        0,
        "an unchanged list is not sent again ({n} heard so far)"
    );

    // Switching the share off in Map Entities takes every marker back.
    r.option("map-entities", "share", false);
    r.pump("three delete orders", |p| {
        p.heard.iter().filter(|e| e.kind.starts_with("t-x-d-d")).count() >= 3
    });
    let mut gone: Vec<String> = r.peer.heard.iter().flat_map(Event::deletes).collect();
    gone.sort();
    gone.dedup();
    assert_eq!(gone, ["corvus.me.g1", "corvus.me.p1", "corvus.me.t1"]);

    // And on again sends them anew; switching TAK's own option off takes them back.
    let before = r.peer.markers().len();
    r.option("map-entities", "share", true);
    r.pump("the markers again", |p| p.markers().len() >= before + 3);
    r.option("tak", "entities", false);
    let deletes_before = r.peer.heard.iter().filter(|e| e.kind.starts_with("t-x-d-d")).count();
    r.pump("a second round of delete orders", |p| {
        p.heard.iter().filter(|e| e.kind.starts_with("t-x-d-d")).count() >= deletes_before + 3
    });
}

#[test]
fn each_plugin_works_alone_and_nothing_is_sent_unless_both_agree() {
    // TAK sends when asked, but Map Entities is not installed at all.
    let mut r = rig("alone", true, true, true);
    assert_eq!(r.quiet(1.5), 0, "without Map Entities nothing is sent");
    drop(r);
    // Map Entities shares, TAK is not asked to send.
    let mut r = rig("tak-off", false, true, false);
    assert_eq!(r.quiet(1.5), 0, "TAK sends nothing it was not asked to");
    drop(r);
    // TAK is asked, Map Entities does not share.
    let mut r = rig("share-off", false, false, true);
    assert_eq!(r.quiet(1.5), 0, "Map Entities shares nothing it was not asked to");
}

#[test]
fn a_contact_handed_over_becomes_a_saved_entity_and_goes_back_out_as_a_marker() {
    let mut r = rig("handover", false, true, true);
    r.pump("the first markers", |p| uids(p).len() == 3);
    // What the TAK plugin's `entity_for_contact` makes of a hostile unit. The crate itself is not linked
    // here: two plugin rlibs in one binary export the same `CORVUS_PLUGIN` symbols.
    assert!(r.reg.bus.send(
        "tak",
        "map-entities",
        json!({"op": "add", "kind": "target", "name": "HOSTILE 1", "lng": 11.6441, "lat": 48.0765,
            "note": "From TAK, a-h-G. by the road"})
    ));
    r.pump("a fourth marker", |p| uids(p).len() == 4);
    let new = uids(&r.peer)
        .into_iter()
        .find(|u| !["corvus.me.g1", "corvus.me.p1", "corvus.me.t1"].contains(&u.as_str()))
        .unwrap();
    let marker = r.peer.markers().into_iter().find(|e| e.uid == new).cloned().unwrap();
    assert_eq!(marker.callsign().as_deref(), Some("HOSTILE 1"));
    assert_eq!(
        marker.kind, "a-u-G",
        "a copy is a target, not a claim that it is hostile"
    );
    assert!(marker.remarks().contains("by the road"), "{}", marker.remarks());
    let saved = http::get_json(r.reg.server(), "/api/plugins").unwrap();
    let list = saved["settings"]["map-entities"]["entities"].as_array().unwrap();
    assert_eq!(list.len(), 4, "the copy was saved");
    assert!(list.iter().any(|e| e["name"] == "HOSTILE 1" && e["kind"] == "target"));
}
