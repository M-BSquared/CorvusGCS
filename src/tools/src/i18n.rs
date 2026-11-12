//! Checks and starts the interface's language files (`src/lang/*.json`).
//!
//! ```text
//! corvus-build i18n check                  every language file: valid, consistent
//! corvus-build i18n new es Spanish Español  a file with every known text, blank
//! corvus-build i18n fmt                    rewrite the files sorted and indented
//! ```
//!
//! A plugin keeps its own translations in `src/plugins/<id>/lang/<code>.json`,
//! in the same format (meta is optional there). `check` and `fmt` cover those
//! files too; a plugin file is compared with the other languages of the same
//! plugin only.
//!
//! A language file maps the English text of the interface to its translation
//! (see `src/core/src/languages.rs`). The list of texts is the union of what
//! the files already carry. `new` starts a file from that union, and `check`
//! lists what a language still lacks against it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{Map, Value};

pub const DEFAULT_CODE: &str = "en";

/// The rule of `src/core/src/languages.rs`: a primary subtag, then at most two more.
pub fn is_valid_code(code: &str) -> bool {
    Regex::new(r"^[a-z]{2,3}(?:-[A-Za-z0-9]{2,8}){0,2}$")
        .unwrap()
        .is_match(code)
}

/// The `{name}` placeholders of a text.
pub fn holes(text: &str) -> BTreeSet<String> {
    Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}")
        .unwrap()
        .captures_iter(text)
        .map(|c| c[1].to_string())
        .collect()
}

/// No em dash, and no en dash or hyphen standing between words as one.
pub fn has_dash(text: &str) -> bool {
    text.contains('\u{2014}') || text.contains('\u{2013}') || text.contains(" - ")
}

/// The interface's language files.
pub fn lang_dir(root: &Path) -> PathBuf {
    root.join("src").join("lang")
}

/// The language files in `dir`, by code.
pub fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .filter(|p| p.file_stem().and_then(|s| s.to_str()).is_some_and(is_valid_code))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Every plugin's `lang` folder with its files.
pub fn plugin_files(root: &Path) -> Vec<(PathBuf, Vec<PathBuf>)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root.join("src").join("plugins")) else {
        return out;
    };
    let mut folders: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path().join("lang")))
        .filter(|p| p.is_dir())
        .collect();
    folders.sort();
    for folder in folders {
        let found = files(&folder);
        if !found.is_empty() {
            out.push((folder, found));
        }
    }
    out
}

fn load(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("not readable: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("not readable: {e}"))
}

fn keys(path: &Path) -> BTreeSet<String> {
    load(path)
        .ok()
        .and_then(|v| {
            v.get("strings")
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect())
        })
        .unwrap_or_default()
}

