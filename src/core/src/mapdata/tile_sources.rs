//! Tile source registry: upstream URL templates, providers and labels
//! (`corvus/tile_sources.py`).
//!
//! One definition of every upstream URL, shared by the frontend (layer
//! switcher, offline downloader) and the serve and download paths. Three
//! views over the same data: [`TILE_SOURCES`] (the flat registry of base
//! layers, which also names each layer's cache file `<id>.mbtiles`),
//! [`PROVIDERS`] (which service serves which layers) and [`TERRAIN_SOURCES`]
//! (RGB encoded elevation tiles, which flow through the same route and cache
//! but are never a base layer).
//!
//! Template tokens: `{z}` `{x}` `{y}` (XYZ row, in any order), `{q}` (Bing
//! quadkey), `{s}` (a shard derived from x and y) and `{k}` (the operator's
//! API key, percent encoded and substituted last). A key never reaches the
//! browser: it is substituted into the upstream URL inside this process.

use std::sync::OnceLock;

use serde_json::{json, Value};

use super::util::quote;

/// The licensed API a keyed layer switches to (see `tile_sessions`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Licensed {
    pub api: &'static str,
    pub map_type: &'static str,
    pub imagery_set: &'static str,
    pub layer_types: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileSource {
    pub id: &'static str,
    pub label: &'static str,
    pub provider: &'static str,
    pub style: &'static str,
    pub upstream: &'static str,
    pub maxzoom: i64,
    pub attribution: &'static str,
    pub licensed: Option<Licensed>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerrainSource {
    pub id: &'static str,
    pub label: &'static str,
    pub provider: &'static str,
    pub encoding: &'static str,
    pub tile_size: i64,
    pub upstream: &'static str,
    pub maxzoom: i64,
    pub sparse_above: Option<i64>,
    pub attribution: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenMeta {
    pub label: &'static str,
    pub signup: &'static str,
    pub help: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Provider {
    pub id: &'static str,
    pub label: &'static str,
    pub sources: &'static [&'static str],
    pub token: Option<TokenMeta>,
    pub token_optional: bool,
}

pub static TILE_SOURCES: &[TileSource] = &[
    TileSource {
        id: "satellite",
        label: "Satellite",
        provider: "esri",
        style: "satellite",
        upstream: "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
        maxzoom: 19,
        attribution: "© Esri, Maxar, Earthstar Geographics",
        licensed: None,
    },
    TileSource {
        id: "streets",
        label: "Streets",
        provider: "esri",
        style: "streets",
        upstream: "https://server.arcgisonline.com/ArcGIS/rest/services/World_Street_Map/MapServer/tile/{z}/{y}/{x}",
        maxzoom: 19,
        attribution: "© Esri, HERE, Garmin, NGA, USGS",
        licensed: None,
    },
    TileSource {
        id: "hybrid",
        label: "Hybrid",
        provider: "esri",
        style: "hybrid",
        upstream: "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
        maxzoom: 19,
        attribution: "© Esri, Maxar, Earthstar Geographics",
        licensed: None,
    },
    TileSource {
        id: "topo",
        label: "Topographic",
        provider: "esri",
        style: "topo",
        upstream: "https://server.arcgisonline.com/ArcGIS/rest/services/World_Topo_Map/MapServer/tile/{z}/{y}/{x}",
        maxzoom: 19,
        attribution: "© Esri, HERE, Garmin, USGS, NGA",
        licensed: None,
    },
    TileSource {
        id: "osm",
        label: "OpenStreetMap",
        provider: "osm",
        style: "streets",
        upstream: "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
        maxzoom: 19,
        attribution: "© OpenStreetMap contributors",
        licensed: None,
    },
    TileSource {
        id: "google_satellite",
        label: "Satellite",
        provider: "google",
        style: "satellite",
        upstream: "https://mt{s}.google.com/vt/lyrs=s&hl=en&x={x}&y={y}&z={z}",
        maxzoom: 20,
        attribution: "© Google",
        licensed: Some(Licensed {
            api: "google_tiles",
            map_type: "satellite",
            imagery_set: "",
            layer_types: &[],
        }),
    },
    TileSource {
        id: "google_streets",
        label: "Streets",
        provider: "google",
        style: "streets",
        upstream: "https://mt{s}.google.com/vt/lyrs=m&hl=en&x={x}&y={y}&z={z}",
        maxzoom: 20,
        attribution: "© Google",
        licensed: Some(Licensed {
            api: "google_tiles",
            map_type: "roadmap",
            imagery_set: "",
            layer_types: &[],
        }),
    },
    TileSource {
        id: "google_hybrid",
        label: "Hybrid",
        provider: "google",
        style: "hybrid",
        upstream: "https://mt{s}.google.com/vt/lyrs=y&hl=en&x={x}&y={y}&z={z}",
        maxzoom: 20,
        attribution: "© Google",
        licensed: Some(Licensed {
            api: "google_tiles",
            map_type: "satellite",
            imagery_set: "",
            layer_types: &["layerRoadmap"],
        }),
    },
    TileSource {
        id: "google_topo",
        label: "Terrain",
        provider: "google",
        style: "topo",
        upstream: "https://mt{s}.google.com/vt/lyrs=p&hl=en&x={x}&y={y}&z={z}",
        maxzoom: 18,
        attribution: "© Google",
        licensed: Some(Licensed {
            api: "google_tiles",
            map_type: "terrain",
            imagery_set: "",
            layer_types: &["layerRoadmap"],
        }),
    },
    TileSource {
        id: "maptiler_satellite",
        label: "Satellite",
        provider: "maptiler",
        style: "satellite",
        upstream: "https://api.maptiler.com/tiles/satellite-v2/{z}/{x}/{y}.jpg?key={k}",
        maxzoom: 20,
        attribution: "© MapTiler, © OpenStreetMap contributors",
        licensed: None,
    },
    TileSource {
        id: "maptiler_streets",
        label: "Streets",
        provider: "maptiler",
        style: "streets",
        upstream: "https://api.maptiler.com/maps/streets-v2/256/{z}/{x}/{y}.png?key={k}",
        maxzoom: 20,
        attribution: "© MapTiler, © OpenStreetMap contributors",
        licensed: None,
    },
    TileSource {
        id: "maptiler_hybrid",
        label: "Hybrid",
        provider: "maptiler",
        style: "hybrid",
        upstream: "https://api.maptiler.com/maps/hybrid/256/{z}/{x}/{y}.jpg?key={k}",
        maxzoom: 20,
        attribution: "© MapTiler, © OpenStreetMap contributors",
        licensed: None,
    },
    TileSource {
        id: "maptiler_topo",
        label: "Topographic",
        provider: "maptiler",
        style: "topo",
        upstream: "https://api.maptiler.com/maps/topo-v2/256/{z}/{x}/{y}.png?key={k}",
        maxzoom: 20,
        attribution: "© MapTiler, © OpenStreetMap contributors",
        licensed: None,
    },
    TileSource {
        id: "mapbox_satellite",
        label: "Satellite",
        provider: "mapbox",
        style: "satellite",
        upstream: "https://api.mapbox.com/v4/mapbox.satellite/{z}/{x}/{y}.jpg90?access_token={k}",
        maxzoom: 20,
        attribution: "© Mapbox, © Maxar",
        licensed: None,
    },
    TileSource {
        id: "mapbox_streets",
        label: "Streets",
        provider: "mapbox",
        style: "streets",
        upstream: "https://api.mapbox.com/styles/v1/mapbox/streets-v12/tiles/256/{z}/{x}/{y}?access_token={k}",
        maxzoom: 20,
        attribution: "© Mapbox, © OpenStreetMap contributors",
        licensed: None,
    },
    TileSource {
        id: "mapbox_hybrid",
        label: "Hybrid",
        provider: "mapbox",
        style: "hybrid",
        upstream:
            "https://api.mapbox.com/styles/v1/mapbox/satellite-streets-v12/tiles/256/{z}/{x}/{y}?access_token={k}",
        maxzoom: 20,
        attribution: "© Mapbox, © Maxar, © OpenStreetMap contributors",
        licensed: None,
    },
    TileSource {
        id: "mapbox_topo",
        label: "Outdoors",
        provider: "mapbox",
        style: "topo",
        upstream: "https://api.mapbox.com/styles/v1/mapbox/outdoors-v12/tiles/256/{z}/{x}/{y}?access_token={k}",
        maxzoom: 20,
        attribution: "© Mapbox, © OpenStreetMap contributors",
        licensed: None,
    },
    TileSource {
        id: "bing_satellite",
        label: "Satellite",
        provider: "bing",
        style: "satellite",
        upstream: "https://ecn.t{s}.tiles.virtualearth.net/tiles/a{q}.jpeg?g=1",
        maxzoom: 19,
        attribution: "© Microsoft, Earthstar Geographics",
        licensed: Some(Licensed {
            api: "bing_rest",
            map_type: "",
            imagery_set: "Aerial",
            layer_types: &[],
        }),
    },
    TileSource {
        id: "bing_streets",
        label: "Streets",
        provider: "bing",
        style: "streets",
        upstream: "https://ecn.t{s}.tiles.virtualearth.net/tiles/r{q}.png?g=1",
        maxzoom: 19,
        attribution: "© Microsoft, HERE",
        licensed: Some(Licensed {
            api: "bing_rest",
            map_type: "",
            imagery_set: "RoadOnDemand",
            layer_types: &[],
        }),
    },
    TileSource {
        id: "bing_hybrid",
        label: "Hybrid",
        provider: "bing",
        style: "hybrid",
        upstream: "https://ecn.t{s}.tiles.virtualearth.net/tiles/h{q}.jpeg?g=1",
        maxzoom: 19,
        attribution: "© Microsoft, Earthstar Geographics",
        licensed: Some(Licensed {
            api: "bing_rest",
            map_type: "",
            imagery_set: "AerialWithLabelsOnDemand",
            layer_types: &[],
        }),
    },
];

pub static TERRAIN_SOURCES: &[TerrainSource] = &[
    TerrainSource { id: "terrain", label: "SRTM (AWS Terrain Tiles)", provider: "terrain", encoding: "terrarium", tile_size: 256, upstream: "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png", maxzoom: 15, sparse_above: None, attribution: "Elevation: AWS Terrain Tiles: SRTM, USGS NED, and national datasets" },
    TerrainSource { id: "copernicus", label: "Copernicus GLO-30", provider: "copernicus", encoding: "terrarium", tile_size: 512, upstream: "https://tiles.mapterhorn.com/{z}/{x}/{y}.webp", maxzoom: 17, sparse_above: Some(12), attribution: "Elevation: © Mapterhorn (mapterhorn.com/attribution), Copernicus GLO-30 © DLR e.V. 2010 to 2014 and © Airbus Defence and Space GmbH 2014 to 2018, provided under COPERNICUS by the European Union and ESA" },
    TerrainSource { id: "maptiler_terrain", label: "MapTiler Terrain", provider: "maptiler", encoding: "mapbox", tile_size: 512, upstream: "https://api.maptiler.com/tiles/terrain-rgb-v2/{z}/{x}/{y}.webp?key={k}", maxzoom: 14, sparse_above: None, attribution: "Elevation: © MapTiler" },
    TerrainSource { id: "mapbox_terrain", label: "Mapbox Terrain", provider: "mapbox", encoding: "mapbox", tile_size: 256, upstream: "https://api.mapbox.com/v4/mapbox.terrain-rgb/{z}/{x}/{y}.pngraw?access_token={k}", maxzoom: 15, sparse_above: None, attribution: "Elevation: © Mapbox" },
];

pub static PROVIDERS: &[Provider] = &[
    Provider { id: "esri", label: "Esri", sources: &["satellite", "streets", "hybrid", "topo"], token: None, token_optional: false },
    Provider { id: "osm", label: "OpenStreetMap", sources: &["osm"], token: None, token_optional: false },
    Provider { id: "google", label: "Google", sources: &["google_satellite", "google_streets", "google_hybrid", "google_topo"], token: Some(TokenMeta { label: "Google Maps API key", signup: "https://console.cloud.google.com/google/maps-apis/credentials", help: "Optional. With a key, Google tiles load through the licensed Map Tiles API, which has to be enabled for that key." }), token_optional: true },
    Provider { id: "bing", label: "Bing", sources: &["bing_satellite", "bing_streets", "bing_hybrid"], token: Some(TokenMeta { label: "Bing Maps key", signup: "https://www.bingmapsportal.com/", help: "Optional. With a key, Bing tiles load through the licensed Bing Maps REST API. Needs a Bing Maps for Enterprise key." }), token_optional: true },
    Provider { id: "maptiler", label: "MapTiler", sources: &["maptiler_satellite", "maptiler_streets", "maptiler_hybrid", "maptiler_topo"], token: Some(TokenMeta { label: "MapTiler API key", signup: "https://cloud.maptiler.com/account/keys/", help: "Licensed satellite and street tiles, on the operator's own quota rather than a shared public endpoint." }), token_optional: false },
    Provider { id: "mapbox", label: "Mapbox", sources: &["mapbox_satellite", "mapbox_streets", "mapbox_hybrid", "mapbox_topo"], token: Some(TokenMeta { label: "Mapbox access token", signup: "https://account.mapbox.com/access-tokens/", help: "Maxar satellite imagery and Mapbox street cartography. A public (pk.*) token is what this needs." }), token_optional: false },
];

/// The DEM every other one falls back to, and the one an offline download
/// always carries: keyless, PNG and complete at every level it serves.
pub const DEFAULT_TERRAIN: &str = "terrain";
/// The DEM 3D draws from when the operator has not chosen one.
pub const PREFERRED_TERRAIN: &str = "copernicus";
/// The provider used when nothing is configured.
pub const DEFAULT_PROVIDER: &str = "esri";

pub const GOOGLE_TILES_SESSION_URL: &str = "https://tile.googleapis.com/v1/createSession?key={k}";
pub const GOOGLE_TILES_URL: &str = "https://tile.googleapis.com/v1/2dtiles/{z}/{x}/{y}?session={session}&key={k}";
pub const BING_METADATA_URL: &str =
    "https://dev.virtualearth.net/REST/v1/Imagery/Metadata/{imagery_set}?output=json&uriScheme=https&key={k}";

/// A source of either kind, as the tile route and the downloader look it up.
#[derive(Debug, Clone, Copy)]
pub enum Spec {
    Tile(&'static TileSource),
    Terrain(&'static TerrainSource),
}

impl Spec {
    pub fn id(&self) -> &'static str {
        match self {
            Spec::Tile(s) => s.id,
            Spec::Terrain(s) => s.id,
        }
    }

    pub fn provider(&self) -> &'static str {
        match self {
            Spec::Tile(s) => s.provider,
            Spec::Terrain(s) => s.provider,
        }
    }

    pub fn upstream(&self) -> &'static str {
        match self {
            Spec::Tile(s) => s.upstream,
            Spec::Terrain(s) => s.upstream,
        }
    }

    pub fn maxzoom(&self) -> i64 {
        match self {
            Spec::Tile(s) => s.maxzoom,
            Spec::Terrain(s) => s.maxzoom,
        }
    }

    pub fn terrain(&self) -> Option<&'static TerrainSource> {
        match self {
            Spec::Terrain(t) => Some(t),
            Spec::Tile(_) => None,
        }
    }
}

