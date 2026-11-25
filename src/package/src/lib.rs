//! What a plugin package is, as the program that installs it and the tool that
//! builds it both need to agree.
//!
//! A plugin is one WebAssembly module, the same on every system. Its manifest
//! ([`Manifest`], the crate's `plugin.json`) lies in a custom section of the
//! module, so it is read without running any of it. It names the oldest Corvus
//! GCS the plugin needs, and **every version from there on takes it**: nothing
//! else about the program is compared.
//!
//! Each plugin is a repository of its own. Its release [`RELEASE_TAG`] carries
//! one [`INDEX_FILE`] and the modules it names, which may be several versions of
//! the plugin: the program takes the newest one it is new enough for. The index
//! is read over HTTPS, and size and SHA-256 of every download are checked
//! against it before anything is written where it could be loaded from. There is
//! no signing key: what is trusted is the plugin's repository on GitHub.

use std::io::Read;
use std::path::Path;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The version of the index layout. 1 listed native libraries.
pub const INDEX_FORMAT: u64 = 2;

/// The file name of the index in a release.
pub const INDEX_FILE: &str = "plugins-index.json";

/// The release of a plugin's repository that holds its modules.
pub const RELEASE_TAG: &str = "plugin";

/// The extension of a plugin module.
pub const EXTENSION: &str = "wasm";

/// The custom section that holds the manifest (`corvus_api::wire::MANIFEST_SECTION`).
pub const MANIFEST_SECTION: &str = "corvus.manifest";

const MANIFEST_MAX: usize = 64 * 1024;
const PERMISSIONS_MAX: usize = 32;

/// A version of Corvus GCS, `YYYY.MM.PP`, to compare. `None` for anything else.
pub fn corvus_version(text: &str) -> Option<[u32; 3]> {
    let mut parts = text.trim().split('.');
    let mut out = [0; 3];
    for slot in &mut out {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// Whether the Corvus GCS of version `corvus` takes a plugin that needs at least
/// `min`. This is the only thing that is compared.
pub fn fits(min: &str, corvus: &str) -> bool {
    matches!((corvus_version(min), corvus_version(corvus)), (Some(m), Some(c)) if c >= m)
}

/// A plugin's own version as numbers, to tell which of two is newer.
pub fn plugin_version(text: &str) -> Vec<u32> {
    text.split('.')
        .map(|p| {
            let digits: String = p.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().unwrap_or(0)
        })
        .collect()
}

/// What a plugin says of itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub description: String,
    /// A Lucide icon name.
    pub icon: String,
    /// The plugin's own version.
    pub version: String,
    /// Where its card sits in the grid: lower is earlier, ties by name.
    pub order: i32,
    /// Whether it can live in a tab of its own.
    pub can_tab: bool,
    /// The oldest Corvus GCS it runs in.
    pub min_corvus: String,
    /// What it may do beyond drawing and reading telemetry. The program refuses
    /// the rest.
    pub permissions: Vec<String>,
}

fn is_permission_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

impl Manifest {
    pub fn from_json(v: &Value) -> Result<Manifest, String> {
        let text = |k: &str, max: usize| -> String {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .chars()
                .take(max)
                .collect()
        };
        let id = text("id", 65);
        if !is_valid_id(&id) {
            return Err(format!("{id:?} is not a plugin id"));
        }
        let min_corvus = text("min_corvus", 32);
        if corvus_version(&min_corvus).is_none() {
            return Err(format!("{id}: min_corvus must be a version of Corvus GCS like 2026.11.94"));
        }
        let mut permissions = Vec::new();
        match v.get("permissions") {
            None => {}
            Some(Value::Array(list)) if list.len() <= PERMISSIONS_MAX => {
                for p in list {
                    let name = p.as_str().unwrap_or("");
                    if !is_permission_name(name) {
                        return Err(format!("{id}: {p} is not the name of a permission"));
                    }
                    if !permissions.iter().any(|have| have == name) {
                        permissions.push(name.to_string());
                    }
                }
            }
            Some(_) => return Err(format!("{id}: permissions must be a short list of names")),
        }
        let order = v.get("order").and_then(Value::as_i64).unwrap_or(100);
        let name = text("name", 80);
        Ok(Manifest {
            name: if name.is_empty() { id.clone() } else { name },
            description: text("description", 300),
            icon: text("icon", 48),
            version: text("version", 32),
            order: i32::try_from(order).unwrap_or(100),
            can_tab: v.get("can_tab") == Some(&Value::Bool(true)),
            min_corvus,
            permissions,
            id,
        })
    }

    pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
        if bytes.len() > MANIFEST_MAX {
            return Err("the manifest is too large".into());
        }
        let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("the manifest is not valid: {e}"))?;
        Manifest::from_json(&v)
    }

    /// The manifest of the module `bytes`, read without running any of it.
    pub fn of_module(bytes: &[u8]) -> Result<Manifest, String> {
        let section = custom_section(bytes, MANIFEST_SECTION)?.ok_or("the file is not a Corvus plugin")?;
        Manifest::parse(section)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "description": self.description,
            "icon": self.icon, "version": self.version, "order": self.order,
            "can_tab": self.can_tab, "min_corvus": self.min_corvus,
            "permissions": self.permissions,
        })
    }

    pub fn fits(&self, corvus: &str) -> bool {
        fits(&self.min_corvus, corvus)
    }
}

