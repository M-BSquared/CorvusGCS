"""Tile source registry — upstream URL templates, providers, and labels.

Centralizes the upstream tile URLs so the frontend (map layer switcher,
Appearance settings, offline downloader) and the backend serve/download
paths share one definition; an upstream URL is never duplicated across
modules.

Two views over the same data
----------------------------
``TILE_SOURCES`` is the flat registry: one entry per concrete raster layer,
keyed by a stable source id that also names its on-disk cache
(``<id>.mbtiles``). ``PROVIDERS`` groups those ids by the *service* that
serves them (Esri / OpenStreetMap / Google / Bing) so the UI can offer "which
map service do you want to use" as a single choice and then show only that
service's layers. The Esri and OSM ids are unprefixed for backwards
compatibility: they predate the provider grouping and name existing caches
and persisted ``map.base_layer`` values.

URL templates
-------------
``upstream`` carries the placeholder tokens :func:`build_tile_url`
substitutes at fetch time:

``{z}`` ``{x}`` ``{y}``
    Slippy-map / XYZ zoom, column, row. *y* is always the XYZ row, never the
    TMS-flipped row. Substitution is order-agnostic, so ArcGIS's native
    ``{z}/{y}/{x}`` path order and OSM's ``{z}/{x}/{y}`` both work unchanged.
``{q}``
    Bing quadkey — the interleaved-bit encoding of (z, x, y) that Virtual
    Earth uses instead of separate path components.
``{s}``
    Server shard for providers that publish numbered mirrors. Derived
    deterministically from (x, y) so the same tile always resolves to the
    same URL (cache-friendly, and test-stable).

Both production URL builders — ``CorvusHandler._fetch_upstream_tile`` (the
online cache-fill) and ``TileDownloader._download_one`` (the pre-download
worker) — call :func:`build_tile_url`, so the two can never drift apart.

Terms of service
----------------
Only the Esri and OpenStreetMap endpoints below are documented public tile
services. The Google and Bing entries address their internal map tile
endpoints directly, which their terms of service do not permit outside of
their own SDKs/APIs. They are registered because the operator asked for
them; using them in a deployed product needs a proper licensed key (Google
Maps Tile API / Bing Maps Key) swapped into the template first.

stdlib only.
"""
from __future__ import annotations

from typing import Any

