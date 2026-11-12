//! The repository's own tools, in Rust: what the build scripts, the manual and
//! the language files need besides cargo. `corvus-build` is the command line;
//! the modules are a library so the tests in `tests/` use the same code.

pub mod appimage;
pub mod bundle;
pub mod elf;
pub mod guide;
pub mod html;
pub mod i18n;
pub mod icon;
pub mod soak;

use std::path::PathBuf;

/// The repository root: two folders above this crate.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}