/// An unsigned LEB128 number of at most 32 bits at `*at`.
fn leb(bytes: &[u8], at: &mut usize) -> Option<usize> {
    let mut out = 0u64;
    for shift in (0..35).step_by(7) {
        let byte = *bytes.get(*at)?;
        *at += 1;
        out |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return u32::try_from(out).ok().map(|n| n as usize);
        }
    }
    None
}

/// The bytes of the custom section `wanted` of a WebAssembly module.
pub fn custom_section<'a>(module: &'a [u8], wanted: &str) -> Result<Option<&'a [u8]>, String> {
    const HEAD: &[u8] = b"\0asm\x01\0\0\0";
    if !module.starts_with(HEAD) {
        return Err("the file is not a Corvus plugin".into());
    }
    let cut = || "the file is cut short or damaged".to_string();
    let mut at = HEAD.len();
    while at < module.len() {
        let id = module[at];
        at += 1;
        let size = leb(module, &mut at).ok_or_else(cut)?;
        let end = at.checked_add(size).filter(|e| *e <= module.len()).ok_or_else(cut)?;
        if id == 0 {
            let mut inner = at;
            let len = leb(module, &mut inner).ok_or_else(cut)?;
            let name_end = inner.checked_add(len).filter(|e| *e <= end).ok_or_else(cut)?;
            if &module[inner..name_end] == wanted.as_bytes() {
                return Ok(Some(&module[name_end..end]));
            }
        }
        at = end;
    }
    Ok(None)
}

/// One module a release offers: what it says of itself and the file it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub manifest: Manifest,
    pub file: String,
    pub sha256: String,
    pub size: u64,
}

impl Entry {
    pub fn to_json(&self) -> Value {
        let mut v = self.manifest.to_json();
        v["file"] = json!(self.file);
        v["sha256"] = json!(self.sha256);
        v["size"] = json!(self.size);
        v
    }

    pub fn from_json(v: &Value) -> Result<Entry, String> {
        let manifest = Manifest::from_json(v)?;
        let file = v.get("file").and_then(Value::as_str).unwrap_or("");
        let sha = v.get("sha256").and_then(Value::as_str).unwrap_or("");
        let size = v.get("size").and_then(Value::as_u64).unwrap_or(0);
        if !is_plain_file_name(file) || sha.len() != 64 || hex_decode(sha).is_none() || size == 0 {
            return Err(format!("the file entry of {} is not valid", manifest.id));
        }
        Ok(Entry {
            file: file.to_string(),
            sha256: sha.to_ascii_lowercase(),
            size,
            manifest,
        })
    }
}

/// What a release of a plugin's repository offers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    pub plugins: Vec<Entry>,
}

impl Index {
    pub fn to_bytes(&self) -> Vec<u8> {
        let v = json!({
            "format": INDEX_FORMAT,
            "plugins": self.plugins.iter().map(Entry::to_json).collect::<Vec<_>>(),
        });
        let mut out = serde_json::to_vec_pretty(&v).unwrap_or_default();
        out.push(b'\n');
        out
    }

    /// Reads an index. An entry this version cannot read is passed over, so a
    /// later kind of entry does not hide the ones that are good.
    pub fn parse(bytes: &[u8]) -> Result<Index, String> {
        let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("the plugin list is not valid: {e}"))?;
        if v.get("format").and_then(Value::as_u64) != Some(INDEX_FORMAT) {
            return Err("the plugin list is of another kind than this version reads".into());
        }
        let list = v.get("plugins").and_then(Value::as_array);
        let plugins: Vec<Entry> = list
            .into_iter()
            .flatten()
            .filter_map(|p| Entry::from_json(p).ok())
            .collect();
        if plugins.is_empty() && list.is_some_and(|l| !l.is_empty()) {
            return Err("no entry of the plugin list can be read".into());
        }
        Ok(Index { plugins })
    }

    /// The newest module of plugin `id` that Corvus GCS `corvus` takes.
    pub fn best(&self, id: &str, corvus: &str) -> Option<&Entry> {
        self.plugins
            .iter()
            .filter(|e| e.manifest.id == id && e.manifest.fits(corvus))
            .max_by_key(|e| {
                (
                    plugin_version(&e.manifest.version),
                    corvus_version(&e.manifest.min_corvus),
                )
            })
    }

    /// The oldest Corvus GCS any module of `id` runs in.
    pub fn needs(&self, id: &str) -> Option<&str> {
        self.plugins
            .iter()
            .filter(|e| e.manifest.id == id)
            .min_by_key(|e| corvus_version(&e.manifest.min_corvus))
            .map(|e| e.manifest.min_corvus.as_str())
    }
}

/// The name cargo gives the module of crate `package`.
pub fn built_name(package: &str) -> String {
    format!("{}.{EXTENSION}", package.replace('-', "_"))
}

