//! The file dialog as a plugin has it: the program shows it, and the plugin
//! can read what the operator picked there and nothing else. A plugin has no
//! file system of its own.

use std::path::{Path, PathBuf};

use crate::ui::filepicker::Filter;
use crate::wire;

fn pick(mode: &str, title: &str, name: &str, filter: Option<Filter<'_>>, start: Option<&Path>) -> Option<PathBuf> {
    let told = super::host::ask(&wire::Query::PickPath(wire::PickPath {
        mode: mode.into(),
        title: title.into(),
        name: name.into(),
        filter: filter.map(|f| {
            (
                f.name.to_string(),
                f.extensions.iter().map(|e| e.to_string()).collect(),
            )
        }),
        start: start.map(|p| p.to_string_lossy().into_owned()),
    }));
    told.path.map(PathBuf::from)
}

pub fn pick_file(title: &str, filter: Option<Filter<'_>>, start: Option<&Path>) -> Option<PathBuf> {
    pick("file", title, "", filter, start)
}

pub fn pick_folder(title: &str, start: Option<&Path>) -> Option<PathBuf> {
    pick("folder", title, "", None, start)
}

pub fn save_file(title: &str, name: &str, filter: Option<Filter<'_>>, start: Option<&Path>) -> Option<PathBuf> {
    pick("save", title, name, filter, start)
}

pub fn read(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let told = super::host::ask(&wire::Query::ReadPicked(wire::ReadPicked {
        path: path.to_string_lossy().into_owned(),
        max,
    }));
    match told.bytes {
        Some(b) => Ok(b.into_vec()),
        None => Err(told.error.unwrap_or_else(|| "the file cannot be read".into())),
    }
}
