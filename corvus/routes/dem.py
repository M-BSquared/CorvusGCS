"""The operator's own elevation models: list, import, rename, remove, serve.

See corvus/custom_dem.py. The tiles go through the shared
``/api/tiles/<id>/<z>/<x>/<y>.png`` route (``_api_tiles_serve`` hands a custom
id to :meth:`DemRoutes._serve_custom_dem`), so MapLibre reads a custom model
exactly as it reads SRTM.
"""
from __future__ import annotations

import logging
import os
import tempfile
from typing import Any
from urllib.parse import parse_qs, urlparse

from .. import dem_tiles, tile_sources
from ..dem_import import DemImportError
from ..http_routes import route

logger = logging.getLogger("corvus.server")

# One upload is one file of a model. A 10 km square of 1 m XYZ text is about
# 2.8 GB, which is no longer a file to load through a settings page; a GeoTIFF
# of the same ground is a few hundred MB.
MAX_DEM_BODY_BYTES = 1024 * 1024 * 1024
_UPLOAD_CHUNK = 1024 * 1024


class DemRoutes:
    custom_dem: Any = None

    @route("GET", "/api/dem")
    def _api_dem_list(self) -> None:
        """Every custom elevation model with its files; always 200."""
        store = self.custom_dem
        self._send_json({"models": store.list_models() if store is not None else [],
                         "available": store is not None})

    @route("POST", "/api/dem/rename")
    def _api_dem_rename(self, payload: dict) -> None:
        store = self.custom_dem
        model_id = payload.get("id")
        name = payload.get("name")
        if store is None:
            self._send_json({"ok": False, "error": "elevation models are unavailable"}, 503)
            return
        if not isinstance(model_id, str) or not isinstance(name, str) or not name.strip():
            self._send_json({"ok": False, "error": "id and name required"}, 400)
            return
        stands = store.rename(model_id, name)
        if stands is None:
            self._send_json({"ok": False, "error": "unknown elevation model"}, 404)
            return
        self._send_json({"ok": True, "name": stands})

    @route("POST", "/api/dem/remove")
    def _api_dem_remove(self, payload: dict) -> None:
        store = self.custom_dem
        model_id = payload.get("id")
        if store is None:
            self._send_json({"ok": False, "error": "elevation models are unavailable"}, 503)
            return
        if not isinstance(model_id, str) or not store.remove(model_id):
            self._send_json({"ok": False, "error": "unknown elevation model"}, 404)
            return
        self._send_json({"ok": True})

    def _api_dem_import_raw(self) -> None:
        """Add one uploaded file to an elevation model.

        Raw ``application/octet-stream`` body, like the firmware upload, so a
        file of hundreds of MB is never base64'd into JSON. It is streamed to a
        temporary file next to the models, never held in memory, and the
        temporary file is gone whatever the outcome.

        Query: ``filename`` (display and format hint only, never opened),
        ``name`` (a new model's name), ``crs`` (EPSG code, overriding the
        file's own) and ``model`` (add to this model instead of a new one).
        """
        store = self.custom_dem
        if store is None:
            self._send_json({"ok": False, "error": "elevation models are unavailable"}, 503)
            return
        try:
            length = int(self.headers.get("Content-Length", 0))
        except (TypeError, ValueError):
            self._send_json({"ok": False, "error": "invalid content-length"}, 400)
            return
        if length <= 0:
            self._send_json({"ok": False, "error": "the file is empty"}, 400)
            return
        if length > MAX_DEM_BODY_BYTES:
            self.close_connection = True
            self._send_json({"ok": False, "error": "that file is larger than 1 GB. Split it "
                                                   "into smaller tiles"}, 413)
            return
        params = parse_qs(urlparse(self.path).query)

        def param(key: str) -> str:
            return (params.get(key, [""])[0] or "").strip()

        filename = os.path.basename(param("filename").replace("\\", "/"))[:200]
        epsg = None
        raw_crs = param("crs").upper().replace("EPSG:", "").strip()
        if raw_crs and raw_crs != "AUTO":
            try:
                epsg = int(raw_crs)
            except ValueError:
                self.close_connection = True
                self._send_json({"ok": False, "error": "the coordinate system must be an "
                                                       "EPSG number, such as 25832"}, 400)
                return
        model_id = param("model") or None

        os.makedirs(store.root, exist_ok=True)
        fd, tmp = tempfile.mkstemp(prefix="upload-", suffix=".part", dir=store.root)
        try:
            remaining = length
            with os.fdopen(fd, "wb") as fh:
                while remaining > 0:
                    chunk = self.rfile.read(min(_UPLOAD_CHUNK, remaining))
                    if not chunk:
                        break
                    fh.write(chunk)
                    remaining -= len(chunk)
            if remaining:
                self.close_connection = True
                self._send_json({"ok": False, "error": "the upload was cut short"}, 400)
                return
            try:
                model = store.import_file(tmp, filename, param("name"), epsg, model_id)
            except DemImportError as exc:
                self._send_json({"ok": False, "error": str(exc)}, 400)
                return
            except MemoryError:
                self._send_json({"ok": False, "error": "not enough memory for that file. "
                                                       "Split it into smaller tiles"}, 400)
                return
            except Exception:  # noqa: BLE001 - a bad file must not 500 the app
                logger.exception("elevation import failed for %s", filename)
                self._send_json({"ok": False, "error": "could not read this file as an "
                                                       "elevation grid"}, 400)
                return
        finally:
            try:
                os.remove(tmp)
            except OSError:
                pass
        logger.info("elevation model %s: added %s", model["id"], filename or "a file")
        self._send_json({"ok": True, "model": model})

    # ---- tiles ----

    def _serve_custom_dem(self, model_id: str, z: int, x: int, y: int) -> None:
        """A tile of a custom model, with the default elevation around it."""
        store = self.custom_dem
        if z < 0 or z > 22 or x < 0 or y < 0 or x >= (1 << z) or y >= (1 << z):
            self._send_json({"error": "tile out of range"}, 404)
            return
        blob = None
        complete = True
        if store.covers(model_id, z, x, y):
            result = store.tile(model_id, z, x, y, self._default_terrain_heights)
            if result is not None:
                blob, complete = result
        else:
            blob = self._default_terrain_at(z, x, y)
            if blob is None:
                # Never a hole: MapLibre draws a missing elevation tile at sea
                # level, and beside a model at 550 m that is a cliff the
                # camera falls into. The model's edge, held outward, instead.
                result = store.tile(model_id, z, x, y, None)
                if result is not None:
                    blob, complete = result[0], False
        if blob is None:
            self._send_json({"error": "tile not available"}, 404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "image/png")
        self.send_header("Content-Length", str(len(blob)))
        self._send_cors()
        self.send_header("Cache-Control", "max-age=86400" if complete else "max-age=300")
        self.end_headers()
        self.wfile.write(blob)

    def _default_terrain_at(self, z: int, x: int, y: int) -> bytes | None:
        """The free default elevation for any tile, past its own last zoom too."""
        spec = tile_sources.get(tile_sources.DEFAULT_TERRAIN)
        if spec is None:
            return None
        top = int(spec["maxzoom"])
        if z <= top:
            return self._default_terrain_png(z, x, y)
        dz = z - top
        ancestor = self._default_terrain_png(top, x >> dz, y >> dz)
        if ancestor is None:
            return None
        mask = (1 << dz) - 1
        return dem_tiles.overzoom(ancestor, dz, x & mask, y & mask)

    def _default_terrain_heights(self, z: int, x: int, y: int) -> list[float] | None:
        """Metres of the free default elevation for a tile, 256 a side, or None."""
        spec = tile_sources.get(tile_sources.DEFAULT_TERRAIN)
        if spec is None:
            return None
        top = int(spec["maxzoom"])
        dz = max(0, z - top)
        png = self._default_terrain_png(z - dz, x >> dz, y >> dz)
        if png is None:
            return None
        mask = (1 << dz) - 1
        return dem_tiles.heights(png, spec["encoding"], dz, x & mask, y & mask, 256)
