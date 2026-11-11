//! The routes of `routes/tiles.py`, `routes/dem.py` and the tile, buildings
//! and place search parts of `server.py`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::json::Map;

use super::buildings::CELL_ZOOM;
use super::dem_import::DemImportError;
use super::dem_tiles;
use super::input::{
    clean_region_name, default_region_name, int_or, parse_near, validate_tile_bounds, validate_tile_zooms,
};
use super::module::{MapData, Resources};
use super::tile_cache::TileCache;
use super::tile_downloader::{delete_region_tiles, tile_count, MAX_TILES_PER_JOB};
use super::tile_http::Listener;
use super::tile_proxy::content_type;
use super::tile_sources as ts;
use super::util::{py_repr, truthy, uuid4_hex};
use crate::core::Core;
use crate::module::Registry;
use crate::request::Request;
use crate::response::Response;
use crate::router::RouteOptions;

/// How long removing a region waits for its cancelled download to end.
const REMOVE_JOB_WAIT_S: f64 = 5.0;
/// One uploaded file of an elevation model, at most.
pub const MAX_DEM_BODY_BYTES: usize = 1024 * 1024 * 1024;
const MAX_MAP_TOKEN_CHARS: usize = 512;

const EMPTY_BUILDINGS: &[u8] = br#"{"type":"FeatureCollection","features":[]}"#;
const PENDING_BUILDINGS: &[u8] = br#"{"type":"FeatureCollection","features":[],"pending":true}"#;
const UNAVAILABLE_BUILDINGS: &[u8] = br#"{"type":"FeatureCollection","features":[],"unavailable":true}"#;

pub fn register(reg: &mut Registry) {
    reg.get("/api/tiles/sources", tiles_sources);
    reg.post("/api/tiles/token", tiles_token);
    reg.get("/api/tiles/jobs", tiles_jobs);
    reg.post("/api/tiles/download", tiles_download);
    reg.get("/api/tiles/regions", tiles_regions);
    reg.post("/api/tiles/regions/rename", regions_rename);
    reg.post("/api/tiles/regions/remove", regions_remove);
    reg.post("/api/tiles/cancel", tiles_cancel);
    reg.get("/api/tiles/progress", tiles_progress);
    reg.get("/api/geocode", geocode);
    reg.get("/api/dem", dem_list);
    reg.post("/api/dem/rename", dem_rename);
    reg.post("/api/dem/remove", dem_remove);
    reg.post_with("/api/dem/import", RouteOptions::raw(MAX_DEM_BODY_BYTES), dem_import);
    reg.get_pattern("/api/tiles/{source}/{z:d}/{x:d}/{y:d}.png", tile_serve);
    reg.get_pattern("/api/buildings/{z:d}/{x:d}/{y:d}.json", buildings_serve);
}

fn res(core: &Core) -> Option<Arc<Resources>> {
    core.service::<MapData>().and_then(|m| m.resources())
}

