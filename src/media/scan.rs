//! Reading a folder of pictures and videos the operator picked: for each file,
//! where and when it was taken. A thread does it, since a memory card holds
//! hundreds of files; it is stopped and joined when dropped.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use corvus_api::plugin::{FolderScan, Placed};
use eframe::egui;

use super::exif::{self, Taken};
use super::source::{self, Kind};
use crate::net::lock::Relock;

/// Folders below the picked one that are looked into (`DCIM/100MEDIA`).
const DEPTH_MAX: usize = 4;
/// Pictures and videos that are read from one folder.
const FILES_MAX: usize = 5000;
/// Entries of all folders together that are looked at.
const ENTRIES_MAX: usize = 50_000;
/// Files with a place that are handed on.
pub const PLACED_MAX: usize = 500;

pub const NOT_A_FOLDER: &str = "The folder cannot be read.";

/// The pictures and videos in `folder` and below it, by their names in order, and
/// whether some were left out. Links and hidden entries are passed over.
fn media_in(folder: &Path, stop: &AtomicBool) -> Result<(Vec<PathBuf>, bool), &'static str> {
    let mut found = Vec::new();
    let mut seen = 0usize;
    let mut open = vec![(folder.to_path_buf(), 0usize)];
    let mut cut = false;
    std::fs::read_dir(folder).map_err(|_| NOT_A_FOLDER)?;
    while let Some((dir, depth)) = open.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen > ENTRIES_MAX || stop.load(Ordering::SeqCst) {
                cut = true;
                open.clear();
                break;
            }
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            // The type of the entry itself: a link is neither.
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() && depth < DEPTH_MAX {
                open.push((entry.path(), depth + 1));
            } else if kind.is_file() && source::kind_of(&entry.path()) != Kind::Other {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    if found.len() > FILES_MAX {
        found.truncate(FILES_MAX);
        cut = true;
    }
    Ok((found, cut))
}

/// Reads every file of `folder`. `ffmpeg` is the one to ask about videos, or
/// empty: they are then counted as unread.
pub fn folder(folder: &Path, ffmpeg: &str, stop: &AtomicBool, child: &Mutex<Option<Child>>) -> FolderScan {
    let (files, cut) = match media_in(folder, stop) {
        Ok(found) => found,
        Err(why) => {
            return FolderScan {
                error: Some(why.into()),
                ..FolderScan::default()
            }
        }
    };
    let mut scan = FolderScan {
        cut,
        ..FolderScan::default()
    };
    for path in files {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if scan.files.len() >= PLACED_MAX {
            scan.cut = true;
            break;
        }
        let taken = match source::kind_of(&path) {
            Kind::Video => {
                if ffmpeg.is_empty() {
                    scan.videos_unread += 1;
                    continue;
                }
                source::says(ffmpeg, &path, stop, child)
                    .map(|said| exif::of_video(&said))
                    .unwrap_or_default()
            }
            Kind::Picture | Kind::OtherPicture => exif::of_picture_file(&path).unwrap_or_default(),
            Kind::Other => Taken::default(),
        };
        let (Some((lat, lng)), Some(text)) = (taken.place, path.to_str()) else {
            scan.without_place += 1;
            continue;
        };
        scan.files.push(Placed {
            path: text.to_string(),
            lat,
            lng,
            alt: taken.alt,
            taken: taken.time.unwrap_or_default(),
        });
    }
    scan
}

/// A folder being read.
pub struct Scan {
    done: Arc<Mutex<Option<FolderScan>>>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    thread: Option<JoinHandle<()>>,
}

impl Scan {
    pub fn start(ctx: &egui::Context, picked: PathBuf, find: source::Find) -> Scan {
        let done = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let thread = {
            let (ctx, out, stop, child) = (ctx.clone(), done.clone(), stop.clone(), child.clone());
            std::thread::Builder::new()
                .name("media-folder".into())
                .spawn(move || {
                    let result =
                        crate::crash::guarded(|| folder(&picked, &find(), &stop, &child)).unwrap_or_else(|_| {
                            FolderScan {
                                error: Some(NOT_A_FOLDER.into()),
                                ..FolderScan::default()
                            }
                        });
                    *out.relock() = Some(result);
                    ctx.request_repaint();
                })
                .ok()
        };
        let scan = Scan {
            done,
            stop,
            child,
            thread,
        };
        if scan.thread.is_none() {
            *scan.done.relock() = Some(FolderScan {
                error: Some(NOT_A_FOLDER.into()),
                ..FolderScan::default()
            });
        }
        scan
    }

