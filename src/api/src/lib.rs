//! The part of the Corvus GCS interface that plugins build on: the widgets
//! and look of the interface (`ui`), the translation layer (`i18n`), the
//! backend client (`net`) and the number and time helpers (`data`). The
//! program re-exports these, so inside it they are still `crate::ui` and so on.

pub mod bridge;
pub mod data;
#[cfg(target_family = "wasm")]
#[doc(hidden)]
pub mod guest;
pub mod i18n;
pub mod net;
pub mod plugin;
pub mod task;
pub mod ui;
pub mod wire;

/// The egui the widgets are built on. A plugin takes it from here, so it cannot use another one.
pub use egui;

/// Makes a crate a plugin: `export_plugin!(plugin, LANG)` beside its
/// `pub fn plugin() -> Box<dyn Plugin>`, its `pub const LANG` and its `plugin.json`.
/// Built for `wasm32-wasip1` the crate is a module Corvus GCS loads; built for
/// this computer (its tests) it is an ordinary library.
#[macro_export]
macro_rules! export_plugin {
    ($make:path, $lang:path) => {
        #[cfg(target_family = "wasm")]
        mod __corvus_export {
            use super::*;

            const MANIFEST: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.json"));

            // A static in a section of its own becomes a custom section of the module, which the
            // program reads without running any of it.
            #[used]
            #[link_section = "corvus.manifest"]
            static SECTION: [u8; MANIFEST.len()] = {
                let mut out = [0; MANIFEST.len()];
                let mut i = 0;
                while i < out.len() {
                    out[i] = MANIFEST[i];
                    i += 1;
                }
                out
            };

            #[no_mangle]
            pub extern "C" fn corvus_alloc(len: u32) -> u32 {
                $crate::guest::alloc(len)
            }

            #[no_mangle]
            pub extern "C" fn corvus_call(ptr: u32, len: u32) -> u64 {
                $crate::guest::call(ptr, len, $make, $lang)
            }
        }
    };
}
