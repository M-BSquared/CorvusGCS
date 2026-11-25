//! The plugin store: the plugins there are, and installing and removing them.
//!
//! Which plugins there are is known from the source this program was built from ([`Offered`]), so
//! the list is complete without a network. A plugin is one module for every system, and it names
//! the oldest Corvus GCS it needs: any version from there on takes it. It comes from one of three
//! places:
//!
//! * Beside the program: the folder `corvus-plugins` next to it, or where a build left it
//!   (`./run.sh`). Needs no network, and is taken first.
//! * The plugin's own repository on GitHub: its release [`corvus_package::RELEASE_TAG`], whose
//!   `plugins-index.json` names the modules, several versions of the plugin if need be. The newest
//!   one this program is new enough for is taken. Read over HTTPS when the operator opens the
//!   page. A module is written only after its size and SHA-256 match the list. There is no
//!   signing key: what is trusted is the plugin's repository on GitHub.
//! * A file the operator picks, for a computer without internet.
//!
//! What a plugin may do is shown before it is installed, and a module that asks for more than was
//! shown is not installed.
//!
//! It goes to the folder the loader reads at start (`loader::packages_dir`). The window's own
//! computer is where it installs: with `--server` the backend may be elsewhere, the module runs
//! here.
//!
//! A module is stored under a name made of its checksum and the manifest that names it is
//! replaced in one rename, so a crash, a full disk or a lost connection leaves the old plugin or
//! the new one and never half of one.
//!
//! A plugin that is installed is loaded at once (`Registry::poll_store`). One that is
//! updated or removed keeps running until the next start, and the page says so.
//!
//! What the setup asked for and could not be had, for want of a connection, is written down
//! ([`WANTED`]) and tried again at the next start.

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use corvus_core::plugins::http_client;
use corvus_package::{Entry, Index, Manifest, INDEX_FILE};
use eframe::egui;
use serde_json::{json, Value};

use super::loader::{self, Installed};
use super::wasm::{runtime, Sandbox};
use crate::net::bg::Pending;

const INDEX_CAP: usize = 1024 * 1024;
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// For a whole module over a slow link. A server that falls silent is given up on much earlier.
const MODULE_TIMEOUT: Duration = Duration::from_secs(1800);
const NOTES_MAX: usize = 32;
const UNREACHED: &str = "could not reach the server";
const NOT_PUBLISHED: &str = "this plugin is not published yet";
const ASKS_MORE: &str =
    "it asks for more than was shown here. Look at it again under Settings, Plugins before you install it";

/// What the operator reads wherever plugins are installed.
pub const RISK: &str = "A plugin runs in a sandbox: it reaches only what its permissions name, which are shown before you install it. Permissions that run commands or control other computers give it wide reach, and not every plugin has been reviewed. Install only what you trust. You do so at your own risk.";

/// The folder beside the program that modules are taken from.
pub const BESIDE: &str = "corvus-plugins";

/// The file in the package folder that holds what still waits to be downloaded:
/// `{id: [permissions the operator saw]}`.
pub const WANTED: &str = "wanted.json";

/// A plugin of this source (`OFFERED`, made by `build.rs` from each `plugin.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offered {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub version: &'static str,
    pub min_corvus: &'static str,
    pub permissions: &'static [&'static str],
    /// Its repository on GitHub, whose releases it is installed from.
    pub repository: &'static str,
}

impl Offered {
    /// The crate the plugin is built from.
    pub fn package(&self) -> String {
        format!("corvus_plugin_{}", self.id.replace('-', "_"))
    }
}

/// Where the plugins come from and go to.
#[derive(Clone)]
pub struct Source {
    /// Releases from somewhere else: a folder per plugin there.
    pub base: Option<String>,
    /// The version of this program, which a plugin's minimum is held against.
    pub corvus: String,
    pub dir: PathBuf,
    /// The folders beside the program, in the order they are looked through.
    pub beside: Vec<PathBuf>,
    /// The plugins there are.
    pub offered: &'static [Offered],
    /// Compiles a module while it is installed, so the next start need not.
    pub sandbox: Option<Arc<Sandbox>>,
}

impl Source {
    /// The real one for this program, or `None` where there is no folder to install to.
    pub fn real(sandbox: Option<Arc<Sandbox>>) -> Option<Source> {
        #[cfg_attr(not(debug_assertions), allow(unused_mut))]
        let mut src = Source {
            base: None,
            corvus: corvus_core::version::version().to_string(),
            dir: loader::packages_dir()?,
            beside: std::env::current_exe()
                .map(|exe| beside(&exe, std::env::var_os("APPIMAGE").map(PathBuf::from).as_deref()))
                .unwrap_or_default(),
            offered: super::OFFERED,
            sandbox,
        };
        // A build for development can read the releases from a place of its own, a folder for
        // each plugin: `CORVUS_PLUGIN_STORE=<base url>`. A shipped build has no such door.
        #[cfg(debug_assertions)]
        if let Ok(base) = std::env::var("CORVUS_PLUGIN_STORE") {
            src.base = Some(base.trim_end_matches('/').to_string());
        }
        Some(src)
    }
}

/// The folders beside the program `exe`: the one it is in, and `corvus-plugins` beside it,
/// beside its folder, beside the `.app` it is in and beside the AppImage it was started from.
fn beside(exe: &Path, appimage: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut add = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    if let Some(dir) = exe.parent() {
        add(dir.to_path_buf());
        add(dir.join(BESIDE));
        let app = dir.ancestors().find(|a| a.extension().is_some_and(|e| e == "app"));
        let around = if app.is_some() {
            app.and_then(Path::parent)
        } else {
            dir.parent()
        };
        if let Some(around) = around {
            add(around.join(BESIDE));
        }
    }
    if let Some(dir) = appimage.and_then(Path::parent) {
        add(dir.join(BESIDE));
    }
    out
}

/// A module on this computer that this program can run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub path: PathBuf,
    pub sha256: String,
    pub manifest: Manifest,
}

/// Whether `name` is one a module of `o` goes by: what cargo calls it, or what it is published as.
fn is_module_name(name: &str, o: &Offered) -> bool {
    name == corvus_package::built_name(&o.package())
        || (name.starts_with(&format!("{}-", o.id)) && name.ends_with(&format!(".{}", corvus_package::EXTENSION)))
}

