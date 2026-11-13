//! The plain MAVLink layer held to pymavlink: every frame in
//! `tests/data/plain_frames.json` (dumped from pymavlink while Corvus was Python)
//! is produced byte for byte from its field values, and read back to them.

use corvus_link::plain::frame::{encode, Event, Parser, Proto, Signing};
use corvus_link::plain::{codec, Message, Value};
use serde_json::Value as Json;

fn cases() -> Json {
    serde_json::from_str(include_str!("data/plain_frames.json")).unwrap()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn scalar(j: &Json) -> Value {
    match j {
        Json::Number(n) if n.is_f64() => Value::Float(n.as_f64().unwrap()),
        Json::Number(n) => Value::Int(
            n.as_i64()
                .map(i128::from)
                .or_else(|| n.as_u64().map(i128::from))
                .unwrap(),
        ),
        other => panic!("{other}"),
    }
}

fn value_of(j: &Json, is_string: bool) -> Value {
    match j {
        Json::String(s) if is_string => Value::Bytes(s.as_bytes().to_vec()),
        Json::Array(a) => Value::Array(a.iter().map(scalar).collect()),
        other => scalar(other),
    }
}

fn message(name: &str, proto: Proto, fields: &Json) -> Message {
    let mut m = Message::named(name, proto.wire()).unwrap();
    for (k, v) in fields.as_object().unwrap() {
        let is_string = m
            .def()
            .fields(proto.wire())
            .iter()
            .any(|f| f.name == k && f.is_string());
        m.set(k, value_of(v, is_string)).unwrap();
    }
    m
}

fn same(got: &Value, want: &Value, field: &str) {
    match (got, want) {
        (Value::Bytes(g), Value::Bytes(w)) => {
            let trim = |b: &[u8]| b.iter().rposition(|&c| c != 0).map_or(0, |p| p + 1);
            assert_eq!(&g[..trim(g)], &w[..trim(w)], "{field}");
        }
        (Value::Float(g), Value::Int(w)) | (Value::Int(w), Value::Float(g)) => assert_eq!(*g, *w as f64, "{field}"),
        (Value::Array(g), Value::Array(w)) => {
            assert_eq!(g.len(), w.len(), "{field}");
            g.iter().zip(w).for_each(|(a, b)| same(a, b, field));
        }
        (g, w) => assert_eq!(g, w, "{field}"),
    }
}

#[test]
fn every_pymavlink_frame_is_produced_byte_for_byte() {
    let c = cases();
    let frames = c["frames"].as_array().unwrap();
    assert!(frames.len() > 50);
    for case in frames {
        let proto = if case["proto"] == 1 { Proto::V1 } else { Proto::V2 };
        let name = case["message"].as_str().unwrap();
        let msg = message(name, proto, &case["fields"]);
        let got = encode(
            proto,
            case["seq"].as_u64().unwrap() as u8,
            case["sysid"].as_u64().unwrap() as u8,
            case["compid"].as_u64().unwrap() as u8,
            &msg,
            None,
        )
        .unwrap();
        let want = unhex(case["frame"].as_str().unwrap());
        assert_eq!(got, want, "{name} v{} {}", case["proto"], case["variant"]);
    }
}

#[test]
fn every_pymavlink_frame_parses_back_to_its_values() {
    let c = cases();
    for case in c["frames"].as_array().unwrap() {
        let proto = if case["proto"] == 1 { Proto::V1 } else { Proto::V2 };
        let name = case["message"].as_str().unwrap();
        let bytes = unhex(case["frame"].as_str().unwrap());
        let mut parser = Parser::new();
        let events = parser.feed(&bytes);
        assert_eq!(events.len(), 1, "{name}: {events:?}");
        let Event::Frame(frame) = &events[0] else {
            panic!("{name}: {events:?}")
        };
        assert_eq!(
            (frame.proto, frame.sysid, frame.compid, frame.seq),
            (proto, 7, 9, 41),
            "{name}"
        );
        assert_eq!(frame.raw, bytes);
        assert_eq!(parser.pending(), 0);
        let msg = frame.message().unwrap();
        assert_eq!(msg.name(), name);
        let want = message(name, proto, &case["fields"]);
        for (field, (g, w)) in msg
            .def()
            .fields(proto.wire())
            .iter()
            .zip(msg.values().iter().zip(want.values()))
        {
            same(g, w, &format!("{name}.{}", field.name));
        }
    }
}

#[test]
fn a_stream_in_pieces_with_noise_between_frames() {
    let c = cases();
    let frames: Vec<Vec<u8>> = c["frames"]
        .as_array()
        .unwrap()
        .iter()
        .take(12)
        .map(|f| unhex(f["frame"].as_str().unwrap()))
        .collect();
    let mut stream = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        stream.extend(std::iter::repeat_n(0x55u8, i % 3));
        stream.extend_from_slice(f);
    }
    let mut parser = Parser::new();
    let mut found = Vec::new();
    for piece in stream.chunks(7) {
        for e in parser.feed(piece) {
            if let Event::Frame(f) = e {
                found.push(f.raw);
            }
        }
    }
    assert_eq!(found, frames);
}