/// The name a module is published under. A part of its checksum is in it, so a
/// module that is published again is another file and never half of two.
pub fn published_name(id: &str, version: &str, sha256: &str) -> String {
    let sum: String = sha256.chars().take(10).collect();
    format!("{id}-{version}-{sum}.{EXTENSION}")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str, min: &str) -> Manifest {
        Manifest {
            id: "vibration".into(),
            name: "Vibration Monitor".into(),
            description: "d".into(),
            icon: "activity".into(),
            version: version.into(),
            order: 10,
            can_tab: false,
            min_corvus: min.into(),
            permissions: vec!["telemetry.rates".into()],
        }
    }

    fn entry(version: &str, min: &str) -> Entry {
        Entry {
            manifest: manifest(version, min),
            file: format!("vibration-{version}.wasm"),
            sha256: sha256_hex(version.as_bytes()),
            size: 3,
        }
    }

    #[test]
    fn every_version_from_the_one_a_plugin_names_takes_it() {
        for newer in ["2026.11.94", "2026.11.95", "2026.11.100", "2026.12.01", "2027.01.01"] {
            assert!(fits("2026.11.94", newer), "{newer}");
        }
        for older in ["2026.11.93", "2026.11.09", "2026.10.99", "2025.12.99"] {
            assert!(!fits("2026.11.94", older), "{older}");
        }
        for broken in ["", "2026.11", "2026.11.94.1", "v2026.11.94", "2026.11.x", "2026..1"] {
            assert!(!fits("2026.11.94", broken), "{broken:?}");
            assert!(!fits(broken, "2099.01.01"), "{broken:?}");
        }
    }

    #[test]
    fn the_newest_module_the_program_is_new_enough_for_is_taken() {
        let index = Index {
            plugins: vec![
                entry("2.0.0", "2026.11.94"),
                entry("2.10.0", "2026.12.01"),
                entry("2.2.0", "2026.11.94"),
                entry("3.0.0", "2027.01.01"),
            ],
        };
        let of = |corvus: &str| index.best("vibration", corvus).map(|e| e.manifest.version.as_str());
        assert_eq!(of("2026.11.93"), None);
        assert_eq!(of("2026.11.99"), Some("2.2.0"));
        assert_eq!(of("2026.12.05"), Some("2.10.0"));
        assert_eq!(of("2028.01.01"), Some("3.0.0"));
        assert_eq!(index.needs("vibration"), Some("2026.11.94"));
        assert_eq!(index.best("tak", "2028.01.01"), None);
    }

    #[test]
    fn an_index_survives_writing_and_reading() {
        let i = Index {
            plugins: vec![entry("2.0.0", "2026.11.94")],
        };
        assert_eq!(Index::parse(&i.to_bytes()).unwrap(), i);
    }

    #[test]
    fn an_entry_that_could_leave_its_folder_is_refused() {
        let good = entry("2.0.0", "2026.11.94").to_json();
        for (key, bad) in [
            ("file", json!("../evil.wasm")),
            ("id", json!("../x")),
            ("sha256", json!("zz")),
            ("min_corvus", json!("any")),
            ("permissions", json!(["Run Everything"])),
            ("permissions", json!("local")),
        ] {
            let mut v = good.clone();
            v[key] = bad.clone();
            assert!(Entry::from_json(&v).is_err(), "{key} {bad}");
        }
    }

    #[test]
    fn a_list_keeps_what_it_can_read() {
        let v = json!({"format": 2, "plugins": [entry("2.0.0", "2026.11.94").to_json(), {"id": "x"}]});
        assert_eq!(Index::parse(v.to_string().as_bytes()).unwrap().plugins.len(), 1);
        assert!(Index::parse(br#"{"format": 2, "plugins": [{"id": "x"}]}"#).is_err());
        assert!(Index::parse(br#"{"format": 1, "plugins": []}"#).is_err());
    }

    /// A module of the head and these sections.
    fn module(sections: &[(u8, &[u8])]) -> Vec<u8> {
        let mut out = b"\0asm\x01\0\0\0".to_vec();
        for (id, body) in sections {
            out.push(*id);
            out.push(body.len() as u8);
            out.extend_from_slice(body);
        }
        out
    }

    #[test]
    fn the_manifest_is_found_in_a_module_and_a_damaged_one_is_refused() {
        let json = br#"{"id":"x","min_corvus":"2026.11.94"}"#;
        let mut body = vec![MANIFEST_SECTION.len() as u8];
        body.extend_from_slice(MANIFEST_SECTION.as_bytes());
        body.extend_from_slice(json);
        let m = module(&[(1, b"xx"), (0, b"\x05other"), (0, &body)]);
        assert_eq!(Manifest::of_module(&m).unwrap().id, "x");
        assert!(Manifest::of_module(&module(&[(1, b"xx")])).is_err());
        assert!(Manifest::of_module(b"not a module").is_err());
        for cut in 9..m.len() {
            let _ = Manifest::of_module(&m[..cut]);
        }
        assert!(Manifest::of_module(&module(&[(0, b"\x7fshort")])).is_err());
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
