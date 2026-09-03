"""Offline MBTiles (SQLite) tile cache for full-field offline map use.

The browser never has internet in the field, so every tile the UI requests
must resolve locally. This module is the cache STORE only — MapLibre wiring
and the download UI live in the frontend. stdlib ``sqlite3`` only,
thread-safe so the backend's ``ThreadingHTTPServer`` can serve tiles from
many HTTP threads.

Alongside the MBTiles ``tiles``/``metadata`` tables the file carries one
Corvus-specific table, ``corvus_regions``: the named areas the operator has
pre-downloaded. Keeping them in the same file as the tiles they describe is
what makes the pairing self-consistent — delete the ``.mbtiles`` and its
region list goes with it, copy it to another machine and the names travel
along. MBTiles readers ignore tables they do not know, so the file stays a
valid MBTiles archive.
"""
from __future__ import annotations

import os
import pathlib
import sqlite3
import threading
import time
from typing import Iterable


def xyz_to_tms(z: int, y: int) -> int:
    """Convert an XYZ (slippy-map) row *y* at zoom *z* to a TMS (y-flipped) row."""
    return (1 << z) - 1 - y


# The columns a region record carries, with the default used when a caller
# omits one. Centralized so add_region can never write a partial row and the
# HTTP layer has one place to read the shape from.
_REGION_DEFAULTS: dict = {
    "id": "",
    "name": "Region",
    "source": "",
    "w": 0.0, "s": 0.0, "e": 0.0, "n": 0.0,
    "minzoom": 0,
    "maxzoom": 0,
    "tile_count": 0,
    "created_at": "",
    "state": "done",
}


