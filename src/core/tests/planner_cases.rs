//! The Rust mission planner and fence answer what the Python ones answered
//! (`python/tools/dump_core_cases.py planner`): the same plans accepted and
//! refused with the same words, the same wire items, the same circles flown as
//! waypoints, the same mission read back, the same files on disk.

use std::path::Path;

use corvus_core::geofence;
use corvus_core::mission::*;
use corvus_core::paths::Paths;
use corvus_core::protocol::missions::{fence_items, DownloadedItem, PlanItem};
use serde_json::{json, Value as Json};

/// Equal numbers (1 and 1.0 are the same; a value computed with trigonometry
/// may differ in the last digits between two math libraries).
fn close(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            x == y || (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0)
        }
        (Json::Array(x), Json::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| close(p, q)),
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| close(v, w)))
        }
        _ => a == b,
    }
}

fn num(v: &Json) -> f64 {
    match v {
        Json::String(s) if s == "nan" => f64::NAN,
        Json::Null => f64::NAN,
        other => other.as_f64().unwrap(),
    }
}

fn nan_text(x: f64) -> Json {
    if x.is_nan() {
        json!("nan")
    } else {
        json!(x)
    }
}

fn plan_item(v: &Json) -> PlanItem {
    PlanItem {
        command: v["command"].as_i64().unwrap(),
        lat: num(&v["lat"]),
        lon: num(&v["lon"]),
        alt: num(&v["alt"]),
        params: v["params"].as_array().unwrap().iter().map(num).collect(),
        item: v["item"].as_i64(),
    }
}

fn plan_item_json(i: &PlanItem) -> Json {
    json!({"command": i.command, "lat": i.lat, "lon": i.lon, "alt": i.alt,
        "params": i.params.iter().map(|x| nan_text(*x)).collect::<Vec<_>>(), "item": i.item})
}

fn downloaded(v: &Json) -> DownloadedItem {
    let p = v["params"].as_array().unwrap();
    let opt = |i: usize| p[i].as_f64();
    DownloadedItem {
        command: v["command"].as_i64().unwrap(),
        frame: v["frame"].as_i64().unwrap(),
        lat: v["lat"].as_f64().unwrap_or(f64::NAN),
        lon: v["lon"].as_f64().unwrap_or(f64::NAN),
        alt: v["alt"].as_f64().unwrap_or(f64::NAN),
        params: [opt(0), opt(1), opt(2), opt(3)],
        autocontinue: 1,
    }
}

fn check(failures: &mut Vec<String>, what: &str, got: &Json, want: &Json) {
    if !close(got, want) {
        failures.push(format!("{what}:\n  got  {got}\n  want {want}"));
    }
}

fn set_mtime(path: &Path, seconds: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
        .unwrap();
}

