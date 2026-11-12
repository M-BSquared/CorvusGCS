//! The plain MAVLink layer held to pymavlink 2.4.50, whose answers were frozen
//! once into `tests/data/pymavlink.json.gz` when Python left the project:
//! the `all` dialect's message tables in both wire versions, every constant
//! and enum, one frame of every message in both versions, the messages
//! pymavlink recovers from a damaged stream, signed frames and checksums.
//!
//! The fixture is the reference and is not regenerated: a change to the
//! definitions in `message_definitions/` that moves a table shows up here,
//! and is accepted by checking it against the MAVLink XML, not by rewriting
//! the fixture to match the code.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use corvus_link::plain::codec::unpack_payload;
use corvus_link::plain::defs::{self, Wire, ENUMS, MESSAGES};
use corvus_link::plain::frame::{encode, Event, Parser, Proto, Signing};
use corvus_link::plain::{crc, Message, Value};
use serde_json::Value as Json;

fn fixture() -> &'static Json {
    static DOC: std::sync::OnceLock<Json> = std::sync::OnceLock::new();
    DOC.get_or_init(|| {
        let gz = include_bytes!("data/pymavlink.json.gz");
        let mut text = String::new();
        flate2::read::GzDecoder::new(&gz[..]).read_to_string(&mut text).unwrap();
        serde_json::from_str(&text).unwrap()
    })
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn strs(j: &Json) -> Vec<&str> {
    j.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect()
}

fn wire_of(key: &str) -> Wire {
    if key == "v1" {
        Wire::V1
    } else {
        Wire::V2
    }
}

#[test]
fn the_fixture_is_the_one_frozen_from_pymavlink() {
    let doc = fixture();
    assert_eq!(doc["pymavlink"], "2.4.50");
    assert_eq!(doc["tables"]["v1"].as_array().unwrap().len(), 194);
    assert_eq!(doc["tables"]["v2"].as_array().unwrap().len(), 353);
}

#[test]
fn every_message_table_matches_in_both_wire_versions() {
    for key in ["v1", "v2"] {
        let wire = wire_of(key);
        let want = fixture()["tables"][key].as_array().unwrap();
        let ours: BTreeSet<u32> = MESSAGES.iter().filter(|m| m.in_view(wire)).map(|m| m.id).collect();
        let theirs: BTreeSet<u32> = want.iter().map(|t| t["id"].as_u64().unwrap() as u32).collect();
        assert_eq!(ours, theirs, "{key}: the message ids");
        for t in want {
            let id = t["id"].as_u64().unwrap() as u32;
            let def = defs::message_by_id(id, wire).unwrap();
            let name = t["name"].as_str().unwrap();
            let at = format!("{key} {name}");
            assert_eq!(def.name, name, "{at}");
            assert_eq!(defs::message_by_name(name, wire).map(|d| d.id), Some(id), "{at}");
            let fields = def.fields(wire);
            let names: Vec<&str> = fields.iter().map(|f| f.name).collect();
            assert_eq!(names, strs(&t["fieldnames"]), "{at}: fields");
            let ordered: Vec<&str> = def.order(wire).iter().map(|&i| def.fields[i as usize].name).collect();
            assert_eq!(ordered, strs(&t["ordered_fieldnames"]), "{at}: wire order");
            let types: Vec<&str> = fields.iter().map(|f| f.type_name).collect();
            assert_eq!(types, strs(&t["fieldtypes"]), "{at}: types");
            let lengths: Vec<u64> = fields.iter().map(|f| f.array_len as u64).collect();
            let want_lengths: Vec<u64> = t["array_lengths"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap())
                .collect();
            assert_eq!(lengths, want_lengths, "{at}: array lengths");
            assert_eq!(
                u64::from(def.crc_extra),
                t["crc_extra"].as_u64().unwrap(),
                "{at}: crc extra"
            );
            assert_eq!(
                def.payload_len(wire) as u64,
                t["payload_len"].as_u64().unwrap(),
                "{at}: length"
            );
            // MAVLink 1 has no instance field where it is an extension.
            let instance = def
                .instance_field
                .map(|i| i as usize)
                .filter(|i| *i < fields.len())
                .map(|i| def.fields[i].name);
            assert_eq!(instance, t["instance_field"].as_str(), "{at}: instance field");
            for (attr, get) in [
                (
                    "units",
                    (|f: &defs::FieldDef| f.units) as fn(&defs::FieldDef) -> &'static str,
                ),
                ("enums", |f| f.enum_name),
                ("display", |f| f.display),
            ] {
                let ours: BTreeMap<&str, &str> = fields
                    .iter()
                    .filter(|f| !get(f).is_empty())
                    .map(|f| (f.name, get(f)))
                    .collect();
                let theirs: BTreeMap<&str, &str> = t[attr]
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter_map(|(k, v)| v.as_str().filter(|s| !s.is_empty()).map(|s| (k.as_str(), s)))
                    .collect();
                assert_eq!(ours, theirs, "{at}: {attr}");
            }
        }
    }
}