/// The newest module of `o` beside the program that this program takes.
fn find_beside(src: &Source, o: &Offered) -> Option<Local> {
    for dir in &src.beside {
        let mut found: Vec<Local> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_str().is_some_and(|n| is_module_name(n, o)))
            .filter_map(|e| {
                let bytes = read_module(&e.path()).ok()?;
                let manifest = Manifest::of_module(&bytes).ok()?;
                (manifest.id == o.id && manifest.fits(&src.corvus)).then(|| Local {
                    sha256: corvus_package::sha256_hex(&bytes),
                    path: e.path(),
                    manifest,
                })
            })
            .collect();
        found.sort_by(|a, b| {
            (corvus_package::plugin_version(&a.manifest.version), &a.path)
                .cmp(&(corvus_package::plugin_version(&b.manifest.version), &b.path))
        });
        if let Some(newest) = found.pop() {
            return Some(newest);
        }
    }
    None
}

/// Every plugin whose module lies beside the program.
pub fn scan_beside(src: &Source) -> BTreeMap<String, Local> {
    src.offered
        .iter()
        .filter_map(|o| find_beside(src, o).map(|l| (o.id.to_string(), l)))
        .collect()
}

fn read_module(path: &Path) -> Result<Vec<u8>, String> {
    let cannot = |e: std::io::Error| format!("the file cannot be read: {e}");
    let file = std::fs::File::open(path).map_err(cannot)?;
    let meta = file.metadata().map_err(cannot)?;
    if !meta.is_file() {
        return Err("it is not a file".into());
    }
    let mut bytes = Vec::new();
    file.take(loader::MODULE_CAP as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(cannot)?;
    if bytes.len() > loader::MODULE_CAP {
        return Err("the file is larger than a plugin can be".into());
    }
    Ok(bytes)
}

fn get(url: &str, cap: usize, timeout: Duration, seen: http_client::Seen) -> Result<Vec<u8>, String> {
    if !cfg!(debug_assertions) && !url.starts_with("https://") {
        return Err("plugins are only fetched over HTTPS".into());
    }
    let ua = format!("CorvusGCS/{}", corvus_core::version::version());
    let reply = http_client::get_seen(url, &ua, "*/*", timeout, cap, seen).map_err(|e| format!("{UNREACHED} ({e})"))?;
    if reply.status == 404 {
        return Err(NOT_PUBLISHED.into());
    }
    if reply.status != 200 {
        return Err(format!("the server answered HTTP {}", reply.status));
    }
    if reply.truncated || reply.body.len() > cap {
        return Err("the answer was larger than expected".into());
    }
    Ok(reply.body)
}

/// The folder the files of plugin `o` are in.
fn release_of(src: &Source, o: &Offered) -> String {
    match &src.base {
        Some(base) => format!("{base}/{}", o.id),
        None => format!("{}/releases/download/{}", o.repository, corvus_package::RELEASE_TAG),
    }
}

/// The newest module the repository of plugin `o` offers that this program takes.
pub fn fetch(src: &Source, o: &Offered) -> Result<Entry, String> {
    let list = get(
        &format!("{}/{INDEX_FILE}", release_of(src, o)),
        INDEX_CAP,
        LIST_TIMEOUT,
        &mut |_, _| (),
    )?;
    let index = Index::parse(&list)?;
    if let Some(best) = index.best(o.id, &src.corvus) {
        return Ok(best.clone());
    }
    Err(match index.needs(o.id) {
        Some(min) => format!("this plugin needs Corvus GCS {min} or newer, this is {}", src.corvus),
        None => NOT_PUBLISHED.into(),
    })
}

/// What the repositories of `wanted` answered.
pub type Answers = BTreeMap<String, Result<Entry, String>>;

/// Asks them all at once: one that does not answer holds nobody up.
pub fn fetch_all(src: &Source, wanted: &[Offered]) -> Answers {
    std::thread::scope(|s| {
        let asked: Vec<_> = wanted.iter().map(|o| (o.id, s.spawn(move || fetch(src, o)))).collect();
        asked
            .into_iter()
            .map(|(id, t)| {
                let r = t.join().unwrap_or_else(|_| Err("the list could not be read".into()));
                (id.to_string(), r)
            })
            .collect()
    })
}

/// A plugin that was installed: its id and version.
pub type Done = (String, String);

/// How far an install has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Step {
    /// Asking where the library is.
    #[default]
    Connecting,
    Downloading {
        got: u64,
        total: u64,
    },
    /// Checking the library and writing it to the disk.
    Installing,
}

impl Step {
    /// The share that has arrived, 0 to 1, when that can be said.
    pub fn fraction(self) -> Option<f32> {
        match self {
            Step::Connecting => None,
            Step::Downloading { got, total } => Some((got as f64 / total.max(1) as f64).min(1.0) as f32),
            Step::Installing => Some(1.0),
        }
    }

    /// In words, for a line beside the bar.
    pub fn words(self) -> String {
        match self {
            Step::Connecting => "connecting\u{2026}".into(),
            Step::Downloading { got, total } => {
                let mb = |n: u64| format!("{:.1} MB", n as f64 / 1e6);
                // Not 100 before all of it is there.
                let percent = got.min(total) * 100 / total.max(1);
                format!("{} of {} \u{b7} {percent}%", mb(got), mb(total))
            }
            Step::Installing => "installing\u{2026}".into(),
        }
    }
}

/// Where an install on its thread says how far it is, for the window to show.
#[derive(Default)]
pub struct Meter {
    step: Mutex<Step>,
    ctx: Option<egui::Context>,
}

impl Meter {
    fn set(&self, step: Step) {
        *self.step.lock().unwrap_or_else(|p| p.into_inner()) = step;
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }

