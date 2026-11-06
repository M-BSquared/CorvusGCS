//! The system file dialog, so a person picks a file instead of typing its
//! path. It blocks the frame while it is open, as the platform's own dialog
//! does, and answers with the chosen path or nothing when it is cancelled.

use std::path::PathBuf;

/// One kind of file the dialog offers: its label and extensions without dots.
#[derive(Debug, Clone, Copy)]
pub struct Filter<'a> {
    pub name: &'a str,
    pub extensions: &'a [&'a str],
}

/// Asks for one file; the title is the dialog's own.
pub fn pick_file(title: &str, filter: Option<Filter<'_>>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    if let Some(f) = filter {
        dialog = dialog.add_filter(f.name, f.extensions);
    }
    dialog.pick_file()
}

/// The file's name as the backend stores it (`logo.png`), whatever the folder.
pub fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map_or(String::new(), |n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_is_the_last_part_of_the_path() {
        assert_eq!(
            file_name(std::path::Path::new("/home/pilot/Pictures/acme logo.png")),
            "acme logo.png"
        );
        assert_eq!(file_name(std::path::Path::new("")), "");
    }
}
