//! Plugins the operator installed: shared libraries under
//! `~/.corvus/plugin-packages/<id>/`, built by the same compiler from the same
//! source as this program and loaded at start.
//!
//! A package folder holds `manifest.json` (`{"id", "version", "library", "sha256"}`) and the
//! library it names. Before the library is loaded its checksum and the stamp in its file are
//! compared, so one built for something else is listed as refused and none of its code runs. A
//! refused or broken package is a line for the operator and never stops the program.
//!
//! A library that takes the whole program down cannot be caught, so a marker file is there for as
//! long as one is being loaded and started ([`Trial`]). One that is still there at the next start
//! means the program did not survive it, and that package is left out until it is installed again.
//!
//! A loaded library is never unloaded: a plugin may still own a thread or a
//! callback whose code lives in it, and unmapping that is a crash at exit.

use std::ffi::{c_char, CStr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eframe::egui;
use serde_json::Value;

use corvus_api::plugin::dynamic::{self, Entry, Link};
use corvus_api::plugin::options::OptionSpec;
use corvus_api::plugin::{Host, Info, MapEvent, McpTool, Plugin};

pub const MANIFEST: &str = "manifest.json";
/// There while the library is loaded and the plugin started.
pub const TRIAL: &str = ".loading";

/// What is left over in the package folder is cleared away only when it is this old: another
/// window may be installing right now.
const LEFTOVER_AGE: Duration = Duration::from_secs(600);

static OFF: AtomicBool = AtomicBool::new(false);

/// Loads no packages in this run (`--no-plugins`).
pub fn switch_off() {
    OFF.store(true, Ordering::Relaxed);
}

/// A package that did not load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub id: String,
    /// In plain words, without the full stop.
    pub why: String,
    /// Built for another version of the program: an update from the store is what it needs.
    pub outdated: bool,
}

/// A plugin that loaded, and the checksum of the library it came from.
pub struct Package {
    pub plugin: Box<dyn Plugin>,
    pub id: String,
    pub sha256: String,
}

/// What `installed` found: the plugins that loaded and why the others did not.
#[derive(Default)]
pub struct Found {
    pub packages: Vec<Package>,
    pub refused: Vec<Refused>,
}

/// Where the packages are kept: `CORVUS_PLUGIN_PACKAGES`, else `~/.corvus/plugin-packages`.
/// Nowhere for a test that names no folder, which must not run what the developer installed.
pub fn packages_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CORVUS_PLUGIN_PACKAGES").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if cfg!(test) {
        return None;
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".corvus").join("plugin-packages"))
}

/// The marker that a package is on trial: made when its library is about to be loaded or its
/// plugin about to be started, and gone again once the program has survived that.
pub struct Trial(PathBuf);

impl Trial {
    pub fn begin(folder: &Path) -> Trial {
        let path = folder.join(TRIAL);
        // Where it cannot be written the package is loaded without the safeguard.
        let _ = std::fs::write(&path, b"");
        Trial(path)
    }
}

impl Drop for Trial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Loads every package under `dir` but those in `taken`, which the program has already. The
/// language files of each go to the translation layer, as a compiled in plugin's do.
pub fn installed(dir: &Path, taken: &[&str]) -> Found {
    let mut found = Found::default();
    if OFF.load(Ordering::Relaxed) {
        return found;
    }
    tidy(dir);
    let Ok(read) = std::fs::read_dir(dir) else {
        return found;
    };
    let mut ids: Vec<String> = read
        .flatten()
        .filter(|e| e.path().join(MANIFEST).is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| corvus_package::is_valid_id(n))
        .collect();
    ids.sort();
    for id in ids {
        // A copy from the time it was a plugin: nothing to load and nothing to tell.
        if super::PARTS.contains(&id.as_str()) {
            continue;
        }
        if taken.contains(&id.as_str()) {
            found.refused.push(Refused {
                id,
                why: "this plugin is part of this version of Corvus GCS, so the installed one is not needed".into(),
                outdated: false,
            });
            continue;
        }
        match load_package(dir, &id) {
            Ok(p) => found.packages.push(p),
            Err(r) => found.refused.push(r),
        }
    }
    found
}

