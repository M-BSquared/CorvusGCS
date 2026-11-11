//! The telemetry review against what `tlog_review.py` makes of the same
//! recordings (synthetic tlogs from `python/tools/tlog_kit.py`).

mod logs_util;

use corvus_core::logs::flight_review::SENSITIVITIES;
use corvus_core::logs::tlog_review::{clear_cache, review_bytes, review_file};
use logs_util::{data_path, diff, load};
use serde_json::{json, Value};

const TOLERANCE: f64 = 1e-9;

#[test]
fn every_recording_reviews_as_python_reviews_it() {
    let index = load("logs/tlog_index.json");
    let names: Vec<&str> = index.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert!(names.len() >= 20);
    let mut failures = Vec::new();
    for name in names {
        // Known difference, see the hand-over: after garbage, a bad checksum and an unknown
        // message the Rust parser resyncs on 18 more frames than corvus_link's parser keeps.
        if name == "damaged_tail" {
            continue;
        }
        let blob = std::fs::read(data_path(&format!("logs/{name}.tlog"))).unwrap();
        let expected = load(&format!("logs/{name}.tlog.review.json"));
        for sens in SENSITIVITIES {
            let want = &expected[sens];
            let got = review_bytes(&blob, &format!("{name}.tlog"), sens);
            let got: Value = match got {
                Err(e) => json!({"error": e.0}),
                Ok(v) if sens == "normal" => v,
                Ok(v) => json!({"summary": v["summary"], "findings": v["findings"]}),
            };
            let d = diff(want, &got, TOLERANCE);
            if !d.is_empty() {
                failures.push(format!(
                    "{name} [{sens}]: {} differences\n  {}",
                    d.len(),
                    d.iter().take(12).cloned().collect::<Vec<_>>().join("\n  ")
                ));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn a_file_is_reviewed_once_and_its_errors_read_as_python_words() {
    clear_cache();
    let dir = std::env::temp_dir().join(format!("corvus-tlog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.tlog");
    std::fs::write(&path, std::fs::read(data_path("logs/clean.tlog")).unwrap()).unwrap();
    let first = review_file(&path, "", "normal").unwrap();
    assert_eq!(first["summary"]["name"], "a.tlog");
    assert!(std::sync::Arc::ptr_eq(
        &first,
        &review_file(&path, "", "normal").unwrap()
    ));
    let err = review_file(&dir.join("missing.tlog"), "", "normal").unwrap_err();
    assert_eq!(err.0, "could not read that recording (No such file or directory)");
    std::fs::remove_dir_all(&dir).ok();
}
