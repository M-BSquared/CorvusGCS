//! The plugins area of the core: the plugin folders, their settings files, the
//! settings export and import, the update check and the small host services the
//! pages use (`plugin_registry.py`, `plugin_config.py`, `plugin_files.py`,
//! `update_check.py`, `file_manager.py`, `checklists.py`, `routes/settings.py`).
//!
//! [`PluginsModule`] is the one line an area adds to the core.

pub mod checklists;
pub mod config;
pub mod file_manager;
pub mod files;
pub mod registry;
mod routes;

pub use registry::PluginRegistry;
pub use routes::{registry, settings_all, Opener, PluginDirs};

use crate::module::{Module, Registry};

/// The routes and services of the plugins area.
#[derive(Default)]
pub struct PluginsModule {
    dirs: PluginDirs,
}

impl PluginsModule {
    pub fn new() -> PluginsModule {
        PluginsModule::default()
    }

    /// With the plugin folders (and what opens one) named, for a test or a
    /// second instance that must not touch the real ones.
    pub fn with_dirs(dirs: PluginDirs) -> PluginsModule {
        PluginsModule { dirs }
    }
}

impl Module for PluginsModule {
    fn name(&self) -> &'static str {
        "plugins"
    }

    fn register(&self, reg: &mut Registry) {
        reg.config_block("checklists", checklists::coerce);
        routes::register(reg, self.dirs.clone());
    }
}
