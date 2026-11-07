//! Translation files for the interface (`languages.py`).
//!
//! A language is one JSON file named after its code. Two folders are scanned:
//! the bundled languages (compiled in, or a folder when one is given) and the
//! operator's `~/.corvus/languages`, whose file wins for the same code. The
//! English text is the key, so an untranslated text stays English. A plugin
//! adds `<plugin>/lang/<code>.json`, merged under the language's own strings.
//! Nothing here raises: a broken file costs its language, never the start.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::is_valid_language_code;
use crate::json::Map;

pub const DEFAULT_CODE: &str = "en";
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_KEY_CHARS: usize = 600;
const MAX_VALUE_CHARS: usize = 1200;
const MAX_NAME_CHARS: usize = 60;

const BUNDLED: [(&str, &str); 4] = [
    ("de", include_str!("../../lang/de.json")),
    ("dl", include_str!("../../lang/dl.json")),
    ("en", include_str!("../../lang/en.json")),
    ("fr", include_str!("../../lang/fr.json")),
];

pub const README: &str = r#"# Corvus GCS languages

Drop one file per language into this directory, then press Reload languages in
Settings > Appearance > Language (or restart Corvus).

    languages/
      es.json
      pt-BR.json

The file name is the language code. `meta` names the language and `strings`
maps the English text of the interface to its translation:

    {
      "meta": {"name": "Spanish", "native_name": "Espa\u00f1ol"},
      "strings": {
        "Settings": "Ajustes",
        "Settings of {name}": "Ajustes de {name}"
      }
    }

The English text is the key, copied exactly, so an English text that is not
listed stays English. A key with `{name}` in it matches any text in that place;
the same `{name}` in the value puts the matched text back.

A file here with the code of a language that ships with Corvus replaces it.

`python3 tools/i18n.py new <code> <name> <native name>` in a Corvus checkout
writes a file with every known text in it, ready to translate.
"#;

/// One installed language.
#[derive(Debug, Clone, PartialEq)]
pub struct Language {
    pub code: String,
    pub name: String,
    pub native_name: String,
    /// `builtin`, `bundled` or `user`.
    pub source: String,
    pub last: bool,
    pub strings: Map,
}

/// Where languages are read from.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// `~/.corvus/languages`.
    pub user_dir: Option<PathBuf>,
    /// A folder of bundled languages; the compiled-in ones when `None`.
    pub bundled_dir: Option<PathBuf>,
}

fn text(raw: Option<&Value>, limit: usize) -> String {
    raw.and_then(Value::as_str)
        .map(|s| s.trim().chars().take(limit).collect())
        .unwrap_or_default()
}

fn clean_strings(raw: Option<&Value>) -> Map {
    let mut out = Map::new();
    let Some(Value::Object(m)) = raw else { return out };
    for (k, v) in m {
        let Some(v) = v.as_str() else { continue };
        if k.trim().is_empty() || v.trim().is_empty() {
            continue;
        }
        if k.chars().count() > MAX_KEY_CHARS || v.chars().count() > MAX_VALUE_CHARS {
            continue;
        }
        out.insert(k.clone(), v.into());
    }
    out
}

fn parse(code: &str, data: &Value, source: &str) -> Option<Language> {
    let obj = data.as_object()?;
    let meta = obj.get("meta").and_then(Value::as_object);
    let name = Some(text(meta.and_then(|m| m.get("name")), MAX_NAME_CHARS))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| code.to_string());
    let native = Some(text(meta.and_then(|m| m.get("native_name")), MAX_NAME_CHARS))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| name.clone());
    Some(Language {
        code: code.to_string(),
        name,
        native_name: native,
        source: source.to_string(),
        last: meta.and_then(|m| m.get("last")) == Some(&Value::Bool(true)),
        strings: clean_strings(obj.get("strings")),
    })
}

