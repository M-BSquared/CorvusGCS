"""The operator's own elevation models, served as elevation tiles.

Why this exists
---------------
The public elevation models 3D mode reads (SRTM, Copernicus) are 30 m grids.
A survey office's digital surface model is 1 m, and it has the trees, the
buildings and the power line masts in it, which is what a low flight has to
clear. An operator who has such data for the field they fly over can load it
under Settings, and 3D mode then offers it under Elevation like any other
model.

How it is served
----------------
There is no separate tile server to run. Each model is converted once, on
import, into plain float32 grids under ``<tile cache dir>/custom-dem/``, and
this process cuts Terrarium tiles out of them on demand, through the same
``/api/tiles/<id>/<z>/<x>/<y>.png`` route every other elevation source uses.
That is what keeps it offline: nothing about a custom model ever touches the
network.

Each grid keeps its own coordinate system (see :mod:`corvus.crs`), and every
tile pixel is taken from Web Mercator into it, so a model in ETRS89 / UTM or
LV95 lands where it belongs without being reprojected into a second copy.
Halved overviews are built on import, and a tile reads the one nearest its own
ground resolution, so a zoomed out view averages the ground rather than
picking single 1 m points out of it.

Outside the model
-----------------
A model covers a few square kilometres; the 3D view draws far more. Every
pixel the model does not cover is filled from the free default elevation
(``tile_sources.DEFAULT_TERRAIN``), which the caller hands in as
``base(z, x, y)``. So choosing a custom model never drops the surrounding
country to sea level. When even that is unavailable (offline, never
downloaded), the model's nearest edge is held outward instead, and the tile
is marked incomplete so it is not kept.

Lifecycle
---------
Grids are memory-mapped, never read whole, so a model larger than the RAM it
would need still serves. :meth:`CustomDemStore.close` unmaps every one; the
server calls it on shutdown, and removing a model unmaps it before its files
are deleted (Windows refuses to delete a mapped file).
"""
from __future__ import annotations

import json
import logging
import math
import mmap
import os
import re
import secrets
import shutil
import sys
import threading
import time
from array import array
from collections import OrderedDict
from collections.abc import Callable
from typing import Any

from . import crs as crs_mod
from .dem_import import DemImportError, Grid, guess_epsg, read_grid

logger = logging.getLogger("corvus.custom_dem")

TILE_SIZE = 256
ENCODING = "terrarium"
ID_PREFIX = "dem_"
_ID_RE = re.compile(r"^dem_[0-9a-f]{10}$")
# An operator name goes into the map's attribution line, which MapLibre
# renders as HTML. Markup characters are dropped rather than escaped.
_NAME_DROP = re.compile(r"[<>&\"'`\x00-\x1f]")
MAX_NAME_CHARS = 60
MAX_MODELS = 64
MAX_GRIDS_PER_MODEL = 400
# Tile pixels are mapped into a grid exactly at every LATTICE_STEP-th pixel and
# linearly in between. Over 16 pixels the projection is linear to far below a
# millimetre, and it makes a tile a few hundred projections instead of 65 536.
LATTICE_STEP = 16
# Overviews stop once a level is this small: a coarser one would never be the
# nearest to a tile, because the free default takes over at that point anyway.
MIN_OVERVIEW_SIDE = 32
RENDERED_CACHE_SIZE = 96
# The finest and coarsest zoom a model is offered at. Beyond the finest,
# MapLibre stretches the last tile itself.
MIN_MAXZOOM = 10
MAX_MAXZOOM = 18
_EARTH_PX_M = 156543.03392804097  # metres per pixel at z0, 256 px tiles, equator

BaseFn = Callable[[int, int, int], "list[float] | None"]

# Terrain or surface model. A wall or a tree edge is a jump of metres between
# two neighbouring cells, which bare ground almost never has: a 1 m terrain
# model of a built-up square kilometre has about 0.03 % of its neighbour pairs
# more than 2 m apart, a village of thirty houses in a surface model already
# 0.1 %, a town several percent. The line sits between the two. Above 5 m per
# cell a house is no longer a jump at all, so such a grid is not judged.
SURFACE_STEP_M = 2.0
SURFACE_FRACTION = 0.0007
CLASSIFY_MAX_RES_M = 5.0
CLASSIFY_PAIRS = 300_000
CLASSIFY_MIN_PAIRS = 2_000
KINDS = ("terrain", "surface", "mixed", "unknown")