#[test]
fn a_bad_checksum_is_dropped_and_the_parser_finds_the_next_frame() {
    let c = cases();
    let first = unhex(c["frames"][0]["frame"].as_str().unwrap());
    let second = unhex(c["frames"][3]["frame"].as_str().unwrap());
    let mut bad = first.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    let mut stream = bad;
    stream.extend_from_slice(&second);
    let events = Parser::new().feed(&stream);
    assert!(events.iter().any(|e| matches!(e, Event::BadCrc { .. })), "{events:?}");
    assert!(events.iter().any(|e| matches!(e, Event::Frame(f) if f.raw == second)));
}

#[test]
fn a_signed_frame_is_signed_as_pymavlink_signs_it() {
    let c = cases();
    let s = &c["signed"];
    let key: [u8; 32] = unhex(s["secret_key"].as_str().unwrap()).try_into().unwrap();
    let mut signing = Signing::new(
        key,
        s["link_id"].as_u64().unwrap() as u8,
        s["timestamp"].as_u64().unwrap(),
    );
    let msg = message("ATTITUDE", Proto::V2, &s["fields"]);
    let got = encode(
        Proto::V2,
        s["seq"].as_u64().unwrap() as u8,
        7,
        9,
        &msg,
        Some(&mut signing),
    )
    .unwrap();
    let want = unhex(s["frame"].as_str().unwrap());
    assert_eq!(got, want);
    assert_eq!(signing.timestamp, s["timestamp"].as_u64().unwrap() + 1);

    let events = Parser::new().feed(&want);
    let Event::Frame(frame) = &events[0] else {
        panic!("{events:?}")
    };
    assert_eq!(frame.timestamp(), s["timestamp"].as_u64());
    let verifier = Signing::new(key, 0, 0);
    assert!(verifier.verify(frame));
    assert!(!Signing::new([1; 32], 0, 0).verify(frame));
}

#[test]
fn values_outside_a_field_are_refused() {
    let mut m = Message::named("HEARTBEAT", corvus_link::plain::defs::Wire::V2).unwrap();
    m.set("type", Value::Int(256)).unwrap();
    assert!(matches!(
        codec::pack_payload(&m, true),
        Err(codec::CodecError::OutOfRange("type"))
    ));
    m.set("type", Value::Float(1.0)).unwrap();
    assert!(matches!(
        codec::pack_payload(&m, true),
        Err(codec::CodecError::WrongType("type"))
    ));
    assert!(m.set("nonsense", Value::Int(0)).is_err());
}

#[test]
fn a_mavlink_2_payload_that_lost_its_zeros_reads_back_zero_filled() {
    let msg = codec::unpack_payload(0, corvus_link::plain::defs::Wire::V2, &[1]).unwrap();
    assert_eq!(msg.get("custom_mode"), Some(&Value::Int(1)));
    assert_eq!(msg.get("system_status"), Some(&Value::Int(0)));
    assert!(codec::unpack_payload(0, corvus_link::plain::defs::Wire::V1, &[0; 10]).is_err());
    assert!(codec::unpack_payload(0, corvus_link::plain::defs::Wire::V1, &[1]).is_err());
    assert!(codec::unpack_payload(9999999, corvus_link::plain::defs::Wire::V2, &[]).is_err());
}

#[test]
fn a_mavlink_2_payload_with_fields_this_dialect_does_not_know_is_still_read() {
    // A newer firmware adds extension fields to a message: its checksum is
    // the same, its payload longer. The fields known here are read.
    let wire = corvus_link::plain::defs::Wire::V2;
    let mut hb = Message::named("HEARTBEAT", wire).unwrap();
    hb.set("custom_mode", Value::Int(0x0403_0201)).unwrap();
    hb.set("type", Value::Int(2)).unwrap();
    hb.set("mavlink_version", Value::Int(3)).unwrap();
    let mut payload = codec::pack_payload(&hb, false).unwrap();
    payload.extend_from_slice(&[7, 7, 7, 7]);
    let msg = codec::unpack_payload(0, wire, &payload).unwrap();
    assert_eq!(msg.get("custom_mode"), Some(&Value::Int(0x0403_0201)));
    assert_eq!(msg.get("type"), Some(&Value::Int(2)));
    assert_eq!(msg.get("mavlink_version"), Some(&Value::Int(3)));

    let mut frame = vec![0xFD, payload.len() as u8, 0, 0, 9, 1, 1, 0, 0, 0];
    frame.extend_from_slice(&payload);
    let crc = corvus_link::plain::crc::accumulate(
        corvus_link::plain::crc::accumulate(0xFFFF, &frame[1..]),
        &[hb.def().crc_extra],
    );
    frame.extend_from_slice(&crc.to_le_bytes());
    let events = Parser::new().feed(&frame);
    let [Event::Frame(f)] = &events[..] else {
        panic!("{events:?}")
    };
    assert_eq!(f.message().unwrap().get("type"), Some(&Value::Int(2)));
}

#[test]
fn connection_strings_that_are_not_links_fail_with_pythons_words() {
    use corvus_link::plain::connection::{parse_target, Options};
    let c = cases();
    let errors = c["connection_errors"].as_object().unwrap();
    assert!(errors.len() >= 10);
    for (text, want) in errors {
        let got = parse_target(text, &Options::default()).expect_err(text).to_string();
        let want = want.as_str().unwrap().split_once(": ").unwrap().1;
        assert_eq!(got, want, "{text}");
    }
}
