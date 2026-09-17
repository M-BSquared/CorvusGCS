"""OSM building footprints with heights, for the map's 3D mode.

What this is, and what it is not
--------------------------------
3D mode extrudes real buildings so the operator can judge clearance and line
of sight against the things that are actually in the way. The footprints and
heights come from OpenStreetMap via the Overpass API and are extruded by
MapLibre; they are *models of* buildings, not photogrammetry.

The photorealistic meshes in Google Earth / Google Maps 3D are a different
product: Google's Photorealistic 3D Tiles, which need a licensed Google Maps
API key and a 3D-Tiles renderer (Cesium, three.js loaders) that MapLibre GL
does not have. They cannot be rendered from the raster tile endpoints this
app already talks to. OSM extrusions are the part of "show the buildings"
that a key-less, offline-capable ground station can actually deliver, so that
is what this module serves.

Why cells and not bounding boxes
--------------------------------
Overpass answers arbitrary bounding boxes, which would make every pan a
distinct, uncacheable query. Requests are instead snapped to the slippy-map
tile grid at :data:`CELL_ZOOM` — the same (z, x, y) addressing the raster
tiles use — so a pan over ground already visited is a cache hit, the cached
unit is the same size everywhere, and a field laptop can carry the buildings
for an operating area the same way it carries its imagery.

Never on the request thread
---------------------------
A cell that is not cached is fetched by a BACKGROUND worker, and the request
that discovered the miss answers "nothing yet" immediately. This is not an
optimization, it is the difference between a working map and a blank one: an
Overpass round trip takes seconds, a browser opens about six connections to
one origin, and the same map is at that moment pulling imagery and elevation
tiles through those connections. A handler that waited on Overpass would hold
connections the map needs to draw the ground, and the operator would watch
the imagery disappear because the buildings were slow.

The frontend asks again a few seconds later, by which time the worker has
usually filled the cache.

Offline
-------
The cache is the product. A miss with no network is an empty
FeatureCollection, never an error and never a stall: 3D mode keeps working
with terrain alone. A hit is served without touching the network, whatever
its age.

stdlib only.
"""
from __future__ import annotations

import gzip
import json
import logging
import math
import os
import queue
import sqlite3
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Iterable

logger = logging.getLogger("corvus.buildings")

# The zoom whose tile grid cells the Overpass queries are snapped to. z15 is
# ~1.2 km across at the equator and ~800 m at European latitudes: small enough
# that one query is quick and a cell is worth caching on its own, large enough
# that a typical operating view is a handful of cells rather than a hundred.
CELL_ZOOM = 15

# Overpass endpoint. The main instance is a volunteer-run service with a fair
# use policy, which the cell grid plus the cache is what keeps us inside:
# a cell is fetched once and then answered from disk forever.
OVERPASS_URL = "https://overpass-api.de/api/interpreter"

# Wall-clock budget for one Overpass round trip. Generous compared to a tile
# fetch because Overpass genuinely takes seconds on a cold cell, but bounded
# so a hung endpoint cannot hold a request thread for a minute.
OVERPASS_TIMEOUT_S = 25.0
# Ceiling on one response body. A dense city cell is a few MB; anything past
# this is a query that went wrong, and parsing it would cost more memory than
# the whole tile cache.
OVERPASS_MAX_BYTES = 24 * 1024 * 1024

# How many cells may be fetched at once. Overpass rate-limits per client and a
# pan can ask for a dozen cells in one gesture, so the pool is deliberately
# narrow — the map fills in over a few seconds instead of earning a 429.
MAX_CONCURRENT_FETCHES = 2
# Cells waiting for a worker. Past this the newest requests are dropped: the
# operator has panned somewhere else and the backlog is for ground they are no
# longer looking at, which the frontend will simply ask for again if they
# return to it.
FETCH_QUEUE_DEPTH = 64

# A cached cell is served whatever its age (offline is the point). This is only
# how old it may be before an ONLINE request refreshes it in place.
CELL_TTL_S = 30 * 24 * 3600.0

