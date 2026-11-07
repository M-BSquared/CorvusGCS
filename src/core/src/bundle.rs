//! One file that carries a whole station setup (`settings_bundle.py`): the
//! Settings export and import. Pure functions over JSON, so the routes that
//! read and write the files (the plugins area) and the tests share them.

use std::collections::{BTreeMap, HashSet};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::Value;

use crate::json::Map;

pub const KIND: &str = "corvus-settings";
pub const FORMAT: i64 = 2;
pub const MAX_LOGO_BYTES: usize = 4 * 1024 * 1024;
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Section id and the config keys it carries. `plugins` and `layout` carry
/// none: the first is the plugin files, the second browser state only.
pub const SECTIONS: &[(&str, &[&str])] = &[
    (
        "interface",
        &["theme", "language", "ui", "controls", "branding", "updates", "review"],
    ),
    ("map", &["map", "map_tokens", "tile_sources"]),
    (
        "connections",
        &[
            "mavlink_connection",
            "autoconnect",
            "forwarding",
            "stream_rates",
            "ssh_connections",
            "mcp",
        ],
    ),
    (
        "vehicle",
        &["battery", "remote_id", "rtk", "video", "parameters", "checklists"],
    ),
    (
        "folders",
        &[
            "http_port",
            "tile_cache_dir",
            "tlog_dir",
            "params_dir",
            "firmware_dir",
            "log_download_dir",
            "missions_dir",
        ],
    ),
    ("plugins", &[]),
    ("layout", &[]),
];

/// Keys a bundle never carries: `plugins` travel as the bundle's own block,
/// `fleet` names this machine's serial ports.
pub const EXCLUDED_KEYS: &[&str] = &["fleet", "plugins"];

pub fn section_of(key: &str) -> Option<&'static str> {
    SECTIONS.iter().find(|(_, keys)| keys.contains(&key)).map(|(s, _)| *s)
}

fn is_section(s: &str) -> bool {
    SECTIONS.iter().any(|(n, _)| *n == s)
}

/// A deep copy of `config` with every secret taken out, the same four
/// `to_public` keeps out of responses.
pub fn redact(config: &Map) -> Map {
    let mut out = config.clone();
    out.shift_remove("map_tokens");
    if let Some(Value::Array(list)) = out.get_mut("ssh_connections") {
        for e in list.iter_mut().filter_map(Value::as_object_mut) {
            e.shift_remove("password");
        }
    }
    if let Some(Value::Object(ntrip)) = out
        .get_mut("rtk")
        .and_then(Value::as_object_mut)
        .and_then(|r| r.get_mut("ntrip"))
    {
        ntrip.insert("password".into(), "".into());
    }
    if let Some(Value::Array(streams)) = out
        .get_mut("video")
        .and_then(Value::as_object_mut)
        .and_then(|v| v.get_mut("streams"))
    {
        for s in streams.iter_mut().filter_map(Value::as_object_mut) {
            s.shift_remove("password");
        }
    }
    out
}

/// What an export is made of beyond the config.
#[derive(Default)]
pub struct BuildOptions {
    /// `None` is every section.
    pub sections: Option<Vec<String>>,
    pub plugins: Map,
    pub plugin_files: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    pub logo: Option<Vec<u8>>,
    pub browser: Map,
    pub include_secrets: bool,
    /// `exported_at`, `YYYY-MM-DDTHH:MM:SSZ`.
    pub exported_at: String,
    pub version: String,
}

