//! Every route the Python server answers is registered by a module of the core:
//! none is left to the 501 stub of `unported`.

use corvus_core::{
    CoreBuilder, FirmwareModule, Http, HttpOptions, LogsModule, MapDataModule, McpModule, Method, Options,
    PeripheralsModule, PluginsModule, SetupSchemasModule, VehicleModule,
};
use serde_json::Value;

const PYTHON_ROUTES: &str = include_str!("data/python_routes.json");

fn sample(pattern: &str) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let end = rest[start..].find('}').map_or(rest.len(), |e| start + e);
        let name = &rest[start + 1..end];
        out.push_str(if name.ends_with(":d") { "1" } else { "x" });
        rest = rest.get(end + 1..).unwrap_or("");
    }
    out.push_str(rest);
    out.replace("x*", "x")
}

#[test]
fn no_python_route_answers_501() {
    let dir = std::env::temp_dir().join(format!("corvus-no501-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let core = CoreBuilder::new(Options::in_dir(&dir))
        .module(VehicleModule::new())
        .module(PeripheralsModule::new())
        .module(LogsModule::new())
        .module(FirmwareModule::new())
        .module(PluginsModule::new())
        .module(McpModule::new())
        .module(SetupSchemasModule::new())
        .module(MapDataModule::new())
        .module(Http::new(HttpOptions {
            port: 0,
            ..HttpOptions::default()
        }))
        .build()
        .unwrap();
    let doc: Value = serde_json::from_str(PYTHON_ROUTES).unwrap();
    let mut missing = Vec::new();
    let mut total = 0;
    for (method, keys) in [
        (Method::Get, ["get", "extra_get", "patterns_get"]),
        (Method::Post, ["post", "extra_post", "post"]),
        (Method::Delete, ["extra_delete", "extra_delete", "extra_delete"]),
    ] {
        for key in keys {
            for path in doc[key].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if path == "/mcp" {
                    continue; // answered by the HTTP server itself, ahead of the router
                }
                total += 1;
                if core.router().find(method, &sample(path)).is_none() {
                    missing.push(format!("{} {path}", method.as_str()));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(total > 150, "route list looks empty: {total}");
    assert!(missing.is_empty(), "routes left to the 501 stub: {missing:#?}");
}
