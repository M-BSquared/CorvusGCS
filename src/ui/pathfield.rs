//! A field for the path of a file or folder: the box looks like a
//! `.field-input` but cannot be typed in. It shows the path in mono, cut in
//! the middle when it is long, with a button (or two) at its right that opens
//! the system file dialog. Nobody types a path on this computer any more.
//!
//! A setting that names a place on the machine running the backend is the one
//! exception: when the interface was started with `--server` for another
//! computer, a path chosen here would mean nothing there, so such a field
//! falls back to a text box (see [`set_backend_remote`]).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Pos2, Rect, Vec2};

use crate::ui::button::{Button, Variant};
use crate::ui::card::{self, Look};
use crate::ui::filepicker::{self, Filter};
use crate::ui::formfield::{self, CONTROL_H};
use crate::ui::style;

static BACKEND_REMOTE: AtomicBool = AtomicBool::new(false);

/// Tells the fields whether the backend runs on another computer.
pub fn set_backend_remote(remote: bool) {
    BACKEND_REMOTE.store(remote, Ordering::Relaxed);
}

/// True when a path of the backend's machine cannot be chosen here.
pub fn backend_remote() -> bool {
    BACKEND_REMOTE.load(Ordering::Relaxed)
}

/// The hint that replaces a backend field's own when it is a text box.
pub const REMOTE_HINT: &str = "The backend runs on another computer, so type the path as that computer sees it.";

/// What a field asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode<'a> {
    /// One existing file, optionally of a kind.
    File(Option<Filter<'a>>),
    /// One existing folder.
    Folder,
    /// A file or a folder, as the person prefers.
    Either(Option<Filter<'a>>),
    /// A file to write: its suggested name, optionally of a kind.
    Save { name: &'a str, filter: Option<Filter<'a>> },
}

/// One button of a field and what it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    PickFile,
    PickFolder,
    SaveFile,
}

impl Mode<'_> {
    /// The buttons, left to right, with their labels.
    pub fn actions(&self) -> Vec<(Action, &'static str)> {
        match self {
            Mode::File(_) => vec![(Action::PickFile, "Choose\u{2026}")],
            Mode::Folder => vec![(Action::PickFolder, "Choose\u{2026}")],
            Mode::Either(_) => vec![
                (Action::PickFile, "Choose file\u{2026}"),
                (Action::PickFolder, "Choose folder\u{2026}"),
            ],
            Mode::Save { .. } => vec![(Action::SaveFile, "Choose\u{2026}")],
        }
    }

    /// The extension filter the file dialog gets; a folder has none.
    pub fn filter(&self) -> Option<Filter<'_>> {
        match self {
            Mode::File(f) | Mode::Either(f) => *f,
            Mode::Save { filter, .. } => *filter,
            Mode::Folder => None,
        }
    }
}

/// What a press on a field ended in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    Chosen(PathBuf),
    Cleared,
}

/// A path field: what it asks for and how it is dressed.
pub struct PathField<'a> {
    pub mode: Mode<'a>,
    /// The title of the system dialog.
    pub title: &'a str,
    /// Shown, quiet, while nothing is chosen.
    pub placeholder: &'a str,
    /// A "Clear" button empties it.
    pub optional: bool,
    /// The path is read on the backend's machine, not here.
    pub backend: bool,
}

impl<'a> PathField<'a> {
    pub fn new(mode: Mode<'a>, title: &'a str) -> Self {
        PathField {
            mode,
            title,
            placeholder: "",
            optional: false,
            backend: false,
        }
    }

    pub fn placeholder(mut self, text: &'a str) -> Self {
        self.placeholder = text;
        self
    }

    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    pub fn backend(mut self) -> Self {
        self.backend = true;
        self
    }

    /// True when this field is a text box right now.
    pub fn typed(&self) -> bool {
        self.backend && backend_remote()
    }

    /// The field `w` wide at `x, y`, [`CONTROL_H`] tall. `value` is the path
    /// shown; it is replaced by what is chosen, which is also returned.
    pub fn show(
        &self,
        ui: &mut egui::Ui,
        x: f32,
        y: f32,
        w: f32,
        value: &mut String,
        id: impl std::hash::Hash + std::fmt::Debug,
    ) -> Option<Picked> {
        if self.typed() {
            formfield::input(ui, x, y, w, value, self.placeholder, true, false, id);
            return None;
        }
        let id = ui.scope_id().with(id);
        let mut buttons: Vec<(Option<Action>, &str)> =
            self.mode.actions().into_iter().map(|(a, l)| (Some(a), l)).collect();
        if self.optional {
            buttons.push((None, "Clear"));
        }
        let widths: Vec<f32> = buttons
            .iter()
            .map(|(_, l)| Button::new(l, Variant::Secondary).small().width(ui))
            .collect();
        let total: f32 = widths.iter().sum::<f32>() + 6.0 * (buttons.len() as f32 - 1.0);
        let box_w = (w - total - 8.0).max(60.0);
        let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::new(box_w, CONTROL_H));
        card::paint(
            ui.painter(),
            rect,
            Look::custom(style::input(0.6), style::border(), style::RADIUS_BTN),
        );
        let inner = rect.shrink2(Vec2::new(11.0, 0.0));
        let hovered = ui.interact(rect, id.with("box"), egui::Sense::hover());
        let (text, color) = if value.trim().is_empty() {
            (crate::i18n::tr(self.placeholder).into_owned(), style::text_3())
        } else {
            (value.clone(), style::text_1())
        };
        let shown = fit_middle(&text, inner.width(), |s| {
            style::galley_mono(ui, s, 12.0, color).size().x
        });
        let g = style::galley_mono(ui, &shown, 12.0, color);
        ui.painter().with_clip_rect(rect).galley(
            Pos2::new(inner.left(), inner.center().y - g.size().y / 2.0),
            g,
            color,
        );
        if !value.trim().is_empty() {
            hovered.on_hover_text(value.as_str());
        }
        let mut out = None;
        let mut bx = x + box_w + 8.0;
        for (i, (action, label)) in buttons.iter().enumerate() {
            let r = Rect::from_min_size(Pos2::new(bx, y), Vec2::new(widths[i], CONTROL_H));
            bx += widths[i] + 6.0;
            if Button::new(label, Variant::Secondary)
                .small()
                .show(ui, r, id.with(("button", i)))
            {
                match action {
                    Some(a) => {
                        let start = filepicker::start_folder(value);
                        if let Some(p) = run(*a, &self.mode, self.title, start.as_deref()) {
                            *value = p.to_string_lossy().into_owned();
                            out = Some(Picked::Chosen(p));
                        }
                    }
                    None => {
                        value.clear();
                        out = Some(Picked::Cleared);
                    }
                }
            }
        }
        out
    }
}