    fn now(&self) -> Step {
        *self.step.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Whether `asks` names nothing that `seen` does not.
fn within(asks: &[String], seen: &[String]) -> bool {
    asks.iter().all(|p| seen.contains(p))
}

/// Installs plugin `id`: the module beside the program, else the one of the release. `seen` is
/// what the operator was shown it may do.
pub fn install(src: &Source, id: &str, seen: &[String], meter: &Meter) -> Result<Done, String> {
    let o = src
        .offered
        .iter()
        .find(|o| o.id == id)
        .ok_or("there is no such plugin")?;
    if let Some(l) = find_beside(src, o) {
        if !within(&l.manifest.permissions, seen) {
            return Err(ASKS_MORE.into());
        }
        meter.set(Step::Installing);
        return install_file(src, &l.path);
    }
    let entry = fetch(src, o)?;
    if !within(&entry.manifest.permissions, seen) {
        return Err(ASKS_MORE.into());
    }
    let cap = usize::try_from(entry.size)
        .unwrap_or(loader::MODULE_CAP)
        .min(loader::MODULE_CAP);
    let total = entry.size;
    let url = format!("{}/{}", release_of(src, o), entry.file);
    let download = || {
        meter.set(Step::Downloading { got: 0, total });
        get(&url, cap, MODULE_TIMEOUT, &mut |got, _| {
            meter.set(Step::Downloading { got: got as u64, total })
        })
    };
    // A link that broke once often holds the next time.
    let bytes = download().or_else(|e| if e.starts_with(UNREACHED) { download() } else { Err(e) })?;
    meter.set(Step::Installing);
    if bytes.len() as u64 != entry.size || corvus_package::sha256_hex(&bytes) != entry.sha256 {
        return Err("the download does not match the list and was thrown away".into());
    }
    let manifest = Manifest::of_module(&bytes)?;
    if manifest.id != id || manifest.version != entry.manifest.version {
        return Err("the download is not the plugin the list names and was thrown away".into());
    }
    if !within(&manifest.permissions, seen) {
        return Err(ASKS_MORE.into());
    }
    write_package(src, &manifest, &bytes, None)?;
    Ok((id.to_string(), manifest.version))
}

/// Installs the plugin module `path`, whatever its name is: what it is stands in it. The
/// operator chose the file, with what it says it may do.
pub fn install_file(src: &Source, path: &Path) -> Result<Done, String> {
    // Before the bytes: a file that changes meanwhile is looked at again at the next start.
    let stamp = file_stamp(path);
    let bytes = read_module(path)?;
    let manifest = Manifest::of_module(&bytes)?;
    if !manifest.fits(&src.corvus) {
        return Err(format!(
            "it needs Corvus GCS {} or newer, this is {}",
            manifest.min_corvus, src.corvus
        ));
    }
    if super::PARTS.contains(&manifest.id.as_str()) {
        return Err("this is a part of Corvus GCS now and does not need to be installed".into());
    }
    // Only what lies beside the program is followed when it changes, not a file from anywhere.
    let origin = path
        .parent()
        .filter(|p| src.beside.iter().any(|b| b == p))
        .map(|_| (path, stamp.as_str()));
    write_package(src, &manifest, &bytes, origin)?;
    Ok((manifest.id, manifest.version))
}

fn write_package(src: &Source, m: &Manifest, bytes: &[u8], origin: Option<(&Path, &str)>) -> Result<(), String> {
    let folder = src.dir.join(&m.id);
    let sha256 = corvus_package::sha256_hex(bytes);
    let module = format!("{}.{}", &sha256[..16], corvus_package::EXTENSION);
    let mut manifest = json!({
        "id": m.id, "name": m.name, "version": m.version, "module": module, "sha256": sha256,
        "permissions": m.permissions,
    });
    if let Some((path, stamp)) = origin {
        manifest["origin"] = json!(path.to_string_lossy());
        manifest["origin_stamp"] = json!(stamp);
    }
    let manifest = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let written = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&folder)?;
        // The same module may be there already.
        let have = corvus_package::sha256_file(&folder.join(&module)).ok();
        if have.as_deref() != Some(sha256.as_str()) {
            put(&folder, &module, bytes)?;
        }
        Ok(())
    })();
    let cannot = |e: std::io::Error| format!("cannot write the plugin to {}: {e}", folder.display());
    written.map_err(cannot)?;
    // Before the manifest names it: what does not compile never replaces what ran.
    if let Some(sandbox) = &src.sandbox {
        runtime::compiled(sandbox, bytes, Some(&folder))?;
    }
    put(&folder, loader::MANIFEST, &manifest).map_err(cannot)
}

/// The size and time of a file, to see at a glance that it is another one.
fn file_stamp(path: &Path) -> String {
    let Ok(m) = std::fs::metadata(path) else {
        return String::new();
    };
    let t = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("{}-{t}", m.len())
}

/// Brings the installed plugins that came from beside the program up to date with what lies there
/// now, before they are loaded: a program that was built again, or copied over together with its
/// plugins, goes on with the plugins that fit it.
pub fn follow_beside(src: &Source) {
    for (id, m) in installed(&src.dir) {
        if super::PARTS.contains(&id.as_str()) {
            continue;
        }
        let origin = Path::new(&m.origin);
        let followed = !m.origin.is_empty() && origin.parent().is_some_and(|p| src.beside.iter().any(|b| b == p));
        if !followed || !origin.is_file() || file_stamp(origin) == m.origin_stamp {
            continue;
        }
        if let Err(e) = install_file(src, origin) {
            eprintln!("plugin {id}: not brought up to date: {e}");
        }
    }
}

/// Writes `name` in `folder` through a file of its own and one rename.
fn put(folder: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = folder.join(format!("{name}.{}.tmp", std::process::id()));
    let done = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, folder.join(name))
    })();
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

/// Removes plugin `id`: without its manifest it is not loaded any more. What it saved stays.
pub fn remove(dir: &Path, id: &str) -> Result<(), String> {
    if !corvus_package::is_valid_id(id) {
        return Err("not a plugin".into());
    }
    let folder = dir.join(id);
    match std::fs::remove_file(folder.join(loader::MANIFEST)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("cannot remove the plugin ({e})")),
        _ => {}
    }
    for e in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
        let ours = e
            .path()
            .extension()
            .is_some_and(|x| x == corvus_package::EXTENSION || x == runtime::COMPILED);
        if ours {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let _ = std::fs::remove_dir(&folder);
    Ok(())
}

/// The installed packages, as their manifests say.
pub fn installed(dir: &Path) -> BTreeMap<String, Installed> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Some(id) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if let Ok(m) = loader::read_manifest(&e.path()) {
            if corvus_package::is_valid_id(&id) && m.id == id {
                out.insert(id, m);
            }
        }
    }
    out
}

/// What still waits to be downloaded, and what the operator saw each may do.
pub type Wanted = BTreeMap<String, Vec<String>>;

pub fn read_wanted(dir: &Path) -> Wanted {
    let Ok(text) = std::fs::read_to_string(dir.join(WANTED)) else {
        return Wanted::new();
    };
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    v.as_object()
        .into_iter()
        .flatten()
        .filter(|(id, _)| corvus_package::is_valid_id(id))
        .map(|(id, seen)| {
            let seen = seen
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p.as_str().map(str::to_string))
                .collect();
            (id.clone(), seen)
        })
        .collect()
}