#[test]
fn every_enum_matches() {
    let want = fixture()["enums"].as_object().unwrap();
    let ours: BTreeSet<&str> = ENUMS.iter().map(|e| e.name).collect();
    let theirs: BTreeSet<&str> = want.keys().map(String::as_str).collect();
    assert_eq!(ours, theirs, "the enums");
    for e in ENUMS {
        let w = &want[e.name];
        assert_eq!(e.bitmask, w["bitmask"].as_bool().unwrap(), "{}: bitmask", e.name);
        // `<ENUM>_ENUM_END`, one past the largest value, is pymavlink's own
        // addition to every enum, and the tables carry it as well.
        let real: Vec<&Json> = w["entries"].as_array().unwrap().iter().collect();
        let ours: Vec<(i64, &str)> = {
            let mut v: Vec<(i64, &str)> = e.entries.iter().map(|x| (x.value, x.name)).collect();
            v.sort();
            v.dedup_by_key(|x| x.0);
            v
        };
        let theirs: Vec<(i64, &str)> = real
            .iter()
            .map(|x| (x["value"].as_i64().unwrap(), x["name"].as_str().unwrap()))
            .collect();
        assert_eq!(ours.len(), theirs.len(), "{}: entries", e.name);
        for x in real {
            let value = x["value"].as_i64().unwrap();
            let name = x["name"].as_str().unwrap();
            let entry = e
                .entries
                .iter()
                .rev()
                .find(|y| y.value == value)
                .unwrap_or_else(|| panic!("{}: no {value}", e.name));
            assert_eq!(entry.name, name, "{}: {value}", e.name);
            assert_eq!(
                entry.description,
                x["description"].as_str().unwrap(),
                "{name}: description"
            );
            assert_eq!(
                entry.has_location,
                x["has_location"].as_bool().unwrap(),
                "{name}: has location"
            );
            let params: BTreeMap<String, &str> = entry.params.iter().map(|p| (p.0.to_string(), p.1)).collect();
            let want_params: BTreeMap<String, &str> = x["param"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap()))
                .collect();
            assert_eq!(params, want_params, "{name}: params");
            let labels: BTreeMap<String, &str> = entry
                .params
                .iter()
                .filter(|p| !p.2.is_empty())
                .map(|p| (p.0.to_string(), p.2))
                .collect();
            let want_labels: BTreeMap<String, &str> = x["label"]
                .as_object()
                .unwrap()
                .iter()
                .filter_map(|(k, v)| v.as_str().filter(|s| !s.is_empty()).map(|s| (k.clone(), s)))
                .collect();
            assert_eq!(labels, want_labels, "{name}: labels");
        }
    }
}

#[test]
fn every_constant_has_pymavlinks_value() {
    let mut checked = 0;
    for (key, doc) in [("v2", &fixture()["constants"]), ("v1", &fixture()["constants_v1"])] {
        let enum_names: BTreeSet<&str> = fixture()["enums"]
            .as_object()
            .unwrap()
            .values()
            .flat_map(|e| e["entries"].as_array().unwrap())
            .map(|x| x["name"].as_str().unwrap())
            .collect();
        for (name, value) in doc.as_object().unwrap() {
            if !enum_names.contains(name.as_str()) {
                continue;
            }
            let Some(ours) = defs::constant(name) else {
                panic!("{key}: no constant {name}");
            };
            assert_eq!(ours, value.as_i64().unwrap(), "{key}: {name}");
            checked += 1;
        }
    }
    assert!(checked > 4000, "{checked} constants");
}