def _clean_region(region: dict) -> dict:
    """Coerce *region* to the stored column set, filling in defaults.

    Never raises: a value that will not coerce falls back to its default, so a
    malformed record degrades to a harmless row instead of losing the download
    it describes. ``created_at`` defaults to now, in UTC ISO-8601.
    """
    src = region if isinstance(region, dict) else {}
    out: dict = {}
    for key, default in _REGION_DEFAULTS.items():
        value = src.get(key, default)
        try:
            if isinstance(default, float):
                out[key] = float(value)
            elif isinstance(default, int):
                out[key] = int(value)
            else:
                out[key] = str(value)
        except (TypeError, ValueError):
            out[key] = default
    if not out["created_at"]:
        out["created_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    if not out["name"]:
        out["name"] = _REGION_DEFAULTS["name"]
    return out


def default_cache_dir() -> str:
    """Return the conventional on-disk location for the tile cache.

    Honors an operator override via the CORVUS_TILE_CACHE_DIR env var (set
    from cfg.tile_cache_dir by serve.py/app.py) so the configured cache dir
    is no longer silently ignored; falls back to ~/.corvus/tiles. The
    directory is created elsewhere (lazily, by whoever first opens a cache
    there via mkdir(parents=True, exist_ok=True)), mirroring
    mavlink_bridge.default_log_dir; this helper only reports the path.
    """
    return os.environ.get("CORVUS_TILE_CACHE_DIR") or os.path.expanduser("~/.corvus/tiles")


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
            # Corvus extension (see the module docstring): the named areas the
            # operator pre-downloaded. CREATE IF NOT EXISTS means an .mbtiles
            # written by an older build gains the table on first open.
            self._conn.execute(
                """CREATE TABLE IF NOT EXISTS corvus_regions (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    source TEXT,
                    w REAL, s REAL, e REAL, n REAL,
                    minzoom INTEGER,
                    maxzoom INTEGER,
                    tile_count INTEGER,
                    created_at TEXT,
                    state TEXT
                )"""
            )
            self._conn.commit()

    def get_tile(self, z: int, x: int, y: int) -> bytes | None:
        """Return the cached tile blob for XYZ (z,x,y), or None if absent.

        Rows are stored in TMS (y-flipped) form per the MBTiles spec.
        """
        # Shutdown close-race: CorvusServer.shutdown() may close this
        # connection while an in-flight HTTP handler thread (daemon, not
        # joined) still calls get_tile. Bail to the safe default instead of
        # hitting sqlite3.ProgrammingError on a closed connection.
        if self._closed:
            return None
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
        if self._closed:
            return
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
        if self._closed:
            return False
        row_num = xyz_to_tms(z, y)
        with self._lock:
            cur = self._conn.execute(
                "SELECT 1 FROM tiles "
                "WHERE zoom_level=? AND tile_column=? AND tile_row=?",
                (z, x, row_num),
            )
            return cur.fetchone() is not None

    # ---- named regions (Corvus extension) ----
    #
    # A "region" is one named area the operator pre-downloaded: the bounds and
    # zoom span that were requested, plus how many tiles actually landed. It is
    # what lets the UI answer "which areas do I have offline?" instead of only
    # "how many tiles do I have", which is a number nobody can act on.

    def add_region(self, region: dict) -> dict:
        """Insert or replace a region record. Returns the stored record.

        Keyed by ``id`` (the caller supplies one — the download job id, so a
        progress update can find the row again). Unknown keys are dropped;
        missing ones take a benign default, so a partially-built record can
        never raise here and lose the download that produced it.
        """
        if self._closed:
            return dict(region)
        row = _clean_region(region)
        with self._lock:
            self._conn.execute(
                """INSERT OR REPLACE INTO corvus_regions
                   (id, name, source, w, s, e, n, minzoom, maxzoom,
                    tile_count, created_at, state)
                   VALUES (?,?,?,?,?,?,?,?,?,?,?,?)""",
                (row["id"], row["name"], row["source"],
                 row["w"], row["s"], row["e"], row["n"],
                 row["minzoom"], row["maxzoom"], row["tile_count"],
                 row["created_at"], row["state"]),
            )
            self._conn.commit()
        return row

    def update_region(self, region_id: str, **fields: object) -> bool:
        """Patch the named columns of one region. Returns True if it existed.

        Used when a download finishes: the row is written at job start (so the
        area shows up as in-progress) and patched with the real tile count and
        final state when the job ends.
        """
        if self._closed:
            return False
        allowed = {"name", "tile_count", "state"}
        sets = {k: v for k, v in fields.items() if k in allowed}
        if not sets:
            return False
        cols = ", ".join(f"{k}=?" for k in sets)
        with self._lock:
            cur = self._conn.execute(
                f"UPDATE corvus_regions SET {cols} WHERE id=?",
                (*sets.values(), region_id),
            )
            self._conn.commit()
            return cur.rowcount > 0

    def list_regions(self) -> list[dict]:
        """Return every region record, newest first."""
        if self._closed:
            return []
        with self._lock:
            cur = self._conn.execute(
                """SELECT id, name, source, w, s, e, n, minzoom, maxzoom,
                          tile_count, created_at, state
                   FROM corvus_regions ORDER BY created_at DESC, rowid DESC"""
            )
            rows = cur.fetchall()
        return [
            {
                "id": r[0], "name": r[1], "source": r[2],
                "bounds": {"w": r[3], "s": r[4], "e": r[5], "n": r[6]},
                "minzoom": r[7], "maxzoom": r[8],
                "tile_count": r[9], "created_at": r[10], "state": r[11],
            }
            for r in rows
        ]

    def get_region(self, region_id: str) -> dict | None:
        """Return one region record by id, or None."""
        for r in self.list_regions():
            if r["id"] == region_id:
                return r
        return None

    def remove_region(self, region_id: str) -> bool:
        """Delete a region record. Returns True if a row was removed.

        Only the record — the tiles stay cached, because regions overlap and
        another region may need them. :meth:`delete_region_tiles` is the
        explicit, separate step for reclaiming disk.
        """
        if self._closed:
            return False
        with self._lock:
            cur = self._conn.execute(
                "DELETE FROM corvus_regions WHERE id=?", (region_id,)
            )
            self._conn.commit()
            return cur.rowcount > 0

    def delete_tiles(self, tiles: "Iterable[tuple[int, int, int]]") -> int:
        """Delete the given XYZ ``(z, x, y)`` tiles. Returns the rows removed.

        Deleted in one transaction so a half-deleted region can never be left
        behind. XYZ→TMS conversion happens here, exactly as in put/get.
        """
        if self._closed:
            return 0
        removed = 0
        with self._lock:
            for z, x, y in tiles:
                cur = self._conn.execute(
                    "DELETE FROM tiles "
                    "WHERE zoom_level=? AND tile_column=? AND tile_row=?",
                    (z, x, xyz_to_tms(z, y)),
                )
                removed += cur.rowcount
            self._conn.commit()
        return removed

    def stats(self) -> dict:
        """Return {count, minzoom, maxzoom} from the tiles table.

        ``minzoom``/``maxzoom`` are None when the cache is empty.
        """
        if self._closed:
            return {"count": 0, "minzoom": None, "maxzoom": None}
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
        if self._closed:
            return
        with self._lock:
            self._conn.execute(
                "INSERT OR REPLACE INTO metadata (name, value) VALUES (?,?)",
                (name, value),
            )
            self._conn.commit()

    def get_metadata(self, name: str) -> str | None:
        """Return the metadata value for *name*, or None if absent."""
        if self._closed:
            return None
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