fn read_file(path: &Path, source: &str) -> Option<Language> {
    let code = path.file_stem()?.to_str()?;
    if path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .as_deref()
        != Some("json")
        || !is_valid_language_code(code)
    {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_FILE_BYTES {
        eprintln!(
            "corvus: language file {} is larger than {MAX_FILE_BYTES} bytes; skipped",
            path.display()
        );
        return None;
    }
    let data = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let Some(data) = data else {
        eprintln!("corvus: language file {} unreadable; skipped", path.display());
        return None;
    };
    if !data.is_object() {
        eprintln!("corvus: language file {} is not a JSON object; skipped", path.display());
        return None;
    }
    parse(code, &data, source)
}

fn scan_dir(dir: &Path, source: &str) -> Vec<Language> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
    names.sort();
    names.iter().filter_map(|p| read_file(p, source)).collect()
}

fn scan_bundled(sources: &Sources) -> Vec<Language> {
    match &sources.bundled_dir {
        Some(dir) => scan_dir(dir, "bundled"),
        None => BUNDLED
            .iter()
            .filter_map(|(code, t)| parse(code, &serde_json::from_str::<Value>(t).ok()?, "bundled"))
            .collect(),
    }
}

fn all(sources: &Sources) -> Vec<Language> {
    let mut found: Vec<Language> = Vec::new();
    let mut put = |l: Language| match found.iter_mut().find(|e| e.code == l.code) {
        Some(e) => *e = l,
        None => found.push(l),
    };
    scan_bundled(sources).into_iter().for_each(&mut put);
    if let Some(user) = &sources.user_dir {
        scan_dir(user, "user").into_iter().for_each(&mut put);
    }
    if !found.iter().any(|l| l.code == DEFAULT_CODE) {
        found.push(Language {
            code: DEFAULT_CODE.into(),
            name: "English".into(),
            native_name: "English".into(),
            source: "builtin".into(),
            last: false,
            strings: Map::new(),
        });
    }
    found
}

/// The installed languages as the rows `GET /api/languages` lists: English
/// first, then by name, a `"last": true` language at the end.
pub fn discover(sources: &Sources) -> Vec<Value> {
    let mut langs = all(sources);
    langs.sort_by_key(|l| (l.code != DEFAULT_CODE, l.last, l.name.to_lowercase(), l.code.clone()));
    langs
        .iter()
        .map(|l| {
            let mut row = Map::new();
            row.insert("code".into(), l.code.clone().into());
            row.insert("name".into(), l.name.clone().into());
            row.insert("native_name".into(), l.native_name.clone().into());
            if l.source == "builtin" {
                row.insert("last".into(), l.last.into());
                row.insert("source".into(), l.source.clone().into());
            } else {
                row.insert("source".into(), l.source.clone().into());
                row.insert("last".into(), l.last.into());
            }
            row.insert("count".into(), l.strings.len().into());
            Value::Object(row)
        })
        .collect()
}

/// The strings every plugin under `roots` has for `code`; a later root wins.
fn plugin_strings(code: &str, roots: &[PathBuf]) -> Map {
    let mut out = Map::new();
    for root in roots {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        let mut folders: Vec<String> = rd
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        folders.sort();
        for folder in folders {
            let path = root.join(&folder).join("lang").join(format!("{code}.json"));
            let ok = path.metadata().is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE_BYTES);
            if !ok {
                continue;
            }
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            {
                Some(Value::Object(data)) => out.extend(clean_strings(data.get("strings"))),
                Some(_) => {}
                None => eprintln!("corvus: plugin language file {} unreadable; skipped", path.display()),
            }
        }
    }
    out
}

/// The language `code` with its strings (plugin strings under the file's
/// own), or `None` when it is not installed.
pub fn load(code: &str, sources: &Sources, plugin_dirs: &[PathBuf]) -> Option<Language> {
    if !is_valid_language_code(code) {
        return None;
    }
    let mut entry = all(sources).into_iter().find(|l| l.code == code)?;
    let mut strings = plugin_strings(code, plugin_dirs);
    if strings.is_empty() {
        return Some(entry);
    }
    strings.extend(entry.strings);
    entry.strings = strings;
    Some(entry)
}

