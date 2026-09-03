"""Tests for the tile cache/serve/download API in corvus/server.py.

Hermetic and offline: a real ``TileCache`` in a tmp dir, a ``FakeTileDownloader``
implementing the ``TileDownloader`` contract (so the tests do not depend on
the parallel-built ``tile_downloader`` module), and ``urllib.request.urlopen``
monkey-patched to fail for the upstream-fetch 404 case. No real network is
hit; no files are written under the user's home.
"""
from __future__ import annotations

import http.client
import json
import queue
import sqlite3
import threading
import urllib.error
from typing import Any

import pytest

from corvus.server import (
    CorvusHandler,
    CorvusServer,
    _BoundedSseBuffer,
    _TileProgressBus,
)
from corvus.tile_cache import TileCache
from corvus.tile_sources import TILE_SOURCES, get as sources_get, list_sources


# ---------------------------------------------------------------------------
# tile_sources unit tests (pure, no server)
# ---------------------------------------------------------------------------

def test_tile_sources_list_sources_shape() -> None:
    sources = list_sources()
    assert [s["id"] for s in sources] == list(TILE_SOURCES)
    for s in sources:
        assert set(s) == {"id", "label", "provider", "style", "maxzoom", "attribution"}
        assert isinstance(s["maxzoom"], int) and s["maxzoom"] > 0


def test_tile_sources_get_returns_copy_or_none() -> None:
    entry = sources_get("satellite")
    assert entry is not None
    assert entry["label"] == "Satellite"
    assert "{z}/{y}/{x}" in entry["upstream"]
    # mutating the returned dict must not corrupt the registry
    entry["upstream"] = "tampered"
    assert sources_get("satellite")["upstream"] != "tampered"
    assert sources_get("does-not-exist") is None


# ---------------------------------------------------------------------------
# FakeTileDownloader: in-process stand-in matching the TileDownloader contract
# ---------------------------------------------------------------------------

class FakeTileDownloader:
    def __init__(self, cache: TileCache | None = None, max_workers: int = 3) -> None:
        self.cache = cache
        self._jobs: dict[str, dict[str, Any]] = {}
        self._counter = 0
        self.shutdown_called = False

    def start(
        self,
        source: str,
        upstream: str,
        bounds: tuple,
        minzoom: int,
        maxzoom: int,
        on_progress: Any = None,
    ) -> str:
        self._counter += 1
        jid = f"job-{self._counter}"
        self._jobs[jid] = {
            "status": {"job_id": jid, "state": "running", "done": 0, "total": 10, "failed": 0},
            "on_progress": on_progress,
        }
        return jid

    def cancel(self, job_id: str) -> bool:
        job = self._jobs.get(job_id)
        if job is None:
            return False
        job["status"]["state"] = "cancelled"
        return True

    def status(self, job_id: str) -> dict | None:
        job = self._jobs.get(job_id)
        return dict(job["status"]) if job else None

    def list_jobs(self) -> list[dict]:
        return [dict(j["status"]) for j in self._jobs.values()]

    def shutdown(self) -> None:
        self.shutdown_called = True


# ---------------------------------------------------------------------------
# Live-HTTP fixture: real caches + fake downloader, on an ephemeral port
# ---------------------------------------------------------------------------

@pytest.fixture
def tile_server(tmp_path):
    """A live CorvusServer wired with per-source TileCaches + a FakeTileDownloader."""
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")

    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    caches = {sid: TileCache(str(cache_dir / f"{sid}.mbtiles")) for sid in TILE_SOURCES}
    downloader = FakeTileDownloader()
    bus = _TileProgressBus()

    saved = (
        CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
        CorvusHandler.tile_caches, CorvusHandler.tile_downloader,
        CorvusHandler.tile_progress_bus,
    )
    CorvusHandler.store = None
    CorvusHandler.mavlink = None
    CorvusHandler.ssh = None
    CorvusHandler.tile_caches = caches
    CorvusHandler.tile_downloader = downloader
    CorvusHandler.tile_progress_bus = bus

    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    server.tile_caches = caches
    server.tile_downloader = downloader
    thread = threading.Thread(
        target=server.serve_forever, name="corvus-test-tiles", daemon=True,
        kwargs={"poll_interval": 0.05},
    )
    thread.start()
    try:
        yield server, downloader, caches
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        # shutdown() already closed the caches; this is a defensive no-op.
        for cache in caches.values():
            cache.close()
        (
            CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
            CorvusHandler.tile_caches, CorvusHandler.tile_downloader,
            CorvusHandler.tile_progress_bus,
        ) = saved


