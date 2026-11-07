//! The protocol host over a `corvus_link::plain` connection: what an engine
//! needs of a bridge, answered by one link and a few flags. The Vehicle area's
//! bridge grows from this; an engine's tests and a bench tool use it as is.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use corvus_link::plain::connection::Connection;
use corvus_link::plain::Message;

use super::params::{ParamEngine, ParamHost, Status};
use super::RequestHost;
use crate::clock;

/// One link, its target vehicle and the state the parameter engine reads.
pub struct LinkHost {
    conn: Arc<Connection>,
    target: (u8, u8),
    encoding: Mutex<String>,
    armed: AtomicBool,
    stop: AtomicBool,
    slow: bool,
    error: Mutex<String>,
    listeners: Mutex<Vec<Listener>>,
    console: Mutex<Vec<(String, String, String)>>,
    engine: Mutex<Option<Arc<ParamEngine>>>,
    watchdog: Mutex<Option<JoinHandle<()>>>,
    me: Mutex<Option<std::sync::Weak<LinkHost>>>,
}

type Listener = Box<dyn Fn(&Status) + Send>;

impl LinkHost {
    pub fn new(conn: Arc<Connection>, target: (u8, u8), encoding: &str, slow: bool) -> Arc<LinkHost> {
        let host = Arc::new(LinkHost {
            conn,
            target,
            encoding: Mutex::new(encoding.to_string()),
            armed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            slow,
            error: Mutex::new(String::new()),
            listeners: Mutex::new(Vec::new()),
            console: Mutex::new(Vec::new()),
            engine: Mutex::new(None),
            watchdog: Mutex::new(None),
            me: Mutex::new(None),
        });
        *host.me.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(&host));
        host
    }

    /// The engine whose watchdog this host starts.
    pub fn bind(&self, engine: Arc<ParamEngine>) {
        *self.engine.lock().unwrap_or_else(|e| e.into_inner()) = Some(engine);
    }

    pub fn set_armed(&self, armed: bool) {
        self.armed.store(armed, Ordering::SeqCst);
    }

    pub fn set_encoding(&self, encoding: &str) {
        *self.encoding.lock().unwrap_or_else(|e| e.into_inner()) = encoding.to_string();
    }

    pub fn add_listener(&self, f: impl Fn(&Status) + Send + 'static) {
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Box::new(f));
    }

    pub fn console_lines(&self) -> Vec<(String, String, String)> {
        self.console.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Feeds what the link delivered to the engine: every PARAM_VALUE.
    /// Returns how many frames were read.
    pub fn pump(self: &Arc<Self>, engine: &ParamEngine, timeout: Duration) -> std::io::Result<usize> {
        let got = self.conn.recv(timeout)?;
        for r in &got {
            if let Ok(m) = &r.message {
                if m.name() == "PARAM_VALUE" {
                    let raw = r.frame.payload.get(..4).map(|b| b.to_vec());
                    engine.handle_param_value(self.as_ref(), m, raw.as_deref());
                }
            }
        }
        Ok(got.len())
    }

    /// Stops the watchdog thread and waits for it. Idempotent.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let handle = self.watchdog.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = handle {
            let _ = h.join();
        }
    }
}

impl RequestHost for LinkHost {
    fn connection_ready(&self) -> bool {
        !self.conn.is_closed()
    }

    fn armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    fn send(&self, msg: Message) -> Result<bool, String> {
        if self.conn.is_closed() {
            return Ok(false);
        }
        self.conn.send(&msg).map(|_| true).map_err(|e| e.to_string())
    }

    fn target(&self) -> (u8, u8) {
        self.target
    }

    fn set_error(&self, text: &str) {
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = text.to_string();
    }

    fn last_error(&self) -> String {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn console(&self, source: &str, text: &str, level: &str) {
        self.console
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((source.into(), text.into(), level.into()));
    }
}

impl ParamHost for LinkHost {
    fn now(&self) -> f64 {
        clock::monotonic()
    }

    fn sleep(&self, seconds: f64) {
        std::thread::sleep(Duration::from_secs_f64(seconds.max(0.0)));
    }

