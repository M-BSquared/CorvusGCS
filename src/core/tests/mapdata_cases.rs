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

mod sources {
    use super::cases;
    use base64::Engine;
    use corvus_core::mapdata::tile_sources as t;
    use serde_json::{json, Value};

    fn b64(s: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.decode(s).unwrap()
    }

    #[test]
    fn the_registry_lists_what_python_lists() {
        let c = cases("tile_sources");
        assert_eq!(Value::Array(t::list_sources()), c["sources"]);
        assert_eq!(Value::Array(t::list_terrain()), c["terrain"]);
        assert_eq!(Value::Array(t::list_providers()), c["providers"]);
        assert_eq!(
            json!([t::DEFAULT_TERRAIN, t::PREFERRED_TERRAIN, t::DEFAULT_PROVIDER]),
            c["defaults"]
        );
        for (id, want) in c["needs_token"].as_object().unwrap() {
            assert_eq!(t::needs_token(id), want.as_bool().unwrap(), "{id}");
        }
        for (id, want) in c["token_required"].as_object().unwrap() {
            assert_eq!(t::token_required(id), want.as_bool().unwrap(), "{id}");
        }
        assert_eq!(json!(t::keyed_providers()), c["keyed"]);
        for (id, want) in c["licensed"].as_object().unwrap() {
            let got = t::licensed(id).map_or(Value::Null, |l| {
                let mut o = json!({"api": l.api});
                if !l.map_type.is_empty() {
                    o["map_type"] = l.map_type.into();
                }
                if !l.imagery_set.is_empty() {
                    o["imagery_set"] = l.imagery_set.into();
                }
                if !l.layer_types.is_empty() {
                    o["layer_types"] = json!(l.layer_types);
                }
                o
            });
            assert_eq!(&got, want, "{id}");
        }
        for (id, want) in c["sparse"].as_object().unwrap() {
            assert_eq!(json!(t::sparse_above(id)), *want, "{id}");
        }
        for (id, want) in c["provider_of"].as_object().unwrap() {
            assert_eq!(json!(t::provider_of(id)), *want, "{id}");
        }
        for (id, want) in c["get"].as_object().unwrap() {
            assert_eq!(t::get(id).is_some(), want.as_bool().unwrap(), "{id}");
        }
        for r in c["resolve"].as_array().unwrap() {
            let style = r["style"].as_str();
            assert_eq!(
                json!(t::resolve_source(r["provider"].as_str().unwrap(), style)),
                r["id"],
                "{r}"
            );
        }
    }

    #[test]
    fn urls_keys_and_images_are_built_as_python_builds_them() {
        let c = cases("tile_sources");
        for u in c["urls"].as_array().unwrap() {
            let got = t::build_tile_url(
                u["template"].as_str().unwrap(),
                u["z"].as_i64().unwrap(),
                u["x"].as_i64().unwrap(),
                u["y"].as_i64().unwrap(),
                u["token"].as_str().unwrap(),
            );
            assert_eq!(got, u["url"].as_str().unwrap(), "{u}");
        }
        for r in c["redact"].as_array().unwrap() {
            assert_eq!(
                t::redact_url(r["in"].as_str().unwrap()),
                r["out"].as_str().unwrap(),
                "{r}"
            );
        }
        for i in c["images"].as_array().unwrap() {
            assert_eq!(
                t::looks_like_image(&b64(i["blob"].as_str().unwrap())),
                i["image"].as_bool().unwrap(),
                "{i}"
            );
        }
        for q in c["quadkeys"].as_array().unwrap() {
            let (z, x, y) = (
                q["z"].as_i64().unwrap(),
                q["x"].as_i64().unwrap(),
                q["y"].as_i64().unwrap(),
            );
            assert_eq!(t::quadkey(z, x, y), q["key"].as_str().unwrap());
            assert_eq!(t::server_shard(x, y), q["shard"].as_str().unwrap());
        }
    }
}

mod cache {
    use super::cases;
    use base64::Engine;
    use corvus_core::mapdata::tile_cache::{xyz_to_tms, Region, TileCache};
    use serde_json::{json, Value};

    fn b64(s: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.decode(s).unwrap()
    }

    fn enc(b: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(b)
    }

    fn tuple(v: &Value, n: usize) -> Vec<i64> {
        v.as_array()
            .unwrap()
            .iter()
            .take(n)
            .map(|x| x.as_i64().unwrap())
            .collect()
    }

