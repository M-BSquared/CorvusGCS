//! The Rust MSP reader answers what `python/corvus/msp.py` answers
//! (`python/tools/dump_core_cases.py msp`): requests, the stream parser, every
//! decoder and whole sessions against a scripted controller.

use std::collections::{HashMap, HashSet};
use std::io;

use corvus_link::plain::msp::*;
use serde_json::{json, Value};

fn cases() -> Value {
    serde_json::from_str(include_str!("data/msp_cases.json")).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn requests_are_framed_like_python() {
    for row in cases()["requests"].as_array().unwrap() {
        let got = match build_request(row[0].as_i64().unwrap(), &unhex(row[1].as_str().unwrap())) {
            Ok(bytes) => json!(hex(&bytes)),
            Err(why) => json!(format!("error: {why}")),
        };
        assert_eq!(got, row[2], "{row}");
    }
}

#[test]
fn the_parser_resynchronises_like_python() {
    for (name, case) in cases()["streams"].as_object().unwrap() {
        let mut parser = MspParser::new();
        let mut frames: Vec<Value> = Vec::new();
        for chunk in case["chunks"].as_array().unwrap() {
            for f in parser.feed(&unhex(chunk.as_str().unwrap())) {
                frames.push(json!([f.command, hex(&f.payload), f.error]));
            }
        }
        assert_eq!(Value::Array(frames), case["frames"], "{name}");
        assert_eq!(json!(parser.frames_ok), case["ok"], "{name}");
        assert_eq!(json!(parser.checksum_errors), case["errors"], "{name}");
    }
}

#[test]
fn every_decoder_answers_like_python() {
    for row in cases()["decoded"].as_array().unwrap() {
        let command = row[0].as_i64().unwrap() as u8;
        let (value, error) = match decode(command, &unhex(row[1].as_str().unwrap())).unwrap() {
            Ok(map) => (Value::Object(map), Value::Null),
            Err(e) => (Value::Null, json!(e.to_string())),
        };
        assert_eq!(value, row[2], "{row}");
        assert_eq!(error, row[3], "{row}");
    }
}

struct Controller {
    answers: HashMap<u8, Vec<u8>>,
    errors: HashSet<u8>,
    silent: bool,
    pending: Vec<u8>,
    requests: Vec<u8>,
    now: std::cell::Cell<f64>,
}

struct Wire<'a>(&'a std::cell::RefCell<Controller>);

fn reply(command: u8, payload: &[u8], error: bool) -> Vec<u8> {
    let mut body = vec![payload.len() as u8, command];
    body.extend_from_slice(payload);
    let sum = body.iter().fold(0u8, |a, b| a ^ b);
    let mut out = if error { b"$M!".to_vec() } else { b"$M>".to_vec() };
    out.extend(body);
    out.push(sum);
    out
}

impl ByteLink for Wire<'_> {
    fn read(&mut self, size: usize) -> io::Result<Vec<u8>> {
        let mut c = self.0.borrow_mut();
        c.now.set(c.now.get() + 0.05);
        let n = size.min(c.pending.len());
        Ok(c.pending.drain(..n).collect())
    }
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        let mut c = self.0.borrow_mut();
        let command = data[4];
        c.requests.push(command);
        if c.silent {
            return Ok(());
        }
        let r = match c.answers.get(&command) {
            Some(a) if !c.errors.contains(&command) => reply(command, a, false),
            _ => reply(command, &[], true),
        };
        c.pending.extend(r);
        Ok(())
    }
    fn reset_input_buffer(&mut self) -> io::Result<()> {
        self.0.borrow_mut().pending.clear();
        Ok(())
    }
    fn close(&mut self) {}
}

#[test]
fn whole_sessions_read_like_python() {
    for (name, case) in cases()["sessions"].as_object().unwrap() {
        let answers: HashMap<u8, Vec<u8>> = case["answers"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.parse().unwrap(), unhex(v.as_str().unwrap())))
            .collect();
        let errors: HashSet<u8> = case["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_u64().unwrap() as u8)
            .collect();
        let controller = std::cell::RefCell::new(Controller {
            answers,
            errors,
            silent: case["silent"].as_bool().unwrap(),
            pending: Vec::new(),
            requests: Vec::new(),
            now: std::cell::Cell::new(0.0),
        });
        let mut wire = Wire(&controller);
        let clock = || controller.borrow().now.get();
        let result = {
            let mut session = MspSession::new(&mut wire, &clock);
            read_with_session(&mut session)
        };
        let (value, error) = match result {
            Ok(map) => (Value::Object(map), Value::Null),
            Err(e) => (Value::Null, json!(e.to_string())),
        };
        assert_eq!(value, case["result"], "{name}");
        assert_eq!(error, case["error"], "{name}");
        assert_eq!(json!(controller.borrow().requests), case["requests"], "{name}");
    }
}

#[test]
fn the_constants_match() {
    let c = cases();
    assert_eq!(json!(DEFAULT_BAUD), c["constants"]["default_baud"]);
    assert_eq!(json!(BAUD_CANDIDATES), c["constants"]["bauds"]);
}

#[test]
fn the_port_is_closed_after_a_failure_and_an_unopenable_port_is_an_operator_error() {
    let closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    struct Silent(std::sync::Arc<std::sync::atomic::AtomicBool>, std::cell::Cell<f64>);
    impl ByteLink for Silent {
        fn read(&mut self, _: usize) -> io::Result<Vec<u8>> {
            self.1.set(self.1.get() + 0.05);
            Ok(Vec::new())
        }
        fn write(&mut self, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
        fn reset_input_buffer(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn close(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let ticks = std::sync::atomic::AtomicU64::new(0);
    let clock = || ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) as f64 * 0.05;
    let flag = closed.clone();
    let err = read_flight_controller(
        "/dev/ttyACM0",
        115_200,
        &move |_, _| Ok(Box::new(Silent(flag.clone(), std::cell::Cell::new(0.0))) as Box<dyn ByteLink>),
        &clock,
    )
    .unwrap_err();
    assert!(err.to_string().contains("USB port"));
    assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
    let err = read_flight_controller("/dev/ttyACM0", 115_200, &|_, _| Err("busy".into()), &clock).unwrap_err();
    assert_eq!(err.to_string(), "could not open /dev/ttyACM0: busy");
}