# The flat source registry. ``provider`` and ``style`` classify each entry for
# the grouped UI; ``maxzoom`` is the highest zoom the upstream serves (requests
# above it cannot be filled); ``attribution`` is the legally-required credit
# string, which the frontend mirrors onto its raster source so MapLibre's
# attribution control can show it.
TILE_SOURCES: dict[str, dict[str, Any]] = {
    # ---- Esri / ArcGIS (unprefixed ids: pre-date the provider grouping) ----
    "satellite": {
        "label": "Satellite",
        "provider": "esri",
        "style": "satellite",
        "upstream": "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
        "maxzoom": 19,
        "attribution": "© Esri, Maxar, Earthstar Geographics",
    },
    "streets": {
        "label": "Streets",
        "provider": "esri",
        "style": "streets",
        "upstream": "https://server.arcgisonline.com/ArcGIS/rest/services/World_Street_Map/MapServer/tile/{z}/{y}/{x}",
        "maxzoom": 19,
        "attribution": "© Esri, HERE, Garmin, NGA, USGS",
    },
    "hybrid": {
        "label": "Hybrid",
        "provider": "esri",
        "style": "hybrid",
        "upstream": "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
        "maxzoom": 19,
        "attribution": "© Esri, Maxar, Earthstar Geographics",
    },
    "topo": {
        "label": "Topographic",
        "provider": "esri",
        "style": "topo",
        "upstream": "https://server.arcgisonline.com/ArcGIS/rest/services/World_Topo_Map/MapServer/tile/{z}/{y}/{x}",
        "maxzoom": 19,
        "attribution": "© Esri, HERE, Garmin, USGS, NGA",
    },

    # ---- OpenStreetMap (single raster style) ----
    # OSM serves slippy-order tiles ({z}/{x}/{y}); the substitution code is
    # order-agnostic so this needs no special handling.
    "osm": {
        "label": "OpenStreetMap",
        "provider": "osm",
        "style": "streets",
        "upstream": "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
        "maxzoom": 19,
        "attribution": "© OpenStreetMap contributors",
    },

    # ---- Google (see the terms-of-service note in the module docstring) ----
    # lyrs= selects the layer: s=satellite, m=roadmap, y=hybrid, p=terrain.
    "google_satellite": {
        "label": "Satellite",
        "provider": "google",
        "style": "satellite",
        "upstream": "https://mt{s}.google.com/vt/lyrs=s&hl=en&x={x}&y={y}&z={z}",
        "maxzoom": 20,
        "attribution": "© Google",
    },
    "google_streets": {
        "label": "Streets",
        "provider": "google",
        "style": "streets",
        "upstream": "https://mt{s}.google.com/vt/lyrs=m&hl=en&x={x}&y={y}&z={z}",
        "maxzoom": 20,
        "attribution": "© Google",
    },
    "google_hybrid": {
        "label": "Hybrid",
        "provider": "google",
        "style": "hybrid",
        "upstream": "https://mt{s}.google.com/vt/lyrs=y&hl=en&x={x}&y={y}&z={z}",
        "maxzoom": 20,
        "attribution": "© Google",
    },
    "google_topo": {
        "label": "Terrain",
        "provider": "google",
        "style": "topo",
        "upstream": "https://mt{s}.google.com/vt/lyrs=p&hl=en&x={x}&y={y}&z={z}",
        "maxzoom": 18,
        "attribution": "© Google",
    },

    # ---- Bing / Virtual Earth (quadkey addressing, see {q} above) ----
    "bing_satellite": {
        "label": "Satellite",
        "provider": "bing",
        "style": "satellite",
        "upstream": "https://ecn.t{s}.tiles.virtualearth.net/tiles/a{q}.jpeg?g=1",
        "maxzoom": 19,
        "attribution": "© Microsoft, Earthstar Geographics",
    },
    "bing_streets": {
        "label": "Streets",
        "provider": "bing",
        "style": "streets",
        "upstream": "https://ecn.t{s}.tiles.virtualearth.net/tiles/r{q}.png?g=1",
        "maxzoom": 19,
        "attribution": "© Microsoft, HERE",
    },
    "bing_hybrid": {
        "label": "Hybrid",
        "provider": "bing",
        "style": "hybrid",
        "upstream": "https://ecn.t{s}.tiles.virtualearth.net/tiles/h{q}.jpeg?g=1",
        "maxzoom": 19,
        "attribution": "© Microsoft, Earthstar Geographics",
    },
}

# Provider grouping. ``sources`` lists the source ids this service serves, in
# the order the UI should present them; the first entry is the provider's
# default layer. Every id here MUST exist in TILE_SOURCES (asserted below).
PROVIDERS: dict[str, dict[str, Any]] = {
    "esri": {
        "label": "Esri",
        "sources": ["satellite", "streets", "hybrid", "topo"],
    },
    "osm": {
        "label": "OpenStreetMap",
        "sources": ["osm"],
    },
    "google": {
        "label": "Google",
        "sources": ["google_satellite", "google_streets", "google_hybrid", "google_topo"],
    },
    "bing": {
        "label": "Bing",
        "sources": ["bing_satellite", "bing_streets", "bing_hybrid"],
    },
}

# The provider used when nothing is configured. Esri keeps the historical
# default base layer ("satellite") working unchanged.
DEFAULT_PROVIDER = "esri"

# Human labels for the cross-provider style axis. The map layer switcher shows
# the provider's own per-source label; these exist so a style can be named
# independently of which provider is active (e.g. when carrying the operator's
# chosen style across a provider switch).
STYLE_LABELS: dict[str, str] = {
    "satellite": "Satellite",
    "streets": "Streets",
    "hybrid": "Hybrid",
    "topo": "Topographic",
}

