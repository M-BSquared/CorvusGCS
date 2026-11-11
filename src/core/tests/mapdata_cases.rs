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
