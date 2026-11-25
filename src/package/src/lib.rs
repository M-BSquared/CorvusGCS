//! What a plugin package is, as the program that installs it and the tool that
//! builds it both need to agree.
//!
//! Each plugin is a repository of its own. A release of it is named after what its
//! libraries fit ([`release_tag`]) and not after a version of Corvus GCS, so every
//! version with the same plugin interface finds it. It carries one `plugins-index.json`
//! and, beside it, one library per platform. The index lists the plugin with the file
//! name, size and SHA-256 of its library for every platform it was built for. The
//! program reads the index over HTTPS from the release for its own interface and checks
//! the size and hash of every file it downloads before it writes anything where a
//! library could be loaded from. There is no signing key: what is trusted is the
//! plugin's repository on GitHub.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// The version of the index layout.
pub const INDEX_FORMAT: u64 = 1;

/// The file name of the index in a release.
pub const INDEX_FILE: &str = "plugins-index.json";

/// The release of a plugin's repository that holds the libraries for programs of stamp `abi`.
/// A new version of Corvus GCS with the same stamp loads what is there already.
pub fn release_tag(abi: &str) -> String {
    let sum = sha256_hex(abi.as_bytes());
    format!("abi-{}", &sum[..16])
}

/// What this computer is called in an index: `macos-aarch64`, `windows-x86_64`, `linux-x86_64`.
pub fn platform() -> String {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

/// The extension of a shared library on this computer.
pub fn library_extension() -> &'static str {
    if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    }
}

/// The name cargo gives the library of crate `package` on this computer.
pub fn built_name(package: &str) -> String {
    let stem = package.replace('-', "_");
    let prefix = if cfg!(target_os = "windows") { "" } else { "lib" };
    format!("{prefix}{stem}.{}", library_extension())
}

/// The name the library of plugin `id` is published under. A part of its checksum is in it, so a
/// library that is published again is another file and never half of two.
pub fn published_name(id: &str, platform: &str, sha256: &str) -> String {
    let sum: String = sha256.chars().take(10).collect();
    format!("{id}-{platform}-{sum}.{}", library_extension())
}

/// Whether `name` is one [`published_name`] gives a library of `id`, with the checksum or without.
pub fn is_published_name(name: &str, id: &str, platform: &str) -> bool {
    let rest = name
        .strip_prefix(id)
        .and_then(|n| n.strip_prefix('-')?.strip_prefix(platform))
        .and_then(|n| n.strip_suffix(library_extension())?.strip_suffix('.'));
    match rest {
        Some("") => true,
        Some(sum) => sum
            .strip_prefix('-')
            .is_some_and(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit())),
        None => false,
    }
}

/// A plugin id is a safe folder name.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A file name that stays in its folder.
pub fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && !name.starts_with('.') && !name.contains(['/', '\\', '\0'])
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

/// The SHA-256 of a file, read in pieces.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hex_encode(&hash.finalize()))
}

/// What marks the stamp in a plugin library (`corvus_api::plugin::dynamic::MARKED_ABI`).
const MARK: &[u8] = b"\0CORVUS-PLUGIN\0";

/// What marks the crate a plugin library was built from (`export_plugin!`).
const CRATE_MARK: &[u8] = b"\0CORVUS-CRATE\0";

/// The stamp of the plugin library `bytes`: what its program must be built from. `None` when the
/// file is no plugin library.
pub fn abi_of(bytes: &[u8]) -> Option<&str> {
    marked(bytes, MARK)
}

/// The crate the plugin library `bytes` was built from (`corvus_plugin_<id, dashes as underscores>`).
pub fn crate_of(bytes: &[u8]) -> Option<&str> {
    marked(bytes, CRATE_MARK)
}

/// The text between `mark` and the next NUL.
fn marked<'a>(bytes: &'a [u8], mark: &[u8]) -> Option<&'a str> {
    let at = bytes.windows(mark.len()).position(|w| w == mark)? + mark.len();
    let end = bytes[at..].iter().take(512).position(|b| *b == 0)?;
    std::str::from_utf8(&bytes[at..at + end]).ok().filter(|s| !s.is_empty())
}

/// One library of a plugin, for one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub file: String,
    pub sha256: String,
    pub size: u64,
}

/// One plugin of an index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub version: String,
    pub files: BTreeMap<String, File>,
}

impl Entry {
    /// The library built for `platform`, if there is one.
    pub fn file_for(&self, platform: &str) -> Option<&File> {
        self.files.get(platform)
    }

    pub fn to_json(&self) -> Value {
        let files: Map<String, Value> = self
            .files
            .iter()
            .map(|(p, f)| (p.clone(), json!({"file": f.file, "sha256": f.sha256, "size": f.size})))
            .collect();
        json!({
            "id": self.id, "name": self.name, "description": self.description,
            "icon": self.icon, "version": self.version, "files": files,
        })
    }