/// Every servable source id: base layers, then elevation sources.
pub fn all_ids() -> Vec<&'static str> {
    TILE_SOURCES
        .iter()
        .map(|s| s.id)
        .chain(TERRAIN_SOURCES.iter().map(|s| s.id))
        .collect()
}

/// The source definition for `source_id`, base layers and elevation alike.
pub fn get(source_id: &str) -> Option<Spec> {
    if let Some(s) = TILE_SOURCES.iter().find(|s| s.id == source_id) {
        return Some(Spec::Tile(s));
    }
    TERRAIN_SOURCES.iter().find(|s| s.id == source_id).map(Spec::Terrain)
}

fn base(source_id: &str) -> Option<&'static TileSource> {
    TILE_SOURCES.iter().find(|s| s.id == source_id)
}

pub fn terrain(source_id: &str) -> Option<&'static TerrainSource> {
    TERRAIN_SOURCES.iter().find(|s| s.id == source_id)
}

/// The Bing quadkey for tile (z, x, y); z = 0 yields "".
pub fn quadkey(z: i64, x: i64, y: i64) -> String {
    let mut out = String::new();
    for i in (1..=z).rev() {
        let mask = 1i64 << (i - 1);
        let mut digit = 0;
        if x & mask != 0 {
            digit += 1;
        }
        if y & mask != 0 {
            digit += 2;
        }
        out.push_str(&digit.to_string());
    }
    out
}

