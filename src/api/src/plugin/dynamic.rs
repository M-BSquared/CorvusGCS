//! What a plugin built as a shared library and the program agree on.
//!
//! The library is built from this crate and the plugin's own, by the same
//! compiler and the same source as the program, so Rust types cross the
//! boundary as they do inside one binary. What does not cross is a `static`: a
//! library carries its own copy of every crate it uses. Everything that follows
//! from that is handled here or in `src/vendor`:
//!
//! * The theme, tablet mode and the language in force are handed over before
//!   every call ([`Link`], [`apply`]).
//! * The queue of threads waiting for a lock: the library uses the program's
//!   (`src/vendor/parking_lot_core`), or a lock both use would never wake the
//!   other side.
//! * The number of a font face: made from its name (`src/vendor/epaint`).
//! * The unwinding of a panic: caught in the library ([`Guarded`]) and raised
//!   again in the program.
//!
//! A plugin crate exports itself with [`export_plugin!`]; the program compares
//! [`ABI`] with the one in the library's file before it loads it, and [`stamp`]
//! before it uses anything of it.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use serde_json::Value;

use crate::plugin::options::OptionSpec;
use crate::plugin::{Host, Info, MapEvent, McpTool, Plugin};
use crate::ui::style::Palette;
use crate::ui::{pagekit, pathfield, style};

/// The compiler and the source this crate and what it shares with a library were built from
/// (`build.rs`). It does not name the program's version: a library stays good until this changes.
pub const ABI: &str = env!("CORVUS_ABI");

const MARKED: &str = concat!("\0CORVUS-PLUGIN\0", env!("CORVUS_ABI"), "\0");

/// [`ABI`] between marks that `corvus_package::abi_of` finds in the file of a library.
#[doc(hidden)]
pub const MARKED_ABI: [u8; MARKED.len()] = {
    let mut out = [0; MARKED.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = MARKED.as_bytes()[i];
        i += 1;
    }
    out
};

/// What `corvus_plugin_abi` of a library must say: [`ABI`], and the identity and size of the
/// types that cross the boundary. The identity of a type follows from the version, the features
/// and the profile its crate was compiled with, so a library built with other ones is refused.
pub fn stamp() -> String {
    use std::any::TypeId;
    use std::mem::size_of;
    format!(
        "{ABI}|{:?}{:?}{:?}{:?}|{}.{}.{}.{}.{}.{}",
        TypeId::of::<egui::Context>(),
        TypeId::of::<egui::Ui>(),
        TypeId::of::<Value>(),
        TypeId::of::<Host<'static>>(),
        size_of::<egui::Style>(),
        size_of::<egui::Ui>(),
        size_of::<Value>(),
        size_of::<Palette>(),
        size_of::<Host>(),
        size_of::<Info>(),
    )
}

#[doc(hidden)]
pub fn stamp_c() -> *const std::ffi::c_char {
    static STAMP: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();
    STAMP
        .get_or_init(|| std::ffi::CString::new(stamp().replace('\0', "")).unwrap_or_default())
        .as_ptr()
}

/// The state of the program's statics a plugin's copy of this crate follows.
pub struct Link {
    palette: Palette,
    touch: bool,
    link_up: bool,
    backend_remote: bool,
    /// The program's own `tr`: `None` when the text has no translation.
    translate: fn(&str) -> Option<String>,
    /// Where the program queues the threads that wait for a lock.
    waiting: *const (),
}

impl Link {
    /// What the program's statics say right now.
    pub fn current() -> Link {
        Link {
            palette: style::palette(),
            touch: style::touch(),
            link_up: pagekit::link_up(),
            backend_remote: pathfield::backend_remote(),
            translate,
            waiting: parking_lot_core::table_handle(),
        }
    }
}

fn translate(english: &str) -> Option<String> {
    match crate::i18n::tr(english) {
        std::borrow::Cow::Owned(t) => Some(t),
        std::borrow::Cow::Borrowed(_) => None,
    }
}

static JOINED: AtomicBool = AtomicBool::new(false);

