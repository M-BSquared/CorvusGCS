"""The interactive tile path's helpers: the offline circuit breaker, the
memory of tiles an upstream does not have, the download progress bus and the
per-source downloader pool.

Moved out of corvus/server.py, which re-exports every name here.
"""
from __future__ import annotations

import collections
import logging
import threading
import time
from typing import Any

from .tile_cache import TileCache

logger = logging.getLogger("corvus.server")

# Offline circuit breaker for the interactive fill path.
#
# In the field there is no internet, so every tile the operator pans onto that
# was not pre-downloaded is a cache miss, and every miss used to spend the full
# timeout failing to reach the upstream. A viewport is dozens of tiles, so the
# map became treacle exactly when it needed to be quick. After this many
# consecutive failures the fill path stops trying and misses 404 instantly,
# which is what the map wants anyway; one success re-arms it.
TILE_UPSTREAM_FAIL_THRESHOLD = 3
# How long to stay tripped before probing the network again. Long enough that a
# genuinely offline session is not re-testing constantly, short enough that
# walking back into coverage recovers on its own without a restart.
TILE_UPSTREAM_COOLDOWN_S = 30.0


class _UpstreamBreaker:
    """Circuit breaker around the interactive tile cache-fill.

    Field use is offline, so a pan onto un-downloaded ground produces a whole
    viewport of cache misses at once. Each miss that tries the network costs
    the full timeout, and the map stops responding precisely when the operator
    is looking for something. After ``threshold`` consecutive failures the
    breaker trips and misses fail instantly for ``cooldown`` seconds; the first
    request after that is allowed through as a probe, and any success closes
    the breaker again — so walking back into coverage recovers by itself.

    Thread-safe: tiles are served from many handler threads at once, which is
    the whole reason the failures arrive in a burst.
    """

    def __init__(self, threshold: int = TILE_UPSTREAM_FAIL_THRESHOLD,
                 cooldown: float = TILE_UPSTREAM_COOLDOWN_S) -> None:
        self._threshold = max(1, int(threshold))
        self._cooldown = float(cooldown)
        self._lock = threading.Lock()
        self._failures = 0
        self._open_until = 0.0

    def allow(self) -> bool:
        """True if a network attempt should be made now.

        When the cooldown has elapsed this returns True exactly once per
        cooldown window (a probe) unless the attempt succeeds — that way a
        still-offline session does not go back to paying the timeout on every
        tile the moment the window expires.
        """
        with self._lock:
            if self._failures < self._threshold:
                return True
            if time.monotonic() >= self._open_until:
                # Probe: re-arm the window now so only this one request gets
                # through until it reports back.
                self._open_until = time.monotonic() + self._cooldown
                return True
            return False

    def record_success(self) -> None:
        with self._lock:
            self._failures = 0
            self._open_until = 0.0

    def record_failure(self) -> None:
        with self._lock:
            self._failures += 1
            if self._failures >= self._threshold:
                self._open_until = time.monotonic() + self._cooldown

    def is_open(self) -> bool:
        """True while the breaker is suppressing network attempts."""
        with self._lock:
            return self._failures >= self._threshold and time.monotonic() < self._open_until

    def reset(self) -> None:
        with self._lock:
            self._failures = 0
            self._open_until = 0.0


class _AbsentTiles:
    """Tiles a regional elevation source said it does not have (HTTP 404).

    Above its ``sparse_above`` zoom a source like Copernicus/Mapterhorn only
    has tiles where a national model exists, and a tile it lacks means its
    children are lacking too. Remembering the 404s turns the next few hundred
    requests over the same ground into an immediate answer instead of a round
    trip each. Memory only and bounded: a restart, or the cap, just asks again.
    """

    def __init__(self, limit: int = 20000) -> None:
        self._limit = max(1, int(limit))
        self._lock = threading.Lock()
        self._keys: set[tuple[str, int, int, int]] = set()

    def add(self, source: str, z: int, x: int, y: int) -> None:
        with self._lock:
            if len(self._keys) >= self._limit:
                self._keys.clear()
            self._keys.add((source, z, x, y))

    def covers(self, source: str, z: int, x: int, y: int, floor: int) -> bool:
        """Is (*z*, *x*, *y*), or an ancestor finer than *floor*, known absent?"""
        with self._lock:
            for level in range(z, floor, -1):
                shift = z - level
                if (source, level, x >> shift, y >> shift) in self._keys:
                    return True
        return False


