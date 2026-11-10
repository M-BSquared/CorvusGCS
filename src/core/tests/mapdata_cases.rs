//! The map data area holds to what the Python modules answer
//! (`python/tools/dump_mapdata_cases.py`).

use corvus_core::mapdata::{crs, geocode};
use serde_json::Value;

fn cases(name: &str) -> Value {
    let path = format!("{}/tests/data/mapdata_{name}_cases.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() <= 1e-9 + 1e-10 * b.abs(), "{what}: {a} vs {b}");
}

#[test]
fn coordinate_systems_answer_what_python_answers() {
    let c = cases("crs");
    assert_eq!(crs::SUPPORTED_HINT, c["hint"].as_str().unwrap());
    for entry in c["codes"].as_array().unwrap() {
        let code = entry["epsg"].as_i64().unwrap();
        let found = crs::get(code);
        if !entry["known"].as_bool().unwrap() {
            assert!(found.is_none(), "{code}");
            continue;
        }
        let sys = found.unwrap_or_else(|| panic!("{code} is unknown"));
        assert_eq!(sys.name, entry["name"].as_str().unwrap(), "{code}");
        assert_eq!(sys.geographic, entry["geographic"].as_bool().unwrap(), "{code}");
        for row in entry["rows"].as_array().unwrap() {
            let (lon, lat) = (row["lon"].as_f64().unwrap(), row["lat"].as_f64().unwrap());
            let (a, b) = sys.from_lonlat(lon, lat);
            let want = row["axes"].as_array().unwrap();
            close(a, want[0].as_f64().unwrap(), &format!("{code} east at {lon},{lat}"));
            close(b, want[1].as_f64().unwrap(), &format!("{code} north at {lon},{lat}"));
            let (x, y) = sys.to_lonlat(a, b);
            let back = row["back"].as_array().unwrap();
            close(x, back[0].as_f64().unwrap(), &format!("{code} lon back at {lon},{lat}"));
            close(y, back[1].as_f64().unwrap(), &format!("{code} lat back at {lon},{lat}"));
        }
    }
}

#[test]
fn place_search_rows_queries_and_cells_match_python() {
    let c = cases("geocode");
    for case in c["parse"].as_array().unwrap() {
        let rows = geocode::parse_results(&case["payload"], case["limit"].as_u64().unwrap() as usize);
        assert_eq!(Value::Array(rows), case["rows"], "payload {}", case["payload"]);
    }
    for case in c["cells"].as_array().unwrap() {
        let near = case["near"].as_array().map(|a| geocode::Near {
            lon: a[0].as_f64().unwrap(),
            lat: a[1].as_f64().unwrap(),
        });
        let cell = geocode::cache_cell(near).map(|(a, b)| serde_json::json!([a, b]));
        assert_eq!(cell.unwrap_or(Value::Null), case["cell"], "near {:?}", case["near"]);
    }
    for case in c["queries"].as_array().unwrap() {
        let near = case["near"].as_array().map(|a| geocode::Near {
            lon: a[0].as_f64().unwrap(),
            lat: a[1].as_f64().unwrap(),
        });
        let q = geocode::build_query(case["q"].as_str().unwrap(), case["limit"].as_i64().unwrap(), near);
        assert_eq!(q, case["query"].as_str().unwrap());
    }
    let d = &c["defaults"];
    assert_eq!(geocode::MIN_INTERVAL_S, d["min_interval"].as_f64().unwrap());
    assert_eq!(geocode::MAX_WAIT_S, d["max_wait"].as_f64().unwrap());
    assert_eq!(geocode::MAX_RESULTS, d["max_results"].as_i64().unwrap());
    assert_eq!(geocode::DEFAULT_RESULTS, d["default_results"].as_i64().unwrap());
    assert_eq!(geocode::SEARCH_URL, d["url"].as_str().unwrap());
    assert_eq!(geocode::OFFLINE_BACKOFF_S, d["backoff"].as_f64().unwrap());
}

#[path = "support/stub_http.rs"]
mod stub_http;

mod service {
    use super::stub_http::{Reply, Stub};
    use corvus_core::mapdata::fetch::Http;
    use corvus_core::mapdata::geocode::{Geocoder, Limits, Near};
    use serde_json::json;
    use std::time::Duration;

    fn payload() -> serde_json::Value {
        json!([{"lat": "48.35", "lon": "11.78", "display_name": "Munich Airport", "type": "aerodrome"}])
    }

    fn quick() -> Limits {
        Limits {
            min_interval: 0.05,
            max_wait: 0.2,
            offline_backoff: 0.3,
            timeout: 2.0,
        }
    }

    fn no_wait(d: Duration) -> bool {
        std::thread::sleep(d);
        false
    }

    #[test]
    fn a_search_is_asked_once_cached_and_biased() {
        let stub = Stub::start(|_| Reply::json(&payload()));
        let g = Geocoder::with("CorvusGCS/test", &stub.url("/search"), quick());
        let near = Some(Near { lon: 11.5, lat: 48.1 });
        let rows = g.search(&Http, "  Munich   airport ", Some(3), near, &no_wait).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["label"], "Munich Airport");
        let again = g.search(&Http, "munich AIRPORT", Some(3), near, &no_wait).unwrap();
        assert_eq!(again, rows);
        assert_eq!(stub.count(), 1);
        let seen = &stub.requests()[0];
        assert_eq!(seen.header("user-agent"), Some("CorvusGCS/test"));
        assert!(seen.target.contains("q=Munich+airport"), "{}", seen.target);
        assert!(
            seen.target.contains("viewbox=9.50000%2C50.10000%2C13.50000%2C46.10000"),
            "{}",
            seen.target
        );
        assert!(seen.target.contains("limit=3"));
        assert_eq!(
            g.search(&Http, "   ", None, None, &no_wait).unwrap(),
            Vec::<serde_json::Value>::new()
        );
        // The same text with no bias is another search.
        g.search(&Http, "munich airport", Some(3), None, &no_wait).unwrap();
        assert_eq!(stub.count(), 2);
    }

    #[test]
    fn a_refusal_backs_off_and_names_the_reason() {
        let stub = Stub::start(|_| Reply::new(429, "text/plain", "slow down"));
        let g = Geocoder::with("t", &stub.url("/search"), quick());
        let e = g.search(&Http, "a", None, None, &no_wait).unwrap_err();
        assert_eq!(e.0, "The place-search service refused the request.");
        let e = g.search(&Http, "b", None, None, &no_wait).unwrap_err();
        assert_eq!(e.0, "No connection to the place-search service.");
        assert_eq!(stub.count(), 1);
    }

    #[test]
    fn a_dead_service_and_a_bad_answer_are_sentences() {
        let g = Geocoder::with("t", "http://127.0.0.1:1/search", quick());
        let e = g.search(&Http, "a", None, None, &no_wait).unwrap_err();
        assert_eq!(e.0, "No connection to the place-search service.");
        let stub = Stub::start(|_| Reply::new(200, "text/plain", "not json"));
        let g = Geocoder::with("t", &stub.url("/search"), quick());
        let e = g.search(&Http, "a", None, None, &no_wait).unwrap_err();
        assert_eq!(e.0, "The place-search service sent an unusable answer.");
    }

    #[test]
    fn a_queue_longer_than_the_ceiling_is_refused_without_taking_a_slot() {
        let stub = Stub::start(|_| Reply::json(&payload()));
        let slow = Limits {
            min_interval: 1.0,
            max_wait: 0.5,
            offline_backoff: 0.3,
            timeout: 2.0,
        };
        let g = Geocoder::with("t", &stub.url("/search"), slow);
        g.search(&Http, "one", None, None, &no_wait).unwrap();
        let e = g.search(&Http, "two", None, None, &no_wait).unwrap_err();
        assert_eq!(e.0, "Place search is busy. Try that again.");
        assert_eq!(stub.count(), 1);
    }
}