fn write_wanted(dir: &Path, wanted: &Wanted) {
    let done = if wanted.is_empty() {
        match std::fs::remove_file(dir.join(WANTED)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        std::fs::create_dir_all(dir).and_then(|()| put(dir, WANTED, json!(wanted).to_string().as_bytes()))
    };
    if let Err(e) = done {
        eprintln!("plugins: cannot write {WANTED}: {e}");
    }
}

/// Where a module on offer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    Beside,
    Release,
}

/// A line of the store's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub description: String,
    /// The version on offer.
    pub version: String,
    /// Where it can be had, and the checksum of its module; `None` when it cannot be had now.
    pub offer: Option<(From, String)>,
    /// What is installed, when something is.
    pub installed: Option<Installed>,
    /// What the version on offer may do.
    pub permissions: Vec<String>,
    /// Why it cannot be had, when its repository could say.
    pub why_not: Option<String>,
}

impl Row {
    /// Another module than the installed one is on offer, and no older one.
    pub fn update_available(&self) -> bool {
        match (&self.installed, &self.offer) {
            (Some(m), Some((_, sha256))) => {
                m.sha256 != *sha256
                    && corvus_package::plugin_version(&self.version) >= corvus_package::plugin_version(&m.version)
            }
            _ => false,
        }
    }

    /// What the version on offer may do that the installed one may not.
    pub fn new_permissions(&self) -> Vec<&str> {
        let Some(m) = &self.installed else {
            return Vec::new();
        };
        self.permissions
            .iter()
            .filter(|p| !m.permissions.contains(p))
            .map(String::as_str)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Act {
    /// With what the operator was shown it may do.
    Install(String, Vec<String>),
    File(PathBuf),
    Remove(String),
}

impl Act {
    /// What it is about, for a message.
    fn subject(&self) -> String {
        match self {
            Act::Install(id, _) | Act::Remove(id) => id.clone(),
            Act::File(p) => p.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        }
    }
}

/// What is being done for a plugin, for its line in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doing {
    Waiting,
    Installing(Step),
    Removing,
}

/// The install under way, for the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// What the operator knows the plugin by.
    pub name: String,
    pub step: Step,
    /// Which one it is of how many that were asked for together.
    pub nth: usize,
    pub of: usize,
}

struct Job {
    act: Act,
    meter: Arc<Meter>,
    answer: Pending<Result<Done, String>>,
}

/// What the window knows of the store.
pub struct Store {
    source: Option<Source>,
    scan: Option<Pending<BTreeMap<String, Local>>>,
    beside: Option<BTreeMap<String, Local>>,
    list: Option<Pending<Answers>>,
    entries: Option<Answers>,
    have: BTreeMap<String, Installed>,
    /// What waits for its download, also across starts.
    wanted: Wanted,
    job: Option<Job>,
    waiting: VecDeque<Act>,
    /// Of what was asked for since nothing was under way: how many, and how many are over.
    asked: usize,
    over: usize,
    /// Results for the notification board, until [`Store::take_notes`].
    notes: Vec<(String, bool)>,
    /// The last result for the operator, and whether it went well.
    message: Option<(String, bool)>,
    /// Installs that have finished: `(id, version)`, until [`Store::take_installed`].
    done: VecDeque<Done>,
}

impl Store {
    /// The store of this program; `sandbox` compiles what is installed.
    pub fn new(sandbox: Option<Arc<Sandbox>>) -> Store {
        Store::with_source(Source::real(sandbox))
    }

    pub fn with_source(source: Option<Source>) -> Store {
        Store {
            wanted: source.as_ref().map(|s| read_wanted(&s.dir)).unwrap_or_default(),
            source,
            scan: None,
            beside: None,
            list: None,
            entries: None,
            have: BTreeMap::new(),
            job: None,
            waiting: VecDeque::new(),
            asked: 0,
            over: 0,
            notes: Vec::new(),
            message: None,
            done: VecDeque::new(),
        }
    }

    /// Where the plugins are installed.
    pub fn dir(&self) -> Option<&Path> {
        self.source.as_ref().map(|s| s.dir.as_path())
    }

