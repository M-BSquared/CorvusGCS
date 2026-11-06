//! A text with a `'static` lifetime for an API that wants one (a map credit
//! for walkers, an id for a text box) without leaking a copy every time it is
//! asked for: the same text comes back as the same slice, and the set that is
//! kept has a ceiling.

use std::collections::HashSet;
use std::sync::Mutex;

/// Distinct texts kept. A credit or a provider id comes from a short list;
/// past this the text is not kept and the empty string comes back.
pub const MAX: usize = 256;

static KEPT: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

pub fn intern(text: &str) -> &'static str {
    let mut guard = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    intern_in(guard.get_or_insert_with(HashSet::new), text)
}

fn intern_in(set: &mut HashSet<&'static str>, text: &str) -> &'static str {
    if let Some(have) = set.get(text) {
        return have;
    }
    if set.len() >= MAX {
        return "";
    }
    let leaked: &'static str = Box::leak(text.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_text_is_the_same_slice() {
        let mut set = HashSet::new();
        let a = intern_in(&mut set, "credit: test one");
        let b = intern_in(&mut set, &String::from("credit: test one"));
        assert!(std::ptr::eq(a, b));
        assert_eq!(a, "credit: test one");
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn the_set_has_a_ceiling() {
        let mut set = HashSet::new();
        for i in 0..(MAX * 2) {
            intern_in(&mut set, &format!("ceiling test {i}"));
        }
        assert_eq!(set.len(), MAX);
        assert_eq!(intern_in(&mut set, "a text that arrives too late"), "");
        assert_eq!(intern_in(&mut set, "ceiling test 0"), "ceiling test 0");
    }
}