# Metres per storey where a building only says how many it has. 3.2 m is the
# usual mixed residential/commercial rule of thumb; it is a modelling constant,
# not a measurement, and buildings that carry a real `height` never use it.
METRES_PER_LEVEL = 3.2
# Height for a building that names neither a height nor a storey count. Two
# storeys and a roof: low enough that a wrong guess does not invent an
# obstacle, tall enough that the footprint still reads as a building.
DEFAULT_HEIGHT_M = 8.0
# Sanity clamp on any parsed height. Below the floor an extrusion is invisible
# and the polygon is just noise; above the ceiling is a tagging error (the
# tallest building on earth is ~830 m) that would otherwise put a wall through
# the whole viewport.
MIN_HEIGHT_M = 2.0
MAX_HEIGHT_M = 900.0

# Features one cell may contribute. A z15 cell over a European city centre
# holds a few thousand buildings; this is the ceiling that keeps a cell from
# being megabytes on the laptop's disk and tens of thousands of extrusions on
# its frame budget. The map already draws terrain and imagery on that budget.
MAX_FEATURES_PER_CELL = 4000

# Decimal places kept on every coordinate. Six is about 0.1 m at these
# latitudes — finer than an OSM footprint is surveyed to, and it roughly halves
# the payload that is stored, gzipped and re-parsed by the browser.
_COORD_PRECISION = 6


# ---------------------------------------------------------------------------
# Tile-grid geometry
# ---------------------------------------------------------------------------

def cell_bounds(z: int, x: int, y: int) -> tuple[float, float, float, float]:
    """Return ``(w, s, e, n)`` degrees for slippy-map cell (z, x, y)."""
    n_tiles = 1 << z
    w = x / n_tiles * 360.0 - 180.0
    e = (x + 1) / n_tiles * 360.0 - 180.0
    north = math.degrees(math.atan(math.sinh(math.pi * (1 - 2 * y / n_tiles))))
    south = math.degrees(math.atan(math.sinh(math.pi * (1 - 2 * (y + 1) / n_tiles))))
    return (w, south, e, north)


# ---------------------------------------------------------------------------
# Tag parsing
# ---------------------------------------------------------------------------

def _parse_length(raw: Any) -> float | None:
    """Metres from an OSM length tag, or None.

    OSM heights are nominally metres and bare numbers, but the wild carries
    ``"12 m"``, ``"12,5"`` and the occasional ``"3"``-with-a-stray-unit. Only
    the leading numeric part is read; anything that is not a finite number is
    no height at all rather than a guess.
    """
    if isinstance(raw, (int, float)) and not isinstance(raw, bool):
        value = float(raw)
        return value if math.isfinite(value) else None
    if not isinstance(raw, str):
        return None
    text = raw.strip().replace(",", ".")
    number = ""
    for ch in text:
        if ch.isdigit() or (ch == "." and "." not in number) or (ch == "-" and not number):
            number += ch
        else:
            break
    try:
        value = float(number)
    except ValueError:
        return None
    return value if math.isfinite(value) else None


def building_height(tags: dict) -> tuple[float, float]:
    """Return ``(min_height, height)`` in metres for an OSM building's *tags*.

    Preference order is the accuracy order: a surveyed ``height`` beats a
    storey count, which beats the default. ``min_height`` lifts the parts of a
    building that start above the ground (a bridge between two towers, an
    overhanging upper storey) off the terrain.
    """
    tags = tags if isinstance(tags, dict) else {}
    height = _parse_length(tags.get("height"))
    if height is None:
        levels = _parse_length(tags.get("building:levels"))
        if levels is not None and levels > 0:
            # Plus a storey's worth of roof and parapet: a 2-level house is
            # taller than 2 x floor-to-floor, and clearance judged from the
            # eaves is the wrong number to judge it from.
            height = levels * METRES_PER_LEVEL + 1.0
    if height is None:
        height = DEFAULT_HEIGHT_M
    height = max(MIN_HEIGHT_M, min(MAX_HEIGHT_M, height))

    base = _parse_length(tags.get("min_height"))
    if base is None:
        min_level = _parse_length(tags.get("building:min_level"))
        base = min_level * METRES_PER_LEVEL if min_level is not None else 0.0
    if base is None or not math.isfinite(base) or base < 0:
        base = 0.0
    # A base at or above the roof would extrude nothing (or inside out).
    base = min(base, height - MIN_HEIGHT_M / 2)
    return (max(0.0, base), height)


