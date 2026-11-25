//! The plugin store: the plugins this release of Corvus GCS offers, and
//! installing and removing them.
//!
//! Which plugins there are is known from the source this program was built from ([`Offered`]), so
//! the list is complete without a network. The libraries come from one of three places:
//!
//! * Beside the program: the folder `corvus-plugins` next to it, or the folder a build left it in
//!   (`./run.sh`). Needs no network, and is taken first.
//! * The plugin's own repository on GitHub: its release for the running version of the program
//!   (`corvus-<version>`), whose `plugins-index.json` (see `corvus_package`) names the library for
//!   each system. Read over HTTPS when the operator opens the page and the plugin is not there
//!   beside the program. A library is written only after its size and SHA-256 match the list.
//!   There is no signing key: what is trusted is the plugin's repository on GitHub.
//! * A file the operator picks, for a computer without internet.
//!
//! Wherever it comes from, only a library with the stamp of this program is taken. It goes to the
//! folder the loader reads at start (`loader::packages_dir`). The window's own computer is where
//! it installs: with `--server` the backend may be elsewhere, the library is loaded here.
//!
//! A library is stored under a name made of its checksum and the manifest that names it is
//! replaced in one rename, so a crash, a full disk or a lost connection leaves the old plugin or
//! the new one and never half of one, and a library that is in use (Windows locks it) is not
//! touched. What is left over the loader clears away at a later start.
//!
//! A plugin that is installed is loaded at once (`Registry::poll_store`). One that is
//! updated or removed keeps running until the next start, because a loaded library is
//! never unloaded, and the page says so.

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use corvus_core::plugins::http_client;
use corvus_package::{Entry, Index, INDEX_FILE};
use eframe::egui;
use serde_json::json;

use super::loader::{self, Manifest};
use crate::net::bg::Pending;

const INDEX_CAP: usize = 1024 * 1024;
const LIBRARY_CAP: usize = 96 * 1024 * 1024;
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// For a whole library over a slow link. A server that falls silent is given up on much earlier.
const LIBRARY_TIMEOUT: Duration = Duration::from_secs(1800);
const NOTES_MAX: usize = 32;
const UNREACHED: &str = "could not reach the server";

/// The folder beside the program that libraries are taken from.
pub const BESIDE: &str = "corvus-plugins";

/// A plugin of this source that is installed by the operator (`OFFERED`, made by `build.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offered {
    pub id: &'static str,
    pub package: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub version: &'static str,
    /// Its repository on GitHub, whose releases it is installed from.
    pub repository: &'static str,
}

/// What a library of crate `package` is: its id and version.
fn of_package(package: &str, offered: &[Offered]) -> (String, String) {
    match offered.iter().find(|o| o.package == package) {
        Some(o) => (o.id.to_string(), o.version.to_string()),
        None => (
            package.trim_start_matches("corvus_plugin_").replace('_', "-"),
            String::new(),
        ),
    }
}

/// Where the plugins come from and go to.
#[derive(Clone)]
pub struct Source {
    /// The version of the program, which the releases are named after.
    pub version: String,
    /// Releases from somewhere else: a folder per plugin there.
    pub base: Option<String>,
    /// What a library must have been built from.
    pub abi: String,
    pub platform: String,
    pub dir: PathBuf,
    /// The folders beside the program, in the order they are looked through.
    pub beside: Vec<PathBuf>,
    /// The plugins there are.
    pub offered: &'static [Offered],
}