    fn interruptible_sleep(&self, seconds: f64) {
        let end = self.now() + seconds;
        while self.now() < end && !self.stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs_f64((end - self.now()).clamp(0.0, 0.05)));
        }
    }

    fn running(&self) -> bool {
        !self.stop.load(Ordering::SeqCst)
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn encoding(&self) -> String {
        self.encoding.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn slow_link(&self) -> bool {
        self.slow
    }

    fn notify(&self, status: &Status) {
        for f in self.listeners.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            f(status);
        }
    }

    fn start_watchdog(&self) {
        if self.stop.load(Ordering::SeqCst) {
            return;
        }
        let mut slot = self.watchdog.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let engine = self.engine.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let me = self
            .me
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        if let (Some(engine), Some(me)) = (engine, me) {
            *slot = std::thread::Builder::new()
                .name("param-watchdog".into())
                .spawn(move || engine.run_watchdog(me.as_ref()))
                .ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    use corvus_link::plain::connection::{open, Options};
    use corvus_link::plain::{defs::Wire, Value};

    use super::*;

    fn free_port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn param_value(name: &str, value: f64, index: i128, count: i128) -> Message {
        Message::named("PARAM_VALUE", Wire::V2)
            .unwrap()
            .with("param_id", Value::Bytes(name.as_bytes().to_vec()))
            .with("param_value", Value::Float(value))
            .with("param_type", Value::Int(9))
            .with("param_index", Value::Int(index))
            .with("param_count", Value::Int(count))
    }

    /// A vehicle on the other end of a real UDP link: answers the list, a
    /// read by name or index, and a set. `skip_first_list` loses the first
    /// answer to a list request for index 2, to make the watchdog earn its keep.
    fn vehicle(port: u16, stop: Arc<AtomicBool>, lost_once: bool) -> JoinHandle<()> {
        let conn = open(
            &format!("udpout:127.0.0.1:{port}"),
            &Options {
                source_system: 1,
                source_component: 1,
                ..Options::default()
            },
        )
        .unwrap();
        std::thread::spawn(move || {
            let mut params = [("A", 1.5), ("B", 2.5), ("C", 3.5), ("D", 4.5), ("E", 5.5)];
            let hb = Message::named("HEARTBEAT", Wire::V2).unwrap();
            let _ = conn.send(&hb);
            let mut list_requests = 0;
            while !stop.load(Ordering::SeqCst) {
                let Ok(got) = conn.recv(Duration::from_millis(50)) else {
                    break;
                };
                for r in got {
                    let Ok(m) = r.message else { continue };
                    let text = |f: &str| match m.get(f) {
                        Some(Value::Bytes(b)) => {
                            String::from_utf8_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())])
                                .into_owned()
                        }
                        _ => String::new(),
                    };
                    match m.name() {
                        "PARAM_REQUEST_LIST" => {
                            list_requests += 1;
                            for (i, (n, v)) in params.iter().enumerate() {
                                if !(lost_once && list_requests == 1 && i == 2) {
                                    let _ = conn.send(&param_value(n, *v, i as i128, params.len() as i128));
                                }
                            }
                        }
                        "PARAM_REQUEST_READ" => {
                            let name = text("param_id");
                            let index = m.get("param_index").and_then(Value::as_i64).unwrap_or(-1);
                            for (i, (n, v)) in params.iter().enumerate() {
                                if (!name.is_empty() && *n == name) || (name.is_empty() && i as i64 == index) {
                                    let _ = conn.send(&param_value(n, *v, i as i128, params.len() as i128));
                                }
                            }
                        }
                        "PARAM_SET" => {
                            let name = text("param_id");
                            let value = m.get("param_value").and_then(Value::as_f64).unwrap_or(0.0);
                            if let Some(i) = params.iter().position(|(n, _)| *n == name) {
                                params[i].1 = value;
                                let _ = conn.send(&param_value(params[i].0, value, i as i128, params.len() as i128));
                            }
                        }
                        _ => {}
                    }
                }
            }
        })
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end {
            if ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}");
    }

    #[test]
    fn the_engine_downloads_writes_and_verifies_over_a_real_link() {
        for lost in [false, true] {
            let port = free_port();
            let gcs = Arc::new(open(&format!("udpin:127.0.0.1:{port}"), &Options::default()).unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let veh = vehicle(port, stop.clone(), lost);
            // The vehicle announces itself so the ground station knows where to send.
            wait_for("the vehicle's heartbeat", || {
                let _ = gcs.recv(Duration::from_millis(50));
                !gcs.systems().is_empty()
            });
            let host = LinkHost::new(gcs.clone(), (1, 1), "bytewise", false);
            let engine = Arc::new(ParamEngine::new());
            host.bind(engine.clone());
            let completed = Arc::new(AtomicUsize::new(0));
            let c = completed.clone();
            host.add_listener(move |s| {
                if s.state == "complete" {
                    c.fetch_add(1, Ordering::SeqCst);
                }
            });
            let pump_stop = Arc::new(AtomicBool::new(false));
            let pump = {
                let (host, engine, stop) = (host.clone(), engine.clone(), pump_stop.clone());
                std::thread::spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        if host.pump(&engine, Duration::from_millis(30)).is_err() {
                            break;
                        }
                    }
                })
            };

            assert!(engine.request_param_list(host.as_ref()));
            wait_for("the download", || engine.get_state() == "complete");
            assert_eq!((engine.count(), engine.received()), (5, 5));
            assert_eq!(engine.entry("C").unwrap().value, 3.5);
            assert!(completed.load(Ordering::SeqCst) >= 1);

            assert!(engine.set_param(host.as_ref(), "B", 9.25), "{}", host.last_error());
            assert_eq!(engine.entry("B").unwrap().value, 9.25);
            assert_eq!(host.console_lines().last().unwrap().1, "B set to 9.25");

            let report = engine
                .verify_params(host.as_ref(), &[("A".into(), 1.5), ("D".into(), 8.0)], None, true, 4.0)
                .unwrap();
            assert!(report.all_confirmed, "{report:?}");
            assert!(report.results[1].rewritten);

            host.set_armed(true);
            assert!(!engine.set_param(host.as_ref(), "A", 2.0));
            assert_eq!(host.last_error(), "cannot set parameter while armed");

            pump_stop.store(true, Ordering::SeqCst);
            stop.store(true, Ordering::SeqCst);
            host.stop();
            pump.join().unwrap();
            veh.join().unwrap();
        }
    }
}