def _get(server: CorvusServer, path: str) -> tuple[int, bytes]:
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    conn.request("GET", path)
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, body


def _post(server: CorvusServer, path: str, payload: dict) -> tuple[int, bytes]:
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    conn.request("POST", path, json.dumps(payload), {"Content-Type": "application/json"})
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, body


# ---------------------------------------------------------------------------
# GET /api/tiles/sources
# ---------------------------------------------------------------------------

def test_tiles_sources_returns_all_registered(tile_server) -> None:
    server, _, _ = tile_server
    status, body = _get(server, "/api/tiles/sources")
    assert status == 200
    data = json.loads(body)
    ids = {s["id"] for s in data["sources"]}
    assert ids == set(TILE_SOURCES)
    for s in data["sources"]:
        assert {"id", "label", "maxzoom", "cached_count", "minzoom", "maxzoom"} <= set(s)
        assert s["cached_count"] == 0  # fresh caches


def test_tiles_sources_does_not_shadow_into_tile_serve(tile_server) -> None:
    """The exact route /api/tiles/sources must win over the .png path-param regex."""
    server, _, _ = tile_server
    status, body = _get(server, "/api/tiles/sources")
    assert status == 200
    assert "sources" in json.loads(body)


# ---------------------------------------------------------------------------
# POST /api/tiles/download
# ---------------------------------------------------------------------------

def test_tiles_download_valid_returns_job_id(tile_server) -> None:
    server, dl, _ = tile_server
    status, body = _post(server, "/api/tiles/download", {
        "source": "satellite",
        "bounds": {"w": 0.0, "s": 0.0, "e": 1.0, "n": 1.0},
        "minzoom": 0, "maxzoom": 10,
    })
    assert status == 200
    data = json.loads(body)
    assert "job_id" in data
    assert dl.status(data["job_id"]) is not None


def test_tiles_download_bad_source_returns_400(tile_server) -> None:
    server, _, _ = tile_server
    status, body = _post(server, "/api/tiles/download", {
        "source": "nope", "bounds": {"w": 0, "s": 0, "e": 1, "n": 1},
        "minzoom": 0, "maxzoom": 10,
    })
    assert status == 400
    assert json.loads(body)["ok"] is False


def test_tiles_download_bad_bounds_returns_400(tile_server) -> None:
    server, _, _ = tile_server
    status, body = _post(server, "/api/tiles/download", {
        "source": "satellite", "bounds": {"w": 200, "s": 0, "e": 1, "n": 1},
        "minzoom": 0, "maxzoom": 10,
    })
    assert status == 400
    assert json.loads(body)["ok"] is False


def test_tiles_download_bad_zoom_returns_400(tile_server) -> None:
    server, _, _ = tile_server
    status, body = _post(server, "/api/tiles/download", {
        "source": "satellite", "bounds": {"w": 0, "s": 0, "e": 1, "n": 1},
        "minzoom": 5, "maxzoom": 3,
    })
    assert status == 400
    assert json.loads(body)["ok"] is False


def test_tiles_download_maxzoom_above_source_cap_returns_400(tile_server) -> None:
    server, _, _ = tile_server
    status, body = _post(server, "/api/tiles/download", {
        "source": "satellite", "bounds": {"w": 0, "s": 0, "e": 1, "n": 1},
        "minzoom": 0, "maxzoom": 22,  # source cap is 19
    })
    assert status == 400


