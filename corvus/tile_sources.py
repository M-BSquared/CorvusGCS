"""Tile source registry — upstream URL templates + labels.

Centralizes the upstream tile URLs so the frontend (source picker) and the
downloader share one definition; an upstream URL is never duplicated across
modules.

ArcGIS REST tile endpoints place path components in ``{z}/{y}/{x}`` order
(ArcGIS-native; *y* here is the slippy-map / XYZ row, not the TMS-flipped
row). The downloader builds the final URL by replacing the literal tokens
``{z}``, ``{y}``, ``{x}`` in whichever order they appear in the template, so
it is agnostic to a given source's path convention. The server's
``_fetch_upstream_tile`` replaces the same tokens, keeping the two in sync
through this one registry.
"""
from __future__ import annotations

from typing import Any

# The source registry. ``upstream`` carries the {z}/{y}/{x} (or {z}/{x}/{y})
# template tokens the downloader/serve path substitute at fetch time. The
# token substitution is order-agnostic, so ArcGIS's {z}/{y}/{x} and OSM's
# {z}/{x}/{y} both work unchanged. ``maxzoom`` is the highest zoom the
# upstream serves (requests above it cannot be filled). ``attribution`` is
# the legally-required credit string for the source; the frontend mirrors
# it on its raster source so MapLibre's attribution control can show it.
TILE_SOURCES: dict[str, dict[str, Any]] = {
    "satellite": {
        "label": "Satellite",
        "upstream": "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
        "maxzoom": 19,
        "attribution": "© Esri, Maxar, Earthstar Geographics",
    },
    "streets": {
        "label": "Streets",
        "upstream": "https://server.arcgisonline.com/ArcGIS/rest/services/World_Street_Map/MapServer/tile/{z}/{y}/{x}",
        "maxzoom": 19,
        "attribution": "© Esri, HERE, Garmin, NGA, USGS",
    },
    "hybrid": {
        "label": "Hybrid",
        "upstream": "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
        "maxzoom": 19,
        "attribution": "© Esri, Maxar, Earthstar Geographics",
    },
    "topo": {
        "label": "Topographic",
        "upstream": "https://server.arcgisonline.com/ArcGIS/rest/services/World_Topo_Map/MapServer/tile/{z}/{y}/{x}",
        "maxzoom": 19,
        "attribution": "© Esri, HERE, Garmin, USGS, NGA",
    },
    # OSM serves slippy-order tiles ({z}/{x}/{y}); the substitution code is
    # order-agnostic so this needs no special handling.
    "osm": {
        "label": "OpenStreetMap",
        "upstream": "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
        "maxzoom": 19,
        "attribution": "© OpenStreetMap contributors",
    },
}


def get(source_id: str) -> dict | None:
    """Return a copy of the source definition for *source_id*, or None."""
    entry = TILE_SOURCES.get(source_id)
    return dict(entry) if entry is not None else None


def list_sources() -> list[dict]:
    """Return ``[{id, label, maxzoom}, ...]`` for the frontend source picker."""
    return [
        {"id": sid, "label": s["label"], "maxzoom": s["maxzoom"]}
        for sid, s in TILE_SOURCES.items()
    ]