# ---------------------------------------------------------------------------
# Overpass -> GeoJSON
# ---------------------------------------------------------------------------

def _ring(geometry: Any) -> list[list[float]] | None:
    # (see _COORD_PRECISION below for why the coordinates are rounded)
    """Closed ``[[lon, lat], ...]`` ring from an Overpass ``geometry`` list.

    Returns None for anything that cannot bound an area: Overpass clips ways
    at the query bbox, so a cell edge can hand back two points of a building
    whose rest lives in the neighbouring cell.
    """
    if not isinstance(geometry, list) or len(geometry) < 4:
        return None
    ring: list[list[float]] = []
    for node in geometry:
        if not isinstance(node, dict):
            return None
        lon, lat = node.get("lon"), node.get("lat")
        if not isinstance(lon, (int, float)) or not isinstance(lat, (int, float)):
            return None
        ring.append([round(float(lon), _COORD_PRECISION),
                     round(float(lat), _COORD_PRECISION)])
    if ring[0] != ring[-1]:
        ring.append(list(ring[0]))
    return ring if len(ring) >= 4 else None


def to_geojson(overpass: Any) -> dict:
    """Convert an Overpass ``out geom`` response into a FeatureCollection.

    Only the two properties the extrusion needs survive — ``height`` and
    ``min_height`` — because the payload is cached on a field laptop and
    every other tag is weight the renderer never reads.

    A multipolygon relation contributes one polygon per ``outer`` member.
    Courtyards (``inner`` members) are dropped rather than cut as holes: a
    filled courtyard is a small error in a rendering aid, while mis-paired
    rings produce inside-out geometry that renders as a black smear.
    """
    features: list[dict] = []
    elements = overpass.get("elements") if isinstance(overpass, dict) else None
    for element in elements or []:
        if not isinstance(element, dict):
            continue
        tags = element.get("tags")
        if not isinstance(tags, dict) or not tags.get("building"):
            continue
        base, height = building_height(tags)
        rings: list[list[list[float]]] = []
        if element.get("type") == "way":
            ring = _ring(element.get("geometry"))
            if ring is not None:
                rings.append(ring)
        elif element.get("type") == "relation":
            for member in element.get("members") or []:
                if not isinstance(member, dict) or member.get("role") != "outer":
                    continue
                ring = _ring(member.get("geometry"))
                if ring is not None:
                    rings.append(ring)
        for ring in rings:
            features.append({
                "type": "Feature",
                "geometry": {"type": "Polygon", "coordinates": [ring]},
                "properties": {
                    "height": round(height, 1),
                    "min_height": round(base, 1),
                },
            })
        if len(features) >= MAX_FEATURES_PER_CELL:
            # A dense city centre. The cut is arbitrary and it is better than
            # the alternative: the payload crosses this laptop's loopback, is
            # kept on its disk, and every polygon past it is another extrusion
            # on a frame budget that also has to draw terrain and imagery.
            logger.debug("building cell truncated at %d features", MAX_FEATURES_PER_CELL)
            break
    return {"type": "FeatureCollection", "features": features}