class _TileProgressBus:
    """Pub-sub fan-out so many SSE clients can watch one download job.

    The downloader accepts a single ``on_progress`` callback per job (set at
    ``start()`` time); SSE clients come and go. The bus is the one callback the
    downloader calls; it reads ``job_id`` from the progress dict and fans the
    dict out to every registered subscriber for that job.

    Contract handed to the downloader: it invokes ``on_progress(progress)``
    with a dict containing at least ``{job_id, state, done, total, failed}``
    — the same shape its own ``status()`` returns. Without ``job_id`` the bus
    cannot route, so the call is dropped.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._subs: dict[str, list[Any]] = collections.defaultdict(list)

    def subscribe(self, job_id: str, fn: Any) -> None:
        with self._lock:
            self._subs[job_id].append(fn)

    def unsubscribe(self, job_id: str, fn: Any) -> None:
        with self._lock:
            try:
                self._subs[job_id].remove(fn)
            except ValueError:
                pass
            if not self._subs[job_id]:
                self._subs.pop(job_id, None)

    def make_on_progress(self) -> Any:
        """Return the single ``on_progress`` callback to hand to the downloader."""
        def _on_progress(progress: Any) -> None:
            if not isinstance(progress, dict):
                return
            job_id = progress.get("job_id")
            if not job_id:
                return
            with self._lock:
                subs = list(self._subs.get(job_id, ()))
            for fn in subs:
                try:
                    fn(progress)
                except Exception:  # noqa: BLE001 - a bad subscriber must not kill the download
                    logger.exception("tile progress subscriber raised")
        return _on_progress


class _TileDownloaderPool:
    """Unified facade over one TileDownloader per source.

    ``TileDownloader`` binds a single ``TileCache`` at construction (see its
    contract); we keep one cache per source so ``/api/tiles/<source>/...``
    serves the correct raster, which requires one downloader per source. The
    pool exposes the single ``start``/``cancel``/``status``/``list_jobs``/
    ``shutdown`` surface the HTTP layer wants and routes by source (``start``)
    or ``job_id`` (``cancel``/``status``).
    """

    def __init__(
        self,
        caches: dict[str, TileCache],
        downloader_cls: Any,
        max_workers: int = 3,
    ) -> None:
        self._downloaders: dict[str, Any] = {
            sid: downloader_cls(cache, max_workers=max_workers)
            for sid, cache in caches.items()
        }

    def start(
        self,
        source: str,
        upstream: str,
        bounds: tuple,
        minzoom: int,
        maxzoom: int,
        on_progress: Any = None,
        token: str = "",
    ) -> str:
        dl = self._downloaders.get(source)
        if dl is None:
            raise ValueError(f"no downloader for source {source!r}")
        return dl.start(source, upstream, bounds, minzoom, maxzoom,
                        on_progress=on_progress, token=token)

    def cancel(self, job_id: str) -> bool:
        for dl in self._downloaders.values():
            if dl.cancel(job_id):
                return True
        return False

    def status(self, job_id: str) -> dict | None:
        for dl in self._downloaders.values():
            st = dl.status(job_id)
            if st is not None:
                return st
        return None

    def list_jobs(self) -> list[dict]:
        jobs: list[dict] = []
        for dl in self._downloaders.values():
            jobs.extend(dl.list_jobs())
        return jobs

    def shutdown(self) -> None:
        for dl in self._downloaders.values():
            try:
                dl.shutdown()
            except Exception:  # noqa: BLE001 - shutdown must not raise
                logger.exception("tile downloader shutdown failed")
        self._downloaders.clear()