# Fail loudly at import time on a registry typo rather than 404-ing tiles at
# runtime: every provider source id must be a real TILE_SOURCES key, and every
# source must belong to exactly one provider.
assert all(
    sid in TILE_SOURCES
    for prov in PROVIDERS.values()
    for sid in prov["sources"]
), "PROVIDERS references an unknown source id"
assert sorted(
    sid for prov in PROVIDERS.values() for sid in prov["sources"]
) == sorted(TILE_SOURCES), "PROVIDERS must cover every source exactly once"


def quadkey(z: int, x: int, y: int) -> str:
    """Return the Bing/Virtual-Earth quadkey for tile (z, x, y).

    The quadkey interleaves the bits of *x* and *y* from the most significant
    level down, one base-4 digit per zoom level, which is how Virtual Earth
    addresses tiles instead of separate path components. z=0 has no digits and
    yields ``""`` (Bing's single world tile).
    """
    digits = []
    for i in range(z, 0, -1):
        mask = 1 << (i - 1)
        digit = 0
        if x & mask:
            digit += 1
        if y & mask:
            digit += 2
        digits.append(str(digit))
    return "".join(digits)


def server_shard(x: int, y: int, count: int = 4) -> str:
    """Return the mirror index for a ``{s}``-sharded upstream.

    Deterministic in (x, y) so a given tile always resolves to the same URL:
    upstream caches and CDNs stay warm, and the URL builders are testable.
    Google numbers its mirrors mt0-mt3 and Bing numbers ecn.t0-t3, so the
    same 0-based index works for both.
    """
    return str((x + y) % count)


def build_tile_url(template: str, z: int, x: int, y: int) -> str:
    """Substitute the tile placeholders in *template* for tile (z, x, y).

    The single URL builder shared by the server's online cache-fill and the
    offline downloader, so a change to the token set can never apply to one
    path and not the other. ``{q}`` and ``{s}`` are computed first because
    their replacements are digit strings that contain no braces, which keeps
    the remaining ``{z}``/``{y}``/``{x}`` substitutions order-agnostic.
    """
    url = template
    if "{q}" in url:
        url = url.replace("{q}", quadkey(z, x, y))
    if "{s}" in url:
        url = url.replace("{s}", server_shard(x, y))
    return (url
            .replace("{z}", str(z))
            .replace("{y}", str(y))
            .replace("{x}", str(x)))


def get(source_id: str) -> dict | None:
    """Return a copy of the source definition for *source_id*, or None."""
    entry = TILE_SOURCES.get(source_id)
    return dict(entry) if entry is not None else None


def list_sources() -> list[dict]:
    """Return ``[{id, label, provider, style, maxzoom, attribution}, ...]``.

    Registry insertion order is preserved so the source picker's order is a
    stable UX contract.
    """
    return [
        {
            "id": sid,
            "label": s["label"],
            "provider": s["provider"],
            "style": s["style"],
            "maxzoom": s["maxzoom"],
            "attribution": s["attribution"],
        }
        for sid, s in TILE_SOURCES.items()
    ]


def list_providers() -> list[dict]:
    """Return ``[{id, label, sources: [...]}, ...]`` for the service picker."""
    return [
        {"id": pid, "label": p["label"], "sources": list(p["sources"])}
        for pid, p in PROVIDERS.items()
    ]


def provider_of(source_id: str) -> str | None:
    """Return the provider id serving *source_id*, or None if unknown."""
    entry = TILE_SOURCES.get(source_id)
    return entry["provider"] if entry is not None else None


def sources_for(provider_id: str) -> list[str]:
    """Return the source ids *provider_id* serves ([] when unknown)."""
    prov = PROVIDERS.get(provider_id)
    return list(prov["sources"]) if prov is not None else []


def resolve_source(provider_id: str, style: str | None = None) -> str | None:
    """Return the source id for *provider_id* in *style*, or its default.

    Used when switching services: the operator was looking at, say, Esri
    Satellite, so a switch to Google should land on Google Satellite rather
    than resetting to an arbitrary layer. Falls back to the provider's first
    (default) source when it does not serve the requested style, and to None
    when the provider itself is unknown.
    """
    ids = sources_for(provider_id)
    if not ids:
        return None
    if style:
        for sid in ids:
            if TILE_SOURCES[sid]["style"] == style:
                return sid
    return ids[0]