/// The stored API key for a provider, or "" (the only reader of the keys).
fn map_token(core: &Core, provider: &str) -> String {
    if provider.is_empty() {
        return String::new();
    }
    core.settings()
        .get()
        .block("map_tokens")
        .and_then(|t| t.get(provider))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn map_tokens_set(core: &Core) -> Vec<String> {
    core.settings()
        .get()
        .block("map_tokens")
        .map(|t| {
            t.iter()
                .filter(|(_, v)| v.as_str().is_some_and(|s| !s.is_empty()))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(b), Value::Object(e)) = (base.as_object_mut(), extra) {
        for (k, v) in e {
            b.insert(k, v);
        }
    }
    base
}

fn cache_stats(r: &Resources, id: &str) -> Value {
    match r.caches.get(id) {
        Some(c) => c.stats(),
        None => json!({"count": 0, "minzoom": null, "maxzoom": null}),
    }
}

fn tiles_sources(core: &Core, _req: &Request) -> Response {
    let r = res(core);
    let stats_of = |id: &str| match &r {
        Some(r) => cache_stats(r, id),
        None => json!({"count": 0, "minzoom": null, "maxzoom": null}),
    };
    let mut sources = Vec::new();
    for entry in ts::list_sources() {
        let stats = stats_of(entry["id"].as_str().unwrap_or(""));
        sources.push(json!({
            "id": entry["id"], "label": entry["label"], "provider": entry["provider"], "style": entry["style"],
            "attribution": entry["attribution"], "minzoom": 0, "maxzoom": entry["maxzoom"],
            "cached_count": stats["count"], "cached_minzoom": stats["minzoom"], "cached_maxzoom": stats["maxzoom"],
        }));
    }
    let keys_set = map_tokens_set(core);
    let mut terrain = Vec::new();
    for entry in ts::list_terrain() {
        let id = entry["id"].as_str().unwrap_or("").to_string();
        let stats = stats_of(&id);
        let extra = json!({
            "minzoom": 0, "cached_count": stats["count"], "cached_minzoom": stats["minzoom"],
            "cached_maxzoom": stats["maxzoom"], "token_required": ts::needs_token(&id),
            "token_set": keys_set.iter().any(|k| Some(k.as_str()) == entry["provider"].as_str()),
        });
        terrain.push(merge(entry, extra));
    }
    if let Some(custom) = r.as_ref().and_then(|r| r.custom_dem.as_ref()) {
        for entry in custom.terrain_entries() {
            let extra = json!({
                "minzoom": 0, "cached_count": 0, "cached_minzoom": null, "cached_maxzoom": null,
                "token_required": false, "token_set": false,
            });
            terrain.push(merge(entry, extra));
        }
    }
    let buildings = match r.as_ref().and_then(|r| r.buildings.as_ref()) {
        Some(b) => b.stats(),
        None => json!({"cells": 0, "bytes": 0}),
    };
    let mut providers = Vec::new();
    for prov in ts::list_providers() {
        let id = prov["id"].as_str().unwrap_or("").to_string();
        let keyed = !prov["token"].is_null();
        let extra = json!({"token_required": ts::token_required(&id), "token_set": keyed && keys_set.contains(&id)});
        providers.push(merge(prov, extra));
    }
    Response::json(json!({
        "sources": sources,
        "providers": providers,
        "default_provider": ts::DEFAULT_PROVIDER,
        "terrain": terrain,
        "default_terrain": ts::DEFAULT_TERRAIN,
        "preferred_terrain": ts::PREFERRED_TERRAIN,
        "buildings": buildings,
        "max_tiles_per_job": MAX_TILES_PER_JOB,
    }))
}

fn tiles_token(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let provider = match payload.get("provider").and_then(Value::as_str) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => return Response::not_ok(400, "provider must be a non-empty string"),
    };
    if ts::token_meta(&provider).is_none() {
        return Response::not_ok(400, &format!("{} is not a keyed map service", py_repr(&provider)));
    }
    let raw = match payload.get("token") {
        None => "",
        Some(Value::String(s)) => s.as_str(),
        Some(_) => return Response::not_ok(400, "token must be a string"),
    };
    let token = raw.trim().to_string();
    let mut tokens: Map = core.settings().get().block("map_tokens").cloned().unwrap_or_default();
    if !token.is_empty() {
        if token.chars().count() > MAX_MAP_TOKEN_CHARS {
            return Response::not_ok(
                400,
                &format!("that key is longer than {MAX_MAP_TOKEN_CHARS} characters"),
            );
        }
        if !token.chars().all(|c| c.is_ascii_alphanumeric() || "._~-".contains(c)) {
            return Response::not_ok(
                400,
                "that does not look like an API key. Use letters, digits, . _ ~ - only, with no spaces or quotes",
            );
        }
        tokens.insert(provider.clone(), Value::from(token.clone()));
    } else {
        tokens.shift_remove(&provider);
    }
    let value = if tokens.is_empty() {
        None
    } else {
        Some(Value::Object(tokens))
    };
    if core
        .settings()
        .update(core, |cfg| cfg.set("map_tokens", value))
        .is_err()
    {
        return Response::not_ok(500, "the key is set for this session but could not be saved");
    }
    Response::json(json!({"ok": true, "provider": provider, "token_set": !token.is_empty()}))
}

fn tiles_jobs(core: &Core, _req: &Request) -> Response {
    match res(core) {
        Some(r) => Response::json(json!({"jobs": r.downloader.list_jobs()})),
        None => Response::json(json!({"jobs": []})),
    }
}

type Bounds = (f64, f64, f64, f64);

/// Writes the region row for a download job that is already running. The job
/// has to exist first (its id is the row's key), so a fast job can end before
/// the row does: the job is read back once the row is in and settled here.
#[allow(clippy::too_many_arguments)]
fn record_region(
    r: &Resources,
    cache: Option<&Arc<TileCache>>,
    job_id: &str,
    name: &str,
    source: &str,
    bounds: Bounds,
    minzoom: i64,
    maxzoom: i64,
) {
    let Some(cache) = cache else { return };
    let (w, s, e, n) = bounds;
    cache.add_region(&json!({
        "id": job_id, "name": name, "source": source, "w": w, "s": s, "e": e, "n": n,
        "minzoom": minzoom, "maxzoom": maxzoom, "tile_count": 0, "state": "running",
    }));
    if let Some(status) = r.downloader.status(job_id) {
        let state = status.get("state").and_then(Value::as_str).unwrap_or("");
        if ["done", "cancelled", "failed"].contains(&state) {
            cache.update_region(
                job_id,
                &[
                    ("tile_count", status.get("done").cloned().unwrap_or(json!(0))),
                    ("state", Value::from(state)),
                ],
            );
        }
    }
}

/// The progress callback handed to the downloader: the live subscribers first,
/// then the region record kept in step with the job, once at the terminal
/// update.
fn make_tile_progress(r: &Arc<Resources>, cache: Option<Arc<TileCache>>) -> Listener {
    let bus_fn = r.bus.make_on_progress();
    let Some(cache) = cache else { return bus_fn };
    let finished = std::sync::atomic::AtomicBool::new(false);
    let r = Arc::clone(r);
    Arc::new(move |progress: &Value| {
        bus_fn(progress);
        let Some(state) = progress.get("state").and_then(Value::as_str) else {
            return;
        };
        if !["done", "cancelled", "failed"].contains(&state) || finished.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        if let Some(source) = progress.get("source").and_then(Value::as_str) {
            if ts::is_terrain(source) {
                r.fallback.forget(Some(source));
            }
        }
        cache.update_region(
            progress.get("job_id").and_then(Value::as_str).unwrap_or(""),
            &[
                ("tile_count", progress.get("done").cloned().unwrap_or(json!(0))),
                ("state", Value::from(state)),
            ],
        );
    })
}

fn tiles_download(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let Some(r) = res(core) else {
        return Response::not_ok(503, "tile service unavailable");
    };
    let source = match payload.get("source").and_then(Value::as_str) {
        Some(s) if ts::get(s).is_some() => s.to_string(),
        _ => return Response::not_ok(400, "unknown source"),
    };
    let src = ts::get(&source).expect("checked");
    let bounds = match validate_tile_bounds(payload.get("bounds")) {
        Ok(b) => b,
        Err(e) => return Response::not_ok(400, &e),
    };
    let (minzoom, maxzoom) = match validate_tile_zooms(payload.get("minzoom"), payload.get("maxzoom"), src.maxzoom()) {
        Ok(z) => z,
        Err(e) => return Response::not_ok(400, &e),
    };
    let mut name = clean_region_name(payload.get("name"));
    if name.is_empty() {
        name = default_region_name(bounds);
    }
    let token = map_token(core, src.provider());
    if ts::needs_token(&source) && token.is_empty() {
        return Response::not_ok(
            400,
            "this map service needs an API key. Add one under Settings, Map service",
        );
    }
    let mut template = src.upstream().to_string();
    if !token.is_empty() && ts::licensed(&source).is_some() {
        match r.proxy.sessions.resolve(r.net.as_ref(), &source, &token) {
            Ok(t) => template = t,
            Err(e) => return Response::not_ok(502, &e.to_string()),
        }
    }
    let cache = r.caches.get(&source).cloned();
    let on_progress = make_tile_progress(&r, cache.clone());
    let job_id = match r
        .downloader
        .start(&source, &template, bounds, minzoom, maxzoom, Some(on_progress), &token)
    {
        Ok(id) => id,
        Err(e) => return Response::not_ok(500, &format!("start failed: {e}")),
    };
    let status = r.downloader.status(&job_id);
    if let Some(st) = status.as_ref().filter(|s| s["state"] == "failed") {
        let err = st.get("error").and_then(Value::as_str).unwrap_or("download failed");
        return Response::not_ok(409, err);
    }
    record_region(&r, cache.as_ref(), &job_id, &name, &source, bounds, minzoom, maxzoom);
    let mut terrain_job = Value::Null;
    let mut chosen_job = Value::Null;
    if payload.get("terrain").is_some_and(truthy) && !ts::is_terrain(&source) {
        terrain_job = start_terrain_companion(core, &r, bounds, minzoom, maxzoom, &name, None);
        if let Some(chosen) = payload.get("terrain_source").and_then(Value::as_str) {
            if chosen != ts::DEFAULT_TERRAIN && ts::is_terrain(chosen) {
                chosen_job = start_terrain_companion(core, &r, bounds, minzoom, maxzoom, &name, Some(chosen));
            }
        }
    }
    let mut building_blocks = 0usize;
    if payload.get("buildings").is_some_and(truthy) {
        if let Some(b) = &r.buildings {
            building_blocks = b.prefetch(
                [bounds.0, bounds.1, bounds.2, bounds.3],
                super::buildings::PREFETCH_MAX_BLOCKS as i64,
            );
        }
    }
    Response::json(json!({
        "job_id": job_id, "name": name, "terrain_job_id": terrain_job,
        "terrain_source_job_id": chosen_job, "building_blocks": building_blocks,
    }))
}

/// The elevation half of a region download; null when it cannot run. Best
/// effort on purpose: the imagery job has been accepted already. It caps the
/// zooms at the model's own and, for a model that only has its finest levels in
/// some regions, at the last level it has everywhere, and starts from zoom 0
/// (a pitched view draws far ground from coarse tiles).
fn start_terrain_companion(
    core: &Core,
    r: &Arc<Resources>,
    bounds: Bounds,
    minzoom: i64,
    maxzoom: i64,
    name: &str,
    terrain_id: Option<&str>,
) -> Value {
    let id = terrain_id.unwrap_or(ts::DEFAULT_TERRAIN);
    let Some(src) = ts::get(id) else { return Value::Null };
    if !ts::is_terrain(id) {
        return Value::Null;
    }
    let token = map_token(core, src.provider());
    if ts::needs_token(id) && token.is_empty() {
        return Value::Null;
    }
    let sparse = ts::sparse_above(id).unwrap_or(src.maxzoom());
    let hi = maxzoom.min(src.maxzoom()).min(sparse);
    let floor = minzoom.min(hi);
    let mut lo = 0;
    while lo < floor && tile_count(bounds, lo, hi) > MAX_TILES_PER_JOB {
        lo += 1;
    }
    let cache = r.caches.get(id).cloned();
    let job_id = match r.downloader.start(
        id,
        src.upstream(),
        bounds,
        lo,
        hi,
        Some(make_tile_progress(r, cache.clone())),
        &token,
    ) {
        Ok(j) => j,
        Err(_) => return Value::Null,
    };
    let failed = r.downloader.status(&job_id).is_some_and(|s| s["state"] == "failed");
    if !failed {
        record_region(r, cache.as_ref(), &job_id, name, id, bounds, lo, hi);
    }
    Value::from(job_id)
}

fn tiles_regions(core: &Core, _req: &Request) -> Response {
    let mut regions: Vec<Value> = Vec::new();
    if let Some(r) = res(core) {
        for sid in ts::all_ids() {
            let Some(cache) = r.caches.get(sid) else { continue };
            for region in cache.list_regions() {
                let mut v = region.to_json();
                if region.source.is_empty() {
                    v["source"] = Value::from(sid);
                }
                regions.push(v);
            }
        }
    }
    regions.sort_by(|a, b| {
        b["created_at"]
            .as_str()
            .unwrap_or("")
            .cmp(a["created_at"].as_str().unwrap_or(""))
    });
    Response::json(json!({"regions": regions}))
}

fn region_cache(r: &Option<Arc<Resources>>, payload: &Map) -> Option<Arc<TileCache>> {
    let source = payload.get("source").and_then(Value::as_str)?;
    r.as_ref()?.caches.get(source).cloned()
}

fn regions_rename(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let id = payload.get("id").and_then(Value::as_str).unwrap_or("");
    let name = clean_region_name(payload.get("name"));
    if id.is_empty() {
        return Response::not_ok(400, "id required");
    }
    if name.is_empty() {
        return Response::not_ok(400, "name required");
    }
    let Some(cache) = region_cache(&res(core), payload) else {
        return Response::not_ok(400, "unknown source");
    };
    if !cache.update_region(id, &[("name", Value::from(name.clone()))]) {
        return Response::not_ok(404, "unknown region");
    }
    Response::json(json!({"ok": true, "name": name}))
}

fn regions_remove(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let id = payload.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    if id.is_empty() {
        return Response::not_ok(400, "id required");
    }
    let r = res(core);
    let Some(cache) = region_cache(&r, payload) else {
        return Response::not_ok(400, "unknown source");
    };
    let Some(region) = cache.get_region(&id) else {
        return Response::not_ok(404, "unknown region");
    };
    let source = payload.get("source").and_then(Value::as_str).unwrap_or("").to_string();
    if let Some(r) = &r {
        if r.downloader.cancel(&id) {
            r.downloader.wait(&id, REMOVE_JOB_WAIT_S);
        }
    }
    let mut removed = 0usize;
    if payload.get("delete_tiles") == Some(&Value::Bool(true)) {
        removed = delete_region_tiles(&cache, &region);
        if removed > 0 && ts::is_terrain(&source) {
            if let Some(r) = &r {
                r.fallback.forget(Some(&source));
            }
        }
    }
    cache.remove_region(&id);
    Response::json(json!({"ok": true, "removed_tiles": removed}))
}

fn tiles_cancel(core: &Core, req: &Request) -> Response {
    let Some(r) = res(core) else {
        return Response::not_ok(503, "tile service unavailable");
    };
    let id = req.payload().get("id").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() {
        return Response::not_ok(400, "id required");
    }
    if r.downloader.cancel(id) {
        Response::json(json!({"ok": true}))
    } else {
        Response::not_ok(404, "unknown job")
    }
}

fn tiles_progress(core: &Core, req: &Request) -> Response {
    let job = req.query.get("id").unwrap_or("");
    if job.is_empty() {
        return Response::error(400, "missing id");
    }
    let Some(r) = res(core) else {
        return Response::error(503, "tile service unavailable");
    };
    if r.downloader.status(job).is_none() {
        return Response::error(404, "unknown job");
    }
    Response::sse(core.topic_stream(&[("tiles", "progress")], Some(job), &["tiles"]))
}

fn geocode(core: &Core, req: &Request) -> Response {
    let query = req.query.get("q").unwrap_or("").trim().to_string();
    if query.is_empty() {
        return Response::json_status(json!({"ok": false, "results": [], "error": "q is required"}), 400);
    }
    let Some(r) = res(core).filter(|r| r.geocoder.is_some()) else {
        return Response::json(
            json!({"ok": false, "results": [], "error": "Place search is not available in this build."}),
        );
    };
    let limit = int_or(req.query.get("limit"), super::geocode::DEFAULT_RESULTS);
    let near = parse_near(req.query.get("near").unwrap_or(""));
    let wait = |d: Duration| core.wait_stopping(d);
    let g = r.geocoder.as_ref().expect("checked");
    match g.search(r.net.as_ref(), &query, Some(limit), near, &wait) {
        Ok(results) => Response::json(json!({"ok": true, "results": results, "error": ""})),
        Err(e) => Response::json(json!({"ok": false, "results": [], "error": e.to_string()})),
    }
}

fn dem_list(core: &Core, _req: &Request) -> Response {
    match res(core).and_then(|r| r.custom_dem.as_ref().map(|c| c.list_models())) {
        Some(models) => Response::json(json!({"models": models, "available": true})),
        None => Response::json(json!({"models": [], "available": false})),
    }
}

fn dem_rename(core: &Core, req: &Request) -> Response {
    let payload = req.payload();
    let r = res(core);
    let Some(store) = r.as_ref().and_then(|r| r.custom_dem.as_ref()) else {
        return Response::not_ok(503, "elevation models are unavailable");
    };
    let (Some(id), Some(name)) = (
        payload.get("id").and_then(Value::as_str),
        payload.get("name").and_then(Value::as_str),
    ) else {
        return Response::not_ok(400, "id and name required");
    };
    if name.trim().is_empty() {
        return Response::not_ok(400, "id and name required");
    }
    match store.rename(id, name) {
        Some(stands) => Response::json(json!({"ok": true, "name": stands})),
        None => Response::not_ok(404, "unknown elevation model"),
    }
}

fn dem_remove(core: &Core, req: &Request) -> Response {
    let r = res(core);
    let Some(store) = r.as_ref().and_then(|r| r.custom_dem.as_ref()) else {
        return Response::not_ok(503, "elevation models are unavailable");
    };
    match req.payload().get("id").and_then(Value::as_str) {
        Some(id) if store.remove(id) => Response::json(json!({"ok": true})),
        _ => Response::not_ok(404, "unknown elevation model"),
    }
}

/// Adds one uploaded file to an elevation model. A raw body, written to a
/// temporary file next to the models that is gone whatever the outcome. Query:
/// `filename` (display and format hint only), `name`, `crs` (an EPSG code that
/// overrides the file's own) and `model` (add to this model).
fn dem_import(core: &Core, req: &Request) -> Response {
    let r = res(core);
    let Some(store) = r.as_ref().and_then(|r| r.custom_dem.as_ref()) else {
        return Response::not_ok(503, "elevation models are unavailable");
    };
    if req.body.is_empty() {
        return Response::not_ok(400, "the file is empty");
    }
    let param = |key: &str| req.query.get(key).unwrap_or("").trim().to_string();
    let filename: String = {
        let raw = param("filename").replace('\\', "/");
        raw.rsplit('/').next().unwrap_or("").chars().take(200).collect()
    };
    let mut epsg = None;
    let raw_crs = param("crs").to_uppercase().replace("EPSG:", "");
    let raw_crs = raw_crs.trim();
    if !raw_crs.is_empty() && raw_crs != "AUTO" {
        match super::util::py_int(raw_crs) {
            Some(code) => epsg = Some(code),
            None => return Response::not_ok(400, "the coordinate system must be an EPSG number, such as 25832"),
        }
    }
    let model = param("model");
    let model_id = (!model.is_empty()).then_some(model.as_str());
    let _ = std::fs::create_dir_all(store.root());
    let tmp = store.root().join(format!("upload-{}.part", &uuid4_hex()[..12]));
    if std::fs::write(&tmp, &req.body).is_err() {
        return Response::not_ok(400, "could not read this file as an elevation grid");
    }
    let imported = store.import_file(&tmp, &filename, &param("name"), epsg, model_id);
    let _ = std::fs::remove_file(&tmp);
    match imported {
        Ok(model) => Response::json(json!({"ok": true, "model": model})),
        Err(DemImportError(e)) => Response::not_ok(400, &e),
    }
}

fn parse_coord(req: &Request, name: &str) -> i64 {
    req.param(name).and_then(|v| v.parse::<i64>().ok()).unwrap_or(i64::MAX)
}

fn out_of_range(z: i64, x: i64, y: i64) -> bool {
    !(0..=22).contains(&z) || x < 0 || y < 0 || x >= (1i64 << z) || y >= (1i64 << z)
}

fn png(blob: Vec<u8>, complete: bool) -> Response {
    Response::bytes(200, "image/png", blob)
        .with_header("Cache-Control", if complete { "max-age=86400" } else { "max-age=300" })
}

/// A tile of a source, filled from its upstream on a miss and cached. Offline
/// (or a failed fetch) a miss is a 404, except for an elevation tile with a
/// coarser ancestor on disk, which is derived from it so 3D terrain has no
/// holes.
fn tile_serve(core: &Core, req: &Request) -> Response {
    let source = req.param("source").unwrap_or("").to_string();
    let (z, x, y) = (parse_coord(req, "z"), parse_coord(req, "x"), parse_coord(req, "y"));
    let r = res(core);
    if let Some(store) = r
        .as_ref()
        .and_then(|r| r.custom_dem.as_ref())
        .filter(|s| s.has(&source))
    {
        return serve_custom_dem(core, r.as_ref().expect("has a store"), store, &source, z, x, y);
    }
    if ts::get(&source).is_none() {
        return Response::error(404, "not found");
    }
    if out_of_range(z, x, y) {
        return Response::error(404, "tile out of range");
    }
    let cache = r.as_ref().and_then(|r| r.caches.get(&source)).cloned();
    let mut blob = cache.as_ref().and_then(|c| c.get_tile(z, x, y).ok().flatten());
    let mut derived = false;
    if blob.is_none() {
        blob = r.as_ref().and_then(|r| fetch_upstream(core, r, &source, z, x, y));
        if blob.is_none() {
            blob = r
                .as_ref()
                .and_then(|r| derive_terrain_tile(core, r, &source, z, x, y, cache.as_deref()));
            derived = blob.is_some();
        }
        let Some(b) = &blob else {
            return Response::error(404, "tile not available");
        };
        if let (Some(cache), false) = (&cache, derived) {
            let _ = cache.put_tile(z, x, y, b);
        }
    }
    let blob = blob.expect("set above");
    Response::bytes(200, content_type(&blob), blob)
        .with_header("Cache-Control", if derived { "max-age=300" } else { "max-age=86400" })
}

fn fetch_upstream(core: &Core, r: &Resources, source: &str, z: i64, x: i64, y: i64) -> Option<Vec<u8>> {
    r.proxy
        .fetch_upstream(source, z, x, y, &|provider| map_token(core, provider))
}

/// An elevation tile resampled from the nearest cached ancestor, or None. Only
/// for elevation sources, and never stored.
fn derive_terrain_tile(
    core: &Core,
    r: &Resources,
    source: &str,
    z: i64,
    x: i64,
    y: i64,
    cache: Option<&TileCache>,
) -> Option<Vec<u8>> {
    let cache = cache?;
    if !ts::is_terrain(source) {
        return None;
    }
    if ts::sparse_above(source).is_some_and(|s| z > s) {
        return None;
    }
    let get = |z: i64, x: i64, y: i64| cache.get_tile(z, x, y).ok().flatten();
    let mut blob = r.fallback.tile(source, z, x, y, &get);
    if blob.is_none() && source != ts::DEFAULT_TERRAIN {
        blob = terrain_from_default(core, r, source, z, x, y);
    }
    blob
}

/// A keyed service's elevation tile, stood in for by the free one, re-packed
/// for `source`.
fn terrain_from_default(core: &Core, r: &Resources, source: &str, z: i64, x: i64, y: i64) -> Option<Vec<u8>> {
    let spec = ts::get(source)?.terrain()?;
    let base_spec = ts::terrain(ts::DEFAULT_TERRAIN)?;
    let base = default_terrain_png(core, r, z, x, y)?;
    dem_tiles::transcode(&base, base_spec.encoding, spec.encoding, spec.tile_size as usize)
}

/// The free default elevation tile: cached, fetched or derived, in that order.
fn default_terrain_png(core: &Core, r: &Resources, z: i64, x: i64, y: i64) -> Option<Vec<u8>> {
    let id = ts::DEFAULT_TERRAIN;
    let cache = r.caches.get(id)?;
    let mut base = cache.get_tile(z, x, y).ok().flatten();
    if base.is_none() {
        base = fetch_upstream(core, r, id, z, x, y);
        if let Some(b) = &base {
            let _ = cache.put_tile(z, x, y, b);
        }
    }
    if base.is_none() {
        let get = |z: i64, x: i64, y: i64| cache.get_tile(z, x, y).ok().flatten();
        base = r.fallback.tile(id, z, x, y, &get);
    }
    base
}

/// The free default elevation for any tile, past its own last zoom too.
fn default_terrain_at(core: &Core, r: &Resources, z: i64, x: i64, y: i64) -> Option<Vec<u8>> {
    let spec = ts::terrain(ts::DEFAULT_TERRAIN)?;
    let top = spec.maxzoom;
    if z <= top {
        return default_terrain_png(core, r, z, x, y);
    }
    let dz = z - top;
    let ancestor = default_terrain_png(core, r, top, x >> dz, y >> dz)?;
    let mask = (1i64 << dz) - 1;
    dem_tiles::overzoom(&ancestor, dz as u32, x & mask, y & mask)
}

/// Metres of the free default elevation for a tile, 256 a side, or None.
fn default_terrain_heights(core: &Core, r: &Resources, z: i64, x: i64, y: i64) -> Option<Vec<f64>> {
    let spec = ts::terrain(ts::DEFAULT_TERRAIN)?;
    let dz = (z - spec.maxzoom).max(0);
    let png = default_terrain_png(core, r, z - dz, x >> dz, y >> dz)?;
    let mask = (1i64 << dz) - 1;
    dem_tiles::heights(&png, spec.encoding, dz as u32, x & mask, y & mask, Some(256))
}

/// A tile of a custom model, with the default elevation around it.
fn serve_custom_dem(
    core: &Core,
    r: &Resources,
    store: &super::custom_dem::CustomDemStore,
    model: &str,
    z: i64,
    x: i64,
    y: i64,
) -> Response {
    if out_of_range(z, x, y) {
        return Response::error(404, "tile out of range");
    }
    let mut blob = None;
    let mut complete = true;
    if store.covers(model, z, x, y) {
        let base = |z: i64, x: i64, y: i64| default_terrain_heights(core, r, z, x, y);
        if let Some((b, c)) = store.tile(model, z, x, y, Some(&base)) {
            blob = Some(b);
            complete = c;
        }
    } else {
        blob = default_terrain_at(core, r, z, x, y);
        if blob.is_none() {
            if let Some((b, _)) = store.tile(model, z, x, y, None) {
                blob = Some(b);
                complete = false;
            }
        }
    }
    match blob {
        Some(b) => png(b, complete),
        None => Response::error(404, "tile not available"),
    }
}

fn buildings_body(body: &[u8], gzipped: bool, cache: bool) -> Response {
    let mut resp = Response::bytes(200, "application/geo+json", body.to_vec());
    if gzipped {
        resp = resp.with_header("Content-Encoding", "gzip");
    }
    resp.with_header("Cache-Control", if cache { "max-age=86400" } else { "no-store" })
}

/// The OSM building footprints of one grid cell as GeoJSON. Always 200 with a
/// FeatureCollection, even when there is nothing to give: this is on the map's
/// render path, and "no buildings" and "the service is down" draw the same.
fn buildings_serve(core: &Core, req: &Request) -> Response {
    let (z, x, y) = (parse_coord(req, "z"), parse_coord(req, "x"), parse_coord(req, "y"));
    let r = res(core);
    let service = r.as_ref().and_then(|r| r.buildings.as_ref());
    let Some(service) = service.filter(|_| z == CELL_ZOOM && x >= 0 && y >= 0 && x < (1i64 << z) && y < (1i64 << z))
    else {
        return buildings_body(EMPTY_BUILDINGS, false, true);
    };
    match service.lookup(z, x, y) {
        (Some(payload), _) => buildings_body(&payload, true, true),
        (None, coming) => buildings_body(
            if coming {
                PENDING_BUILDINGS
            } else {
                UNAVAILABLE_BUILDINGS
            },
            false,
            false,
        ),
    }
}
