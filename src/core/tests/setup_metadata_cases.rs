//! The Rust parameter metadata readers answer what `python/corvus/param_metadata.py`
//! answers for PX4 JSON (plain and xz) and ArduPilot packs
//! (`python/tools/dump_setup_schema_cases.py metadata`).

use base64::Engine;
use corvus_core::setup_schemas::param_metadata::*;
use serde_json::Value;

fn cases() -> Value {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/setup_metadata_cases.json"
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn files_parse_like_python() {
    for case in cases()["parse"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let raw = base64::engine::general_purpose::STANDARD
            .decode(case["raw"].as_str().unwrap())
            .unwrap();
        let got = parse_param_metadata(case["format"].as_str().unwrap(), &raw);
        match (case.get("ok"), case.get("error")) {
            (Some(expected), _) => {
                let got = got.unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(&Value::Object(got), expected, "{name}");
            }
            (None, Some(message)) => {
                let e = got.expect_err(name);
                let message = message.as_str().unwrap();
                if !message.is_empty() {
                    assert_eq!(e.0, message, "{name}");
                }
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn cache_file_names_match() {
    let c = cases();
    let cache = ParamMetadataCache::new("/nowhere");
    let _ = cache;
    for row in c["cache_names"].as_array().unwrap() {
        let got = ParamMetadataCache::file_name(row[0].as_str().unwrap(), row[1].as_u64().unwrap() as u32);
        assert_eq!(got, row[2].as_str().unwrap(), "{row}");
    }
    for row in c["cache_matches"].as_array().unwrap() {
        assert_eq!(
            ParamMetadataCache::is_cache_file(row[0].as_str().unwrap()),
            row[1].as_bool().unwrap(),
            "{row}"
        );
    }
    for row in c["error_texts"].as_array().unwrap() {
        assert_eq!(
            fetch_error_text(10, "x"),
            "this firmware was built without parameter metadata"
        );
        assert_eq!(fetch_error_text(7, "x"), "the autopilot does not support MAVLink FTP");
        assert_eq!(fetch_error_text(1, "boom"), "boom");
        let _ = row;
    }
}

#[test]
fn the_copy_store_keeps_loads_and_prunes() {
    let dir = std::env::temp_dir().join(format!("corvus-meta-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = ParamMetadataCache::new(&dir);
    assert_eq!(cache.load("px4-json", 1), None);
    for crc in 1..=15u32 {
        cache.store("px4-json", crc, &[crc as u8; 10]);
    }
    let info = cache.info();
    assert_eq!(info["files"], ParamMetadataCache::MAX_FILES);
    assert_eq!(info["bytes"], ParamMetadataCache::MAX_FILES * 10);
    assert_eq!(info["dir"], dir.to_string_lossy().as_ref());
    assert_eq!(cache.load("px4-json", 15), Some(vec![15; 10]));
    cache.store("px4-json", 99, b"");
    assert_eq!(cache.load("px4-json", 99), None, "an empty copy is no copy");
    std::fs::write(dir.join("notes.txt"), b"keep").unwrap();
    cache.discard("px4-json", 15);
    assert_eq!(cache.load("px4-json", 15), None);
    let removed = cache.clear();
    assert_eq!(removed, cache.info()["files"].as_u64().map_or(0, |_| removed));
    assert_eq!(cache.info()["files"], 0);
    assert!(dir.join("notes.txt").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
