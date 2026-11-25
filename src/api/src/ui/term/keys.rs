//! The bytes a key sends to a shell, as a terminal does it.

use egui;

/// The bytes a key sends: `None` for one that sends nothing.
pub fn key_bytes(key: egui::Key, mods: egui::Modifiers, app_cursor: bool) -> Option<String> {
    use egui::Key::*;
    let cursor = |c: char| {
        if app_cursor {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        }
    };
    let s = match key {
        Enter => "\r".to_string(),
        Tab if mods.shift => "\x1b[Z".to_string(),
        Tab => "\t".to_string(),
        Backspace => "\x7f".to_string(),
        Escape => "\x1b".to_string(),
        ArrowUp => cursor('A'),
        ArrowDown => cursor('B'),
        ArrowRight => cursor('C'),
        ArrowLeft => cursor('D'),
        Home => cursor('H'),
        End => cursor('F'),
        Insert => "\x1b[2~".to_string(),
        Delete => "\x1b[3~".to_string(),
        PageUp => "\x1b[5~".to_string(),
        PageDown => "\x1b[6~".to_string(),
        F1 => "\x1bOP".to_string(),
        F2 => "\x1bOQ".to_string(),
        F3 => "\x1bOR".to_string(),
        F4 => "\x1bOS".to_string(),
        F5 => "\x1b[15~".to_string(),
        F6 => "\x1b[17~".to_string(),
        F7 => "\x1b[18~".to_string(),
        F8 => "\x1b[19~".to_string(),
        F9 => "\x1b[20~".to_string(),
        F10 => "\x1b[21~".to_string(),
        F11 => "\x1b[23~".to_string(),
        F12 => "\x1b[24~".to_string(),
        k if mods.ctrl && !mods.alt => {
            let name = k.name();
            let c = name.chars().next().filter(|_| name.len() == 1)?;
            match c.to_ascii_lowercase() {
                l @ 'a'..='z' => ((l as u8 - b'a' + 1) as char).to_string(),
                '[' => "\x1b".to_string(),
                '\\' => "\x1c".to_string(),
                ']' => "\x1d".to_string(),
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(if mods.alt && !s.starts_with('\x1b') {
        format!("\x1b{s}")
    } else {
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};

    #[test]
    fn keys_become_what_a_terminal_sends() {
        let none = Modifiers::NONE;
        assert_eq!(key_bytes(Key::Enter, none, false).as_deref(), Some("\r"));
        assert_eq!(key_bytes(Key::ArrowUp, none, false).as_deref(), Some("\x1b[A"));
        assert_eq!(key_bytes(Key::ArrowUp, none, true).as_deref(), Some("\x1bOA"));
        assert_eq!(key_bytes(Key::Delete, none, false).as_deref(), Some("\x1b[3~"));
        assert_eq!(key_bytes(Key::C, Modifiers::CTRL, false).as_deref(), Some("\x03"));
        assert_eq!(key_bytes(Key::D, Modifiers::CTRL, false).as_deref(), Some("\x04"));
        assert_eq!(key_bytes(Key::A, none, false), None);
    }
}
