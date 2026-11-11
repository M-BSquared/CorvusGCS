//! Offline MBTiles (SQLite) tile cache (`corvus/tile_cache.py`).
//!
//! The same file layout Python's `sqlite3` module reads and writes: MBTiles 1.3
//! `tiles` and `metadata` tables with TMS rows, WAL journal, plus the Corvus
//! table `corvus_regions` holding the named areas the operator downloaded, so a
//! region list travels with the tiles it describes. One shared connection is
//! guarded by a mutex; `close` folds the WAL back into the file and is
//! idempotent, and every method answers its safe default after it (an
//! in-flight handler thread may still call one while the core shuts down).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use super::util::{expand_user, iso_utc, json_float, json_int, json_str, normpath, now_secs};

/// An XYZ (slippy map) row to the TMS (flipped) row stored in the file.
pub fn xyz_to_tms(z: i64, y: i64) -> i64 {
    (1i64 << z) - 1 - y
}

/// One named area the operator downloaded.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub id: String,
    pub name: String,
    pub source: String,
    pub w: f64,
    pub s: f64,
    pub e: f64,
    pub n: f64,
    pub minzoom: i64,
    pub maxzoom: i64,
    pub tile_count: i64,
    pub created_at: String,
    pub state: String,
}

impl Region {
    /// A record coerced to the stored columns, defaults filling what is
    /// missing or will not coerce (`_clean_region`). Never fails.
    pub fn clean(raw: &Value) -> Region {
        let empty = serde_json::Map::new();
        let src = raw.as_object().unwrap_or(&empty);
        let text = |k: &str, d: &str| src.get(k).map_or_else(|| d.to_string(), json_str);
        let float = |k: &str| src.get(k).and_then(json_float).unwrap_or(0.0);
        let int = |k: &str| src.get(k).and_then(json_int).unwrap_or(0);
        let mut r = Region {
            id: text("id", ""),
            name: text("name", "Region"),
            source: text("source", ""),
            w: float("w"),
            s: float("s"),
            e: float("e"),
            n: float("n"),
            minzoom: int("minzoom"),
            maxzoom: int("maxzoom"),
            tile_count: int("tile_count"),
            created_at: text("created_at", ""),
            state: text("state", "done"),
        };
        if r.created_at.is_empty() {
            r.created_at = iso_utc(now_secs());
        }
        if r.name.is_empty() {
            r.name = "Region".into();
        }
        r
    }

    /// The shape `list_regions` and the routes answer.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "source": self.source,
            "bounds": {"w": self.w, "s": self.s, "e": self.e, "n": self.n},
            "minzoom": self.minzoom, "maxzoom": self.maxzoom,
            "tile_count": self.tile_count, "created_at": self.created_at, "state": self.state,
        })
    }

    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        (self.w, self.s, self.e, self.n)
    }
}

/// The conventional on-disk location of the tile cache: `CORVUS_TILE_CACHE_DIR`
/// when set, else `<state folder>/tiles`.
pub fn default_cache_dir(state: &Path) -> PathBuf {
    match std::env::var("CORVUS_TILE_CACHE_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(normpath(&expand_user(v.trim()))),
        _ => state.join("tiles"),
    }
}

/// A thread safe MBTiles 1.3 store backed by one SQLite file.
pub struct TileCache {
    path: PathBuf,
    conn: Mutex<Option<Connection>>,
}

