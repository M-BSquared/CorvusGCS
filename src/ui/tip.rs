//! Hover texts through the translation layer. A `title` of the web interface
//! is translated with the rest of the page; `Response::on_hover_text` is not,
//! so a tip in English stayed English in every other language. Use
//! `on_hover_tr` wherever a hover text is shown.

use std::borrow::Cow;

use eframe::egui;

use crate::i18n::tr;

/// The text of a hover card in the language in force. Several lines are
/// translated one by one, as the layer does for any text.
pub fn text(english: &str) -> Cow<'_, str> {
    tr(english)
}

pub trait HoverTr {
    /// `on_hover_text` with the text translated.
    fn on_hover_tr(self, english: impl AsRef<str>) -> Self;
}

impl HoverTr for egui::Response {
    fn on_hover_tr(self, english: impl AsRef<str>) -> Self {
        self.on_hover_text(text(english.as_ref()).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::with_language;

    #[test]
    fn a_hover_text_is_translated_and_an_unknown_one_stays() {
        with_language("de", || {
            assert_eq!(text("Not connected"), "Nicht verbunden");
            assert_eq!(text("a text no file has"), "a text no file has");
        });
    }
}
