//! The TAK service: the connections to a TAK network, the contacts heard on
//! them and what Corvus says back.
//!
//! One thread per switched on connection (UDP multicast or unicast, TCP, TLS)
//! reads whole CoT events, hands them to [`Inner::ingest`] and writes what is in
//! its outbox; one more thread tells the listeners what changed (at most four
//! times a second), shares the aircraft's position when the operator allowed it
//! and drops contacts that went stale. Every thread ends within a slice of
//! 100 ms of `stop`, except one that is inside a connect (3 s at most), and
//! every socket is closed with it.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};
use socket2::{Domain, Protocol, Socket, Type};

use super::config::{Connection, Settings};
use super::cot::{self, Event, Framer, Own};
use crate::store::VehicleStateStore;

const MAX_TRACKS: usize = 1000;
const MAX_CHAT: usize = 200;
const OUTBOX_MAX: usize = 200;
/// A contact stays listed (as stale) this long after its stale time.
pub const STALE_GRACE_S: f64 = 300.0;
const SLICE: Duration = Duration::from_millis(100);
const READ_SLICE: Duration = Duration::from_millis(250);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const FLUSH_EVERY_S: f64 = 0.25;
const PRUNE_EVERY_S: f64 = 5.0;
const OWN_STALE_FACTOR: f64 = 3.0;

pub type Listener = Arc<dyn Fn(&Value) + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

#[derive(Clone, Default)]
struct Status {
    state: &'static str,
    detail: String,
    since: f64,
    rx: u64,
    tx: u64,
    bad: u64,
}

#[derive(Default)]
struct Shared {
    status: Mutex<Status>,
    outbox: Mutex<VecDeque<String>>,
}

impl Shared {
    fn set(&self, state: &'static str, detail: &str) {
        let mut s = lock(&self.status);
        if s.state != state {
            s.since = now();
        }
        s.state = state;
        s.detail = detail.to_string();
    }

    fn count(&self, rx: u64, tx: u64, bad: u64) {
        let mut s = lock(&self.status);
        s.rx += rx;
        s.tx += tx;
        s.bad += bad;
    }

    fn push(&self, msg: String) {
        let mut q = lock(&self.outbox);
        if q.len() >= OUTBOX_MAX {
            q.pop_front();
        }
        q.push_back(msg);
    }

    fn pop(&self) -> Option<String> {
        lock(&self.outbox).pop_front()
    }
}

struct Link {
    spec: Connection,
    stop: Arc<AtomicBool>,
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl Link {
    fn is_up(&self) -> bool {
        matches!(lock(&self.shared.status).state, "connected" | "listening")
    }
}

struct Track {
    event: Event,
    source: String,
    received: f64,
}

#[derive(Default)]
struct Pending {
    upsert: BTreeSet<String>,
    remove: BTreeSet<String>,
    chat: Vec<Value>,
    status: bool,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.upsert.is_empty() && self.remove.is_empty() && self.chat.is_empty() && !self.status
    }
}

struct Inner {
    stop: AtomicBool,
    settings: Mutex<Settings>,
    tracks: Mutex<HashMap<String, Track>>,
    chat: Mutex<VecDeque<Value>>,
    links: Mutex<Vec<Link>>,
    retired: Mutex<Vec<JoinHandle<()>>>,
    pending: Mutex<Pending>,
    listeners: Mutex<Vec<(u64, Listener)>>,
    next_listener: AtomicU64,
    seq: AtomicU64,
    store: OnceLock<Arc<VehicleStateStore>>,
    last_own: Mutex<f64>,
}

/// What the module publishes as a service.
pub struct TakService {
    inner: Arc<Inner>,
    publisher: Mutex<Option<JoinHandle<()>>>,
}

impl Default for TakService {
    fn default() -> Self {
        TakService::new()
    }
}

impl TakService {
    pub fn new() -> TakService {
        TakService {
            inner: Arc::new(Inner {
                stop: AtomicBool::new(false),
                settings: Mutex::new(Settings::default()),
                tracks: Mutex::default(),
                chat: Mutex::default(),
                links: Mutex::default(),
                retired: Mutex::default(),
                pending: Mutex::default(),
                listeners: Mutex::default(),
                next_listener: AtomicU64::new(1),
                seq: AtomicU64::new(0),
                store: OnceLock::new(),
                last_own: Mutex::new(0.0),
            }),
            publisher: Mutex::new(None),
        }
    }

