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

Terrain (elevation) sources
---------------------------
``TERRAIN_SOURCES`` is a THIRD, deliberately separate registry: RGB-encoded
digital elevation model tiles, which are data rather than imagery. They are
kept out of ``TILE_SOURCES`` because everything that iterates that registry
means "a base layer the operator can look at" — the layer switcher, the
Appearance picker, the provider partition assertion below. A DEM is never a
base layer; it is what 3D mode reads heights from. They still flow through the
same ``/api/tiles/<id>/<z>/<x>/<y>.png`` route and the same per-id MBTiles
cache, which is what makes 3D terrain work offline in the field: use
:func:`all_sources` wherever a lookup means "anything servable" and
``TILE_SOURCES`` wherever it means "a base layer".

``encoding`` names the height packing so the frontend can hand it to
MapLibre's ``raster-dem`` source unchanged. ``terrarium`` is Mapzen's
(height = R * 256 + G + B / 256 - 32768 metres).

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
``{k}``
    The operator's API key for a keyed service (see *Keyed services* below).
    Percent-encoded at substitution time, so a key pasted with a stray
    character cannot alter the rest of the URL.

Keyed services
--------------
A provider carrying a ``token`` block serves nothing without an API key the
operator supplies. That key is a credential: it is stored server-side in
``~/.corvus/config.json`` (0600), it is substituted into the upstream URL
inside this process, and it never reaches the browser — the frontend only ever
asks this server for ``/api/tiles/<id>/<z>/<x>/<y>.png``, which is the same
route every other source uses. ``GET /api/config`` does not carry it either;
the UI learns only whether one is set.

The ``token`` block says what the operator needs to know to get a key and
nothing about the key itself:

``label``
    What the service calls it, so the field in the dialog matches the page the
    operator copied it from ("API key", "access token").
``signup``
    Where to get one. Printed as text, not linked: the desktop build runs in
    QtWebEngine, where an external link goes nowhere (same reason the credits
    dialog prints its URLs).
``help``
    One line on what the key buys.

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

The MapTiler and Mapbox entries are the licensed answer to that: both publish
plain XYZ raster endpoints, both are used here exactly as their terms
describe, and both are keyed — which is why they appear only once the operator
has put a key in. Higher-resolution imagery and a rate limit that belongs to
the operator rather than to a shared public endpoint is what the key buys.