/// What is wrong with one file; empty when it is fine. A line starting with
/// `note:` is not an error.
pub fn problems(path: &Path, union: &BTreeSet<String>, need_meta: bool) -> Vec<String> {
    let data = match load(path) {
        Ok(v) => v,
        Err(e) => return vec![e],
    };
    let Some(obj) = data.as_object() else {
        return vec!["not a JSON object".into()];
    };
    let mut out = Vec::new();
    let named = obj
        .get("meta")
        .and_then(Value::as_object)
        .and_then(|m| m.get("name"))
        .map(|n| match n {
            Value::String(s) => !s.trim().is_empty(),
            Value::Null => false,
            other => !other.to_string().trim().is_empty(),
        })
        .unwrap_or(false);
    if need_meta && !named {
        out.push("meta.name is missing".into());
    }
    let Some(strings) = obj.get("strings").and_then(Value::as_object) else {
        out.push("strings is missing".into());
        return out;
    };
    for (key, value) in strings {
        let Some(value) = value.as_str() else {
            out.push(format!("{key:?}: the translation is not a string"));
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        if holes(key) != holes(value) {
            out.push(format!(
                "{key:?}: placeholders differ ({:?} vs {:?})",
                holes(key),
                holes(value)
            ));
        }
        if has_dash(value) && !has_dash(key) {
            out.push(format!("{key:?}: the translation uses a dash"));
        }
        if value.contains('\n') && !key.contains('\n') {
            out.push(format!("{key:?}: the translation has a line break the English lacks"));
        }
    }
    if path.file_stem().and_then(|s| s.to_str()) != Some(DEFAULT_CODE) {
        let have: BTreeSet<&String> = strings.keys().collect();
        let missing = union.iter().filter(|k| !have.contains(k)).count();
        if missing > 0 {
            out.push(format!("note: {missing} known texts have no translation yet"));
        }
    }
    out
}

fn report(label: &str, found: &[String], log: &mut dyn FnMut(String)) -> bool {
    let errors: Vec<&String> = found.iter().filter(|m| !m.starts_with("note:")).collect();
    let note = found
        .iter()
        .find(|m| m.starts_with("note:"))
        .map(|n| format!(" ({})", &n[6..]));
    log(format!(
        "{label}: {}{}",
        if errors.is_empty() { "ok" } else { "FAIL" },
        note.unwrap_or_default()
    ));
    for m in errors.iter().take(30) {
        log(format!("    {m}"));
    }
    if errors.len() > 30 {
        log(format!("    ... and {} more", errors.len() - 30));
    }
    !errors.is_empty()
}

/// `check`: every file checked; true when all are fine.
pub fn check(root: &Path, log: &mut dyn FnMut(String)) -> bool {
    let paths = files(&lang_dir(root));
    if paths.is_empty() {
        log(format!("no language files found in {}", lang_dir(root).display()));
        return false;
    }
    let union: BTreeSet<String> = paths.iter().flat_map(|p| keys(p)).collect();
    let mut failed = false;
    for p in &paths {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        failed |= report(&name, &problems(p, &union, true), log);
    }
    for (_, group) in plugin_files(root) {
        let union: BTreeSet<String> = group.iter().flat_map(|p| keys(p)).collect();
        for p in &group {
            let label = p.strip_prefix(root).unwrap_or(p).display().to_string();
            failed |= report(&label, &problems(p, &union, false), log);
        }
    }
    !failed
}

/// The order a file keeps its texts in: case blind, then as written.
fn sort_key(s: &str) -> (String, String) {
    (s.to_lowercase(), s.to_string())
}

/// The text of a language file as it is written to disk.
pub fn render(meta: Option<(&str, &str, bool)>, strings: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = strings.iter().collect();
    sorted.sort_by_key(|pair| sort_key(&pair.0));
    let mut body = Map::new();
    if let Some((name, native, last)) = meta {
        let mut m = Map::new();
        m.insert("name".into(), Value::from(name));
        m.insert("native_name".into(), Value::from(native));
        if last {
            m.insert("last".into(), Value::Bool(true));
        }
        body.insert("meta".into(), Value::Object(m));
    }
    let texts: Map<String, Value> = sorted
        .into_iter()
        .map(|(k, v)| (k.clone(), Value::from(v.as_str())))
        .collect();
    body.insert("strings".into(), Value::Object(texts));
    let mut out = serde_json::to_string_pretty(&Value::Object(body)).expect("JSON");
    out.push('\n');
    out
}

fn string_pairs(data: &Value) -> Vec<(String, String)> {
    data.get("strings")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn text_of(v: Option<&Value>, fallback: &str) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => fallback.to_string(),
        Some(other) => other.to_string(),
    }
}

/// `fmt`: every file rewritten sorted and indented.
pub fn fmt(root: &Path, log: &mut dyn FnMut(String)) -> Result<(), String> {
    for p in files(&lang_dir(root)) {
        let data = load(&p)?;
        let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
        let meta = data.get("meta");
        let name = text_of(meta.and_then(|m| m.get("name")), &stem);
        let native = text_of(meta.and_then(|m| m.get("native_name")), &name);
        let last = meta.and_then(|m| m.get("last")) == Some(&Value::Bool(true));
        std::fs::write(&p, render(Some((&name, &native, last)), &string_pairs(&data))).map_err(|e| e.to_string())?;
        log(format!("formatted {}", p.file_name().unwrap().to_string_lossy()));
    }
    for (_, group) in plugin_files(root) {
        for p in group {
            let data = load(&p)?;
            let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
            let meta = data.get("meta").filter(|m| m.is_object()).map(|m| {
                let name = text_of(m.get("name"), &stem);
                let native = text_of(m.get("native_name"), &name);
                (name, native)
            });
            let text = render(
                meta.as_ref().map(|(n, v)| (n.as_str(), v.as_str(), false)),
                &string_pairs(&data),
            );
            std::fs::write(&p, text).map_err(|e| e.to_string())?;
            log(format!("formatted {}", p.strip_prefix(root).unwrap_or(&p).display()));
        }
    }
    Ok(())
}

