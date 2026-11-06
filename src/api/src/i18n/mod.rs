//! The interface language.
//!
//! The English text of the interface is the key, exactly as in the web
//! interface (`python/src/js/i18n.js`): a language file maps that text to its
//! translation, and a text a language does not list stays English. Nothing in
//! the code carries a message id, a missing key never panics, and the same
//! files serve both interfaces.
//!
//! Where the translation happens. The web interface swaps text as it reaches
//! the page; here the same swap sits where text is laid out (`ui::style::galley`,
//! `ui::text::wrapped`, the hint of a text box, the plot titles), so a page
//! writes plain English and every widget on it follows the language. For text
//! that is built or compared away from a painter, use [`tr`] and [`tr_fmt`].
//!
//! `{name}` in a key is a pattern: it matches any text in that place, and the
//! same `{name}` in the translation puts the matched text back, translated too
//! when the language has an entry for it. A label ending in an autopilot
//! parameter, `Action on RC loss (NAV_RCL_ACT)`, is translated by the label
//! alone.
//!
//! Text that is data and not wording (a parameter value, a vehicle's name, the
//! log, the terminal) is laid out with the mono helpers, which do not translate.

mod files;
mod table;

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::Value;

pub use files::{Language, DEFAULT_CODE};
pub use table::{fill, normalize};

const CACHE_MAX: usize = 4000;
const MISSED_MAX: usize = 6000;

/// One language in force: its table and the answers already worked out.
struct Active {
    table: table::Table,
    values: HashSet<String>,
    cache: Mutex<HashMap<String, Option<String>>>,
}

#[derive(Default)]
struct State {
    languages: Option<Vec<Language>>,
    code: Option<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    languages: None,
    code: None,
});
static ACTIVE: RwLock<Option<Arc<Active>>> = RwLock::new(None);
static ON: AtomicBool = AtomicBool::new(false);
static MISSED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

#[cfg(test)]
thread_local! {
    static LOCAL: std::cell::RefCell<Option<Arc<Active>>> = const { std::cell::RefCell::new(None) };
}

fn has_letter(text: &str) -> bool {
    text.chars().any(char::is_alphabetic)
}

fn current() -> Option<Arc<Active>> {
    #[cfg(test)]
    if let Some(a) = LOCAL.with(|l| l.borrow().clone()) {
        return Some(a);
    }
    if !ON.load(Ordering::Relaxed) {
        return None;
    }
    ACTIVE.read().ok().and_then(|g| g.clone())
}

fn lines(raw: &str, active: &Active) -> Option<String> {
    let mut any = false;
    let out: Vec<String> = raw
        .split('\n')
        .map(|line| match active.one(line) {
            Some(t) => {
                any = true;
                t
            }
            None => line.to_string(),
        })
        .collect();
    any.then(|| out.join("\n"))
}

impl Active {
    fn build(lang: &Language) -> Active {
        let mut strings = files::plugin_strings(&lang.code);
        strings.extend(lang.strings.iter().map(|(k, v)| (k.clone(), v.clone())));
        let table = table::Table::new(strings.iter());
        let values = strings.values().map(|v| normalize(v)).collect();
        Active {
            table,
            values,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The translation of one piece of text with its outer whitespace kept.
    fn one(&self, raw: &str) -> Option<String> {
        let key = normalize(raw);
        if key.is_empty() || !has_letter(&key) {
            return None;
        }
        let out = self.table.lookup(&key)?;
        let lead = &raw[..raw.len() - raw.trim_start().len()];
        let trail = &raw[raw.trim_end().len()..];
        Some(format!("{lead}{out}{trail}"))
    }

    fn translate(&self, raw: &str) -> Option<String> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache.get(raw) {
                return hit.clone();
            }
        }
        let out = self
            .one(raw)
            .or_else(|| if raw.contains('\n') { lines(raw, self) } else { None });
        if out.is_none() && !self.values.contains(&normalize(raw)) {
            if let Ok(mut m) = MISSED.lock() {
                if m.len() < MISSED_MAX {
                    m.insert(normalize(raw));
                }
            }
        }
        if let Ok(mut cache) = self.cache.lock() {
            if cache.len() >= CACHE_MAX {
                cache.clear();
            }
            cache.insert(raw.to_string(), out.clone());
        }
        out
    }
}

/// The text for `english` in the language in force. English, a text without a
/// letter and a text the language does not list come back as given.
pub fn tr(english: &str) -> Cow<'_, str> {
    let Some(active) = current() else {
        return Cow::Borrowed(english);
    };
    if !has_letter(english) {
        return Cow::Borrowed(english);
    }
    match active.translate(english) {
        Some(t) => Cow::Owned(t),
        None => Cow::Borrowed(english),
    }
}

