//! Where Corvus keeps its state: `~/.corvus` (`paths.py`).

use std::path::{Path, PathBuf};

pub const CORVUS_DIR_NAME: &str = ".corvus";

/// The folder state lives in. `Paths::system()` is the user's `~/.corvus`;
/// a test or a second instance points it elsewhere with `Paths::at`.
#[derive(Debug, Clone)]
pub struct Paths {
    home: PathBuf,
}

impl Paths {
    /// `~/.corvus` of the current user.
    pub fn system() -> Paths {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Paths::at(home.join(CORVUS_DIR_NAME))
    }

    /// A state folder at `dir` itself (not its parent).
    pub fn at(dir: impl Into<PathBuf>) -> Paths {
        Paths { home: dir.into() }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn join(&self, part: &str) -> PathBuf {
        self.home.join(part)
    }

    pub fn config(&self) -> PathBuf {
        self.join("config.json")
    }

    pub fn lock(&self) -> PathBuf {
        self.join("corvus.lock")
    }

    pub fn languages(&self) -> PathBuf {
        self.join("languages")
    }

    pub fn mcp_token(&self) -> PathBuf {
        self.join("mcp-token")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_sit_in_the_state_folder() {
        let p = Paths::at("/x/.corvus");
        assert_eq!(p.config(), Path::new("/x/.corvus/config.json"));
        assert_eq!(p.lock(), Path::new("/x/.corvus/corvus.lock"));
        assert!(Paths::system().home().ends_with(".corvus"));
    }
}