/// `new`: a file for `code` holding every known text, untranslated.
pub fn new_language(root: &Path, code: &str, name: &str, native: &str, force: bool) -> Result<String, String> {
    if !is_valid_code(code) {
        return Err(format!("{code:?} is not a language code such as es or pt-BR"));
    }
    let target = lang_dir(root).join(format!("{code}.json"));
    if target.exists() && !force {
        return Err(format!("{} exists; pass --force to replace it", target.display()));
    }
    let union: BTreeSet<String> = files(&lang_dir(root)).iter().flat_map(|p| keys(p)).collect();
    let blank: Vec<(String, String)> = union.iter().map(|k| (k.clone(), String::new())).collect();
    std::fs::write(&target, render(Some((name, native, false)), &blank)).map_err(|e| e.to_string())?;
    Ok(format!(
        "wrote {} with {} texts to translate",
        target.display(),
        union.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_holes_and_dashes() {
        for good in ["en", "de", "pt-BR", "zh-Hant-TW", "dl"] {
            assert!(is_valid_code(good), "{good}");
        }
        for bad in ["EN", "e", "english", "pt_BR", "a-b-c-d"] {
            assert!(!is_valid_code(bad), "{bad}");
        }
        assert_eq!(holes("{a} of {b} and {a}").into_iter().collect::<Vec<_>>(), ["a", "b"]);
        assert!(has_dash("one \u{2014} two") && has_dash("one - two") && !has_dash("one-two"));
    }

    #[test]
    fn a_file_is_written_sorted_with_meta_first() {
        let text = render(
            Some(("German", "Deutsch", false)),
            &[
                ("b".into(), "B".into()),
                ("A".into(), "Ä".into()),
                ("a".into(), "x".into()),
            ],
        );
        assert_eq!(
            text,
            "{\n  \"meta\": {\n    \"name\": \"German\",\n    \"native_name\": \"Deutsch\"\n  },\n  \"strings\": {\n    \"A\": \"Ä\",\n    \"a\": \"x\",\n    \"b\": \"B\"\n  }\n}\n"
        );
    }

    #[test]
    fn the_repository_language_files_are_clean_and_formatted() {
        let root = crate::repo_root();
        let mut log = Vec::new();
        assert!(check(&root, &mut |l| log.push(l)), "{}", log.join("\n"));
        for p in files(&lang_dir(&root)) {
            let text = std::fs::read_to_string(&p).unwrap();
            let data: Value = serde_json::from_str(&text).unwrap();
            let meta = data.get("meta").unwrap();
            let name = text_of(meta.get("name"), "");
            let native = text_of(meta.get("native_name"), &name);
            let last = meta.get("last") == Some(&Value::Bool(true));
            assert_eq!(
                render(Some((&name, &native, last)), &string_pairs(&data)),
                text,
                "{}: run corvus-build i18n fmt",
                p.display()
            );
        }
    }

    #[test]
    fn a_file_with_wrong_placeholders_or_a_dash_fails() {
        let dir = std::env::temp_dir().join(format!("corvus-i18n-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("xx.json");
        std::fs::write(&p, r#"{"meta": {"name": "X"}, "strings": {"Hi {a}": "Hallo {b}", "One": "Eins — zwei", "Two": "", "Three": 3}}"#).unwrap();
        let union: BTreeSet<String> = ["Hi {a}", "One", "Two", "Three", "Four"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let found = problems(&p, &union, true);
        assert!(found.iter().any(|m| m.contains("placeholders differ")));
        assert!(found.iter().any(|m| m.contains("uses a dash")));
        assert!(found.iter().any(|m| m.contains("not a string")));
        assert!(found.iter().any(|m| m == "note: 1 known texts have no translation yet"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
