//! The lookup structure of one language: an exact map, and the keys that carry
//! `{name}` as patterns. A port of `compile` / `lookup` in
//! `python/src/js/i18n.js`, so a key resolves the way the web interface
//! resolves it.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

/// Longest text a pattern is tried on; longer text is exact match only.
const TEXT_MAX: usize = 400;
/// How deep a `{name}` is translated inside a `{name}`.
const DEPTH_MAX: usize = 2;

/// The lookup key of a piece of text: whitespace folded and trimmed.
pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `{name}` holes of `text` as (start, end, name).
fn holes(text: &str) -> Vec<(usize, usize, String)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            let mut j = i + 1;
            if j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == b'_') {
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                if j < b.len() && b[j] == b'}' {
                    out.push((i, j + 1, text[i + 1..j].to_string()));
                    i = j + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// `template` with each `{name}` replaced from `vars`; an unknown name stays.
pub fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    if vars.is_empty() {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len());
    let mut last = 0;
    for (start, end, name) in holes(template) {
        out.push_str(&template[last..start]);
        match vars.iter().find(|(n, _)| *n == name) {
            Some((_, v)) => out.push_str(v),
            None => out.push_str(&template[start..end]),
        }
        last = end;
    }
    out.push_str(&template[last..]);
    out
}

struct Pattern {
    source: String,
    re: OnceLock<Option<Regex>>,
    names: Vec<String>,
    value: String,
    prefix: String,
    suffix: String,
    weight: usize,
}

impl Pattern {
    fn regex(&self) -> Option<&Regex> {
        self.re.get_or_init(|| Regex::new(&self.source).ok()).as_ref()
    }
}

/// One language's strings, ready to look up.
pub struct Table {
    exact: HashMap<String, String>,
    patterns: Vec<Pattern>,
}

/// `Action on RC loss (NAV_RCL_ACT)` split into the label and the parameter.
fn param_suffix(key: &str) -> Option<(&str, &str)> {
    let body = key.strip_suffix(')')?;
    let at = body.rfind(" (")?;
    let (label, name) = (&body[..at], &body[at + 2..]);
    let ok = name.len() >= 3
        && name.as_bytes()[0].is_ascii_uppercase()
        && name
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_');
    (ok && !label.is_empty()).then_some((label, name))
}

impl Table {
    /// The table of a `strings` map. An empty translation is an untranslated
    /// entry and is left out, as is a key made only of holes.
    pub fn new<'a>(strings: impl IntoIterator<Item = (&'a String, &'a String)>) -> Table {
        let mut exact = HashMap::new();
        let mut patterns = Vec::new();
        for (raw_key, value) in strings {
            if value.trim().is_empty() {
                continue;
            }
            let key = normalize(raw_key);
            if key.is_empty() {
                continue;
            }
            let hs = holes(&key);
            if hs.is_empty() {
                exact.insert(key, value.clone());
                continue;
            }
            let mut source = String::from("^(?s)");
            let mut last = 0;
            let mut names = Vec::new();
            for (s, e, name) in &hs {
                source.push_str(&regex::escape(&key[last..*s]));
                source.push_str("(.+?)");
                names.push(name.clone());
                last = *e;
            }
            let mut bare = String::new();
            let mut at = 0;
            for (s, e, _) in &hs {
                bare.push_str(&key[at..*s]);
                at = *e;
            }
            bare.push_str(&key[at..]);
            if bare.trim().is_empty() {
                continue;
            }
            let suffix = key[last..].to_string();
            source.push_str(&regex::escape(&suffix));
            source.push('$');
            patterns.push(Pattern {
                source,
                re: OnceLock::new(),
                names,
                value: value.clone(),
                prefix: key[..hs[0].0].to_string(),
                suffix,
                weight: key.len(),
            });
        }
        patterns.sort_by(|a, b| b.weight.cmp(&a.weight));
        Table { exact, patterns }
    }

    /// How many entries it holds.
    pub fn len(&self) -> usize {
        self.exact.len() + self.patterns.len()
    }

    /// The translation of the normalized `key`, or None.
    ///
    /// What a `{name}` matched is translated too when the language has an
    /// entry for it, and a label that names its parameter is translated by the
    /// label alone (`Action on RC loss (NAV_RCL_ACT)`).
    pub fn lookup(&self, key: &str) -> Option<String> {
        self.lookup_at(key, 0)
    }

    fn lookup_at(&self, key: &str, depth: usize) -> Option<String> {
        if key.is_empty() {
            return None;
        }
        if let Some(hit) = self.exact.get(key) {
            return Some(hit.clone());
        }
        if depth > DEPTH_MAX {
            return None;
        }
        if let Some((label, name)) = param_suffix(key) {
            return self.lookup_at(label, depth + 1).map(|l| format!("{l} ({name})"));
        }
        if key.len() > TEXT_MAX {
            return None;
        }
        for p in &self.patterns {
            if !p.prefix.is_empty() && !key.starts_with(&p.prefix) {
                continue;
            }
            if !p.suffix.is_empty() && !key.ends_with(&p.suffix) {
                continue;
            }
            let Some(caps) = p.regex().and_then(|re| re.captures(key)) else {
                continue;
            };
            let inner: Vec<String> = (0..p.names.len())
                .map(|n| {
                    let m = caps.get(n + 1).map_or("", |m| m.as_str());
                    self.lookup_at(&normalize(m), depth + 1)
                        .unwrap_or_else(|| m.to_string())
                })
                .collect();
            let vars: Vec<(&str, &str)> = p
                .names
                .iter()
                .map(String::as_str)
                .zip(inner.iter().map(String::as_str))
                .collect();
            return Some(fill(&p.value, &vars));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(pairs: &[(&str, &str)]) -> Table {
        let owned: Vec<(String, String)> = pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        Table::new(owned.iter().map(|(a, b)| (a, b)))
    }

    #[test]
    fn exact_and_whitespace() {
        let t = table(&[("Settings", "Einstellungen")]);
        assert_eq!(t.lookup(&normalize("  Settings \n")), Some("Einstellungen".into()));
        assert_eq!(t.lookup("Other"), None);
    }

    #[test]
    fn pattern_fills_and_translates_the_hole() {
        let t = table(&[
            ("Settings of {name}", "Einstellungen von {name}"),
            ("Vibration", "Vibration"),
        ]);
        assert_eq!(t.lookup("Settings of Gate"), Some("Einstellungen von Gate".into()));
        let t = table(&[("Settings of {name}", "Einstellungen von {name}"), ("Gate", "Tor")]);
        assert_eq!(t.lookup("Settings of Gate"), Some("Einstellungen von Tor".into()));
    }

    #[test]
    fn longest_pattern_wins() {
        let t = table(&[
            ("{a} items", "{a} Eintraege"),
            ("Delete {a} items", "{a} Eintraege loeschen"),
        ]);
        assert_eq!(t.lookup("Delete 3 items"), Some("3 Eintraege loeschen".into()));
    }

    #[test]
    fn parameter_suffix_is_translated_by_label() {
        let t = table(&[("Action on RC loss", "Aktion bei RC Verlust")]);
        assert_eq!(
            t.lookup("Action on RC loss (NAV_RCL_ACT)"),
            Some("Aktion bei RC Verlust (NAV_RCL_ACT)".into())
        );
        assert_eq!(t.lookup("Unknown (NAV_RCL_ACT)"), None);
    }

    #[test]
    fn empty_translation_and_hole_only_keys_are_dropped() {
        let t = table(&[("A", ""), ("{x}", "{x}"), ("B", "C")]);
        assert_eq!(t.len(), 1);
        assert_eq!(t.lookup("A"), None);
    }

    #[test]
    fn fill_leaves_unknown_names() {
        assert_eq!(fill("{a} and {b}", &[("a", "1")]), "1 and {b}");
        assert_eq!(fill("no holes", &[("a", "1")]), "no holes");
    }
}