def _clean_name(raw: Any, fallback: str = "") -> str:
    text = _NAME_DROP.sub("", str(raw or "")).strip()
    text = re.sub(r"\s+", " ", text)[:MAX_NAME_CHARS].strip()
    if not text:
        text = _NAME_DROP.sub("", fallback).strip()[:MAX_NAME_CHARS] or "Elevation model"
    return text


def tile_lonlat(z: int, px: float, py: float) -> tuple[float, float]:
    """Longitude and latitude of global pixel (*px*, *py*) at zoom *z*."""
    world = TILE_SIZE * (1 << z)
    lon = px / world * 360.0 - 180.0
    lat = math.degrees(math.atan(math.sinh(math.pi * (1.0 - 2.0 * py / world))))
    return lon, lat


def lonlat_pixel(z: int, lon: float, lat: float) -> tuple[float, float]:
    """Global pixel of (*lon*, *lat*) at zoom *z*."""
    world = TILE_SIZE * (1 << z)
    lat = max(-85.05112878, min(85.05112878, lat))
    s = math.sin(math.radians(lat))
    return ((lon + 180.0) / 360.0 * world,
            (0.5 - math.log((1.0 + s) / (1.0 - s)) / (4.0 * math.pi)) * world)


def _maxzoom_for(resolution_m: float, lat: float) -> int:
    ground = _EARTH_PX_M * max(0.05, math.cos(math.radians(lat)))
    if resolution_m <= 0:
        return MAX_MAXZOOM
    z = math.ceil(math.log2(ground / resolution_m))
    return max(MIN_MAXZOOM, min(MAX_MAXZOOM, z))


def _overview(values, width: int, height: int) -> tuple[array, int, int]:
    """The grid at half resolution: the mean of each 2 by 2 block's valid cells."""
    w2 = (width + 1) // 2
    h2 = (height + 1) // 2
    out = array("f", bytes(4 * w2 * h2))
    nan = math.nan
    for r2 in range(h2):
        r = 2 * r2
        top = r * width
        bottom = top + width if r + 1 < height else -1
        base = r2 * w2
        for c2 in range(w2):
            c = 2 * c2
            total = 0.0
            n = 0
            v = values[top + c]
            if v == v:
                total += v
                n += 1
            if c + 1 < width:
                v = values[top + c + 1]
                if v == v:
                    total += v
                    n += 1
            if bottom >= 0:
                v = values[bottom + c]
                if v == v:
                    total += v
                    n += 1
                if c + 1 < width:
                    v = values[bottom + c + 1]
                    if v == v:
                        total += v
                        n += 1
            out[base + c2] = total / n if n else nan
    return out, w2, h2