    /// Starts the publisher and the connections `settings` switches on.
    pub fn start(&self, settings: Settings, store: Arc<VehicleStateStore>) -> std::io::Result<()> {
        let _ = self.inner.store.set(store);
        self.inner.stop.store(false, Ordering::SeqCst);
        self.apply(settings);
        let inner = Arc::clone(&self.inner);
        let handle = std::thread::Builder::new()
            .name("tak-publisher".into())
            .spawn(move || inner.publish_loop())?;
        *lock(&self.publisher) = Some(handle);
        Ok(())
    }

    /// Ends every thread and closes every socket.
    pub fn stop(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        if let Some(h) = lock(&self.publisher).take() {
            let _ = h.join();
        }
        let links: Vec<Link> = std::mem::take(&mut *lock(&self.inner.links));
        for l in &links {
            l.stop.store(true, Ordering::SeqCst);
        }
        for mut l in links {
            if let Some(h) = l.handle.take() {
                let _ = h.join();
            }
        }
        for h in std::mem::take(&mut *lock(&self.inner.retired)) {
            let _ = h.join();
        }
    }

    pub fn settings(&self) -> Settings {
        lock(&self.inner.settings).clone()
    }

    /// Takes `settings` into force: connections that were switched off, removed
    /// or changed end, new and changed ones that are on start. Nothing blocks.
    pub fn apply(&self, settings: Settings) {
        let inner = &self.inner;
        *lock(&inner.settings) = settings.clone();
        let mut links = lock(&inner.links);
        let mut kept = Vec::new();
        for mut l in std::mem::take(&mut *links) {
            let same = settings.connections.iter().any(|c| c.enabled && *c == l.spec);
            if same && !inner.stop.load(Ordering::SeqCst) {
                kept.push(l);
            } else {
                l.stop.store(true, Ordering::SeqCst);
                if let Some(h) = l.handle.take() {
                    lock(&inner.retired).push(h);
                }
            }
        }
        if !inner.stop.load(Ordering::SeqCst) {
            for c in settings.connections.iter().filter(|c| c.enabled) {
                if !kept.iter().any(|l| l.spec == *c) {
                    kept.push(inner.spawn_link(c.clone()));
                }
            }
        }
        *links = kept;
        drop(links);
        lock(&inner.pending).status = true;
    }

    pub fn add_listener(&self, f: Listener) -> u64 {
        let id = self.inner.next_listener.fetch_add(1, Ordering::SeqCst);
        lock(&self.inner.listeners).push((id, f));
        id
    }

    pub fn remove_listener(&self, id: u64) {
        lock(&self.inner.listeners).retain(|(i, _)| *i != id);
    }

    /// Sequence number of the last update event, so a listener can see a gap.
    pub fn seq(&self) -> u64 {
        self.inner.seq.load(Ordering::SeqCst)
    }

    pub fn status(&self) -> Value {
        self.inner.status_json()
    }

    pub fn tracks(&self, q: &TrackQuery) -> Vec<Value> {
        self.inner.query(q)
    }

    pub fn chat(&self) -> Vec<Value> {
        lock(&self.inner.chat).iter().cloned().collect()
    }

    /// Puts `event` on every connection that may send. The number of
    /// connections it was queued for, or why it went nowhere.
    pub fn send(&self, event: &Event) -> Result<usize, String> {
        let n = self.inner.broadcast(&event.to_xml());
        if n == 0 {
            return Err("No TAK connection is switched on, connected and allowed to send. Switch one on and wait until it says Connected.".into());
        }
        Ok(n)
    }

    /// Records a marker this station made, so it is listed like the others.
    pub fn record_own_marker(&self, event: Event) {
        self.inner.insert(event, "corvus", now());
    }

    /// Forgets a contact now (the delete order goes out with `send`).
    pub fn forget(&self, uid: &str) -> bool {
        let gone = lock(&self.inner.tracks).remove(uid).is_some();
        if gone {
            lock(&self.inner.pending).remove.insert(uid.to_string());
        }
        gone
    }

    pub fn track(&self, uid: &str) -> Option<Value> {
        let t = lock(&self.inner.tracks);
        t.get(uid).map(|t| track_json(t, now()))
    }

    /// Feeds one event as if a connection had read it (tests and tools).
    pub fn ingest(&self, text: &str, source: &str) -> Result<(), String> {
        self.inner.ingest(text, source, now())
    }
}