    fn normalise(mut v: Value) -> Value {
        if let Value::Array(rows) = &mut v {
            for r in rows {
                if r["id"] == "" {
                    r["created_at"] = "<now>".into();
                }
            }
        }
        v
    }

    fn run(cache: &TileCache, op: &Value) -> Value {
        let i = |k: &str| op[k].as_i64().unwrap();
        match op["op"].as_str().unwrap() {
            "put_tile" => {
                cache
                    .put_tile(i("z"), i("x"), i("y"), &b64(op["blob"].as_str().unwrap()))
                    .unwrap();
                Value::Null
            }
            "get_tile" => cache
                .get_tile(i("z"), i("x"), i("y"))
                .unwrap()
                .map_or(Value::Null, |b| enc(&b).into()),
            "has_tile" => cache.has_tile(i("z"), i("x"), i("y")).into(),
            "put_tiles" => {
                let tiles: Vec<_> = op["tiles"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| {
                        let a = tuple(t, 3);
                        (a[0], a[1], a[2], b64(t[3].as_str().unwrap()))
                    })
                    .collect();
                cache.put_tiles(&tiles).unwrap().into()
            }
            "count_tiles" => {
                let r: Vec<_> = op["ranges"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| {
                        let a = tuple(r, 5);
                        (a[0], a[1], a[2], a[3], a[4])
                    })
                    .collect();
                cache.count_tiles(&r).into()
            }
            "stats" => cache.stats(),
            "delete_tiles" => {
                let t: Vec<_> = op["tiles"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| {
                        let a = tuple(t, 3);
                        (a[0], a[1], a[2])
                    })
                    .collect();
                cache.delete_tiles(&t).into()
            }
            "set_metadata" => {
                cache.set_metadata(op["name"].as_str().unwrap(), op["value"].as_str().unwrap());
                Value::Null
            }
            "get_metadata" => json!(cache.get_metadata(op["name"].as_str().unwrap())),
            "bounds" => cache.bounds().map_or(Value::Null, |(w, s, e, n)| json!([w, s, e, n])),
            "add_region" => {
                let mut v = cache.add_region(&op["region"]).to_json();
                // add_region answers the flat row, list_regions the nested one
                let b = v["bounds"].take();
                for k in ["w", "s", "e", "n"] {
                    v[k] = b[k].clone();
                }
                v.as_object_mut().unwrap().shift_remove("bounds");
                // the flat row has the Python key order
                let o = v.as_object().unwrap();
                let order = [
                    "id",
                    "name",
                    "source",
                    "w",
                    "s",
                    "e",
                    "n",
                    "minzoom",
                    "maxzoom",
                    "tile_count",
                    "created_at",
                    "state",
                ];
                let mut out = serde_json::Map::new();
                for k in order {
                    out.insert(k.into(), o[k].clone());
                }
                normalise(json!([Value::Object(out)]))[0].clone()
            }
            "list_regions" => normalise(Value::Array(cache.list_regions().iter().map(Region::to_json).collect())),
            "get_region" => cache
                .get_region(op["id"].as_str().unwrap())
                .map_or(Value::Null, |r| r.to_json()),
            "update_region" => {
                let f: Vec<(&str, Value)> = op["fields"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.clone()))
                    .collect();
                cache.update_region(op["id"].as_str().unwrap(), &f).into()
            }
            "remove_region" => cache.remove_region(op["id"].as_str().unwrap()).into(),
            other => panic!("{other}"),
        }
    }

    #[test]
    fn the_same_operations_give_the_same_answers() {
        let c = cases("tile_cache");
        let dir = tempdir("replay");
        let cache = TileCache::new(dir.join("satellite.mbtiles")).unwrap();
        for step in c["steps"].as_array().unwrap() {
            let got = run(&cache, &step["op"]);
            assert_eq!(got, step["result"], "{}", step["op"]);
        }
    }

    #[test]
    fn a_file_python_wrote_is_read_and_one_rust_wrote_is_a_valid_mbtiles() {
        let c = cases("tile_cache");
        let dir = tempdir("pyfile");
        let path = dir.join("copy.mbtiles");
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/mapdata_cache_python.mbtiles"),
            &path,
        )
        .unwrap();
        let cache = TileCache::new(&path).unwrap();
        assert_eq!(cache.stats(), c["final"]["stats"]);
        assert_eq!(
            enc(&cache.get_tile(3, 4, 3).unwrap().unwrap()),
            c["final"]["tile"].as_str().unwrap()
        );
        assert_eq!(cache.get_metadata("bounds").as_deref(), c["final"]["bounds"].as_str());
        assert_eq!(xyz_to_tms(5, 1), c["final"]["tms"].as_i64().unwrap());
        let regions = normalise(Value::Array(cache.list_regions().iter().map(Region::to_json).collect()));
        let want: Vec<Value> = c["final"]["regions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let mut r = r.clone();
                if r["id"] == "" {
                    r["created_at"] = "<now>".into();
                }
                r
            })
            .collect();
        assert_eq!(regions, Value::Array(want));
        // The row layout in the file is the MBTiles one: TMS rows.
        cache.put_tile(2, 1, 0, b"abc").unwrap();
        cache.close();
        cache.close();
        assert_eq!(cache.get_tile(2, 1, 0).unwrap(), None);
        let raw = rusqlite::Connection::open(&path).unwrap();
        let row: i64 = raw
            .query_row(
                "SELECT tile_row FROM tiles WHERE zoom_level=2 AND tile_column=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row, 3);
    }

    #[test]
    fn a_region_record_keeps_its_defaults() {
        let r = Region::clean(&json!({"id": "x", "created_at": "2026-01-01T00:00:00Z"}));
        assert_eq!((r.name.as_str(), r.state.as_str(), r.maxzoom), ("Region", "done", 0));
        assert_eq!(Region::clean(&json!("text")).created_at.len(), 20);
    }

    pub fn tempdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "corvus-mapdata-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}

