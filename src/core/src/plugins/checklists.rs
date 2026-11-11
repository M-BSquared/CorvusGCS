//! The operator's preflight checklists: what is kept, and its bounds
//! (`checklists.py`).
//!
//! A checklist is the operator's own text. Nothing here interprets an item or
//! acts on one: ticking an item off happens in the interface and is not stored,
//! because it belongs to one flight rather than to the station.
//!
//! Stored under the `checklists` key of the config. Every key is optional and
//! kept independently. `lists` absent means the operator never wrote one and the
//! interface offers its standard list; `lists` present and empty means they
//! deleted every list, which stays that way.

use std::collections::HashSet;

use serde_json::Value;

use crate::json::Map;

pub const MAX_LISTS: usize = 32;
pub const MAX_ITEMS: usize = 120;
pub const MAX_NAME: usize = 80;
pub const MAX_TEXT: usize = 200;

/// `^[A-Za-z0-9_-]{1,40}$`; like Python's `$` it lets one trailing newline pass.
fn is_id(s: &str) -> bool {
    let s = s.strip_suffix('\n').unwrap_or(s);
    (1..=40).contains(&s.chars().count()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// One line of text, trimmed and cut to `limit`; "" for anything else. Control
/// characters become one space so an item stays one row.
fn clean_text(value: Option<&Value>, limit: usize) -> String {
    let Some(Value::String(text)) = value else {
        return String::new();
    };
    let mut out = String::with_capacity(text.len());
    let mut in_control = false;
    for c in text.chars() {
        if (c as u32) < 0x20 || c as u32 == 0x7f {
            if !in_control {
                out.push(' ');
            }
            in_control = true;
        } else {
            out.push(c);
            in_control = false;
        }
    }
    let mut squeezed = String::with_capacity(out.len());
    let mut spaces = 0;
    for c in out.chars() {
        if c == ' ' {
            spaces += 1;
            if spaces == 1 {
                squeezed.push(c);
            }
        } else {
            spaces = 0;
            squeezed.push(c);
        }
    }
    first_chars(squeezed.trim(), limit).trim_end().to_string()
}

fn coerce_item(raw: &Value) -> Option<Value> {
    let text = match raw {
        Value::String(_) => clean_text(Some(raw), MAX_TEXT),
        Value::Object(o) => clean_text(o.get("text"), MAX_TEXT),
        _ => return None,
    };
    if text.is_empty() {
        return None;
    }
    let heading = raw.get("heading") == Some(&Value::Bool(true));
    let mut m = Map::new();
    m.insert("text".into(), text.into());
    m.insert("heading".into(), heading.into());
    Some(Value::Object(m))
}

fn coerce_list(raw: &Value, index: usize, taken: &mut HashSet<String>) -> Option<Value> {
    let raw = raw.as_object()?;
    let mut id = match raw.get("id") {
        Some(Value::String(s)) if is_id(s) && !taken.contains(s) => s.clone(),
        _ => {
            let mut id = format!("list{}", index + 1);
            let mut suffix = 1;
            while taken.contains(&id) {
                suffix += 1;
                id = format!("list{}_{suffix}", index + 1);
            }
            id
        }
    };
    taken.insert(id.clone());
    let mut name = clean_text(raw.get("name"), MAX_NAME);
    if name.is_empty() {
        name = format!("Checklist {}", index + 1);
    }
    let mut items = Vec::new();
    if let Some(Value::Array(entries)) = raw.get("items") {
        for entry in entries {
            if let Some(item) = coerce_item(entry) {
                items.push(item);
            }
            if items.len() >= MAX_ITEMS {
                break;
            }
        }
    }
    let mut m = Map::new();
    m.insert("id".into(), std::mem::take(&mut id).into());
    m.insert("name".into(), name.into());
    m.insert("items".into(), Value::Array(items));
    Some(Value::Object(m))
}

/// Keeps the known checklist keys with bounded values; `None` when none are left.
/// Booleans are genuine booleans only: `"false"` in a hand edited file is not on.
pub fn coerce(raw: &Value) -> Option<Map> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    for key in ["enabled", "home_window"] {
        if let Some(Value::Bool(b)) = raw.get(key) {
            out.insert(key.into(), (*b).into());
        }
    }
    if let Some(Value::Array(entries)) = raw.get("lists") {
        let mut taken = HashSet::new();
        let mut lists = Vec::new();
        for (index, entry) in entries.iter().take(MAX_LISTS).enumerate() {
            if let Some(list) = coerce_list(entry, index, &mut taken) {
                lists.push(list);
            }
        }
        out.insert("lists".into(), Value::Array(lists));
    }
    if let Some(Value::String(active)) = raw.get("active") {
        if is_id(active) {
            out.insert("active".into(), active.clone().into());
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_is_one_trimmed_line() {
        assert_eq!(clean_text(Some(&json!("  a\n\tb   c  ")), 200), "a b c");
        assert_eq!(clean_text(Some(&json!(5)), 200), "");
        assert_eq!(clean_text(Some(&json!("abcdef ")), 3), "abc");
        assert_eq!(clean_text(Some(&json!("ab  cd")), 3), "ab");
    }

    #[test]
    fn an_id_may_end_in_one_newline_as_python_allows() {
        assert!(is_id("abc\n"));
        assert!(!is_id("abc\n\n"));
        assert!(!is_id(""));
        assert!(!is_id(&"a".repeat(41)));
    }

    #[test]
    fn repeated_ids_are_replaced_and_lists_cut() {
        let got = coerce(&json!({"lists": [{"id": "a"}, {"id": "a", "name": "x"}, 5, {}]})).unwrap();
        let ids: Vec<&str> = got["lists"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["a", "list2", "list4"]);
        assert!(coerce(&json!({"enabled": "false"})).is_none());
        assert!(coerce(&json!([])).is_none());
    }
}
