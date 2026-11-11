//! The elevation file readers hold to what `dem_import.py` reads from the same
//! files (`python/tools/dump_mapdata_cases.py dem_import`).

use corvus_core::mapdata::dem_import as di;
use serde_json::Value;
use std::path::PathBuf;

fn data() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn cases() -> Value {
    serde_json::from_str(&std::fs::read_to_string(data().join("mapdata_dem_import_cases.json")).unwrap()).unwrap()
}

#[test]
fn every_file_reads_as_python_reads_it() {
    let c = cases();
    let list = c["cases"].as_array().unwrap();
    assert!(list.len() > 60);
    for case in list {
        let file = case["file"].as_str().unwrap();
        let filename = case["filename"].as_str().unwrap();
        let path = data().join("dem_import").join(file);
        let format = di::detect_format(&path, filename);
        if case["format"].is_null() {
            assert_eq!(format.unwrap_err().0, case["error"].as_str().unwrap(), "{file}");
            continue;
        }
        assert_eq!(format.unwrap(), case["format"].as_str().unwrap(), "{file}");
        let grid = di::read_grid(&path, filename);
        if let Some(want) = case.get("error") {
            assert_eq!(grid.unwrap_err().0, want.as_str().unwrap(), "{file}");
            continue;
        }
        let g = grid.unwrap_or_else(|e| panic!("{file}: {e}"));
        assert_eq!(g.width as u64, case["width"].as_u64().unwrap(), "{file}");
        assert_eq!(g.height as u64, case["height"].as_u64().unwrap(), "{file}");
        for (name, got) in [("x0", g.x0), ("y0", g.y0), ("dx", g.dx), ("dy", g.dy)] {
            assert_eq!(got, case[name].as_f64().unwrap(), "{file} {name}");
        }
        assert_eq!(g.epsg, case["epsg"].as_i64(), "{file} epsg");
        assert_eq!(di::guess_epsg(&g), case["guess"].as_i64(), "{file} guess");
        let want = case["values"].as_array().unwrap();
        assert_eq!(g.values.len(), want.len(), "{file}");
        for (i, (a, b)) in g.values.iter().zip(want).enumerate() {
            match b.as_f64() {
                None => assert!(a.is_nan(), "{file} cell {i}: {a} should be no data"),
                Some(b) => assert_eq!(f64::from(*a), b, "{file} cell {i}"),
            }
        }
    }
}

#[test]
fn commas_group_thousands() {
    assert_eq!(di::commas(0), "0");
    assert_eq!(di::commas(999), "999");
    assert_eq!(di::commas(1000), "1,000");
    assert_eq!(di::commas(120_000_000), "120,000,000");
}