/// The mirror index for a `{s}` sharded upstream, deterministic in (x, y).
pub fn server_shard(x: i64, y: i64) -> String {
    (x + y).rem_euclid(4).to_string()
}

/// Substitutes the tile placeholders in `template` for tile (z, x, y).
/// `{k}` is the operator's key, percent encoded and substituted last.
pub fn build_tile_url(template: &str, z: i64, x: i64, y: i64, token: &str) -> String {
    let mut url = template.to_string();
    if url.contains("{q}") {
        url = url.replace("{q}", &quadkey(z, x, y));
    }
    if url.contains("{s}") {
        url = url.replace("{s}", &server_shard(x, y));
    }
    url = url
        .replace("{z}", &z.to_string())
        .replace("{y}", &y.to_string())
        .replace("{x}", &x.to_string());
    if url.contains("{k}") {
        url = url.replace("{k}", &quote(token));
    }
    url
}

fn token_params() -> &'static Vec<String> {
    static PARAMS: OnceLock<Vec<String>> = OnceLock::new();
    PARAMS.get_or_init(|| {
        let mut out: Vec<String> = Vec::new();
        let templates = TILE_SOURCES
            .iter()
            .map(|s| s.upstream)
            .chain(TERRAIN_SOURCES.iter().map(|s| s.upstream))
            .chain([GOOGLE_TILES_SESSION_URL, GOOGLE_TILES_URL, BING_METADATA_URL]);
        for t in templates {
            for suffix in ["={k}", "={session}"] {
                let mut from = 0;
                while let Some(at) = t[from..].find(suffix) {
                    let end = from + at;
                    let start = t[..end].rfind(['?', '&']).filter(|&i| {
                        t[i + 1..end]
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
                    });
                    if let Some(i) = start {
                        let name = t[i + 1..end].to_string();
                        if !name.is_empty() && !out.contains(&name) {
                            out.push(name);
                        }
                    }
                    from = end + suffix.len();
                }
            }
        }
        out
    })
}