#[test]
fn the_framing_constants_are_pymavlinks() {
    use corvus_link::plain::frame::{FLAG_SIGNED, MAGIC_V1, MAGIC_V2, SIGNATURE_LEN};
    let c = &fixture()["constants"];
    assert_eq!(i64::from(MAGIC_V1), c["PROTOCOL_MARKER_V1"]);
    assert_eq!(i64::from(MAGIC_V2), c["PROTOCOL_MARKER_V2"]);
    assert_eq!(i64::from(FLAG_SIGNED), c["MAVLINK_IFLAG_SIGNED"]);
    assert_eq!(SIGNATURE_LEN as i64, c["MAVLINK_SIGNATURE_BLOCK_LEN"]);
}

#[test]
fn every_message_id_constant_names_its_message() {
    for (key, doc) in [("v2", &fixture()["constants"]), ("v1", &fixture()["constants_v1"])] {
        let wire = wire_of(key);
        for (name, value) in doc.as_object().unwrap() {
            // BAD_DATA and UNKNOWN are pymavlink's own, below zero.
            if let (Some(msg), Some(id)) = (name.strip_prefix("MAVLINK_MSG_ID_"), value.as_u64()) {
                let def = defs::message_by_id(id as u32, wire).unwrap_or_else(|| panic!("{key}: {name}"));
                assert_eq!(def.name, msg, "{key}: {name}");
            }
        }
    }
}

fn field_value(field: &defs::FieldDef, j: &Json) -> Value {
    let scalar = |j: &Json| match j {
        Json::Number(n) if n.is_f64() => Value::Float(n.as_f64().unwrap()),
        Json::Number(n) => Value::Int(
            n.as_i64()
                .map(i128::from)
                .or_else(|| n.as_u64().map(i128::from))
                .unwrap(),
        ),
        other => panic!("{}: {other}", field.name),
    };
    match j {
        // A char[n] is text; a lone char is its byte, which pymavlink passes
        // as a one-byte string.
        Json::Object(o) if field.is_string() => Value::Bytes(unhex(o["hex"].as_str().unwrap())),
        Json::Object(o) => Value::Int(unhex(o["hex"].as_str().unwrap()).first().map_or(0, |b| i128::from(*b))),
        Json::Array(a) => Value::Array(a.iter().map(scalar).collect()),
        other => {
            let v = scalar(other);
            match (field.ty, v) {
                (defs::Ty::F32 | defs::Ty::F64, Value::Int(i)) => Value::Float(i as f64),
                (_, v) => v,
            }
        }
    }
}

fn message_of(case: &Json) -> (Proto, Message) {
    let proto = if case["proto"] == 1 { Proto::V1 } else { Proto::V2 };
    let name = case["message"].as_str().unwrap();
    let mut m = Message::named(name, proto.wire()).unwrap_or_else(|| panic!("no {name}"));
    for (k, v) in case["fields"].as_object().unwrap() {
        let field = m.def().fields(proto.wire()).iter().find(|f| f.name == k).unwrap();
        m.set(k, field_value(field, v))
            .unwrap_or_else(|e| panic!("{name}.{k}: {e}"));
    }
    (proto, m)
}

fn same(got: &Value, want: &Value, at: &str) {
    match (got, want) {
        (Value::Bytes(g), Value::Bytes(w)) => {
            let trim = |b: &[u8]| b.iter().rposition(|&c| c != 0).map_or(0, |p| p + 1);
            assert_eq!(&g[..trim(g)], &w[..trim(w)], "{at}");
        }
        (Value::Float(g), Value::Int(w)) | (Value::Int(w), Value::Float(g)) => assert_eq!(*g, *w as f64, "{at}"),
        (Value::Array(g), Value::Array(w)) => {
            assert_eq!(g.len(), w.len(), "{at}");
            g.iter().zip(w).for_each(|(a, b)| same(a, b, at));
        }
        (g, w) => assert_eq!(g, w, "{at}"),
    }
}

