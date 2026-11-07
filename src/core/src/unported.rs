//! The routes of the areas that are not ported to Rust yet.
//!
//! Every route the Python server answers that no module registered answers
//! 501 "Not available in this build", so a client sees a clear refusal and not
//! a 404. A module that registers a route takes it over with no edit here:
//! the stub is only looked at when nothing is registered for the path.
//!
//! `tests/data/python_routes.json` is the list of everything the Python
//! server answers, dumped by `python/tools/dump_core_cases.py routes`.

use crate::request::Method;
use crate::router::Router;

const PYTHON_ROUTES: &str = include_str!("../tests/data/python_routes.json");

/// Adds a stub for every Python route to `router`.
pub(crate) fn add_stubs(router: &mut Router) -> Result<(), String> {
    let doc: serde_json::Value = serde_json::from_str(PYTHON_ROUTES).map_err(|e| format!("python_routes.json: {e}"))?;
    let list = |key: &str| -> Vec<String> {
        doc[key]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    for (method, keys) in [
        (Method::Get, ["get", "extra_get"]),
        (Method::Post, ["post", "extra_post"]),
        (Method::Delete, ["extra_delete", "extra_delete"]),
    ] {
        for key in keys {
            for path in list(key) {
                router.add_unavailable(method, &path);
            }
        }
    }
    for pattern in list("patterns_get") {
        router.add_unavailable_pattern(Method::Get, &pattern)?;
    }
    Ok(())
}
