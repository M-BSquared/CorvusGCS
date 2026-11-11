//! The map data area as a [`Module`]: tile caches per source, the downloader
//! pool, the upstream proxy, derived elevation tiles, the operator's own
//! elevation models, OSM buildings and the place search.
//!
//! Everything that holds a thread, a socket or a file is built in `start` and
//! closed in `stop`; `register` only declares routes and the `tiles` topic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde_json::Value;

use super::buildings::{self, BuildingService};
use super::custom_dem::CustomDemStore;
use super::dem_tiles::DemFallback;
use super::fetch::{Fetch, Http};
use super::geocode::Geocoder;
use super::tile_cache::{default_cache_dir, TileCache};
use super::tile_downloader::{tile_ranges, DownloaderPool};
use super::tile_http::ProgressBus;
use super::tile_proxy::TileProxy;
use super::tile_sources as ts;
use super::util::{expand_user, normpath};
use crate::core::Core;
use crate::events::{Binding, Topic};
use crate::module::{Module, Registry};

/// Capacity of the `tiles` topic (`TILES_PROGRESS_SSE_CAPACITY`).
pub const TILES_PROGRESS_SSE_CAPACITY: usize = 16;
/// Workers per download job (`_TileDownloaderPool(max_workers=3)`).
pub const DOWNLOAD_WORKERS: usize = 3;

/// What a test changes: the network, the upstream addresses and the folder.
#[derive(Clone, Default)]
pub struct MapDataOptions {
    /// Every outgoing request (tiles, Overpass, place search) goes through it.
    pub net: Option<Arc<dyn Fetch>>,
    pub overpass_url: Option<String>,
    pub geocode_url: Option<String>,
    /// The tile cache folder, instead of the configured or default one.
    pub cache_dir: Option<PathBuf>,
}

/// What `start` builds.
pub struct Resources {
    pub cache_dir: PathBuf,
    pub caches: HashMap<String, Arc<TileCache>>,
    pub bus: Arc<ProgressBus>,
    pub downloader: DownloaderPool,
    pub proxy: TileProxy,
    pub fallback: DemFallback,
    pub buildings: Option<BuildingService>,
    pub custom_dem: Option<CustomDemStore>,
    pub geocoder: Option<Geocoder>,
    pub net: Arc<dyn Fetch>,
}

/// The module's service, `core.service::<MapData>()`.
pub struct MapData {
    options: MapDataOptions,
    resources: OnceLock<Arc<Resources>>,
}

impl MapData {
    /// The resources, once the module has started.
    pub fn resources(&self) -> Option<Arc<Resources>> {
        self.resources.get().cloned()
    }

    fn stop(&self) {
        let Some(r) = self.resources.get() else { return };
        r.downloader.shutdown();
        if let Some(b) = &r.buildings {
            b.close();
        }
        if let Some(c) = &r.custom_dem {
            c.close();
        }
        for cache in r.caches.values() {
            cache.close();
        }
    }
}

pub struct MapDataModule {
    inner: Arc<MapData>,
}

impl MapDataModule {
    pub fn new() -> MapDataModule {
        MapDataModule::with_options(MapDataOptions::default())
    }

    pub fn with_options(options: MapDataOptions) -> MapDataModule {
        MapDataModule {
            inner: Arc::new(MapData {
                options,
                resources: OnceLock::new(),
            }),
        }
    }
}

impl Default for MapDataModule {
    fn default() -> Self {
        MapDataModule::new()
    }
}

/// The folder the per source `.mbtiles` caches live in: the operator's
/// override (with `~` expanded) or the default.
pub fn tile_cache_dir(configured: &str, state: &Path) -> PathBuf {
    let configured = configured.trim();
    if configured.is_empty() {
        default_cache_dir(state)
    } else {
        PathBuf::from(normpath(&expand_user(configured)))
    }
}

/// `CorvusGCS/<version>`, what every outgoing request calls itself.
pub fn user_agent() -> String {
    format!("CorvusGCS/{}", crate::version::version())
}

