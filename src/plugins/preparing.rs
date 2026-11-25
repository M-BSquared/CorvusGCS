//! The installed plugins that have to be compiled before they can start: after
//! an update of the program that changes what compiles them, the compiled forms
//! kept beside them are stale. Compiling all of them takes seconds, on a slow
//! field laptop far longer, so a thread does it while the window runs, and each
//! plugin starts as soon as it is ready.
//!
//! One thread compiles one module after the other (the compiler itself uses
//! every core). It is joined when this is dropped, after the module it is at:
//! a compilation cannot be cut short.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;

use eframe::egui;

use super::loader::{Prepared, Refused};
use super::wasm::Sandbox;

/// What one package came to.
pub type Ready = Result<Prepared, Refused>;

const LOST: &str = "it could not be prepared. It is tried again at the next start";

pub struct Preparing {
    ready: Receiver<Ready>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// The ids that have not come back yet.
    left: Vec<String>,
}

fn lost(id: &str) -> Refused {
    Refused {
        id: id.to_string(),
        why: LOST.into(),
        needs_newer: false,
    }
}

impl Preparing {
    /// Compiles `waiting` on a thread of its own and has `ctx` repainted after each.
    pub fn start(sandbox: Arc<Sandbox>, ctx: &egui::Context, waiting: Vec<Prepared>) -> Preparing {
        let left = waiting.iter().map(|p| p.id().to_string()).collect();
        let (done, ready) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let (asked_to_stop, ctx) = (stop.clone(), ctx.clone());
        let thread = std::thread::Builder::new()
            .name("plugin-compiler".into())
            .spawn(move || {
                for mut prepared in waiting {
                    if asked_to_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let id = prepared.id().to_string();
                    // A compiler that panics takes one plugin with it, not the rest.
                    let result = match crate::crash::guarded(|| prepared.compile(&sandbox)) {
                        Ok(Ok(())) => Ok(prepared),
                        Ok(Err(refused)) => Err(refused),
                        Err(_) => Err(lost(&id)),
                    };
                    if done.send(result).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .inspect_err(|e| eprintln!("plugins: no thread to compile them: {e}"))
            .ok();
        Preparing {
            ready,
            stop,
            thread,
            left,
        }
    }

    /// What has become ready since the last call.
    pub fn take(&mut self) -> Vec<Ready> {
        let mut out = Vec::new();
        loop {
            match self.ready.try_recv() {
                Ok(r) => {
                    let id = match &r {
                        Ok(p) => p.id(),
                        Err(refused) => &refused.id,
                    };
                    self.left.retain(|l| l != id);
                    out.push(r);
                }
                Err(TryRecvError::Empty) => break,
                // The thread is gone: nothing else will come.
                Err(TryRecvError::Disconnected) => {
                    out.extend(self.left.drain(..).map(|id| Err(lost(&id))));
                    break;
                }
            }
        }
        out
    }

    /// Whether plugin `id` is still to come.
    pub fn is_waiting_for(&self, id: &str) -> bool {
        self.left.iter().any(|l| l == id)
    }

    /// Plugin `id` got here another way, or is not wanted any more.
    pub fn forget(&mut self, id: &str) {
        self.left.retain(|l| l != id);
    }

    pub fn done(&self) -> bool {
        self.left.is_empty()
    }
}

impl Drop for Preparing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use serde_json::json;

    use super::super::wasm::{runtime, testlib};
    use super::super::{loader, Host, Info, Plugin, Registry};
    use crate::net::http::Server;

    /// A part of the program whose card belongs after the probe's module here.
    struct Last;

    impl Plugin for Last {
        fn info(&self) -> Info {
            Info {
                id: "last",
                name: "Last",
                icon: "puzzle",
                description: "",
                version: "1.0.0",
                order: 500,
                can_tab: false,
            }
        }
        fn show(&mut self, _ui: &mut eframe::egui::Ui, _area: eframe::egui::Rect, _h: &mut Host) -> f32 {
            0.0
        }
    }

    fn compiled_in(folder: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(folder)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == runtime::COMPILED))
            .collect()
    }