/// Creates `dir` with its README (when absent). Never fails: a read-only home
/// costs the folder, not the program.
pub fn ensure_user_dir(dir: &Path) {
    let result = std::fs::create_dir_all(dir).and_then(|_| {
        let readme = dir.join("README.md");
        if readme.exists() {
            Ok(())
        } else {
            std::fs::write(readme, README)
        }
    });
    if let Err(e) = result {
        eprintln!("corvus: could not create language folder {}: {e}", dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cases() -> Value {
        serde_json::from_str(include_str!("../tests/data/languages_cases.json")).unwrap()
    }

    fn materialize(cases: &Value) -> PathBuf {
        let root = std::env::temp_dir().join(format!("corvus-core-lang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (group, files) in cases["files"].as_object().unwrap() {
            for (name, body) in files.as_object().unwrap() {
                let path = root.join(group).join(name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, body.as_str().unwrap()).unwrap();
            }
        }
        std::fs::write(
            root.join("bundled/big.json"),
            format!(
                "{{\"meta\": {{\"name\": \"Big\"}}}}{}",
                " ".repeat(MAX_FILE_BYTES as usize + 1)
            ),
        )
        .unwrap();
        root
    }

    fn as_value(l: &Language) -> Value {
        json!({"code": l.code, "name": l.name, "native_name": l.native_name, "source": l.source, "last": l.last, "strings": l.strings})
    }

    #[test]
    fn discovery_and_loading_match_the_python_ones() {
        let c = cases();
        let root = materialize(&c);
        let both = Sources {
            user_dir: Some(root.join("user")),
            bundled_dir: Some(root.join("bundled")),
        };
        assert_eq!(Value::Array(discover(&both)), c["discover_both"]);
        let bundled = Sources {
            user_dir: Some(root.join("none")),
            bundled_dir: Some(root.join("bundled")),
        };
        assert_eq!(Value::Array(discover(&bundled)), c["discover_bundled"]);
        let none = Sources {
            user_dir: Some(root.join("none")),
            bundled_dir: Some(root.join("none")),
        };
        assert_eq!(Value::Array(discover(&none)), c["discover_nothing"]);
        let plugins = vec![root.join("plugins_a"), root.join("plugins_b"), root.join("missing")];
        for (key, want) in c["loads"].as_object().unwrap() {
            let (code, with) = key.rsplit_once('|').unwrap();
            let dirs: &[PathBuf] = if with == "1" { &plugins } else { &[] };
            let got = load(code, &both, dirs).map(|l| as_value(&l)).unwrap_or(Value::Null);
            assert_eq!(&got, want, "{key}");
        }
        for (code, ok) in c["valid_codes"].as_object().unwrap() {
            assert_eq!(is_valid_language_code(code), ok.as_bool().unwrap(), "{code:?}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_compiled_in_languages_are_the_four_files() {
        let s = Sources::default();
        let rows = discover(&s);
        let codes: Vec<&str> = rows.iter().filter_map(|r| r["code"].as_str()).collect();
        assert_eq!(codes[0], "en");
        for c in ["de", "fr", "dl"] {
            assert!(codes.contains(&c), "{c}");
        }
        assert!(load("de", &s, &[]).unwrap().strings.len() > 100);
        assert_eq!(rows.last().unwrap()["code"], "dl");
    }

    #[test]
    fn the_user_folder_gets_its_readme_once() {
        let dir = std::env::temp_dir().join(format!("corvus-core-lang-readme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_user_dir(&dir);
        assert_eq!(std::fs::read_to_string(dir.join("README.md")).unwrap(), README);
        std::fs::write(dir.join("README.md"), "mine").unwrap();
        ensure_user_dir(&dir);
        assert_eq!(std::fs::read_to_string(dir.join("README.md")).unwrap(), "mine");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_readme_is_the_pythons() {
        assert_eq!(json!(README), cases()["readme"]);
    }
}
