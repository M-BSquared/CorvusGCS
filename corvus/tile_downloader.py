"""Rate-limited, resumable, cancellable XYZ tile downloader for offline caching.

Field use: the operator pre-downloads a map region at home (online), then flies
offline. This worker enumerates the slippy-map (XYZ) tile set for a WGS84 bounds
x zoom range, skips tiles already in the :class:`TileCache` (so an interrupted
job resumes from where it stopped), and fetches the rest with a small thread
pool. stdlib only.

The downloader owns no sockets itself — every fetch goes through
``urllib.request.urlopen`` and the blob is handed to ``TileCache.put_tile``,
which converts XYZ→TMS internally. ``shutdown`` is bounded (it never blocks
forever): it signals every job to stop, cancels pending fetches, and joins the
orchestrator threads with a short timeout. In-flight ``urlopen`` calls are
bounded by their own 15 s timeout, so a clean process exit drains within that
window even without ``SIGKILL``.
"""
from __future__ import annotations

import math
import threading
import time
import urllib.request
import uuid
from concurrent.futures import ThreadPoolExecutor
from collections.abc import Callable, Iterator

from corvus import tile_sources
from corvus.tile_cache import TileCache
from corvus.version import get_version

# A single job never exceeds this many tiles; bigger regions must be split.
MAX_TILES_PER_JOB = 50_000
_FETCH_TIMEOUT = 15.0      # seconds per upstream tile request
# Largest tile body kept. The same bound the interactive cache-fill uses
# (server.TILE_UPSTREAM_MAX_BYTES): anything bigger is an error page or a
# server that has stopped being a tile server, not a map tile.
_MAX_TILE_BYTES = 4 * 1024 * 1024
_SUBMIT_PAUSE = 0.05       # seconds between task submissions — be respectful
# Fetched tiles are written to the cache in batches of this many, in one
# transaction each (TileCache.put_tiles). A region job is capped at 50 000
# tiles; writing them one commit at a time made the SQLite write, not the
# network, the dominant cost of a download on a field laptop's disk.
_WRITE_BATCH = 256
# ...but a batch also waits no longer than this before going out, so a small
# job, a slow link and the tail of a big job all still report progress while
# they are happening. Progress is only counted once a batch has landed, which
# is what keeps "downloaded" meaning "on disk".
_WRITE_BATCH_SECONDS = 1.0


def new_job(
    job_id: str,
    source: str,
    total: int,
    ranges: list | None = None,
    template: str = "",
    on_progress: Callable[[dict], None] | None = None,
    token: str = "",
) -> dict:
    """Build a download job.

    One definition of the shape, because there was more than one: the tests
    hand-rolled their own literal, and it fell behind the moment the worker
    started buffering writes (``_pending``) — a ``KeyError`` in a test that
    was about URL templates and had no opinion about any of this. A job is
    a plain dict rather than a class because the snapshot the HTTP layer
    serializes is a subset of these very keys; the underscore-prefixed ones
    are internal and never leave the module.
    """
    return {
        "job_id": job_id,
        "source": source,
        "state": "running",
        "done": 0,
        "total": total,
        "failed": 0,
        "error": None,
        "_cancel": threading.Event(),
        "_ranges": ranges if ranges is not None else [],
        "_template": template,
        # The API key for a keyed service, or "". Underscore-prefixed like the
        # rest of the internals, and deliberately absent from ``_snapshot`` —
        # the job's public state is serialized straight to the browser.
        "_token": token,
        "_on_progress": on_progress,
        "_lock": threading.Lock(),
        "_executor": None,
        "_thread": None,
        # Tiles fetched but not yet committed, and when the oldest of them was
        # added. Both are guarded by "_lock".
        "_pending": [],
        "_pending_since": 0.0,
    }


def lon_to_x(lon: float, z: int) -> int:
    """Slippy-map column for a longitude at zoom *z* (XYZ convention)."""
    n = 1 << z
    return int(math.floor((lon + 180.0) / 360.0 * n))


def lat_to_y(lat: float, z: int) -> int:
    """Slippy-map row for a latitude at zoom *z* (XYZ, north == row 0).

    Web Mercator is undefined at the poles; clamp to its valid span so a
    near-polar latitude does not blow up ``tan``/``sec``.
    """
    lat = max(min(lat, 85.0511), -85.0511)
    lat_rad = math.radians(lat)
    n = 1 << z
    return int(math.floor(
        (1.0 - math.log(math.tan(lat_rad) + 1.0 / math.cos(lat_rad)) / math.pi) / 2.0 * n
    ))


