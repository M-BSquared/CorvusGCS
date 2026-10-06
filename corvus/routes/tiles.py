"""Map tiles: serving, the cache, offline regions and their downloads.

Route handlers moved out of corvus/server.py. CorvusHandler inherits
TileRoutes, and the @route registry wires them exactly as before.
"""
from __future__ import annotations

import logging
from typing import Any
from urllib.parse import parse_qs, urlparse
from .. import tile_sessions, tile_sources
from ..config import MAX_MAP_TOKEN_CHARS, _MAP_TOKEN_CHARS
from ..http_input import _clean_region_name, _default_region_name, _validate_tile_bounds, _validate_tile_zooms
from ..http_routes import _config_write_lock, route
from ..tile_http import _delete_region_tiles

logger = logging.getLogger("corvus.server")


class TileRoutes:
    def _map_token(self, provider_id: str) -> str:
        """The stored API key for *provider_id*, or "" when none is set.

        The only reader of the map service credentials. They never leave this
        process: the frontend asks this server for a tile, this substitutes the
        key into the upstream URL, and the browser is handed the image bytes —
        so an injected script in the page has nothing to steal, and a key does
        not end up in a browser cache, a history entry or a referrer.
        """
        if not provider_id:
            return ""
        tokens = self._live_config().map_tokens
        if not isinstance(tokens, dict):
            return ""
        value = tokens.get(provider_id)
        return value if isinstance(value, str) else ""

    def _map_tokens_set(self) -> set[str]:
        """Provider ids with a key stored — never the keys themselves."""
        tokens = self._live_config().map_tokens
        if not isinstance(tokens, dict):
            return set()
        return {pid for pid, value in tokens.items() if isinstance(value, str) and value}

    # ---- Tile cache / download / serve ----
    @route("GET", "/api/tiles/sources")
    def _api_tiles_sources(self) -> None:
        """List every source with its source cap and live cache stats; always 200.

        ``maxzoom`` is the SOURCE cap (the highest zoom the upstream serves,
        e.g. 19) — a constant capability, never the cache contents. ``minzoom``
        is the source floor (every registered source serves a single z0 world
        tile). The cache stats are reported under distinct ``cached_*`` keys so
        they can never clobber the source cap: the prior duplicate ``maxzoom``
        key returned the cache max (or null when empty), hiding the real 19
        cap from the UI and forcing a hardcoded ``SOURCE_ZOOM_CAP`` workaround.
        """
        caches = self.tile_caches or {}
        sources = []
        for entry in tile_sources.list_sources():
            sid = entry["id"]
            cache = caches.get(sid)
            stats = cache.stats() if cache is not None else {
                "count": 0, "minzoom": None, "maxzoom": None,
            }
            sources.append({
                "id": sid,
                "label": entry["label"],
                # Provider grouping so the UI can offer "which map service"
                # as one choice and then filter the layer/download pickers to
                # that service. Mirrors corvus/tile_sources.PROVIDERS.
                "provider": entry["provider"],
                "style": entry["style"],
                # The legally-required credit string. Served here so the
                # frontend can put it on its MapLibre raster source instead of
                # hand-mirroring the registry — that mirror was the one place
                # a new source could silently ship without attribution.
                "attribution": entry["attribution"],
                "minzoom": 0,
                "maxzoom": entry["maxzoom"],
                "cached_count": stats["count"],
                "cached_minzoom": stats["minzoom"],
                "cached_maxzoom": stats["maxzoom"],
            })
        # Elevation sources travel in their own key, never mixed into
        # `sources`: everything that reads that list means "a base layer the
        # operator can pick", and a DEM in the layer switcher would paint the
        # map in false colour. The frontend builds its raster-dem source from
        # this, encoding included, so the height packing is stated once.
        # A keyed DEM says whether its key is set, as the map services below
        # do, so the frontend picks it only when it can actually load.
        keys_set = self._map_tokens_set()
        terrain = []
        for entry in tile_sources.list_terrain():
            cache = caches.get(entry["id"])
            stats = cache.stats() if cache is not None else {
                "count": 0, "minzoom": None, "maxzoom": None,
            }
            terrain.append({
                **entry,
                "minzoom": 0,
                "cached_count": stats["count"],
                "cached_minzoom": stats["minzoom"],
                "cached_maxzoom": stats["maxzoom"],
                "token_required": tile_sources.needs_token(entry["id"]),
                "token_set": entry["provider"] in keys_set,
            })
        buildings = (self.buildings.stats()
                     if self.buildings is not None else {"cells": 0, "bytes": 0})
        # A keyed service reports whether the operator has given it a key —
        # never the key. ``token_set`` is the whole of what the UI needs: the
        # tiles are proxied by this process, so the browser has no use for the
        # credential itself. A service that needs one and has not got one is
        # still listed, with the reason on it, rather than hidden: a layer that
        # vanished would leave the operator with nothing to act on. A service
        # whose key is optional (Google, Bing) draws either way; its key only
        # moves it onto the licensed API.
        have = keys_set
        providers = []
        for prov in tile_sources.list_providers():
            keyed = prov.get("token") is not None
            providers.append({
                **prov,
                "token_required": tile_sources.token_required(prov["id"]),
                "token_set": keyed and prov["id"] in have,
            })
        from ..tile_downloader import MAX_TILES_PER_JOB
        self._send_json({
            "sources": sources,
            "providers": providers,
            "default_provider": tile_sources.DEFAULT_PROVIDER,
            "terrain": terrain,
            "default_terrain": tile_sources.DEFAULT_TERRAIN,
            "preferred_terrain": tile_sources.PREFERRED_TERRAIN,
            "buildings": buildings,
            # The per-job cap POST /api/tiles/download enforces, so the dialog
            # can refuse an area before the operator presses Download rather
            # than after.
            "max_tiles_per_job": MAX_TILES_PER_JOB,
        })

    @route("POST", "/api/tiles/token")
    def _api_tiles_token(self, payload: dict) -> None:
        """Store (or clear) one map service's API key.

        Its own endpoint rather than a key in ``POST /api/config``, for the
        same reason the NTRIP password and the SSH passwords are handled
        apart: this is a credential. It goes in one direction only — in through
        here, out only as a substitution into an upstream URL made inside this
        process. ``GET /api/config`` does not carry it back, and neither does
        anything else; the UI learns from ``token_set`` that one is stored.

        An empty ``token`` removes the key, which is how the dialog's REMOVE
        works. Removing one that was never there is a success, so a stale UI
        cannot report an error for the state the operator already wanted.
        """
        provider = payload.get("provider")
        if not isinstance(provider, str) or not provider:
            self._send_json({"ok": False, "error": "provider must be a non-empty string"}, 400)
            return
        if tile_sources.token_meta(provider) is None:
            # Refused rather than stored: an id that is not a keyed service is
            # either a typo or an attempt to use the config file as scratch
            # space for something that is never read back.
            self._send_json(
                {"ok": False, "error": f"{provider!r} is not a keyed map service"}, 400)
            return
        raw = payload.get("token", "")
        if not isinstance(raw, str):
            self._send_json({"ok": False, "error": "token must be a string"}, 400)
            return
        token = raw.strip()

        cfg = self._live_config()
        tokens = dict(cfg.map_tokens) if isinstance(cfg.map_tokens, dict) else {}
        if token:
            if len(token) > MAX_MAP_TOKEN_CHARS:
                self._send_json(
                    {"ok": False,
                     "error": f"that key is longer than {MAX_MAP_TOKEN_CHARS} characters"},
                    400)
                return
            if not set(token) <= _MAP_TOKEN_CHARS:
                # Named rather than silently cleaned: the overwhelmingly likely
                # cause is a paste that took the surrounding quotes or the
                # "key=" prefix with it, and an operator told what is wrong
                # fixes it in one go.
                self._send_json(
                    {"ok": False,
                     "error": "that does not look like an API key. Use letters, digits, "
                              ". _ ~ - only, with no spaces or quotes"},
                    400)
                return
            tokens[provider] = token
        else:
            tokens.pop(provider, None)

        with _config_write_lock:
            cfg.map_tokens = tokens or None
            try:
                self._save_live_config()
            except Exception:  # noqa: BLE001 - never 500 over a settings write
                logger.exception("could not persist the map service key")
                self._send_json(
                    {"ok": False,
                     "error": "the key is set for this session but could not be saved"},
                    500)
                return
        # Logged as a fact, never with the value: an operator reading the log
        # should be able to see that a key was set without the log becoming a
        # place the key lives.
        logger.info("map service key for %s %s", provider,
                    "stored" if token else "removed")
        self._send_json({"ok": True, "provider": provider, "token_set": bool(token)})

    @route("GET", "/api/tiles/jobs")
    def _api_tiles_jobs(self) -> None:
        """List all download jobs across every source."""
        if self.tile_downloader is None:
            self._send_json({"jobs": []})
            return
        self._send_json({"jobs": self.tile_downloader.list_jobs()})

    @route("POST", "/api/tiles/download")
    def _api_tiles_download(self, payload: dict) -> None:
        """Start a tile download job for a source within bounds/zooms."""
        if self.tile_downloader is None:
            self._send_json({"ok": False, "error": "tile service unavailable"}, 503)
            return
        source = payload.get("source")
        if not isinstance(source, str) or tile_sources.get(source) is None:
            self._send_json({"ok": False, "error": "unknown source"}, 400)
            return
        src = tile_sources.get(source)
        err, bounds = _validate_tile_bounds(payload.get("bounds"))
        if err is not None:
            self._send_json({"ok": False, "error": err}, 400)
            return
        zoom_err = _validate_tile_zooms(payload.get("minzoom"), payload.get("maxzoom"), src["maxzoom"])
        if zoom_err is not None:
            self._send_json({"ok": False, "error": zoom_err}, 400)
            return
        minzoom = int(payload["minzoom"])
        maxzoom = int(payload["maxzoom"])
        # The operator's name for this area. Optional: an unnamed download is
        # still recorded, under a generated name, so every cached area is
        # accounted for in the region list rather than silently invisible.
        name = _clean_region_name(payload.get("name")) or _default_region_name(bounds)

        # A keyed service with no key downloads nothing but 401s, and does it
        # for however many thousand tiles the region covers. Refused up front
        # with the one thing the operator can act on, rather than reported as
        # a job that failed every tile.
        token = self._map_token(str(src.get("provider") or ""))
        if tile_sources.needs_token(source) and not token:
            self._send_json({
                "ok": False,
                "error": "this map service needs an API key. Add one under "
                         "Settings, Map service",
            }, 400)
            return
        # A licensed API hands out its template once, here, and the job keeps
        # it: a session outlives any download, and a refused key is reported
        # before a single tile is asked for.
        template = src["upstream"]
        if token and tile_sources.licensed(source) is not None:
            try:
                template = tile_sessions.resolve(source, token)
            except tile_sessions.TileSessionError as exc:
                self._send_json({"ok": False, "error": str(exc)}, 502)
                return

        cache = (self.tile_caches or {}).get(source)
        on_progress = self._make_tile_progress(cache)
        try:
            job_id = self.tile_downloader.start(
                source, template, bounds, minzoom, maxzoom,
                on_progress=on_progress, token=token,
            )
        except Exception as exc:  # noqa: BLE001 - a download submit must never 500
            logger.exception("tile download start failed")
            self._send_json({"ok": False, "error": f"start failed: {exc}"}, 500)
            return
        status = self.tile_downloader.status(job_id) if job_id else None
        if status and status.get("state") == "failed":
            self._send_json(
                {"ok": False, "error": status.get("error", "download failed")}, 409
            )
            return
        # Record the region as soon as the job exists, keyed by the job id, so
        # the area shows on the map while it is still downloading. The progress
        # callback patches the final tile count and state when the job ends.
        self._record_region(cache, job_id, name, source, bounds, minzoom, maxzoom)
        # Elevation rides along when asked for, as its own job on its own
        # cache. Without it a pre-downloaded area is flat the moment the
        # laptop leaves the network — imagery cached for the field and terrain
        # that is not is exactly the split that makes 3D mode useless there.
        terrain_job = None
        chosen_job = None
        if payload.get("terrain") and not tile_sources.is_terrain(source):
            terrain_job = self._start_terrain_companion(bounds, minzoom, maxzoom, name)
            # The free elevation always comes along: it is what every other
            # DEM falls back to. The one 3D is actually drawing from comes
            # too, when that is a different one, so the field sees the same
            # ground the office did.
            chosen = payload.get("terrain_source")
            if (isinstance(chosen, str) and chosen != tile_sources.DEFAULT_TERRAIN
                    and tile_sources.is_terrain(chosen)):
                chosen_job = self._start_terrain_companion(
                    bounds, minzoom, maxzoom, name, chosen)
        # Buildings for 3D, queued behind whatever the map is showing and
        # fetched in the background. Not a tile job: Overpass is a shared
        # service with a fair use policy, and a building block is one query,
        # not a URL template the downloader could walk.
        building_blocks = 0
        if payload.get("buildings") and self.buildings is not None:
            try:
                building_blocks = int(self.buildings.prefetch(bounds))
            except Exception:  # noqa: BLE001 - the imagery job is already running
                logger.exception("building prefetch failed to start")
        self._send_json({
            "job_id": job_id, "name": name, "terrain_job_id": terrain_job,
            "terrain_source_job_id": chosen_job, "building_blocks": building_blocks,
        })

    def _start_terrain_companion(
        self, bounds: tuple, minzoom: int, maxzoom: int, name: str,
        terrain_id: str | None = None,
    ) -> str | None:
        """Start the elevation half of a region download. None if it cannot run.

        *terrain_id* is the DEM to download, the free default when omitted.
        Best effort on purpose: the imagery job has already been accepted and
        the operator has been shown progress for it, so a DEM that cannot be
        started must not turn their download into an error. It also caps the
        zooms at the DEM's own — elevation is a smooth surface, and asking for
        z19 heights would quadruple the job for tiles the upstream does not
        even serve — and, for a DEM that only has its finest levels in some
        regions, at the last level it has everywhere.
        """
        default_terrain = terrain_id or tile_sources.DEFAULT_TERRAIN
        src = tile_sources.get(default_terrain)
        if src is None or self.tile_downloader is None or not tile_sources.is_terrain(default_terrain):
            return None
        token = self._map_token(str(src.get("provider") or ""))
        if tile_sources.needs_token(default_terrain) and not token:
            return None
        hi = min(maxzoom, src["maxzoom"], src.get("sparse_above", src["maxzoom"]))
        # From zoom 0, whatever the imagery starts at. A pitched view draws
        # its far ground from coarse tiles, and every missing fine tile is
        # derived from a coarser ancestor (corvus/dem_tiles.py): an area whose
        # elevation starts at z14 has neither, and its terrain drops to sea
        # level at the first tile that is not on disk. Over one area the
        # levels below the imagery's add a tile or two each. Only an area
        # already near the per-job cap gives some of them back, so the
        # elevation job is never refused where it used to be accepted.
        from ..tile_downloader import MAX_TILES_PER_JOB, tile_count
        lo, floor = 0, min(minzoom, hi)
        while lo < floor and tile_count(bounds, lo, hi) > MAX_TILES_PER_JOB:
            lo += 1
        cache = (self.tile_caches or {}).get(default_terrain)
        try:
            job_id = self.tile_downloader.start(
                default_terrain, src["upstream"], bounds, lo, hi,
                on_progress=self._make_tile_progress(cache), token=token,
            )
        except Exception:  # noqa: BLE001 - the imagery job is already running
            logger.exception("terrain companion download start failed")
            return None
        if job_id:
            self._record_region(cache, job_id, name, default_terrain, bounds, lo, hi)
        return job_id

    def _record_region(self, cache: Any, job_id: str, name: str, source: str,
                       bounds: tuple, minzoom: int, maxzoom: int) -> None:
        """Write the region row for a download job that is already running.

        The job has to exist first, because its id is the row's key, so a fast
        job can end before this row does: an area already on disk is walked
        in milliseconds. Its final update then found no row to patch, and the
        row written after it said "downloading" until the next launch settled
        it as cancelled. So the job is read back once the row is in, and one
        that has already ended is settled here. Never raises: bookkeeping must
        not fail a download the operator has been shown.
        """
        if cache is None:
            return
        w, s, e, n = bounds
        try:
            cache.add_region({
                "id": job_id, "name": name, "source": source,
                "w": w, "s": s, "e": e, "n": n,
                "minzoom": minzoom, "maxzoom": maxzoom,
                "tile_count": 0, "state": "running",
            })
            status = (self.tile_downloader.status(job_id)
                      if self.tile_downloader is not None else None)
            state = status.get("state") if isinstance(status, dict) else None
            if state in self._SSE_TERMINAL_STATES:
                cache.update_region(job_id, tile_count=status.get("done", 0), state=state)
        except Exception:  # noqa: BLE001 - bookkeeping must not fail the download
            logger.exception("region record write failed")

    def _make_tile_progress(self, cache: Any) -> Any:
        """Compose the progress callback handed to the downloader.

        Two jobs on one callback because the downloader accepts exactly one:
        fan the update out to the SSE subscribers (the bus), and keep the
        region record in step with the job. The bus half runs first and is
        never skipped, so a bookkeeping failure cannot cost the operator their
        live progress. Returns None when there is nothing to do.
        """
        bus_fn = (self.tile_progress_bus.make_on_progress()
                  if self.tile_progress_bus is not None else None)
        if cache is None:
            return bus_fn

        # The terminal update must be applied once. The downloader fires
        # progress from several worker threads, so guard the transition with a
        # flag rather than relying on the last call winning.
        finished: dict[str, bool] = {"done": False}

        def _on_progress(progress: Any) -> None:
            if bus_fn is not None:
                bus_fn(progress)
            if not isinstance(progress, dict):
                return
            state = progress.get("state")
            if state not in ("done", "cancelled", "failed") or finished["done"]:
                return
            finished["done"] = True
            # Elevation derived from what was on disk before this job is
            # stale now that finer tiles may have landed under it.
            source = progress.get("source")
            if self.dem_fallback is not None and tile_sources.is_terrain(source):
                self.dem_fallback.forget(source)
            try:
                # `done` counts tiles now present in the cache — fetched plus
                # the ones that were already there — so it is exactly how much
                # of the region is available offline.
                cache.update_region(
                    progress.get("job_id", ""),
                    tile_count=progress.get("done", 0),
                    state=state,
                )
            except Exception:  # noqa: BLE001 - never kill the download
                logger.exception("region record update failed")

        return _on_progress

    @route("GET", "/api/tiles/regions")
    def _api_tiles_regions(self) -> None:
        """List every named, pre-downloaded area across all sources.

        Always 200: a source whose cache cannot be read contributes nothing
        rather than failing the whole list, because the map draws these and a
        500 here would blank every region the operator does have.
        """
        regions: list[dict] = []
        for sid, cache in (self.tile_caches or {}).items():
            try:
                for region in cache.list_regions():
                    region["source"] = region.get("source") or sid
                    regions.append(region)
            except Exception:  # noqa: BLE001
                logger.exception("region list failed for source %s", sid)
        regions.sort(key=lambda r: r.get("created_at", ""), reverse=True)
        self._send_json({"regions": regions})

    @route("POST", "/api/tiles/regions/rename")
    def _api_tiles_regions_rename(self, payload: dict) -> None:
        """Rename a stored region.

        Areas downloaded without a name get a coordinate-derived one, which is
        exact but not memorable; this is how "47.350°N 8.550°E" becomes
        "Landing site".
        """
        source = payload.get("source")
        region_id = payload.get("id")
        name = _clean_region_name(payload.get("name"))
        if not isinstance(region_id, str) or not region_id:
            self._send_json({"ok": False, "error": "id required"}, 400)
            return
        if not name:
            self._send_json({"ok": False, "error": "name required"}, 400)
            return
        cache = (self.tile_caches or {}).get(source) if isinstance(source, str) else None
        if cache is None:
            self._send_json({"ok": False, "error": "unknown source"}, 400)
            return
        if not cache.update_region(region_id, name=name):
            self._send_json({"ok": False, "error": "unknown region"}, 404)
            return
        self._send_json({"ok": True, "name": name})

    @route("POST", "/api/tiles/regions/remove")
    def _api_tiles_regions_remove(self, payload: dict) -> None:
        """Forget a named region, optionally deleting its cached tiles.

        ``delete_tiles`` defaults to False: regions overlap, so dropping the
        tiles of one can silently punch holes in another. The caller asks for
        it explicitly, and only the tiles no OTHER region still covers are
        removed.
        """
        source = payload.get("source")
        region_id = payload.get("id")
        if not isinstance(region_id, str) or not region_id:
            self._send_json({"ok": False, "error": "id required"}, 400)
            return
        cache = (self.tile_caches or {}).get(source) if isinstance(source, str) else None
        if cache is None:
            self._send_json({"ok": False, "error": "unknown source"}, 400)
            return
        region = cache.get_region(region_id)
        if region is None:
            self._send_json({"ok": False, "error": "unknown region"}, 404)
            return

        removed_tiles = 0
        if payload.get("delete_tiles") is True:
            try:
                removed_tiles = _delete_region_tiles(cache, region)
            except Exception as exc:  # noqa: BLE001 - deletion must never 500
                logger.exception("region tile deletion failed")
                self._send_json({"ok": False, "error": f"tile deletion failed: {exc}"}, 500)
                return
            # Elevation derived from the tiles just deleted must not outlive them.
            if removed_tiles and self.dem_fallback is not None and tile_sources.is_terrain(source):
                self.dem_fallback.forget(source)
        cache.remove_region(region_id)
        self._send_json({"ok": True, "removed_tiles": removed_tiles})

    @route("POST", "/api/tiles/cancel")
    def _api_tiles_cancel(self, payload: dict) -> None:
        """Cancel a running download job by id."""
        if self.tile_downloader is None:
            self._send_json({"ok": False, "error": "tile service unavailable"}, 503)
            return
        job_id = payload.get("id")
        if not isinstance(job_id, str) or not job_id:
            self._send_json({"ok": False, "error": "id required"}, 400)
            return
        if self.tile_downloader.cancel(job_id):
            self._send_json({"ok": True})
        else:
            self._send_json({"ok": False, "error": "unknown job"}, 404)

    @route("GET", "/api/tiles/progress")
    def _sse_tiles_progress(self) -> None:
        """SSE stream of one download job's progress (latest-wins, coalesced)."""
        query = urlparse(self.path).query
        params = parse_qs(query)
        job_id = params.get("id", [None])[0]
        if not job_id:
            self._send_json({"error": "missing id"}, 400)
            return
        if self.tile_downloader is None:
            self._send_json({"error": "tile service unavailable"}, 503)
            return
        status = self.tile_downloader.status(job_id)
        if status is None:
            self._send_json({"error": "unknown job"}, 404)
            return
        # The validation above is what this endpoint adds over the bare topic:
        # a missing, unknown or unservable job is a 4xx/5xx answer, and a
        # stream cannot express one once its headers have gone out. The
        # multiplexed form has no equivalent — there a bad job id simply never
        # fires, which is the right failure for one topic among several.
        del status  # re-read by the topic binder, which owns the subscription
        self._serve_sse_topics(
            {"tiles": "progress"}, job_id=job_id, close_on=("tiles",),
        )