def classify(values, width: int, height: int, resolution_m: float) -> str:
    """``"terrain"``, ``"surface"`` or ``"unknown"`` for one grid.

    Counts neighbour pairs further apart than a wall could be on bare ground,
    over evenly spaced rows so a large grid costs no more than a small one.
    """
    if not (0 < resolution_m <= CLASSIFY_MAX_RES_M) or width < 2 or height < 2:
        return "unknown"
    limit = max(SURFACE_STEP_M, 1.5 * resolution_m)
    rows = max(1, min(height - 1, CLASSIFY_PAIRS // (2 * width)))
    stride = max(1, (height - 1) // rows)
    pairs = 0
    steps = 0
    for r in range(0, height - 1, stride):
        top = r * width
        below = top + width
        for c in range(width - 1):
            a = values[top + c]
            if a != a:
                continue
            b = values[top + c + 1]
            if b == b:
                pairs += 1
                if abs(b - a) > limit:
                    steps += 1
            d = values[below + c]
            if d == d:
                pairs += 1
                if abs(d - a) > limit:
                    steps += 1
    if pairs < CLASSIFY_MIN_PAIRS:
        return "unknown"
    return "surface" if steps / pairs >= SURFACE_FRACTION else "terrain"


def _model_kind(grids: list[dict[str, Any]]) -> str:
    """One word for a model of several grids: the kind they agree on, or mixed."""
    known = {g.get("kind") for g in grids} - {None, "unknown"}
    if not known:
        return "unknown"
    return known.pop() if len(known) == 1 else "mixed"


def _write_f32(path: str, values: array) -> None:
    data = array("f", values)
    if sys.byteorder != "little":
        data.byteswap()
    tmp = path + ".part"
    with open(tmp, "wb") as fh:
        data.tofile(fh)
    os.replace(tmp, path)


class _Level:
    """One mapped level of a grid: its size, pixel size and the open map."""

    __slots__ = ("width", "height", "dx", "dy", "fh", "map", "view")

    def __init__(self, path: str, width: int, height: int, dx: float, dy: float) -> None:
        self.width = width
        self.height = height
        self.dx = dx
        self.dy = dy
        self.fh = open(path, "rb")
        try:
            self.map = mmap.mmap(self.fh.fileno(), 0, access=mmap.ACCESS_READ)
        except Exception:
            self.fh.close()
            raise
        if len(self.map) < 4 * width * height:
            self.close()
            raise DemImportError("a stored elevation grid is shorter than its size")
        if sys.byteorder == "little":
            self.view = memoryview(self.map).cast("f")
        else:
            data = array("f")
            data.frombytes(self.map[:])
            data.byteswap()
            self.view = data

    def close(self) -> None:
        view = getattr(self, "view", None)
        if isinstance(view, memoryview):
            view.release()
        self.view = None
        try:
            self.map.close()
        except Exception:  # noqa: BLE001 - closing is best effort
            pass
        self.fh.close()


class _LoadedGrid:
    """A grid's metadata, its coordinate system and its mapped levels."""

    def __init__(self, folder: str, meta: dict[str, Any]) -> None:
        self.meta = meta
        crs = crs_mod.get(meta["epsg"])
        if crs is None:
            raise DemImportError(f"EPSG {meta['epsg']} is no longer supported")
        self.crs = crs
        self.x0 = float(meta["x0"])
        self.y0 = float(meta["y0"])
        self.bounds = tuple(meta["bounds"])
        self.resolution_m = float(meta["resolution_m"])
        self.levels: list[_Level] = []
        try:
            for level in meta["levels"]:
                self.levels.append(_Level(os.path.join(folder, level["file"]),
                                          int(level["width"]), int(level["height"]),
                                          float(level["dx"]), float(level["dy"])))
        except Exception:
            self.close()
            raise

    def close(self) -> None:
        for level in self.levels:
            try:
                level.close()
            except Exception:  # noqa: BLE001 - closing is best effort
                logger.debug("grid close failed", exc_info=True)
        self.levels = []

    def pick_level(self, ground_m: float) -> _Level:
        """The finest level not finer than half of a tile pixel's ground size."""
        index = 0
        ratio = ground_m / self.resolution_m if self.resolution_m > 0 else 1.0
        while index + 1 < len(self.levels) and ratio >= 2.0:
            ratio /= 2.0
            index += 1
        return self.levels[index]


class CustomDemStore:
    """Every custom model on disk, and the tiles cut from them.

    Thread-safe: imports, removals and tile renders arrive on different
    handler threads. ``_lock`` guards the model table and the rendered cache;
    each model's own lock is held while its maps are read, so removing a model
    waits for the tile being cut from it instead of pulling the map from
    under it.
    """

    def __init__(self, root: str) -> None:
        self.root = root
        self._lock = threading.Lock()
        self._models: dict[str, dict[str, Any]] = {}
        self._grids: dict[str, list[_LoadedGrid]] = {}
        self._model_locks: dict[str, threading.Lock] = {}
        self._rendered: OrderedDict[tuple, bytes] = OrderedDict()
        self._closed = False
        self._load_all()

    # ------------------------------------------------------------ catalogue

    def _load_all(self) -> None:
        try:
            names = sorted(os.listdir(self.root))
        except OSError:
            return
        for name in names:
            if not _ID_RE.match(name):
                continue
            folder = os.path.join(self.root, name)
            try:
                with open(os.path.join(folder, "model.json"), encoding="utf-8") as fh:
                    meta = json.load(fh)
                if meta.get("id") != name or not meta.get("grids"):
                    raise ValueError("model.json does not describe this folder")
                grids = [_LoadedGrid(folder, g) for g in meta["grids"]]
            except Exception:  # noqa: BLE001 - one broken model must not hide the rest
                logger.exception("custom elevation model %s could not be loaded", name)
                continue
            # Models imported before the kind was judged get it now, once.
            missing = [(g, lg) for g, lg in zip(meta["grids"], grids) if "kind" not in g]
            if missing:
                for g, lg in missing:
                    level = lg.levels[0]
                    g["kind"] = classify(level.view, level.width, level.height,
                                         float(g["resolution_m"]))
                self._summarise(meta)
                try:
                    self._save(meta)
                except OSError:
                    logger.warning("could not record the kind of elevation model %s", name)
            self._models[name] = meta
            self._grids[name] = grids
            self._model_locks[name] = threading.Lock()

    def has(self, model_id: str) -> bool:
        with self._lock:
            return model_id in self._models

    def list_models(self) -> list[dict[str, Any]]:
        """Every model, newest first, as the settings page shows it."""
        with self._lock:
            metas = [json.loads(json.dumps(m)) for m in self._models.values()]
        metas.sort(key=lambda m: m.get("created_at", ""), reverse=True)
        out = []
        for meta in metas:
            out.append({
                "id": meta["id"],
                "name": meta["name"],
                "created_at": meta.get("created_at", ""),
                "bounds": meta["bounds"],
                "resolution_m": meta["resolution_m"],
                "maxzoom": meta["maxzoom"],
                "min": meta["min"],
                "max": meta["max"],
                "bytes": meta.get("bytes", 0),
                "kind": meta.get("kind", "unknown"),
                "files": [{
                    "file_name": g.get("file_name", ""),
                    "epsg": g["epsg"],
                    "crs_name": g.get("crs_name", ""),
                    "width": g["levels"][0]["width"],
                    "height": g["levels"][0]["height"],
                    "resolution_m": g["resolution_m"],
                    "kind": g.get("kind", "unknown"),
                } for g in meta["grids"]],
            })
        return out

    def terrain_entries(self) -> list[dict[str, Any]]:
        """The models as elevation sources, shaped like ``tile_sources.list_terrain``."""
        return [{
            "id": m["id"],
            "label": m["name"],
            "provider": "custom",
            "encoding": ENCODING,
            "tile_size": TILE_SIZE,
            "maxzoom": m["maxzoom"],
            "sparse_above": None,
            "attribution": f"Elevation: {m['name']} (own data)",
            "custom": True,
            "kind": m["kind"],
            "bounds": m["bounds"],
        } for m in self.list_models()]

    # ------------------------------------------------------------ import

    def import_file(self, path: str, filename: str, name: str = "", epsg: int | None = None,
                    model_id: str | None = None) -> dict[str, Any]:
        """Add the grid in *path* to a model, a new one unless *model_id* is given.

        *epsg* overrides the coordinate system the file states. Raises
        :class:`DemImportError` with the reason when the file cannot be used.
        Returns the model as :meth:`list_models` describes it.
        """
        if self._closed:
            raise DemImportError("the elevation store is shut down")
        with self._lock:
            if model_id is not None and model_id not in self._models:
                raise DemImportError("that elevation model no longer exists")
            if model_id is None and len(self._models) >= MAX_MODELS:
                raise DemImportError(f"there are already {MAX_MODELS} elevation models. Remove one first")
            if model_id is not None and len(self._models[model_id]["grids"]) >= MAX_GRIDS_PER_MODEL:
                raise DemImportError(f"one model holds at most {MAX_GRIDS_PER_MODEL} files")

        grid = read_grid(path, filename)
        code = epsg or grid.epsg or guess_epsg(grid)
        if code is None:
            raise DemImportError(
                "the file does not say which coordinate system it uses. Choose it "
                "under Coordinate system and load it again (German state data is "
                "usually ETRS89 / UTM zone 32N, EPSG 25832)")
        crs = crs_mod.get(code)
        if crs is None:
            raise DemImportError(
                f"EPSG {code} is not a coordinate system Corvus knows. Supported: "
                f"{crs_mod.SUPPORTED_HINT}")
        bounds = self._lonlat_bounds(grid, crs)
        lo, hi, valid = self._stats(grid.values)
        if not valid:
            raise DemImportError("the file holds no heights, only no-data cells")
        mid_lat = (bounds[1] + bounds[3]) / 2.0
        if crs.geographic:
            resolution = grid.dx * 111320.0 * math.cos(math.radians(mid_lat))
        elif code in (3857, 900913, 102100, 102113):
            resolution = grid.dx * math.cos(math.radians(mid_lat))
        else:
            resolution = grid.dx
        resolution = max(resolution, 0.01)

        creating = model_id is None
        if creating:
            model_id = ID_PREFIX + secrets.token_hex(5)
        folder = os.path.join(self.root, model_id)
        os.makedirs(folder, exist_ok=True)
        stem = f"g{int(time.time() * 1000):x}{secrets.token_hex(2)}"
        levels = []
        written: list[str] = []
        try:
            values, width, height = grid.values, grid.width, grid.height
            dx, dy = grid.dx, grid.dy
            level = 0
            while True:
                fname = f"{stem}.{level}.f32"
                _write_f32(os.path.join(folder, fname), values)
                written.append(fname)
                levels.append({"file": fname, "width": width, "height": height,
                               "dx": dx, "dy": dy})
                if min(width, height) < 2 * MIN_OVERVIEW_SIDE:
                    break
                values, width, height = _overview(values, width, height)
                dx *= 2.0
                dy *= 2.0
                level += 1
            grid_meta = {
                "file_name": os.path.basename(str(filename or ""))[:120],
                "epsg": code,
                "crs_name": crs.name,
                "x0": grid.x0,
                "y0": grid.y0,
                "bounds": bounds,
                "resolution_m": round(resolution, 3),
                "min": lo,
                "max": hi,
                "kind": classify(grid.values, grid.width, grid.height, resolution),
                "levels": levels,
                "bytes": sum(os.path.getsize(os.path.join(folder, f)) for f in written),
            }
            loaded = _LoadedGrid(folder, grid_meta)
        except Exception:
            for fname in written:
                try:
                    os.remove(os.path.join(folder, fname))
                except OSError:
                    pass
            if creating:
                shutil.rmtree(folder, ignore_errors=True)
            raise

        with self._lock:
            if creating:
                meta = {"id": model_id, "name": _clean_name(name, os.path.splitext(
                            os.path.basename(str(filename or "")))[0]),
                        "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                        "grids": []}
                self._models[model_id] = meta
                self._grids[model_id] = []
                self._model_locks[model_id] = threading.Lock()
            meta = self._models[model_id]
            meta["grids"].append(grid_meta)
            self._grids[model_id].append(loaded)
            self._summarise(meta)
            self._forget_rendered(model_id)
            self._save(meta)
        return next(m for m in self.list_models() if m["id"] == model_id)

    @staticmethod
    def _lonlat_bounds(grid: Grid, crs: crs_mod.Crs) -> list[float]:
        x1 = grid.x0 + grid.width * grid.dx
        y1 = grid.y0 - grid.height * grid.dy
        lons: list[float] = []
        lats: list[float] = []
        steps = 16
        for k in range(steps + 1):
            fx = grid.x0 + (x1 - grid.x0) * k / steps
            fy = y1 + (grid.y0 - y1) * k / steps
            for px, py in ((fx, grid.y0), (fx, y1), (grid.x0, fy), (x1, fy)):
                try:
                    lon, lat = crs.to_lonlat(px, py)
                except (ValueError, OverflowError, ZeroDivisionError):
                    lon, lat = math.nan, math.nan
                lons.append(lon)
                lats.append(lat)
        if not all(math.isfinite(v) for v in lons + lats) \
                or min(lons) < -180.0 or max(lons) > 180.0 \
                or min(lats) < -85.0 or max(lats) > 85.0 \
                or max(lons) - min(lons) > 20.0 or max(lats) - min(lats) > 20.0:
            raise DemImportError(
                f"in {crs.name} (EPSG {crs.epsg}) these coordinates are not a place "
                "on Earth. The file is probably in another coordinate system")
        return [round(min(lons), 7), round(min(lats), 7), round(max(lons), 7), round(max(lats), 7)]

    @staticmethod
    def _stats(values) -> tuple[float, float, int]:
        lo = math.inf
        hi = -math.inf
        n = 0
        for v in values:
            if v == v:
                n += 1
                if v < lo:
                    lo = v
                if v > hi:
                    hi = v
        return (round(lo, 2), round(hi, 2), n) if n else (0.0, 0.0, 0)

    @staticmethod
    def _summarise(meta: dict[str, Any]) -> None:
        grids = meta["grids"]
        meta["bounds"] = [min(g["bounds"][0] for g in grids), min(g["bounds"][1] for g in grids),
                          max(g["bounds"][2] for g in grids), max(g["bounds"][3] for g in grids)]
        meta["resolution_m"] = min(g["resolution_m"] for g in grids)
        meta["min"] = min(g["min"] for g in grids)
        meta["max"] = max(g["max"] for g in grids)
        meta["bytes"] = sum(int(g.get("bytes", 0)) for g in grids)
        meta["kind"] = _model_kind(grids)
        mid_lat = (meta["bounds"][1] + meta["bounds"][3]) / 2.0
        meta["maxzoom"] = _maxzoom_for(meta["resolution_m"], mid_lat)

    def _save(self, meta: dict[str, Any]) -> None:
        path = os.path.join(self.root, meta["id"], "model.json")
        tmp = path + ".part"
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(meta, fh, indent=1)
        os.replace(tmp, path)

    # ------------------------------------------------------------ edits

    def rename(self, model_id: str, name: str) -> str | None:
        """Give *model_id* a new name. The name that stands, or None if unknown."""
        with self._lock:
            meta = self._models.get(model_id)
            if meta is None:
                return None
            meta["name"] = _clean_name(name, meta["name"])
            self._save(meta)
            return meta["name"]

    def remove(self, model_id: str) -> bool:
        """Delete *model_id* and its files. False when there is no such model."""
        with self._lock:
            if model_id not in self._models:
                return False
            self._models.pop(model_id)
            grids = self._grids.pop(model_id, [])
            model_lock = self._model_locks.pop(model_id, None)
            self._forget_rendered(model_id)
        if model_lock is not None:
            with model_lock:
                for g in grids:
                    g.close()
        else:
            for g in grids:
                g.close()
        shutil.rmtree(os.path.join(self.root, model_id), ignore_errors=True)
        return True

    def close(self) -> None:
        """Unmap every grid. The store answers nothing afterwards."""
        with self._lock:
            self._closed = True
            entries = [(self._model_locks.get(mid), grids) for mid, grids in self._grids.items()]
            self._models.clear()
            self._grids.clear()
            self._model_locks.clear()
            self._rendered.clear()
        for lock, grids in entries:
            if lock is not None:
                with lock:
                    for g in grids:
                        g.close()
            else:
                for g in grids:
                    g.close()

    def _forget_rendered(self, model_id: str) -> None:
        for key in [k for k in self._rendered if k[0] == model_id]:
            del self._rendered[key]

    # ------------------------------------------------------------ tiles

    def covers(self, model_id: str, z: int, x: int, y: int) -> bool:
        """Does tile (*z*, *x*, *y*) overlap the model at all?"""
        with self._lock:
            meta = self._models.get(model_id)
            if meta is None:
                return False
            w, s, e, n = meta["bounds"]
        west, north = tile_lonlat(z, x * TILE_SIZE, y * TILE_SIZE)
        east, south = tile_lonlat(z, (x + 1) * TILE_SIZE, (y + 1) * TILE_SIZE)
        return not (east < w or west > e or north < s or south > n)

    def tile(self, model_id: str, z: int, x: int, y: int,
             base: BaseFn | None = None) -> tuple[bytes, bool] | None:
        """A Terrarium PNG for the tile, and whether it is complete.

        Incomplete means the default elevation around the model was not
        available, so the model's edge was held outward instead; such a tile
        is not remembered, and the browser should ask again. None for an
        unknown model.
        """
        key = (model_id, z, x, y)
        with self._lock:
            if model_id not in self._models:
                return None
            hit = self._rendered.get(key)
            if hit is not None:
                self._rendered.move_to_end(key)
                return hit, True
            grids = list(self._grids.get(model_id, ()))
            model_lock = self._model_locks.get(model_id)
        if model_lock is None:
            return None
        base_values = None
        if base is not None:
            try:
                base_values = base(z, x, y)
            except Exception:  # noqa: BLE001 - the model alone still draws
                logger.debug("base elevation for %d/%d/%d failed", z, x, y, exc_info=True)
                base_values = None
            if base_values is not None and len(base_values) != TILE_SIZE * TILE_SIZE:
                base_values = None
        with model_lock:
            if not grids or not grids[0].levels:
                return None
            values, filled = render_tile(grids, z, x, y, base_values)
        blob = _encode(values)
        complete = base_values is not None or filled
        if complete:
            with self._lock:
                if model_id in self._models:
                    self._rendered[key] = blob
                    self._rendered.move_to_end(key)
                    while len(self._rendered) > RENDERED_CACHE_SIZE:
                        self._rendered.popitem(last=False)
        return blob, complete


def _encode(values: list[float]) -> bytes:
    from .dem_tiles import encode_heights
    return encode_heights(TILE_SIZE, values, ENCODING)


def render_tile(grids: list, z: int, x: int, y: int,
                base: list[float] | None) -> tuple[list[float], bool]:
    """Heights for every pixel of a tile, and whether the grids filled all of it.

    *grids* are ``_LoadedGrid`` objects in import order; a later one wins
    where two overlap. Pixels no grid covers take *base*; without it they
    hold the nearest grid edge.
    """
    size = TILE_SIZE
    nan = math.nan
    out = list(base) if base is not None else [nan] * (size * size)
    covered = [False] * (size * size)
    ox = x * size
    oy = y * size
    centre_lat = tile_lonlat(z, ox + size / 2.0, oy + size / 2.0)[1]
    ground = _EARTH_PX_M * max(0.05, math.cos(math.radians(centre_lat))) / (1 << z)
    steps = size // LATTICE_STEP
    for grid in grids:
        w, s, e, n = grid.bounds
        px0, py0 = lonlat_pixel(z, w, n)
        px1, py1 = lonlat_pixel(z, e, s)
        c_lo = max(0, int(math.floor(px0 - ox)) - 1)
        c_hi = min(size - 1, int(math.ceil(px1 - ox)) + 1)
        r_lo = max(0, int(math.floor(py0 - oy)) - 1)
        r_hi = min(size - 1, int(math.ceil(py1 - oy)) + 1)
        if c_lo > c_hi or r_lo > r_hi:
            continue
        level = grid.pick_level(ground)
        _sample(grid, level, z, ox, oy, steps, c_lo, c_hi, r_lo, r_hi, out, covered, False)
    filled = all(covered)
    if base is None and not filled:
        nearest = _nearest_grid(grids, z, ox, oy)
        if nearest is not None:
            level = nearest.pick_level(ground)
            _sample(nearest, level, z, ox, oy, steps, 0, size - 1, 0, size - 1,
                    out, covered, True)
        for i, v in enumerate(out):
            if v != v:
                out[i] = 0.0
    return out, filled


def _nearest_grid(grids: list, z: int, ox: int, oy: int):
    cx, cy = tile_lonlat(z, ox + TILE_SIZE / 2.0, oy + TILE_SIZE / 2.0)
    best = None
    best_d = math.inf
    for grid in grids:
        w, s, e, n = grid.bounds
        dx = max(w - cx, 0.0, cx - e)
        dy = max(s - cy, 0.0, cy - n)
        d = dx * dx + dy * dy
        if d < best_d:
            best, best_d = grid, d
    return best


def _sample(grid, level, z: int, ox: int, oy: int, steps: int,
            c_lo: int, c_hi: int, r_lo: int, r_hi: int,
            out: list[float], covered: list[bool], clamp: bool) -> None:
    """Bilinear heights from *level* into *out* for the pixel window given.

    With *clamp*, only pixels not yet covered are written, and the grid's
    edge is held outward for those outside it.
    """
    to_grid = grid.crs.from_lonlat
    x0, y0 = grid.x0, grid.y0
    dx, dy = level.dx, level.dy
    width, height = level.width, level.height
    view = level.view
    step = LATTICE_STEP
    cols: list[list[float]] = []
    rows: list[list[float]] = []
    for k in range(steps + 1):
        row_c: list[float] = []
        row_r: list[float] = []
        for m in range(steps + 1):
            lon, lat = tile_lonlat(z, ox + m * step, oy + k * step)
            try:
                gx, gy = to_grid(lon, lat)
            except (ValueError, OverflowError, ZeroDivisionError):
                gx, gy = math.nan, math.nan
            row_c.append((gx - x0) / dx - 0.5)
            row_r.append((y0 - gy) / dy - 0.5)
        cols.append(row_c)
        rows.append(row_r)
    last_c = width - 1
    last_r = height - 1
    lo_c, hi_c = -0.5, width - 0.5
    lo_r, hi_r = -0.5, height - 0.5
    size = TILE_SIZE
    inv = 1.0 / step
    for j in range(r_lo, r_hi + 1):
        v = (j + 0.5) * inv
        k = min(int(v), steps - 1)
        fv = v - k
        ca, cb = cols[k], cols[k + 1]
        ra, rb = rows[k], rows[k + 1]
        line_c = [a + (b - a) * fv for a, b in zip(ca, cb)]
        line_r = [a + (b - a) * fv for a, b in zip(ra, rb)]
        base_index = j * size
        for i in range(c_lo, c_hi + 1):
            index = base_index + i
            if clamp and covered[index]:
                continue
            u = (i + 0.5) * inv
            m = min(int(u), steps - 1)
            fu = u - m
            c = line_c[m] + (line_c[m + 1] - line_c[m]) * fu
            r = line_r[m] + (line_r[m + 1] - line_r[m]) * fu
            if not (c == c and r == r):
                continue
            if not clamp and (c < lo_c or c > hi_c or r < lo_r or r > hi_r):
                continue
            if c < 0.0:
                c = 0.0
            elif c > last_c:
                c = float(last_c)
            if r < 0.0:
                r = 0.0
            elif r > last_r:
                r = float(last_r)
            c0 = int(c)
            r0 = int(r)
            fc = c - c0
            fr = r - r0
            c1 = c0 + 1 if c0 < last_c else c0
            r1 = r0 + 1 if r0 < last_r else r0
            top = r0 * width
            bot = r1 * width
            v00 = view[top + c0]
            v01 = view[top + c1]
            v10 = view[bot + c0]
            v11 = view[bot + c1]
            if v00 == v00 and v01 == v01 and v10 == v10 and v11 == v11:
                a = v00 + (v01 - v00) * fc
                b = v10 + (v11 - v10) * fc
                out[index] = a + (b - a) * fr
                covered[index] = True
                continue
            total = 0.0
            weight = 0.0
            for val, wgt in ((v00, (1 - fc) * (1 - fr)), (v01, fc * (1 - fr)),
                             (v10, (1 - fc) * fr), (v11, fc * fr)):
                if val == val and wgt > 0.0:
                    total += val * wgt
                    weight += wgt
            if weight > 0.0:
                out[index] = total / weight
                covered[index] = True
