"""Tests for corvus/tile_downloader.py — XYZ tile math, resumable downloads,
cancel, shutdown, and the per-job tile cap.

Fast (< 3 s): tiny bounds, a stubbed ``urllib.request.urlopen`` returning fixed
PNG bytes, and a real on-disk :class:`TileCache` in ``tmp_path``.
"""
from __future__ import annotations

import time

import pytest

import corvus.tile_downloader as td
from corvus.tile_cache import TileCache
from corvus.tile_downloader import (
    MAX_TILES_PER_JOB,
    TileDownloader,
    enumerate_tiles,
    lat_to_y,
    lon_to_x,
    tile_count,
)


# ---------------------------------------------------------------------------
# Stubbed upstream: a context-manager response returning fixed PNG bytes.
# Patched onto urllib.request so the downloader never touches the network.
# ---------------------------------------------------------------------------
class _FakeResp:
    def __init__(self, data: bytes) -> None:
        self._data = data

    def __enter__(self) -> _FakeResp:
        return self

    def __exit__(self, *exc) -> bool:
        return False

    def read(self) -> bytes:
        return self._data


@pytest.fixture
def stub_urlopen(monkeypatch):
    """Patch urlopen to return fixed bytes and count how often it is called."""
    counter = {"n": 0, "urls": []}

    def fake_urlopen(url, timeout=None):
        counter["n"] += 1
        counter["urls"].append(url)
        return _FakeResp(b"\x89PNG\r\n\x1a\n" + b"tile-bytes")

    monkeypatch.setattr(td.urllib.request, "urlopen", fake_urlopen)
    return counter


@pytest.fixture
def cache(tmp_path) -> TileCache:
    c = TileCache(str(tmp_path / "dl.mbtiles"))
    yield c
    c.close()