    pub fn from_json(v: &Value) -> Result<Entry, String> {
        let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let id = text("id");
        if !is_valid_id(&id) {
            return Err(format!("{id:?} is not a plugin id"));
        }
        let mut files = BTreeMap::new();
        for (platform, f) in v.get("files").and_then(Value::as_object).into_iter().flatten() {
            let file = f.get("file").and_then(Value::as_str).unwrap_or("");
            let sha = f.get("sha256").and_then(Value::as_str).unwrap_or("");
            let size = f.get("size").and_then(Value::as_u64).unwrap_or(0);
            if !is_plain_file_name(file) || sha.len() != 64 || hex_decode(sha).is_none() || size == 0 {
                return Err(format!("the file entry of {id} for {platform} is not valid"));
            }
            files.insert(
                platform.clone(),
                File {
                    file: file.to_string(),
                    sha256: sha.to_ascii_lowercase(),
                    size,
                },
            );
        }
        Ok(Entry {
            name: if text("name").is_empty() {
                id.clone()
            } else {
                text("name")
            },
            description: text("description"),
            icon: text("icon"),
            version: text("version"),
            id,
            files,
        })
    }
}

/// The plugins one release of Corvus GCS offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// What the libraries were built from (`corvus_api::plugin::dynamic::ABI`).
    pub abi: String,
    pub plugins: Vec<Entry>,
}

impl Index {
    pub fn to_bytes(&self) -> Vec<u8> {
        let v = json!({
            "format": INDEX_FORMAT,
            "abi": self.abi,
            "plugins": self.plugins.iter().map(Entry::to_json).collect::<Vec<_>>(),
        });
        let mut out = serde_json::to_vec_pretty(&v).unwrap_or_default();
        out.push(b'\n');
        out
    }

    /// Reads an index.
    pub fn parse(bytes: &[u8]) -> Result<Index, String> {
        let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("the plugin list is not valid: {e}"))?;
        if v.get("format").and_then(Value::as_u64) != Some(INDEX_FORMAT) {
            return Err("the plugin list is of a newer kind than this version reads".into());
        }
        let abi = v.get("abi").and_then(Value::as_str).unwrap_or("").to_string();
        let mut plugins = Vec::new();
        for p in v.get("plugins").and_then(Value::as_array).into_iter().flatten() {
            plugins.push(Entry::from_json(p)?);
        }
        Ok(Index { abi, plugins })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> Index {
        let mut files = BTreeMap::new();
        files.insert(
            "macos-aarch64".to_string(),
            File {
                file: "vibration-macos-aarch64.dylib".into(),
                sha256: sha256_hex(b"lib"),
                size: 3,
            },
        );
        Index {
            abi: "2026.11.83|rustc 1.99".into(),
            plugins: vec![Entry {
                id: "vibration".into(),
                name: "Vibration Monitor".into(),
                description: "d".into(),
                icon: "activity".into(),
                version: "1.0.0".into(),
                files,
            }],
        }
    }

    #[test]
    fn an_index_survives_writing_and_reading() {
        let i = index();
        assert_eq!(Index::parse(&i.to_bytes()).unwrap(), i);
    }

    #[test]
    fn an_entry_that_could_leave_its_folder_is_refused() {
        let mut v = index().plugins[0].to_json();
        v["files"]["macos-aarch64"]["file"] = json!("../evil.dylib");
        assert!(Entry::from_json(&v).is_err());
        let mut v = index().plugins[0].to_json();
        v["id"] = json!("../x");
        assert!(Entry::from_json(&v).is_err());
        let mut v = index().plugins[0].to_json();
        v["files"]["macos-aarch64"]["sha256"] = json!("zz");
        assert!(Entry::from_json(&v).is_err());
    }

    #[test]
    fn the_stamp_is_found_in_the_bytes_of_a_library() {
        let mut lib = vec![0xcf, 0, 1, 2];
        assert_eq!(abi_of(&lib), None);
        lib.extend_from_slice(b"\0CORVUS-PLUGIN\x001|rustc 1.99|00ff\0rest");
        assert_eq!(abi_of(&lib), Some("1|rustc 1.99|00ff"));
        assert_eq!(abi_of(b"\0CORVUS-PLUGIN\0cut"), None);
        assert_eq!(abi_of(b"\0CORVUS-PLUGIN\0\0"), None);
        assert_eq!(crate_of(&lib), None);
        lib.extend_from_slice(b"\0CORVUS-CRATE\0corvus_plugin_x\0");
        assert_eq!(crate_of(&lib), Some("corvus_plugin_x"));
    }

    #[test]
    fn a_published_library_is_known_by_its_name() {
        let name = published_name("tak", "linux-x86_64", &"ab".repeat(32));
        assert!(name.starts_with("tak-linux-x86_64-ababababab."), "{name}");
        assert!(is_published_name(&name, "tak", "linux-x86_64"));
        assert!(!is_published_name(&name, "ta", "linux-x86_64"));
        assert!(!is_published_name(&name, "tak", "linux"));
        assert!(!is_published_name("tak-linux-x86_64.entry.json", "tak", "linux-x86_64"));
    }

    #[test]
    fn a_release_is_named_after_the_stamp_alone() {
        let tag = release_tag("1|rustc 1.99.0|00ff");
        assert!(tag.starts_with("abi-") && tag.len() == 20, "{tag}");
        assert_eq!(tag, release_tag("1|rustc 1.99.0|00ff"));
        assert_ne!(tag, release_tag("1|rustc 1.99.0|0100"));
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(hex_decode(&hex_encode(&[0, 15, 255])), Some(vec![0, 15, 255]));
        assert_eq!(hex_decode("abc"), None);
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