/// [`tr`] with each `{name}` of the result filled from `vars`. The key is the
/// text with its holes, as the language files carry it, for instance
/// `Settings of {name}` with `("name", plugin)`.
pub fn tr_fmt(english: &str, vars: &[(&str, &str)]) -> String {
    fill(&tr(english), vars)
}

/// [`tr`] for a line of parts joined with ` \u{b7} `, each part on its own, as
/// the web translates the pieces of a row it builds.
pub fn tr_joined(text: &str) -> String {
    text.split(" \u{b7} ")
        .map(|p| tr(p).into_owned())
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// What the app's languages are, discovered on first use.
fn with_languages<R>(f: impl FnOnce(&mut Vec<Language>, &mut Option<String>) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner());
    let State { languages, code } = &mut *g;
    let list = languages.get_or_insert_with(|| files::discover(files::user_dir().as_deref()));
    f(list, code)
}

/// One installed language, as the dropdown lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub code: String,
    pub label: String,
}

/// The installed languages: English first, then by name.
pub fn languages() -> Vec<Info> {
    with_languages(|list, _| {
        list.iter()
            .map(|l| Info {
                code: l.code.clone(),
                label: l.label(),
            })
            .collect()
    })
}

/// The code in force.
pub fn language() -> String {
    with_languages(|_, code| code.clone().unwrap_or_else(|| DEFAULT_CODE.into()))
}

fn apply(list: &[Language], code: &mut Option<String>, target: &str) {
    let lang = list.iter().find(|l| l.code == target);
    let active = lang
        .filter(|l| l.code != DEFAULT_CODE)
        .map(|l| Arc::new(Active::build(l)));
    if let Ok(mut g) = ACTIVE.write() {
        ON.store(active.is_some(), Ordering::Relaxed);
        *g = active;
    }
    if let Ok(mut m) = MISSED.lock() {
        m.clear();
    }
    *code = Some(target.to_string());
}

/// Put `wanted` (a code, or what [`files::match_code`] maps to one) in force.
/// An unknown language leaves the current one alone; English always works.
/// Returns the code now in force.
pub fn set_language(wanted: &str) -> String {
    with_languages(|list, code| {
        let codes: Vec<String> = list.iter().map(|l| l.code.clone()).collect();
        let Some(target) = files::match_code(wanted, &codes) else {
            return code.clone().unwrap_or_else(|| DEFAULT_CODE.into());
        };
        if code.as_deref() != Some(&target) {
            apply(list, code, &target);
        }
        target
    })
}

/// Registers the language files a plugin carries (`lang/` of its crate) and
/// puts them to use in the language already in force.
pub fn register_plugin_files(files: &'static [(&'static str, &'static str)]) {
    if files::add_plugin_files(files) {
        with_languages(|list, code| {
            if let Some(c) = code.clone() {
                apply(list, code, &c);
            }
        });
    }
}

/// Read the operator's folder again, and the language in force with it. A
/// language whose file is gone falls back to English.
pub fn reload() -> usize {
    with_languages(|list, code| {
        *list = files::discover(files::user_dir().as_deref());
        let keep = code.clone().unwrap_or_else(|| DEFAULT_CODE.into());
        let target = if list.iter().any(|l| l.code == keep) {
            keep
        } else {
            DEFAULT_CODE.into()
        };
        apply(list, code, &target);
        list.len()
    })
}

/// The language a `GET /api/config` body asks for ("en" when it names none).
pub fn from_config(cfg: &Value) -> String {
    cfg.get("language")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_CODE)
        .to_string()
}

/// The English texts a translated language was asked for and does not have,
/// sorted. For writing a language file: run the pages in it, then read this.
pub fn missing() -> Vec<String> {
    MISSED.lock().map(|m| m.iter().cloned().collect()).unwrap_or_default()
}

/// Run `f` with `code` in force for this thread only, so a test can look at a
/// language without changing it for the tests running beside it.
#[cfg(test)]
pub fn with_language<R>(code: &str, f: impl FnOnce() -> R) -> R {
    let list = files::discover(None);
    let active = list
        .iter()
        .find(|l| l.code == code)
        .filter(|l| l.code != DEFAULT_CODE)
        .map(|l| Arc::new(Active::build(l)));
    LOCAL.with(|l| *l.borrow_mut() = active);
    let r = f();
    LOCAL.with(|l| *l.borrow_mut() = None);
    r
}

#[cfg(test)]
mod tests;
