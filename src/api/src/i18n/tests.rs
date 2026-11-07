use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::*;

#[test]
fn english_is_the_identity() {
    assert_eq!(tr("Settings"), "Settings");
    assert_eq!(tr("Anything at all {x}"), "Anything at all {x}");
    assert!(matches!(tr("Settings"), Cow::Borrowed(_)));
}

#[test]
fn german_translates_and_keeps_outer_whitespace() {
    with_language("de", || {
        assert_eq!(tr("Settings"), "Einstellungen");
        assert_eq!(tr("  Settings\n"), "  Einstellungen\n");
        assert_eq!(tr("Interface   language"), "Oberflächensprache");
    });
}

#[test]
fn french_and_the_joke_language_load() {
    with_language("fr", || assert_eq!(tr("Settings"), "Réglages"));
    with_language("dl", || assert_ne!(tr("Settings"), "Settings"));
}

#[test]
fn a_missing_key_stays_english_and_never_panics() {
    with_language("de", || {
        for odd in [
            "This text is in no language file at all",
            "",
            " ",
            "{",
            "{}",
            "{name}",
            "(NAV_X)",
            " (ABC)",
            "a (NAV_RCL_ACT)",
            "\u{0}\u{1f980}",
            "Settings of ",
            &"x".repeat(5000),
        ] {
            assert_eq!(tr(odd), odd);
        }
        assert!(missing().iter().all(|m| !m.is_empty()));
    });
}

#[test]
fn placeholders_are_filled_from_the_match_and_from_the_caller() {
    with_language("de", || {
        assert_eq!(
            tr("Settings of Vibration Monitor"),
            "Einstellungen von Vibrationsmonitor"
        );
        assert_eq!(
            tr_fmt("Settings of {name}", &[("name", "Gate")]),
            "Einstellungen von Gate"
        );
        assert_eq!(
            tr_fmt("Reloaded. {n} languages installed.", &[("n", "4")]),
            "Neu geladen. 4 Sprachen installiert."
        );
        assert_eq!(
            tr_fmt("Reloaded. {n} languages installed.", &[]),
            "Neu geladen. {n} Sprachen installiert."
        );
    });
    assert_eq!(tr_fmt("Settings of {name}", &[("name", "Gate")]), "Settings of Gate");
}

#[test]
fn a_parameter_label_is_translated_by_the_label() {
    with_language("de", || {
        assert_eq!(
            tr("Action on RC loss (NAV_RCL_ACT)"),
            "Aktion bei RC-Verlust (NAV_RCL_ACT)"
        );
    });
}

#[test]
fn lines_of_a_text_with_breaks_are_translated_one_by_one() {
    with_language("de", || {
        assert_eq!(tr("Settings\nLanguage"), "Einstellungen\nSprache");
        assert_eq!(tr("Settings\nno such text here"), "Einstellungen\nno such text here");
    });
}

#[test]
fn text_without_a_letter_is_left_alone() {
    with_language("de", || {
        for t in ["12.5", "--", "100 %", "\u{2026}"] {
            assert_eq!(tr(t), t);
        }
    });
}

#[test]
fn plugin_texts_are_merged_under_the_language() {
    files::add_plugin_files(&[("de", r#"{"strings":{"Plugin text":"Plugin-Text"}}"#)]);
    let de = files::plugin_strings("de");
    assert_eq!(de.get("Plugin text").map(String::as_str), Some("Plugin-Text"));
    assert!(files::plugin_strings("xx").is_empty());
}

#[test]
fn config_names_the_language() {
    assert_eq!(from_config(&serde_json::json!({ "language": "fr" })), "fr");
    assert_eq!(from_config(&serde_json::json!({ "language": "" })), "en");
    assert_eq!(from_config(&serde_json::json!({})), "en");
    assert_eq!(from_config(&serde_json::json!({ "language": 3 })), "en");
}

#[test]
fn every_language_file_keeps_its_placeholders_and_has_no_dash() {
    let hole = regex::Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap();
    for l in files::discover(None).iter().filter(|l| l.code != "en") {
        for (k, v) in &l.strings {
            let names = |s: &str| -> BTreeSet<String> { hole.captures_iter(s).map(|c| c[1].to_string()).collect() };
            assert_eq!(names(k), names(v), "{}: placeholders of {k:?}", l.code);
            assert!(
                !v.contains('\u{2014}') && !v.contains('\u{2013}') && !v.contains(" - "),
                "{}: dash in {v:?}",
                l.code
            );
        }
    }
}

#[test]
fn the_bundled_files_are_the_web_interfaces_files() {
    let root = &repo_root();
    for code in ["en", "de", "fr", "dl"] {
        let ours = std::fs::read_to_string(root.join("src/lang").join(format!("{code}.json"))).unwrap();
        let web = std::fs::read_to_string(root.join("python/src/lang").join(format!("{code}.json"))).unwrap();
        assert_eq!(ours, web, "{code}.json differs between src/lang and python/src/lang");
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for e in read.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "link" || n == "lang") {
                continue;
            }
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The string literals handed to `tr(` and `tr_fmt(` in the program.
fn used_keys() -> Vec<(PathBuf, String)> {
    let call = regex::Regex::new(r#"\btr(?:_fmt)?\(\s*"((?:[^"\\]|\\.)*)""#).unwrap();
    let mut files = Vec::new();
    rust_files(&repo_root().join("src"), &mut files);
    let mut out = Vec::new();
    for f in files {
        if f.ends_with("i18n/tests.rs") {
            continue;
        }
        let body = std::fs::read_to_string(&f).unwrap();
        for c in call.captures_iter(&body) {
            let lit: Value = serde_json::from_str(&format!("\"{}\"", &c[1])).unwrap_or(Value::Null);
            out.push((f.clone(), lit.as_str().unwrap_or(&c[1]).to_string()));
        }
    }
    out
}

#[test]
fn every_key_used_in_the_program_is_in_the_language_files() {
    let langs = files::discover(None);
    let keys = used_keys();
    let mut absent = Vec::new();
    for (file, key) in &keys {
        let k = normalize(key);
        for l in langs.iter().filter(|l| l.code != "en") {
            let present = l.strings.keys().any(|s| normalize(s) == k);
            if !present {
                absent.push(format!("{}: {k:?} is not in {}.json", file.display(), l.code));
            }
        }
    }
    assert!(absent.is_empty(), "keys with no entry:\n{}", absent.join("\n"));
}

#[test]
fn text_set_in_capitals_is_found_under_its_web_wording() {
    with_language("de", || {
        assert_eq!(tr("SETTINGS"), "EINSTELLUNGEN");
    });
}
