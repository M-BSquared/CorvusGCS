//! The options a plugin declares and the operator sets from its gear in
//! Settings (`cleanOptionSpecs`, `coerceOption`, `emptyOption` and
//! `valuesFor` of `python/src/js/plugins.js`). Values are saved under
//! `_options` in the plugin's own config, where `save_settings` never
//! reaches them.

use serde_json::{json, Map, Value};

/// Where the operator's option values live inside a plugin's saved settings.
pub const OPTIONS_KEY: &str = "_options";
/// The one option Corvus adds by itself, for a plugin that can have a tab.
pub const TAB_OPTION: &str = "tab";

/// What kind of value an option holds.
#[allow(dead_code)] // every type the web's `options` has
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Toggle,
    /// `(value, label)` pairs.
    Select(Vec<(String, String)>),
    Text {
        placeholder: String,
    },
    Number {
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
    },
}

/// One declared option.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionSpec {
    pub key: String,
    pub label: String,
    pub hint: String,
    pub kind: Kind,
    pub default: Value,
}

#[allow(dead_code)] // a constructor for every type of option
impl OptionSpec {
    pub fn toggle(key: &str, label: &str, default: bool) -> OptionSpec {
        OptionSpec::raw(key, label, Kind::Toggle, json!(default))
    }

    pub fn select(key: &str, label: &str, choices: &[(&str, &str)], default: &str) -> OptionSpec {
        let choices = choices.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect();
        OptionSpec::raw(key, label, Kind::Select(choices), json!(default))
    }

    pub fn text(key: &str, label: &str, default: &str) -> OptionSpec {
        OptionSpec::raw(
            key,
            label,
            Kind::Text {
                placeholder: String::new(),
            },
            json!(default),
        )
    }

    pub fn number(
        key: &str,
        label: &str,
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
        default: f64,
    ) -> OptionSpec {
        OptionSpec::raw(key, label, Kind::Number { min, max, step }, json!(default))
    }

    pub fn hint(mut self, hint: &str) -> OptionSpec {
        self.hint = hint.to_string();
        self
    }

    fn raw(key: &str, label: &str, kind: Kind, default: Value) -> OptionSpec {
        OptionSpec {
            key: key.to_string(),
            label: label.to_string(),
            hint: String::new(),
            kind,
            default,
        }
    }
}

/// A usable key: a letter first, then letters, digits or underscores, 32 at
/// most (`OPTION_KEY_RE`).
fn key_ok(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && key.len() <= 32
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The value an option has when nothing at all was given.
pub fn empty(spec: &OptionSpec) -> Value {
    match &spec.kind {
        Kind::Toggle => json!(false),
        Kind::Select(c) => json!(c.first().map(|c| c.0.as_str()).unwrap_or("")),
        Kind::Number { min, .. } => json!(min.unwrap_or(0.0)),
        Kind::Text { .. } => json!(""),
    }
}

/// `value` as this option's type, or `None` when it cannot be one: a select
/// value that is not a choice, a number that is not a number. A number is
/// clamped to min and max, a text cut at 1000 characters.
pub fn coerce(spec: &OptionSpec, value: &Value) -> Option<Value> {
    if value.is_null() {
        return None;
    }
    match &spec.kind {
        Kind::Toggle => value.as_bool().map(Value::Bool),
        Kind::Select(choices) => {
            let v = js_string(value);
            choices.iter().any(|c| c.0 == v).then(|| json!(v))
        }
        Kind::Number { min, max, .. } => {
            let mut n = match value {
                Value::String(s) if s.trim().is_empty() => return None,
                Value::String(s) => s.trim().parse::<f64>().ok()?,
                Value::Number(n) => n.as_f64()?,
                Value::Bool(b) => f64::from(u8::from(*b)),
                _ => return None,
            };
            if !n.is_finite() {
                return None;
            }
            if let Some(lo) = min {
                n = n.max(*lo);
            }
            if let Some(hi) = max {
                n = n.min(*hi);
            }
            Some(json!(n))
        }
        Kind::Text { .. } => Some(json!(js_string(value).chars().take(1000).collect::<String>())),
    }
}

/// `String(value)` for the JSON values an option can hold.
fn js_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        other => other.to_string(),
    }
}

/// The declared options that can be shown and stored: a usable, unique key
/// that is not `tab`, a label, a select with at least one choice. The default
/// is coerced to the type.
pub fn clean(specs: Vec<OptionSpec>) -> Vec<OptionSpec> {
    let mut seen = vec![TAB_OPTION.to_string()];
    let mut out = Vec::new();
    for mut o in specs {
        if !key_ok(&o.key) || seen.contains(&o.key) {
            continue;
        }
        if matches!(&o.kind, Kind::Select(c) if c.is_empty()) {
            continue;
        }
        if o.label.trim().is_empty() {
            o.label = o.key.clone();
        }
        o.default = coerce(&o, &o.default).unwrap_or_else(|| empty(&o));
        seen.push(o.key.clone());
        out.push(o);
    }
    out
}