#[derive(Default, Clone)]
pub struct TrackQuery {
    pub text: Option<String>,
    pub class: Option<String>,
    pub affiliation: Option<String>,
    pub include_stale: bool,
    pub near: Option<(f64, f64)>,
    pub within_m: Option<f64>,
    pub limit: usize,
}

fn track_json(t: &Track, now: f64) -> Value {
    let mut v = t.event.to_json(now);
    if let Some(o) = v.as_object_mut() {
        o.insert("source".into(), t.source.clone().into());
        o.insert("age_s".into(), ((now - t.received).max(0.0)).round().into());
    }
    v
}

/// Great circle distance in metres.
pub fn distance_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, la2) = (a.0.to_radians(), b.0.to_radians());
    let (dlat, dlon) = ((b.0 - a.0).to_radians(), (b.1 - a.1).to_radians());
    let h = (dlat / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlon / 2.0).sin().powi(2);
    6_371_000.0 * 2.0 * h.sqrt().min(1.0).asin()
}

impl Inner {
    fn halted(&self, own: &AtomicBool) -> bool {
        self.stop.load(Ordering::SeqCst) || own.load(Ordering::SeqCst)
    }

    /// Sleeps `d` in slices, and ends early when either flag is set.
    fn pause(&self, d: Duration, own: &AtomicBool) {
        let mut left = d;
        while !left.is_zero() && !self.halted(own) {
            let step = left.min(SLICE);
            std::thread::sleep(step);
            left -= step;
        }
    }

