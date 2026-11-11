//! The plugins area of the core: the plugin folders, their settings files, the
//! settings export and import, the update check and the small host services the
//! pages use (`plugin_registry.py`, `plugin_config.py`, `plugin_files.py`,
//! `update_check.py`, `file_manager.py`, `checklists.py`, `routes/settings.py`).
//!
//! [`PluginsModule`] is the one line an area adds to the core.

pub mod checklists;
pub mod file_manager;

use crate::module::{Module, Registry};

/// The routes and services of the plugins area.
#[derive(Default)]
pub struct PluginsModule;

impl PluginsModule {
    pub fn new() -> PluginsModule {
        PluginsModule
    }
}

impl Module for PluginsModule {
    fn name(&self) -> &'static str {
        "plugins"
    }

    fn register(&self, reg: &mut Registry) {
        reg.config_block("checklists", checklists::coerce);
    }
}