/// `url` with any API key blanked, for a log line.
pub fn redact_url(url: &str) -> String {
    let mut text = url.to_string();
    for param in token_params() {
        let marker = format!("{param}=");
        let mut start = text.find(&marker);
        while let Some(s) = start {
            let head = s + marker.len();
            let mut end = text.len();
            for sep in ['&', '#'] {
                if let Some(hit) = text[head..].find(sep) {
                    end = end.min(head + hit);
                }
            }
            text = format!("{}REDACTED{}", &text[..head], &text[end..]);
            let next = head + "REDACTED".len();
            start = text[next..].find(&marker).map(|i| i + next);
        }
    }
    text
}

/// True when `blob` starts like a PNG, JPEG, WebP or GIF tile.
pub fn looks_like_image(blob: &[u8]) -> bool {
    blob.starts_with(b"\x89PNG\r\n\x1a\n")
        || blob.starts_with(b"\xff\xd8\xff")
        || (blob.starts_with(b"RIFF") && blob.get(8..12) == Some(b"WEBP"))
        || blob.starts_with(b"GIF87a")
        || blob.starts_with(b"GIF89a")
}

/// The `token` block for `provider_id`, or `None` when it needs no key.
pub fn token_meta(provider_id: &str) -> Option<TokenMeta> {
    PROVIDERS.iter().find(|p| p.id == provider_id)?.token
}