fn clean_browser(raw: &Map) -> Map {
    raw.iter()
        .filter(|(k, v)| k.starts_with("corvus.") && v.is_string())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

pub fn build(config: &Map, opts: &BuildOptions) -> Map {
    let chosen: HashSet<&str> = match &opts.sections {
        None => SECTIONS.iter().map(|(n, _)| *n).collect(),
        Some(list) => list.iter().map(String::as_str).filter(|s| is_section(s)).collect(),
    };
    let mut cfg: Map = config
        .iter()
        .filter(|(k, _)| !EXCLUDED_KEYS.contains(&k.as_str()) && section_of(k).is_some_and(|s| chosen.contains(s)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if !opts.include_secrets {
        cfg = redact(&cfg);
    }
    let with_plugins = chosen.contains("plugins");
    let mut bundle = Map::new();
    bundle.insert("kind".into(), KIND.into());
    bundle.insert("format".into(), FORMAT.into());
    bundle.insert("product".into(), "Corvus GCS".into());
    bundle.insert("version".into(), opts.version.clone().into());
    bundle.insert("exported_at".into(), opts.exported_at.clone().into());
    bundle.insert("secrets".into(), opts.include_secrets.into());
    bundle.insert(
        "sections".into(),
        Value::Array(
            SECTIONS
                .iter()
                .filter(|(n, _)| chosen.contains(n))
                .map(|(n, _)| Value::from(*n))
                .collect(),
        ),
    );
    bundle.insert("config".into(), Value::Object(cfg));
    let plugins: Map = if with_plugins {
        opts.plugins
            .iter()
            .filter(|(_, v)| v.is_object())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    } else {
        Map::new()
    };
    bundle.insert("plugins".into(), Value::Object(plugins));
    let mut files = Map::new();
    if with_plugins {
        for (id, list) in &opts.plugin_files {
            let encoded: Map = list
                .iter()
                .map(|(rel, data)| (rel.clone(), Value::from(STANDARD.encode(data))))
                .collect();
            files.insert(id.clone(), Value::Object(encoded));
        }
    }
    bundle.insert("plugin_files".into(), Value::Object(files));
    bundle.insert("browser".into(), Value::Object(clean_browser(&opts.browser)));
    if let Some(logo) = opts.logo.as_ref().filter(|l| !l.is_empty()) {
        if chosen.contains("interface") {
            bundle.insert("logo".into(), STANDARD.encode(logo).into());
        }
    }
    bundle
}

/// A bundle checked and normalised.
#[derive(Debug, PartialEq)]
pub struct Parsed {
    pub version: String,
    pub exported_at: String,
    pub secrets: bool,
    pub sections: Vec<String>,
    pub config: Map,
    pub plugins: Map,
    pub plugin_files: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    pub logo: Option<Vec<u8>>,
    pub browser: Map,
}

/// `^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$`, a plugin's id.
pub fn is_valid_plugin_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

/// Checks the files of one plugin the way an install does
/// (`plugin_files.check`); the plugins area supplies it.
pub type PluginCheck<'a> = &'a dyn Fn(BTreeMap<String, Vec<u8>>) -> Result<BTreeMap<String, Vec<u8>>, String>;

pub fn decode_logo(raw: &Value) -> Result<Vec<u8>, String> {
    let unreadable = || "The company logo in the settings file is unreadable.".to_string();
    let text = raw.as_str().ok_or_else(unreadable)?;
    let blob = STANDARD.decode(text).map_err(|_| unreadable())?;
    if blob.len() > MAX_LOGO_BYTES {
        return Err("The company logo in the settings file is larger than 4 MB.".into());
    }
    if !blob.starts_with(PNG_MAGIC) {
        return Err("The company logo in the settings file is not a PNG image.".into());
    }
    Ok(blob)
}

pub fn decode_plugin_files(
    raw: &Value,
    check: PluginCheck,
) -> Result<BTreeMap<String, BTreeMap<String, Vec<u8>>>, String> {
    let unreadable = || "The plugins in the settings file are unreadable.".to_string();
    if raw.is_null() {
        return Ok(BTreeMap::new());
    }
    let map = raw.as_object().ok_or_else(unreadable)?;
    let mut out = BTreeMap::new();
    for (id, files) in map {
        let files = files
            .as_object()
            .filter(|_| is_valid_plugin_id(id))
            .ok_or_else(unreadable)?;
        let mut decoded = BTreeMap::new();
        for (rel, blob) in files {
            let bytes = blob
                .as_str()
                .and_then(|t| STANDARD.decode(t).ok())
                .ok_or_else(|| format!("The plugin {id} in the settings file is unusable ({rel})."))?;
            decoded.insert(rel.clone(), bytes);
        }
        out.insert(
            id.clone(),
            check(decoded).map_err(|e| format!("The plugin {id} in the settings file is unusable ({e})."))?,
        );
    }
    Ok(out)
}

/// Checks that `data` is a settings bundle and returns it normalised. The
/// error is a sentence an operator can act on.
pub fn parse(data: &Value, check: PluginCheck) -> Result<Parsed, String> {
    let obj = data
        .as_object()
        .filter(|o| o.get("kind").and_then(Value::as_str) == Some(KIND));
    let Some(obj) = obj else {
        return Err("This is not a Corvus GCS settings file.".into());
    };
    let fmt = obj
        .get("format")
        .filter(|v| v.is_i64() || v.is_u64())
        .and_then(Value::as_i64)
        .filter(|f| *f >= 1);
    let Some(fmt) = fmt else {
        return Err("The settings file has no valid format number.".into());
    };
    if fmt > FORMAT {
        return Err("The settings file was written by a newer Corvus GCS. Update this station first.".into());
    }
    let Some(config) = obj.get("config").and_then(Value::as_object) else {
        return Err("The settings file carries no configuration.".into());
    };
    let sections: HashSet<String> = if fmt < 2 {
        SECTIONS.iter().map(|(n, _)| n.to_string()).collect()
    } else {
        let Some(list) = obj.get("sections").and_then(Value::as_array) else {
            return Err("The settings file does not say which settings it carries.".into());
        };
        list.iter()
            .filter_map(Value::as_str)
            .filter(|s| is_section(s))
            .map(str::to_string)
            .collect()
    };
    let has = |s: &str| sections.contains(s);
    let logo = match obj.get("logo") {
        Some(v) if !v.is_null() && has("interface") => Some(decode_logo(v)?),
        _ => None,
    };
    let text = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let plugins = match obj.get("plugins").and_then(Value::as_object) {
        Some(p) if has("plugins") => p
            .iter()
            .filter(|(_, v)| v.is_object())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        _ => Map::new(),
    };
    let plugin_files = if has("plugins") && fmt >= 2 {
        decode_plugin_files(obj.get("plugin_files").unwrap_or(&Value::Null), check)?
    } else {
        BTreeMap::new()
    };
    Ok(Parsed {
        version: text("version"),
        exported_at: text("exported_at"),
        secrets: obj.get("secrets") == Some(&Value::Bool(true)),
        sections: SECTIONS
            .iter()
            .filter(|(n, _)| has(n))
            .map(|(n, _)| n.to_string())
            .collect(),
        config: config
            .iter()
            .filter(|(k, _)| !EXCLUDED_KEYS.contains(&k.as_str()) && section_of(k).is_some_and(has))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        plugins,
        plugin_files,
        logo,
        browser: obj
            .get("browser")
            .and_then(Value::as_object)
            .map(clean_browser)
            .unwrap_or_default(),
    })
}

/// The known section ids in `raw`; every section when it is `None`.
pub fn normalize_sections(raw: Option<&Value>) -> Result<HashSet<String>, String> {
    match raw {
        None | Some(Value::Null) => Ok(SECTIONS.iter().map(|(n, _)| n.to_string()).collect()),
        Some(Value::Array(a)) => Ok(a
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| is_section(s))
            .map(str::to_string)
            .collect()),
        Some(_) => Err("sections must be a list".into()),
    }
}

/// The ids in `raw` that are also in `available`; all of them when it is `None`.
pub fn normalize_ids(raw: Option<&Value>, available: &[&str], name: &str) -> Result<HashSet<String>, String> {
    match raw {
        None | Some(Value::Null) => Ok(available.iter().map(|s| s.to_string()).collect()),
        Some(Value::Array(a)) => Ok(a
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| available.contains(s))
            .map(str::to_string)
            .collect()),
        Some(_) => Err(format!("{name} must be a list")),
    }
}

/// The config that results from importing `imported` over `current`: every
/// key of a chosen section is replaced (a key the file leaves out goes back
/// to its default), the others stay. Without `secrets`, stored secrets are
/// carried over to entries that are still the same entry.
pub fn merge(current: &Map, imported: &Map, sections: &[&str], secrets: bool) -> Map {
    let mut out = current.clone();
    for section in sections {
        let keys = SECTIONS
            .iter()
            .find(|(n, _)| n == section)
            .map(|(_, k)| *k)
            .unwrap_or(&[]);
        for key in keys {
            match imported.get(*key) {
                Some(v) => {
                    out.insert((*key).into(), v.clone());
                }
                None => {
                    out.shift_remove(*key);
                }
            }
        }
    }
    if !secrets {
        keep_secrets(&mut out, current, sections);
    }
    out
}

fn obj<'a>(m: &'a Map, key: &str) -> Option<&'a Map> {
    m.get(key).and_then(Value::as_object)
}

