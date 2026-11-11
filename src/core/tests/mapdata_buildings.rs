//! OSM buildings hold to `buildings.py` (`python/tools/dump_mapdata_cases.py
//! buildings`), the service against a local Overpass.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use corvus_core::mapdata::buildings as b;
use corvus_core::mapdata::fetch::Http;
use serde_json::{json, Value};

#[path = "support/stub_http.rs"]
mod stub_http;
use stub_http::{Reply, Stub};

fn cases() -> Value {
    let path = format!("{}/tests/data/mapdata_buildings_cases.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("corvus-buildings-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() <= 1e-9 + 1e-10 * b.abs(), "{what}: {a} vs {b}");
}

fn tuple(v: &Value) -> (i64, i64, i64) {
    let a = v.as_array().unwrap();
    (a[0].as_i64().unwrap(), a[1].as_i64().unwrap(), a[2].as_i64().unwrap())
}

fn bounds4(v: &Value) -> [f64; 4] {
    let a = v.as_array().unwrap();
    [0, 1, 2, 3].map(|i| a[i].as_f64().unwrap_or(f64::NAN))
}

fn gunzip(data: &[u8]) -> Value {
    let mut text = String::new();
    flate2::read::GzDecoder::new(data).read_to_string(&mut text).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn pure_helpers_match_python() {
    let c = cases();
    for case in c["parse_length"].as_array().unwrap() {
        if case["skip"].as_bool().unwrap() {
            continue;
        }
        let got = b::parse_length(&case["raw"]);
        assert_eq!(got, case["value"].as_f64(), "{}", case["raw"]);
    }
    for case in c["heights"].as_array().unwrap() {
        let (base, height) = b::building_height(&case["tags"]);
        close(
            base,
            case["result"][0].as_f64().unwrap(),
            &format!("min_height of {}", case["tags"]),
        );
        close(
            height,
            case["result"][1].as_f64().unwrap(),
            &format!("height of {}", case["tags"]),
        );
    }
    for case in c["bounds"].as_array().unwrap() {
        let (w, s, e, n) = b::cell_bounds(
            case["z"].as_i64().unwrap(),
            case["x"].as_i64().unwrap(),
            case["y"].as_i64().unwrap(),
        );
        let want = bounds4(&case["bounds"]);
        for (got, want) in [w, s, e, n].into_iter().zip(want) {
            close(got, want, &format!("{case}"));
        }
    }
    for case in c["cells"].as_array().unwrap() {
        let got = b::cell_of(
            case["lon"].as_f64().unwrap(),
            case["lat"].as_f64().unwrap(),
            case["zoom"].as_i64().unwrap(),
        );
        assert_eq!(got, tuple(&case["cell"]), "{case}");
    }
    for case in c["geojson"].as_array().unwrap() {
        assert_eq!(
            b::to_geojson(&case["overpass"]),
            case["geojson"],
            "{}",
            case["overpass"].to_string().chars().take(80).collect::<String>()
        );
    }
    for case in c["failed"].as_array().unwrap() {
        assert_eq!(
            b::overpass_failed(&case["payload"]),
            case["failed"].as_bool().unwrap(),
            "{}",
            case["payload"]
        );
    }
    let block = tuple(&c["block"]);
    let geo = b::to_geojson(&c["geojson"][0]["overpass"]);
    let split = b::split_by_cell(&geo, &b::cells_of_block(block.0, block.1, block.2));
    let want = c["split"].as_array().unwrap();
    assert_eq!(split.len(), want.len());
    for ((cell, coll), w) in split.iter().zip(want) {
        assert_eq!(*cell, tuple(&w["cell"]));
        assert_eq!(coll, &w["collection"]);
    }
    let other = b::split_by_cell(&geo, &[(15, 1, 1)]);
    assert_eq!(other[0].1, c["split_other"][0]["collection"]);
    let edge_in = json!({"features": [
        {"geometry": {"coordinates": [[[11.5, 48.1]]]}}, {"geometry": {"coordinates": []}}, {"geometry": {}}, {},
        {"geometry": {"coordinates": [[[11.5, 48.1], [11.5002, 48.1], [11.5, 48.1]]]}},
        {"geometry": {"coordinates": [[["a", 1], [2, 3]]]}}]});
    let edge = b::split_by_cell(&edge_in, &b::cells_of_block(14, 8715, 5688));
    for ((cell, coll), w) in edge.iter().zip(c["split_edge"].as_array().unwrap()) {
        assert_eq!(*cell, tuple(&w["cell"]));
        assert_eq!(
            coll["features"].as_array().unwrap().len() as u64,
            w["n"].as_u64().unwrap()
        );
    }
    for case in c["queries"].as_array().unwrap() {
        let [w, s, e, n] = bounds4(&case["bounds"]);
        assert_eq!(b::overpass_query(w, s, e, n), case["query"].as_str().unwrap());
    }
    for case in c["blocks"].as_array().unwrap() {
        let bd = bounds4(&case["bounds"]);
        let got: Vec<(i64, i64, i64)> = b::blocks_outward(bd).take(120).collect();
        let want: Vec<(i64, i64, i64)> = case["blocks"].as_array().unwrap().iter().map(tuple).collect();
        assert_eq!(got, want, "blocks of {bd:?}");
        if !case["cell_count"].is_null() {
            let cells = b::cells_for_bounds(bd, 15);
            assert_eq!(cells.len() as u64, case["cell_count"].as_u64().unwrap(), "{bd:?}");
            let first: Vec<_> = cells.into_iter().take(50).collect();
            let want: Vec<_> = case["cells"].as_array().unwrap().iter().map(tuple).collect();
            assert_eq!(first, want);
        }
    }
    for case in c["spans"].as_array().unwrap() {
        let (z, x, y) = (
            case["z"].as_i64().unwrap(),
            case["x"].as_i64().unwrap(),
            case["y"].as_i64().unwrap(),
        );
        assert_eq!(b::block_of(z, x, y), tuple(&case["block"]));
        let cells: Vec<_> = b::cells_of_block(z, x, y);
        let want: Vec<_> = case["cells"].as_array().unwrap().iter().map(tuple).collect();
        assert_eq!(cells, want);
    }
}

#[test]
fn a_dense_cell_is_cut_at_the_feature_cap() {
    let ring: Vec<Value> = [(0.0, 0.0), (0.0001, 0.0), (0.0001, 0.0001), (0.0, 0.0001)]
        .iter()
        .map(|(x, y)| json!({"lon": x, "lat": y}))
        .collect();
    let elements: Vec<Value> = (0..9000)
        .map(|i| json!({"type": "way", "id": i, "tags": {"building": "yes"}, "geometry": ring}))
        .collect();
    let out = b::to_geojson(&json!({"elements": elements}));
    assert_eq!(out["features"].as_array().unwrap().len(), b::MAX_FEATURES_PER_CELL);
}

fn service(dir: &std::path::Path, url: &str) -> b::BuildingService {
    let options = b::Options {
        url: url.into(),
        timeout: 5.0,
        user_agent: "CorvusTest".into(),
        workers: 1,
    };
    b::BuildingService::new(dir.join("b.sqlite"), Arc::new(Http), options).unwrap()
}

#[test]
fn the_service_fills_a_block_in_the_background_like_python() {
    let c = cases();
    let reply = c["geojson"][0]["overpass"].to_string();
    let stub = Stub::start(move |_| Reply::new(200, "application/json", reply.clone()));
    let dir = scratch("fill");
    let svc = service(&dir, &stub.url("/api"));
    let (payload, coming) = svc.lookup(15, 17430, 11376);
    assert!(payload.is_none());
    assert!(coming);
    wait_for("the block", || svc.cache().get(15, 17430, 11376).is_some());
    let s = &c["service"];
    assert_eq!(json!([false, true]), s["first"]);
    for (key, want) in s["stored"].as_object().unwrap() {
        let p: Vec<i64> = key.split('/').map(|v| v.parse().unwrap()).collect();
        wait_for("every cell", || svc.cache().get(p[0], p[1], p[2]).is_some());
        let (blob, _) = svc.cache().get(p[0], p[1], p[2]).unwrap();
        assert_eq!(&gunzip(&blob), want, "{key}");
    }
    let (after, coming) = svc.lookup(15, 17430, 11376);
    assert!(after.is_some());
    assert_eq!(coming, s["after_coming"].as_bool().unwrap());
    wait_for("an idle queue", || svc.stats()["queued"] == 0);
    assert_eq!(svc.stats()["queued"], 0);
    assert_eq!(svc.stats()["cells"], s["stats"]["cells"]);
    let seen = stub.requests();
    assert_eq!(seen.len(), 1);
    let want = &s["requests"][0];
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].target, want["path"].as_str().unwrap());
    assert_eq!(seen[0].header("content-type"), want["ctype"].as_str());
    assert_eq!(seen[0].header("user-agent"), Some("CorvusTest"));
    let body = String::from_utf8(seen[0].body.clone()).unwrap();
    let expected = format!(
        "data={}",
        corvus_core::mapdata::util::quote_plus(want["form"]["data"][0].as_str().unwrap())
    );
    assert_eq!(body, expected);
    svc.close();
    assert_eq!(svc.lookup(15, 1, 1), (None, false));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_refusal_backs_off_and_is_said_not_implied() {
    let stub = Stub::start(|_| Reply::new(429, "text/plain", "slow down"));
    let dir = scratch("backoff");
    let svc = service(&dir, &stub.url("/api"));
    assert_eq!(svc.lookup(15, 100, 100), (None, true));
    wait_for("the failed fetch", || stub.count() == 1 && svc.stats()["queued"] == 0);
    assert_eq!(svc.lookup(15, 300, 300), (None, false));
    assert_eq!(stub.count(), 1);
    assert_eq!(svc.cache().stats()["cells"], 0);
    svc.close();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_answer_cut_short_is_not_cached() {
    let stub = Stub::start(|_| Reply::json(&json!({"elements": [], "remark": "runtime error: Query timed out"})));
    let dir = scratch("remark");
    let svc = service(&dir, &stub.url("/api"));
    assert_eq!(svc.lookup(15, 100, 100), (None, true));
    wait_for("the answer", || stub.count() == 1 && svc.stats()["queued"] == 0);
    assert_eq!(svc.cache().stats()["cells"], 0);
    assert_eq!(svc.lookup(15, 500, 500), (None, false));
    svc.close();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_stale_cell_is_served_and_refreshed_behind_the_answer() {
    let stub = Stub::start(|_| Reply::json(&json!({"elements": []})));
    let dir = scratch("stale");
    let svc = service(&dir, &stub.url("/api"));
    let old = b::gzip_json(&json!({"type": "FeatureCollection", "features": [{"old": true}]}));
    svc.cache().put_at(15, 40, 40, &old, 1000.0);
    let (payload, coming) = svc.lookup(15, 40, 40);
    assert_eq!(payload.as_deref(), Some(&old[..]));
    assert!(!coming);
    wait_for("the refresh", || stub.count() == 1);
    wait_for("the new cell", || {
        svc.cache().get(15, 40, 40).is_some_and(|(_, at)| at > 1e9)
    });
    assert_eq!(gunzip(&svc.cache().get(15, 40, 40).unwrap().0), b::empty_collection());
    svc.close();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_download_prefetches_blocks_nearest_the_centre_first_and_skips_fresh_ones() {
    let stub = Stub::start(|_| Reply::json(&json!({"elements": []})));
    let dir = scratch("bulk");
    let svc = service(&dir, &stub.url("/api"));
    let bounds = [11.5, 48.1, 11.6, 48.2];
    let blocks: Vec<_> = b::blocks_outward(bounds).collect();
    assert!(blocks.len() > 4);
    let fresh = b::cells_of_block(blocks[0].0, blocks[0].1, blocks[0].2);
    for cell in &fresh {
        svc.cache()
            .put(cell.0, cell.1, cell.2, &b::gzip_json(&b::empty_collection()));
    }
    let queued = svc.prefetch(bounds, 3);
    assert_eq!(queued, 3);
    wait_for("the three blocks", || stub.count() == 3 && svc.stats()["queued"] == 0);
    for block in &blocks[1..4] {
        for cell in b::cells_of_block(block.0, block.1, block.2) {
            assert!(svc.cache().get(cell.0, cell.1, cell.2).is_some(), "{cell:?}");
        }
    }
    assert!(svc.cache().get(15, 2 * blocks[4].1, 2 * blocks[4].2).is_none());
    assert_eq!(svc.prefetch([f64::NAN, 0.0, 1.0, 1.0], 10), 0);
    assert_eq!(svc.prefetch(bounds, 0), 0);
    svc.close();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_queue_is_bounded_and_a_block_is_never_queued_twice() {
    let stub = Stub::start(|_| {
        std::thread::sleep(Duration::from_millis(300));
        Reply::json(&json!({"elements": []}))
    });
    let dir = scratch("depth");
    let svc = service(&dir, &stub.url("/api"));
    assert_eq!(svc.lookup(15, 2000, 2000), (None, true));
    assert_eq!(svc.lookup(15, 2001, 2001), (None, true));
    wait_for("the worker to take the first", || stub.count() >= 1);
    let mut accepted = 0;
    for i in 0..200 {
        if svc.lookup(15, 4 * i, 8000).1 {
            accepted += 1;
        }
    }
    assert!(accepted <= b::FETCH_QUEUE_DEPTH + 1, "{accepted}");
    assert!(accepted >= b::FETCH_QUEUE_DEPTH - 1, "{accepted}");
    svc.close();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn close_leaves_no_worker_waiting() {
    let dir = scratch("close");
    let svc = service(&dir, "http://127.0.0.1:9/never");
    let started = Instant::now();
    svc.close();
    svc.close();
    assert!(started.elapsed() < Duration::from_secs(2));
    let _ = std::fs::remove_dir_all(dir);
}