/// Provider ids that take an operator supplied key, in registry order.
pub fn keyed_providers() -> Vec<&'static str> {
    PROVIDERS.iter().filter(|p| p.token.is_some()).map(|p| p.id).collect()
}

/// Does `provider_id` serve nothing at all without a key?
pub fn token_required(provider_id: &str) -> bool {
    PROVIDERS
        .iter()
        .find(|p| p.id == provider_id)
        .is_some_and(|p| p.token.is_some() && !p.token_optional)
}

/// Does serving `source_id` require a key the operator has to supply?
pub fn needs_token(source_id: &str) -> bool {
    get(source_id).is_some_and(|s| token_required(s.provider()))
}

/// The `licensed` block of `source_id`.
pub fn licensed(source_id: &str) -> Option<Licensed> {
    base(source_id)?.licensed
}

/// The last zoom `source_id` covers everywhere, when finer ones are regional.
pub fn sparse_above(source_id: &str) -> Option<i64> {
    terrain(source_id)?.sparse_above
}

pub fn is_terrain(source_id: &str) -> bool {
    terrain(source_id).is_some()
}

pub fn list_terrain() -> Vec<Value> {
    TERRAIN_SOURCES
        .iter()
        .map(|s| {
            json!({
                "id": s.id, "label": s.label, "provider": s.provider, "encoding": s.encoding,
                "tile_size": s.tile_size, "maxzoom": s.maxzoom, "sparse_above": s.sparse_above,
                "attribution": s.attribution,
            })
        })
        .collect()
}

