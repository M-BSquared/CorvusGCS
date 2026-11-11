//! The firmware catalogue, the ArduPilot half and the image container against
//! what `firmware_catalog.py`, `ardupilot_firmware.py` and `firmware_uploader.py`
//! answer (`python/tools/dump_logs_cases.py firmware`).

mod logs_util;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use corvus_core::firmware::ardupilot as ap;
use corvus_core::firmware::catalog as fc;
use corvus_core::firmware::image::{board_ids_compatible, parse_firmware_image};
use corvus_core::firmware::net::{Net, Reply};
use corvus_core::firmware::pyjson;
use corvus_core::protocol::common::Event;
use logs_util::{diff, load};
use serde_json::{json, Map, Value};

fn cases() -> Value {
    load("firmware_cases.json")
}

fn b64(text: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(text).unwrap()
}

fn same(what: &str, want: &Value, got: &Value) {
    let d = diff(want, got, 0.0);
    assert!(
        d.is_empty(),
        "{what}:\n  {}",
        d.iter().take(8).cloned().collect::<Vec<_>>().join("\n  ")
    );
}

#[test]
fn the_px4_tables_are_the_pythons() {
    let c = cases();
    let labels: Map<String, Value> = fc_labels();
    same("labels", &c["px4_tables"]["labels"], &Value::Object(labels));
    let vendors: Map<String, Value> = corvus_core::firmware::tables::VENDOR_LABELS
        .iter()
        .map(|(k, v)| ((*k).to_string(), json!(v)))
        .collect();
    same("vendors", &c["px4_tables"]["vendors"], &Value::Object(vendors));
}

fn fc_labels() -> Map<String, Value> {
    corvus_core::firmware::tables::BOARD_LABELS
        .iter()
        .map(|(k, v)| ((*k).to_string(), json!(v)))
        .collect()
}

#[test]
fn the_ardupilot_tables_are_the_pythons() {
    let t = &cases()["ap_tables"];
    let vehicles: Vec<Value> = ap::VEHICLES
        .iter()
        .map(|v| json!({"key": v.key, "directory": v.directory, "suffix": v.suffix, "binary": v.binary, "label": v.label}))
        .collect();
    same("vehicles", &t["vendors"], &json!(vehicles));
    let channels: Vec<Value> = ap::CHANNELS.iter().map(|(a, b, c)| json!([a, b, c])).collect();
    same("channels", &t["channels"], &json!(channels));
    let prefixes: Vec<Value> = corvus_core::firmware::tables::AP_VENDOR_PREFIXES
        .iter()
        .map(|(a, b)| json!([a, b]))
        .collect();
    same("prefixes", &t["prefixes"], &json!(prefixes));
    let fmu: Map<String, Value> = corvus_core::firmware::tables::AP_FMU_BOARDS
        .iter()
        .map(|(k, v)| ((*k).to_string(), json!(v)))
        .collect();
    same("fmu boards", &t["fmu_boards"], &Value::Object(fmu));
    let mut not_flashable: Vec<&str> = corvus_core::firmware::tables::AP_NOT_FLASHABLE.to_vec();
    not_flashable.sort();
    same("not flashable", &t["not_flashable"], &json!(not_flashable));
}

#[test]
fn a_target_describes_itself_as_python_describes_it() {
    for case in cases()["describe"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        same(name, &case["describe"], &Value::Object(fc::describe_board(name)));
        assert_eq!(case["label"].as_str().unwrap(), fc::board_label(name), "{name}");
        assert_eq!(
            case["flashable"].as_bool().unwrap(),
            fc::is_flashable_asset(name),
            "{name}"
        );
    }
    for case in cases()["peripheral"].as_array().unwrap() {
        let board = case["board"].as_str().unwrap();
        assert_eq!(
            case["peripheral"].as_bool().unwrap(),
            fc::is_peripheral_board(board),
            "{board}"
        );
    }
}

