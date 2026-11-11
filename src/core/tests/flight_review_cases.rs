//! The flight review against what `flight_review.py` makes of the same logs
//! (synthetic flights from `python/tools/logs_kit.py`).

mod logs_util;

use corvus_core::logs::flight_review::{clear_cache, review_bytes, SENSITIVITIES};
use logs_util::{data_path, diff};
use serde_json::{json, Value};

const TOLERANCE: f64 = 1e-9;

fn check_dir(dir: &std::path::Path) {
    let index: Value = serde_json::from_slice(&std::fs::read(dir.join("index.json")).unwrap()).unwrap();
    let names: Vec<&str> = index.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert!(names.len() >= 15);
    let mut failures = Vec::new();
    for name in names {
        let blob = std::fs::read(dir.join(format!("{name}.ulg"))).unwrap();
        let expected: Value =
            serde_json::from_slice(&std::fs::read(dir.join(format!("{name}.review.json"))).unwrap()).unwrap();
        for sens in SENSITIVITIES {
            clear_cache();
            let got = review_bytes(&blob, &format!("{name}.ulg"), sens).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let want = &expected[sens];
            let got: Value = if sens == "normal" {
                (*got).clone()
            } else {
                json!({"summary": got["summary"], "findings": got["findings"]})
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
fn every_stored_flight_reviews_as_python_reviews_it() {
    check_dir(&data_path("logs"));
}

/// Every flight of the kit, dumped with `CORVUS_DUMP_ALL=DIR tools/dump_logs_cases.py flights`.
#[test]
#[ignore]
fn every_flight_of_the_kit_reviews_as_python_reviews_it() {
    let dir = std::env::var("CORVUS_LOGS_FLIGHTS").expect("CORVUS_LOGS_FLIGHTS");
    check_dir(std::path::Path::new(&dir));
}

#[test]
fn a_file_is_read_once_and_a_rewritten_one_again() {
    use corvus_core::logs::flight_review::{review_file, ReviewError};
    use std::sync::Arc;
    clear_cache();
    let blob = std::fs::read(data_path("logs/clipping.ulg")).unwrap();
    let dir = std::env::temp_dir().join(format!("corvus-review-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.ulg");
    std::fs::write(&path, &blob).unwrap();
    let first = review_file(&path, "", "normal").unwrap();
    assert_eq!(first["summary"]["name"], "a.ulg");
    let again = review_file(&path, "", "normal").unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    let strict = review_file(&path, "", "strict").unwrap();
    assert!(!Arc::ptr_eq(&first, &strict));
    let junk = dir.join("b.ulg");
    std::fs::write(&junk, b"not a log at all, no").unwrap();
    assert!(matches!(review_file(&junk, "", "normal"), Err(ReviewError::Ulog(m)) if m == "not a ULog file"));
    let mut longer = blob.clone();
    longer.extend_from_slice(&[0u8; 3]);
    std::fs::write(&path, &longer).unwrap();
    let rewritten = review_file(&path, "", "normal").unwrap();
    assert!(!Arc::ptr_eq(&first, &rewritten));
    match review_file(&dir.join("missing.ulg"), "", "normal") {
        Err(ReviewError::Io(m)) => assert!(m.starts_with("[Errno 2] No such file or directory: '"), "{m}"),
        other => panic!("{other:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}