/// Clears away what an interrupted install or a removal left behind: a library no manifest names,
/// a half written file, a folder with no manifest.
fn tidy(dir: &Path) {
    let old = |p: &Path| {
        std::fs::symlink_metadata(p)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|e| e > LEFTOVER_AGE))
    };
    for folder in std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()) {
        let valid = folder
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(corvus_package::is_valid_id);
        if !valid || !folder.is_dir() {
            continue;
        }
        let named = read_manifest(&folder).ok().map(|m| m.library);
        if named.is_none() && folder.join(MANIFEST).exists() {
            // A manifest that does not read is for the operator to see, not to lose.
            continue;
        }
        for e in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let ours = name.ends_with(".tmp")
                || name.ends_with(corvus_package::library_extension())
                || (named.is_none() && name == TRIAL);
            if ours && named.as_deref() != Some(name.as_str()) && old(&e.path()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
        if named.is_none() {
            // Only an empty folder goes.
            let _ = std::fs::remove_dir(&folder);
        }
    }
}

/// What a package says of itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub library: String,
    /// Empty when the manifest carries none.
    pub sha256: String,
    /// The file beside the program it was installed from, and what that looked like
    /// (`store::follow_beside`); empty for one from a release or from anywhere else.
    pub origin: String,
    pub origin_stamp: String,
}

pub fn read_manifest(folder: &Path) -> Result<Manifest, String> {
    let text =
        std::fs::read_to_string(folder.join(MANIFEST)).map_err(|e| format!("manifest.json cannot be read: {e}"))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("manifest.json is not valid: {e}"))?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let library = s("library");
    if !corvus_package::is_plain_file_name(&library) {
        return Err("manifest.json names no library file".into());
    }
    Ok(Manifest {
        id: s("id"),
        version: s("version"),
        library,
        sha256: s("sha256").to_ascii_lowercase(),
        origin: s("origin"),
        origin_stamp: s("origin_stamp"),
    })
}

/// Loads the package `id` of `dir`: its library, checked, and its language files registered.
pub fn load_package(dir: &Path, id: &str) -> Result<Package, Refused> {
    let refuse = |why: String| Refused {
        id: id.to_string(),
        why,
        outdated: false,
    };
    if OFF.load(Ordering::Relaxed) {
        return Err(refuse("plugins are switched off for this run (--no-plugins)".into()));
    }
    let folder = dir.join(id);
    let manifest = read_manifest(&folder).map_err(refuse)?;
    if manifest.id != id {
        return Err(refuse(format!(
            "manifest.json is of {:?}, not of this folder",
            manifest.id
        )));
    }
    if folder.join(TRIAL).exists() {
        return Err(refuse(
            "Corvus GCS ended while starting it the last time, so it is left out. Install it again to try once more"
                .into(),
        ));
    }
    let path = folder.join(&manifest.library);
    let bytes = std::fs::read(&path).map_err(|e| refuse(format!("the library cannot be read: {e}")))?;
    let sha256 = corvus_package::sha256_hex(&bytes);
    if manifest.sha256 != sha256 && !(manifest.sha256.is_empty() && cfg!(debug_assertions)) {
        return Err(refuse(
            "the library is not the file that was installed (its checksum differs)".into(),
        ));
    }
    match corvus_package::abi_of(&bytes) {
        None => return Err(refuse("the library is not a Corvus plugin".into())),
        Some(abi) if abi != dynamic::ABI => {
            return Err(Refused {
                id: id.to_string(),
                why: "built for another version of Corvus GCS".into(),
                outdated: true,
            })
        }
        Some(_) => {}
    }
    drop(bytes);
    let trial = Trial::begin(&folder);
    let entry = open(&path).map_err(|(why, outdated)| Refused {
        id: id.to_string(),
        why,
        outdated,
    })?;
    (entry.sync)(&Link::current());
    let plugin = (entry.make)().map_err(|e| refuse(format!("it could not be started: {e}")))?;
    drop(trial);
    if plugin.info().id != id {
        return Err(refuse(format!("the library is the plugin {:?}", plugin.info().id)));
    }
    corvus_api::i18n::register_plugin_files(entry.lang);
    Ok(Package {
        plugin: Box::new(Loaded { inner: plugin, entry }),
        id: id.to_string(),
        sha256,
    })
}

