pub mod bg;
pub mod fetch;
pub mod http;
pub mod sse;
#[cfg(not(target_family = "wasm"))]
pub mod stream;
/// In a plugin the program follows the stream and delivers its events.
#[cfg(target_family = "wasm")]
pub mod stream {
    pub use crate::guest::mail::Stream;
}

/// A GET of the station's own routes as something to wait for. In a plugin it
/// goes through the program, under the plugin's permissions.
pub fn station_get(
    ctx: &egui::Context,
    server: &http::Server,
    path: &str,
) -> bg::Pending<Result<serde_json::Value, String>> {
    #[cfg(not(target_family = "wasm"))]
    {
        let (server, path) = (server.clone(), path.to_string());
        bg::Pending::spawn(ctx, move || http::get_json(&server, &path))
    }
    #[cfg(target_family = "wasm")]
    {
        let _ = (ctx, server);
        crate::guest::mail::request("GET", path, None)
    }
}
