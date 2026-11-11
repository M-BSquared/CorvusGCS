//! The operator's own elevation models hold to `custom_dem.py`
//! (`python/tools/dump_mapdata_cases.py custom_dem`).

use corvus_core::mapdata::custom_dem as cd;
use corvus_core::mapdata::dem_tiles;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn data() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn cases() -> Value {
    serde_json::from_str(&std::fs::read_to_string(data().join("mapdata_custom_dem_cases.json")).unwrap()).unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("corvus-custom-dem-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() <= 1e-9 + 1e-10 * b.abs(), "{what}: {a} vs {b}");
}

fn strip(m: &Value) -> Value {
    let mut m = m.clone();
    for k in ["id", "created_at", "bytes"] {
        m.as_object_mut().unwrap().remove(k);
    }
    m
}

#[test]
fn helpers_match_python() {
    let c = cases();
    for case in c["clean_name"].as_array().unwrap() {
        let got = cd::clean_name(case["raw"].as_str().unwrap(), case["fallback"].as_str().unwrap());
        assert_eq!(got, case["name"].as_str().unwrap(), "{case}");
    }
    for case in c["maxzoom"].as_array().unwrap() {
        let got = cd::maxzoom_for(case["res"].as_f64().unwrap(), case["lat"].as_f64().unwrap());
        assert_eq!(got, case["z"].as_i64().unwrap(), "{case}");
    }
    for case in c["lonlat"].as_array().unwrap() {
        let z = case["z"].as_i64().unwrap();
        let (px, py) = (case["px"].as_f64().unwrap(), case["py"].as_f64().unwrap());
        let (lon, lat) = cd::tile_lonlat(z, px, py);
        close(lon, case["lonlat"][0].as_f64().unwrap(), "lon");
        close(lat, case["lonlat"][1].as_f64().unwrap(), "lat");
        let (qx, qy) = cd::lonlat_pixel(z, lon, lat);
        close(qx, case["pixel"][0].as_f64().unwrap(), "px");
        close(qy, case["pixel"][1].as_f64().unwrap(), "py");
    }
    for case in c["overview"].as_array().unwrap() {
        let vals: Vec<f32> = case["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().map_or(f32::NAN, |x| x as f32))
            .collect();
        let (out, w2, h2) = cd::overview(
            &vals,
            case["width"].as_u64().unwrap() as usize,
            case["height"].as_u64().unwrap() as usize,
        );
        assert_eq!(
            (w2 as u64, h2 as u64),
            (case["w2"].as_u64().unwrap(), case["h2"].as_u64().unwrap())
        );
        for (a, b) in out.iter().zip(case["out"].as_array().unwrap()) {
            match b.as_f64() {
                None => assert!(a.is_nan()),
                Some(b) => assert_eq!(f64::from(*a), b),
            }
        }
    }
    for case in c["classify"].as_array().unwrap() {
        let vals: Vec<f32> = case["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().map_or(f32::NAN, |x| x as f32))
            .collect();
        let kind = cd::classify(
            &|i| vals[i],
            case["width"].as_u64().unwrap() as usize,
            case["height"].as_u64().unwrap() as usize,
            case["res"].as_f64().unwrap(),
        );
        assert_eq!(kind, case["kind"].as_str().unwrap(), "{}", case["name"]);
    }
}

fn base(z: i64, x: i64, y: i64) -> Option<Vec<f64>> {
    if z < 15 {
        return None;
    }
    Some(
        (0..65536usize)
            .map(|i| 300.0 + ((x * 7 + y * 3) as f64 + (i % 256) as f64 * 0.01 + (i / 256) as f64 * 0.02) % 50.0)
            .collect(),
    )
}

#[test]
fn a_model_imports_lists_renders_and_goes_like_python() {
    let c = cases();
    let dir = scratch("model");
    let src = data().join(c["grid"]["file"].as_str().unwrap());
    let store = cd::CustomDemStore::new(dir.join("store"));
    std::fs::create_dir_all(store.root()).unwrap();
    let model = store
        .import_file(&src, "Ground Model.asc", "  My <Model>  ", Some(25832), None)
        .unwrap();
    assert_eq!(strip(&model), c["model"]);
    let id = model["id"].as_str().unwrap().to_string();
    assert!(cd::is_model_id(&id));
    assert!(store.root().join(&id).join("model.json").is_file());
    let second = store
        .import_file(&src, "Second.asc", "", Some(25832), Some(&id))
        .unwrap();
    assert_eq!(strip(&second), c["second"]);
    let listing: Vec<Value> = store.list_models().iter().map(strip).collect();
    assert_eq!(Value::Array(listing), c["listing"]);
    let terrain: Vec<Value> = store
        .terrain_entries()
        .into_iter()
        .map(|mut t| {
            t.as_object_mut().unwrap().remove("id");
            t
        })
        .collect();
    assert_eq!(Value::Array(terrain), c["terrain"]);

    for t in c["tiles"].as_array().unwrap() {
        let (z, x, y) = (
            t["z"].as_i64().unwrap(),
            t["x"].as_i64().unwrap(),
            t["y"].as_i64().unwrap(),
        );
        let use_base = t["base"].as_bool().unwrap();
        let res = store.tile(&id, z, x, y, if use_base { Some(&base) } else { None });
        let label = format!("tile {z}/{x}/{y} base={use_base}");
        let (blob, complete) = res.unwrap_or_else(|| panic!("{label}: none"));
        assert_eq!(complete, t["complete"].as_bool().unwrap(), "{label}");
        assert_eq!(store.covers(&id, z, x, y), t["covers"].as_bool().unwrap(), "{label}");
        let px = dem_tiles::decode(&blob).unwrap().values;
        assert_eq!(
            px.iter().map(|v| u64::from(*v)).sum::<u64>(),
            t["sum"].as_u64().unwrap(),
            "{label} sum"
        );
        for (k, want) in t["samples"].as_array().unwrap().iter().enumerate() {
            assert_eq!(u64::from(px[k * 997]), want.as_u64().unwrap(), "{label} sample {k}");
        }
    }
    assert!(store.tile("dem_0000000000", 14, 0, 0, None).is_none());

    // A model reloads from its folder with the same answers.
    drop(store);
    let again = cd::CustomDemStore::new(dir.join("store"));
    assert!(again.has(&id));
    let reloaded: Vec<Value> = again.list_models().iter().map(strip).collect();
    assert_eq!(Value::Array(reloaded), c["listing"]);
    assert_eq!(again.rename(&id, " <new> name ").as_deref(), c["renamed"].as_str());
    assert_eq!(again.rename("dem_0000000000", "x"), None);
    assert!(c["norename"].is_null());
    assert_eq!(again.remove(&id), c["removed"].as_bool().unwrap());
    assert_eq!(again.remove(&id), c["gone"].as_bool().unwrap());
    assert!(!again.root().join(&id).exists());
    again.close();
    assert!(again.import_file(&src, "a.asc", "", Some(25832), None).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn imports_that_cannot_work_say_why_like_python() {
    let c = cases();
    let dir = scratch("errors");
    let store = cd::CustomDemStore::new(dir.join("store"));
    std::fs::create_dir_all(store.root()).unwrap();
    for case in c["errors"].as_array().unwrap() {
        let file: &Path = &dir.join("in.asc");
        std::fs::write(file, case["text"].as_str().unwrap()).unwrap();
        let got = store.import_file(
            file,
            case["filename"].as_str().unwrap(),
            "",
            case["epsg"].as_i64(),
            None,
        );
        match (got, case.get("error")) {
            (Err(e), Some(want)) => assert_eq!(e.0, want.as_str().unwrap(), "{}", case["label"]),
            (Ok(m), None) => assert_eq!(strip(&m), case["ok"], "{}", case["label"]),
            (other, _) => panic!("{}: {other:?}", case["label"]),
        }
    }
    let file = dir.join("in.asc");
    let missing = store
        .import_file(&file, "a.asc", "", Some(25832), Some("dem_ffffffffff"))
        .unwrap_err();
    assert_eq!(Some(missing.0.as_str()), c["missing"].as_str());
    store.close();
    let closed = store.import_file(&file, "a.asc", "", Some(25832), None).unwrap_err();
    assert_eq!(Some(closed.0.as_str()), c["closed"].as_str());
    let _ = std::fs::remove_dir_all(dir);
}