fn run(action: Action, mode: &Mode<'_>, title: &str, start: Option<&std::path::Path>) -> Option<PathBuf> {
    match action {
        Action::PickFile => filepicker::pick_file(title, mode.filter(), start),
        Action::PickFolder => filepicker::pick_folder(title, start),
        Action::SaveFile => {
            let name = match mode {
                Mode::Save { name, .. } => *name,
                _ => "",
            };
            filepicker::save_file(title, name, mode.filter(), start)
        }
    }
}

/// `text` cut in the middle with an ellipsis until `width_of` says it fits in
/// `max`. The end of a path (the file name) is kept longer than its start.
pub fn fit_middle(text: &str, max: f32, width_of: impl Fn(&str) -> f32) -> String {
    if width_of(text) <= max {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let build = |keep: usize| -> String {
        let head = keep / 3;
        let tail = keep - head;
        let mut s: String = chars[..head].iter().collect();
        s.push('\u{2026}');
        s.extend(&chars[chars.len() - tail..]);
        s
    };
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if width_of(&build(mid)) <= max {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    build(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: Filter<'static> = Filter {
        name: "PNG",
        extensions: &["png"],
    };

    fn chars(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn a_short_path_is_shown_whole() {
        assert_eq!(fit_middle("/home/a.png", 20.0, chars), "/home/a.png");
        assert_eq!(fit_middle("", 5.0, chars), "");
    }

    #[test]
    fn a_long_path_is_cut_in_the_middle_and_keeps_its_end() {
        let p = "/home/pilot/Documents/flights/2026/logs/flight_0042.ulg";
        let s = fit_middle(p, 24.0, chars);
        assert!(chars(&s) <= 24.0, "{s}");
        assert!(s.contains('\u{2026}'));
        assert!(s.starts_with("/home"), "{s}");
        assert!(s.ends_with("flight_0042.ulg"), "{s}");
        assert_eq!(chars(&s), 24.0, "it uses the room it has: {s}");
    }

    #[test]
    fn nothing_fits_into_a_tiny_box() {
        assert_eq!(fit_middle("abcdefghij", 1.0, chars), "\u{2026}");
        assert_eq!(fit_middle("abcdefghij", 0.0, chars), "\u{2026}");
    }

    #[test]
    fn multibyte_paths_are_cut_on_characters() {
        let s = fit_middle("/Users/Müller/Überflug/Größe/ä.ulg", 12.0, chars);
        assert!(chars(&s) <= 12.0);
        assert!(s.ends_with(".ulg"));
    }

    #[test]
    fn each_mode_offers_its_buttons() {
        assert_eq!(
            Mode::File(Some(PNG)).actions(),
            vec![(Action::PickFile, "Choose\u{2026}")]
        );
        assert_eq!(Mode::Folder.actions(), vec![(Action::PickFolder, "Choose\u{2026}")]);
        assert_eq!(
            Mode::Either(None).actions(),
            vec![
                (Action::PickFile, "Choose file\u{2026}"),
                (Action::PickFolder, "Choose folder\u{2026}")
            ]
        );
        let save = Mode::Save {
            name: "a.params",
            filter: None,
        };
        assert_eq!(save.actions(), vec![(Action::SaveFile, "Choose\u{2026}")]);
    }

    #[test]
    fn the_mode_decides_the_filter() {
        assert_eq!(Mode::File(Some(PNG)).filter(), Some(PNG));
        assert_eq!(Mode::Either(Some(PNG)).filter(), Some(PNG));
        assert_eq!(
            Mode::Save {
                name: "x",
                filter: Some(PNG)
            }
            .filter(),
            Some(PNG)
        );
        assert_eq!(Mode::Folder.filter(), None);
        assert_eq!(Mode::File(None).filter(), None);
    }

    #[test]
    fn a_remote_backend_turns_only_its_own_fields_into_text() {
        set_backend_remote(true);
        let here = PathField::new(Mode::Folder, "x");
        let there = PathField::new(Mode::Folder, "x").backend();
        let (a, b) = (here.typed(), there.typed());
        set_backend_remote(false);
        assert!(!a);
        assert!(b);
        assert!(!there.typed());
    }
}
