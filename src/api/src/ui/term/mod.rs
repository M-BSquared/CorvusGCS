//! A terminal a page or a plugin can show: the screen that takes a shell's
//! bytes ([`emu`]), the widget that draws it and reads the keyboard ([`view`])
//! and the bytes a key sends ([`keys`]). What the bytes come from and go to is
//! the caller's: it hands the view a [`Shell`].

pub mod emu;
pub mod keys;
pub mod view;

use std::sync::{Arc, Mutex};

/// What the terminal view talks to: an SSH session, a shell over MAVLink, a
/// plugin's own.
pub trait Shell {
    /// The screen the shell's output is fed into.
    fn emulator(&self) -> Arc<Mutex<emu::Emulator>>;
    /// What the operator typed, as the bytes a terminal sends.
    fn send(&mut self, data: String);
    /// The grid the view shows now.
    fn resize(&mut self, cols: usize, rows: usize);
}
