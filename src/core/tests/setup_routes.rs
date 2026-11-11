//! The setup page routes are registered (no longer 501) and answer the "no vehicle"
//! shape the Python routes answer with a bridge that has no link: 200, `connected`
//! false and the reason, with the page empty.

use corvus_core::{CoreBuilder, Options, SetupSchemasModule, VehicleModule};
use serde_json::json;

#[test]
fn the_pages_answer_without_a_vehicle() {
    let dir = std::env::temp_dir().join(format!("corvus-setup-routes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let core = CoreBuilder::new(Options::in_dir(&dir))
        .module(VehicleModule::new())
        .module(SetupSchemasModule::new())
        .build()
        .unwrap();
    core.start().unwrap();

    let (status, safety) = core.internal_call("GET", "/api/safety?extra=A,b", None);
    assert_eq!(status, 200);
    assert_eq!(safety["connected"], false);
    assert_eq!(safety["sections"], json!([]));
    assert_eq!(safety["received"], 0);
    assert_eq!(safety["error"], "not connected");
    assert_eq!(safety["extra"][0]["param"], "A");
    assert_eq!(safety["extra"][1]["param"], "B");
    assert_eq!(safety["extra"][1]["present"], false);

    let (status, tuning) = core.internal_call("GET", "/api/tuning", None);
    assert_eq!(
        (status, &tuning["connected"], &tuning["groups"]),
        (200, &json!(false), &json!([]))
    );

    let (status, mounting) = core.internal_call("GET", "/api/mounting", None);
    assert_eq!(status, 200);
    assert_eq!(mounting["connected"], false);
    assert_eq!(mounting["positions"], json!([]));
    assert_eq!(mounting["orientation"], json!(null));

    let (status, battery) = core.internal_call("GET", "/api/battery", None);
    assert_eq!(status, 200);
    assert_eq!(battery["connected"], false);
    assert_eq!(battery["sections"], json!([]));
    assert_eq!(battery["pack"]["cells"], 0);
    assert_eq!(battery["settings"]["estimate"], false);
    assert_eq!(battery["configured"], json!({}));
    assert!(battery["chemistries"].as_array().is_some_and(|c| !c.is_empty()));
    core.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