#[test]
fn urls_names_and_hosts_are_the_pythons() {
    let c = cases();
    for case in c["asset_urls"].as_array().unwrap() {
        let url = fc::asset_url(case["tag"].as_str().unwrap(), case["name"].as_str().unwrap());
        assert_eq!(case["url"].as_str().unwrap(), url);
    }
    for case in c["safe_names"].as_array().unwrap() {
        assert_eq!(
            case["safe"].as_str().unwrap(),
            fc::safe_name(case["name"].as_str().unwrap()),
            "{case}"
        );
    }
    for case in c["hosts"].as_array().unwrap() {
        let url = case["url"].as_str().unwrap();
        assert_eq!(case["px4"].as_bool().unwrap(), fc::host_allowed(url), "px4 {url:?}");
        assert_eq!(
            case["ardupilot"].as_bool().unwrap(),
            ap::host_allowed(url),
            "ardupilot {url:?}"
        );
    }
}

#[test]
fn a_release_payload_is_reduced_as_python_reduces_it() {
    for case in cases()["parse_releases"].as_array().unwrap() {
        let got = fc::parse_releases(&case["payload"]).unwrap();
        same("releases", &case["releases"], &Value::Array(got));
    }
}

#[test]
fn a_board_is_detected_as_python_detects_it() {
    let c = cases();
    let sets = c["board_sets"].as_array().unwrap();
    for case in c["px4_detect"].as_array().unwrap() {
        let boards = sets[case["set"].as_u64().unwrap() as usize].as_array().unwrap();
        let got = fc::detect_board(&case["hints"], boards).unwrap_or(Value::Null);
        same(&format!("{}", case["hints"]), &case["found"], &got);
    }
    let entries = c["ap_entries"].as_array().unwrap();
    for case in c["ap_detect"].as_array().unwrap() {
        let got = ap::detect_board(&case["hints"], entries).unwrap_or(Value::Null);
        same(&format!("ardupilot {}", case["hints"]), &case["found"], &got);
    }
    for case in c["ap_detect_few"].as_array().unwrap() {
        let got = ap::detect_board(&case["hints"], &entries[..4]).unwrap_or(Value::Null);
        same(&format!("ardupilot few {}", case["hints"]), &case["found"], &got);
    }
    let release_sets = c["release_sets"].as_array().unwrap();
    for case in c["detect_boards"].as_array().unwrap() {
        let releases = release_sets[case["set"].as_u64().unwrap() as usize].as_array().unwrap();
        same(
            &format!("{}", case["hints"]),
            &case["found"],
            &fc::detect_boards(&case["hints"], releases),
        );
    }
}

#[test]
fn ardupilot_names_urls_and_tags_are_the_pythons() {
    let c = cases();
    for case in c["ap_describe"].as_array().unwrap() {
        let board = case["board"].as_str().unwrap();
        same(board, &case["describe"], &Value::Object(ap::describe_board(board)));
        assert_eq!(
            case["flashable"].as_bool().unwrap(),
            ap::is_flashable_board(board),
            "{board}"
        );
    }
    for case in c["ap_tags"].as_array().unwrap() {
        let got = case["tag"]
            .as_str()
            .and_then(ap::parse_tag)
            .map_or(Value::Null, |(a, b)| json!([a, b]));
        same(&format!("{}", case["tag"]), &case["parsed"], &got);
    }
    for case in c["ap_urls"].as_array().unwrap() {
        let (v, ch, b) = (
            case["vehicle"].as_str().unwrap(),
            case["channel"].as_str().unwrap(),
            case["board"].as_str().unwrap(),
        );
        assert_eq!(case["name"].as_str().unwrap(), ap::asset_name(v, ch, b));
        assert_eq!(case["url"].as_str().unwrap(), ap::asset_url(v, ch, b));
    }
    for case in c["ap_index_urls"].as_array().unwrap() {
        let url = ap::index_url(case["directory"].as_str().unwrap(), case["channel"].as_str().unwrap());
        assert_eq!(case["url"].as_str().unwrap(), url);
    }
    for case in c["ap_vehicle_for_mav_type"].as_array().unwrap() {
        assert_eq!(
            case["vehicle"].as_str().unwrap(),
            ap::vehicle_for_mav_type(&case["type"]),
            "{}",
            case["type"]
        );
    }
    for case in c["ap_binary"].as_array().unwrap() {
        assert_eq!(
            case["binary"].as_str().unwrap(),
            ap::vehicle_binary(case["vehicle"].as_str().unwrap())
        );
    }
}