stdlib only.
"""
from __future__ import annotations

import re
import urllib.parse
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

    # ---- MapTiler (keyed; see "Keyed services" in the module docstring) ----
    # 256-px raster endpoints rather than the vector styles: everything
    # downstream of here — the MBTiles cache, the offline downloader, the
    # region maths — is built on 256-px raster XYZ tiles, and a vector source
    # would be a second pipeline rather than a second provider.
    "maptiler_satellite": {
        "label": "Satellite",
        "provider": "maptiler",
        "style": "satellite",
        "upstream": "https://api.maptiler.com/tiles/satellite-v2/{z}/{x}/{y}.jpg?key={k}",
        "maxzoom": 20,
        "attribution": "© MapTiler, © OpenStreetMap contributors",
    },
    "maptiler_streets": {
        "label": "Streets",
        "provider": "maptiler",
        "style": "streets",
        "upstream": "https://api.maptiler.com/maps/streets-v2/256/{z}/{x}/{y}.png?key={k}",
        "maxzoom": 20,
        "attribution": "© MapTiler, © OpenStreetMap contributors",
    },
    "maptiler_hybrid": {
        "label": "Hybrid",
        "provider": "maptiler",
        "style": "hybrid",
        "upstream": "https://api.maptiler.com/maps/hybrid/256/{z}/{x}/{y}.jpg?key={k}",
        "maxzoom": 20,
        "attribution": "© MapTiler, © OpenStreetMap contributors",
    },
    "maptiler_topo": {
        "label": "Topographic",
        "provider": "maptiler",
        "style": "topo",
        "upstream": "https://api.maptiler.com/maps/topo-v2/256/{z}/{x}/{y}.png?key={k}",
        "maxzoom": 20,
        "attribution": "© MapTiler, © OpenStreetMap contributors",
    },

    # ---- Mapbox (keyed; see "Keyed services" in the module docstring) ----
    # The raster tile APIs, 256 px, for the same reason MapTiler's are.
    "mapbox_satellite": {
        "label": "Satellite",
        "provider": "mapbox",
        "style": "satellite",
        "upstream": "https://api.mapbox.com/v4/mapbox.satellite/{z}/{x}/{y}.jpg90?access_token={k}",
        "maxzoom": 20,
        "attribution": "© Mapbox, © Maxar",
    },
    "mapbox_streets": {
        "label": "Streets",
        "provider": "mapbox",
        "style": "streets",
        "upstream": "https://api.mapbox.com/styles/v1/mapbox/streets-v12/tiles/256/{z}/{x}/{y}?access_token={k}",
        "maxzoom": 20,
        "attribution": "© Mapbox, © OpenStreetMap contributors",
    },
    "mapbox_hybrid": {
        "label": "Hybrid",
        "provider": "mapbox",
        "style": "hybrid",
        "upstream": "https://api.mapbox.com/styles/v1/mapbox/satellite-streets-v12/tiles/256/{z}/{x}/{y}?access_token={k}",
        "maxzoom": 20,
        "attribution": "© Mapbox, © Maxar, © OpenStreetMap contributors",
    },
    "mapbox_topo": {
        "label": "Outdoors",
        "provider": "mapbox",
        "style": "topo",
        "upstream": "https://api.mapbox.com/styles/v1/mapbox/outdoors-v12/tiles/256/{z}/{x}/{y}?access_token={k}",
        "maxzoom": 20,
        "attribution": "© Mapbox, © OpenStreetMap contributors",
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

# Elevation tiles. One entry today, but a registry rather than a constant so a
# second DEM (a national high-resolution model, an operator's own tile server)
# is a line here and nothing else.
#
# AWS Terrain Tiles is the elevation set the Mapzen project left behind: a
# global merge of SRTM, the USGS National Elevation Dataset, and national
# models, served from S3 with no key and no rate limit. maxzoom 15 is the
# grid's own limit (~4 m/px at the equator); the underlying data is coarser
# than that almost everywhere, which is why 3D terrain still reads correctly
# when only z12 tiles were cached for a region.
TERRAIN_SOURCES: dict[str, dict[str, Any]] = {
    "terrain": {
        "label": "Elevation",
        "provider": "terrain",
        "style": "dem",
        "encoding": "terrarium",
        "upstream": "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png",
        "maxzoom": 15,
        "attribution": "Elevation: AWS Terrain Tiles: SRTM, USGS NED, and national datasets",
    },
}

# The DEM 3D mode reads from when the operator has not chosen otherwise.
DEFAULT_TERRAIN = "terrain"

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
    # Keyed services. ``token`` is what makes a provider keyed; its presence is
    # the only test anything performs (see :func:`token_meta`), so adding a
    # third keyed service is a block here and nothing else.
    "maptiler": {
        "label": "MapTiler",
        "sources": [
            "maptiler_satellite", "maptiler_streets",
            "maptiler_hybrid", "maptiler_topo",
        ],
        "token": {
            "label": "MapTiler API key",
            "signup": "https://cloud.maptiler.com/account/keys/",
            "help": "Licensed satellite and street tiles, on the operator's "
                    "own quota rather than a shared public endpoint.",
        },
    },
    "mapbox": {
        "label": "Mapbox",
        "sources": [
            "mapbox_satellite", "mapbox_streets",
            "mapbox_hybrid", "mapbox_topo",
        ],
        "token": {
            "label": "Mapbox access token",
            "signup": "https://account.mapbox.com/access-tokens/",
            "help": "Maxar satellite imagery and Mapbox street cartography. "
                    "A public (pk.*) token is what this needs.",
        },
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
# A DEM id that shadowed a base id would give the two one shared MBTiles cache
# and serve elevation bytes as imagery (or the reverse). They share a route and
# a cache directory, so the ids have to stay disjoint.
assert not (set(TERRAIN_SOURCES) & set(TILE_SOURCES)), \
    "a terrain source id must not shadow a base layer id"
assert DEFAULT_TERRAIN in TERRAIN_SOURCES, "DEFAULT_TERRAIN must name a real DEM"


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


def build_tile_url(template: str, z: int, x: int, y: int, token: str = "") -> str:
    """Substitute the tile placeholders in *template* for tile (z, x, y).

    The single URL builder shared by the server's online cache-fill and the
    offline downloader, so a change to the placeholder set can never apply to
    one path and not the other. ``{q}`` and ``{s}`` are computed first because
    their replacements are digit strings that contain no braces, which keeps
    the remaining ``{z}``/``{y}``/``{x}`` substitutions order-agnostic.

    ``{k}`` is the operator's API key, and it is the one substitution whose
    value did not come from this file. It is percent-encoded with an empty
    safe set, so a key pasted with a stray ``&`` or ``#`` — or with something
    worse in it — becomes one opaque query value rather than extra URL
    structure. It is substituted LAST, after the placeholders whose values this
    module computes, so an encoded key can never contain a brace that the
    earlier passes would then have interpreted.
    """
    url = template
    if "{q}" in url:
        url = url.replace("{q}", quadkey(z, x, y))
    if "{s}" in url:
        url = url.replace("{s}", server_shard(x, y))
    url = (url
           .replace("{z}", str(z))
           .replace("{y}", str(y))
           .replace("{x}", str(x)))
    if "{k}" in url:
        url = url.replace("{k}", urllib.parse.quote(str(token or ""), safe=""))
    return url


# The query parameter names a key travels under, derived from the templates
# rather than listed by hand: a keyed service added above brings its own
# parameter name with it, and :func:`redact_url` must not need a second edit to
# keep that service's key out of the logs.
_TOKEN_QUERY_PARAMS: frozenset[str] = frozenset(
    match.group(1)
    for entry in TILE_SOURCES.values()
    for match in re.finditer(r"[?&]([A-Za-z0-9_.\-]+)=\{k\}", entry["upstream"])
)


def redact_url(url: str) -> str:
    """*url* with any API key blanked, for a log line.

    Every upstream failure is logged with the URL that failed, which is the
    only way to debug a tile that will not load — and for a keyed service that
    URL carries the operator's credential. Logs are copied into bug reports;
    this is what keeps the key out of them.
    """
    text = str(url or "")
    for param in _TOKEN_QUERY_PARAMS:
        marker = param + "="
        start = text.find(marker)
        while start != -1:
            head = start + len(marker)
            end = len(text)
            for sep in ("&", "#"):
                hit = text.find(sep, head)
                if hit != -1:
                    end = min(end, hit)
            text = text[:head] + "REDACTED" + text[end:]
            start = text.find(marker, head + len("REDACTED"))
    return text


def token_meta(provider_id: str) -> dict[str, str] | None:
    """The ``token`` block for *provider_id*, or None when it needs no key.

    Carrying a block IS being keyed — there is no separate flag to keep in
    step with it.
    """
    prov = PROVIDERS.get(provider_id)
    if prov is None:
        return None
    meta = prov.get("token")
    return dict(meta) if isinstance(meta, dict) else None


def keyed_providers() -> list[str]:
    """Provider ids that need an operator-supplied key, in registry order."""
    return [pid for pid in PROVIDERS if token_meta(pid) is not None]


def needs_token(source_id: str) -> bool:
    """Does serving *source_id* require a key the operator has to supply?

    Answered from the provider rather than from the template so a source whose
    URL happens not to carry ``{k}`` still counts as keyed if its service is —
    the question callers are really asking is "will this 401 without a key".
    """
    provider = provider_of(source_id)
    return provider is not None and token_meta(provider) is not None


def all_sources() -> dict[str, dict[str, Any]]:
    """Every servable source: the base layers plus the elevation tiles.

    The "anything with an upstream URL and a cache" view, used by the parts
    that do not care what a tile *means* — cache construction, the serve
    route, the downloader pool. Anything that means "a base layer the operator
    can look at" must read ``TILE_SOURCES`` instead, or a DEM ends up in the
    layer switcher.
    """
    return {**TILE_SOURCES, **TERRAIN_SOURCES}


def get(source_id: str) -> dict | None:
    """Return a copy of the source definition for *source_id*, or None.

    Resolves base layers and elevation sources alike: this is what the tile
    route and the downloader look up, and both serve either kind.
    """
    entry = TILE_SOURCES.get(source_id) or TERRAIN_SOURCES.get(source_id)
    return dict(entry) if entry is not None else None


def is_terrain(source_id: str) -> bool:
    """True when *source_id* names an elevation source rather than a layer."""
    return source_id in TERRAIN_SOURCES


def list_terrain() -> list[dict]:
    """Return ``[{id, label, encoding, maxzoom, attribution}, ...]``.

    Served next to :func:`list_sources` so the frontend can build its
    ``raster-dem`` source — including the height packing — without a
    hand-mirrored copy of this registry, exactly as it does for base layers.
    """
    return [
        {
            "id": sid,
            "label": s["label"],
            "encoding": s["encoding"],
            "maxzoom": s["maxzoom"],
            "attribution": s["attribution"],
        }
        for sid, s in TERRAIN_SOURCES.items()
    ]


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
    """Return ``[{id, label, sources, token}, ...]`` for the service picker.

    ``token`` is the metadata block for a keyed service or None — never the
    key. Whether one is actually stored is the server's answer to give, since
    only it can see the config file.
    """
    return [
        {
            "id": pid,
            "label": p["label"],
            "sources": list(p["sources"]),
            "token": token_meta(pid),
        }
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

# A keyed provider that forgot the placeholder would fetch its tiles without
# the key and get a wall of 401s that looks exactly like "no internet"; an
# unkeyed one that carried it would build a URL with an empty key in it. The
# template and the provider block have to agree, both ways.
assert all(
    ("{k}" in TILE_SOURCES[sid]["upstream"]) == (token_meta(pid) is not None)
    for pid, prov in PROVIDERS.items()
    for sid in prov["sources"]
), "a {k} placeholder and a provider token block must imply each other"