fn open(path: &Path) -> Result<&'static Entry, (String, bool)> {
    let other = || ("built for another version of Corvus GCS".to_string(), true);
    // SAFETY: loading runs the library's initialisers. It is the file that was installed (its
    // checksum) and a plugin of this very build (the stamp in the file), both checked before.
    let lib =
        unsafe { libloading::Library::new(path) }.map_err(|e| (format!("the library does not load: {e}"), false))?;
    // The library stays mapped for the life of the process (see the module docs).
    let lib: &'static libloading::Library = Box::leak(Box::new(lib));
    // SAFETY: `corvus_plugin_abi` is `extern "C" fn() -> *const c_char` as
    // `export_plugin!` writes it, a NUL ended string with static lifetime.
    let stamp = unsafe {
        let f: libloading::Symbol<unsafe extern "C" fn() -> *const c_char> =
            lib.get(b"corvus_plugin_abi\0").map_err(|_| other())?;
        let p = f();
        if p.is_null() {
            return Err(other());
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    };
    let want = dynamic::stamp();
    if stamp != want {
        eprintln!("plugin {}: {stamp} is not {want}", path.display());
        return Err(other());
    }
    // SAFETY: the stamp matched, so the library was built from this source by this
    // compiler and `CORVUS_PLUGIN` is an `Entry` with static lifetime.
    unsafe {
        let s: libloading::Symbol<*const Entry> = lib.get(b"CORVUS_PLUGIN\0").map_err(|_| other())?;
        s.as_ref().ok_or_else(other)
    }
}

/// `--check-plugins DIR`: whether every package of `dir` loads in this program. For the build,
/// which runs it with the finished program and the libraries it is about to publish.
pub fn check(dir: &Path) -> Result<String, String> {
    let found = installed(dir, &[]);
    for p in &found.packages {
        println!("plugin {} {}: loads", p.id, p.plugin.info().version);
    }
    if found.refused.is_empty() && !found.packages.is_empty() {
        return Ok(format!("{} plugins load", found.packages.len()));
    }
    let why: Vec<String> = found.refused.iter().map(|r| format!("{}: {}", r.id, r.why)).collect();
    Err(if why.is_empty() {
        format!("no plugin packages in {}", dir.display())
    } else {
        why.join("\n")
    })
}

/// A plugin from a library. Its copy of `corvus_api` is brought up to date with the
/// program's before every call into it.
struct Loaded {
    inner: Box<dyn Plugin>,
    entry: &'static Entry,
}

impl Loaded {
    fn sync(&self) {
        (self.entry.sync)(&Link::current());
    }

    /// Re-raises, in the program, a panic the library caught in itself.
    fn after(&self) {
        if let Some(why) = (self.entry.panicked)() {
            panic!("the plugin library panicked: {why}");
        }
    }
}

impl Plugin for Loaded {
    fn info(&self) -> Info {
        self.inner.info()
    }
    fn options(&self) -> Vec<OptionSpec> {
        self.sync();
        let r = self.inner.options();
        self.after();
        r
    }
    fn start(&mut self, h: &mut Host) {
        self.sync();
        self.inner.start(h);
        self.after();
    }
    fn tick(&mut self, h: &mut Host) {
        self.sync();
        self.inner.tick(h);
        self.after();
    }
    fn open(&mut self, h: &mut Host) {
        self.sync();
        self.inner.open(h);
        self.after();
    }
    fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect, h: &mut Host) -> f32 {
        self.sync();
        let r = self.inner.show(ui, area, h);
        self.after();
        r
    }
    fn close(&mut self, h: &mut Host) {
        self.sync();
        self.inner.close(h);
        self.after();
    }
    fn options_changed(&mut self, h: &mut Host) {
        self.sync();
        self.inner.options_changed(h);
        self.after();
    }
    fn map_event(&mut self, e: MapEvent, h: &mut Host) {
        self.sync();
        self.inner.map_event(e, h);
        self.after();
    }
    fn message(&mut self, from: &str, msg: &Value, h: &mut Host) {
        self.sync();
        self.inner.message(from, msg, h);
        self.after();
    }
    fn ssh_setup_done(&mut self, tag: u64, saved: bool, h: &mut Host) {
        self.sync();
        self.inner.ssh_setup_done(tag, saved, h);
        self.after();
    }
    fn mcp_tools(&self, h: &Host) -> Vec<McpTool> {
        self.sync();
        let r = self.inner.mcp_tools(h);
        self.after();
        r
    }
    fn mcp_call(&mut self, name: &str, args: &Value, token: u64, h: &mut Host) {
        self.sync();
        self.inner.mcp_call(name, args, token, h);
        self.after();
    }
    fn mcp_take(&mut self, token: u64) -> Option<Result<Value, String>> {
        self.sync();
        let r = self.inner.mcp_take(token);
        self.after();
        r
    }
}