# ---------------------------------------------------------------------------
# GET /api/tiles/jobs
# ---------------------------------------------------------------------------

def test_tiles_jobs_lists_job(tile_server) -> None:
    server, _, _ = tile_server
    _post(server, "/api/tiles/download", {
        "source": "satellite", "bounds": {"w": 0, "s": 0, "e": 1, "n": 1},
        "minzoom": 0, "maxzoom": 10,
    })
    status, body = _get(server, "/api/tiles/jobs")
    assert status == 200
    data = json.loads(body)
    assert len(data["jobs"]) == 1
    assert data["jobs"][0]["job_id"].startswith("job-")


def test_tiles_jobs_route_not_shadowed_by_png_regex(tile_server) -> None:
    server, _, _ = tile_server
    status, body = _get(server, "/api/tiles/jobs")
    assert status == 200
    assert "jobs" in json.loads(body)


# ---------------------------------------------------------------------------
# POST /api/tiles/cancel
# ---------------------------------------------------------------------------

def test_tiles_cancel_returns_ok(tile_server) -> None:
    server, _, _ = tile_server
    _, body = _post(server, "/api/tiles/download", {
        "source": "satellite", "bounds": {"w": 0, "s": 0, "e": 1, "n": 1},
        "minzoom": 0, "maxzoom": 10,
    })
    jid = json.loads(body)["job_id"]
    status, body = _post(server, "/api/tiles/cancel", {"id": jid})
    assert status == 200
    assert json.loads(body)["ok"] is True


def test_tiles_cancel_unknown_returns_404(tile_server) -> None:
    server, _, _ = tile_server
    status, body = _post(server, "/api/tiles/cancel", {"id": "nope"})
    assert status == 404
    assert json.loads(body)["ok"] is False


# ---------------------------------------------------------------------------
# GET /api/tiles/<source>/<z>/<x>/<y>.png
# ---------------------------------------------------------------------------

def test_tiles_serve_cached_returns_png(tile_server) -> None:
    server, _, caches = tile_server
    blob = b"\x89PNG\r\n\x1a\n" + b"fake-tile-bytes"
    caches["satellite"].put_tile(0, 0, 0, blob)
    status, body = _get(server, "/api/tiles/satellite/0/0/0.png")
    assert status == 200
    assert body == blob


def test_tiles_serve_uncached_offline_returns_404(tile_server, monkeypatch) -> None:
    server, _, _ = tile_server

    def boom(*args: object, **kwargs: object) -> None:
        raise urllib.error.URLError("offline")

    monkeypatch.setattr("urllib.request.urlopen", boom)
    status, _ = _get(server, "/api/tiles/satellite/5/0/0.png")
    assert status == 404


def test_tiles_serve_unknown_source_returns_404(tile_server) -> None:
    server, _, _ = tile_server
    status, _ = _get(server, "/api/tiles/unknown/0/0/0.png")
    assert status == 404


def test_tiles_serve_out_of_range_zoom_returns_404(tile_server) -> None:
    server, _, _ = tile_server
    # z=30 exceeds the 0..22 cap; the regex matches digits, the handler rejects.
    status, _ = _get(server, "/api/tiles/satellite/30/0/0.png")
    assert status == 404


def _get_full(server: CorvusServer, path: str) -> tuple[int, str, bytes]:
    """Like _get but also returns the response headers (for Content-Type checks)."""
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    conn.request("GET", path)
    resp = conn.getresponse()
    body = resp.read()
    ctype = resp.getheader("Content-Type", "")
    conn.close()
    return resp.status, ctype, body


