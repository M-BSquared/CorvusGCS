//! The map data area of the core: tile sources and the offline cache, the tile
//! proxy, region downloads, elevation tiles, the operator's own elevation
//! models, OSM buildings and the place search (`tile_*.py`, `dem_*.py`,
//! `custom_dem.py`, `buildings.py`, `crs.py`, `geocode.py`, `routes/tiles.py`
//! and `routes/dem.py` of the Python program).

pub mod crs;
pub mod fetch;
pub mod geocode;
pub mod tile_cache;
pub mod tile_http;
pub mod tile_proxy;
pub mod tile_sessions;
pub mod tile_sources;
pub mod util;