/// Real plugin libraries for the tests: cargo builds one for every dev-dependency that is a plugin.
#[cfg(test)]
pub mod testlib {
    use super::*;

    /// The library cargo built for crate `krate`, beside this test program.
    pub fn built(krate: &str) -> PathBuf {
        let deps = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        let name = if cfg!(target_os = "windows") {
            format!("{krate}.dll")
        } else {
            format!("lib{krate}.{}", corvus_package::library_extension())
        };
        deps.join(name)
    }

    pub fn empty(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("corvus-loader-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Installs the library of `krate` as package `id` of `dir`, as the store does.
    pub fn package(dir: &Path, id: &str, krate: &str) -> PathBuf {
        let folder = dir.join(id);
        std::fs::create_dir_all(&folder).unwrap();
        let file = format!("lib.{}", corvus_package::library_extension());
        std::fs::copy(built(krate), folder.join(&file)).unwrap();
        let sha = corvus_package::sha256_file(&folder.join(&file)).unwrap();
        let manifest = serde_json::json!({"id": id, "version": "1.0.0", "library": file, "sha256": sha});
        std::fs::write(folder.join(MANIFEST), manifest.to_string()).unwrap();
        folder
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use corvus_api::i18n;
    use corvus_api::ui::{fonts, style};
    use corvus_plugin_probe::{knock, FOREIGN};
    use serde_json::json;

    use super::testlib::{empty, package};
    use super::*;
    use crate::net::http::Server;
    use crate::plugins::Registry;

    /// The panic of a library is one `static` of it, so the tests that make it panic take turns.
    static TURN: Mutex<()> = Mutex::new(());

    struct Rig {
        reg: Registry,
        ctx: egui::Context,
        dir: PathBuf,
        _turn: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Rig {
        fn new(tag: &str) -> Rig {
            let turn = TURN.lock().unwrap_or_else(|p| p.into_inner());
            let dir = empty(tag);
            package(&dir, "probe", "corvus_plugin_probe");
            let ctx = egui::Context::default();
            fonts::install(&ctx);
            // Nothing listens there: the registry goes on without saved settings.
            let reg = Registry::with_packages(Server::local(9), &ctx, &json!({}), Vec::new(), &dir);
            assert_eq!(reg.refused, []);
            let mut rig = Rig {
                reg,
                ctx,
                dir,
                _turn: turn,
            };
            let end = Instant::now() + Duration::from_secs(10);
            while !rig.reg.loaded {
                assert!(Instant::now() < end, "the registry did not start");
                rig.turn();
            }
            rig.turn();
            rig
        }

        fn turn(&mut self) {
            let ctx = self.ctx.clone();
            self.reg.update(&ctx, &json!({}));
        }

        fn tell(&mut self, what: &str) {
            self.reg.bus.send("test", "probe", json!(what));
            self.turn();
            self.turn();
        }

        fn read(&self, name: &str) -> Value {
            self.reg.bus.read(&format!("probe/{name}")).cloned().unwrap_or_default()
        }

        /// One frame of the window with the plugin drawn in it.
        fn frame(&mut self) {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))),
                ..Default::default()
            };
            let reg = &mut self.reg;
            let mut out = self.ctx.clone().run_ui(input, |ui| {
                let ctx = ui.ctx().clone();
                reg.with(0, &ctx, &json!({}), |p, h| p.show(ui, ui.max_rect(), h));
            });
            out.textures_delta.clear();
        }
    }