/// The operator's saved option values, as stored.
pub fn saved(settings: &Value) -> Map<String, Value> {
    settings
        .get(OPTIONS_KEY)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// The current value of every option: its saved value or its default. A
/// plugin that can have a tab also gets `tab`, the operator's own choice or
/// `tabs_allowed`, the old global switch.
pub fn values(specs: &[OptionSpec], settings: &Value, can_tab: bool, tabs_allowed: bool) -> Map<String, Value> {
    let saved = saved(settings);
    let mut out = Map::new();
    for o in specs {
        let v = saved.get(&o.key).and_then(|v| coerce(o, v));
        out.insert(o.key.clone(), v.unwrap_or_else(|| o.default.clone()));
    }
    if can_tab {
        let own = saved.get(TAB_OPTION).and_then(Value::as_bool);
        out.insert(TAB_OPTION.into(), json!(own.unwrap_or(tabs_allowed)));
    }
    out
}

/// The "Own tab" switch the dialog shows first for a plugin that can have a tab.
pub fn tab_spec() -> OptionSpec {
    OptionSpec::toggle(TAB_OPTION, "Own tab", false)
        .hint("A tab of its own in the side panel, before PLUGINS, instead of a card under PLUGINS.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(min: Option<f64>, max: Option<f64>) -> OptionSpec {
        OptionSpec::number("n", "N", min, max, None, 0.0)
    }

    #[test]
    fn a_number_is_clamped_and_text_that_is_no_number_is_refused() {
        let s = n(Some(1.0), Some(10.0));
        assert_eq!(coerce(&s, &json!(50)), Some(json!(10.0)));
        assert_eq!(coerce(&s, &json!("0")), Some(json!(1.0)));
        assert_eq!(coerce(&s, &json!(" 4.5 ")), Some(json!(4.5)));
        assert_eq!(coerce(&s, &json!("")), None);
        assert_eq!(coerce(&s, &json!("abc")), None);
        assert_eq!(coerce(&s, &Value::Null), None);
    }

    #[test]
    fn a_select_takes_only_its_choices_as_strings() {
        let s = OptionSpec::select("u", "Units", &[("m", "Metres"), ("ft", "Feet")], "m");
        assert_eq!(coerce(&s, &json!("ft")), Some(json!("ft")));
        assert_eq!(coerce(&s, &json!("km")), None);
    }

    #[test]
    fn a_toggle_takes_booleans_only() {
        let s = OptionSpec::toggle("t", "T", false);
        assert_eq!(coerce(&s, &json!(true)), Some(json!(true)));
        assert_eq!(coerce(&s, &json!("true")), None);
        assert_eq!(coerce(&s, &json!(1)), None);
    }

    #[test]
    fn text_is_cut_at_a_thousand_characters() {
        let s = OptionSpec::text("t", "T", "");
        let long = "x".repeat(1500);
        assert_eq!(coerce(&s, &json!(long)).unwrap().as_str().unwrap().len(), 1000);
    }

    #[test]
    fn clean_drops_bad_keys_tab_duplicates_and_empty_selects() {
        let out = clean(vec![
            OptionSpec::toggle("1bad", "x", false),
            OptionSpec::toggle("tab", "x", false),
            OptionSpec::toggle("ok", "", true),
            OptionSpec::toggle("ok", "again", false),
            OptionSpec::select("empty", "E", &[], ""),
            OptionSpec::number("count", "Count", Some(1.0), Some(5.0), None, 9.0),
        ]);
        let keys: Vec<_> = out.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, ["ok", "count"]);
        assert_eq!(out[0].label, "ok");
        assert_eq!(out[1].default, json!(5.0));
    }

    #[test]
    fn values_fall_back_to_the_default_and_the_tab_follows_the_global_switch() {
        let specs = clean(vec![OptionSpec::toggle("labels", "Labels", true)]);
        let none = json!({});
        assert_eq!(values(&specs, &none, true, false)["labels"], json!(true));
        assert_eq!(values(&specs, &none, true, false)["tab"], json!(false));
        assert_eq!(values(&specs, &none, true, true)["tab"], json!(true));
        let set = json!({"_options": {"labels": false, "tab": false}});
        let v = values(&specs, &set, true, true);
        assert_eq!(v["labels"], json!(false));
        assert_eq!(v["tab"], json!(false));
        assert!(!values(&specs, &none, false, true).contains_key("tab"));
    }

    #[test]
    fn a_saved_value_of_the_wrong_type_is_ignored() {
        let specs = clean(vec![OptionSpec::select("u", "U", &[("m", "m"), ("ft", "ft")], "m")]);
        let set = json!({"_options": {"u": "km"}});
        assert_eq!(values(&specs, &set, false, false)["u"], json!("m"));
    }
}