def _ranges_for_bounds(
    bounds: tuple, minzoom: int, maxzoom: int
) -> tuple[list[tuple[int, int, int, int, int]], int]:
    """Return per-zoom (z, x_min, x_max, y_min, y_max) ranges and total count.

    x/y are clamped to ``[0, 2^z - 1]``. A bounds crossing the antimeridian
    (w > e) is not fully modeled — the two edge columns are simply swapped so
    the range stays non-empty. Field use is in Europe, so this is a degenerate
    case rather than a normal one.
    """
    w, s, e, n = bounds
    ranges: list[tuple[int, int, int, int, int]] = []
    total = 0
    for z in range(minzoom, maxzoom + 1):
        n_tiles = 1 << z
        x_min = lon_to_x(w, z)
        x_max = lon_to_x(e, z)
        if x_min > x_max:
            x_min, x_max = x_max, x_min
        x_min = max(0, min(x_min, n_tiles - 1))
        x_max = max(0, min(x_max, n_tiles - 1))
        # North (n) maps to the smaller row; south (s) to the larger.
        y_min = lat_to_y(n, z)
        y_max = lat_to_y(s, z)
        if y_min > y_max:
            y_min, y_max = y_max, y_min
        y_min = max(0, min(y_min, n_tiles - 1))
        y_max = max(0, min(y_max, n_tiles - 1))
        count = (x_max - x_min + 1) * (y_max - y_min + 1)
        ranges.append((z, x_min, x_max, y_min, y_max))
        total += count
    return ranges, total


def tile_count(bounds: tuple, minzoom: int, maxzoom: int) -> int:
    """Total number of XYZ tiles covering *bounds* across the zoom range."""
    _, total = _ranges_for_bounds(bounds, minzoom, maxzoom)
    return total


def enumerate_tiles(
    bounds: tuple, minzoom: int, maxzoom: int
) -> Iterator[tuple[int, int, int]]:
    """Yield every ``(z, x, y)`` XYZ tile covering *bounds* x zoom range."""
    ranges, _ = _ranges_for_bounds(bounds, minzoom, maxzoom)
    for z, x_min, x_max, y_min, y_max in ranges:
        for x in range(x_min, x_max + 1):
            for y in range(y_min, y_max + 1):
                yield z, x, y


def tile_ranges(
    bounds: tuple, minzoom: int, maxzoom: int
) -> list[tuple[int, int, int, int, int]]:
    """Per-zoom ``(z, x_min, x_max, y_min, y_max)`` XYZ ranges covering *bounds*.

    The tile set :func:`enumerate_tiles` yields, as rectangles rather than
    tiles, so a caller can ask the cache how much of an area it holds without
    materializing the area.
    """
    ranges, _ = _ranges_for_bounds(bounds, minzoom, maxzoom)
    return ranges


def tile_cover(
    bounds: tuple, minzoom: int, maxzoom: int
) -> Callable[[tuple[int, int, int]], bool]:
    """A predicate: is ``(z, x, y)`` one of the tiles :func:`enumerate_tiles` yields?

    The same tile set, answered from the per-zoom ranges instead of being
    materialized — a membership test costs a dict lookup whatever the size of
    the area.
    """
    ranges, _ = _ranges_for_bounds(bounds, minzoom, maxzoom)
    by_zoom = {z: (x_min, x_max, y_min, y_max) for z, x_min, x_max, y_min, y_max in ranges}

    def covers(tile: tuple[int, int, int]) -> bool:
        z, x, y = tile
        span = by_zoom.get(z)
        return span is not None and span[0] <= x <= span[1] and span[2] <= y <= span[3]

    return covers