mod sessions {
    use super::stub_http::{Reply, Stub};
    use corvus_core::mapdata::fetch::Http;
    use corvus_core::mapdata::tile_sessions::{Endpoints, Sessions};
    use corvus_core::mapdata::tile_sources as ts;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    type Answer = Arc<Mutex<Box<dyn Fn() -> Reply + Send>>>;

    fn google_ok(session: &str, ttl: i64) -> Value {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        json!({"session": session, "expiry": (now + ttl).to_string()})
    }

    fn bing_ok(url: &str, subdomains: Value) -> Value {
        json!({"resourceSets": [{"resources": [{"imageUrl": url, "imageUrlSubdomains": subdomains}]}]})
    }

    const BING_URL: &str =
        "https://ecn.{subdomain}.tiles.virtualearth.net/tiles/a{quadkey}.jpeg?g=14237&mkt={culture}&n=z";

    fn setup(answer: impl Fn() -> Reply + Send + 'static) -> (Stub, Sessions, Answer) {
        let slot: Answer = Arc::new(Mutex::new(Box::new(answer)));
        let s2 = slot.clone();
        let stub = Stub::start(move |_| (s2.lock().unwrap())());
        let ends = Endpoints {
            google_session: stub.url("/google/createSession?key={k}"),
            google_tiles: ts::GOOGLE_TILES_URL.to_string(),
            bing_metadata: stub.url("/bing/{imagery_set}?output=json&uriScheme=https&key={k}"),
        };
        let sessions = Sessions::with("CorvusGCS/test", ends);
        (stub, sessions, slot)
    }

    #[test]
    fn a_google_session_asks_for_the_right_layer() {
        for (sid, map_type, layers) in [
            ("google_satellite", "satellite", None),
            ("google_streets", "roadmap", None),
            ("google_hybrid", "satellite", Some("layerRoadmap")),
            ("google_topo", "terrain", Some("layerRoadmap")),
        ] {
            let (stub, s, _) = setup(|| Reply::json(&google_ok("sess-1", 3600 * 20)));
            let template = s.resolve(&Http, sid, "AIzaKEY").unwrap();
            let seen = &stub.requests()[0];
            assert_eq!(seen.method, "POST");
            assert_eq!(seen.target, "/google/createSession?key=AIzaKEY");
            let body: Value = serde_json::from_slice(&seen.body).unwrap();
            assert_eq!(body["mapType"], map_type);
            assert_eq!(body.get("layerTypes").and_then(|v| v[0].as_str()), layers);
            assert_eq!(seen.header("content-type"), Some("application/json"));
            assert_eq!(seen.header("user-agent"), Some("CorvusGCS/test"));
            assert_eq!(
                ts::build_tile_url(&template, 12, 2200, 1400, "AIzaKEY"),
                "https://tile.googleapis.com/v1/2dtiles/12/2200/1400?session=sess-1&key=AIzaKEY"
            );
        }
    }

