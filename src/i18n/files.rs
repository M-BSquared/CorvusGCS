//! Language files: the ones compiled into the program (`src/lang`), the
//! operator's own in `~/.corvus/languages`, and the plugin files merged under
//! a language of the same code. The rules are `python/corvus/languages.py`'s.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub const DEFAULT_CODE: &str = "en";

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_KEY_CHARS: usize = 600;
const MAX_VALUE_CHARS: usize = 1200;
const MAX_NAME_CHARS: usize = 60;

/// The languages that ship with the program, compiled in so the binary needs
/// no folder beside it.
const BUNDLED: &[(&str, &str)] = &[
    ("en", include_str!("../lang/en.json")),
    ("de", include_str!("../lang/de.json")),
    ("fr", include_str!("../lang/fr.json")),
    ("dl", include_str!("../lang/dl.json")),
];

/// The files of the plugins, by language code. A plugin's entry never wins
/// over the program's own for the same English text.
const PLUGIN_FILES: &[(&str, &str)] = &[
    ("de", include_str!("../plugins/map_entities/lang/de.json")),
    ("fr", include_str!("../plugins/map_entities/lang/fr.json")),
    ("dl", include_str!("../plugins/map_entities/lang/dl.json")),
];

/// One language file, validated.
#[derive(Debug, Clone)]
pub struct Language {
    pub code: String,
    pub name: String,
    pub native_name: String,
    pub last: bool,
    pub strings: HashMap<String, String>,
}

impl Language {
    /// How the dropdown lists it: its own name first ("Deutsch (German)").
    pub fn label(&self) -> String {
        if self.name != self.native_name {
            format!("{} ({})", self.native_name, self.name)
        } else {
            self.native_name.clone()
        }
    }

    fn english() -> Language {
        Language {
            code: DEFAULT_CODE.into(),
            name: "English".into(),
            native_name: "English".into(),
            last: false,
            strings: HashMap::new(),
        }
    }
}

/// Whether `code` is safe as a file name: a primary subtag, then at most two
/// more of two to eight letters or digits.
pub fn is_valid_code(code: &str) -> bool {
    let mut parts = code.split('-');
    let primary = parts.next().unwrap_or("");
    if !(2..=3).contains(&primary.len()) || !primary.bytes().all(|c| c.is_ascii_lowercase()) {
        return false;
    }
    let rest: Vec<&str> = parts.collect();
    rest.len() <= 2
        && rest
            .iter()
            .all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_alphanumeric()))
}

/// The user folder: `~/.corvus/languages`.
pub fn user_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".corvus").join("languages"))
}

fn text(v: Option<&Value>, limit: usize) -> String {
    v.and_then(Value::as_str)
        .map(|s| s.trim().chars().take(limit).collect())
        .unwrap_or_default()
}

/// The usable entries of a `strings` block: a non-string, an empty one and one
/// over the caps are dropped, so one bad line costs a line, not a language.
pub fn clean_strings(raw: Option<&Value>) -> HashMap<String, String> {
    let Some(map) = raw.and_then(Value::as_object) else {
        return HashMap::new();
    };
    map.iter()
        .filter_map(|(k, v)| {
            let v = v.as_str()?;
            let ok = !k.trim().is_empty()
                && !v.trim().is_empty()
                && k.chars().count() <= MAX_KEY_CHARS
                && v.chars().count() <= MAX_VALUE_CHARS;
            ok.then(|| (k.clone(), v.to_string()))
        })
        .collect()
}

/// A language from the JSON `body` of the file named `code`, or None.
pub fn parse(code: &str, body: &str) -> Option<Language> {
    if !is_valid_code(code) {
        return None;
    }
    let data: Value = serde_json::from_str(body).ok()?;
    let obj = data.as_object()?;
    let meta = obj.get("meta").and_then(Value::as_object);
    let field = |k: &str, limit: usize| text(meta.and_then(|m| m.get(k)), limit);
    let name = Some(field("name", MAX_NAME_CHARS))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| code.to_string());
    let native = Some(field("native_name", MAX_NAME_CHARS))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| name.clone());
    Some(Language {
        code: code.to_string(),
        name,
        native_name: native,
        last: meta.and_then(|m| m.get("last")).and_then(Value::as_bool) == Some(true),
        strings: clean_strings(obj.get("strings")),
    })
}

fn scan(dir: &Path, into: &mut HashMap<String, Language>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for p in paths {
        let (Some(stem), Some(ext)) = (
            p.file_stem().and_then(|s| s.to_str()),
            p.extension().and_then(|s| s.to_str()),
        ) else {
            continue;
        };
        if !ext.eq_ignore_ascii_case("json") || std::fs::metadata(&p).map_or(true, |m| m.len() > MAX_FILE_BYTES) {
            continue;
        }
        if let Some(lang) = std::fs::read_to_string(&p).ok().and_then(|b| parse(stem, &b)) {
            into.insert(lang.code.clone(), lang);
        }
    }
}