def overpass_query(w: float, s: float, e: float, n: float) -> str:
    """The Overpass QL asking for every building in the ``(w, s, e, n)`` box.

    ``out geom`` inlines each way's node coordinates, so one request answers
    with everything needed to build polygons — the alternative (``out body``
    plus a node lookup) is two round trips to the same rate-limited service.
    """
    bbox = f"{s:.6f},{w:.6f},{n:.6f},{e:.6f}"
    return (
        f"[out:json][timeout:{int(OVERPASS_TIMEOUT_S)}];"
        "("
        f'way["building"]({bbox});'
        f'relation["building"]["type"="multipolygon"]({bbox});'
        ");"
        "out geom;"
    )


# ---------------------------------------------------------------------------
# Cache
# ---------------------------------------------------------------------------

class BuildingCache:
    """SQLite store of gzipped GeoJSON, one row per grid cell.

    Its own file rather than a table inside a source's ``.mbtiles``: the
    MBTiles files are a documented interchange format other tools read, and a
    Corvus-specific table of building polygons does not belong in one.

    Every method is failure-tolerant by design. A corrupt or unwritable cache
    degrades to "no buildings", which is a map missing an overlay; an
    exception here would be a map that does not draw.
    """

    def __init__(self, path: str) -> None:
        self.path = path
        self._lock = threading.Lock()
        directory = os.path.dirname(os.path.abspath(path))
        if directory:
            os.makedirs(directory, exist_ok=True)
        self._conn = sqlite3.connect(path, check_same_thread=False)
        self._conn.execute("PRAGMA journal_mode=WAL")
        self._init_schema()

    def _init_schema(self) -> None:
        with self._lock, self._conn:
            self._conn.execute(
                """CREATE TABLE IF NOT EXISTS building_cells (
                       z INTEGER NOT NULL,
                       x INTEGER NOT NULL,
                       y INTEGER NOT NULL,
                       fetched_at REAL NOT NULL,
                       payload BLOB NOT NULL,
                       PRIMARY KEY (z, x, y)
                   )"""
            )

    def get(self, z: int, x: int, y: int) -> tuple[bytes, float] | None:
        """Return ``(gzipped GeoJSON, fetched_at)`` for a cell, or None."""
        try:
            with self._lock:
                row = self._conn.execute(
                    "SELECT payload, fetched_at FROM building_cells WHERE z=? AND x=? AND y=?",
                    (z, x, y),
                ).fetchone()
        except Exception:  # noqa: BLE001 - a broken cache is a miss
            logger.debug("building cache read failed", exc_info=True)
            return None
        return (bytes(row[0]), float(row[1])) if row else None

    def put(self, z: int, x: int, y: int, payload: bytes) -> None:
        """Store *payload* (already gzipped) for a cell. Best effort."""
        try:
            with self._lock, self._conn:
                self._conn.execute(
                    """INSERT OR REPLACE INTO building_cells (z, x, y, fetched_at, payload)
                       VALUES (?, ?, ?, ?, ?)""",
                    (z, x, y, time.time(), sqlite3.Binary(payload)),
                )
        except Exception:  # noqa: BLE001 - caching is best effort
            logger.debug("building cache write failed", exc_info=True)

    def stats(self) -> dict:
        """``{"cells": n, "bytes": n}`` — what the operator has offline."""
        try:
            with self._lock:
                row = self._conn.execute(
                    "SELECT COUNT(*), COALESCE(SUM(LENGTH(payload)), 0) FROM building_cells"
                ).fetchone()
        except Exception:  # noqa: BLE001
            return {"cells": 0, "bytes": 0}
        return {"cells": int(row[0]), "bytes": int(row[1])}

    def close(self) -> None:
        """Close the connection. Idempotent, and never raises."""
        try:
            with self._lock:
                self._conn.close()
        except Exception:  # noqa: BLE001 - shutdown must not raise
            logger.debug("building cache close failed", exc_info=True)


# ---------------------------------------------------------------------------
# Service
# ---------------------------------------------------------------------------

