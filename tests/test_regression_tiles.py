"""Tile endpoint smoke regression tests.

Hermetic, offline: real ``TileCache`` MBTiles files in ``tmp_path``, the
real ``_TileDownloaderPool`` (so the download POST exercises the genuine
job-creation path), and ``urllib.request.urlopen`` monkeypatched so no
network is ever hit. Mirrors the wiring in ``tests/test_server_tiles.py``
but at smoke-test granularity: sources listing, cached/offline tile serve,
and the download job-creation + bad-source rejection contracts.
"""
from __future__ import annotations

import http.client
import json
import threading
import urllib.error
from typing import Any

import pytest

from corvus.tile_sources import TILE_SOURCES


# ---------------------------------------------------------------------------
# Live-HTTP fixture: real caches + real downloader pool, ephemeral port.
# ---------------------------------------------------------------------------

@pytest.fixture
def tile_server(tmp_path):
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    from corvus.server import CorvusHandler, CorvusServer, _TileProgressBus
    from corvus.tile_cache import TileCache
    from corvus.tile_downloader import TileDownloader

    cache_dir = tmp_path / "tiles"
    cache_dir.mkdir()
    caches = {sid: TileCache(str(cache_dir / f"{sid}.mbtiles")) for sid in TILE_SOURCES}
    # Real per-source downloader pool (one TileDownloader per source).
    downloader = _RealDownloaderPool(caches, TileDownloader)
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
        target=server.serve_forever, name="corvus-reg-tiles", daemon=True,
        kwargs={"poll_interval": 0.05},
    )
    thread.start()
    try:
        yield server, caches
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        downloader.shutdown()
        for c in caches.values():
            c.close()
        (
            CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
            CorvusHandler.tile_caches, CorvusHandler.tile_downloader,
            CorvusHandler.tile_progress_bus,
        ) = saved


class _RealDownloaderPool:
    """Thin stand-in mirroring ``corvus.server._TileDownloaderPool`` so this
    test owns its teardown and does not import the private server symbol.

    Wraps one real :class:`TileDownloader` per source; routes start by source
    and cancel/status by job_id. ``shutdown()`` joins every orchestrator.
    """

    def __init__(self, caches, downloader_cls, max_workers: int = 3) -> None:
        self._downloaders: dict[str, Any] = {
            sid: downloader_cls(cache, max_workers=max_workers)
            for sid, cache in caches.items()
        }

    def start(self, source, upstream, bounds, minzoom, maxzoom, on_progress=None) -> str:
        dl = self._downloaders.get(source)
        if dl is None:
            raise ValueError(f"no downloader for source {source!r}")
        return dl.start(source, upstream, bounds, minzoom, maxzoom, on_progress=on_progress)

    def cancel(self, job_id: str) -> bool:
        return any(dl.cancel(job_id) for dl in self._downloaders.values())

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
            except Exception:
                pass
        self._downloaders.clear()


# ---------------------------------------------------------------------------
# HTTP helpers
# ---------------------------------------------------------------------------

def _get(server, path) -> tuple[int, bytes, dict[str, str]]:
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    conn.request("GET", path)
    resp = conn.getresponse()
    body = resp.read()
    headers = {k.lower(): v for k, v in resp.getheaders()}
    conn.close()
    return resp.status, body, headers


def _post(server, path, payload: dict) -> tuple[int, bytes]:
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    conn.request("POST", path, json.dumps(payload), {"Content-Type": "application/json"})
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, body


# ---------------------------------------------------------------------------
# 1. GET /api/tiles/sources
# ---------------------------------------------------------------------------

def test_tiles_sources_returns_three_ids(tile_server) -> None:
    server, _ = tile_server
    status, body, _ = _get(server, "/api/tiles/sources")
    assert status == 200
    data = json.loads(body)
    ids = {s["id"] for s in data["sources"]}
    assert ids == {"satellite", "streets", "hybrid"}
    assert ids == set(TILE_SOURCES)
    # Each source entry carries the cache stats fields the frontend needs.
    for s in data["sources"]:
        assert {"id", "label", "maxzoom", "cached_count"} <= set(s)


# ---------------------------------------------------------------------------
# 2. GET /api/tiles/<source>/<z>/<x>/<y>.png
# ---------------------------------------------------------------------------

def test_tiles_serve_cached_returns_png(tile_server) -> None:
    server, caches = tile_server
    blob = b"\x89PNG\r\n\x1a\n" + b"regression-tile-bytes"
    caches["satellite"].put_tile(0, 0, 0, blob)
    status, body, headers = _get(server, "/api/tiles/satellite/0/0/0.png")
    assert status == 200
    assert body == blob
    assert headers.get("content-type") == "image/png"


def test_tiles_serve_uncached_offline_returns_404(tile_server, monkeypatch) -> None:
    server, _ = tile_server

    def boom(*args: object, **kwargs: object) -> None:
        raise urllib.error.URLError("offline")

    monkeypatch.setattr("urllib.request.urlopen", boom)
    # streets at z=5 is uncached and the upstream fetch fails -> 404.
    status, body, _ = _get(server, "/api/tiles/streets/5/0/0.png")
    assert status == 404


def test_tiles_serve_unknown_source_returns_404(tile_server) -> None:
    server, _ = tile_server
    status, _, _ = _get(server, "/api/tiles/unknown/0/0/0.png")
    assert status == 404


# ---------------------------------------------------------------------------
# 3. POST /api/tiles/download
# ---------------------------------------------------------------------------

def test_tiles_download_valid_returns_job_id(tile_server, monkeypatch) -> None:
    server, _ = tile_server

    # Stub urlopen so the background fetch never touches the network.
    class _Resp:
        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False

        def read(self):
            return b"\x89PNG\r\n\x1a\n" + b"stub"

    monkeypatch.setattr("urllib.request.urlopen", lambda *a, **k: _Resp())

    # Whole world at z=0 -> exactly 1 tile; the job completes fast and hermetic.
    status, body = _post(server, "/api/tiles/download", {
        "source": "satellite",
        "bounds": {"w": -180.0, "s": -85.0, "e": 180.0, "n": 85.0},
        "minzoom": 0, "maxzoom": 0,
    })
    assert status == 200
    data = json.loads(body)
    assert "job_id" in data
    assert isinstance(data["job_id"], str) and data["job_id"]


def test_tiles_download_bad_source_returns_400(tile_server) -> None:
    server, _ = tile_server
    status, body = _post(server, "/api/tiles/download", {
        "source": "nope",
        "bounds": {"w": 0, "s": 0, "e": 1, "n": 1},
        "minzoom": 0, "maxzoom": 10,
    })
    assert status == 400
    assert json.loads(body).get("ok") is False