    fn spawn_link(self: &Arc<Inner>, spec: Connection) -> Link {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared::default());
        shared.set("connecting", "");
        let handle = {
            let (inner, spec, shared, stop) = (Arc::clone(self), spec.clone(), Arc::clone(&shared), Arc::clone(&stop));
            std::thread::Builder::new()
                .name(format!("tak-{}", spec.id))
                .spawn(move || inner.run_link(spec, shared, stop))
                .ok()
        };
        if handle.is_none() {
            shared.set("error", "The connection could not start a thread.");
        }
        Link {
            spec,
            stop,
            shared,
            handle,
        }
    }

    fn run_link(self: Arc<Inner>, spec: Connection, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
        let mut backoff = 2.0f64;
        while !self.halted(&stop) {
            shared.set("connecting", "");
            lock(&self.pending).status = true;
            let began = std::time::Instant::now();
            let result = match spec.kind.as_str() {
                "multicast" | "udp" => self.run_udp(&spec, &shared, &stop),
                _ => self.run_tcp(&spec, &shared, &stop),
            };
            if self.halted(&stop) {
                break;
            }
            if began.elapsed() > Duration::from_secs(10) {
                backoff = 2.0;
            }
            shared.set("error", &result.err().unwrap_or_else(|| "The connection ended.".into()));
            lock(&self.pending).status = true;
            self.pause(Duration::from_secs_f64(backoff), &stop);
            backoff = (backoff * 2.0).min(30.0);
        }
        shared.set("off", "");
        lock(&self.pending).status = true;
    }

    // ---- UDP ----------------------------------------------------------------

    fn run_udp(&self, spec: &Connection, shared: &Shared, stop: &AtomicBool) -> Result<(), String> {
        let sock = open_udp(spec)?;
        let dest = resolve(&spec.host, spec.port)?;
        shared.set("listening", &format!("{}:{}", spec.host, spec.port));
        lock(&self.pending).status = true;
        let mut buf = vec![0u8; 65_536];
        while !self.halted(stop) {
            match sock.recv_from(&mut buf) {
                Ok((n, _)) => {
                    let framed = Framer::default().push(&buf[..n]);
                    self.take_framed(framed, &spec.name, shared);
                }
                Err(e) if idle(&e) => {}
                Err(e) => return Err(format!("Reading failed: {e}")),
            }
            if spec.send {
                while let Some(msg) = shared.pop() {
                    match sock.send_to(msg.as_bytes(), dest) {
                        Ok(_) => shared.count(0, 1, 0),
                        Err(e) => return Err(format!("Sending failed: {e}")),
                    }
                }
            }
        }
        Ok(())
    }

    // ---- TCP and TLS --------------------------------------------------------

    fn run_tcp(&self, spec: &Connection, shared: &Shared, stop: &AtomicBool) -> Result<(), String> {
        let addr = resolve(&spec.host, spec.port)?;
        let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
            .map_err(|e| format!("Could not connect to {}:{}: {e}", spec.host, spec.port))?;
        let _ = tcp.set_nodelay(true);
        let keepalive = socket2::TcpKeepalive::new().with_time(Duration::from_secs(30));
        let _ = socket2::SockRef::from(&tcp).set_tcp_keepalive(&keepalive);
        let _ = tcp.set_write_timeout(Some(Duration::from_secs(3)));
        let mut conn = if spec.kind == "tls" {
            Conn::Tls(Box::new(super::tls::connect(spec, tcp)?))
        } else {
            Conn::Plain(tcp)
        };
        conn.set_read_timeout(READ_SLICE)?;
        shared.set("connected", &format!("{}:{}", spec.host, spec.port));
        lock(&self.pending).status = true;
        let (mut framer, mut buf) = (Framer::default(), vec![0u8; 16 * 1024]);
        while !self.halted(stop) {
            match conn.read(&mut buf) {
                Ok(0) => return Err("The server closed the connection.".into()),
                Ok(n) => {
                    let framed = framer.push(&buf[..n]);
                    self.take_framed(framed, &spec.name, shared);
                }
                Err(e) if idle(&e) => {}
                Err(e) => return Err(format!("Reading failed: {e}")),
            }
            if spec.send {
                while let Some(msg) = shared.pop() {
                    conn.write_all(msg.as_bytes())
                        .and_then(|_| conn.flush())
                        .map_err(|e| format!("Sending failed: {e}"))?;
                    shared.count(0, 1, 0);
                }
            }
        }
        Ok(())
    }

    fn take_framed(&self, framed: cot::Framed, source: &str, shared: &Shared) {
        let t = now();
        let mut bad = framed.dropped as u64;
        let mut good = 0;
        for text in &framed.events {
            match self.ingest(text, source, t) {
                Ok(()) => good += 1,
                Err(_) => bad += 1,
            }
        }
        shared.count(good, 0, bad);
    }

    // ---- what was heard -----------------------------------------------------

    fn own_uid(&self) -> String {
        lock(&self.settings).uid.clone()
    }

    /// One event from the network.
    fn ingest(&self, text: &str, source: &str, now: f64) -> Result<(), String> {
        let ev = Event::parse(text, now)?;
        let own = self.own_uid();
        if !own.is_empty() && (ev.uid == own || ev.uid.starts_with(&format!("GeoChat.{own}."))) {
            return Ok(());
        }
        let gone = ev.deletes();
        if !gone.is_empty() {
            let mut tracks = lock(&self.tracks);
            let mut p = lock(&self.pending);
            for uid in gone {
                if tracks.remove(&uid).is_some() {
                    p.remove.insert(uid);
                }
            }
            return Ok(());
        }
        if let Some((room, from, text)) = ev.chat() {
            let msg = json!({"uid": ev.uid, "time": cot::format_time(ev.time), "room": room, "from": from,
                "text": text, "source": source});
            let mut chat = lock(&self.chat);
            if chat.iter().any(|m| m["uid"] == msg["uid"]) {
                return Ok(());
            }
            if chat.len() >= MAX_CHAT {
                chat.pop_front();
            }
            chat.push_back(msg.clone());
            lock(&self.pending).chat.push(msg);
            return Ok(());
        }
        match ev.classify().class {
            "unit" | "point" | "route" | "drawing" | "alert" => self.insert(ev, source, now),
            _ => {}
        }
        Ok(())
    }

    fn insert(&self, ev: Event, source: &str, now: f64) {
        let uid = ev.uid.clone();
        let mut tracks = lock(&self.tracks);
        let mut p = lock(&self.pending);
        if !tracks.contains_key(&uid) && tracks.len() >= MAX_TRACKS {
            let oldest = tracks
                .iter()
                .min_by(|a, b| a.1.received.total_cmp(&b.1.received))
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                tracks.remove(&k);
                p.remove.insert(k);
            }
        }
        tracks.insert(
            uid.clone(),
            Track {
                event: ev,
                source: source.to_string(),
                received: now,
            },
        );
        p.remove.remove(&uid);
        p.upsert.insert(uid);
    }

    fn query(&self, q: &TrackQuery) -> Vec<Value> {
        let t = now();
        let needle = q.text.as_ref().map(|s| s.to_lowercase());
        let mut rows: Vec<(Option<f64>, String, Value)> = lock(&self.tracks)
            .values()
            .filter(|tr| {
                let k = tr.event.classify();
                (q.include_stale || tr.event.stale > t)
                    && q.class.as_deref().is_none_or(|c| c == k.class)
                    && q.affiliation.as_deref().is_none_or(|a| a == k.affiliation)
                    && needle.as_ref().is_none_or(|n| {
                        tr.event.callsign().unwrap_or_default().to_lowercase().contains(n)
                            || tr.event.uid.to_lowercase().contains(n)
                            || tr.event.remarks().to_lowercase().contains(n)
                    })
            })
            .filter_map(|tr| {
                let d = q.near.map(|n| distance_m(n, (tr.event.point.lat, tr.event.point.lon)));
                if q.within_m.zip(d).is_some_and(|(m, d)| d > m) {
                    return None;
                }
                let mut v = track_json(tr, t);
                if let (Some(d), Some(o)) = (d, v.as_object_mut()) {
                    o.insert("distance_m".into(), d.round().into());
                }
                Some((d, tr.event.callsign().unwrap_or_default().to_lowercase(), v))
            })
            .collect();
        rows.sort_by(|a, b| match (a.0, b.0) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            _ => a.1.cmp(&b.1),
        });
        let limit = if q.limit == 0 { 200 } else { q.limit.min(MAX_TRACKS) };
        rows.into_iter().take(limit).map(|r| r.2).collect()
    }

    fn status_json(&self) -> Value {
        let settings = lock(&self.settings).clone();
        let links = lock(&self.links);
        let rows: Vec<Value> = settings
            .connections
            .iter()
            .map(|c| {
                let live = links.iter().find(|l| l.spec == *c);
                let s = live.map(|l| lock(&l.shared.status).clone());
                let mut v = c.to_value();
                if let Some(o) = v.as_object_mut() {
                    let (state, detail) = match &s {
                        Some(s) => (s.state, s.detail.clone()),
                        None => ("off", String::new()),
                    };
                    o.insert("state".into(), state.into());
                    o.insert("detail".into(), detail.into());
                    o.insert("since".into(), s.as_ref().map_or(0.0, |s| s.since).into());
                    o.insert("received".into(), s.as_ref().map_or(0, |s| s.rx).into());
                    o.insert("sent".into(), s.as_ref().map_or(0, |s| s.tx).into());
                    o.insert("rejected".into(), s.as_ref().map_or(0, |s| s.bad).into());
                }
                v
            })
            .collect();
        drop(links);
        json!({
            "callsign": settings.callsign, "uid": settings.uid, "team": settings.team, "role": settings.role,
            "share_position": settings.share_position, "share_interval_s": settings.share_interval_s,
            "connections": rows,
            "contacts": lock(&self.tracks).len(),
            "seq": self.seq.load(Ordering::SeqCst),
        })
    }

    // ---- what is said -------------------------------------------------------

    /// Queues `xml` on every connection that is up and allowed to send; one that
    /// is still connecting would only deliver it late.
    fn broadcast(&self, xml: &str) -> usize {
        let links = lock(&self.links);
        let mut n = 0;
        for l in links.iter().filter(|l| l.spec.send && l.is_up()) {
            l.shared.push(xml.to_string());
            n += 1;
        }
        n
    }

    fn can_send(&self) -> bool {
        lock(&self.links).iter().any(|l| l.spec.send && l.is_up())
    }

    fn share_own(&self, now: f64) {
        let s = lock(&self.settings).clone();
        if !s.share_position || s.uid.is_empty() || !self.can_send() {
            return;
        }
        {
            let mut last = lock(&self.last_own);
            if now - *last < s.share_interval_s {
                return;
            }
            *last = now;
        }
        let Some(store) = self.store.get() else { return };
        if store.get("connected") != Some(Value::Bool(true)) {
            return;
        }
        let Some(pos) = store.get("position") else { return };
        let at = |i: usize| pos.get(i).and_then(Value::as_f64).filter(|v| v.is_finite());
        let (Some(lon), Some(lat)) = (at(0), at(1)) else { return };
        if (lat == 0.0 && lon == 0.0) || !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return;
        }
        let num = |k: &str| store.get(k).and_then(|v| v.as_f64()).filter(|v| v.is_finite());
        let ev = cot::position(
            &Own {
                uid: &s.uid,
                callsign: &s.callsign,
                team: &s.team,
                role: &s.role,
                lat,
                lon,
                hae: num("altitude_amsl"),
                course: num("heading"),
                speed: num("groundspeed"),
                battery: num("battery_percent"),
                stale_s: (s.share_interval_s * OWN_STALE_FACTOR).max(10.0),
            },
            now,
        );
        self.broadcast(&ev.to_xml());
    }

    // ---- the publisher thread -----------------------------------------------

    fn publish_loop(self: Arc<Inner>) {
        let never = AtomicBool::new(false);
        let (mut last_flush, mut last_prune) = (0.0, 0.0);
        while !self.stop.load(Ordering::SeqCst) {
            let t = now();
            self.share_own(t);
            if t - last_prune >= PRUNE_EVERY_S {
                last_prune = t;
                self.prune(t);
                self.reap();
            }
            if t - last_flush >= FLUSH_EVERY_S {
                last_flush = t;
                self.flush(t);
            }
            self.pause(SLICE, &never);
        }
    }

    fn prune(&self, now: f64) {
        let mut tracks = lock(&self.tracks);
        let dead: Vec<String> = tracks
            .iter()
            .filter(|(_, t)| now > t.event.stale + STALE_GRACE_S)
            .map(|(k, _)| k.clone())
            .collect();
        if dead.is_empty() {
            return;
        }
        let mut p = lock(&self.pending);
        for k in dead {
            tracks.remove(&k);
            p.remove.insert(k);
        }
    }

    /// Joins the threads of connections that were switched off.
    fn reap(&self) {
        let mut retired = lock(&self.retired);
        let mut i = 0;
        while i < retired.len() {
            if retired[i].is_finished() {
                let _ = retired.swap_remove(i).join();
            } else {
                i += 1;
            }
        }
    }

    fn flush(&self, now: f64) {
        let p = std::mem::take(&mut *lock(&self.pending));
        if p.is_empty() {
            return;
        }
        let upsert: Vec<Value> = {
            let tracks = lock(&self.tracks);
            p.upsert
                .iter()
                .filter_map(|u| tracks.get(u))
                .map(|t| track_json(t, now))
                .collect()
        };
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let event = json!({
            "type": "update", "seq": seq, "upsert": upsert, "remove": p.remove, "chat": p.chat,
            "status": self.status_json(),
        });
        let listeners: Vec<Listener> = lock(&self.listeners).iter().map(|(_, f)| Arc::clone(f)).collect();
        for f in listeners {
            f(&event);
        }
    }
}