pub fn list_sources() -> Vec<Value> {
    TILE_SOURCES
        .iter()
        .map(|s| {
            json!({
                "id": s.id, "label": s.label, "provider": s.provider, "style": s.style,
                "maxzoom": s.maxzoom, "attribution": s.attribution,
            })
        })
        .collect()
}

pub fn token_json(meta: Option<TokenMeta>) -> Value {
    match meta {
        Some(m) => json!({"label": m.label, "signup": m.signup, "help": m.help}),
        None => Value::Null,
    }
}

pub fn list_providers() -> Vec<Value> {
    PROVIDERS
        .iter()
        .map(|p| {
            json!({
                "id": p.id, "label": p.label, "sources": p.sources,
                "token": token_json(p.token), "token_optional": p.token_optional,
            })
        })
        .collect()
}

/// The provider id serving `source_id` (a base layer).
pub fn provider_of(source_id: &str) -> Option<&'static str> {
    base(source_id).map(|s| s.provider)
}

/// The source ids `provider_id` serves.
pub fn sources_for(provider_id: &str) -> Vec<&'static str> {
    PROVIDERS
        .iter()
        .find(|p| p.id == provider_id)
        .map(|p| p.sources.to_vec())
        .unwrap_or_default()
}

/// The source id for `provider_id` in `style`, or the provider's default.
pub fn resolve_source(provider_id: &str, style: Option<&str>) -> Option<&'static str> {
    let ids = sources_for(provider_id);
    let first = *ids.first()?;
    if let Some(style) = style.filter(|s| !s.is_empty()) {
        for id in &ids {
            if base(id).is_some_and(|s| s.style == style) {
                return Some(id);
            }
        }
    }
    Some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_consistent() {
        for p in PROVIDERS {
            for id in p.sources {
                let s = base(id).unwrap();
                assert_eq!(s.provider, p.id);
                assert_eq!(s.upstream.contains("{k}"), token_required(p.id), "{id}");
                assert_eq!(s.licensed.is_some(), p.token.is_some() && !token_required(p.id), "{id}");
            }
        }
        let mut listed: Vec<&str> = PROVIDERS.iter().flat_map(|p| p.sources.iter().copied()).collect();
        let mut all: Vec<&str> = TILE_SOURCES.iter().map(|s| s.id).collect();
        listed.sort();
        all.sort();
        assert_eq!(listed, all);
        assert!(terrain(DEFAULT_TERRAIN).is_some() && terrain(PREFERRED_TERRAIN).is_some());
        assert!(TERRAIN_SOURCES.iter().all(|t| base(t.id).is_none()));
    }

    #[test]
    fn a_key_is_one_opaque_value_and_never_logged() {
        let url = build_tile_url("https://x/{z}/{x}/{y}.png?key={k}&b=1", 3, 4, 5, "a&b#c{z}");
        assert_eq!(url, "https://x/3/4/5.png?key=a%26b%23c%7Bz%7D&b=1");
        let red = redact_url("https://api.mapbox.com/v4/x/1/2/3.jpg90?access_token=pk.secret&z=1#frag");
        assert_eq!(
            red,
            "https://api.mapbox.com/v4/x/1/2/3.jpg90?access_token=REDACTED&z=1#frag"
        );
        assert_eq!(
            redact_url("https://tile.googleapis.com/v1/2dtiles/1/2/3?session=abc&key=sekret"),
            "https://tile.googleapis.com/v1/2dtiles/1/2/3?session=REDACTED&key=REDACTED"
        );
    }
}