    /// The plugins there are, installed or not.
    pub fn offered(&self) -> &'static [Offered] {
        self.source.as_ref().map_or(&[], |s| s.offered)
    }

    /// Takes in what came back and starts what waits. Every frame, and cheap when nothing is
    /// under way.
    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(found) = self.scan.as_ref().and_then(Pending::take) {
            self.scan = None;
            self.look();
            // What lies beside the program is not asked for.
            let wanted: Vec<Offered> = self
                .offered()
                .iter()
                .filter(|o| !found.contains_key(o.id))
                .copied()
                .collect();
            match &self.source {
                Some(src) if !wanted.is_empty() => {
                    let src = src.clone();
                    self.list = Some(Pending::spawn(ctx, move || fetch_all(&src, &wanted)));
                }
                _ => self.entries = Some(Answers::new()),
            }
            self.beside = Some(found);
        }
        if let Some(r) = self.list.as_ref().and_then(Pending::take) {
            self.entries = Some(r);
            self.list = None;
            self.look();
        }
        if let Some(r) = self.job.as_ref().and_then(|j| j.answer.take()) {
            if let Some(Job { act, .. }) = self.job.take() {
                self.over += 1;
                match (r, &act) {
                    (Ok((id, _)), Act::Remove(_)) => {
                        self.say(format!("{id} was removed. Restart Corvus GCS to finish."), true)
                    }
                    (Ok(done), _) => {
                        self.forget(&done.0);
                        self.done.push_back(done);
                    }
                    (Err(e), Act::Install(id, _)) if self.wanted.contains_key(id) => {
                        let name = self
                            .offered()
                            .iter()
                            .find(|o| o.id == id)
                            .map_or(id.as_str(), |o| o.name);
                        let text = format!("{name} is not downloaded yet: {e}. It is tried again at the next start.");
                        self.say(text, false)
                    }
                    (Err(e), _) => self.say(format!("{}: {e}.", act.subject()), false),
                }
                self.look();
            }
        }
        if self.job.is_none() {
            if let (Some(act), Some(src)) = (self.waiting.pop_front(), self.source.clone()) {
                let what = act.clone();
                let meter = Arc::new(Meter {
                    step: Mutex::new(match act {
                        Act::File(_) => Step::Installing,
                        _ => Step::Connecting,
                    }),
                    ctx: Some(ctx.clone()),
                });
                let m = meter.clone();
                let answer = Pending::spawn(ctx, move || match what {
                    Act::Install(id, seen) => install(&src, &id, &seen, &m),
                    Act::File(path) => install_file(&src, &path),
                    Act::Remove(id) => remove(&src.dir, &id).map(|()| (id, String::new())),
                });
                self.job = Some(Job { act, meter, answer });
            }
        }
    }

    fn look(&mut self) {
        self.have = self.source.as_ref().map(|s| installed(&s.dir)).unwrap_or_default();
    }

    /// The next plugin whose install has finished, once.
    pub fn take_installed(&mut self) -> Option<Done> {
        self.done.pop_front()
    }

    /// Asks for the list the first time it is wanted: the operator opened the page.
    pub fn want(&mut self, ctx: &egui::Context) {
        if self.entries.is_none() && self.list.is_none() && self.scan.is_none() {
            self.refresh(ctx);
        }
    }

    /// Asks for the list again.
    pub fn refresh(&mut self, ctx: &egui::Context) {
        self.look();
        match self.source.clone() {
            Some(src) => self.scan = Some(Pending::spawn(ctx, move || scan_beside(&src))),
            None => {
                self.entries = Some(Answers::new());
                self.message = Some(("There is no folder to install plugins to.".into(), false));
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.job.is_some() || self.list.is_some() || self.scan.is_some() || !self.waiting.is_empty()
    }

    /// Whether something is done or about to be done with plugin `id`.
    pub fn working_on(&self, id: &str) -> bool {
        self.job
            .iter()
            .map(|j| &j.act)
            .chain(&self.waiting)
            .any(|a| matches!(a, Act::Install(x, _) | Act::Remove(x) if x == id))
    }

    /// What is being done for plugin `id`.
    pub fn doing(&self, id: &str) -> Option<Doing> {
        let about = |a: &Act| matches!(a, Act::Install(x, _) | Act::Remove(x) if x == id);
        match &self.job {
            Some(j) if about(&j.act) => Some(match j.act {
                Act::Remove(_) => Doing::Removing,
                _ => Doing::Installing(j.meter.now()),
            }),
            _ => self.waiting.iter().any(about).then_some(Doing::Waiting),
        }
    }

    /// The install under way, when one is.
    pub fn progress(&self) -> Option<Progress> {
        let j = self.job.as_ref().filter(|j| !matches!(j.act, Act::Remove(_)))?;
        let subject = j.act.subject();
        let name = self.offered().iter().find(|o| o.id == subject);
        Some(Progress {
            name: name.map_or(subject, |o| o.name.to_string()),
            step: j.meter.now(),
            nth: self.over + 1,
            of: self.asked,
        })
    }

    /// Tells the operator how something went. A failure stays until something else is asked for,
    /// so a plugin that came after it does not hide it.
    pub fn say(&mut self, text: String, ok: bool) {
        if self.notes.len() < NOTES_MAX {
            self.notes.push((text.clone(), ok));
        }
        if !ok || self.message.as_ref().is_none_or(|(_, was_ok)| *was_ok) {
            self.message = Some((text, ok));
        }
    }

    /// The last result, and whether it went well.
    pub fn message(&self) -> Option<&(String, bool)> {
        self.message.as_ref()
    }

    /// The results since the last time, for the notification board.
    pub fn take_notes(&mut self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.notes)
    }

    /// Why a plugin of the list cannot be had, when one cannot.
    pub fn problem(&self) -> Option<&str> {
        self.entries
            .iter()
            .flatten()
            .find_map(|(_, r)| r.as_ref().err())
            .map(String::as_str)
    }

    pub fn loading(&self) -> bool {
        self.entries.is_none()
    }

    /// The list, each plugin with where it can be had and what is installed of it.
    pub fn rows(&self) -> Vec<Row> {
        let Some(src) = &self.source else {
            return Vec::new();
        };
        let mut rows: Vec<Row> = src
            .offered
            .iter()
            .map(|o| {
                let answer = self.entries.as_ref().and_then(|e| e.get(o.id));
                let entry = answer.and_then(|a| a.as_ref().ok());
                let beside = self.beside.as_ref().and_then(|b| b.get(o.id));
                let (offer, version, permissions) = match (beside, entry) {
                    (Some(l), _) => (
                        Some((From::Beside, l.sha256.clone())),
                        l.manifest.version.clone(),
                        l.manifest.permissions.clone(),
                    ),
                    (None, Some(e)) => (
                        Some((From::Release, e.sha256.clone())),
                        e.manifest.version.clone(),
                        e.manifest.permissions.clone(),
                    ),
                    (None, None) => (
                        None,
                        o.version.to_string(),
                        o.permissions.iter().map(|p| p.to_string()).collect(),
                    ),
                };
                Row {
                    id: o.id.into(),
                    name: o.name.into(),
                    description: o.description.into(),
                    version,
                    offer,
                    installed: self.have.get(o.id).cloned(),
                    permissions,
                    why_not: answer
                        .and_then(|a| a.as_ref().err())
                        .filter(|_| beside.is_none())
                        .cloned(),
                }
            })
            .collect();
        // Installed from a file and not on any list: it can be removed here.
        for (id, m) in &self.have {
            if super::PARTS.contains(&id.as_str()) {
                continue;
            }
            if !rows.iter().any(|r| r.id == *id) {
                rows.push(Row {
                    id: id.clone(),
                    name: id.clone(),
                    description: String::new(),
                    version: m.version.clone(),
                    offer: None,
                    permissions: m.permissions.clone(),
                    installed: Some(m.clone()),
                    why_not: None,
                });
            }
        }
        rows.sort_by_key(|r| r.name.to_lowercase());
        rows
    }

    /// What the operator sees plugin `id` may do.
    fn shown(&self, id: &str) -> Vec<String> {
        self.rows()
            .into_iter()
            .find(|r| r.id == id)
            .map(|r| r.permissions)
            .unwrap_or_default()
    }

    /// Installs or updates plugin `id`; one thing at a time, in the order asked.
    pub fn install(&mut self, id: &str) {
        if !self.working_on(id) {
            let seen = self.shown(id);
            self.queue(Act::Install(id.to_string(), seen));
        }
    }

    /// [`install`](Self::install) for the setup: asked for once, it is tried again at later
    /// starts until it is there, since a station may be set up without a connection.
    pub fn install_when_possible(&mut self, id: &str) {
        let seen = self.shown(id);
        self.wanted.insert(id.to_string(), seen);
        self.keep_wanted();
        self.install(id);
    }

    /// Queues what still waits from an earlier start. Once, when the program starts.
    pub fn resume(&mut self) {
        self.look();
        let there: Vec<String> = self
            .wanted
            .keys()
            .filter(|id| self.have.contains_key(*id))
            .cloned()
            .collect();
        for id in there {
            self.forget(&id);
        }
        let waiting: Vec<(String, Vec<String>)> = self.wanted.clone().into_iter().collect();
        for (id, seen) in waiting {
            if !self.working_on(&id) {
                self.queue(Act::Install(id, seen));
            }
        }
    }

    /// The plugins that still wait for their download.
    pub fn waiting_for_download(&self) -> Vec<String> {
        self.wanted.keys().cloned().collect()
    }

    fn keep_wanted(&self) {
        if let Some(src) = &self.source {
            write_wanted(&src.dir, &self.wanted);
        }
    }

    fn forget(&mut self, id: &str) {
        if self.wanted.remove(id).is_some() {
            self.keep_wanted();
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.forget(id);
        if !self.working_on(id) {
            self.queue(Act::Remove(id.to_string()));
        }
    }

    /// Installs the library `path` the operator picked.
    pub fn install_file(&mut self, path: PathBuf) {
        self.queue(Act::File(path));
    }

    fn queue(&mut self, act: Act) {
        if self.job.is_none() && self.waiting.is_empty() {
            (self.asked, self.over) = (0, 0);
            self.message = None;
        }
        self.asked += 1;
        self.waiting.push_back(act);
    }
}

impl Default for Store {
    fn default() -> Self {
        Store::new(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::wasm::testlib;
    use std::collections::HashMap;
    use std::net::TcpListener;
    use std::time::Instant;

    const NOW: &str = "2026.11.94";

    const OFFERED: &[Offered] = &[Offered {
        id: "vibration",
        name: "Vibration Monitor",
        description: "d",
        version: "2.0.0",
        min_corvus: "2026.11.94",
        permissions: &["telemetry.rates"],
        repository: "https://github.com/M-BSquared/corvus-vibration",
    }];

    fn seen() -> Vec<String> {
        vec!["telemetry.rates".into()]
    }

    fn manifest(version: &str, min: &str, permissions: &[&str]) -> Manifest {
        Manifest {
            id: "vibration".into(),
            name: "Vibration Monitor".into(),
            description: "d".into(),
            icon: "activity".into(),
            version: version.into(),
            order: 10,
            can_tab: false,
            min_corvus: min.into(),
            permissions: permissions.iter().map(|p| p.to_string()).collect(),
        }
    }

    /// A module that is its manifest and nothing else: all the store looks at.
    fn module(m: &Manifest) -> Vec<u8> {
        let mut section = vec![corvus_package::MANIFEST_SECTION.len() as u8];
        section.extend_from_slice(corvus_package::MANIFEST_SECTION.as_bytes());
        section.extend_from_slice(m.to_json().to_string().as_bytes());
        let mut out = b"\0asm\x01\0\0\0".to_vec();
        out.push(0);
        let mut len = section.len();
        loop {
            let byte = (len & 0x7f) as u8;
            len >>= 7;
            out.push(if len > 0 { byte | 0x80 } else { byte });
            if len == 0 {
                break;
            }
        }
        out.extend(section);
        out
    }

    fn usual() -> Vec<u8> {
        module(&manifest("2.0.0", NOW, &["telemetry.rates"]))
    }

    fn entry(bytes: &[u8]) -> Entry {
        let manifest = Manifest::of_module(bytes).unwrap();
        let sha256 = corvus_package::sha256_hex(bytes);
        Entry {
            file: corvus_package::published_name(&manifest.id, &manifest.version, &sha256),
            size: bytes.len() as u64,
            sha256,
            manifest,
        }
    }

    fn serve(files: HashMap<String, Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let files = Arc::new(files);
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let files = files.clone();
                std::thread::spawn(move || {
                    let mut conn = conn;
                    let mut head = [0u8; 2048];
                    let n = conn.read(&mut head).unwrap_or(0);
                    let text = String::from_utf8_lossy(&head[..n]).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let reply = match files.get(&path) {
                        Some(body) => {
                            let mut r = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            )
                            .into_bytes();
                            r.extend_from_slice(body);
                            r
                        }
                        None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                    };
                    let _ = conn.write_all(&reply);
                });
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    /// A release that lists `listed` and serves their files; `served` replaces what is sent.
    fn release(listed: &[Vec<u8>], served: Option<&[u8]>) -> HashMap<String, Vec<u8>> {
        let index = Index {
            plugins: listed.iter().map(|b| entry(b)).collect(),
        };
        let mut files = HashMap::new();
        files.insert(format!("/vibration/{INDEX_FILE}"), index.to_bytes());
        for b in listed {
            files.insert(
                format!("/vibration/{}", entry(b).file),
                served.map_or_else(|| b.clone(), <[u8]>::to_vec),
            );
        }
        files
    }

    fn source(base: String, tag: &str) -> Source {
        let dir = testlib::empty(&format!("store-{tag}"));
        Source {
            base: Some(base),
            corvus: NOW.into(),
            beside: vec![dir.join("beside")],
            dir: dir.join("packages"),
            offered: OFFERED,
            sandbox: None,
        }
    }

    fn clean(src: &Source) {
        let _ = std::fs::remove_dir_all(src.dir.parent().unwrap());
    }

    fn get_it(src: &Source) -> Result<Done, String> {
        install(src, "vibration", &seen(), &Meter::default())
    }

    #[test]
    fn a_plugin_is_asked_for_in_its_own_repository_under_one_name() {
        let mut src = source(String::new(), "release-of");
        src.base = None;
        assert_eq!(
            release_of(&src, &OFFERED[0]),
            "https://github.com/M-BSquared/corvus-vibration/releases/download/plugin"
        );
        clean(&src);
    }

    #[test]
    fn a_listed_plugin_is_installed_found_and_removed() {
        let src = source(serve(release(&[usual()], None)), "ok");
        assert_eq!(get_it(&src), Ok(("vibration".into(), "2.0.0".into())));
        let folder = src.dir.join("vibration");
        let kept = &installed(&src.dir)["vibration"];
        assert_eq!(kept.sha256, corvus_package::sha256_hex(&usual()));
        assert_eq!(kept.permissions, seen());
        assert_eq!(std::fs::read(folder.join(&kept.module)).unwrap(), usual());
        let files = || std::fs::read_dir(&folder).unwrap().count();
        assert_eq!(files(), 2, "nothing half written stays");
        get_it(&src).expect("installing it again");
        assert_eq!(files(), 2, "the same module is not stored twice");
        remove(&src.dir, "vibration").unwrap();
        assert!(installed(&src.dir).is_empty());
        assert!(!folder.exists());
        clean(&src);
    }

    #[test]
    fn the_newest_module_this_version_is_new_enough_for_is_installed() {
        let versions = [
            module(&manifest("2.0.0", NOW, &[])),
            module(&manifest("2.1.0", NOW, &[])),
            module(&manifest("3.0.0", "2027.01.01", &[])),
        ];
        let mut src = source(serve(release(&versions, None)), "newest");
        assert_eq!(get_it(&src).unwrap().1, "2.1.0");
        src.corvus = "2027.03.07".into();
        assert_eq!(get_it(&src).unwrap().1, "3.0.0");
        src.corvus = "2026.11.93".into();
        let why = get_it(&src).unwrap_err();
        assert!(why.contains("2026.11.94 or newer, this is 2026.11.93"), "{why}");
        clean(&src);
    }

    #[test]
    fn a_module_that_asks_for_more_than_was_shown_is_not_installed() {
        let greedy = module(&manifest("2.0.0", NOW, &["telemetry.rates", "local"]));
        let src = source(serve(release(std::slice::from_ref(&greedy), None)), "greedy");
        assert_eq!(get_it(&src), Err(ASKS_MORE.to_string()));
        assert!(!src.dir.exists());
        clean(&src);
        // A list that says less than its module does.
        let mut lied = entry(&greedy);
        lied.manifest.permissions = seen();
        let mut files = release(&[greedy], None);
        files.insert(
            format!("/vibration/{INDEX_FILE}"),
            Index { plugins: vec![lied] }.to_bytes(),
        );
        let src = source(serve(files), "lied");
        assert_eq!(get_it(&src), Err(ASKS_MORE.to_string()));
        assert!(installed(&src.dir).is_empty());
        clean(&src);
    }

    #[test]
    fn a_list_that_is_not_a_list_installs_nothing() {
        for (tag, text) in [
            ("junk", "not json".to_string()),
            ("native", r#"{"format": 1, "abi": "x", "plugins": []}"#.to_string()),
            ("newer", r#"{"format": 99, "plugins": []}"#.to_string()),
            (
                "evil",
                format!(
                    r#"{{"format": 2, "plugins": [{{"id": "vibration", "min_corvus": "{NOW}", "file": "../evil.wasm", "sha256": "{}", "size": 25}}]}}"#,
                    corvus_package::sha256_hex(b"x")
                ),
            ),
        ] {
            let mut files = HashMap::new();
            files.insert(format!("/vibration/{INDEX_FILE}"), text.into_bytes());
            let src = source(serve(files), tag);
            assert!(get_it(&src).is_err(), "{tag}");
            assert!(!src.dir.exists(), "{tag}");
            clean(&src);
        }
    }

    #[test]
    fn a_download_that_is_not_the_listed_one_is_thrown_away() {
        let mut other = usual();
        *other.last_mut().unwrap() ^= 1;
        let src = source(serve(release(&[usual()], Some(&other))), "swapped");
        let err = get_it(&src).unwrap_err();
        assert!(err.contains("does not match"), "{err}");
        assert!(installed(&src.dir).is_empty());
        clean(&src);
        let mut longer = usual();
        longer.extend_from_slice(b"and more");
        let src = source(serve(release(&[usual()], Some(&longer))), "long");
        assert!(get_it(&src).unwrap_err().contains("larger than expected"));
        assert!(installed(&src.dir).is_empty());
        clean(&src);
    }

    #[test]
    fn a_missing_release_is_plain() {
        let src = source(serve(HashMap::new()), "gone");
        assert_eq!(fetch(&src, &OFFERED[0]).unwrap_err(), NOT_PUBLISHED);
        clean(&src);
    }

    #[test]
    fn a_module_beside_the_program_needs_no_release_and_is_followed() {
        let src = source("http://127.0.0.1:9".into(), "beside");
        let beside = &src.beside[0];
        std::fs::create_dir_all(beside).unwrap();
        let built = beside.join(corvus_package::built_name(&OFFERED[0].package()));
        std::fs::write(&built, usual()).unwrap();
        // One that needs a newer program, and one of another plugin under this name.
        std::fs::write(
            beside.join("vibration-9.0.0-abc.wasm"),
            module(&manifest("9.0.0", "2099.01.01", &[])),
        )
        .unwrap();
        let mut other = manifest("9.0.0", NOW, &[]);
        other.id = "tak".into();
        std::fs::write(beside.join("vibration-9.0.1-abc.wasm"), module(&other)).unwrap();
        let found = scan_beside(&src);
        assert_eq!(found["vibration"].path, built);
        assert_eq!(get_it(&src).unwrap().1, "2.0.0");
        std::fs::write(&built, module(&manifest("2.5.0", NOW, &["telemetry.rates"]))).unwrap();
        follow_beside(&src);
        assert_eq!(installed(&src.dir)["vibration"].version, "2.5.0");
        clean(&src);
    }

    #[test]
    fn a_picked_file_is_installed_for_what_it_is() {
        let src = source("http://127.0.0.1:9".into(), "file");
        let dir = src.dir.parent().unwrap().to_path_buf();
        let path = dir.join("whatever.bin");
        std::fs::write(&path, usual()).unwrap();
        assert_eq!(install_file(&src, &path).unwrap(), ("vibration".into(), "2.0.0".into()));
        assert!(installed(&src.dir)["vibration"].origin.is_empty(), "not followed");
        for (bytes, want) in [
            (b"MZ a native library".to_vec(), "not a Corvus plugin"),
            (module(&manifest("2.0.0", "2099.01.01", &[])), "2099.01.01 or newer"),
        ] {
            std::fs::write(&path, bytes).unwrap();
            let why = install_file(&src, &path).unwrap_err();
            assert!(why.contains(want), "{why}");
        }
        let mut part = manifest("2.0.0", NOW, &[]);
        part.id = "ssh-launcher".into();
        std::fs::write(&path, module(&part)).unwrap();
        assert!(install_file(&src, &path).unwrap_err().contains("part of Corvus GCS"));
        clean(&src);
    }

    #[test]
    fn installing_compiles_the_module_and_one_that_does_not_compile_replaces_nothing() {
        let mut src = source("http://127.0.0.1:9".into(), "compile");
        src.sandbox = Some(testlib::sandbox());
        src.corvus = corvus_core::version::version().into();
        let dir = src.dir.parent().unwrap().to_path_buf();
        let good = dir.join("good.wasm");
        let mut m = manifest("2.0.0", "2026.11.01", &[]);
        std::fs::write(&good, module(&m)).unwrap();
        install_file(&src, &good).expect("a module of nothing but its manifest compiles");
        let before = installed(&src.dir)["vibration"].clone();
        m.version = "9.0.0".into();
        let mut broken = module(&m);
        broken.extend_from_slice(&[1, 200, 1, 9, 9]);
        let bad = dir.join("bad.wasm");
        std::fs::write(&bad, broken).unwrap();
        assert!(install_file(&src, &bad).is_err());
        assert_eq!(installed(&src.dir)["vibration"], before);
        clean(&src);
    }

    fn until(store: &mut Store, ctx: &egui::Context, what: &str, f: impl Fn(&Store) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            store.poll(ctx);
            if f(store) {
                return;
            }
            assert!(Instant::now() < end, "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn what_the_setup_asked_for_without_a_connection_is_downloaded_at_a_later_start() {
        let ctx = egui::Context::default();
        // Nothing listens: the first start after the update, in the field.
        let offline = source("http://127.0.0.1:9".into(), "later");
        let mut store = Store::with_source(Some(offline.clone()));
        store.want(&ctx);
        until(&mut store, &ctx, "the list", |s| !s.busy());
        store.install_when_possible("vibration");
        until(&mut store, &ctx, "the try", |s| !s.busy());
        assert!(installed(&offline.dir).is_empty());
        assert_eq!(store.waiting_for_download(), ["vibration"]);
        let (said, ok) = store.message().expect("the operator is told");
        assert!(!ok && said.contains("tried again at the next start"), "{said}");

        // The next start, with a connection.
        let mut online = offline.clone();
        online.base = Some(serve(release(&[usual()], None)));
        let mut store = Store::with_source(Some(online.clone()));
        assert_eq!(store.waiting_for_download(), ["vibration"]);
        store.resume();
        until(&mut store, &ctx, "the download", |s| !s.busy());
        assert_eq!(installed(&online.dir)["vibration"].version, "2.0.0");
        assert!(store.waiting_for_download().is_empty());
        assert!(!online.dir.join(WANTED).exists());
        clean(&online);
    }

    #[test]
    fn what_waits_gets_no_more_than_was_shown_when_it_was_asked_for() {
        let ctx = egui::Context::default();
        let greedy = module(&manifest("2.0.0", NOW, &["telemetry.rates", "local"]));
        let mut src = source("http://127.0.0.1:9".into(), "waits-more");
        let mut store = Store::with_source(Some(src.clone()));
        store.install_when_possible("vibration");
        until(&mut store, &ctx, "the try", |s| !s.busy());
        src.base = Some(serve(release(&[greedy], None)));
        let mut store = Store::with_source(Some(src.clone()));
        store.resume();
        until(&mut store, &ctx, "the second try", |s| !s.busy());
        assert!(installed(&src.dir).is_empty());
        clean(&src);
    }

    #[test]
    fn removing_a_plugin_takes_it_off_the_list_of_what_waits() {
        let src = source("http://127.0.0.1:9".into(), "cancel");
        let mut store = Store::with_source(Some(src.clone()));
        store.install_when_possible("vibration");
        store.remove("vibration");
        assert!(store.waiting_for_download().is_empty());
        assert!(!src.dir.join(WANTED).exists());
        clean(&src);
    }

    #[test]
    fn an_id_that_leaves_the_folder_is_not_removed_or_waited_for() {
        let dir = testlib::empty("evil-id");
        assert!(remove(&dir, "../etc").is_err());
        std::fs::write(dir.join(WANTED), r#"{"../x": [], "tak": ["tak", 3]}"#).unwrap();
        let wanted = read_wanted(&dir);
        assert_eq!(wanted.keys().collect::<Vec<_>>(), ["tak"]);
        assert_eq!(wanted["tak"], ["tak"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn notes_nobody_takes_do_not_pile_up() {
        let mut store = Store::with_source(None);
        for i in 0..NOTES_MAX * 3 {
            store.say(format!("{i}"), true);
        }
        assert_eq!(store.take_notes().len(), NOTES_MAX);
    }

    #[test]
    fn the_list_is_complete_without_a_release() {
        let src = source("http://127.0.0.1:9".into(), "complete");
        let store = Store::with_source(Some(src.clone()));
        let rows = store.rows();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].offer.is_none() && rows[0].installed.is_none());
        assert_eq!(rows[0].permissions, seen(), "what it may do is known without a network");
        clean(&src);
    }

    #[test]
    fn the_folders_beside_the_program() {
        let got = beside(
            Path::new("/opt/Corvus GCS.app/Contents/MacOS/corvus-gcs"),
            Some(Path::new("/home/p/Apps/Corvus.AppImage")),
        );
        assert_eq!(
            got,
            [
                "/opt/Corvus GCS.app/Contents/MacOS",
                "/opt/Corvus GCS.app/Contents/MacOS/corvus-plugins",
                "/opt/corvus-plugins",
                "/home/p/Apps/corvus-plugins",
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn every_plugin_of_the_repository_is_on_the_list_with_what_it_may_do() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins");
        let mut folders: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|e| e.path().join("Cargo.toml").is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        folders.sort();
        let listed: Vec<&str> = crate::plugins::OFFERED.iter().map(|o| o.id).collect();
        assert_eq!(folders, listed);
        for o in crate::plugins::OFFERED {
            for p in o.permissions {
                assert!(
                    crate::plugins::wasm::permissions::of(p).is_some(),
                    "{}: there is no permission {p}",
                    o.id
                );
            }
        }
    }
}