class TileDownloader:
    """Resumable, cancellable, rate-limited tile fetcher.

    One orchestrator background thread per job drives a small
    :class:`ThreadPoolExecutor` that performs the actual HTTP fetches. Tiles
    already present in *cache* are skipped, so restarting a job with the same
    bounds resumes from where it stopped.
    """

    def __init__(self, cache: TileCache, max_workers: int = 3) -> None:
        self._cache = cache
        self._max_workers = max(1, int(max_workers))
        self._jobs: dict[str, dict] = {}
        self._jobs_lock = threading.Lock()
        self._stop_event = threading.Event()  # global shutdown flag
        self._threads: list[threading.Thread] = []
        self._shutdown = False

    # ---- public API ----

    def start(
        self,
        source: str,
        upstream_url_template: str,
        bounds: tuple,
        minzoom: int,
        maxzoom: int,
        on_progress: Callable[[dict], None] | None = None,
        token: str = "",
    ) -> str:
        """Schedule a download job. Returns the job id (uuid4 hex).

        Bounds is ``(w, s, e, n)`` in WGS84 degrees. *upstream_url_template*
        uses ``{z}/{x}/{y}`` (slippy order). *token* is the operator's API key
        for a keyed service, substituted into ``{k}`` per request and kept off
        every public snapshot. A job whose tile set exceeds
        :data:`MAX_TILES_PER_JOB`, is empty, or cannot be enumerated is recorded
        in state ``"failed"`` and its id returned (never raises) so the HTTP
        endpoint can surface the error.
        """
        job_id = uuid.uuid4().hex
        try:
            ranges, total = _ranges_for_bounds(bounds, minzoom, maxzoom)
        except Exception as exc:  # enumeration failure -> failed job, not a raise
            self._record_failed(job_id, source, 0, f"enumeration error: {exc}")
            return job_id
        if total == 0:
            self._record_failed(job_id, source, 0, "no tiles in range")
            return job_id
        if total > MAX_TILES_PER_JOB:
            self._record_failed(
                job_id, source, total,
                f"too many tiles ({total}); per-job cap is {MAX_TILES_PER_JOB}",
            )
            return job_id

        job = new_job(
            job_id, source, total, ranges, upstream_url_template, on_progress,
            token,
        )
        thread = threading.Thread(
            target=self._run, args=(job,),
            name=f"tile-dl-{job_id[:8]}", daemon=True,
        )
        job["_thread"] = thread
        with self._jobs_lock:
            self._jobs[job_id] = job
            self._threads.append(thread)
        thread.start()
        return job_id

    def cancel(self, job_id: str) -> bool:
        """Signal a running job to stop. Returns True if the job is known."""
        with self._jobs_lock:
            job = self._jobs.get(job_id)
        if job is None:
            return False
        cancel = job.get("_cancel")
        if cancel is not None:
            cancel.set()
        return True

    def status(self, job_id: str) -> dict | None:
        """Return a snapshot of the job's public state, or None if unknown."""
        with self._jobs_lock:
            job = self._jobs.get(job_id)
        return self._snapshot(job) if job is not None else None

    def list_jobs(self) -> list[dict]:
        """Return snapshots of every job (running, done, cancelled, failed)."""
        with self._jobs_lock:
            jobs = list(self._jobs.values())
        return [self._snapshot(j) for j in jobs]

    def shutdown(self) -> None:
        """Cancel every job, drop pending fetches, and join orchestrator threads.

        Idempotent. Never blocks forever: in-flight ``urlopen`` calls are left
        to drain within their own 15 s timeout; orchestrator threads are joined
        with a 2 s timeout and are daemon-threaded as a backstop.
        """
        if self._shutdown:
            return
        self._shutdown = True
        self._stop_event.set()
        with self._jobs_lock:
            jobs = list(self._jobs.values())
        for job in jobs:
            cancel = job.get("_cancel")
            if cancel is not None:
                cancel.set()
            executor = job.get("_executor")
            if executor is not None:
                try:
                    executor.shutdown(wait=False, cancel_futures=True)
                except Exception:
                    pass  # best-effort; never raise in the shutdown path
        for thread in list(self._threads):
            try:
                thread.join(timeout=2.0)
            except Exception:
                pass

    # ---- internals ----

    def _record_failed(self, job_id: str, source: str, total: int, error: str) -> None:
        job = {
            "job_id": job_id, "source": source, "state": "failed",
            "done": 0, "total": total, "failed": 0, "error": error,
        }
        with self._jobs_lock:
            self._jobs[job_id] = job

    def _snapshot(self, job: dict) -> dict:
        # CPython's GIL makes each int/str field read atomic; a progress
        # snapshot may be momentarily stale, which is fine for the UI.
        return {
            "job_id": job["job_id"],
            "source": job.get("source"),
            "state": job["state"],
            "done": job["done"],
            "total": job["total"],
            "failed": job["failed"],
            "error": job.get("error"),
        }

    def _fire_progress(self, job: dict, on_progress: Callable[[dict], None] | None) -> None:
        if on_progress is None:
            return
        try:
            on_progress(self._snapshot(job))
        except Exception:
            pass  # a bad progress sink must not kill the worker

    def _download_one(self, job: dict, z: int, x: int, y: int) -> None:
        cancel = job["_cancel"]
        if cancel.is_set() or self._stop_event.is_set():
            return
        url = tile_sources.build_tile_url(
            job["_template"], z, x, y, job.get("_token", ""))
        # Identified like the interactive cache-fill: OSM's tile policy blocks
        # a library's default User-Agent, and a bulk download is exactly the
        # traffic a tile operator wants to be able to attribute.
        req = urllib.request.Request(
            url, headers={"User-Agent": f"CorvusGCS/{get_version()}"})
        blob: bytes | None = None
        for attempt in range(2):  # one retry on transient network errors
            try:
                with urllib.request.urlopen(req, timeout=_FETCH_TIMEOUT) as resp:
                    data = resp.read(_MAX_TILE_BYTES + 1)
                # Not an image is a failed tile, not a stored one: behind a
                # captive portal every tile of the area would otherwise be its
                # login page, and a later run would skip them all as present.
                if (data and len(data) <= _MAX_TILE_BYTES
                        and tile_sources.looks_like_image(data)):
                    blob = data
                break
            except Exception:  # noqa: BLE001 - every failure is a failed tile
                # Broad on purpose: an exception escaping this method dies in
                # its future, so the tile would be neither done nor failed and
                # the job would still report success.
                if attempt == 1 or cancel.is_set() or self._stop_event.is_set():
                    break
        if blob is None:
            with job["_lock"]:
                job["failed"] += 1
            self._fire_progress(job, job["_on_progress"])
            return
        # Buffer, then flush a full (or old enough) batch in one transaction.
        # The counters do not move here: a tile is "done" when it is in the
        # database, not when it is in this list, which is what stops a crash
        # mid-download from having reported tiles it did not keep.
        now = time.monotonic()
        with job["_lock"]:
            pending = job["_pending"]
            if not pending:
                job["_pending_since"] = now
            pending.append((z, x, y, blob))
            due = (
                len(pending) >= _WRITE_BATCH
                or now - job["_pending_since"] >= _WRITE_BATCH_SECONDS
            )
        if due:
            self._flush_pending(job)

    def _flush_pending(self, job: dict) -> int:
        """Commit whatever this job has buffered, and count it. Never raises.

        Returns the number of tiles written. A whole batch succeeds or fails
        together: that is coarser than the per-tile accounting it replaced,
        and it is the honest reading of it, because these rows go to SQLite in
        one transaction and it either kept all of them or none.

        The counters move here rather than at fetch time, so "done" keeps
        meaning "on disk". Another worker may append between the take and the
        write; that only makes the batch bigger.
        """
        with job["_lock"]:
            batch = job["_pending"]
            job["_pending"] = []
            job["_pending_since"] = 0.0
        if not batch:
            return 0
        try:
            written = int(self._cache.put_tiles(batch) or 0)
        except Exception:
            written = 0
        with job["_lock"]:
            job["done"] += written
            job["failed"] += len(batch) - written
        self._fire_progress(job, job["_on_progress"])
        return written

    def _run(self, job: dict) -> None:
        cancel = job["_cancel"]
        lock = job["_lock"]
        on_progress = job["_on_progress"]
        executor = ThreadPoolExecutor(max_workers=self._max_workers)
        job["_executor"] = executor
        try:
            for z, x_min, x_max, y_min, y_max in job["_ranges"]:
                for x in range(x_min, x_max + 1):
                    for y in range(y_min, y_max + 1):
                        if cancel.is_set() or self._stop_event.is_set():
                            break
                        if self._cache.has_tile(z, x, y):
                            with lock:
                                job["done"] += 1
                            self._fire_progress(job, on_progress)
                            continue
                        if self._stop_event.is_set() or cancel.is_set():
                            break
                        try:
                            executor.submit(self._download_one, job, z, x, y)
                        except RuntimeError:
                            # Executor was shut down underneath us (global
                            # shutdown raced the loop) — stop dispatching.
                            break
                        time.sleep(_SUBMIT_PAUSE)  # pace — don't hammer upstream
                    if cancel.is_set() or self._stop_event.is_set():
                        break
                if cancel.is_set() or self._stop_event.is_set():
                    break
        finally:
            # On a global shutdown, do not wait for in-flight fetches (bounded by
            # their own timeout); on normal/cancel completion, drain so the
            # final counters are accurate before announcing "done"/"cancelled".
            #
            # Queued fetches are dropped only when the job was stopped. The
            # loop above submits faster than a slow link fetches, so at the end
            # of a normal run most of a region can still be in the queue, and
            # cancelling it here reported a finished job with those tiles
            # silently missing from the cache.
            stopped = cancel.is_set() or self._stop_event.is_set()
            try:
                executor.shutdown(
                    wait=not self._stop_event.is_set(), cancel_futures=stopped)
            except Exception:
                pass
            # Everything fetched but not yet committed goes in before the job
            # is called finished — including on a cancel, where those tiles are
            # already paid for and throwing them away would make the next run
            # fetch them again.
            self._flush_pending(job)
            with lock:
                if job["state"] == "running":
                    if cancel.is_set() or self._stop_event.is_set():
                        job["state"] = "cancelled"
                    elif job["done"] == 0 and job["failed"] > 0:
                        # Not one tile is on disk for this area. Reporting that
                        # as a finished download is how an operator drives out
                        # believing they have a map they do not have.
                        job["state"] = "failed"
                        job["error"] = ("no tile could be downloaded. Check the "
                                        "internet connection and try again")
                    else:
                        job["state"] = "done"
            self._fire_progress(job, on_progress)