    fn registry(dir: &Path, ctx: &eframe::egui::Context) -> Registry {
        // Nothing listens there: the registry goes on without saved settings.
        Registry::with_packages(
            &testlib::sandbox(),
            Server::local(9),
            ctx,
            &json!({}),
            vec![Box::new(Last)],
            dir,
        )
    }

    fn ids(r: &Registry, order: &[usize]) -> Vec<&'static str> {
        order.iter().map(|&i| r.info(i).id).collect()
    }

    #[test]
    fn a_plugin_that_must_be_compiled_does_not_hold_up_the_start() {
        let dir = testlib::empty("preparing");
        let folder = testlib::package(&dir, "probe", "corvus_plugin_probe");
        let ctx = eframe::egui::Context::default();
        let began = Instant::now();
        let mut r = registry(&dir, &ctx);
        let waited = began.elapsed();
        assert!(r.preparing() && r.is_preparing("probe"));
        assert_eq!(r.index_of("probe"), None);
        assert!(
            compiled_in(&folder).is_empty() || waited.as_millis() < 200,
            "{waited:?}"
        );
        // The settings that came while it was still being compiled are its own all the same.
        r.backend = Some(json!({"settings": {"probe": {"kept": 7}}}));
        r.finish_preparing();
        assert!(!r.preparing() && !r.is_preparing("probe"));
        assert_eq!(r.refused, []);
        let probe = r.index_of("probe").expect("it is there now");
        assert_eq!(r.entries[probe].saved, json!({"kept": 7}));
        assert!(r.package_of("probe").is_some());
        // Appended, so every index that was handed out still means the same plugin, and shown
        // where its order puts it.
        assert_eq!(ids(&r, &(0..r.len()).collect::<Vec<_>>()), ["last", "probe"]);
        assert_eq!(ids(&r, &r.by_place()), ["probe", "last"]);
        assert_eq!(compiled_in(&folder).len(), 1);
        drop(r);

        // The next start takes what was kept and has the plugin at once.
        let r = registry(&dir, &ctx);
        assert!(!r.preparing());
        assert!(r.index_of("probe").is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_damaged_compiled_module_is_made_again_off_the_window() {
        let dir = testlib::empty("preparing-damaged");
        let folder = testlib::package(&dir, "probe", "corvus_plugin_probe");
        assert_eq!(loader::installed(&testlib::sandbox(), &dir, &[]).refused, []);
        let kept = compiled_in(&folder).pop().expect("kept");
        std::fs::write(&kept, b"cut short").unwrap();
        let found = loader::scan(&testlib::sandbox(), &dir, &[]);
        assert!(found.packages.is_empty() && found.refused.is_empty());
        assert_eq!(found.waiting.len(), 1);
        assert_eq!(std::fs::read(&kept).unwrap(), b"cut short", "the scan compiles nothing");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_plugin_removed_while_it_was_compiled_does_not_come() {
        let dir = testlib::empty("preparing-removed");
        testlib::package(&dir, "probe", "corvus_plugin_probe");
        let ctx = eframe::egui::Context::default();
        let mut r = registry(&dir, &ctx);
        super::super::store::remove(&dir, "probe").unwrap();
        r.finish_preparing();
        assert_eq!(r.index_of("probe"), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn closing_waits_for_the_module_at_hand_and_starts_no_other() {
        let dir = testlib::empty("preparing-closed");
        testlib::package(&dir, "probe", "corvus_plugin_probe");
        let second = testlib::package(&dir, "vibration", "corvus_plugin_vibration");
        let ctx = eframe::egui::Context::default();
        let mut r = registry(&dir, &ctx);
        assert!(r.is_preparing("probe") && r.is_preparing("vibration"));
        r.shut_down(&ctx, &json!({}));
        assert!(!r.preparing());
        assert!(compiled_in(&second).is_empty(), "the second was not begun");
        // The thread is joined: nothing writes there any more.
        std::fs::remove_dir_all(dir).unwrap();
    }
}
