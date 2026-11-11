//! The parameter metadata fetch of one link (`ParamMetadataMixin` of
//! `param_metadata.py`): one background read per link cycle of the file the
//! autopilot keeps on board, over MAVLink FTP, in the states idle, loading,
//! ready and unavailable.
//!
//! A reset (a new link, a stop) bumps the generation and cancels the transfer,
//! so a fetch that outlived its cycle cannot publish metadata for a vehicle that
//! is no longer there.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::param_metadata::{fetch_error_text, parse_param_metadata, ParamMeta, ParamMetadataCache};
use crate::bridge::Bridge;
use crate::protocol::ftp::{Cancel, FtpError, FTP_ERR_FILE_NOT_FOUND};

struct Inner {
    state: &'static str,
    error: String,
    meta: ParamMeta,
    progress: (u64, u64),
    source: &'static str,
    generation: u64,
}

/// The state of one link's metadata fetch.
pub struct MetadataFetch {
    inner: Mutex<Inner>,
    cancel: Mutex<Cancel>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Default for MetadataFetch {
    fn default() -> Self {
        MetadataFetch::new()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn join_within(handle: Option<JoinHandle<()>>, limit: Duration) -> Option<JoinHandle<()>> {
    let handle = handle?;
    let deadline = Instant::now() + limit;
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    if handle.is_finished() {
        let _ = handle.join();
        None
    } else {
        Some(handle)
    }
}

impl MetadataFetch {
    pub fn new() -> MetadataFetch {
        MetadataFetch {
            inner: Mutex::new(Inner {
                state: "idle",
                error: String::new(),
                meta: ParamMeta::new(),
                progress: (0, 0),
                source: "",
                generation: 0,
            }),
            cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
            thread: Mutex::new(None),
        }
    }

    /// `{state, error, received, size, count, source}`, plus `params` when asked
    /// and ready.
    pub fn status(&self, include_params: bool) -> Value {
        Self::status_of(&lock(&self.inner), include_params)
    }

    fn status_of(inner: &Inner, include_params: bool) -> Value {
        let mut out = json!({
            "state": inner.state, "error": inner.error,
            "received": inner.progress.0, "size": inner.progress.1,
            "count": inner.meta.len(), "source": inner.source,
        });
        if include_params && inner.state == "ready" {
            out["params"] = Value::Object(inner.meta.clone());
        }
        out
    }

    /// Forgets the metadata and cancels a fetch; a new link may be a new vehicle.
    pub fn reset(&self) {
        {
            let mut inner = lock(&self.inner);
            inner.generation += 1;
            inner.state = "idle";
            inner.error = String::new();
            inner.meta = ParamMeta::new();
            inner.progress = (0, 0);
            inner.source = "";
        }
        lock(&self.cancel).store(true, Ordering::SeqCst);
    }

    /// Cancels and joins the fetch thread; part of the bridge's stop.
    pub fn stop(&self) {
        self.reset();
        let handle = lock(&self.thread).take();
        if let Some(left) = join_within(handle, Duration::from_secs(2)) {
            *lock(&self.thread) = Some(left);
        }
    }

    /// Starts reading the vehicle's metadata in the background. A no-op while a
    /// fetch runs or once it has succeeded for this link; a failed one is tried
    /// again. `cache` is ignored for a stack whose file is not fixed per build.
    pub fn start(&self, bridge: &Bridge, cache: Option<ParamMetadataCache>) -> Value {
        let dialect = bridge.dialect();
        let busy = |inner: &Inner| matches!(inner.state, "loading" | "ready");
        {
            let inner = lock(&self.inner);
            if busy(&inner) {
                return Self::status_of(&inner, false);
            }
        }
        let previous = lock(&self.thread).take();
        let previous = join_within(previous, Duration::from_secs(1));
        let mut inner = lock(&self.inner);
        if busy(&inner) {
            *lock(&self.thread) = previous;
            return Self::status_of(&inner, false);
        }
        if dialect.param_metadata_path().is_empty() {
            inner.state = "unavailable";
            inner.error = "this autopilot does not publish parameter defaults".into();
            *lock(&self.thread) = previous;
            return Self::status_of(&inner, false);
        }
        if !bridge.is_connected() {
            inner.error = "not connected".into();
            *lock(&self.thread) = previous;
            return Self::status_of(&inner, false);
        }
        if previous.is_some() {
            inner.error = "the previous transfer is still closing".into();
            *lock(&self.thread) = previous;
            return Self::status_of(&inner, false);
        }
        inner.generation += 1;
        let generation = inner.generation;
        inner.state = "loading";
        inner.error = String::new();
        inner.progress = (0, 0);
        inner.source = "";
        let cancel: Cancel = Arc::new(AtomicBool::new(false));
        *lock(&self.cancel) = Arc::clone(&cancel);
        let use_cache = if dialect.param_metadata_cacheable() {
            cache
        } else {
            None
        };
        let (path, fmt) = (dialect.param_metadata_path(), dialect.param_metadata_format());
        let me = bridge.clone();
        *lock(&self.thread) = std::thread::Builder::new()
            .name("param-metadata".into())
            .spawn(move || {
                me.metadata_fetch()
                    .worker(&me, generation, path, fmt, &cancel, use_cache)
            })
            .ok();
        Self::status_of(&inner, false)
    }

    fn worker(
        &self,
        bridge: &Bridge,
        generation: u64,
        path: &str,
        fmt: &str,
        cancel: &Cancel,
        cache: Option<ParamMetadataCache>,
    ) {
        let progress = |received: u64, size: u64| {
            let mut inner = lock(&self.inner);
            if generation == inner.generation {
                inner.progress = (received, size);
            }
        };
        let mut meta = ParamMeta::new();
        let mut error = String::new();
        let mut source = "vehicle";
        match self.fetch(bridge, path, fmt, cancel, cache.as_ref(), &progress) {
            Ok((found, from)) => {
                if !found.values().any(|m| m.get("default").is_some()) {
                    error = "the vehicle reports no parameter defaults".into();
                }
                meta = found;
                source = from;
            }
            Err(e) => error = e,
        }
        let mut inner = lock(&self.inner);
        if generation != inner.generation {
            return;
        }
        if error.is_empty() {
            inner.state = "ready";
            inner.meta = meta;
            inner.source = source;
        } else {
            inner.state = "unavailable";
            inner.error = error;
            inner.meta = ParamMeta::new();
        }
    }

    fn fetch(
        &self,
        bridge: &Bridge,
        path: &str,
        fmt: &str,
        cancel: &Cancel,
        cache: Option<&ParamMetadataCache>,
        progress: &(dyn Fn(u64, u64) + Sync),
    ) -> Result<(ParamMeta, &'static str), String> {
        let ftp = |e: FtpError| fetch_error_text(e.code, &e.message);
        let crc = match cache {
            Some(_) => Self::checksum(bridge, path, cancel).map_err(ftp)?,
            None => None,
        };
        if let (Some(cache), Some(crc)) = (cache, crc) {
            if let Some(raw) = cache.load(fmt, crc) {
                match parse_param_metadata(fmt, &raw) {
                    Ok(meta) => return Ok((meta, "cache")),
                    Err(_) => cache.discard(fmt, crc),
                }
            }
        }
        let raw = bridge
            .ftp_read_file_with(path, Some(progress), Some(Arc::clone(cancel)))
            .map_err(ftp)?;
        let meta =
            parse_param_metadata(fmt, &raw).map_err(|e| format!("could not read the parameter metadata: {e}"))?;
        if let (Some(cache), Some(crc)) = (cache, crc) {
            cache.store(fmt, crc, &raw);
        }
        Ok((meta, "vehicle"))
    }

    /// The vehicle's CRC32 of its metadata file, or None to skip the copies. A
    /// missing file or a cancel is passed on: asking twice would only double the
    /// wait. Zero is refused as a key, because a firmware that answers it for
    /// every file would make every copy match.
    fn checksum(bridge: &Bridge, path: &str, cancel: &Cancel) -> Result<Option<u32>, FtpError> {
        match bridge.ftp_file_crc32_with(path, Some(Arc::clone(cancel))) {
            Ok(crc) => Ok((crc != 0).then_some(crc)),
            Err(e) if e.code == FTP_ERR_FILE_NOT_FOUND || e.message == "cancelled" => Err(e),
            Err(_) => Ok(None),
        }
    }
}