impl Source {
    /// The real one for this program, or `None` where there is no folder to install to.
    pub fn real() -> Option<Source> {
        #[cfg_attr(not(debug_assertions), allow(unused_mut))]
        let mut src = Source {
            version: corvus_core::version::version().to_string(),
            base: None,
            abi: corvus_api::plugin::dynamic::ABI.to_string(),
            platform: corvus_package::platform(),
            dir: loader::packages_dir()?,
            beside: std::env::current_exe()
                .map(|exe| beside(&exe, std::env::var_os("APPIMAGE").map(PathBuf::from).as_deref()))
                .unwrap_or_default(),
            offered: super::OFFERED,
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

/// The folders beside the program `exe`: the one it is in, where a build leaves the libraries,
/// and `corvus-plugins` beside it, beside its folder, beside the `.app` it is in and beside the
/// AppImage it was started from.
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

/// A library on this computer that this program can load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub path: PathBuf,
    pub sha256: String,
}

/// The library of `o` beside the program, if there is one of this build. It has the name cargo
/// gave it or the one it is published under, which ends in a part of its checksum.
fn find_beside(src: &Source, o: &Offered) -> Option<Local> {
    let built = corvus_package::built_name(o.package);
    for dir in &src.beside {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| *n == built || corvus_package::is_published_name(n, o.id, &src.platform))
            .collect();
        names.sort();
        for path in names.iter().map(|n| dir.join(n)) {
            let Ok(bytes) = read_library(&path) else { continue };
            let ours = corvus_package::abi_of(&bytes) == Some(src.abi.as_str())
                && corvus_package::crate_of(&bytes) == Some(o.package);
            if ours {
                return Some(Local {
                    sha256: corvus_package::sha256_hex(&bytes),
                    path,
                });
            }
        }
    }
    None
}

/// Every plugin whose library lies beside the program.
pub fn scan_beside(src: &Source) -> BTreeMap<String, Local> {
    src.offered
        .iter()
        .filter_map(|o| find_beside(src, o).map(|l| (o.id.to_string(), l)))
        .collect()
}

fn read_library(path: &Path) -> Result<Vec<u8>, String> {
    let cannot = |e: std::io::Error| format!("the file cannot be read: {e}");
    let file = std::fs::File::open(path).map_err(cannot)?;
    let meta = file.metadata().map_err(cannot)?;
    if !meta.is_file() {
        return Err("it is not a file".into());
    }
    let mut bytes = Vec::new();
    file.take(LIBRARY_CAP as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(cannot)?;
    if bytes.len() > LIBRARY_CAP {
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
        return Err("there is nothing published for this version of Corvus GCS".into());
    }
    if reply.status != 200 {
        return Err(format!("the server answered HTTP {}", reply.status));
    }
    if reply.truncated || reply.body.len() > cap {
        return Err("the answer was larger than expected".into());
    }
    Ok(reply.body)
}

/// The folder the files of plugin `o` are in, for this version of the program.
fn release_of(src: &Source, o: &Offered) -> String {
    match &src.base {
        Some(base) => format!("{base}/{}", o.id),
        None => format!("{}/releases/download/corvus-{}", o.repository, src.version),
    }
}

/// What the repository of plugin `o` offers for this program and this computer.
pub fn fetch(src: &Source, o: &Offered) -> Result<Entry, String> {
    let list = get(
        &format!("{}/{INDEX_FILE}", release_of(src, o)),
        INDEX_CAP,
        LIST_TIMEOUT,
        &mut |_, _| (),
    )?;
    let index = Index::parse(&list)?;
    if index.abi != src.abi {
        return Err("the published plugins were built for another build of Corvus GCS".into());
    }
    index
        .plugins
        .into_iter()
        .find(|e| e.id == o.id && e.file_for(&src.platform).is_some())
        .ok_or_else(|| "this plugin is not offered for this computer".into())
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

/// Installs plugin `id`: the library beside the program, else the one of the release.
pub fn install(src: &Source, id: &str, meter: &Meter) -> Result<Done, String> {
    let o = src.offered.iter().find(|o| o.id == id);
    if let Some(l) = o.and_then(|o| find_beside(src, o)) {
        meter.set(Step::Installing);
        return install_file(src, &l.path);
    }
    let o = o.ok_or("this plugin is not offered for this computer")?;
    let entry = fetch(src, o)?;
    let file = entry
        .file_for(&src.platform)
        .ok_or("this plugin is not offered for this computer")?;
    let cap = usize::try_from(file.size).unwrap_or(LIBRARY_CAP).min(LIBRARY_CAP);
    let total = file.size;
    let url = format!("{}/{}", release_of(src, o), file.file);
    let download = || {
        meter.set(Step::Downloading { got: 0, total });
        get(&url, cap, LIBRARY_TIMEOUT, &mut |got, _| {
            meter.set(Step::Downloading { got: got as u64, total })
        })
    };
    // A link that broke once often holds the next time.
    let bytes = download().or_else(|e| if e.starts_with(UNREACHED) { download() } else { Err(e) })?;
    meter.set(Step::Installing);
    if bytes.len() as u64 != file.size || corvus_package::sha256_hex(&bytes) != file.sha256 {
        return Err("the download does not match the list and was thrown away".into());
    }
    write_package(src, id, &entry.name, &entry.version, &bytes, None)?;
    Ok((id.to_string(), entry.version.clone()))
}

/// Installs the plugin library `path`, whatever its name is: what it is stands in it.
pub fn install_file(src: &Source, path: &Path) -> Result<Done, String> {
    // Before the bytes: a file that changes meanwhile is looked at again at the next start.
    let stamp = file_stamp(path);
    let bytes = read_library(path)?;
    let abi = corvus_package::abi_of(&bytes).ok_or("the file is not a Corvus plugin for this computer")?;
    let package = corvus_package::crate_of(&bytes).ok_or("the file is not a Corvus plugin for this computer")?;
    if abi != src.abi {
        return Err("the file was built for another version of Corvus GCS".into());
    }
    let (id, version) = of_package(package, src.offered);
    if !corvus_package::is_valid_id(&id) {
        return Err("the file is not a Corvus plugin for this computer".into());
    }
    let name = src.offered.iter().find(|o| o.id == id).map_or(id.as_str(), |o| o.name);
    // Only what lies beside the program is followed when it changes, not a file from anywhere.
    let origin = path
        .parent()
        .filter(|p| src.beside.iter().any(|b| b == p))
        .map(|_| (path, stamp.as_str()));
    write_package(src, &id, name, &version, &bytes, origin)?;
    Ok((id, version))
}

fn write_package(
    src: &Source,
    id: &str,
    name: &str,
    version: &str,
    bytes: &[u8],
    origin: Option<(&Path, &str)>,
) -> Result<(), String> {
    let folder = src.dir.join(id);
    let sha256 = corvus_package::sha256_hex(bytes);
    let library = format!("{}.{}", &sha256[..16], corvus_package::library_extension());
    let mut manifest = json!({"id": id, "name": name, "version": version, "library": library, "sha256": sha256});
    if let Some((path, stamp)) = origin {
        manifest["origin"] = json!(path.to_string_lossy());
        manifest["origin_stamp"] = json!(stamp);
    }
    let manifest = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let written = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&folder)?;
        // The same library may be there already, and loaded.
        let have = corvus_package::sha256_file(&folder.join(&library)).ok();
        if have.as_deref() != Some(sha256.as_str()) {
            put(&folder, &library, bytes)?;
        }
        put(&folder, loader::MANIFEST, &manifest)?;
        // Whoever installs it again wants it tried again.
        match std::fs::remove_file(folder.join(loader::TRIAL)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    })();
    written.map_err(|e| format!("cannot write the plugin to {}: {e}", folder.display()))
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

/// Removes plugin `id`: without its manifest it is not loaded any more. Its library goes too where
/// the system lets it, and at a later start where it does not.
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
        let name = e.file_name().to_string_lossy().into_owned();
        if name == loader::TRIAL || name.ends_with(corvus_package::library_extension()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let _ = std::fs::remove_dir(&folder);
    Ok(())
}

/// The installed packages, as their manifests say.
pub fn installed(dir: &Path) -> BTreeMap<String, Manifest> {
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

/// Where a library on offer is.
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
    /// Where it can be had, and the checksum of its library; `None` when it cannot be had now.
    pub offer: Option<(From, String)>,
    /// What is installed, when something is.
    pub installed: Option<Manifest>,
}

impl Row {
    /// Another library than the installed one is on offer. Its version may well be the same: a
    /// plugin is built again for a new version of the program.
    pub fn update_available(&self) -> bool {
        match (&self.installed, &self.offer) {
            (Some(m), Some((_, sha256))) => m.sha256 != *sha256,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Act {
    Install(String),
    File(PathBuf),
    Remove(String),
}

impl Act {
    /// What it is about, for a message.
    fn subject(&self) -> String {
        match self {
            Act::Install(id) | Act::Remove(id) => id.clone(),
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
    have: BTreeMap<String, Manifest>,
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
    pub fn new() -> Store {
        Store::with_source(Source::real())
    }

    pub fn with_source(source: Option<Source>) -> Store {
        Store {
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
                    (Ok(done), _) => self.done.push_back(done),
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
                    Act::Install(id) => install(&src, &id, &m),
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
            .any(|a| matches!(a, Act::Install(x) | Act::Remove(x) if x == id))
    }

    /// What is being done for plugin `id`.
    pub fn doing(&self, id: &str) -> Option<Doing> {
        let about = |a: &Act| matches!(a, Act::Install(x) | Act::Remove(x) if x == id);
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
                let entry = self.entries.as_ref().and_then(|e| e.get(o.id)?.as_ref().ok());
                let beside = self.beside.as_ref().and_then(|b| b.get(o.id));
                let offer = match (beside, entry.and_then(|e| e.file_for(&src.platform))) {
                    (Some(l), _) => Some((From::Beside, l.sha256.clone())),
                    (None, Some(f)) => Some((From::Release, f.sha256.clone())),
                    (None, None) => None,
                };
                let published = entry.filter(|_| beside.is_none());
                Row {
                    id: o.id.into(),
                    name: o.name.into(),
                    description: o.description.into(),
                    version: published.map_or_else(|| o.version.to_string(), |e| e.version.clone()),
                    offer,
                    installed: self.have.get(o.id).cloned(),
                }
            })
            .collect();
        // Installed from a file and not on any list: it can be removed here.
        for (id, m) in &self.have {
            if !rows.iter().any(|r| r.id == *id) {
                rows.push(Row {
                    id: id.clone(),
                    name: id.clone(),
                    description: String::new(),
                    version: m.version.clone(),
                    offer: None,
                    installed: Some(m.clone()),
                });
            }
        }
        rows.sort_by_key(|r| r.name.to_lowercase());
        rows
    }

    /// Installs, updates or removes plugin `id`; one thing at a time, in the order asked.
    pub fn install(&mut self, id: &str) {
        if !self.working_on(id) {
            self.queue(Act::Install(id.to_string()));
        }
    }

    pub fn remove(&mut self, id: &str) {
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
        Store::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::HashMap;
    use std::io::Read;
    use std::net::TcpListener;

    const LIB: &[u8] = b"pretend this is a library";

    const OFFERED: &[Offered] = &[Offered {
        id: "vibration",
        package: "corvus_plugin_vibration",
        name: "Vibration Monitor",
        description: "d",
        version: "1.0.0",
        repository: "https://github.com/M-BSquared/corvus-vibration",
    }];

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

    fn release(lib: &[u8], list_text: Option<&str>) -> (HashMap<String, Vec<u8>>, String) {
        let platform = corvus_package::platform();
        let mut files = std::collections::BTreeMap::new();
        files.insert(
            platform.clone(),
            corvus_package::File {
                file: "vibration.lib".into(),
                sha256: corvus_package::sha256_hex(LIB),
                size: LIB.len() as u64,
            },
        );
        let index = Index {
            abi: "abi-1".into(),
            plugins: vec![Entry {
                id: "vibration".into(),
                name: "Vibration Monitor".into(),
                description: "d".into(),
                icon: "activity".into(),
                version: "1.0.0".into(),
                files,
            }],
        };
        let list = list_text.map_or_else(|| index.to_bytes(), |t| t.as_bytes().to_vec());
        let mut served = HashMap::new();
        served.insert(format!("/vibration/{INDEX_FILE}"), list);
        served.insert("/vibration/vibration.lib".to_string(), lib.to_vec());
        (served, platform)
    }

    fn source(base: String, platform: String, tag: &str) -> Source {
        let dir = std::env::temp_dir().join(format!("corvus-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Source {
            version: "2026.11.90".into(),
            base: Some(base),
            abi: "abi-1".into(),
            platform,
            beside: vec![dir.join("beside")],
            dir: dir.join("packages"),
            offered: OFFERED,
        }
    }

    #[test]
    fn a_listed_plugin_is_installed_found_and_removed() {
        let (files, platform) = release(LIB, None);
        let src = source(serve(files), platform, "ok");
        assert_eq!(
            install(&src, "vibration", &Meter::default()),
            Ok(("vibration".into(), "1.0.0".into()))
        );
        let folder = src.dir.join("vibration");
        let manifest: Value = serde_json::from_slice(&std::fs::read(folder.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["sha256"], corvus_package::sha256_hex(LIB));
        let library = folder.join(manifest["library"].as_str().unwrap());
        assert_eq!(std::fs::read(&library).unwrap(), LIB);
        assert_eq!(installed(&src.dir)["vibration"].version, "1.0.0");
        let files = || std::fs::read_dir(&folder).unwrap().count();
        assert_eq!(files(), 2, "nothing half written stays");
        std::fs::write(folder.join(loader::TRIAL), b"").unwrap();
        install(&src, "vibration", &Meter::default()).expect("installing it again");
        assert_eq!(
            files(),
            2,
            "the same library is not stored twice, and it is tried again"
        );
        remove(&src.dir, "vibration").unwrap();
        assert!(installed(&src.dir).is_empty());
        assert!(!folder.exists());
        let _ = std::fs::remove_dir_all(src.dir.parent().unwrap());
    }

    #[test]
    fn a_list_that_is_not_a_list_installs_nothing() {
        for (tag, text) in [
            ("junk", "not json"),
            ("newer", r#"{"format": 99, "abi": "abi-1", "plugins": []}"#),
        ] {
            let (files, platform) = release(LIB, Some(text));
            let src = source(serve(files), platform, tag);
            assert!(install(&src, "vibration", &Meter::default()).is_err(), "{tag}");
            assert!(!src.dir.exists(), "{tag}");
        }
    }

    #[test]
    fn an_entry_that_could_leave_its_folder_is_refused() {
        let hash = corvus_package::sha256_hex(LIB);
        let text = format!(
            r#"{{"format": 1, "abi": "abi-1", "plugins": [{{"id": "vibration", "files": {{"{}": {{"file": "../evil.lib", "sha256": "{hash}", "size": 25}}}}}}]}}"#,
            corvus_package::platform()
        );
        let (files, platform) = release(LIB, Some(&text));
        let src = source(serve(files), platform, "evil");
        assert!(install(&src, "vibration", &Meter::default())
            .unwrap_err()
            .contains("not valid"));
        assert!(!src.dir.exists());
    }

    #[test]
    fn a_library_that_is_not_the_listed_one_is_thrown_away() {
        let (files, platform) = release(b"PRETEND THIS IS A LIBRARY", None);
        assert_eq!(LIB.len(), 25);
        let src = source(serve(files), platform, "lib");
        let err = install(&src, "vibration", &Meter::default()).unwrap_err();
        assert!(err.contains("does not match"), "{err}");
        assert!(installed(&src.dir).is_empty());
        let (files, platform) = release(b"pretend this is another, longer library", None);
        let src = source(serve(files), platform, "long");
        assert!(install(&src, "vibration", &Meter::default())
            .unwrap_err()
            .contains("larger than expected"));
        assert!(installed(&src.dir).is_empty());
    }

    #[test]
    fn a_list_for_another_build_is_refused() {
        let (files, platform) = release(LIB, None);
        let mut src = source(serve(files), platform, "abi");
        src.abi = "abi-2".into();
        assert!(fetch(&src, &OFFERED[0]).unwrap_err().contains("another build"));
    }

    #[test]
    fn a_missing_release_is_plain() {
        let (files, platform) = release(LIB, None);
        let mut src = source(serve(files), platform, "gone");
        src.base = src.base.map(|b| b + "/missing");
        assert!(fetch(&src, &OFFERED[0]).unwrap_err().contains("nothing published"));
    }

    #[test]
    fn an_id_that_leaves_the_folder_is_not_removed() {
        let dir = std::env::temp_dir();
        assert!(remove(&dir, "../etc").is_err());
    }

    #[test]
    fn a_library_built_again_is_an_update_though_its_version_is_the_same() {
        let (files, platform) = release(LIB, None);
        let src = source(serve(files), platform, "update");
        let folder = src.dir.join("vibration");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join(loader::MANIFEST),
            r#"{"id": "vibration", "version": "1.0.0", "library": "old.lib", "sha256": "0badc0de"}"#,
        )
        .unwrap();
        let ctx = egui::Context::default();
        let mut store = Store::with_source(Some(src.clone()));
        store.want(&ctx);
        let settle = |store: &mut Store| {
            for _ in 0..500 {
                store.poll(&ctx);
                if !store.busy() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("the store did not finish");
        };
        settle(&mut store);
        assert!(store.rows()[0].update_available());
        store.install("vibration");
        store.install("vibration");
        settle(&mut store);
        assert_eq!(store.take_installed(), Some(("vibration".into(), "1.0.0".into())));
        assert_eq!(store.take_installed(), None, "pressed twice is done once");
        assert!(!store.rows()[0].update_available());
        store.remove("vibration");
        settle(&mut store);
        assert_eq!(store.rows()[0].installed, None);
        let _ = std::fs::remove_dir_all(src.dir.parent().unwrap());
    }

    /// Serves `files`, the library in two halves with a wait for `go` between them, and only
    /// half of it the first `broken` times it is asked for.
    fn serve_slowly(files: HashMap<String, Vec<u8>>, broken: usize, go: std::sync::mpsc::Receiver<()>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut broken = broken;
            for mut conn in listener.incoming().flatten() {
                let mut got = Vec::new();
                let mut buf = [0u8; 2048];
                while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                    match conn.read(&mut buf) {
                        Ok(n) if n > 0 => got.extend_from_slice(&buf[..n]),
                        _ => break,
                    }
                }
                let text = String::from_utf8_lossy(&got).to_string();
                let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                let Some(body) = files.get(&path) else {
                    let _ = conn.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    continue;
                };
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                let _ = conn.write_all(head.as_bytes());
                if !path.ends_with(".lib") {
                    let _ = conn.write_all(body);
                    continue;
                }
                let (a, b) = body.split_at(body.len() / 2);
                let _ = conn.write_all(a);
                if broken > 0 {
                    broken -= 1;
                    continue;
                }
                let _ = go.recv_timeout(Duration::from_secs(10));
                let _ = conn.write_all(b);
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    fn until(store: &mut Store, ctx: &egui::Context, what: &str, f: impl Fn(&Store) -> bool) {
        for _ in 0..1000 {
            store.poll(ctx);
            if f(store) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("never: {what}");
    }

    #[test]
    fn a_download_says_how_far_it_is_and_a_broken_one_is_tried_again() {
        let (files, platform) = release(LIB, None);
        let (go, gate) = std::sync::mpsc::channel();
        let src = source(serve_slowly(files, 1, gate), platform, "meter");
        let ctx = egui::Context::default();
        let mut store = Store::with_source(Some(src.clone()));
        assert_eq!((store.progress(), store.doing("vibration")), (None, None));
        store.install("vibration");
        store.install_file(src.dir.join("not-there.lib"));
        assert_eq!(store.doing("vibration"), Some(Doing::Waiting));
        let half = Step::Downloading { got: 12, total: 25 };
        until(&mut store, &ctx, "half of the library", |s| {
            s.progress().is_some_and(|p| p.step == half)
        });
        assert_eq!(store.doing("vibration"), Some(Doing::Installing(half)));
        let p = store.progress().unwrap();
        assert_eq!((p.name.as_str(), p.nth, p.of), ("Vibration Monitor", 1, 2));
        assert_eq!(p.step.fraction(), Some(0.48));
        assert_eq!(p.step.words(), "0.0 MB of 0.0 MB \u{b7} 48%");
        go.send(()).unwrap();
        until(&mut store, &ctx, "the end", |s| !s.busy());
        assert_eq!(store.take_installed(), Some(("vibration".into(), "1.0.0".into())));
        assert_eq!((store.progress(), store.doing("vibration")), (None, None));
        let _ = std::fs::remove_dir_all(src.dir.parent().unwrap());
    }

    #[test]
    fn a_failure_holds_up_no_other_install_and_stays_in_sight() {
        let (files, platform) = release(LIB, None);
        let src = source(serve(files), platform, "goes-on");
        let ctx = egui::Context::default();
        let mut store = Store::with_source(Some(src.clone()));
        store.install("nothing-of-that-name");
        store.install("vibration");
        until(&mut store, &ctx, "the end", |s| !s.busy());
        assert_eq!(store.take_installed(), Some(("vibration".into(), "1.0.0".into())));
        store.say("vibration is ready.".into(), true);
        assert!(store.message().is_some_and(|(_, ok)| !ok), "{:?}", store.message());
        let notes = store.take_notes();
        assert_eq!(notes.iter().map(|n| n.1).collect::<Vec<_>>(), [false, true]);
        assert!(store.take_notes().is_empty());
        store.remove("vibration");
        assert_eq!(store.message(), None, "what is asked for next starts afresh");
        until(&mut store, &ctx, "the removal", |s| !s.busy());
        let _ = std::fs::remove_dir_all(src.dir.parent().unwrap());
    }

    #[test]
    fn notes_nobody_takes_do_not_pile_up() {
        let mut store = Store::with_source(None);
        for i in 0..1000 {
            store.say(format!("{i}"), true);
        }
        assert_eq!(store.take_notes().len(), NOTES_MAX);
    }

    fn library(abi: &str, package: &str, rest: &str) -> Vec<u8> {
        format!("\0CORVUS-PLUGIN\0{abi}\0CORVUS-CRATE\0{package}\0{rest}").into_bytes()
    }

    fn local(tag: &str) -> Source {
        let src = source("http://127.0.0.1:9".into(), corvus_package::platform(), tag);
        std::fs::create_dir_all(&src.beside[0]).unwrap();
        src
    }

    #[test]
    fn a_library_beside_the_program_needs_no_release() {
        for published in [false, true] {
            let src = local("beside");
            let name = if published {
                corvus_package::published_name("vibration", &src.platform, &"ab".repeat(32))
            } else {
                corvus_package::built_name("corvus_plugin_vibration")
            };
            let lib = library("abi-1", "corvus_plugin_vibration", "");
            std::fs::write(src.beside[0].join(name), &lib).unwrap();
            assert_eq!(
                install(&src, "vibration", &Meter::default()),
                Ok(("vibration".into(), "1.0.0".into()))
            );
            assert_eq!(
                installed(&src.dir)["vibration"].sha256,
                corvus_package::sha256_hex(&lib)
            );
            let _ = std::fs::remove_dir_all(src.dir.parent().unwrap());
        }
    }

    #[test]
    fn a_library_of_another_build_or_another_plugin_is_not_offered() {
        let src = local("other");
        let path = src.beside[0].join(corvus_package::built_name("corvus_plugin_vibration"));
        for lib in [
            library("abi-2", "corvus_plugin_vibration", ""),
            library("abi-1", "corvus_plugin_tak", ""),
            b"junk".to_vec(),
            Vec::new(),
        ] {
            std::fs::write(&path, lib).unwrap();
            assert!(scan_beside(&src).is_empty());
        }
        let _ = std::fs::remove_dir_all(src.dir.parent().unwrap());
    }

    #[test]
    fn a_picked_file_is_installed_for_what_it_is() {
        let src = local("file");
        let path = src.dir.parent().unwrap().join("download (1).bin");
        std::fs::write(&path, library("abi-1", "corvus_plugin_vibration", "")).unwrap();
        assert_eq!(install_file(&src, &path), Ok(("vibration".into(), "1.0.0".into())));
        assert_eq!(
            installed(&src.dir)["vibration"].origin,
            "",
            "a file from anywhere is not followed"
        );
        std::fs::write(&path, library("abi-2", "corvus_plugin_vibration", "")).unwrap();
        assert!(install_file(&src, &path).unwrap_err().contains("another version"));
        std::fs::write(&path, library("abi-1", "../../evil", "")).unwrap();
        assert!(install_file(&src, &path).unwrap_err().contains("not a Corvus plugin"));
        std::fs::write(&path, b"a picture").unwrap();
        assert!(install_file(&src, &path).unwrap_err().contains("not a Corvus plugin"));
        assert!(install_file(&src, &src.beside[0]).is_err());
        assert_eq!(installed(&src.dir).len(), 1);
        let _ = std::fs::remove_dir_all(src.dir.parent().unwrap());
    }

    #[test]
    fn what_was_installed_from_beside_the_program_follows_it() {
        let src = local("follow");
        let path = src.beside[0].join(corvus_package::built_name("corvus_plugin_vibration"));
        std::fs::write(&path, library("abi-1", "corvus_plugin_vibration", "old")).unwrap();
        install(&src, "vibration", &Meter::default()).unwrap();
        let sha = |src: &Source| installed(&src.dir)["vibration"].sha256.clone();
        let first = sha(&src);
        follow_beside(&src);
        assert_eq!(sha(&src), first);
        let newer = library("abi-1", "corvus_plugin_vibration", "built again");
        std::fs::write(&path, &newer).unwrap();
        follow_beside(&src);
        assert_eq!(sha(&src), corvus_package::sha256_hex(&newer));
        // One of another build does not replace what is there, and neither does a missing one.
        std::fs::write(&path, library("abi-2", "corvus_plugin_vibration", "later")).unwrap();
        follow_beside(&src);
        assert_eq!(sha(&src), corvus_package::sha256_hex(&newer));
        std::fs::remove_file(&path).unwrap();
        follow_beside(&src);
        assert_eq!(sha(&src), corvus_package::sha256_hex(&newer));
        // Another program, which does not lie beside it, leaves it alone.
        std::fs::write(&path, library("abi-1", "corvus_plugin_vibration", "third")).unwrap();
        let mut elsewhere = src.clone();
        elsewhere.beside.clear();
        follow_beside(&elsewhere);
        assert_eq!(sha(&src), corvus_package::sha256_hex(&newer));
        let _ = std::fs::remove_dir_all(src.dir.parent().unwrap());
    }

    #[test]
    fn the_list_is_complete_without_a_release() {
        let src = local("offline");
        let ctx = egui::Context::default();
        let mut store = Store::with_source(Some(src.clone()));
        store.want(&ctx);
        for _ in 0..500 {
            store.poll(&ctx);
            if !store.busy() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(store.problem().is_some());
        let rows = store.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].name.as_str(), &rows[0].offer), ("Vibration Monitor", &None));
        let _ = std::fs::remove_dir_all(src.dir.parent().unwrap());
    }

    #[test]
    fn the_folders_beside_the_program() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(
            beside(&p("/work/target/debug/corvus-gcs"), None),
            [
                p("/work/target/debug"),
                p("/work/target/debug/corvus-plugins"),
                p("/work/target/corvus-plugins")
            ]
        );
        assert!(
            beside(&p("/Applications/Corvus GCS.app/Contents/MacOS/corvus-gcs"), None)
                .contains(&p("/Applications/corvus-plugins"))
        );
        assert!(beside(
            &p("/tmp/.mount/usr/bin/corvus-gcs-ui"),
            Some(&p("/home/x/Corvus.AppImage"))
        )
        .contains(&p("/home/x/corvus-plugins")));
    }

    #[test]
    fn a_plugin_is_asked_for_in_its_own_repository() {
        let mut src = source(String::new(), "linux-x86_64".into(), "own");
        src.base = None;
        assert_eq!(
            release_of(&src, &OFFERED[0]),
            "https://github.com/M-BSquared/corvus-vibration/releases/download/corvus-2026.11.90"
        );
        for o in crate::plugins::OFFERED {
            let want = format!("https://github.com/M-BSquared/corvus-{}", o.id);
            assert_eq!(o.repository, want, "{}", o.id);
        }
    }

    #[test]
    fn one_repository_that_does_not_answer_holds_up_no_other() {
        let (files, platform) = release(LIB, None);
        let src = source(serve(files), platform, "some");
        let other = Offered {
            id: "tak",
            ..OFFERED[0]
        };
        let got = fetch_all(&src, &[other, OFFERED[0]]);
        assert!(got["tak"].as_ref().unwrap_err().contains("nothing published"));
        assert_eq!(got["vibration"].as_ref().unwrap().version, "1.0.0");
    }

    #[test]
    fn every_plugin_of_the_repository_is_on_the_list() {
        let ids: Vec<&str> = crate::plugins::OFFERED.iter().map(|o| o.id).collect();
        for id in [
            "ai-chat",
            "map-entities",
            "nuttx-console",
            "remote-id-check",
            "schwalby",
            "ssh-launcher",
            "tak",
            "trajectory-viewer",
            "vibration",
        ] {
            assert!(ids.contains(&id), "{id}");
        }
        for o in crate::plugins::OFFERED {
            assert!(
                !o.name.is_empty() && !o.description.is_empty() && !o.version.is_empty(),
                "{}",
                o.id
            );
        }
    }
}
