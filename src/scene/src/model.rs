//! A script in the place of a language model, for the AI Chat picture: it asks
//! for two drawing tools and then says what it drew. The tools are the
//! station's own, what they draw is real.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

pub const PORT: u16 = 8979;

pub struct Model {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Model {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn start() -> Result<Model, String> {
    let listener = TcpListener::bind(("127.0.0.1", PORT)).map_err(|e| format!("port {PORT}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((c, _)) => {
                    let _ = answer(c);
                }
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    });
    Ok(Model {
        stop,
        thread: Some(thread),
    })
}

fn call(name: &str, args: Value) -> Value {
    json!({"choices": [{"finish_reason": "tool_calls", "message": {"role": "assistant", "content": "",
        "tool_calls": [{"id": name, "type": "function",
            "function": {"name": name, "arguments": args.to_string()}}]}}]})
}

fn reply(asked: &Value) -> Value {
    let answers = asked["messages"].as_array().into_iter().flatten();
    match answers.filter(|m| m["role"] == "tool").count() {
        0 => call(
            "draw_on_map",
            json!({"shapes": [
                {"kind": "polygon", "id": "area", "color": "yellow", "text": "Search area", "points": [
                    {"lat": 48.07655, "lon": 11.6421}, {"lat": 48.07672, "lon": 11.6439},
                    {"lat": 48.07612, "lon": 11.64405}, {"lat": 48.07594, "lon": 11.6422}]},
                {"kind": "circle", "id": "keep", "lat": 48.0762, "lon": 11.64135, "radius_m": 22, "color": "red",
                    "text": "Keep out"}]}),
        ),
        1 => call("list_drawings", json!({})),
        _ => json!({"choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content":
            "Drawn on your map:\n\n- **Search area**, in yellow, over the field\n- **Keep out**, 22 m around the trees\n\nNothing flies before you allow it."}}]}),
    }
}

fn answer(conn: TcpStream) -> std::io::Result<()> {
    conn.set_nonblocking(false)?;
    conn.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut r = BufReader::new(conn.try_clone()?);
    let (mut first, mut length) = (String::new(), 0usize);
    r.read_line(&mut first)?;
    loop {
        let mut l = String::new();
        if r.read_line(&mut l)? == 0 || l.trim().is_empty() {
            break;
        }
        if let Some(n) = l.to_ascii_lowercase().strip_prefix("content-length:") {
            length = n.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length.min(8 << 20)];
    r.read_exact(&mut body)?;
    let out = if first.starts_with("GET") {
        json!({"data": [{"id": "local-model"}]})
    } else {
        reply(&serde_json::from_slice(&body).unwrap_or(Value::Null))
    }
    .to_string();
    let mut conn = conn;
    conn.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
            out.len()
        )
        .as_bytes(),
    )
}