def test_tiles_sources_exposes_cached_minmax_and_source_cap(tile_server) -> None:
    """``maxzoom`` is the SOURCE cap (19); cached stats live under cached_*.

    Pins the duplicate-key rename: previously a second ``maxzoom`` key held the
    cache max (or null when empty), clobbering the real 19 cap and forcing a
    hardcoded SOURCE_ZOOM_CAP workaround in the UI.
    """
    server, _, caches = tile_server
    caches["satellite"].put_tile(5, 3, 2, b"\x89PNG\r\n\x1a\n" + b"x")
    status, body = _get(server, "/api/tiles/sources")
    assert status == 200
    src = next(s for s in json.loads(body)["sources"] if s["id"] == "satellite")
    # maxzoom is the constant source cap, NOT the cache max (5).
    assert src["maxzoom"] == 19
    assert src["minzoom"] == 0
    # The cache stats are under distinct keys that can never clobber maxzoom.
    assert src["cached_count"] == 1
    assert src["cached_minzoom"] == 5
    assert src["cached_maxzoom"] == 5
    # An empty cache reports null cached_* (not 0) so the UI can tell "no tiles".
    empty = next(s for s in json.loads(body)["sources"] if s["id"] == "streets")
    assert empty["cached_count"] == 0
    assert empty["cached_minzoom"] is None
    assert empty["cached_maxzoom"] is None


def test_tiles_serve_get_tile_exception_treated_as_miss(tile_server, monkeypatch) -> None:
    """A raised get_tile (closed/corrupt cache) must NOT 500 — it's a cache miss.

    The handler wraps the cache read in try/except so a closed SQLite handle
    during the shutdown close-race falls through to the upstream fetch (or a
    404 offline) instead of crashing the request handler.
    """
    server, _, caches = tile_server

    def boom(z: int, x: int, y: int) -> bytes | None:
        raise sqlite3.ProgrammingError("Cannot operate on a closed database")

    monkeypatch.setattr(caches["satellite"], "get_tile", boom)
    # Offline so the fall-through upstream fetch also fails -> 404, not 500.
    monkeypatch.setattr(
        "urllib.request.urlopen",
        lambda *a, **k: (_ for _ in ()).throw(urllib.error.URLError("offline")),
    )
    status, _ = _get(server, "/api/tiles/satellite/5/0/0.png")
    assert status == 404


def test_tiles_serve_sniffs_jpeg_content_type(tile_server) -> None:
    """Satellite tiles are JPEG; the Content-Type is sniffed from magic bytes."""
    server, _, caches = tile_server
    jpeg = b"\xff\xd8\xff\xe0" + b"JFIF-data"
    caches["satellite"].put_tile(6, 1, 1, jpeg)
    status, ctype, body = _get_full(server, "/api/tiles/satellite/6/1/1.png")
    assert status == 200
    assert ctype == "image/jpeg", f"JPEG magic must sniff as image/jpeg, got {ctype!r}"
    assert body == jpeg


def test_tiles_serve_sniffs_png_content_type(tile_server) -> None:
    """Streets/topo tiles are PNG; the magic-byte sniff labels them image/png."""
    server, _, caches = tile_server
    png = b"\x89PNG\r\n\x1a\n" + b"PNG-data"
    caches["streets"].put_tile(6, 1, 1, png)
    status, ctype, body = _get_full(server, "/api/tiles/streets/6/1/1.png")
    assert status == 200
    assert ctype == "image/png"
    assert body == png


# ---------------------------------------------------------------------------
# GET /api/tiles/progress (pre-stream validation over HTTP; stream over handler)
# ---------------------------------------------------------------------------

def test_tiles_progress_missing_id_returns_400(tile_server) -> None:
    server, _, _ = tile_server
    status, body = _get(server, "/api/tiles/progress")
    assert status == 400
    assert "id" in json.loads(body).get("error", "")


def test_tiles_progress_unknown_job_returns_404(tile_server) -> None:
    server, _, _ = tile_server
    status, body = _get(server, "/api/tiles/progress?id=nope")
    assert status == 404


