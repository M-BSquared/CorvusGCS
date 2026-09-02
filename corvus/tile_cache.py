"""Offline MBTiles (SQLite) tile cache for full-field offline map use.

The browser never has internet in the field, so every tile the UI requests
must resolve locally. This module is the cache STORE only — MapLibre wiring
and the download UI live in the frontend. stdlib ``sqlite3`` only,
thread-safe so the backend's ``ThreadingHTTPServer`` can serve tiles from
many HTTP threads.
"""
from __future__ import annotations

import os
import pathlib
import sqlite3
import threading


def xyz_to_tms(z: int, y: int) -> int:
    """Convert an XYZ (slippy-map) row *y* at zoom *z* to a TMS (y-flipped) row."""
    return (1 << z) - 1 - y


def default_cache_dir() -> str:
    """Return the conventional on-disk location for the tile cache.

    The directory is created elsewhere (lazily, by whoever first opens a
    cache there); this helper only reports the path string.
    """
    return os.path.expanduser("~/.corvus/tiles")


class TileCache:
    """Thread-safe MBTiles 1.3 tile store backed by a single SQLite file.

    One shared connection (``check_same_thread=False``) is guarded by an
    :class:`threading.RLock`. The lock serializes writers and protects the
    connection object; ``journal_mode=WAL`` lets other connections read
    concurrently while a write is in flight. An RLock
    (not a plain Lock) is required so a public method such as :meth:`bounds`
    may call another locked method (:meth:`get_metadata`) without self-deadlock.
    """

    def __init__(self, path: str) -> None:
        self._path = path
        # Parent dir must exist before sqlite3 can create the file.
        pathlib.Path(path).parent.mkdir(parents=True, exist_ok=True)
        # check_same_thread=False: the backend serves tiles from many HTTP
        # threads through this one shared connection; the RLock serializes use.
        self._conn = sqlite3.connect(path, check_same_thread=False)
        self._lock = threading.RLock()
        self._closed = False
        self._init_schema()

    def _init_schema(self) -> None:
        with self._lock:
            self._conn.execute("PRAGMA journal_mode=WAL")
            self._conn.execute(
                "CREATE TABLE IF NOT EXISTS metadata (name TEXT, value TEXT)"
            )
            self._conn.execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS metadata_name_idx ON metadata(name)"
            )
            self._conn.execute(
                """CREATE TABLE IF NOT EXISTS tiles (
                    zoom_level INTEGER,
                    tile_column INTEGER,
                    tile_row INTEGER,
                    tile_data BLOB
                )"""
            )
            self._conn.execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS tiles_idx "
                "ON tiles(zoom_level, tile_column, tile_row)"
            )
            self._conn.commit()

    def get_tile(self, z: int, x: int, y: int) -> bytes | None:
        """Return the cached tile blob for XYZ (z,x,y), or None if absent.

        Rows are stored in TMS (y-flipped) form per the MBTiles spec.
        """
        row_num = xyz_to_tms(z, y)
        with self._lock:
            cur = self._conn.execute(
                "SELECT tile_data FROM tiles "
                "WHERE zoom_level=? AND tile_column=? AND tile_row=?",
                (z, x, row_num),
            )
            result = cur.fetchone()
        return result[0] if result is not None else None

    def put_tile(self, z: int, x: int, y: int, blob: bytes) -> None:
        """Insert or replace the tile for XYZ (z,x,y). No-op on an empty blob."""
        if not blob:
            return
        row_num = xyz_to_tms(z, y)
        with self._lock:
            self._conn.execute(
                "INSERT OR REPLACE INTO tiles "
                "(zoom_level, tile_column, tile_row, tile_data) VALUES (?,?,?,?)",
                (z, x, row_num, blob),
            )
            self._conn.commit()

    def has_tile(self, z: int, x: int, y: int) -> bool:
        """Return True if a tile for XYZ (z,x,y) is present in the cache."""
        row_num = xyz_to_tms(z, y)
        with self._lock:
            cur = self._conn.execute(
                "SELECT 1 FROM tiles "
                "WHERE zoom_level=? AND tile_column=? AND tile_row=?",
                (z, x, row_num),
            )
            return cur.fetchone() is not None

    def stats(self) -> dict:
        """Return {count, minzoom, maxzoom} from the tiles table.

        ``minzoom``/``maxzoom`` are None when the cache is empty.
        """
        with self._lock:
            cur = self._conn.execute(
                "SELECT COUNT(*), MIN(zoom_level), MAX(zoom_level) FROM tiles"
            )
            count, minzoom, maxzoom = cur.fetchone()
        return {
            "count": count or 0,
            "minzoom": minzoom,
            "maxzoom": maxzoom,
        }

    def bounds(self) -> tuple[float, float, float, float] | None:
        """Parse the ``bounds`` metadata key ("w,s,e,n") into a (w,s,e,n) tuple.

        Returns None if the key is missing or malformed.
        """
        raw = self.get_metadata("bounds")
        if not raw:
            return None
        parts = [p.strip() for p in raw.split(",")]
        if len(parts) != 4:
            return None
        try:
            w, s, e, n = (float(parts[0]), float(parts[1]),
                          float(parts[2]), float(parts[3]))
        except ValueError:
            return None
        return (w, s, e, n)

    def set_metadata(self, name: str, value: str) -> None:
        """Insert or replace a metadata row keyed by *name*."""
        with self._lock:
            self._conn.execute(
                "INSERT OR REPLACE INTO metadata (name, value) VALUES (?,?)",
                (name, value),
            )
            self._conn.commit()

    def get_metadata(self, name: str) -> str | None:
        """Return the metadata value for *name*, or None if absent."""
        with self._lock:
            cur = self._conn.execute(
                "SELECT value FROM metadata WHERE name=?", (name,)
            )
            result = cur.fetchone()
        return result[0] if result is not None else None

    def close(self) -> None:
        """Close the connection and flush the WAL. Idempotent (safe to re-call)."""
        with self._lock:
            if self._closed:
                return
            self._closed = True
            try:
                # Fold the WAL back into the main file so a clean shutdown
                # persists data and a later reopen sees it without a sidecar.
                self._conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            except Exception:
                pass  # best-effort flush; never raise in the shutdown path
            try:
                self._conn.close()
            except Exception:
                pass

    def __enter__(self) -> "TileCache":
        return self

    def __exit__(self, exc_type: object, exc_val: object, exc_tb: object) -> None:
        self.close()
