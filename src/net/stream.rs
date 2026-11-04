//! A server-sent event stream on a thread of its own: `/api/events` topics
//! such as `console`, the way `Corvus.events` shares one connection in the
//! web interface.
//!
//! Reopened a second after it drops, for as long as it runs, and stopped
//! within a quarter second (the read timeout of `http`). A drop is handed to
//! the consumer as an `error` event, as the web stream reports one.

use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::net::http::{self, Server};
use crate::net::sse;

const POLL: Duration = Duration::from_millis(250);
const RECONNECT: Duration = Duration::from_secs(1);

/// The thread behind one stream.
pub struct Stream {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Stream {
    /// Follows `path` on `server`; `on_event` gets every event, and an
    /// `error` event with empty data each time the stream drops.
    pub fn start(server: Server, path: String, mut on_event: impl FnMut(sse::Event) + Send + 'static) -> Stream {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("events".into())
            .spawn(move || {
                let mut was_up = false;
                while !flag.load(Ordering::SeqCst) {
                    let up = follow(&server, &path, &flag, &mut on_event);
                    if (up || was_up) && !flag.load(Ordering::SeqCst) {
                        on_event(sse::Event {
                            name: "error".into(),
                            data: String::new(),
                        });
                    }
                    was_up = false;
                    let mut waited = Duration::ZERO;
                    while waited < RECONNECT && !flag.load(Ordering::SeqCst) {
                        std::thread::sleep(POLL);
                        waited += POLL;
                    }
                }
            })
            .expect("the events thread starts");
        Stream {
            stop,
            thread: Some(thread),
        }
    }

    /// Stops the thread and waits for it.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Reads one connection to its end; true when it was open at all.
fn follow(server: &Server, path: &str, stop: &AtomicBool, on_event: &mut dyn FnMut(sse::Event)) -> bool {
    let Ok(mut response) = http::request(server, "GET", path, None, "text/event-stream", POLL) else {
        return false;
    };
    if response.status != 200 {
        return false;
    }
    let mut parser = sse::Parser::default();
    let mut line = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        match response.body.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Err(_) => break,
        }
        if line.last() != Some(&b'\n') {
            continue;
        }
        line.pop();
        let text = String::from_utf8_lossy(&line).into_owned();
        line.clear();
        if let Some(event) = parser.line(&text) {
            if event.name != "ping" {
                on_event(event);
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::Mutex;

    #[test]
    fn events_arrive_and_a_drop_is_reported() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server::local(listener.local_addr().unwrap().port());
        let srv = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let body = "event: ping\ndata: {}\n\nevent: console\ndata: {\"text\": \"hello\"}\n\n";
            s.write_all(format!("HTTP/1.0 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{body}").as_bytes())
                .unwrap();
        });
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let mut stream = Stream::start(server, "/api/events?topics=console".into(), move |e| {
            s2.lock().unwrap().push(e)
        });
        srv.join().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while seen.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        stream.stop();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].name, "console", "ping is not handed on");
        assert_eq!(seen[1].name, "error");
    }
}