/// Every installed language, English first, then by name; a language marked
/// `last` goes to the end. `user` is the operator's folder, if any.
pub fn discover(user: Option<&Path>) -> Vec<Language> {
    let mut found: HashMap<String, Language> = HashMap::new();
    for (code, body) in BUNDLED {
        if let Some(l) = parse(code, body) {
            found.insert(l.code.clone(), l);
        }
    }
    if let Some(dir) = user {
        scan(dir, &mut found);
    }
    found.entry(DEFAULT_CODE.into()).or_insert_with(Language::english);
    let mut rows: Vec<Language> = found.into_values().collect();
    rows.sort_by(|a, b| {
        (a.code != DEFAULT_CODE, a.last, a.name.to_lowercase(), &a.code).cmp(&(
            b.code != DEFAULT_CODE,
            b.last,
            b.name.to_lowercase(),
            &b.code,
        ))
    });
    rows
}

/// The strings the plugins have for `code`, to merge under the language's own.
pub fn plugin_strings(code: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (c, body) in PLUGIN_FILES {
        if *c != code {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(body) {
            out.extend(clean_strings(v.get("strings")));
        }
    }
    out
}

/// The installed code that best answers `wanted`: the same code in any case,
/// else the same primary language ("de-AT" is served by "de"), else None.
pub fn match_code(wanted: &str, codes: &[String]) -> Option<String> {
    let w = wanted.trim().to_lowercase();
    if w.is_empty() {
        return None;
    }
    if let Some(c) = codes.iter().find(|c| c.to_lowercase() == w) {
        return Some(c.clone());
    }
    let primary = w.split('-').next().unwrap_or("");
    if let Some(c) = codes.iter().find(|c| c.to_lowercase() == primary) {
        return Some(c.clone());
    }
    codes
        .iter()
        .find(|c| c.to_lowercase().split('-').next() == Some(primary))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        for ok in ["de", "fr", "pt-BR", "zh-Hant-TW", "dl"] {
            assert!(is_valid_code(ok), "{ok}");
        }
        for bad in ["", "d", "DE", "../x", "de/x", ".de", "de-", "toolong", "a-b-c-d-e"] {
            assert!(!is_valid_code(bad), "{bad}");
        }
    }

    #[test]
    fn parse_cleans_and_names() {
        let body = r#"{"meta":{"name":"German","native_name":"Deutsch"},
            "strings":{"A":"B","C":"","  ":"x","D":5}}"#;
        let l = parse("de", body).unwrap();
        assert_eq!(l.label(), "Deutsch (German)");
        assert_eq!(l.strings.len(), 1);
        assert!(parse("DE", body).is_none());
        assert!(parse("de", "not json").is_none());
        assert!(parse("de", "[]").is_none());
        let bare = parse("es", r#"{"strings":{}}"#).unwrap();
        assert_eq!(bare.name, "es");
    }

    #[test]
    fn bundled_languages_are_listed_english_first() {
        let list = discover(None);
        let codes: Vec<&str> = list.iter().map(|l| l.code.as_str()).collect();
        assert_eq!(codes[0], "en");
        for c in ["de", "fr", "dl"] {
            assert!(codes.contains(&c), "{c}");
        }
        assert_eq!(codes.last(), Some(&"dl"), "the joke language sorts last");
        assert!(list.iter().find(|l| l.code == "de").unwrap().strings.len() > 2000);
    }

    #[test]
    fn user_file_replaces_a_bundled_one() {
        let dir = std::env::temp_dir().join(format!("corvus-lang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("de.json"),
            r#"{"meta":{"name":"German"},"strings":{"Settings":"Optionen"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("es.json"),
            r#"{"meta":{"name":"Spanish","native_name":"Español"},"strings":{}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("broken.json"), "{").unwrap();
        let list = discover(Some(&dir));
        let de = list.iter().find(|l| l.code == "de").unwrap();
        assert_eq!(de.strings.get("Settings").map(String::as_str), Some("Optionen"));
        assert!(list.iter().any(|l| l.code == "es"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn match_code_finds_the_closest() {
        let codes: Vec<String> = ["en", "de", "pt-BR"].iter().map(|s| s.to_string()).collect();
        assert_eq!(match_code("DE", &codes).as_deref(), Some("de"));
        assert_eq!(match_code("de-AT", &codes).as_deref(), Some("de"));
        assert_eq!(match_code("pt", &codes).as_deref(), Some("pt-BR"));
        assert_eq!(match_code("xx", &codes), None);
        assert_eq!(match_code("", &codes), None);
    }
}
