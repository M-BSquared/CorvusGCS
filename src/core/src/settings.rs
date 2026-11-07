//! The live configuration: loaded once, changed under one lock, saved
//! atomically (`_config_write_lock`, `_live_config`, `_save_live_config` in
//! `server.py`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::config::{self, Coercers, CorvusConfig};
use crate::core::Core;
use crate::json::Map;

/// Called with the config after every change, to hand a new value to the
/// thing that runs on it (the battery estimator, the Remote ID identity, the
/// auto-connect watcher). Runs under the write lock; must not call back into
/// `update`.
pub type ConfigListener = Arc<dyn Fn(&Core, &CorvusConfig) + Send + Sync>;

pub struct Settings {
    path: PathBuf,
    current: RwLock<Arc<CorvusConfig>>,
    coercers: RwLock<Coercers>,
    listeners: RwLock<Vec<ConfigListener>>,
    write: Mutex<()>,
    first_start: AtomicBool,
}

impl Settings {
    pub(crate) fn new(path: PathBuf) -> Settings {
        Settings {
            path,
            current: RwLock::new(Arc::new(CorvusConfig::default())),
            coercers: RwLock::new(Coercers::default()),
            listeners: RwLock::new(Vec::new()),
            write: Mutex::new(()),
            first_start: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_coercers(&self, c: Coercers) {
        *self.coercers.write().unwrap_or_else(|e| e.into_inner()) = c;
    }

    pub(crate) fn set_listeners(&self, l: Vec<ConfigListener>) {
        *self.listeners.write().unwrap_or_else(|e| e.into_inner()) = l;
    }

    /// Reads the file. No file yet is a first start, which stays pending
    /// until the first-start setup is finished or skipped.
    pub(crate) fn load(&self) {
        self.first_start.store(!self.path.exists(), Ordering::SeqCst);
        let cfg = config::load(&self.path, &self.coercers());
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(cfg);
    }

    pub fn coercers(&self) -> Coercers {
        self.coercers.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The config in force. A snapshot: it does not change under the reader.
    pub fn get(&self) -> Arc<CorvusConfig> {
        self.current.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn notify(&self, core: &Core, cfg: &CorvusConfig) {
        let listeners = self.listeners.read().unwrap_or_else(|e| e.into_inner()).clone();
        for l in listeners {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| l(core, cfg))).is_err() {
                eprintln!("corvus: a config listener failed");
            }
        }
    }

    /// Changes the config as one read-modify-write: nothing else changes it
    /// meanwhile, and what is saved is never half old and half new. `f`
    /// edits a copy; the result is saved and listeners are told. A failed
    /// write is logged and returned, and the change stays in force for this
    /// session.
    pub fn update<F>(&self, core: &Core, f: F) -> std::io::Result<()>
    where
        F: FnOnce(&mut CorvusConfig),
    {
        let _w = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = (*self.get()).clone();
        f(&mut cfg);
        self.commit(core, cfg)
    }

    /// `update` for a caller with no `Core` at hand (a worker thread saving
    /// the vehicle list): saved the same way, and the listeners are not told.
    pub fn update_quiet<F>(&self, f: F) -> std::io::Result<()>
    where
        F: FnOnce(&mut CorvusConfig),
    {
        let _w = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = (*self.get()).clone();
        f(&mut cfg);
        let cfg = Arc::new(cfg);
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = cfg.clone();
        let saved = config::save(&cfg, &self.path);
        if let Err(e) = &saved {
            eprintln!("corvus: config save to {} failed: {e}", self.path.display());
        }
        saved
    }

    fn commit(&self, core: &Core, cfg: CorvusConfig) -> std::io::Result<()> {
        let cfg = Arc::new(cfg);
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = cfg.clone();
        let saved = config::save(&cfg, &self.path);
        if let Err(e) = &saved {
            eprintln!("corvus: config save to {} failed: {e}", self.path.display());
        }
        self.notify(core, &cfg);
        saved
    }

    /// `POST /api/config`: a partial update, validated, merged, saved. The
    /// new config as a response may carry it, or the refusal text.
    pub fn apply_partial(&self, core: &Core, partial: &Map) -> Result<Map, String> {
        let _w = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let next = config::apply_partial(&self.get(), partial, &self.coercers())?;
        let public = next.to_public();
        let _ = self.commit(core, next);
        Ok(public)
    }

    /// Writes the config as it is, even when nothing changed: a config file
    /// on disk is what tells the next start this station has been set up.
    pub fn save(&self) -> std::io::Result<()> {
        let _w = self.write.lock().unwrap_or_else(|e| e.into_inner());
        config::save(&self.get(), &self.path)
    }

    pub fn first_start_pending(&self) -> bool {
        self.first_start.load(Ordering::SeqCst)
    }

    pub fn finish_first_start(&self) {
        self.first_start.store(false, Ordering::SeqCst);
    }
}