/// Puts `link` into this copy of the crate's statics. The loader calls it first of all, before a
/// plugin is made and any thread of the library exists.
pub fn apply(link: &Link) {
    if !JOINED.swap(true, Ordering::AcqRel) {
        // SAFETY: `waiting` is the program's handle (`Link` is only made by `current`), from a copy
        // of `parking_lot_core` the stamp says is this one, and the program outlives its libraries.
        // This is the first call into the library, so no thread waits through its own table.
        unsafe { parking_lot_core::adopt_table(link.waiting) };
    }
    style::set_palette(link.palette);
    style::set_touch(link.touch);
    pagekit::set_link_up(link.link_up);
    pathfield::set_backend_remote(link.backend_remote);
    crate::i18n::set_remote(Some(link.translate));
}

/// What a plugin library exports under the name `CORVUS_PLUGIN`.
pub struct Entry {
    pub make: fn() -> Result<Box<dyn Plugin>, String>,
    /// The plugin's language files, as `corvus_api::i18n::register_plugin_files` takes them.
    pub lang: &'static [(&'static str, &'static str)],
    pub sync: fn(&Link),
    /// [`take_panic`] of the library.
    pub panicked: fn() -> Option<String>,
}

/// Makes the crate a plugin library: `export_plugin!(plugin, LANG)` beside the
/// crate's `pub fn plugin() -> Box<dyn Plugin>` and `pub const LANG`.
#[macro_export]
macro_rules! export_plugin {
    ($make:path, $lang:path) => {
        #[no_mangle]
        pub extern "C" fn corvus_plugin_abi() -> *const ::std::ffi::c_char {
            $crate::plugin::dynamic::stamp_c()
        }

        #[no_mangle]
        pub static CORVUS_PLUGIN_MARK: [u8; $crate::plugin::dynamic::MARKED_ABI.len()] =
            $crate::plugin::dynamic::MARKED_ABI;

        #[no_mangle]
        pub static CORVUS_PLUGIN_CRATE: &str = concat!("\0CORVUS-CRATE\0", env!("CARGO_PKG_NAME"), "\0");

        #[no_mangle]
        pub static CORVUS_PLUGIN: $crate::plugin::dynamic::Entry = $crate::plugin::dynamic::Entry {
            make: || $crate::plugin::dynamic::Guarded::make($make),
            lang: $lang,
            sync: $crate::plugin::dynamic::apply,
            panicked: $crate::plugin::dynamic::take_panic,
        };
    };
}

static PANICKED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

thread_local! {
    static PLACE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// The panic a call into this library ended in, once. A panic cannot leave the
/// library (each copy of `std` has its own unwinding machinery, so the program
/// could not catch it and would abort): [`Guarded`] catches it here and the
/// program asks after every call.
pub fn take_panic() -> Option<String> {
    PANICKED.lock().unwrap_or_else(|p| p.into_inner()).take()
}

fn guard<R>(f: impl FnOnce() -> R) -> Option<R> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(payload) => {
            let text = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "a panic".into());
            let place = PLACE.with(|p| std::mem::take(&mut *p.borrow_mut()));
            // The payload is the plugin's own value, and dropping it may panic again.
            std::mem::forget(payload);
            *PANICKED.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!("{text} ({place})"));
            None
        }
    }
}

/// The plugin as the library hands it out: every call is caught inside the library, and a plugin
/// that panicked is not called again.
pub struct Guarded {
    inner: Option<Box<dyn Plugin>>,
    info: Info,
    dead: bool,
}

impl Guarded {
    /// Makes the plugin, for `export_plugin!`.
    pub fn make(new: fn() -> Box<dyn Plugin>) -> Result<Box<dyn Plugin>, String> {
        static HOOK: std::sync::Once = std::sync::Once::new();
        HOOK.call_once(|| {
            // The program's hook does not see a panic in here; what it writes to its crash log is
            // what the loader raises again, so the place is kept for that.
            let _ = catch_unwind(|| {
                let default = std::panic::take_hook();
                std::panic::set_hook(Box::new(move |info| {
                    if let Some(l) = info.location() {
                        let _ = PLACE.try_with(|p| *p.borrow_mut() = format!("{}:{}", l.file(), l.line()));
                    }
                    default(info);
                }));
            });
        });
        match guard(|| {
            let inner = new();
            let info = inner.info();
            Guarded {
                inner: Some(inner),
                info,
                dead: false,
            }
        }) {
            Some(g) => Ok(Box::new(g)),
            None => Err(take_panic().unwrap_or_default()),
        }
    }