    /// What the operator does in Settings, with the real libraries: every plugin there is, installed
    /// from where the build left it, runs at once and is there again at the next start.
    #[test]
    fn every_plugin_is_installed_from_beside_the_program_and_runs() {
        use crate::plugins::store::{Source, Store};
        let _turn = TURN.lock().unwrap_or_else(|p| p.into_inner());
        let dir = empty("store");
        let ctx = egui::Context::default();
        fonts::install(&ctx);
        let source = Source {
            base: Some("http://127.0.0.1:9".into()),
            abi: dynamic::ABI.into(),
            platform: corvus_package::platform(),
            dir: dir.clone(),
            beside: vec![super::testlib::built("x").parent().unwrap().to_path_buf()],
            offered: crate::plugins::OFFERED,
        };
        let server = || Server::local(9);
        let mut reg = Registry::with_packages(server(), &ctx, &json!({}), Vec::new(), &dir);
        assert_eq!(reg.len(), 0);
        reg.store = Store::with_source(Some(source));
        reg.store.want(&ctx);
        for o in crate::plugins::OFFERED {
            reg.store.install(o.id);
        }
        let end = Instant::now() + Duration::from_secs(60);
        while reg.len() < crate::plugins::OFFERED.len() || !reg.loaded {
            assert!(Instant::now() < end, "{:?} {:?}", reg.store.message(), reg.refused);
            reg.update(&ctx, &json!({}));
            std::thread::sleep(Duration::from_millis(5));
        }
        for _ in 0..3 {
            reg.update(&ctx, &json!({}));
        }
        assert_eq!(reg.refused, []);
        for i in 0..reg.len() {
            assert_eq!(reg.failure(i), None, "{}", reg.info(i).id);
        }
        drop(reg);
        let again = Registry::with_packages(server(), &ctx, &json!({}), Vec::new(), &dir);
        assert_eq!(again.refused, []);
        assert_eq!(again.len(), crate::plugins::OFFERED.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every plugin that is published loads in the program it is published for.
    #[test]
    fn every_published_plugin_loads_as_a_library() {
        let _turn = TURN.lock().unwrap_or_else(|p| p.into_inner());
        let dir = empty("all");
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let dev = text.split("[dev-dependencies]").nth(1).unwrap_or_default();
        let mut seen = Vec::new();
        for e in std::fs::read_dir(root.join("plugins")).unwrap().flatten() {
            let manifest = std::fs::read_to_string(e.path().join("Cargo.toml")).unwrap_or_default();
            if !manifest.lines().any(|l| l.trim() == "loadable = true") {
                continue;
            }
            let id = e.file_name().into_string().unwrap();
            let krate = format!("corvus_plugin_{}", id.replace('-', "_"));
            assert!(
                dev.contains(&format!("\n{krate} = ")),
                "add {krate} to [dev-dependencies] in Cargo.toml, so this test can load its library"
            );
            let folder = package(&dir, &id, &krate);
            let found = load_package(&dir, &id).unwrap_or_else(|r| panic!("{id}: {}", r.why));
            assert_eq!(found.plugin.info().id, id);
            assert!(!folder.join(TRIAL).exists());
            seen.push(id);
        }
        assert!(["vibration", "nuttx-console", "trajectory-viewer", "tak"]
            .iter()
            .all(|id| seen.iter().any(|s| s == id)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_library_and_the_program_wake_each_other() {
        let mut r = Rig::new("wake");
        r.tell("knock");
        let stop = Arc::new(AtomicBool::new(false));
        let counts: Vec<Arc<AtomicU64>> = (0..2).map(|_| Arc::default()).collect();
        let threads: Vec<_> = counts
            .iter()
            .map(|n| {
                let (ctx, stop, n) = (r.ctx.clone(), stop.clone(), n.clone());
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        knock(&ctx);
                        n.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        // A thread that slept through its wake up call stays asleep, so each must still be moving
        // at the end. A frozen window would not come back from `frame`.
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            r.frame();
            r.turn();
        }
        let before: Vec<u64> = counts.iter().map(|n| n.load(Ordering::Relaxed)).collect();
        let lib_before = r.read("rounds").as_u64().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let moving = |r: &mut Rig| {
            r.turn();
            counts.iter().zip(&before).all(|(n, b)| n.load(Ordering::Relaxed) > *b)
                && r.read("rounds").as_u64().unwrap() > lib_before
        };
        while !moving(&mut r) {
            assert!(
                Instant::now() < deadline,
                "a thread waits for a lock nobody will wake it for"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        stop.store(true, Ordering::Relaxed);
        for t in threads {
            t.join().unwrap();
        }
    }

    #[test]
    fn a_font_found_by_a_library_does_not_replace_the_programs() {
        let mut r = Rig::new("font");
        let other = egui::FontDefinitions::default().font_data["Hack"].clone();
        let width = |ctx: &egui::Context, text: &str, size: f32| {
            let mut w = 0.0;
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let g = style::galley(ui, text, size, style::Weight::W400, style::text_1(), 0.0);
                w = g.size().x;
            });
            out.textures_delta.clear();
            w
        };
        let (a, b) = (r.ctx.clone(), egui::Context::default());
        fonts::install(&b);
        for ctx in [&a, &b] {
            let data = (*other).clone();
            ctx.add_font_provider(Arc::new(move |_: &egui::epaint::text::FallbackRequest<'_>| {
                Some(egui::epaint::text::FontInsert::new("found", data.clone(), Vec::new()))
            }));
        }
        assert!(width(&a, "Altitude 120 m", 14.0) > 0.0);
        // The library draws a character only the found font has, and is the first to use a weight.
        r.frame();
        assert!(FOREIGN.chars().count() == 1);
        for size in [13.0, 15.0, 22.0] {
            for text in ["Altitude 120 m", "Corvus", "WWW iii"] {
                assert_eq!(width(&a, text, size), width(&b, text, size), "{text} {size}");
            }
        }
    }

    #[test]
    fn a_library_follows_the_theme_the_language_and_tablet_mode() {
        let mut r = Rig::new("follow");
        let (theme, lang) = (style::palette(), i18n::language());
        for (id, code, text) in [("blue", "de", "Sondentext"), ("light", "en", "Probe text")] {
            style::set_theme(id);
            i18n::set_language(code);
            r.tell("look");
            let seen = r.read("look");
            assert_eq!(seen["bg"], json!(style::palette_of(id).bg), "{id}");
            assert_eq!(seen["text"], text);
            assert_eq!(seen["none"], "Text nobody translated");
            assert_eq!(seen["touch"], json!(style::touch()));
        }
        style::set_palette(theme);
        i18n::set_language(&lang);
    }

    #[test]
    fn a_panic_in_a_library_stops_that_plugin_and_nothing_else() {
        let mut r = Rig::new("panic");
        assert_eq!(r.reg.failure(0), None);
        r.tell("panic");
        let why = r.reg.failure(0).expect("the plugin was stopped").to_string();
        assert!(
            why.contains("the probe was told to") && why.contains("lib.rs:"),
            "{why}"
        );
        // It is left alone from here on, and the next one of its kind is not held to it.
        r.frame();
        r.turn();
        let again = load_package(&r.dir, "probe")
            .map(|p| p.plugin)
            .unwrap_or_else(|e| panic!("{}", e.why));
        assert_eq!(again.info().id, "probe");
    }

    #[test]
    fn a_panic_while_a_plugin_is_dropped_stays_in_its_library() {
        let mut r = Rig::new("drop");
        r.tell("drop panics");
        drop(std::mem::take(&mut r.reg.entries));
        let left = load_package(&r.dir, "probe")
            .map(|p| p.plugin)
            .unwrap_or_else(|e| panic!("{}", e.why));
        let _ = left.options();
    }

    #[test]
    fn a_package_that_cannot_load_is_refused_with_a_reason() {
        let lib = format!("x.{}", corvus_package::library_extension());
        let sha = |b: &[u8]| corvus_package::sha256_hex(b);
        let mine = format!("\0CORVUS-PLUGIN\0{}\0", dynamic::ABI).into_bytes();
        let other = b"\0CORVUS-PLUGIN\x001|rustc 0.1|00\0".to_vec();
        let manifest = |b: &[u8]| json!({"id": "p", "library": lib, "sha256": sha(b)}).to_string();
        for (tag, manifest, bytes, reason, outdated) in [
            ("none", r#"{"id": "p"}"#.to_string(), None, "names no library", false),
            (
                "path",
                r#"{"id": "p", "library": "../x.dylib"}"#.into(),
                None,
                "names no library",
                false,
            ),
            ("bad", "not json".into(), None, "not valid", false),
            (
                "id",
                json!({"id": "q", "library": lib}).to_string(),
                None,
                "not of this folder",
                false,
            ),
            ("gone", manifest(b""), None, "cannot be read", false),
            (
                "sum",
                manifest(b"one"),
                Some(b"two".to_vec()),
                "checksum differs",
                false,
            ),
            (
                "what",
                manifest(b"one"),
                Some(b"one".to_vec()),
                "not a Corvus plugin",
                false,
            ),
            ("old", manifest(&other), Some(other.clone()), "another version", true),
            ("junk", manifest(&mine), Some(mine.clone()), "does not load", false),
        ] {
            let dir = empty(tag);
            std::fs::create_dir_all(dir.join("p")).unwrap();
            std::fs::write(dir.join("p").join(MANIFEST), manifest).unwrap();
            if let Some(b) = bytes {
                std::fs::write(dir.join("p").join(&lib), b).unwrap();
            }
            let found = installed(&dir, &[]);
            assert!(found.packages.is_empty(), "{tag}");
            assert_eq!(found.refused.len(), 1, "{tag}");
            let r = &found.refused[0];
            assert!(r.why.contains(reason) && r.outdated == outdated, "{tag}: {r:?}");
            assert!(check(&dir).is_err());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_package_the_program_did_not_survive_is_left_out_until_it_is_installed_again() {
        let dir = empty("trial");
        let folder = package(&dir, "vibration", "corvus_plugin_vibration");
        std::mem::forget(Trial::begin(&folder));
        let found = installed(&dir, &[]);
        assert!(found.packages.is_empty());
        assert!(found.refused[0].why.contains("Install it again"), "{:?}", found.refused);
        assert!(folder.join(TRIAL).exists(), "it stays left out");
        std::fs::remove_file(folder.join(TRIAL)).unwrap();
        assert_eq!(installed(&dir, &[]).packages.len(), 1);
        assert!(!folder.join(TRIAL).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plugin_the_program_has_is_not_loaded_a_second_time() {
        let dir = empty("taken");
        package(&dir, "vibration", "corvus_plugin_vibration");
        let found = installed(&dir, &["vibration"]);
        assert!(found.packages.is_empty() && found.refused[0].why.contains("part of this version"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_old_leftovers_are_cleared_away() {
        let dir = empty("tidy");
        let ext = corvus_package::library_extension();
        let keep = format!("keep.{ext}");
        let (p, gone) = (dir.join("p"), dir.join("gone"));
        for f in [&p, &gone] {
            std::fs::create_dir_all(f).unwrap();
        }
        std::fs::write(p.join(MANIFEST), json!({"id": "p", "library": keep}).to_string()).unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        for f in [
            p.join(&keep),
            p.join(format!("old.{ext}")),
            p.join("half.1.tmp"),
            p.join("notes.txt"),
            gone.join(format!("lib.{ext}")),
        ] {
            std::fs::write(&f, b"x").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&f)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        std::fs::write(p.join(format!("new.{ext}")), b"x").unwrap();
        tidy(&dir);
        let mut left: Vec<String> = std::fs::read_dir(&p)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, [keep, MANIFEST.into(), format!("new.{ext}"), "notes.txt".into()]);
        assert!(!gone.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_packages_folder_is_no_packages() {
        let found = installed(Path::new("/nonexistent/corvus-packages"), &[]);
        assert!(found.packages.is_empty() && found.refused.is_empty());
    }
}