    /// The result, once.
    pub fn take(&self) -> Option<FolderScan> {
        self.done.relock().take()
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        source::end(&self.stop, &self.child, self.thread.take());
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::super::exif::tests::{block, jpeg};
    use super::*;

    fn card(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("corvus-scan-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("DCIM/100MEDIA")).unwrap();
        std::fs::create_dir_all(dir.join(".trash")).unwrap();
        let photo = jpeg(&block(
            true,
            ("N", [48, 4, 35040]),
            ("E", [11, 38, 34800]),
            5205,
            "2026:11:25 14:02:11",
        ));
        std::fs::write(dir.join("DCIM/100MEDIA/b.JPG"), &photo).unwrap();
        std::fs::write(dir.join("DCIM/100MEDIA/a.jpg"), &photo).unwrap();
        std::fs::write(dir.join("DCIM/100MEDIA/._a.jpg"), &photo).unwrap();
        std::fs::write(dir.join(".trash/gone.jpg"), &photo).unwrap();
        std::fs::write(dir.join("screenshot.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        dir
    }

    fn read(dir: &Path, ffmpeg: &str) -> FolderScan {
        folder(dir, ffmpeg, &AtomicBool::new(false), &Mutex::new(None))
    }

    #[test]
    fn a_card_is_read_through_its_folders_and_what_has_no_place_is_counted() {
        let dir = card("read");
        std::fs::write(dir.join("clip.mp4"), b"x").unwrap();
        let scan = read(&dir, "");
        let names: Vec<&str> = scan.files.iter().map(|f| &f.path[f.path.len() - 5..]).collect();
        assert_eq!(names, ["a.jpg", "b.JPG"], "{scan:?}");
        assert_eq!((scan.without_place, scan.videos_unread, scan.cut), (1, 1, false));
        assert_eq!(scan.files[0].taken, "2026-11-25 14:02:11");
        assert_eq!(scan.files[0].alt, Some(520.5));
        assert_eq!(
            read(&dir.join("no such folder"), "").error.as_deref(),
            Some(NOT_A_FOLDER)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_not_followed() {
        let dir = card("link");
        std::os::unix::fs::symlink(&dir, dir.join("DCIM/around")).unwrap();
        assert_eq!(read(&dir, "").files.len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// With the ffmpeg of the computer the tests run on.
    #[test]
    fn a_video_tells_where_it_was_taken() {
        use corvus_core::peripherals::video::ffmpeg;
        let (found, _) = ffmpeg::resolve_ffmpeg("");
        if found.is_empty() {
            eprintln!("no ffmpeg here: reading a video's place is not tested");
            return;
        }
        let dir = card("video");
        let made = ffmpeg::command(&found)
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x48:rate=5:duration=1",
            ])
            .args(["-metadata", "location=+48.0764+011.6430+520.500/"])
            .args(["-metadata", "creation_time=2026-11-25T13:02:11Z"])
            .arg(dir.join("clip.mp4"))
            .status()
            .unwrap();
        assert!(made.success());
        let ctx = egui::Context::default();
        let running = Scan::start(&ctx, dir.clone(), Box::new(move || found));
        let end = Instant::now() + Duration::from_secs(20);
        let scan = loop {
            if let Some(scan) = running.take() {
                break scan;
            }
            assert!(Instant::now() < end, "the folder was not read");
            std::thread::sleep(Duration::from_millis(5));
        };
        let clip = scan
            .files
            .iter()
            .find(|f| f.path.ends_with("clip.mp4"))
            .unwrap_or_else(|| panic!("{scan:?}"));
        assert_eq!((clip.lat, clip.lng), (48.0764, 11.643));
        assert_eq!(clip.taken, "2026-11-25 13:02:11 UTC");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