impl TileCache {
    pub fn new(path: impl Into<PathBuf>) -> Result<TileCache, String> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(&path).map_err(|e| e.to_string())?;
        let cache = TileCache {
            path,
            conn: Mutex::new(Some(conn)),
        };
        cache.init_schema().map_err(|e| e.to_string())?;
        Ok(cache)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> MutexGuard<'_, Option<Connection>> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn init_schema(&self) -> rusqlite::Result<()> {
        let guard = self.lock();
        let c = guard.as_ref().expect("just opened");
        // NORMAL under WAL cannot corrupt the file, only lose the last few
        // transactions, and every row is a tile that can be fetched again.
        c.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        c.execute_batch(
            "PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS metadata (name TEXT, value TEXT);
             CREATE UNIQUE INDEX IF NOT EXISTS metadata_name_idx ON metadata(name);
             CREATE TABLE IF NOT EXISTS tiles (
                 zoom_level INTEGER,
                 tile_column INTEGER,
                 tile_row INTEGER,
                 tile_data BLOB
             );
             CREATE UNIQUE INDEX IF NOT EXISTS tiles_idx ON tiles(zoom_level, tile_column, tile_row);
             CREATE TABLE IF NOT EXISTS corvus_regions (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 source TEXT,
                 w REAL, s REAL, e REAL, n REAL,
                 minzoom INTEGER,
                 maxzoom INTEGER,
                 tile_count INTEGER,
                 created_at TEXT,
                 state TEXT
             );",
        )
    }

    /// The cached blob for XYZ (z, x, y), or `None`. An error is the caller's
    /// to treat as a miss.
    pub fn get_tile(&self, z: i64, x: i64, y: i64) -> Result<Option<Vec<u8>>, String> {
        let guard = self.lock();
        let Some(c) = guard.as_ref() else { return Ok(None) };
        c.query_row(
            "SELECT tile_data FROM tiles WHERE zoom_level=?1 AND tile_column=?2 AND tile_row=?3",
            params![z, x, xyz_to_tms(z, y)],
            |r| r.get::<_, Option<Vec<u8>>>(0),
        )
        .optional()
        .map(Option::flatten)
        .map_err(|e| e.to_string())
    }

    /// Inserts or replaces one tile; an empty blob is a no op.
    pub fn put_tile(&self, z: i64, x: i64, y: i64, blob: &[u8]) -> Result<(), String> {
        if blob.is_empty() {
            return Ok(());
        }
        let guard = self.lock();
        let Some(c) = guard.as_ref() else { return Ok(()) };
        c.execute(
            "INSERT OR REPLACE INTO tiles (zoom_level, tile_column, tile_row, tile_data) VALUES (?1,?2,?3,?4)",
            params![z, x, xyz_to_tms(z, y), blob],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// Inserts or replaces many `(z, x, y, blob)` tiles in one transaction and
    /// returns how many were written. An error means none were.
    pub fn put_tiles(&self, tiles: &[(i64, i64, i64, Vec<u8>)]) -> Result<usize, String> {
        let rows: Vec<_> = tiles.iter().filter(|t| !t.3.is_empty()).collect();
        if rows.is_empty() {
            return Ok(0);
        }
        let mut guard = self.lock();
        let Some(c) = guard.as_mut() else { return Ok(0) };
        let tx = c.transaction().map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO tiles (zoom_level, tile_column, tile_row, tile_data) VALUES (?1,?2,?3,?4)",
                )
                .map_err(|e| e.to_string())?;
            for (z, x, y, blob) in &rows {
                stmt.execute(params![z, x, xyz_to_tms(*z, *y), blob])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(rows.len())
    }

    pub fn has_tile(&self, z: i64, x: i64, y: i64) -> bool {
        let guard = self.lock();
        let Some(c) = guard.as_ref() else { return false };
        c.query_row(
            "SELECT 1 FROM tiles WHERE zoom_level=?1 AND tile_column=?2 AND tile_row=?3",
            params![z, x, xyz_to_tms(z, y)],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    }

    /// Inserts or replaces a region record and returns what was stored.
    pub fn add_region(&self, region: &Value) -> Region {
        let row = Region::clean(region);
        let guard = self.lock();
        if let Some(c) = guard.as_ref() {
            let _ = c.execute(
                "INSERT OR REPLACE INTO corvus_regions
                 (id, name, source, w, s, e, n, minzoom, maxzoom, tile_count, created_at, state)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    row.id,
                    row.name,
                    row.source,
                    row.w,
                    row.s,
                    row.e,
                    row.n,
                    row.minzoom,
                    row.maxzoom,
                    row.tile_count,
                    row.created_at,
                    row.state
                ],
            );
        }
        row
    }

    /// Patches `name`, `tile_count` or `state` of one region. `true` when it
    /// existed. Other keys are ignored.
    pub fn update_region(&self, region_id: &str, fields: &[(&str, Value)]) -> bool {
        let sets: Vec<&(&str, Value)> = fields
            .iter()
            .filter(|(k, _)| matches!(*k, "name" | "tile_count" | "state"))
            .collect();
        if sets.is_empty() {
            return false;
        }
        let cols = sets
            .iter()
            .map(|(k, _)| format!("{k}=?"))
            .collect::<Vec<_>>()
            .join(", ");
        let guard = self.lock();
        let Some(c) = guard.as_ref() else { return false };
        let mut values: Vec<rusqlite::types::Value> = sets
            .iter()
            .map(|(_, v)| match v {
                Value::Number(n) if n.is_i64() => rusqlite::types::Value::Integer(n.as_i64().unwrap_or(0)),
                Value::Number(n) => rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0)),
                Value::Null => rusqlite::types::Value::Null,
                other => rusqlite::types::Value::Text(json_str(other)),
            })
            .collect();
        values.push(rusqlite::types::Value::Text(region_id.to_string()));
        c.execute(
            &format!("UPDATE corvus_regions SET {cols} WHERE id=?"),
            rusqlite::params_from_iter(values),
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    /// Every region record, newest first.
    pub fn list_regions(&self) -> Vec<Region> {
        let guard = self.lock();
        let Some(c) = guard.as_ref() else { return Vec::new() };
        let Ok(mut stmt) = c.prepare(
            "SELECT id, name, source, w, s, e, n, minzoom, maxzoom, tile_count, created_at, state
             FROM corvus_regions ORDER BY created_at DESC, rowid DESC",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map([], |r| {
            Ok(Region {
                id: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                source: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                w: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                s: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                e: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                n: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                minzoom: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                maxzoom: r.get::<_, Option<i64>>(8)?.unwrap_or(0),
                tile_count: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                created_at: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
                state: r.get::<_, Option<String>>(11)?.unwrap_or_default(),
            })
        });
        match rows {
            Ok(it) => it.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn get_region(&self, region_id: &str) -> Option<Region> {
        self.list_regions().into_iter().find(|r| r.id == region_id)
    }

    /// Deletes the record only; the tiles stay, because regions overlap.
    pub fn remove_region(&self, region_id: &str) -> bool {
        let guard = self.lock();
        let Some(c) = guard.as_ref() else { return false };
        c.execute("DELETE FROM corvus_regions WHERE id=?1", params![region_id])
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    /// Deletes the given XYZ tiles in one transaction; returns the rows removed.
    pub fn delete_tiles(&self, tiles: &[(i64, i64, i64)]) -> usize {
        let mut guard = self.lock();
        let Some(c) = guard.as_mut() else { return 0 };
        let Ok(tx) = c.transaction() else { return 0 };
        let mut removed = 0;
        {
            let Ok(mut stmt) =
                tx.prepare_cached("DELETE FROM tiles WHERE zoom_level=?1 AND tile_column=?2 AND tile_row=?3")
            else {
                return 0;
            };
            for (z, x, y) in tiles {
                removed += stmt.execute(params![z, x, xyz_to_tms(*z, *y)]).unwrap_or(0);
            }
        }
        let _ = tx.commit();
        removed
    }

    /// The cached tiles inside XYZ `(z, x_min, x_max, y_min, y_max)` ranges,
    /// one indexed query per zoom.
    pub fn count_tiles(&self, ranges: &[(i64, i64, i64, i64, i64)]) -> i64 {
        let guard = self.lock();
        let Some(c) = guard.as_ref() else { return 0 };
        let mut total = 0;
        for (z, x_min, x_max, y_min, y_max) in ranges {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM tiles WHERE zoom_level=?1 AND tile_column BETWEEN ?2 AND ?3 AND tile_row BETWEEN ?4 AND ?5",
                    params![z, x_min, x_max, xyz_to_tms(*z, *y_max), xyz_to_tms(*z, *y_min)],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            total += n;
        }
        total
    }

    /// `{count, minzoom, maxzoom}`; the zooms are null when the cache is empty.
    pub fn stats(&self) -> Value {
        let guard = self.lock();
        let Some(c) = guard.as_ref() else {
            return json!({"count": 0, "minzoom": null, "maxzoom": null});
        };
        let row = c.query_row(
            "SELECT COUNT(*), MIN(zoom_level), MAX(zoom_level) FROM tiles",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            },
        );
        match row {
            Ok((count, min, max)) => json!({"count": count, "minzoom": min, "maxzoom": max}),
            Err(_) => json!({"count": 0, "minzoom": null, "maxzoom": null}),
        }
    }

    /// The `bounds` metadata ("w,s,e,n") as a tuple, or `None` when it is
    /// missing or malformed.
    pub fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let raw = self.get_metadata("bounds")?;
        if raw.is_empty() {
            return None;
        }
        let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
        if parts.len() != 4 {
            return None;
        }
        let f: Vec<f64> = parts.iter().filter_map(|p| super::util::py_float(p)).collect();
        (f.len() == 4).then(|| (f[0], f[1], f[2], f[3]))
    }

    pub fn set_metadata(&self, name: &str, value: &str) {
        let guard = self.lock();
        if let Some(c) = guard.as_ref() {
            let _ = c.execute(
                "INSERT OR REPLACE INTO metadata (name, value) VALUES (?1,?2)",
                params![name, value],
            );
        }
    }

    pub fn get_metadata(&self, name: &str) -> Option<String> {
        let guard = self.lock();
        let c = guard.as_ref()?;
        c.query_row("SELECT value FROM metadata WHERE name=?1", params![name], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()
        .ok()
        .flatten()
        .flatten()
    }

    /// Folds the WAL back into the file and closes the connection. Idempotent.
    pub fn close(&self) {
        let mut guard = self.lock();
        if let Some(c) = guard.take() {
            let _ = c.pragma_update_and_check(None, "wal_checkpoint", "TRUNCATE", |_| Ok(()));
            let _ = c.close();
        }
    }
}

impl Drop for TileCache {
    fn drop(&mut self) {
        self.close();
    }
}