    #[test]
    fn a_session_is_reused_replaced_on_invalidate_and_never_adds_url_structure() {
        let (stub, s, _) = setup(|| Reply::json(&google_ok("a&key=evil#{z}/..", 3600 * 20)));
        let first = s.resolve(&Http, "google_satellite", "k1").unwrap();
        assert_eq!(s.resolve(&Http, "google_satellite", "k1").unwrap(), first);
        assert_eq!(stub.count(), 1);
        s.resolve(&Http, "google_streets", "k1").unwrap();
        s.resolve(&Http, "google_satellite", "k2").unwrap();
        assert_eq!(stub.count(), 3);
        s.invalidate("google_satellite", "k1");
        s.resolve(&Http, "google_satellite", "k1").unwrap();
        assert_eq!(stub.count(), 4);
        let url = ts::build_tile_url(&first, 1, 0, 0, "k");
        assert_eq!(
            url.split_once('?').unwrap().1,
            "session=a%26key%3Devil%23%7Bz%7D%2F..&key=k"
        );
        let safe = ts::redact_url(&url);
        assert!(!safe.contains("evil"), "{safe}");
    }

    #[test]
    fn concurrent_tiles_open_one_session() {
        let (stub, s, _) = setup(|| {
            std::thread::sleep(std::time::Duration::from_millis(150));
            Reply::json(&google_ok("sess", 3600 * 20))
        });
        let s = Arc::new(s);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let s = s.clone();
                std::thread::spawn(move || s.resolve(&Http, "google_satellite", "k").unwrap())
            })
            .collect();
        let results: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(results.iter().all(|r| r == &results[0]));
        assert_eq!(stub.count(), 1);
    }

    #[test]
    fn refusals_are_remembered_and_offline_is_told_apart() {
        let (stub, s, _) = setup(|| Reply::new(403, "application/json", "{}"));
        for _ in 0..3 {
            let e = s.resolve(&Http, "google_satellite", "bad").unwrap_err();
            assert!(!e.offline);
            assert!(e.message.contains("403") && !e.message.contains("bad"), "{e}");
        }
        assert_eq!(stub.count(), 1, "a refused key must not be retried per tile");
        let dead = Sessions::with(
            "t",
            Endpoints {
                google_session: "http://127.0.0.1:1/x?key={k}".into(),
                google_tiles: ts::GOOGLE_TILES_URL.into(),
                bing_metadata: "http://127.0.0.1:1/{imagery_set}?key={k}".into(),
            },
        );
        assert!(dead.resolve(&Http, "google_satellite", "k").unwrap_err().offline);
        let e = dead.resolve(&Http, "satellite", "k").unwrap_err();
        assert!(e.message.contains("no licensed API"));
    }

    #[test]
    fn an_expired_or_empty_session_is_refused() {
        let (_stub, s, slot) = setup(|| Reply::json(&google_ok("s", 60)));
        let e = s.resolve(&Http, "google_satellite", "k").unwrap_err();
        assert!(!e.offline);
        *slot.lock().unwrap() = Box::new(|| Reply::json(&json!({"session": ""})));
        s.clear();
        assert_eq!(
            s.resolve(&Http, "google_satellite", "k").unwrap_err().message,
            "the map service opened no session"
        );
        *slot.lock().unwrap() = Box::new(|| Reply::new(200, "application/json", "[1]"));
        s.clear();
        assert_eq!(
            s.resolve(&Http, "google_satellite", "k").unwrap_err().message,
            "the map service answer was not an object"
        );
        *slot.lock().unwrap() = Box::new(|| Reply::new(200, "application/json", "no"));
        s.clear();
        assert_eq!(
            s.resolve(&Http, "google_satellite", "k").unwrap_err().message,
            "the map service answer was not JSON"
        );
    }

    #[test]
    fn bing_metadata_becomes_a_quadkey_template() {
        for (sid, set) in [
            ("bing_satellite", "Aerial"),
            ("bing_streets", "RoadOnDemand"),
            ("bing_hybrid", "AerialWithLabelsOnDemand"),
        ] {
            let (stub, s, _) = setup(|| Reply::json(&bing_ok(BING_URL, json!(["t0", "t1", "t2", "t3"]))));
            let template = s.resolve(&Http, sid, "BingKey").unwrap();
            assert_eq!(
                stub.requests()[0].target,
                format!("/bing/{set}?output=json&uriScheme=https&key=BingKey")
            );
            let tile = ts::build_tile_url(&template, 3, 4, 5, "BingKey");
            let (shard, q) = (ts::server_shard(4, 5), ts::quadkey(3, 4, 5));
            assert_eq!(
                tile,
                format!("https://ecn.t{shard}.tiles.virtualearth.net/tiles/a{q}.jpeg?g=14237&mkt=en-US&n=z")
            );
            assert!(!tile.contains("BingKey"));
        }
        let (_s, s, _) = setup(|| Reply::json(&bing_ok(BING_URL, json!(["a", "b"]))));
        assert!(s
            .resolve(&Http, "bing_satellite", "k")
            .unwrap()
            .starts_with("https://ecn.a.tiles.virtualearth.net/"));
    }

    #[test]
    fn a_bing_address_corvus_will_not_use_is_refused() {
        for url in [
            "http://ecn.{subdomain}.tiles.virtualearth.net/tiles/a{quadkey}.jpeg",
            "https://evil.example/tiles/a{quadkey}.jpeg",
            "https://ecn.{subdomain}.tiles.virtualearth.net/tiles/a{quadkey}.jpeg?x={unknown}",
            "https://ecn.{subdomain}.tiles.virtualearth.net/tiles/static.jpeg",
        ] {
            let (_stub, s, _) = setup(move || Reply::json(&bing_ok(url, json!(["t0", "t1", "t2", "t3"]))));
            let e = s.resolve(&Http, "bing_satellite", "k").unwrap_err();
            assert!(!e.offline, "{url}");
        }
        for answer in [
            json!({}),
            json!({"resourceSets": []}),
            json!({"resourceSets": [{"resources": [{}]}]}),
        ] {
            let (_stub, s, _) = setup(move || Reply::json(&answer));
            assert!(s.resolve(&Http, "bing_satellite", "k").is_err());
        }
        let (_stub, s, _) = setup(|| {
            Reply::json(&bing_ok(
                "https://ecn.{subdomain}.tiles.virtualearth.net/a{quadkey}",
                json!([]),
            ))
        });
        assert_eq!(
            s.resolve(&Http, "bing_satellite", "k").unwrap_err().message,
            "the map service sent no tile servers"
        );
    }
}

