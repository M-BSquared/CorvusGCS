"""Tests for the offline MBTiles tile cache (corvus/tile_cache.py).

Pure stdlib + pytest. Covers schema creation, XYZ/TMS conversion, round-trip,
stats, metadata/bounds, thread safety under the shared-connection lock,
persistence across close/reopen, and idempotent close.
"""
from __future__ import annotations

import os
import sqlite3
from concurrent.futures import ThreadPoolExecutor

from corvus.tile_cache import TileCache, default_cache_dir, xyz_to_tms


def test_xyz_to_tms_conversion() -> None:
    assert xyz_to_tms(0, 0) == 0
    assert xyz_to_tms(1, 0) == 1
    assert xyz_to_tms(1, 1) == 0
    # z=2 has rows 0..3; TMS flips: y0 -> row 3, y3 -> row 0.
    assert xyz_to_tms(2, 0) == 3
    assert xyz_to_tms(2, 3) == 0


def test_default_cache_dir_is_under_home() -> None:
    assert default_cache_dir() == os.path.expanduser("~/.corvus/tiles")


def test_create_and_roundtrip_tile(tmp_path) -> None:
    cache = TileCache(str(tmp_path / "c.mbtiles"))
    blob = b"\x89PNG\r\n\x1a\n" + b"tile-bytes"
    assert cache.get_tile(3, 2, 1) is None
    cache.put_tile(3, 2, 1, blob)
    assert cache.get_tile(3, 2, 1) == blob
    cache.close()


def test_has_tile_true_false(tmp_path) -> None:
    cache = TileCache(str(tmp_path / "c.mbtiles"))
    assert cache.has_tile(5, 0, 0) is False
    cache.put_tile(5, 0, 0, b"x")
    assert cache.has_tile(5, 0, 0) is True
    cache.close()


def test_put_tile_noop_on_empty(tmp_path) -> None:
    cache = TileCache(str(tmp_path / "c.mbtiles"))
    cache.put_tile(4, 1, 1, b"")
    assert cache.has_tile(4, 1, 1) is False
    assert cache.stats()["count"] == 0
    cache.close()


def test_put_tile_replaces_existing(tmp_path) -> None:
    cache = TileCache(str(tmp_path / "c.mbtiles"))
    cache.put_tile(6, 3, 2, b"old")
    cache.put_tile(6, 3, 2, b"new")
    assert cache.get_tile(6, 3, 2) == b"new"
    assert cache.stats()["count"] == 1
    cache.close()


def test_stats_counts_and_zoom_range(tmp_path) -> None:
    cache = TileCache(str(tmp_path / "c.mbtiles"))
    cache.put_tile(1, 0, 0, b"a")
    cache.put_tile(2, 0, 0, b"b")
    cache.put_tile(3, 0, 0, b"c")
    assert cache.stats() == {"count": 3, "minzoom": 1, "maxzoom": 3}
    cache.close()


def test_stats_empty_cache_returns_none_zooms(tmp_path) -> None:
    cache = TileCache(str(tmp_path / "c.mbtiles"))
    assert cache.stats() == {"count": 0, "minzoom": None, "maxzoom": None}
    cache.close()


def test_metadata_roundtrip(tmp_path) -> None:
    cache = TileCache(str(tmp_path / "c.mbtiles"))
    assert cache.get_metadata("name") is None
    cache.set_metadata("name", "Corvus field cache")
    cache.set_metadata("format", "png")
    assert cache.get_metadata("name") == "Corvus field cache"
    assert cache.get_metadata("format") == "png"
    # set_metadata replaces; it does not append a duplicate row.
    cache.set_metadata("name", "renamed")
    assert cache.get_metadata("name") == "renamed"
    cache.close()


def test_bounds_parse(tmp_path) -> None:
    cache = TileCache(str(tmp_path / "c.mbtiles"))
    assert cache.bounds() is None
    cache.set_metadata("bounds", "13.3,52.4,13.6,52.6")
    assert cache.bounds() == (13.3, 52.4, 13.6, 52.6)
    cache.close()


def test_bounds_tolerates_spaces_and_rejects_malformed(tmp_path) -> None:
    cache = TileCache(str(tmp_path / "c.mbtiles"))
    cache.set_metadata("bounds", "13.3, 52.4, 13.6, 52.6")
    assert cache.bounds() == (13.3, 52.4, 13.6, 52.6)
    cache.set_metadata("bounds", "not,coords")
    assert cache.bounds() is None
    cache.close()


def test_tms_storage_form(tmp_path) -> None:
    # XYZ y=0 at z=1 maps to TMS row 1; the on-disk row must be TMS-flipped.
    cache = TileCache(str(tmp_path / "c.mbtiles"))
    cache.put_tile(1, 0, 0, b"k")
    cache.close()
    con = sqlite3.connect(str(tmp_path / "c.mbtiles"))
    row = con.execute(
        "SELECT tile_row FROM tiles WHERE zoom_level=1 AND tile_column=0"
    ).fetchone()
    con.close()
    assert row is not None
    assert row[0] == 1


def test_thread_safety_50_tiles_5_threads(tmp_path) -> None:
    cache = TileCache(str(tmp_path / "c.mbtiles"))
    blobs = {i: f"tile-{i}".encode() for i in range(50)}

    def writer(i: int) -> None:
        # z=10 is wide enough that x in 0..49 is an in-range slippy-map column.
        cache.put_tile(10, i, 0, blobs[i])

    with ThreadPoolExecutor(max_workers=5) as pool:
        list(pool.map(writer, range(50)))

    assert cache.stats()["count"] == 50
    # No corruption: every tile reads back its exact blob.
    for i in range(50):
        assert cache.get_tile(10, i, 0) == blobs[i]
    cache.close()


def test_close_then_reopen_persists_data(tmp_path) -> None:
    path = str(tmp_path / "c.mbtiles")
    blob = b"persistent-bytes"
    with TileCache(path) as cache:
        cache.put_tile(7, 5, 3, blob)
        cache.set_metadata("name", "persisted")
    reopened = TileCache(path)
    assert reopened.get_tile(7, 5, 3) == blob
    assert reopened.get_metadata("name") == "persisted"
    reopened.close()


def test_close_is_idempotent(tmp_path) -> None:
    cache = TileCache(str(tmp_path / "c.mbtiles"))
    cache.put_tile(2, 0, 0, b"x")
    cache.close()
    cache.close()  # second close must not raise


def test_context_manager_closes_on_exit(tmp_path) -> None:
    path = str(tmp_path / "c.mbtiles")
    with TileCache(path) as cache:
        cache.put_tile(8, 1, 1, b"ctx")
    # After the with-block the connection is closed; reopen confirms data.
    reopened = TileCache(path)
    assert reopened.get_tile(8, 1, 1) == b"ctx"
    reopened.close()