fn keep_secrets(out: &mut Map, current: &Map, chosen: &[&str]) {
    let has = |s: &str| chosen.contains(&s);
    if has("map") {
        if let Some(stored) = obj(current, "map_tokens").filter(|m| !m.is_empty()) {
            let mut joined = stored.clone();
            if let Some(new) = obj(out, "map_tokens") {
                joined.extend(new.clone());
            }
            out.insert("map_tokens".into(), Value::Object(joined));
        }
    }
    let ident = |e: &Map, keys: &[&str]| -> Vec<Option<Value>> { keys.iter().map(|k| e.get(*k).cloned()).collect() };
    if has("connections") {
        let stored: Vec<(Vec<Option<Value>>, Option<Value>)> = current
            .get("ssh_connections")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_object)
                    .map(|e| (ident(e, &["name", "host", "username"]), e.get("password").cloned()))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(Value::Array(list)) = out.get_mut("ssh_connections") {
            for e in list.iter_mut().filter_map(Value::as_object_mut) {
                if e.get("password").is_some_and(crate::config::truthy) {
                    continue;
                }
                let id = ident(e, &["name", "host", "username"]);
                if let Some((_, Some(pw))) = stored.iter().find(|(i, _)| *i == id) {
                    if crate::config::truthy(pw) {
                        e.insert("password".into(), pw.clone());
                    }
                }
            }
        }
    }
    if has("vehicle") {
        let old = obj(current, "rtk").and_then(|r| obj(r, "ntrip")).cloned();
        if let (Some(old), Some(Value::Object(rtk))) = (old, out.get_mut("rtk")) {
            if let Some(Value::Object(new)) = rtk.get_mut("ntrip") {
                let same = ["host", "port", "username"].iter().all(|k| new.get(*k) == old.get(*k));
                if !new.get("password").is_some_and(crate::config::truthy)
                    && old.get("password").is_some_and(crate::config::truthy)
                    && same
                {
                    new.insert("password".into(), old["password"].clone());
                }
            }
        }
        let stored: Vec<(Vec<Option<Value>>, Option<Value>)> = obj(current, "video")
            .and_then(|v| v.get("streams"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_object)
                    .map(|s| (ident(s, &["url", "username"]), s.get("password").cloned()))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(Value::Array(streams)) = out
            .get_mut("video")
            .and_then(Value::as_object_mut)
            .and_then(|v| v.get_mut("streams"))
        {
            for s in streams.iter_mut().filter_map(Value::as_object_mut) {
                if s.get("password").is_some_and(crate::config::truthy) {
                    continue;
                }
                let id = ident(s, &["url", "username"]);
                if let Some((_, Some(pw))) = stored.iter().find(|(i, _)| *i == id) {
                    if crate::config::truthy(pw) {
                        s.insert("password".into(), pw.clone());
                    }
                }
            }
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time in UTC.
pub fn utc_iso(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cases() -> Value {
        serde_json::from_str(include_str!("../tests/data/settings_bundle_cases.json")).unwrap()
    }

    fn config() -> Map {
        let v = json!({
            "theme": {"name": "green"}, "language": "de", "ui": {"scale": 1.5},
            "map": {"provider": "esri"}, "map_tokens": {"mapbox": "tok"},
            "mavlink_connection": "udp:0.0.0.0:14550", "http_port": 8000,
            "ssh_connections": [{"name": "a", "host": "h", "username": "u", "password": "pw"},
                                {"name": "b", "host": "h2", "username": "u"}],
            "rtk": {"ntrip": {"host": "c", "port": 2101, "username": "n", "password": "secret"}, "enabled": true},
            "video": {"streams": [{"url": "rtsp://x", "username": "v", "password": "cam"}], "ffmpeg": ""},
            "plugins": {"legacy": {"a": 1}}, "fleet": {"links": []},
            "battery": {"estimate": true}
        });
        v.as_object().unwrap().clone()
    }

    fn pass(f: BTreeMap<String, Vec<u8>>) -> Result<BTreeMap<String, Vec<u8>>, String> {
        Ok(f)
    }

    #[test]
    fn the_sections_are_the_pythons() {
        let c = cases();
        let want = c["sections"].as_object().unwrap();
        assert_eq!(want.len(), SECTIONS.len());
        for (name, keys) in SECTIONS {
            let w: Vec<&str> = want[*name]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert_eq!(&w, keys, "{name}");
        }
        let mut ex: Vec<&str> = EXCLUDED_KEYS.to_vec();
        ex.sort();
        assert_eq!(json!(ex), c["excluded"]);
        for (k, s) in c["section_of"].as_object().unwrap() {
            assert_eq!(section_of(k).map(Value::from).unwrap_or(Value::Null), *s, "{k}");
        }
    }

    #[test]
    fn redact_and_build_match_the_python_bundles() {
        let c = cases();
        assert_eq!(Value::Object(redact(&config())), c["redacted"]);
        let runs: Vec<(&str, BuildOptions)> = vec![
            ("all", BuildOptions::default()),
            (
                "interface_only",
                BuildOptions {
                    sections: Some(vec!["interface".into()]),
                    ..Default::default()
                },
            ),
            (
                "with_secrets",
                BuildOptions {
                    include_secrets: true,
                    ..Default::default()
                },
            ),
            (
                "no_sections",
                BuildOptions {
                    sections: Some(vec![]),
                    ..Default::default()
                },
            ),
            (
                "plugins_section",
                BuildOptions {
                    sections: Some(vec!["plugins".into(), "layout".into()]),
                    plugins: json!({"p": {"k": 1}, "bad": 3}).as_object().unwrap().clone(),
                    plugin_files: BTreeMap::from([(
                        "p".to_string(),
                        BTreeMap::from([("a.js".to_string(), b"x".to_vec())]),
                    )]),
                    browser: json!({"corvus.a": "1", "other": "2", "corvus.b": 3})
                        .as_object()
                        .unwrap()
                        .clone(),
                    ..Default::default()
                },
            ),
        ];
        for (name, mut opts) in runs {
            opts.exported_at = "<time>".into();
            opts.version = "<version>".into();
            assert_eq!(Value::Object(build(&config(), &opts)), c["built"][name], "{name}");
        }
    }

    #[test]
    fn merge_matches_the_python_merges() {
        let c = cases();
        let current = json!({"theme": {"name": "blue"}, "http_port": 9000, "map_tokens": {"mapbox": "old", "bing": "b"},
            "ssh_connections": [{"name": "a", "host": "h", "username": "u", "password": "pw"}],
            "rtk": {"ntrip": {"host": "c", "port": 2101, "username": "n", "password": "secret"}},
            "video": {"streams": [{"url": "rtsp://x", "username": "v", "password": "cam"}]}});
        let imported = json!({"theme": {"name": "pink"}, "map_tokens": {"mapbox": "new"},
            "ssh_connections": [{"name": "a", "host": "h", "username": "u"}, {"name": "c", "host": "z"}],
            "rtk": {"ntrip": {"host": "c", "port": 2101, "username": "n", "password": ""}},
            "video": {"streams": [{"url": "rtsp://x", "username": "v"}, {"url": "rtsp://y", "username": "v"}]}});
        let all: Vec<&str> = SECTIONS.iter().map(|(n, _)| *n).collect();
        let runs: [(&str, Vec<&str>, bool); 4] = [
            ("interface", vec!["interface"], false),
            ("all_no_secrets", all.clone(), false),
            ("all_secrets", all, true),
            ("map_only", vec!["map"], false),
        ];
        for (name, sections, secrets) in runs {
            let out = merge(
                current.as_object().unwrap(),
                imported.as_object().unwrap(),
                &sections,
                secrets,
            );
            assert_eq!(Value::Object(out), c["merges"][name], "{name}");
        }
    }

    #[test]
    fn parse_matches_the_python_verdicts() {
        let c = cases();
        let inputs = json!({
            "good_v2": {"kind": "corvus-settings", "format": 2, "sections": ["interface", "map"],
                "config": {"theme": {"name": "x"}, "http_port": 1, "map": {}, "plugins": {}, "fleet": {}},
                "version": "1", "secrets": true, "browser": {"corvus.x": "1"}},
            "v1_all_sections": {"kind": "corvus-settings", "format": 1, "config": {"theme": {"name": "x"}, "http_port": 1}},
            "wrong_kind": {"kind": "other", "format": 1, "config": {}},
            "not_a_dict": [],
            "bad_format": {"kind": "corvus-settings", "format": true, "config": {}},
            "newer": {"kind": "corvus-settings", "format": 3, "config": {}},
            "no_config": {"kind": "corvus-settings", "format": 2, "sections": []},
            "no_sections": {"kind": "corvus-settings", "format": 2, "config": {}},
            "bad_logo": {"kind": "corvus-settings", "format": 2, "sections": ["interface"], "config": {}, "logo": "!!!"},
            "logo_not_png": {"kind": "corvus-settings", "format": 2, "sections": ["interface"], "config": {}, "logo": "aGVsbG8="},
            "logo_png": {"kind": "corvus-settings", "format": 2, "sections": ["interface"], "config": {},
                "logo": "iVBORw0KGgoAAAA="},
        });
        for (name, data) in inputs.as_object().unwrap() {
            let want = &c["parses"][name];
            match parse(data, &pass) {
                Ok(p) => {
                    assert_eq!(want["ok"], json!(true), "{name}");
                    let got = json!({
                        "version": p.version, "exported_at": p.exported_at, "secrets": p.secrets,
                        "sections": p.sections, "config": p.config, "plugins": p.plugins,
                        "plugin_files": {}, "logo": p.logo.as_ref().map(|l| l.iter().map(|b| format!("{b:02x}")).collect::<String>()),
                        "browser": p.browser,
                    });
                    assert_eq!(got, want["value"], "{name}");
                }
                Err(e) => {
                    assert_eq!(want["ok"], json!(false), "{name}: {e}");
                    assert_eq!(json!(e), want["error"], "{name}");
                }
            }
        }
    }

    #[test]
    fn normalisers_and_ids() {
        let c = cases();
        let mut all: Vec<String> = normalize_sections(None).unwrap().into_iter().collect();
        all.sort();
        assert_eq!(json!(all), c["normalize"]["none"]);
        let mut some: Vec<String> = normalize_sections(Some(&json!(["map", "x", 3])))
            .unwrap()
            .into_iter()
            .collect();
        some.sort();
        assert_eq!(json!(some), c["normalize"]["list"]);
        assert!(normalize_sections(Some(&json!("map"))).is_err());
        let mut ids: Vec<String> = normalize_ids(Some(&json!(["a", "z", 3])), &["a", "b"], "ids")
            .unwrap()
            .into_iter()
            .collect();
        ids.sort();
        assert_eq!(json!(ids), c["ids"]["some"]);
        assert!(is_valid_plugin_id("ssh-launcher") && !is_valid_plugin_id("-x") && !is_valid_plugin_id("a/b"));
    }

    #[test]
    fn plugin_files_are_checked_and_decoded() {
        let ok = decode_plugin_files(&json!({"p": {"a.js": "eA=="}}), &pass).unwrap();
        assert_eq!(ok["p"]["a.js"], b"x");
        assert!(decode_plugin_files(&json!({"../x": {}}), &pass).is_err());
        assert!(decode_plugin_files(&json!({"p": {"a": "!!"}}), &pass)
            .unwrap_err()
            .contains("unusable"));
        assert!(decode_plugin_files(&json!([]), &pass).is_err());
        assert_eq!(decode_plugin_files(&Value::Null, &pass).unwrap().len(), 0);
    }

    #[test]
    fn utc_iso_is_the_calendar() {
        assert_eq!(utc_iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_iso(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(utc_iso(951_782_400), "2000-02-29T00:00:00Z");
    }
}
