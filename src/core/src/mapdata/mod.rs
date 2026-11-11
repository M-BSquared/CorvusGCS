//! The map data area of the core: tile sources and the offline cache, the tile
//! proxy, region downloads, elevation tiles, the operator's own elevation
//! models, OSM buildings and the place search (`tile_*.py`, `dem_*.py`,
//! `custom_dem.py`, `buildings.py`, `crs.py`, `geocode.py`, `routes/tiles.py`
//! and `routes/dem.py` of the Python program).

pub mod buildings;
pub mod crs;
pub mod custom_dem;
pub mod dem_import;
pub mod dem_tiles;
pub mod fetch;
pub mod geocode;
pub mod tile_cache;
pub mod tile_downloader;
pub mod tile_http;
pub mod tile_proxy;
pub mod tile_sessions;
pub mod tile_sources;
pub mod util;