def test_sse_tiles_progress_emits_done_and_unsubscribes(monkeypatch) -> None:
    """Handler-level SSE test mirroring the test_sse_params_* pattern."""
    bus = _TileProgressBus()
    dl = FakeTileDownloader()
    jid = dl.start("satellite", "u", (0.0, 0.0, 1.0, 1.0), 0, 1)

    handler = object.__new__(CorvusHandler)
    handler.tile_progress_bus = bus
    handler.tile_downloader = dl
    handler.tile_caches = None
    sent: list[tuple[str, str]] = []
    handler._send_sse = lambda event, data: sent.append((event, data))  # type: ignore[method-assign]
    handler.send_response = lambda status: None  # type: ignore[method-assign]
    handler.send_header = lambda name, value: None  # type: ignore[method-assign]
    handler.end_headers = lambda: None  # type: ignore[method-assign]
    handler.path = f"/api/tiles/progress?id={jid}"

    calls = {"n": 0}

    def fake_get(self: _BoundedSseBuffer, timeout: float | None = None) -> Any:
        calls["n"] += 1
        if calls["n"] == 1:
            return {"job_id": jid, "state": "done", "done": 10, "total": 10, "failed": 0}
        raise queue.Empty

    monkeypatch.setattr(_BoundedSseBuffer, "get", fake_get)

    handler._sse_tiles_progress()

    # initial status (running) then the done event
    assert sent[0][0] == "progress"
    assert json.loads(sent[0][1])["state"] == "running"
    assert sent[1][0] == "progress"
    assert json.loads(sent[1][1])["state"] == "done"
    # the loop broke on "done" and unsubscribed
    assert bus._subs.get(jid) is None


def test_tile_progress_bus_fans_out_and_drops_without_job_id() -> None:
    bus = _TileProgressBus()
    received: list[dict] = []
    bus.subscribe("job-1", received.append)
    cb = bus.make_on_progress()
    # routed progress reaches the subscriber
    cb({"job_id": "job-1", "state": "running", "done": 1, "total": 4, "failed": 0})
    assert received and received[-1]["done"] == 1
    # a progress dict without job_id is dropped (cannot route)
    cb({"state": "running", "done": 2, "total": 4, "failed": 0})
    assert len(received) == 1
    # a bad subscriber must not abort the fan-out
    def angry(_progress: dict) -> None:
        raise RuntimeError("boom")
    bus.subscribe("job-1", angry)
    received2: list[dict] = []
    bus.subscribe("job-1", received2.append)
    cb({"job_id": "job-1", "state": "done", "done": 4, "total": 4, "failed": 0})
    assert received2 and received2[-1]["state"] == "done"
    # unsubscribe cleans up
    bus.unsubscribe("job-1", received.append)
    bus.unsubscribe("job-1", angry)
    bus.unsubscribe("job-1", received2.append)
    assert bus._subs.get("job-1") is None


# ---------------------------------------------------------------------------
# Clean shutdown: CorvusServer.shutdown() releases tile resources
# ---------------------------------------------------------------------------

def test_server_shutdown_closes_tile_resources(tmp_path) -> None:
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    caches = {sid: TileCache(str(cache_dir / f"{sid}.mbtiles")) for sid in TILE_SOURCES}
    downloader = FakeTileDownloader()
    saved = (
        CorvusHandler.tile_caches, CorvusHandler.tile_downloader,
        CorvusHandler.tile_progress_bus,
    )
    CorvusHandler.tile_caches = caches
    CorvusHandler.tile_downloader = downloader
    CorvusHandler.tile_progress_bus = _TileProgressBus()
    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    server.tile_caches = caches
    server.tile_downloader = downloader
    thread = threading.Thread(
        target=server.serve_forever, name="corvus-shutdown-test", daemon=True,
        kwargs={"poll_interval": 0.05},
    )
    thread.start()
    try:
        server.shutdown()
    finally:
        server.server_close()
        thread.join(timeout=2)
        (
            CorvusHandler.tile_caches, CorvusHandler.tile_downloader,
            CorvusHandler.tile_progress_bus,
        ) = saved
    assert downloader.shutdown_called is True
    assert all(c._closed for c in caches.values())