mod proxy {
    use super::stub_http::{Reply, Stub};
    use corvus_core::mapdata::fetch::Http;
    use corvus_core::mapdata::tile_proxy::{content_type, TileProxy};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nxxxx";

    fn none(_: &str) -> String {
        String::new()
    }

    /// Source ids use the real registry, whose upstreams are on the internet,
    /// so a test points a custom template through the network layer instead.
    struct Redirect {
        to: String,
    }

    impl corvus_core::mapdata::fetch::Fetch for Redirect {
        fn send(
            &self,
            req: &corvus_core::mapdata::fetch::Req<'_>,
        ) -> Result<corvus_core::mapdata::fetch::Fetched, corvus_core::mapdata::fetch::NetError> {
            let path = req.url.splitn(4, '/').nth(3).unwrap_or("");
            let url = format!("{}/{path}", self.to);
            Http.send(&corvus_core::mapdata::fetch::Req {
                url: &url,
                ..req.clone()
            })
        }
    }

    fn redirected(stub: &Stub) -> TileProxy {
        let mut p = TileProxy::new(
            Arc::new(Redirect {
                to: format!("http://{}", stub.addr),
            }),
            "CorvusGCS/test",
        );
        p.timeout = Duration::from_secs(2);
        p
    }

    #[test]
    fn a_tile_is_asked_with_the_right_url_and_identity() {
        let stub = Stub::start(|_| Reply::new(200, "image/png", PNG));
        let p = redirected(&stub);
        let got = p.fetch_upstream("osm", 5, 17, 11, &none).unwrap();
        assert_eq!(got, PNG);
        let seen = &stub.requests()[0];
        assert_eq!(seen.target, "/5/17/11.png");
        assert_eq!(seen.header("user-agent"), Some("CorvusGCS/test"));
        let got = p.fetch_upstream("satellite", 5, 17, 11, &none).unwrap();
        assert_eq!(got, PNG);
        assert_eq!(
            stub.requests()[1].target,
            "/ArcGIS/rest/services/World_Imagery/MapServer/tile/5/11/17"
        );
    }

