//! The system file dialog, so a person picks a file or a folder instead of
//! typing its path. It blocks the frame while it is open, as the platform's
//! own dialog does, and answers with the chosen path or nothing when it is
//! cancelled.

use std::path::{Path, PathBuf};

/// One kind of file the dialog offers: its label and extensions without dots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Filter<'a> {
    pub name: &'a str,
    pub extensions: &'a [&'a str],
}

fn dialog(title: &str, start: Option<&Path>) -> rfd::FileDialog {
    let mut d = rfd::FileDialog::new().set_title(crate::i18n::tr(title).as_ref());
    if let Some(dir) = start {
        d = d.set_directory(dir);
    }
    d
}

/// Asks for one file; the title is the dialog's own. It opens in `start`.
pub fn pick_file(title: &str, filter: Option<Filter<'_>>, start: Option<&Path>) -> Option<PathBuf> {
    let mut d = dialog(title, start);
    if let Some(f) = filter {
        d = d.add_filter(crate::i18n::tr(f.name).as_ref(), f.extensions);
    }
    d.pick_file()
}

/// Asks for one folder. It opens in `start`.
pub fn pick_folder(title: &str, start: Option<&Path>) -> Option<PathBuf> {
    dialog(title, start).pick_folder()
}

/// Asks for the path of a file to write, suggesting `name`. It opens in `start`.
pub fn save_file(title: &str, name: &str, filter: Option<Filter<'_>>, start: Option<&Path>) -> Option<PathBuf> {
    let mut d = dialog(title, start).set_file_name(name);
    if let Some(f) = filter {
        d = d.add_filter(crate::i18n::tr(f.name).as_ref(), f.extensions);
    }
    d.save_file()
}

/// The file's name as the backend stores it (`logo.png`), whatever the folder.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or(String::new(), |n| n.to_string_lossy().into_owned())
}

/// `~/rest` with the home folder written out; any other path as it is.
/// `home` is the user's home folder, when the system names one.
pub fn expand_home_with(path: &str, home: Option<&str>) -> String {
    let rest = if path == "~" {
        Some("")
    } else {
        path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\"))
    };
    match (rest, home) {
        (Some(rest), Some(home)) if !home.is_empty() => {
            if rest.is_empty() {
                home.to_string()
            } else {
                format!("{}/{rest}", home.trim_end_matches(['/', '\\']))
            }
        }
        _ => path.to_string(),
    }
}

/// [`expand_home_with`] for this computer's home folder.
pub fn expand_home(path: &str) -> String {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok();
    expand_home_with(path, home.as_deref())
}

/// The folder a dialog should open in for the path now held: the folder
/// itself, or the folder of a file, or the nearest one that exists above a
/// path that is not there yet. Nothing for an empty or unusable value.
pub fn start_folder(value: &str) -> Option<PathBuf> {
    let value = value.trim().trim_matches('"');
    if value.is_empty() {
        return None;
    }
    let path = PathBuf::from(expand_home(value));
    if path.is_dir() {
        return Some(path);
    }
    path.ancestors()
        .skip(1)
        .find(|a| !a.as_os_str().is_empty() && a.is_dir())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_is_the_last_part_of_the_path() {
        assert_eq!(
            file_name(Path::new("/home/pilot/Pictures/acme logo.png")),
            "acme logo.png"
        );
        assert_eq!(file_name(Path::new("")), "");
    }

    #[test]
    fn a_home_prefix_is_written_out() {
        let home = Some("/home/pilot");
        assert_eq!(expand_home_with("~/Downloads", home), "/home/pilot/Downloads");
        assert_eq!(expand_home_with("~", home), "/home/pilot");
        assert_eq!(expand_home_with("/opt/x", home), "/opt/x");
        assert_eq!(expand_home_with("~other/x", home), "~other/x");
        assert_eq!(expand_home_with("~/x", None), "~/x");
        assert_eq!(expand_home_with("~/x", Some("/home/pilot/")), "/home/pilot/x");
    }

    #[test]
    fn the_dialog_starts_where_the_value_is() {
        let tmp = std::env::temp_dir();
        let tmp_s = tmp.to_string_lossy().into_owned();
        assert_eq!(start_folder(""), None);
        assert_eq!(start_folder("   "), None);
        assert_eq!(start_folder(&tmp_s), Some(tmp.clone()));
        let file = tmp.join("corvus-no-such-file.txt");
        assert_eq!(start_folder(&file.to_string_lossy()), Some(tmp.clone()));
        let deep = tmp.join("corvus-no-such-dir").join("deeper").join("f.px4");
        assert_eq!(start_folder(&deep.to_string_lossy()), Some(tmp.clone()));
        assert_eq!(start_folder(&format!("\"{tmp_s}\"")), Some(tmp));
    }
}
