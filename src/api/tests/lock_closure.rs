//! What of `Cargo.lock` goes into the stamp of the plugin libraries (`build.rs`).

include!("../lock_closure.rs");

const LOCK: &str = r#"version = 4

[[package]]
name = "corvus_api"
version = "0.0.0"
dependencies = [
 "eframe",
 "twice 2.0.0",
]

[[package]]
name = "corvus_core"
version = "0.0.0"
dependencies = [
 "database",
]

[[package]]
name = "database"
version = "1.0.0"
checksum = "d1"

[[package]]
name = "eframe"
version = "0.37.0"
checksum = "e1"

[[package]]
name = "twice"
version = "1.0.0"
checksum = "t1"

[[package]]
name = "twice"
version = "2.0.0"
checksum = "t2"
"#;

#[test]
fn only_what_the_plugin_interface_is_built_from_counts() {
    let base = lock_closure(LOCK, "corvus_api");
    assert!(base.contains("\"e1\"") && base.contains("\"t2\""), "{base}");
    for outside in ["\"d1\"", "\"t1\"", "corvus_core"] {
        assert!(!base.contains(outside), "{outside} in {base}");
    }
    // The core may take a new version or another crate: the published plugins stay good.
    let core_moved = LOCK
        .replace("\"d1\"", "\"d2\"")
        .replace(" \"database\",\n", " \"database\",\n \"another\",\n");
    assert_eq!(lock_closure(&core_moved, "corvus_api"), base);
    // What a plugin library shares with the program may not.
    assert_ne!(lock_closure(&LOCK.replace("\"e1\"", "\"e2\""), "corvus_api"), base);
    assert_eq!(lock_closure(&LOCK.replace('\n', "\r\n"), "corvus_api"), base);
}

#[test]
fn a_lock_file_without_the_interface_counts_whole() {
    assert_eq!(lock_closure(LOCK, "nobody"), LOCK);
}

#[test]
fn the_repositorys_lock_file_is_read() {
    let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock")).unwrap();
    let closure = lock_closure(&lock, "corvus_api");
    for inside in ["egui", "eframe", "serde_json", "parking_lot_core", "epaint"] {
        assert!(closure.contains(&format!("name = \"{inside}\"\n")), "{inside}");
    }
    assert!(!closure.contains("name = \"corvus_core\"\n"));
}