    #[test]
    fn an_offline_viewport_stops_hammering_and_a_probe_recovers() {
        let alive = Arc::new(Mutex::new(false));
        let a2 = alive.clone();
        let stub = Stub::start(move |_| {
            if *a2.lock().unwrap() {
                Reply::new(200, "image/png", PNG)
            } else {
                Reply::new(200, "text/html", "<html>portal</html>")
            }
        });
        let p = redirected(&stub);
        for i in 0..10 {
            assert!(p.fetch_upstream("osm", 4, i, 0, &none).is_none());
        }
        assert_eq!(
            stub.count(),
            3,
            "three failures trip the breaker, then misses are instant"
        );
        assert!(p.breaker.is_open());
        *alive.lock().unwrap() = true;
        p.breaker.reset();
        assert!(p.fetch_upstream("osm", 4, 1, 0, &none).is_some());
        assert!(!p.breaker.is_open());
    }

    #[test]
    fn a_status_answer_never_opens_the_breaker() {
        let stub = Stub::start(|_| Reply::new(429, "text/plain", "slow"));
        let p = redirected(&stub);
        for i in 0..6 {
            assert!(p.fetch_upstream("osm", 4, i, 0, &none).is_none());
        }
        assert_eq!(stub.count(), 6);
        assert!(!p.breaker.is_open());
    }

    #[test]
    fn a_keyed_service_without_a_key_asks_nobody_and_keeps_the_breaker_closed() {
        let stub = Stub::start(|_| Reply::new(200, "image/png", PNG));
        let p = redirected(&stub);
        for i in 0..6 {
            assert!(p.fetch_upstream("mapbox_streets", 4, i, 0, &none).is_none());
        }
        assert_eq!(stub.count(), 0);
        assert!(!p.breaker.is_open());
        let tok = |provider: &str| {
            if provider == "mapbox" {
                "pk.k".to_string()
            } else {
                String::new()
            }
        };
        assert!(p.fetch_upstream("mapbox_streets", 4, 1, 0, &tok).is_some());
        assert!(stub.requests()[0].target.contains("access_token=pk.k"));
    }

    #[test]
    fn a_regional_elevation_remembers_a_missing_fine_tile() {
        let stub = Stub::start(|_| Reply::new(404, "text/plain", "no"));
        let p = redirected(&stub);
        assert!(p.fetch_upstream("copernicus", 14, 100, 100, &none).is_none());
        assert_eq!(stub.count(), 1);
        // The tile below it, and the same one again, are answered without asking.
        assert!(p.fetch_upstream("copernicus", 15, 200, 200, &none).is_none());
        assert!(p.fetch_upstream("copernicus", 14, 100, 100, &none).is_none());
        assert_eq!(stub.count(), 1);
        // At or above the covered level a 404 is not remembered.
        assert!(p.fetch_upstream("copernicus", 12, 10, 10, &none).is_none());
        assert!(p.fetch_upstream("copernicus", 12, 10, 10, &none).is_none());
        assert_eq!(stub.count(), 3);
    }

    #[test]
    fn a_dead_network_counts_as_a_failure_and_unknown_sources_are_none() {
        let mut p = TileProxy::new(
            Arc::new(Redirect {
                to: "http://127.0.0.1:1".into(),
            }),
            "t",
        );
        p.timeout = Duration::from_millis(500);
        assert!(p.fetch_upstream("osm", 1, 0, 0, &none).is_none());
        assert!(p.fetch_upstream("nope", 1, 0, 0, &none).is_none());
        assert!(!p.breaker.is_open());
        for _ in 0..2 {
            p.fetch_upstream("osm", 1, 0, 0, &none);
        }
        assert!(p.breaker.is_open());
    }

    #[test]
    fn the_content_type_comes_from_the_bytes() {
        assert_eq!(content_type(PNG), "image/png");
        assert_eq!(content_type(b"\xff\xd8\xff\xe0"), "image/jpeg");
        assert_eq!(content_type(b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
        assert_eq!(content_type(b"GIF89a"), "image/png");
    }
}