#[test]
fn a_listing_is_read_as_python_reads_it() {
    for case in cases()["ap_index"].as_array().unwrap() {
        let got = ap::parse_index(case["html"].as_str().unwrap(), case["base"].as_str().unwrap());
        same(&format!("base {}", case["base"]), &case["boards"], &json!(got));
    }
    for case in cases()["ap_vehicle_boards"].as_array().unwrap() {
        let vehicle = ap::VEHICLES
            .iter()
            .find(|v| v.key == case["vehicle"].as_str().unwrap())
            .unwrap();
        let listing: Vec<String> = case["listing"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        same(
            "vehicle boards",
            &case["boards"],
            &json!(ap::vehicle_boards(vehicle, &listing)),
        );
    }
}

#[test]
fn a_cached_build_resolves_as_python_resolves_it() {
    let cache = json!([
        {"tag": "ardupilot:Copter/stable", "vendor": "ardupilot", "boards": [
            {"name": "arducopter-stable-CubeOrange.apj", "board": "CubeOrange", "url": "https://evil.com/x"},
            {"name": "arducopter-stable-sitl.apj", "board": "sitl"},
            {"name": "arducopter-stable-bad board.apj", "board": "bad board"},
            {"name": "x.apj"}]},
        {"tag": "ardupilot:Plane/beta", "boards": [{"name": "arduplane-beta-Pixhawk1.apj", "board": "Pixhawk1"}]},
    ]);
    for case in cases()["ap_resolve"].as_array().unwrap() {
        let got = ap::resolve(
            cache.as_array().unwrap(),
            case["tag"].as_str().unwrap(),
            case["name"].as_str().unwrap(),
        );
        same(
            &format!("{} {}", case["tag"], case["name"]),
            &case["found"],
            &got.unwrap_or(Value::Null),
        );
    }
}

struct Body {
    data: Vec<u8>,
    at: usize,
    length: Option<i64>,
}

impl Reply for Body {
    fn read(&mut self, n: usize) -> Result<Vec<u8>, String> {
        let end = (self.at + n).min(self.data.len());
        let chunk = self.data[self.at..end].to_vec();
        self.at = end;
        Ok(chunk)
    }

    fn content_length(&self) -> Option<i64> {
        self.length
    }
}

/// A network that serves what it is told to and says "no route" otherwise.
#[derive(Default)]
struct Stub {
    routes: HashMap<String, Result<Vec<u8>, String>>,
    requests: Mutex<Vec<String>>,
}

impl Net for Stub {
    fn get(&self, url: &str, _accept: &str, _timeout: f64) -> Result<Box<dyn Reply>, String> {
        self.requests.lock().unwrap().push(url.to_string());
        match self.routes.get(url) {
            Some(Ok(data)) => Ok(Box::new(Body {
                data: data.clone(),
                at: 0,
                length: None,
            })),
            Some(Err(e)) => Err(e.clone()),
            None => Err("<urlopen error no route>".into()),
        }
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("corvus-firmware-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_catalogue_answers_as_python_answers_on_every_network() {
    for case in cases()["catalog_scenarios"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let dir = tmp(name);
        for (file, blob) in case["files"].as_object().unwrap() {
            std::fs::write(dir.join(file), b64(blob.as_str().unwrap())).unwrap();
        }
        for (file, key) in [("catalog.json", "catalog"), ("catalog-ardupilot.json", "ardupilot")] {
            if !case[key].is_null() {
                std::fs::write(
                    dir.join(file),
                    serde_json::to_vec(&json!({"releases": case[key]})).unwrap(),
                )
                .unwrap();
            }
        }
        let mut stub = Stub::default();
        for (url, text) in case["routes"].as_object().unwrap() {
            let error = case["route_errors"].get(url).and_then(Value::as_str);
            stub.routes.insert(
                url.clone(),
                match error {
                    Some(e) => Err(e.to_string()),
                    None => Ok(b64(text.as_str().unwrap())),
                },
            );
        }
        let catalog = fc::FirmwareCatalog::new(dir.to_string_lossy(), Arc::new(stub));
        let mut got = catalog.catalog(case["refresh"].as_bool().unwrap()).unwrap();
        got["dir"] = json!("<dir>");
        same(&format!("{name}: answer"), &case["answer"], &got);
        let mut stored = Map::new();
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        for file in files.iter().filter(|f| f.ends_with(".json")) {
            stored.insert(
                file.clone(),
                serde_json::from_str(&std::fs::read_to_string(dir.join(file)).unwrap()).unwrap(),
            );
        }
        same(&format!("{name}: stored"), &case["stored"], &Value::Object(stored));
        for r in case["resolves"].as_array().unwrap() {
            let found = catalog.resolve(r["tag"].as_str().unwrap(), r["board"].as_str().unwrap());
            same(
                &format!("{name}: resolve {} {}", r["tag"], r["board"]),
                &r["found"],
                &found.unwrap_or(Value::Null),
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn a_download_ends_as_python_ends_it() {
    let image: Vec<u8> = (0..=255u8).cycle().take(256 * 700).collect();
    for case in cases()["downloads"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let dir = tmp(&format!("dl-{name}"));
        let entry = case["entry"].clone();
        if case["precached"].as_bool().unwrap() {
            std::fs::write(dir.join("a.px4"), b"cached image").unwrap();
        }
        let mut stub = Stub::default();
        let url = entry["url"].as_str().unwrap().to_string();
        stub.routes.insert(
            url,
            match case["body_len"].as_u64() {
                None => Err("<urlopen error refused>".into()),
                Some(n) if name == "too_large" => Ok(vec![0u8; n as usize]),
                Some(n) => Ok(image.iter().copied().cycle().take(n as usize).collect()),
            },
        );
        let catalog = fc::FirmwareCatalog::new(dir.to_string_lossy(), Arc::new(stub));
        let event = Event::new();
        if case["cancel"].as_bool().unwrap() {
            event.set();
        }
        let progress: Mutex<Vec<(i64, i64)>> = Mutex::default();
        let report = |a: i64, b: i64| progress.lock().unwrap().push((a, b));
        let got = catalog.download(&entry, Some(&report), Some(&event));
        let result = match got {
            Ok(bytes) if bytes.len() < 100_000 => {
                json!({"ok": base64::engine::general_purpose::STANDARD.encode(bytes)})
            }
            Ok(bytes) => json!({"ok": bytes.len()}),
            Err(e) => json!({"error": e}),
        };
        same(&format!("{name}: result"), &case["result"], &result);
        let progress = progress.into_inner().unwrap();
        assert_eq!(
            case["progress_count"].as_u64().unwrap() as usize,
            progress.len(),
            "{name}"
        );
        // `progress[:5] + progress[-2:]`
        let pair = |p: &(i64, i64)| json!([p.0, p.1]);
        let mut shown: Vec<Value> = progress.iter().take(5).map(pair).collect();
        shown.extend(progress[progress.len().saturating_sub(2)..].iter().map(pair));
        same(&format!("{name}: progress"), &case["progress"], &Value::Array(shown));
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        same(&format!("{name}: files"), &case["on_disk"], &json!(files));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn a_firmware_container_decodes_as_python_decodes_it() {
    for case in cases()["images"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let got = match parse_firmware_image(&b64(case["data"].as_str().unwrap())) {
            Ok(i) => json!({
                "image": base64::engine::general_purpose::STANDARD.encode(&i.image),
                "board_id": i.board_id,
            }),
            Err(e) => json!({"error": e}),
        };
        same(name, &case["result"], &got);
    }
    for case in cases()["compatible"].as_array().unwrap() {
        let (b, f) = (case["bootloader"].as_i64().unwrap(), case["firmware"].as_i64().unwrap());
        assert_eq!(case["ok"].as_bool().unwrap(), board_ids_compatible(b, f), "{b} {f}");
    }
}

#[test]
fn json_errors_read_as_pythons_do() {
    for case in cases()["pyjson"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let got = pyjson::check(text);
        assert_eq!(case["error"].as_str().map(str::to_string), got, "{text:?}");
    }
}
