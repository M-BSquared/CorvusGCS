//! The interactive cache fill of the tile route: one tile asked of its
//! upstream, guarded by the shared breaker (`CorvusHandler._fetch_upstream_tile`
//! in `server.py`).
//!
//! Short timeout on purpose: this is the browser waiting for a map tile, and a
//! tile that takes longer than this has already missed the frame it was wanted
//! for. The offline downloader has its own, longer one.

use std::sync::Arc;
use std::time::Duration;

use super::fetch::{Fetch, Req};
use super::tile_http::{AbsentTiles, UpstreamBreaker};
use super::tile_sessions::Sessions;
use super::tile_sources as ts;

/// Interactive cache fill timeout.
pub const TILE_UPSTREAM_TIMEOUT_S: f64 = 4.0;
/// Largest tile body accepted from an upstream.
pub const TILE_UPSTREAM_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Everything the fill path shares between handler threads.
pub struct TileProxy {
    pub net: Arc<dyn Fetch>,
    pub breaker: UpstreamBreaker,
    pub absent: AbsentTiles,
    pub sessions: Sessions,
    pub user_agent: String,
    pub timeout: Duration,
}

impl TileProxy {
    pub fn new(net: Arc<dyn Fetch>, user_agent: &str) -> TileProxy {
        TileProxy {
            net,
            breaker: UpstreamBreaker::default(),
            absent: AbsentTiles::default(),
            sessions: Sessions::new(user_agent),
            user_agent: user_agent.to_string(),
            timeout: Duration::from_secs_f64(TILE_UPSTREAM_TIMEOUT_S),
        }
    }

    /// Best effort online cache fill for one tile; `None` on any failure.
    /// `token_for` is the stored key of a provider ("" when none).
    ///
    /// Once the network has failed a few times in a row this returns `None`
    /// at once instead of spending the timeout, so an offline pan renders its
    /// cached tiles at full speed and leaves the rest blank. An answer with a
    /// status code never opens the breaker (the link is up, and every source
    /// shares the breaker); a keyless keyed service never reaches it at all.
    pub fn fetch_upstream(
        &self,
        source: &str,
        z: i64,
        x: i64,
        y: i64,
        token_for: &dyn Fn(&str) -> String,
    ) -> Option<Vec<u8>> {
        let src = ts::get(source)?;
        let sparse = ts::sparse_above(source);
        let beyond_sparse = sparse.filter(|&s| z > s);
        if let Some(s) = beyond_sparse {
            if self.absent.covers(source, z, x, y, s) {
                return None;
            }
        }
        let token = token_for(src.provider());
        if ts::needs_token(source) && token.is_empty() {
            return None;
        }
        if !self.breaker.allow() {
            return None;
        }
        let mut template = src.upstream().to_string();
        let session_based = !token.is_empty() && ts::licensed(source).is_some();
        if session_based {
            match self.sessions.resolve(self.net.as_ref(), source, &token) {
                Ok(t) => template = t,
                Err(e) => {
                    if e.offline {
                        self.breaker.record_failure();
                    }
                    return None;
                }
            }
        }
        let url = ts::build_tile_url(&template, z, x, y, &token);
        let headers = [("User-Agent", self.user_agent.as_str())];
        let req = Req {
            url: &url,
            headers: &headers,
            body: None,
            timeout: self.timeout,
            max_body: TILE_UPSTREAM_MAX_BYTES,
        };
        let data = match self.net.send(&req) {
            Err(_) => {
                self.breaker.record_failure();
                return None;
            }
            Ok(r) if r.status >= 400 => {
                if session_based && matches!(r.status, 401 | 403) {
                    self.sessions.invalidate(source, &token);
                }
                if r.status == 404 && beyond_sparse.is_some() {
                    self.absent.add(source, z, x, y);
                }
                return None;
            }
            Ok(r) => r.body,
        };
        if data.is_empty() || !ts::looks_like_image(&data) {
            self.breaker.record_failure();
            return None;
        }
        self.breaker.record_success();
        Some(data)
    }
}

/// The `Content-Type` of a tile, sniffed from its magic bytes: satellite tiles
/// are JPEG, streets and topo PNG, a hardcoded one would mislabel them.
pub fn content_type(blob: &[u8]) -> &'static str {
    if blob.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if blob.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if blob.starts_with(b"RIFF") && blob.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        "image/png"
    }
}