fn idle(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
    )
}

fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("Could not find {host}: {e}"))?
        .find(|a| a.is_ipv4())
        .or_else(|| (host, port).to_socket_addrs().ok().and_then(|mut a| a.next()))
        .ok_or_else(|| format!("Could not find {host}"))
}

fn open_udp(spec: &Connection) -> Result<UdpSocket, String> {
    let fail = |what: &str, e: std::io::Error| format!("{what}: {e}");
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(|e| fail("No socket", e))?;
    sock.set_reuse_address(true).map_err(|e| fail("No socket", e))?;
    #[cfg(unix)]
    let _ = sock.set_reuse_port(true);
    let bind: SocketAddr = (Ipv4Addr::UNSPECIFIED, spec.port).into();
    sock.bind(&bind.into())
        .map_err(|e| fail(&format!("Port {} is not free", spec.port), e))?;
    if spec.kind == "multicast" {
        let group: Ipv4Addr = spec
            .host
            .parse()
            .map_err(|_| format!("{} is not a group address", spec.host))?;
        sock.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)
            .map_err(|e| fail("Could not join the group", e))?;
        let _ = sock.set_multicast_loop_v4(true);
        let _ = sock.set_multicast_ttl_v4(1);
    } else {
        let _ = sock.set_broadcast(true);
    }
    sock.set_read_timeout(Some(READ_SLICE))
        .map_err(|e| fail("No socket", e))?;
    Ok(sock.into())
}

/// A TCP stream, with or without TLS on it.
pub enum Conn {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Conn {
    fn set_read_timeout(&self, d: Duration) -> Result<(), String> {
        let tcp = match self {
            Conn::Plain(t) => t,
            Conn::Tls(t) => &t.sock,
        };
        tcp.set_read_timeout(Some(d)).map_err(|e| e.to_string())
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(t) => t.read(buf),
            Conn::Tls(t) => t.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(t) => t.write(buf),
            Conn::Tls(t) => t.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Plain(t) => t.flush(),
            Conn::Tls(t) => t.flush(),
        }
    }
}
