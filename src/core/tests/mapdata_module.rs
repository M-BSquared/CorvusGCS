//! The map data module through its routes, against stub upstreams: tiles, the
//! cache, downloads, regions, elevation models, buildings and the place search.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corvus_core::mapdata::dem_tiles;
use corvus_core::mapdata::fetch::{Fetch, Fetched, Http, NetError, Req};
use corvus_core::response::{Body, SseNext};
use corvus_core::{Core, CoreBuilder, MapData, MapDataModule, MapDataOptions, Method, Options, Request, Response};
use serde_json::{json, Value};

#[path = "support/stub_http.rs"]
mod stub_http;
use stub_http::{Reply, Stub};

type Handler = dyn Fn(&str) -> Result<Fetched, NetError> + Send + Sync;

struct Net {
    handler: Box<Handler>,
    seen: Mutex<Vec<String>>,
}

impl Net {
    fn new(f: impl Fn(&str) -> Result<Fetched, NetError> + Send + Sync + 'static) -> Arc<Net> {
        Arc::new(Net {
            handler: Box::new(f),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn offline() -> Arc<Net> {
        Net::new(|_| Err(NetError("offline".into())))
    }

    fn urls(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Fetch for Net {
    fn send(&self, req: &Req<'_>) -> Result<Fetched, NetError> {
        if req.url.starts_with("http://127.0.0.1") {
            return Http.send(req);
        }
        self.seen.lock().unwrap().push(req.url.to_string());
        (self.handler)(req.url)
    }
}

fn ok(body: Vec<u8>) -> Result<Fetched, NetError> {
    Ok(Fetched {
        status: 200,
        body,
        headers: vec![],
    })
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("corvus-mapdata-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn start(dir: &std::path::Path, net: Arc<Net>, tweak: impl FnOnce(&mut MapDataOptions)) -> Core {
    let mut options = MapDataOptions {
        net: Some(net),
        cache_dir: Some(dir.join("tiles")),
        ..MapDataOptions::default()
    };
    tweak(&mut options);
    let core = CoreBuilder::new(Options::in_dir(dir.join("state")))
        .module(MapDataModule::with_options(options))
        .build()
        .unwrap();
    core.start().unwrap();
    core
}

fn data(core: &Core) -> Arc<corvus_core::mapdata::Resources> {
    core.service::<MapData>().unwrap().resources().unwrap()
}

fn terrain_png(base: u32) -> Vec<u8> {
    let values: Vec<u32> = (0..256 * 256u32)
        .map(|i| 8_388_608 + base + (i % 256) * 3 + (i / 256) * 5)
        .collect();
    dem_tiles::encode(256, 256, &values)
}

fn image_png() -> Vec<u8> {
    dem_tiles::encode(2, 2, &[1, 2, 3, 4])
}

fn get(core: &Core, target: &str) -> Response {
    core.dispatch(Request::new(Method::Get, target))
}

fn bytes(r: Response) -> (u16, Vec<(String, String)>, Vec<u8>) {
    match r.body {
        Body::Bytes(b) => (r.status, r.headers, b),
        _ => (r.status, r.headers, Vec::new()),
    }
}

fn header<'a>(h: &'a [(String, String)], name: &str) -> Option<&'a str> {
    h.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn call(core: &Core, method: &str, path: &str, body: Value) -> (u16, Value) {
    core.internal_call(method, path, body.as_object().cloned())
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_sources_route_lists_what_python_lists() {
    let dir = scratch("sources");
    let core = start(&dir, Net::offline(), |_| {});
    let (status, body) = call(&core, "GET", "/api/tiles/sources", json!({}));
    assert_eq!(status, 200);
    for key in [
        "sources",
        "providers",
        "default_provider",
        "terrain",
        "default_terrain",
        "preferred_terrain",
        "buildings",
        "max_tiles_per_job",
    ] {
        assert!(body.get(key).is_some(), "{key}");
    }
    assert_eq!(body["max_tiles_per_job"], 50000);
    let first = &body["sources"][0];
    assert_eq!(first["minzoom"], 0);
    assert_eq!(first["cached_count"], 0);
    assert!(first["cached_minzoom"].is_null());
    let keys: Vec<&str> = body["terrain"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "id",
            "label",
            "provider",
            "encoding",
            "tile_size",
            "maxzoom",
            "sparse_above",
            "attribution",
            "minzoom",
            "cached_count",
            "cached_minzoom",
            "cached_maxzoom",
            "token_required",
            "token_set"
        ]
    );
    assert_eq!(body["buildings"]["cells"], 0);
    assert!(body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["token_set"] == false));
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_key_is_stored_checked_and_reported_but_never_returned() {
    let dir = scratch("token");
    let core = start(&dir, Net::offline(), |_| {});
    for (body, status, error) in [
        (json!({}), 400, "provider must be a non-empty string"),
        (json!({"provider": ""}), 400, "provider must be a non-empty string"),
        (
            json!({"provider": "nobody"}),
            400,
            "'nobody' is not a keyed map service",
        ),
        (json!({"provider": "mapbox", "token": 5}), 400, "token must be a string"),
        (
            json!({"provider": "mapbox", "token": "a b"}),
            400,
            "that does not look like an API key. Use letters, digits, . _ ~ - only, with no spaces or quotes",
        ),
        (
            json!({"provider": "mapbox", "token": "k".repeat(513)}),
            400,
            "that key is longer than 512 characters",
        ),
    ] {
        let (s, b) = call(&core, "POST", "/api/tiles/token", body.clone());
        assert_eq!(
            (s, b["error"].as_str().unwrap(), &b["ok"]),
            (status, error, &json!(false)),
            "{body}"
        );
    }
    let (s, b) = call(
        &core,
        "POST",
        "/api/tiles/token",
        json!({"provider": "mapbox", "token": "  pk.abc-1_2~3 "}),
    );
    assert_eq!(
        (s, b),
        (200, json!({"ok": true, "provider": "mapbox", "token_set": true}))
    );
    let saved = std::fs::read_to_string(dir.join("state/config.json")).unwrap();
    assert!(saved.contains("pk.abc-1_2~3"));
    let (_, body) = call(&core, "GET", "/api/tiles/sources", json!({}));
    let mapbox = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "mapbox")
        .unwrap();
    assert_eq!(mapbox["token_set"], true);
    assert!(!body.to_string().contains("pk.abc"));
    let (s, b) = call(
        &core,
        "POST",
        "/api/tiles/token",
        json!({"provider": "mapbox", "token": ""}),
    );
    assert_eq!((s, b["token_set"].clone()), (200, json!(false)));
    let (s, _) = call(&core, "POST", "/api/tiles/token", json!({"provider": "mapbox"}));
    assert_eq!(s, 200);
    assert!(!std::fs::read_to_string(dir.join("state/config.json"))
        .unwrap()
        .contains("pk.abc"));
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_tile_is_filled_from_upstream_once_and_then_served_from_the_cache() {
    let dir = scratch("serve");
    let net = Net::new(|_| ok(image_png()));
    let core = start(&dir, net.clone(), |_| {});
    let (s, h, b) = bytes(get(&core, "/api/tiles/satellite/3/4/5.png"));
    assert_eq!(
        (s, header(&h, "content-type"), header(&h, "cache-control")),
        (200, Some("image/png"), Some("max-age=86400"))
    );
    assert_eq!(b, image_png());
    assert_eq!(net.urls().len(), 1);
    let (s, _, b2) = bytes(get(&core, "/api/tiles/satellite/3/4/5.png"));
    assert_eq!((s, b2), (200, image_png()));
    assert_eq!(net.urls().len(), 1);
    assert_eq!(
        data(&core).caches["satellite"].get_tile(3, 4, 5).unwrap(),
        Some(image_png())
    );
    for (path, status, error) in [
        ("/api/tiles/nothing/1/0/0.png", 404, "not found"),
        ("/api/tiles/satellite/23/0/0.png", 404, "tile out of range"),
        ("/api/tiles/satellite/3/8/0.png", 404, "tile out of range"),
        (
            "/api/tiles/satellite/3/0/99999999999999999999999.png",
            404,
            "tile out of range",
        ),
    ] {
        let (s, _, b) = bytes(get(&core, path));
        assert_eq!(
            (s, serde_json::from_slice::<Value>(&b).unwrap()),
            (status, json!({"error": error})),
            "{path}"
        );
    }
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn offline_a_miss_is_a_404_and_an_elevation_tile_is_derived_and_never_stored() {
    let dir = scratch("derive");
    let core = start(&dir, Net::offline(), |_| {});
    let (s, _, b) = bytes(get(&core, "/api/tiles/satellite/3/4/5.png"));
    assert_eq!(
        (s, serde_json::from_slice::<Value>(&b).unwrap()),
        (404, json!({"error": "tile not available"}))
    );
    assert_eq!(bytes(get(&core, "/api/tiles/terrain/5/9/9.png")).0, 404);
    let cache = data(&core).caches["terrain"].clone();
    cache.put_tile(3, 1, 1, &terrain_png(1000)).unwrap();
    let (s, h, b) = bytes(get(&core, "/api/tiles/terrain/5/5/5.png"));
    assert_eq!((s, header(&h, "cache-control")), (200, Some("max-age=300")));
    assert_eq!(dem_tiles::decode(&b).unwrap().width, 256);
    assert_eq!(cache.get_tile(5, 5, 5).unwrap(), None);
    let want = dem_tiles::DemFallback::default()
        .tile("terrain", 5, 5, 5, &|z, x, y| cache.get_tile(z, x, y).ok().flatten())
        .unwrap();
    assert_eq!(
        dem_tiles::decode(&b).unwrap().values,
        dem_tiles::decode(&want).unwrap().values
    );
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_keyed_elevation_tile_is_stood_in_for_by_the_free_one() {
    let dir = scratch("keyed");
    let core = start(&dir, Net::offline(), |_| {});
    let keyed = corvus_core::mapdata::tile_sources::list_terrain()
        .into_iter()
        .find(|t| t["id"] != "terrain" && corvus_core::mapdata::tile_sources::needs_token(t["id"].as_str().unwrap()))
        .expect("a keyed elevation source");
    let id = keyed["id"].as_str().unwrap().to_string();
    let provider = keyed["provider"].as_str().unwrap().to_string();
    call(
        &core,
        "POST",
        "/api/tiles/token",
        json!({"provider": provider, "token": "k123"}),
    );
    data(&core).caches["terrain"]
        .put_tile(2, 1, 1, &terrain_png(500))
        .unwrap();
    let (s, h, b) = bytes(get(&core, &format!("/api/tiles/{id}/2/1/1.png")));
    assert_eq!(s, 200, "{id}");
    assert_eq!(header(&h, "cache-control"), Some("max-age=300"));
    assert_eq!(dem_tiles::decode(&b).unwrap().width, 512);
    assert_eq!(data(&core).caches[&id].get_tile(2, 1, 1).unwrap(), None);
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
}

fn poll_job(core: &Core, id: &str) -> Value {
    let mut last = Value::Null;
    wait_for("the job to end", || {
        let (_, jobs) = call(core, "GET", "/api/tiles/jobs", json!({}));
        last = jobs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["job_id"] == id)
            .cloned()
            .unwrap_or(Value::Null);
        ["done", "failed", "cancelled"].contains(&last["state"].as_str().unwrap_or(""))
    });
    last
}

#[test]
fn a_download_runs_records_its_region_and_the_region_is_renamed_and_removed() {
    let dir = scratch("download");
    let net = Net::new(|url| {
        if url.contains("terrain") || url.contains("elevation") {
            ok(terrain_png(0))
        } else {
            ok(image_png())
        }
    });
    let core = start(&dir, net, |_| {});
    let area = json!({"w": 11.5, "s": 48.1, "e": 11.502, "n": 48.102});
    for (body, status, error) in [
        (json!({"source": 5}), 400, "unknown source"),
        (
            json!({"source": "satellite", "bounds": 5}),
            400,
            "bounds must be an object {w,s,e,n}",
        ),
        (
            json!({"source": "satellite", "bounds": {"w": 1, "s": 2, "e": 3}}),
            400,
            "bounds.n must be a finite number",
        ),
        (
            json!({"source": "satellite", "bounds": {"w": 5, "s": 2, "e": 3, "n": 4}}),
            400,
            "bounds must satisfy w<=e and s<=n",
        ),
        (
            json!({"source": "satellite", "bounds": {"w": 1, "s": 2, "e": 3, "n": 400}}),
            400,
            "bounds s/n must be in [-90, 90]",
        ),
        (
            json!({"source": "satellite", "bounds": area, "minzoom": "1", "maxzoom": 3}),
            400,
            "minzoom must be a number",
        ),
        (
            json!({"source": "satellite", "bounds": area, "minzoom": 1.5, "maxzoom": 3}),
            400,
            "zooms must be integers",
        ),
        (
            json!({"source": "satellite", "bounds": area, "minzoom": 5, "maxzoom": 3}),
            400,
            "minzoom must be <= maxzoom",
        ),
        (
            json!({"source": "satellite", "bounds": area, "minzoom": 1, "maxzoom": 40}),
            400,
            "zooms must be in [0, 22]",
        ),
    ] {
        let (s, b) = call(&core, "POST", "/api/tiles/download", body.clone());
        assert_eq!(
            (s, b["error"].as_str().unwrap(), &b["ok"]),
            (status, error, &json!(false)),
            "{body}"
        );
    }
    let (s, b) = call(
        &core,
        "POST",
        "/api/tiles/download",
        json!({"source": "satellite", "bounds": area, "minzoom": 10, "maxzoom": 11, "name": "  My   field \n", "terrain": true, "buildings": true}),
    );
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["name"], "My field");
    assert!(b["terrain_job_id"].is_string());
    assert!(b["terrain_source_job_id"].is_null());
    assert!(b["building_blocks"].as_u64().unwrap() > 0);
    let job = b["job_id"].as_str().unwrap().to_string();
    let done = poll_job(&core, &job);
    assert_eq!(done["state"], "done");
    let terrain_done = poll_job(&core, b["terrain_job_id"].as_str().unwrap());
    assert_eq!(terrain_done["state"], "done");
    wait_for("the region record", || {
        let (_, r) = call(&core, "GET", "/api/tiles/regions", json!({}));
        r["regions"].as_array().unwrap().iter().all(|x| x["state"] == "done")
    });
    let (_, r) = call(&core, "GET", "/api/tiles/regions", json!({}));
    let regions = r["regions"].as_array().unwrap();
    assert_eq!(regions.len(), 2);
    let region = regions.iter().find(|x| x["source"] == "satellite").unwrap();
    assert_eq!(region["name"], "My field");
    assert_eq!(region["tile_count"], done["done"]);
    assert_eq!(region["id"], job.as_str());
    assert_eq!(region["bounds"]["w"], 11.5);
    let cache = data(&core).caches["satellite"].clone();
    assert!(cache.has_tile(10, 544, 355) || cache.stats()["count"].as_i64().unwrap() > 0);

    let (s, b) = call(
        &core,
        "POST",
        "/api/tiles/regions/rename",
        json!({"source": "satellite", "id": job, "name": " Landing   site "}),
    );
    assert_eq!((s, b), (200, json!({"ok": true, "name": "Landing site"})));
    for (body, status, error) in [
        (
            json!({"source": "satellite", "id": "", "name": "x"}),
            400,
            "id required",
        ),
        (
            json!({"source": "satellite", "id": "a", "name": "  "}),
            400,
            "name required",
        ),
        (json!({"source": "nope", "id": "a", "name": "x"}), 400, "unknown source"),
        (
            json!({"source": "satellite", "id": "a", "name": "x"}),
            404,
            "unknown region",
        ),
    ] {
        let (s, b) = call(&core, "POST", "/api/tiles/regions/rename", body.clone());
        assert_eq!((s, b["error"].as_str().unwrap()), (status, error), "{body}");
    }
    let (s, b) = call(
        &core,
        "POST",
        "/api/tiles/regions/remove",
        json!({"source": "satellite", "id": job, "delete_tiles": "yes"}),
    );
    assert_eq!((s, b), (200, json!({"ok": true, "removed_tiles": 0})));
    assert!(cache.stats()["count"].as_i64().unwrap() > 0);
    let (s, _) = call(
        &core,
        "POST",
        "/api/tiles/regions/remove",
        json!({"source": "satellite", "id": job}),
    );
    assert_eq!(s, 404);

    let (s, b) = call(
        &core,
        "POST",
        "/api/tiles/download",
        json!({"source": "satellite", "bounds": area, "minzoom": 10, "maxzoom": 10}),
    );
    assert_eq!(s, 200);
    let again = b["job_id"].as_str().unwrap().to_string();
    poll_job(&core, &again);
    let before = cache.stats()["count"].as_u64().unwrap();
    let (s, b) = call(
        &core,
        "POST",
        "/api/tiles/regions/remove",
        json!({"source": "satellite", "id": again, "delete_tiles": true}),
    );
    assert_eq!(s, 200);
    let removed = b["removed_tiles"].as_u64().unwrap();
    assert!(removed > 0);
    assert_eq!(cache.stats()["count"].as_u64().unwrap(), before - removed);

    assert_eq!(
        call(&core, "POST", "/api/tiles/cancel", json!({"id": "nobody"})),
        (404, json!({"ok": false, "error": "unknown job"}))
    );
    assert_eq!(
        call(&core, "POST", "/api/tiles/cancel", json!({})).1["error"],
        "id required"
    );
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_keyed_service_without_a_key_is_refused_before_a_single_tile_is_asked_for() {
    let dir = scratch("nokey");
    let net = Net::offline();
    let core = start(&dir, net.clone(), |_| {});
    let keyed = corvus_core::mapdata::tile_sources::all_ids()
        .into_iter()
        .find(|id| corvus_core::mapdata::tile_sources::needs_token(id))
        .unwrap();
    let (s, b) = call(
        &core,
        "POST",
        "/api/tiles/download",
        json!({"source": keyed, "bounds": {"w": 1, "s": 1, "e": 1.1, "n": 1.1}, "minzoom": 1, "maxzoom": 2}),
    );
    assert_eq!(
        (s, b),
        (
            400,
            json!({"ok": false, "error": "this map service needs an API key. Add one under Settings, Map service"})
        )
    );
    assert!(net.urls().is_empty());
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_job_is_followed_over_the_progress_stream_until_it_ends() {
    let dir = scratch("progress");
    let core = start(&dir, Net::new(|_| ok(image_png())), |_| {});
    assert_eq!(
        call(&core, "GET", "/api/tiles/progress", json!({})).1["error"],
        "missing id"
    );
    assert_eq!(get(&core, "/api/tiles/progress?id=nobody").status, 404);
    let (_, b) = call(
        &core,
        "POST",
        "/api/tiles/download",
        json!({"source": "satellite", "bounds": {"w": 11.5, "s": 48.1, "e": 11.501, "n": 48.101}, "minzoom": 9, "maxzoom": 9}),
    );
    let job = b["job_id"].as_str().unwrap().to_string();
    poll_job(&core, &job);
    let r = get(&core, &format!("/api/tiles/progress?id={job}"));
    assert_eq!(r.status, 200);
    let Body::Sse(mut stream) = r.body else {
        panic!("not a stream")
    };
    let first = stream.open();
    assert_eq!(first[0].name, "progress");
    let state: Value = serde_json::from_slice(&first[0].data).unwrap();
    assert_eq!(state["state"], "done");
    stream.close();
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn elevation_models_are_imported_listed_renamed_served_and_removed() {
    let dir = scratch("dem");
    let core = start(&dir, Net::offline(), |_| {});
    assert_eq!(
        call(&core, "GET", "/api/dem", json!({})),
        (200, json!({"models": [], "available": true}))
    );
    let asc: String = {
        let mut s =
            String::from("ncols 300\nnrows 240\nxllcorner 690000\nyllcorner 5330000\ncellsize 1\nNODATA_value -9999\n");
        for r in 0..240 {
            s.push_str(
                &(0..300)
                    .map(|c| format!("{}", 500 + (r % 7) + (c % 5)))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            s.push('\n');
        }
        s
    };
    let upload = |query: &str, body: &[u8]| {
        let mut req = Request::new(Method::Post, &format!("/api/dem/import{query}"));
        req.body = body.to_vec();
        let (s, _, b) = bytes(core.dispatch(req));
        (s, serde_json::from_slice::<Value>(&b).unwrap())
    };
    assert_eq!(
        upload("", b""),
        (400, json!({"ok": false, "error": "the file is empty"}))
    );
    assert_eq!(
        upload("?crs=wgs", b"x").1["error"],
        "the coordinate system must be an EPSG number, such as 25832"
    );
    let (s, b) = upload(
        "?filename=C%3A%5Cdata%5Cground.asc&crs=EPSG:25832&name=%20Field%20",
        asc.as_bytes(),
    );
    assert_eq!(s, 200, "{b}");
    let model = b["model"].clone();
    let id = model["id"].as_str().unwrap().to_string();
    assert_eq!(model["name"], "Field");
    assert_eq!(model["files"][0]["file_name"], "ground.asc");
    assert!(std::fs::read_dir(data(&core).custom_dem.as_ref().unwrap().root())
        .unwrap()
        .all(|e| !e.unwrap().file_name().to_string_lossy().starts_with("upload-")));
    let (s, b) = upload("?crs=auto&filename=x.asc", asc.as_bytes());
    assert_eq!(
        (
            s,
            b["error"]
                .as_str()
                .unwrap()
                .starts_with("the file does not say which coordinate system")
        ),
        (400, true)
    );
    let (s, b) = upload(&format!("?crs=25832&model={id}"), asc.as_bytes());
    assert_eq!((s, b["model"]["files"].as_array().unwrap().len()), (200, 2));

    let (_, list) = call(&core, "GET", "/api/dem", json!({}));
    assert_eq!(list["models"][0]["id"], id.as_str());
    let (_, sources) = call(&core, "GET", "/api/tiles/sources", json!({}));
    let entry = sources["terrain"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id.as_str())
        .unwrap();
    assert_eq!(
        (
            entry["custom"].clone(),
            entry["token_set"].clone(),
            entry["cached_count"].clone()
        ),
        (json!(true), json!(false), json!(0))
    );

    assert_eq!(
        call(&core, "POST", "/api/dem/rename", json!({"id": id, "name": " <New> "})),
        (200, json!({"ok": true, "name": "New"}))
    );
    assert_eq!(
        call(&core, "POST", "/api/dem/rename", json!({"id": id})).1["error"],
        "id and name required"
    );
    assert_eq!(
        call(
            &core,
            "POST",
            "/api/dem/rename",
            json!({"id": "dem_0000000000", "name": "x"})
        )
        .0,
        404
    );

    let (px, py) = corvus_core::mapdata::custom_dem::lonlat_pixel(17, 11.5538, 48.0959);
    let tile = format!("/api/tiles/{id}/17/{}/{}.png", (px / 256.0) as i64, (py / 256.0) as i64);
    let (s, h, b) = bytes(get(&core, &tile));
    assert_eq!((s, header(&h, "cache-control")), (200, Some("max-age=300")));
    assert_eq!(dem_tiles::decode(&b).unwrap().width, 256);
    let (s, h, _) = bytes(get(&core, &format!("/api/tiles/{id}/17/0/0.png")));
    assert_eq!((s, header(&h, "cache-control")), (200, Some("max-age=300")));
    assert_eq!(bytes(get(&core, &format!("/api/tiles/{id}/3/9/0.png"))).0, 404);

    assert_eq!(
        call(&core, "POST", "/api/dem/remove", json!({"id": id})),
        (200, json!({"ok": true}))
    );
    assert_eq!(call(&core, "POST", "/api/dem/remove", json!({"id": id})).0, 404);
    assert_eq!(bytes(get(&core, &tile)).0, 404);
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_model_is_surrounded_by_the_default_elevation_when_it_is_cached() {
    let dir = scratch("demfill");
    let net = Net::new(|_| ok(terrain_png(2000)));
    let core = start(&dir, net, |_| {});
    let model = data(&core)
        .custom_dem
        .as_ref()
        .unwrap()
        .import_file(&corvus_core_test_asc(&dir), "m.asc", "M", Some(25832), None)
        .unwrap();
    let id = model["id"].as_str().unwrap();
    let (s, h, b) = bytes(get(&core, &format!("/api/tiles/{id}/12/2178/1422.png")));
    assert_eq!((s, header(&h, "cache-control")), (200, Some("max-age=86400")));
    let px = dem_tiles::decode(&b).unwrap().values;
    assert!(px.iter().all(|v| *v > 0));
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
}

fn corvus_core_test_asc(dir: &std::path::Path) -> PathBuf {
    let mut s = String::from("ncols 70\nnrows 70\nxllcorner 690000\nyllcorner 5330000\ncellsize 1\n");
    for r in 0..70 {
        s.push_str(
            &(0..70)
                .map(|c| format!("{}", 500 + r % 3 + c % 4))
                .collect::<Vec<_>>()
                .join(" "),
        );
        s.push('\n');
    }
    let path = dir.join("m.asc");
    std::fs::write(&path, s).unwrap();
    path
}

#[test]
fn building_cells_answer_pending_then_the_gzipped_footprints() {
    let dir = scratch("buildings");
    let overpass = Stub::start(|_| {
        Reply::json(
            &json!({"elements": [{"type": "way", "id": 1, "tags": {"building": "yes", "height": "12"},
            "geometry": [{"lon": 11.5, "lat": 48.1}, {"lon": 11.5003, "lat": 48.1}, {"lon": 11.5003, "lat": 48.1003}, {"lon": 11.5, "lat": 48.1003}]}]}),
        )
    });
    let core = start(&dir, Net::offline(), |o| {
        o.overpass_url = Some(overpass.url("/api/interpreter"))
    });
    let (px, py) = (17430, 11376);
    let target = format!("/api/buildings/15/{px}/{py}.json");
    let (s, h, b) = bytes(get(&core, &target));
    assert_eq!(s, 200);
    assert_eq!(header(&h, "content-type"), Some("application/geo+json"));
    assert_eq!(header(&h, "cache-control"), Some("no-store"));
    assert_eq!(
        serde_json::from_slice::<Value>(&b).unwrap(),
        json!({"type": "FeatureCollection", "features": [], "pending": true})
    );
    wait_for("the cell", || {
        header(&bytes(get(&core, &target)).1, "content-encoding") == Some("gzip")
    });
    let (_, h, b) = bytes(get(&core, &target));
    assert_eq!(header(&h, "cache-control"), Some("max-age=86400"));
    let mut text = String::new();
    std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&b[..]), &mut text).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        v["features"][0]["properties"],
        json!({"height": 12.0, "min_height": 0.0})
    );
    for off in ["/api/buildings/14/1/1.json", "/api/buildings/15/99999/0.json"] {
        let (s, h, b) = bytes(get(&core, off));
        assert_eq!(
            (
                s,
                header(&h, "cache-control"),
                serde_json::from_slice::<Value>(&b).unwrap()
            ),
            (
                200,
                Some("max-age=86400"),
                json!({"type": "FeatureCollection", "features": []})
            ),
            "{off}"
        );
    }
    let (_, sources) = call(&core, "GET", "/api/tiles/sources", json!({}));
    assert!(sources["buildings"]["cells"].as_i64().unwrap() >= 1);
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_place_search_answers_always_200_with_ok_results_and_error() {
    let dir = scratch("geocode");
    let nominatim = Stub::start(|_| {
        Reply::json(
            &json!([{"lat": "48.1", "lon": "11.5", "display_name": "Munich", "type": "city", "boundingbox": ["48.0", "48.2", "11.4", "11.6"]}]),
        )
    });
    let core = start(&dir, Net::offline(), |o| o.geocode_url = Some(nominatim.url("/search")));
    let (s, b) = call(&core, "GET", "/api/geocode", json!({}));
    assert_eq!(
        (s, b),
        (400, json!({"ok": false, "results": [], "error": "q is required"}))
    );
    let (s, b) = call(
        &core,
        "GET",
        "/api/geocode?q=%20munich%20&near=11.5,48.1&limit=3",
        json!({}),
    );
    assert_eq!(s, 200);
    assert_eq!(
        (b["ok"].clone(), b["error"].clone(), b["results"][0]["label"].clone()),
        (json!(true), json!(""), json!("Munich"))
    );
    let seen = nominatim.requests();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].target.contains("q=munich") && seen[0].target.contains("limit=3"),
        "{}",
        seen[0].target
    );
    core.stop();
    let dir2 = scratch("geocode-off");
    let core = start(&dir2, Net::offline(), |o| {
        o.geocode_url = Some("http://127.0.0.1:9/search".into())
    });
    let (s, b) = call(&core, "GET", "/api/geocode?q=x", json!({}));
    assert_eq!(
        (s, b),
        (
            200,
            json!({"ok": false, "results": [], "error": "No connection to the place-search service."})
        )
    );
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir2);
}

#[test]
fn a_region_left_running_is_settled_as_cancelled_at_the_next_start() {
    let dir = scratch("settle");
    let core = start(&dir, Net::offline(), |_| {});
    let cache = data(&core).caches["satellite"].clone();
    cache.add_region(&json!({"id": "old", "name": "Old", "source": "satellite", "w": 11.5, "s": 48.1, "e": 11.51, "n": 48.11, "minzoom": 10, "maxzoom": 10, "tile_count": 0, "state": "running"}));
    core.stop();
    let core = start(&dir, Net::offline(), |_| {});
    let (_, r) = call(&core, "GET", "/api/tiles/regions", json!({}));
    assert_eq!(r["regions"][0]["state"], "cancelled");
    core.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn stop_closes_the_caches_and_the_workers() {
    let dir = scratch("stop");
    let core = start(&dir, Net::offline(), |_| {});
    let res = data(&core);
    let started = Instant::now();
    core.stop();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(res.caches["satellite"].get_tile(1, 0, 0).ok().flatten(), None);
    assert_eq!(res.buildings.as_ref().unwrap().lookup(15, 1, 1), (None, false));
    assert!(res
        .custom_dem
        .as_ref()
        .unwrap()
        .import_file(&corvus_core_test_asc(&dir), "m.asc", "", Some(25832), None)
        .is_err());
    let _ = std::fs::remove_dir_all(dir);
    let _ = SseNext::End;
}