class BuildingService:
    """Answers "what buildings are in cell (z, x, y)" from the cache, and fills
    the cache in the background.

    One instance per process, held by the HTTP handler class the way the tile
    caches are. Three things it guarantees:

    * **The request thread never waits on Overpass.** :meth:`cell` answers out
      of the cache or says "pending" and returns. See the module docstring:
      a handler blocked on Overpass holds one of the browser's handful of
      connections to this origin, and the map needs those to draw the ground.
    * **Never two requests for the same cell at once.** A pan asks for the
      same cell from several connections, and every one of them would
      otherwise queue its own identical query.
    * **A hard ceiling on concurrent fetches.** Overpass is a shared volunteer
      service, and a rate-limit ban would take the feature out for every
      operator, not just the one who panned too fast.

    The workers are daemon threads over a bounded queue rather than a
    ``ThreadPoolExecutor``: that class registers an interpreter-exit hook that
    JOINS its threads, so a shutdown during a slow Overpass call would hang
    the app for the length of the timeout — against the one rule the process
    lifecycle has.
    """

    def __init__(
        self,
        cache_path: str,
        *,
        url: str = OVERPASS_URL,
        timeout: float = OVERPASS_TIMEOUT_S,
        user_agent: str = "CorvusGCS",
        workers: int = MAX_CONCURRENT_FETCHES,
    ) -> None:
        self.cache = BuildingCache(cache_path)
        self.url = url
        self.timeout = timeout
        self.user_agent = user_agent
        self._queue: "queue.Queue[tuple[int, int, int] | None]" = queue.Queue(
            maxsize=FETCH_QUEUE_DEPTH)
        self._lock = threading.Lock()
        self._queued: set[tuple[int, int, int]] = set()
        self._stop = threading.Event()
        # Set while the network is known to be unreachable, so an offline pan
        # over uncached ground stops queueing work nobody can do. Cleared by
        # the next success.
        self._offline_until = 0.0
        self._workers = [
            threading.Thread(target=self._worker, name=f"corvus-buildings-{i}", daemon=True)
            for i in range(max(1, workers))
        ]
        for worker in self._workers:
            worker.start()

    # -- public ------------------------------------------------------------

    def cell(self, z: int, x: int, y: int) -> bytes | None:
        """Gzipped GeoJSON for a cell, or None when it is not cached yet.

        None also queues the fetch. It never raises and never blocks: the
        caller is an HTTP handler on the map's render path.
        """
        cached = self.cache.get(z, x, y)
        if cached is not None:
            # Stale is still served — an old footprint of a building is still
            # a building, and offline it is all there will ever be. The
            # refresh happens behind the answer.
            if (time.time() - cached[1]) >= CELL_TTL_S:
                self._enqueue(z, x, y)
            return cached[0]
        self._enqueue(z, x, y)
        return None

    def stats(self) -> dict:
        return self.cache.stats()

    def close(self) -> None:
        """Stop the workers and close the cache. Idempotent, never raises.

        Does not join: the workers are daemons and one of them may be inside a
        socket read with seconds left on its timeout. Waiting for that is
        exactly the shutdown stall this design exists to avoid.
        """
        self._stop.set()
        for _ in self._workers:
            try:
                self._queue.put_nowait(None)
            except queue.Full:
                break
        self.cache.close()

    # -- internals ---------------------------------------------------------

    def _enqueue(self, z: int, x: int, y: int) -> None:
        """Queue a cell for fetching, unless it is already queued or offline."""
        if self._stop.is_set() or time.time() < self._offline_until:
            return
        key = (z, x, y)
        with self._lock:
            if key in self._queued:
                return
            self._queued.add(key)
        try:
            self._queue.put_nowait(key)
        except queue.Full:
            # The backlog is for ground the operator has panned away from.
            # Forget the claim so a later request can queue it again.
            with self._lock:
                self._queued.discard(key)

    def _worker(self) -> None:
        while not self._stop.is_set():
            try:
                key = self._queue.get(timeout=0.5)
            except queue.Empty:
                continue
            if key is None:
                return
            try:
                payload = self._request(*key)
                if payload is not None:
                    self.cache.put(key[0], key[1], key[2], payload)
            except Exception:  # noqa: BLE001 - a worker must outlive one bad cell
                logger.exception("building cell fetch failed: %s", key)
            finally:
                with self._lock:
                    self._queued.discard(key)

    def _request(self, z: int, x: int, y: int) -> bytes | None:
        """One Overpass round trip. None on any failure — offline is normal."""
        w, s, e, n = cell_bounds(z, x, y)
        body = urllib.parse.urlencode({"data": overpass_query(w, s, e, n)}).encode()
        request = urllib.request.Request(
            self.url,
            data=body,
            headers={
                "User-Agent": self.user_agent,
                "Content-Type": "application/x-www-form-urlencoded",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                raw = response.read(OVERPASS_MAX_BYTES + 1)
        except urllib.error.HTTPError as exc:
            # 429/504 are Overpass saying "slow down" or "that took too long",
            # not "there are no buildings" — back off rather than caching a
            # miss that would then be served for a month.
            logger.debug("overpass HTTP %s for %d/%d/%d", exc.code, z, x, y)
            self._back_off(seconds=60.0 if exc.code in (429, 504) else 0.0)
            return None
        except Exception:  # noqa: BLE001 - offline field use must never 500
            logger.debug("overpass request failed for %d/%d/%d", z, x, y, exc_info=True)
            self._back_off(seconds=30.0)
            return None
        if not raw or len(raw) > OVERPASS_MAX_BYTES:
            logger.debug("overpass response unusable for %d/%d/%d (%d bytes)",
                         z, x, y, len(raw))
            return None
        try:
            parsed = json.loads(raw.decode("utf-8", "replace"))
        except ValueError:
            logger.debug("overpass response was not JSON for %d/%d/%d", z, x, y)
            return None
        self._offline_until = 0.0
        return _gzip_json(to_geojson(parsed))

    def _back_off(self, seconds: float) -> None:
        if seconds > 0:
            self._offline_until = time.time() + seconds


def _gzip_json(obj: dict) -> bytes:
    """Serialize *obj* and gzip it.

    Gzipped in the cache AND on the wire: building polygons are long runs of
    similar coordinate digits and compress to roughly a fifth, which matters
    twice over on a field laptop — once on its disk, once on the loopback
    round trip the map makes for every cell it pans over.
    """
    data = json.dumps(obj, separators=(",", ":")).encode("utf-8")
    return gzip.compress(data, compresslevel=6)


def default_cache_path(cache_dir: str) -> str:
    """Where the building cache lives inside the tile cache directory.

    Alongside the ``.mbtiles`` files rather than in its own place: it is the
    same kind of thing (ground data downloaded for an area), it is governed by
    the same operator-configurable ``tile_cache_dir``, and one directory is
    one thing to copy onto the field laptop.
    """
    return os.path.join(cache_dir, "buildings.sqlite")


def cells_for_bounds(bounds: Iterable[float], zoom: int = CELL_ZOOM) -> list[tuple[int, int, int]]:
    """Every ``(z, x, y)`` cell covering the ``(w, s, e, n)`` *bounds*.

    Shared by whatever wants to pre-warm an area (a region download) and by
    tests; the frontend computes the same set for the current viewport.
    """
    w, s, e, n = (float(v) for v in bounds)
    if not all(math.isfinite(v) for v in (w, s, e, n)):
        return []
    s = max(s, -85.05112878)
    n = min(n, 85.05112878)
    if n < s or e < w:
        return []
    count = 1 << zoom

    def _x(lon: float) -> int:
        return max(0, min(count - 1, int((lon + 180.0) / 360.0 * count)))

    def _y(lat: float) -> int:
        rad = math.radians(lat)
        frac = (1.0 - math.log(math.tan(rad) + 1.0 / math.cos(rad)) / math.pi) / 2.0
        return max(0, min(count - 1, int(frac * count)))

    return [
        (zoom, x, y)
        for x in range(_x(w), _x(e) + 1)
        for y in range(_y(n), _y(s) + 1)
    ]