    fn run<R>(&mut self, f: impl FnOnce(&mut dyn Plugin) -> R) -> Option<R> {
        let inner = self.inner.as_mut().filter(|_| !self.dead)?;
        let r = guard(|| f(inner.as_mut()));
        self.dead = r.is_none();
        r
    }

    fn look<R>(&self, f: impl FnOnce(&dyn Plugin) -> R) -> Option<R> {
        let inner = self.inner.as_ref().filter(|_| !self.dead)?;
        guard(|| f(inner.as_ref()))
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            // Nobody asks about this one any more; the next plugin of the library is not to blame.
            if guard(|| drop(inner)).is_none() {
                take_panic();
            }
        }
    }
}

impl Plugin for Guarded {
    fn info(&self) -> Info {
        self.info
    }
    fn options(&self) -> Vec<OptionSpec> {
        self.look(|p| p.options()).unwrap_or_default()
    }
    fn start(&mut self, h: &mut Host) {
        self.run(|p| p.start(h));
    }
    fn tick(&mut self, h: &mut Host) {
        self.run(|p| p.tick(h));
    }
    fn open(&mut self, h: &mut Host) {
        self.run(|p| p.open(h));
    }
    fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect, h: &mut Host) -> f32 {
        self.run(|p| p.show(ui, area, h)).unwrap_or(0.0)
    }
    fn close(&mut self, h: &mut Host) {
        self.run(|p| p.close(h));
    }
    fn options_changed(&mut self, h: &mut Host) {
        self.run(|p| p.options_changed(h));
    }
    fn map_event(&mut self, e: MapEvent, h: &mut Host) {
        self.run(|p| p.map_event(e, h));
    }
    fn message(&mut self, from: &str, msg: &Value, h: &mut Host) {
        self.run(|p| p.message(from, msg, h));
    }
    fn ssh_setup_done(&mut self, tag: u64, saved: bool, h: &mut Host) {
        self.run(|p| p.ssh_setup_done(tag, saved, h));
    }
    fn mcp_tools(&self, h: &Host) -> Vec<McpTool> {
        self.look(|p| p.mcp_tools(h)).unwrap_or_default()
    }
    fn mcp_call(&mut self, name: &str, args: &Value, token: u64, h: &mut Host) {
        self.run(|p| p.mcp_call(name, args, token, h));
    }
    fn mcp_take(&mut self, token: u64) -> Option<Result<Value, String>> {
        self.run(|p| p.mcp_take(token)).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `static` of this crate. A plugin library has its own copy of each, so one the program
    /// changes is stale in the library unless it is handed over in [`Link`]. A new one fails this
    /// test until it is listed here with what happens to it.
    const STATICS: &[(&str, &str)] = &[
        ("ui/style.rs", "PALETTE"),
        ("ui/style.rs", "TOUCH"),
        ("ui/pagekit.rs", "LINK_UP"),
        ("ui/pathfield.rs", "BACKEND_REMOTE"),
        ("i18n/mod.rs", "REMOTE"),
        // Set and read by the widgets of one call, inside one copy.
        ("ui/style.rs", "IN_MODAL"),
        ("plugin/ssh.rs", "N"),
        // A cache of this copy, GPU objects are shared through the context.
        ("ui/frost.rs", "RESOURCES"),
        // Constants.
        ("plugin/dynamic.rs", "CORVUS_PLUGIN_CRATE"),
        ("ui/fonts.rs", "INTER_LATIN"),
        ("ui/fonts.rs", "INTER_LATIN_EXT"),
        ("ui/fonts.rs", "MONO_LATIN"),
        ("ui/fonts.rs", "MONO_LATIN_EXT"),
        // The program only: a library translates through `REMOTE` and its language files are
        // registered by the program.
        ("i18n/mod.rs", "STATE"),
        ("i18n/mod.rs", "ACTIVE"),
        ("i18n/mod.rs", "ON"),
        ("i18n/mod.rs", "MISSED"),
        ("i18n/mod.rs", "HAS_REMOTE"),
        ("i18n/mod.rs", "LOCAL"),
        ("i18n/files.rs", "EXTRA_FILES"),
        // This module: the panic of one library, and the stamp it reports.
        ("plugin/dynamic.rs", "PANICKED"),
        ("plugin/dynamic.rs", "STAMP"),
        // The export of a plugin library, written by `export_plugin!`.
        ("plugin/dynamic.rs", "CORVUS_PLUGIN"),
        ("plugin/dynamic.rs", "CORVUS_PLUGIN_MARK"),
        ("plugin/dynamic.rs", "JOINED"),
        ("plugin/dynamic.rs", "PLACE"),
        ("plugin/dynamic.rs", "HOOK"),
    ];

    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                rust_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    #[test]
    fn every_static_of_the_crate_is_accounted_for() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_files(&root, &mut files);
        let mut found: Vec<(String, String)> = Vec::new();
        for f in files {
            let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
            for line in std::fs::read_to_string(&f).unwrap().lines() {
                let t = line.trim_start();
                let t = t.strip_prefix("pub ").unwrap_or(t);
                if let Some(rest) = t.strip_prefix("static ") {
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_ascii_uppercase() || *c == '_' || c.is_ascii_digit())
                        .collect();
                    if !name.is_empty() && rest[name.len()..].starts_with(':') {
                        found.push((rel.clone(), name));
                    }
                }
            }
        }
        let known: Vec<(String, String)> = STATICS.iter().map(|(f, n)| (f.to_string(), n.to_string())).collect();
        let new: Vec<_> = found.iter().filter(|s| !known.contains(s)).collect();
        let gone: Vec<_> = known.iter().filter(|s| !found.contains(s)).collect();
        assert!(
            new.is_empty(),
            "a new static: hand it over in dynamic::Link or list it in STATICS: {new:?}"
        );
        assert!(gone.is_empty(), "listed but not found: {gone:?}");
    }

    struct Boom;

    impl Plugin for Boom {
        fn info(&self) -> Info {
            Info {
                id: "boom",
                name: "Boom",
                icon: "x",
                description: "",
                version: "0",
                order: 0,
                can_tab: false,
            }
        }
        fn show(&mut self, _ui: &mut egui::Ui, _area: egui::Rect, _h: &mut Host) -> f32 {
            panic!("inside the library");
        }
    }

    fn boom() -> Box<dyn Plugin> {
        Box::new(Boom)
    }

    /// One test, because the panic of a library is one `static`.
    #[test]
    fn a_panic_is_caught_in_the_library_reported_once_and_ends_the_plugin() {
        let mut g = Guarded {
            inner: Some(boom()),
            info: boom().info(),
            dead: false,
        };
        assert_eq!(g.run(|p| p.options().len()), Some(0));
        assert_eq!(g.run(|_| -> u8 { panic!("inside the library") }), None);
        assert!(take_panic().unwrap().starts_with("inside the library"));
        assert_eq!(take_panic(), None);
        assert_eq!(
            g.run(|p| p.options().len()),
            None,
            "a plugin that panicked is not called again"
        );
        assert_eq!(take_panic(), None);

        struct Bad;
        impl Drop for Bad {
            fn drop(&mut self) {
                panic!("in its drop");
            }
        }
        impl Plugin for Bad {
            fn info(&self) -> Info {
                Boom.info()
            }
            fn show(&mut self, _ui: &mut egui::Ui, _area: egui::Rect, _h: &mut Host) -> f32 {
                0.0
            }
        }
        drop(Guarded::make(|| Box::new(Bad)).unwrap());
        assert_eq!(take_panic(), None, "a panic in a drop is nobody else's");
        let why = Guarded::make(|| panic!("never made")).err().unwrap();
        assert!(why.starts_with("never made ("), "{why}");
    }

    #[test]
    fn the_mark_in_a_library_holds_the_abi() {
        let text = String::from_utf8(MARKED_ABI.to_vec()).unwrap();
        assert_eq!(text, format!("\0CORVUS-PLUGIN\0{ABI}\0"));
        assert!(stamp().starts_with(ABI));
    }
}