#[test]
fn the_planner_answers_like_python() {
    let data: Json = serde_json::from_str(include_str!("data/planner_cases.json")).unwrap();
    let mut failures = Vec::new();

    for (input, want) in data["safe_names"].as_array().unwrap().iter().map(|c| (&c[0], &c[1])) {
        check(
            &mut failures,
            &format!("safe_plan_name({input})"),
            &json!(safe_plan_name(input)),
            want,
        );
    }
    for (input, want) in data["point_names"].as_array().unwrap().iter().map(|c| (&c[0], &c[1])) {
        check(
            &mut failures,
            &format!("clean_point_name({input})"),
            &json!(clean_point_name(input)),
            want,
        );
    }
    for case in data["refused"].as_array().unwrap() {
        let table: Vec<(i64, String)> = case[0]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.parse().unwrap(), v.as_str().unwrap().to_string()))
            .collect();
        check(
            &mut failures,
            "refused_item_types",
            &Json::Object(refused_item_types(&table)),
            &case[1],
        );
    }
    for case in data["labels"].as_array().unwrap() {
        check(
            &mut failures,
            "label_of_command",
            &json!(label_of_command(case[0].as_i64().unwrap())),
            &case[1],
        );
    }

    for case in data["validate"].as_array().unwrap() {
        let got = match validate_plan(&case[0]) {
            Ok(plan) => json!({"plan": plan, "error": ""}),
            Err(why) => json!({"plan": null, "error": why}),
        };
        check(
            &mut failures,
            &format!(
                "validate_plan({})",
                case[0].to_string().chars().take(200).collect::<String>()
            ),
            &got,
            &case[1],
        );
    }
    for case in data["to_items"].as_array().unwrap() {
        let got = Json::Array(plan_to_items(&case[0]).iter().map(plan_item_json).collect());
        check(
            &mut failures,
            &format!("plan_to_items({})", case[0]["name"]),
            &got,
            &case[1],
        );
    }
    for case in data["distance"].as_array().unwrap() {
        check(
            &mut failures,
            "plan_distance_m",
            &json!(plan_distance_m(&case[0])),
            &case[1],
        );
    }
    for case in data["circles"].as_array().unwrap() {
        let items: Vec<PlanItem> = case[0].as_array().unwrap().iter().map(plan_item).collect();
        let got = Json::Array(lower_circles(&items).iter().map(plan_item_json).collect());
        check(&mut failures, "lower_circles", &got, &case[1]);
    }
    for case in data["back"].as_array().unwrap() {
        let input = &case[0];
        let items: Vec<DownloadedItem> = input["items"].as_array().unwrap().iter().map(downloaded).collect();
        let home = input["home"]
            .as_object()
            .map(|h| (h["lat"].as_f64().unwrap(), h["lon"].as_f64().unwrap()));
        let got = items_to_plan(&items, home, input["home_alt"].as_f64(), "Vehicle mission");
        check(
            &mut failures,
            &format!(
                "items_to_plan({})",
                input["items"].to_string().chars().take(160).collect::<String>()
            ),
            &got,
            &case[1],
        );
    }
    for case in data["legs"].as_array().unwrap() {
        check(
            &mut failures,
            "leg_distance_m",
            &json!(leg_distance_m(&case[0], &case[1])),
            &case[2],
        );
    }

    let tmp = std::env::temp_dir().join(format!("corvus-planner-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    {
        let directory = tmp.join("missions");
        let store = &data["store"];
        for (i, step) in store["steps"].as_array().unwrap().iter().enumerate() {
            let want = &store["results"][i];
            let got = match step["op"].as_str().unwrap() {
                "list" => Json::Array(list_plans(&directory)),
                "read" => read_plan(&directory, step["name"].as_str().unwrap()).unwrap_or(Json::Null),
                "remove" => json!(remove_plan(&directory, step["name"].as_str().unwrap())),
                "write" => {
                    let stored = write_plan(&directory, step["name"].as_str().unwrap(), &step["plan"]).unwrap();
                    let text = std::fs::read_to_string(directory.join(format!("{stored}.json"))).unwrap();
                    json!({"stored": stored, "text": text})
                }
                "touch" => {
                    set_mtime(
                        &directory.join(step["file"].as_str().unwrap()),
                        step["mtime"].as_u64().unwrap(),
                    );
                    Json::Null
                }
                "touch_all" => {
                    let mut names: Vec<String> = std::fs::read_dir(&directory)
                        .unwrap()
                        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                        .collect();
                    names.sort();
                    for (n, name) in names.iter().enumerate() {
                        set_mtime(&directory.join(name), 7000 + n as u64 * 100);
                    }
                    Json::Null
                }
                "raw" => {
                    let path = directory.join(step["file"].as_str().unwrap());
                    std::fs::write(&path, step["text"].as_str().unwrap()).unwrap();
                    set_mtime(&path, 100);
                    Json::Null
                }
                "files" => {
                    let mut names: Vec<String> = std::fs::read_dir(&directory)
                        .unwrap()
                        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                        .collect();
                    names.sort();
                    json!(names)
                }
                other => panic!("{other}"),
            };
            check(&mut failures, &format!("store step {i} {}", step["op"]), &got, want);
        }
    }
    for case in data["missions_dir"].as_array().unwrap() {
        let got = missions_dir(case[0].as_str().unwrap(), &Paths::at(&tmp));
        check(&mut failures, "missions_dir", &json!(got.to_string_lossy()), &case[1]);
    }
    for (i, empty) in ["", "  "].iter().enumerate() {
        let got = missions_dir(empty, &Paths::at(&tmp));
        assert!(got.ends_with("missions") == data["missions_dir_default"][i].as_bool().unwrap());
    }

    for case in data["polygons"].as_array().unwrap() {
        let got = match geofence::validate_polygon(&case[0]) {
            Ok(polygon) => {
                json!({"polygon": polygon.iter().map(|(lon, lat)| json!([lon, lat])).collect::<Vec<_>>(), "error": ""})
            }
            Err(why) => json!({"polygon": null, "error": why}),
        };
        check(&mut failures, &format!("validate_polygon({})", case[0]), &got, &case[1]);
    }
    for case in data["fence_items"].as_array().unwrap() {
        let polygon: Vec<(f64, f64)> = case[0]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap()))
            .collect();
        let got = Json::Array(
            fence_items(&polygon)
                .iter()
                .map(|i| json!({"command": i.command, "lat": i.lat, "lon": i.lon, "params": i.params}))
                .collect(),
        );
        check(&mut failures, "fence_items", &got, &case[1]);
    }
    let path = tmp.join("sub").join("geofence.json");
    for case in data["geofence_files"].as_array().unwrap() {
        let got = match case[0].as_str().unwrap() {
            "load" => geofence::load(&path),
            "save" => {
                geofence::save(&case[1], &path).unwrap();
                json!({"text": std::fs::read_to_string(&path).unwrap(), "loaded": geofence::load(&path)})
            }
            _ => {
                std::fs::write(&path, case[1].as_str().unwrap()).unwrap();
                geofence::load(&path)
            }
        };
        check(
            &mut failures,
            &format!("geofence {} {}", case[0], case[1]),
            &got,
            &case[2],
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.iter().take(12).cloned().collect::<Vec<_>>().join("\n\n")
    );
}