def _wait_until(predicate, timeout=3.0, interval=0.01):
    """Poll predicate until True or timeout (seconds). Returns last value."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


# ===========================================================================
# XYZ tile math
# ===========================================================================

def test_lon_to_x_lat_to_y_known_values() -> None:
    # At z=0 the whole world is a single tile (0, 0).
    assert lon_to_x(0.0, 0) == 0
    assert lon_to_x(179.9, 0) == 0
    assert lat_to_y(0.0, 0) == 0
    assert lat_to_y(47.0, 0) == 0
    # Equator at z=1: longitude 0 -> column 1; -90 -> 0; +90 -> 1.
    assert lon_to_x(-90.0, 1) == 0
    assert lon_to_x(0.0, 1) == 1
    assert lon_to_x(90.0, 1) == 1
    # Latitude: at z=1, north hemisphere -> row 0, south -> row 1.
    assert lat_to_y(45.0, 1) == 0
    assert lat_to_y(-45.0, 1) == 1


def test_lat_to_y_clamps_near_poles() -> None:
    # A latitude beyond the Mercator limit must not raise (clamped internally).
    assert lat_to_y(89.0, 4) == 0
    assert lat_to_y(-89.0, 4) == (1 << 4) - 1


def test_tile_count_z0_is_one() -> None:
    # Any finite bounds at z=0 cover exactly the one world tile.
    assert tile_count((8.0, 47.0, 9.0, 48.0), 0, 0) == 1
    assert list(enumerate_tiles((8.0, 47.0, 9.0, 48.0), 0, 0)) == [(0, 0, 0)]


def test_tile_count_whole_world_z1_is_four() -> None:
    bounds = (-180.0, -85.0, 180.0, 85.0)
    assert tile_count(bounds, 1, 1) == 4
    tiles = sorted(enumerate_tiles(bounds, 1, 1))
    assert tiles == [(1, 0, 0), (1, 0, 1), (1, 1, 0), (1, 1, 1)]


def test_tile_count_small_bounds_matches_reference() -> None:
    # Independent re-derivation of the count for a small European bounds.
    bounds = (8.0, 47.0, 9.0, 48.0)
    z = 12
    expected = 0
    for _x in range(lon_to_x(8.0, z), lon_to_x(9.0, z) + 1):
        for _y in range(lat_to_y(48.0, z), lat_to_y(47.0, z) + 1):
            expected += 1
    assert tile_count(bounds, z, z) == expected
    assert expected > 1, "bounds spans more than one tile at z=12"


def test_enumerate_tiles_yields_in_bounds() -> None:
    bounds = (8.0, 47.0, 9.0, 48.0)
    for z, x, y in enumerate_tiles(bounds, 10, 10):
        assert 0 <= x < (1 << z)
        assert 0 <= y < (1 << z)


# ===========================================================================
# start / status / list_jobs — happy path
# ===========================================================================

def test_start_returns_jobid_status_running_then_done(cache, stub_urlopen) -> None:
    dl = TileDownloader(cache, max_workers=3)
    bounds = (-180.0, -85.0, 180.0, 85.0)  # 4 tiles at z=1
    jid = dl.start("satellite", "https://up/{z}/{x}/{y}.png", bounds, 1, 1)
    assert isinstance(jid, str) and len(jid) == 32  # uuid4 hex

    # Listed immediately.
    assert any(j["job_id"] == jid for j in dl.list_jobs())
    assert dl.status(jid)["state"] in ("running", "done")

    # Drains to done with all 4 tiles fetched and cached.
    assert _wait_until(lambda: dl.status(jid)["state"] in ("done", "cancelled", "failed"))
    st = dl.status(jid)
    assert st["state"] == "done"
    assert st["done"] == 4
    assert st["failed"] == 0
    assert st["total"] == 4
    assert stub_urlopen["n"] == 4
    # Every requested URL was built from the slippy template.
    for url in stub_urlopen["urls"]:
        assert url.startswith("https://up/")
        assert url.endswith(".png")
    # The cache holds all 4 tiles.
    assert cache.stats()["count"] == 4
    dl.shutdown()


def test_on_progress_callback_invoked_with_snapshots(cache, stub_urlopen) -> None:
    snapshots: list[dict] = []
    dl = TileDownloader(cache)
    bounds = (-180.0, -85.0, 180.0, 85.0)
    jid = dl.start("satellite", "https://up/{z}/{x}/{y}.png", bounds, 1, 1,
                   on_progress=snapshots.append)
    assert _wait_until(lambda: dl.status(jid)["state"] == "done")
    # Per BATCH, not per tile. Cache writes are committed in batches of
    # _WRITE_BATCH (or after _WRITE_BATCH_SECONDS, whichever comes first) and
    # the counters move when a batch lands, so four tiles fetched inside one
    # second are one progress event, not four. That is the point: a 50 000-tile
    # region used to fire fifty thousand of these at the SSE stream, one per
    # serialized commit.
    assert snapshots, "progress never fired"
    assert snapshots[-1]["state"] == "done"
    assert snapshots[-1]["done"] == 4
    # Whatever the granularity, the count never goes backwards and never
    # overshoots — that is what the UI's bar actually depends on.
    counts = [s["done"] for s in snapshots]
    assert counts == sorted(counts), f"progress went backwards: {counts}"
    assert max(counts) == 4
    dl.shutdown()


# ===========================================================================
# Resumable — cached tiles are skipped on a second run
# ===========================================================================

def test_resumable_skips_cached_tiles(cache, stub_urlopen) -> None:
    dl = TileDownloader(cache)
    bounds = (-180.0, -85.0, 180.0, 85.0)  # 4 tiles at z=1

    jid1 = dl.start("satellite", "https://up/{z}/{x}/{y}.png", bounds, 1, 1)
    assert _wait_until(lambda: dl.status(jid1)["state"] == "done")
    assert dl.status(jid1)["done"] == 4
    assert stub_urlopen["n"] == 4
    assert cache.stats()["count"] == 4

    # Second run with identical bounds: every tile is already cached, so the
    # upstream is never hit again and the job completes immediately.
    stub_urlopen["n"] = 0
    jid2 = dl.start("satellite", "https://up/{z}/{x}/{y}.png", bounds, 1, 1)
    assert _wait_until(lambda: dl.status(jid2)["state"] == "done")
    assert dl.status(jid2)["done"] == 4
    assert dl.status(jid2)["failed"] == 0
    assert stub_urlopen["n"] == 0, "cached tiles were not re-fetched"
    assert cache.stats()["count"] == 4
    dl.shutdown()


# ===========================================================================
# Cancel — stops dispatching; state becomes "cancelled"; resume completes
# ===========================================================================

def test_cancel_sets_state_cancelled(cache, stub_urlopen) -> None:
    dl = TileDownloader(cache)
    # Whole world at z=2 -> 16 tiles. The 0.05 s submission pace guarantees the
    # job is still running when we cancel shortly after start.
    bounds = (-180.0, -85.0, 180.0, 85.0)
    assert tile_count(bounds, 2, 2) == 16
    jid = dl.start("satellite", "https://up/{z}/{x}/{y}.png", bounds, 2, 2)

    # Wait until the worker has processed at least one tile, then cancel.
    assert _wait_until(lambda: dl.status(jid)["done"] >= 1, timeout=2.0), "job started"
    assert dl.cancel(jid) is True
    assert _wait_until(lambda: dl.status(jid)["state"] in ("cancelled", "done"))
    st = dl.status(jid)
    # Cancel arrived before all 16 were dispatched (race-safe upper bound).
    assert st["state"] in ("cancelled", "done")
    if st["state"] == "cancelled":
        assert st["done"] < 16, "cancel stopped dispatch before completion"

    # Cancelling an unknown job is False, not a crash.
    assert dl.cancel("does-not-exist") is False
    dl.shutdown()


def test_resume_after_cancel_completes(cache, stub_urlopen) -> None:
    dl = TileDownloader(cache)
    bounds = (-180.0, -85.0, 180.0, 85.0)  # 16 tiles at z=2
    jid = dl.start("satellite", "https://up/{z}/{x}/{y}.png", bounds, 2, 2)
    assert _wait_until(lambda: dl.status(jid)["done"] >= 1, timeout=2.0)
    dl.cancel(jid)
    assert _wait_until(lambda: dl.status(jid)["state"] in ("cancelled", "done"))
    partial = cache.stats()["count"]

    # A fresh job over the same bounds resumes: skips the cached tiles, fetches
    # the rest, and reaches done==16.
    jid2 = dl.start("satellite", "https://up/{z}/{x}/{y}.png", bounds, 2, 2)
    assert _wait_until(lambda: dl.status(jid2)["state"] == "done", timeout=3.0)
    assert dl.status(jid2)["done"] == 16
    assert dl.status(jid2)["failed"] == 0
    assert cache.stats()["count"] == 16
    assert cache.stats()["count"] >= partial
    dl.shutdown()


# ===========================================================================
# Failure modes — empty range and the 50k cap return a "failed" job
# ===========================================================================

def test_empty_range_returns_failed_job(cache, stub_urlopen) -> None:
    dl = TileDownloader(cache)
    # minzoom > maxzoom -> no zoom levels -> zero tiles.
    jid = dl.start("satellite", "https://up/{z}/{x}/{y}.png", (8.0, 47.0, 9.0, 48.0), 5, 3)
    st = dl.status(jid)
    assert st is not None
    assert st["state"] == "failed"
    assert st["total"] == 0
    assert "no tiles" in (st["error"] or "")
    assert any(j["job_id"] == jid for j in dl.list_jobs())
    # No thread was spawned and no fetch happened.
    assert stub_urlopen["n"] == 0
    dl.shutdown()


def test_cap_exceeds_returns_failed_job(cache, stub_urlopen) -> None:
    dl = TileDownloader(cache)
    # Whole world at z=8 -> 256 x 256 = 65_536 tiles > 50_000 cap.
    bounds = (-180.0, -85.0, 180.0, 85.0)
    assert tile_count(bounds, 8, 8) == 256 * 256
    jid = dl.start("satellite", "https://up/{z}/{x}/{y}.png", bounds, 8, 8)
    st = dl.status(jid)
    assert st["state"] == "failed"
    assert st["total"] == 256 * 256
    assert "cap" in (st["error"] or "").lower()
    assert st["total"] > MAX_TILES_PER_JOB
    # Nothing fetched for a failed job.
    assert stub_urlopen["n"] == 0
    dl.shutdown()


# ===========================================================================
# Clean shutdown — joins orchestrator threads and is idempotent
# ===========================================================================

def test_shutdown_joins_threads_and_is_idempotent(cache, stub_urlopen) -> None:
    dl = TileDownloader(cache)
    bounds = (-180.0, -85.0, 180.0, 85.0)
    jid = dl.start("satellite", "https://up/{z}/{x}/{y}.png", bounds, 1, 1)
    # Capture the orchestrator thread so we can inspect it after shutdown.
    with dl._jobs_lock:
        thread = dl._jobs[jid].get("_thread")
    assert thread is not None and thread.is_alive()

    dl.shutdown()
    assert _wait_until(lambda: not thread.is_alive(), timeout=3.0), "orchestrator joined"
    # Idempotent — a second call is a no-op and must not raise.
    dl.shutdown()
    dl.shutdown()


# ===========================================================================
# Batched cache writes — one transaction per batch, not per tile
# ===========================================================================

def test_tiles_are_committed_in_batches_not_one_at_a_time(cache, stub_urlopen) -> None:
    """A whole job's tiles reach the cache in far fewer commits than tiles.

    This is the shape of the fix, not an incidental detail: a region job is
    capped at MAX_TILES_PER_JOB (50 000) tiles and used to pay one SQLite
    transaction — and, before synchronous=NORMAL, one fsync — for every one of
    them, through a single lock shared by three workers. On a field laptop's
    disk that was the dominant cost of downloading a region, well above the
    network it was waiting on.

    Asserted as "many tiles, few writes" rather than an exact count, because
    the batch boundary depends on _WRITE_BATCH_SECONDS and on how fast the
    machine running this fetches.
    """
    commits: list[int] = []
    real_put_tiles = cache.put_tiles

    def counting_put_tiles(tiles):
        rows = list(tiles)
        commits.append(len(rows))
        return real_put_tiles(rows)

    cache.put_tiles = counting_put_tiles
    dl = TileDownloader(cache, max_workers=3)
    bounds = (-180.0, -85.0, 180.0, 85.0)
    jid = dl.start("satellite", "https://up/{z}/{x}/{y}.png", bounds, 1, 2)
    assert _wait_until(lambda: dl.status(jid)["state"] == "done")
    st = dl.status(jid)

    tiles = st["total"]
    assert tiles == 20, "4 tiles at z=1 plus 16 at z=2"
    assert st["done"] == tiles
    assert st["failed"] == 0
    # Every tile landed, and the counters only ever claimed tiles that did.
    assert cache.stats()["count"] == tiles
    assert sum(commits) == tiles
    assert len(commits) < tiles, (
        f"{tiles} tiles took {len(commits)} commits — batching is not happening")
    dl.shutdown()