/// Closes out regions a previous run left in state `running`: at startup no
/// job exists, so such a region lost its process before the final update.
/// It becomes `cancelled` with the tile count the cache holds for its area.
fn settle_interrupted_regions(cache: &TileCache) -> usize {
    let mut settled = 0;
    for region in cache.list_regions() {
        if region.state != "running" {
            continue;
        }
        let ranges = tile_ranges(region.bounds(), region.minzoom, region.maxzoom);
        let count = cache.count_tiles(&ranges);
        cache.update_region(
            &region.id,
            &[("state", Value::from("cancelled")), ("tile_count", Value::from(count))],
        );
        settled += 1;
    }
    settled
}

impl Module for MapDataModule {
    fn name(&self) -> &'static str {
        "mapdata"
    }

    fn register(&self, reg: &mut Registry) {
        reg.service(Arc::clone(&self.inner));
        super::routes::register(reg);
        reg.topic(Topic::new("tiles", TILES_PROGRESS_SSE_CAPACITY, |core, sink, job| {
            let Some(job) = job.filter(|j| !j.is_empty()) else {
                return Binding::none();
            };
            let Some(res) = core.service::<MapData>().and_then(|m| m.resources()) else {
                return Binding::none();
            };
            let job = job.to_string();
            let put = sink.clone();
            let listener: super::tile_http::Listener =
                Arc::new(move |progress: &Value| put.put_latest(progress.clone()));
            let id = res.bus.subscribe(&job, listener);
            let Some(status) = res.downloader.status(&job) else {
                res.bus.unsubscribe(&job, id);
                return Binding::none();
            };
            let bus = Arc::clone(&res.bus);
            Binding {
                unbind: Box::new(move || bus.unsubscribe(&job, id)),
                initial: Some(status),
            }
        }));
    }

    fn start(&self, core: &Core) -> std::io::Result<()> {
        let o = &self.inner.options;
        let cfg = core.settings().get();
        let cache_dir = o
            .cache_dir
            .clone()
            .unwrap_or_else(|| tile_cache_dir(cfg.str_field("tile_cache_dir"), core.paths().home()));
        let net: Arc<dyn Fetch> = o.net.clone().unwrap_or_else(|| Arc::new(Http));
        let ua = user_agent();

        let mut caches: HashMap<String, Arc<TileCache>> = HashMap::new();
        let mut list: Vec<(String, Arc<TileCache>)> = Vec::new();
        for sid in ts::all_ids() {
            match TileCache::new(cache_dir.join(format!("{sid}.mbtiles"))) {
                Ok(cache) => {
                    settle_interrupted_regions(&cache);
                    let cache = Arc::new(cache);
                    caches.insert(sid.to_string(), Arc::clone(&cache));
                    list.push((sid.to_string(), cache));
                }
                Err(e) => eprintln!("corvus: tile cache for {sid} unavailable: {e}"),
            }
        }
        let bus = Arc::new(ProgressBus::new());
        let downloader = DownloaderPool::new(&list, &net, &ua, DOWNLOAD_WORKERS);
        let proxy = TileProxy::new(Arc::clone(&net), &ua);

        let building_options = buildings::Options {
            url: o
                .overpass_url
                .clone()
                .unwrap_or_else(|| buildings::OVERPASS_URL.to_string()),
            user_agent: ua.clone(),
            ..buildings::Options::default()
        };
        let buildings = match BuildingService::new(
            buildings::default_cache_path(&cache_dir),
            Arc::clone(&net),
            building_options,
        ) {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("corvus: building service init failed: {e}");
                None
            }
        };
        let custom_dem = {
            let root = cache_dir.join("custom-dem");
            match std::fs::create_dir_all(&root) {
                Ok(()) => Some(CustomDemStore::new(root)),
                Err(e) => {
                    eprintln!("corvus: custom elevation store init failed: {e}");
                    None
                }
            }
        };
        let geocoder = Some(match &o.geocode_url {
            Some(url) => Geocoder::with(&ua, url, super::geocode::Limits::default()),
            None => Geocoder::new(&ua),
        });
        let resources = Arc::new(Resources {
            cache_dir,
            caches,
            bus,
            downloader,
            proxy,
            fallback: DemFallback::default(),
            buildings,
            custom_dem,
            geocoder,
            net,
        });
        let _ = self.inner.resources.set(resources);
        Ok(())
    }

    fn stop(&self, _core: &Core) {
        self.inner.stop();
    }
}