#[test]
fn every_message_is_encoded_as_pymavlink_encodes_it() {
    let frames = fixture()["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 194 + 353);
    for case in frames {
        let (proto, msg) = message_of(case);
        let seq = case["seq"].as_u64().unwrap() as u8;
        let got = encode(proto, seq, 42, 7, &msg, None).unwrap();
        assert_eq!(
            got,
            unhex(case["frame"].as_str().unwrap()),
            "{} v{}",
            msg.name(),
            case["proto"]
        );
    }
}

#[test]
fn every_pymavlink_frame_reads_back_to_its_values() {
    for case in fixture()["frames"].as_array().unwrap() {
        let (proto, want) = message_of(case);
        let bytes = unhex(case["frame"].as_str().unwrap());
        let mut parser = Parser::new();
        let events = parser.feed(&bytes);
        let name = want.name();
        let [Event::Frame(frame)] = events.as_slice() else {
            panic!("{name}: {events:?}")
        };
        assert_eq!((frame.proto, frame.sysid, frame.compid), (proto, 42, 7), "{name}");
        assert_eq!(frame.raw, bytes, "{name}");
        let got = frame.message().unwrap();
        assert_eq!(got.name(), name);
        for (field, (g, w)) in got
            .def()
            .fields(proto.wire())
            .iter()
            .zip(got.values().iter().zip(want.values()))
        {
            same(g, w, &format!("{name}.{}", field.name));
        }
        let again = unpack_payload(frame.msgid, proto.wire(), &frame.payload).unwrap();
        assert_eq!(again.values(), got.values(), "{name}");
    }
}

#[test]
fn a_damaged_stream_gives_back_the_messages_pymavlink_recovers() {
    let d = &fixture()["damaged"];
    let stream = unhex(d["stream"].as_str().unwrap());
    let want: Vec<(String, Vec<u8>)> = d["good"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["type"].as_str().unwrap().to_string(),
                unhex(g["frame"].as_str().unwrap()),
            )
        })
        .collect();
    assert!(want.len() > 20, "{} recovered", want.len());
    // pymavlink, having taken a damaged length for a frame, throws away what
    // follows; the plain parser steps one byte and finds the next frame. So
    // everything pymavlink recovers is found too, in order, and anything more
    // is a frame whose checksum holds and that reads as its message.
    for piece in [1usize, 7, 40, stream.len()] {
        let mut parser = Parser::new();
        let mut got = Vec::new();
        for chunk in stream.chunks(piece) {
            for e in parser.feed(chunk) {
                if let Event::Frame(f) = e {
                    let m = f
                        .message()
                        .unwrap_or_else(|e| panic!("a frame that does not read: {e:?}"));
                    got.push((m.name().to_string(), f.raw));
                }
            }
        }
        let mut rest = got.iter();
        for w in &want {
            assert!(rest.any(|g| g == w), "in pieces of {piece}: {} not found in order", w.0);
        }
        assert!(
            got.len() <= 71,
            "in pieces of {piece}: {} frames from 71 sent",
            got.len()
        );
    }
}

#[test]
fn signed_frames_are_signed_as_pymavlink_signs_them() {
    for case in fixture()["signed"].as_array().unwrap() {
        let key: [u8; 32] = unhex(case["key"].as_str().unwrap()).try_into().unwrap();
        let want = unhex(case["frame"].as_str().unwrap());
        let mut parser = Parser::new();
        let events = parser.feed(&want);
        let [Event::Frame(frame)] = events.as_slice() else {
            panic!("{events:?}")
        };
        let msg = frame.message().unwrap();
        let mut signing = Signing::new(
            key,
            case["link_id"].as_u64().unwrap() as u8,
            case["timestamp"].as_u64().unwrap(),
        );
        let got = encode(
            Proto::V2,
            frame.seq,
            frame.sysid,
            frame.compid,
            &msg,
            Some(&mut signing),
        )
        .unwrap();
        assert_eq!(got, want, "{}", msg.name());
        assert_eq!(frame.timestamp(), Some(case["next_timestamp"].as_u64().unwrap() - 1));
        assert!(signing.verify(frame), "{}: its own signature", msg.name());
        let mut tampered = want.clone();
        *tampered.last_mut().unwrap() ^= 1;
        let mut p = Parser::new();
        if let [Event::Frame(bad)] = p.feed(&tampered).as_slice() {
            assert!(!signing.verify(bad), "{}: a changed signature", msg.name());
        }
    }
}

#[test]
fn x25_checksums_match() {
    for case in fixture()["x25"].as_array().unwrap() {
        let data = unhex(case["data"].as_str().unwrap());
        assert_eq!(
            u64::from(crc::x25(&data)),
            case["crc"].as_u64().unwrap(),
            "{} bytes",
            data.len()
        );
    }
}
