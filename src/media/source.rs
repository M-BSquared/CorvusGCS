//! What a media card reads: which files it can show, a picture decoded off the
//! window's thread, and a video that ffmpeg turns into JPEG frames on a pipe,
//! as it does for the cameras. ffmpeg stays a child process; it is ended and
//! reaped when its player is dropped.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use corvus_core::peripherals::video::ffmpeg::{self, JpegSplitter, MAX_FRAME_BYTES};
use corvus_core::peripherals::video::stream;
use eframe::egui;

use crate::net::lock::Relock;

/// The largest picture file that is read.
const IMAGE_BYTES_MAX: u64 = 200 * 1024 * 1024;
/// The most pixels along one side that a picture is decoded from.
const IMAGE_SIDE_MAX: u32 = 30_000;
const IMAGE_ALLOC_MAX: u64 = 1024 * 1024 * 1024;
/// The frame rate a video is played at, which is also how its position is counted.
const FPS: u32 = 25;
const VIDEO_WIDTH: u32 = 1280;
/// How long ffmpeg is given to end by itself before it is killed.
const TERM: Duration = Duration::from_millis(500);
/// The end of what ffmpeg said, kept for the reason it failed.
const SAID_MAX: usize = 4096;

pub const NO_FFMPEG: &str =
    "ffmpeg is not installed. Install it to play videos here, or open the file with the system's program.";
pub const NOT_THERE: &str = "This file is not there.";
pub const NOT_ABSOLUTE: &str = "The path must be a full one, from the root of the disk or from your home folder (~).";
pub const NETWORK: &str = "Network addresses are not opened here. Copy the file to this computer.";
pub const TOO_LARGE: &str = "This picture is too large to show.";
pub const UNREADABLE: &str = "This file cannot be shown. It may be damaged or of a kind Corvus does not read.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A picture this program decodes itself.
    Picture,
    /// A picture ffmpeg decodes for it.
    OtherPicture,
    /// A video, or a moving picture.
    Video,
    /// Anything else: the card says what it is and offers the folder.
    Other,
}

pub fn kind_of(path: &Path) -> Kind {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" => Kind::Picture,
        "bmp" | "tif" | "tiff" | "heic" | "heif" => Kind::OtherPicture,
        "mp4" | "mov" | "m4v" | "mkv" | "avi" | "webm" | "ts" | "mts" | "m2ts" | "mpg" | "mpeg" | "3gp" | "gif" => {
            Kind::Video
        }
        _ => Kind::Other,
    }
}

/// Whether the system's own program may be asked to open the file. Only kinds
/// that are looked at or read: never anything that could run.
pub fn system_may_open(path: &Path) -> bool {
    let plain = path.extension().is_some_and(|e| {
        let e = e.to_string_lossy().to_ascii_lowercase();
        matches!(e.as_str(), "pdf" | "txt" | "log" | "csv" | "json")
    });
    plain || kind_of(path) != Kind::Other
}

/// The file `text` names, or why it is not opened.
pub fn local_file(text: &str) -> Result<PathBuf, &'static str> {
    let text = text.trim().trim_matches('"');
    let path = PathBuf::from(corvus_api::ui::filepicker::expand_home(text));
    // A UNC path would reach out over the network, with this user's login.
    if text.starts_with("\\\\") || text.starts_with("//") {
        return Err(NETWORK);
    }
    if !path.is_absolute() {
        return Err(NOT_ABSOLUTE);
    }
    if !path.is_file() {
        return Err(NOT_THERE);
    }
    Ok(path)
}

/// A picture no larger than `side` along either edge, the way its camera held it.
pub fn picture(path: &Path, side: u32) -> Result<egui::ColorImage, &'static str> {
    use image::ImageDecoder;
    let meta = std::fs::metadata(path).map_err(|_| NOT_THERE)?;
    if meta.len() > IMAGE_BYTES_MAX {
        return Err(TOO_LARGE);
    }
    let mut reader = image::ImageReader::open(path)
        .map_err(|_| NOT_THERE)?
        .with_guessed_format()
        .map_err(|_| UNREADABLE)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(IMAGE_SIDE_MAX);
    limits.max_image_height = Some(IMAGE_SIDE_MAX);
    limits.max_alloc = Some(IMAGE_ALLOC_MAX);
    reader.limits(limits);
    let said = |e: image::ImageError| match e {
        image::ImageError::Limits(_) => TOO_LARGE,
        _ => UNREADABLE,
    };
    let mut decoder = reader.into_decoder().map_err(said)?;
    // Phones and drone cameras store the turn beside the picture.
    let turn = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(said)?;
    img.apply_orientation(turn);
    let side = side.max(64);
    if img.width() > side || img.height() > side {
        img = img.thumbnail(side, side);
    }
    Ok(color_image(&img.to_rgba8()))
}

fn color_image(rgba: &image::RgbaImage) -> egui::ColorImage {
    let size = [rgba.width() as usize, rgba.height() as usize];
    egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
}

/// `Duration: 00:01:02.34` of what ffmpeg says about a file, in seconds.
pub fn duration_of(said: &str) -> Option<f64> {
    let rest = &said[said.find("Duration:")? + "Duration:".len()..];
    let clock = rest.split(',').next()?.trim();
    let mut parts = clock.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    let total = h * 3600.0 + m * 60.0 + s;
    (total.is_finite() && total > 0.0).then_some(total)
}

/// What a player has made so far.
#[derive(Default)]
pub struct Played {
    /// The newest picture, until the window takes it.
    pub frame: Option<egui::ColorImage>,
    /// How many pictures came since the start.
    pub frames: u64,
    pub duration: Option<f64>,
    /// ffmpeg is done: the end of the file, or `error`.
    pub ended: bool,
    pub error: Option<&'static str>,
}

/// How a player starts.
#[derive(Debug, Clone, Copy, Default)]
pub struct Start {
    /// Seconds into the file.
    pub at: f64,
    /// One picture only: what is there, while paused.
    pub still: bool,
    pub duration: Option<f64>,
}

/// ffmpeg reading one file from `at`, until the end or until dropped.
pub struct Player {
    pub shared: Arc<Mutex<Played>>,
    pub start: Start,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    thread: Option<JoinHandle<()>>,
}

/// Where the ffmpeg of this computer is, asked when a player starts.
pub type Find = Box<dyn FnOnce() -> String + Send>;

impl Player {
    pub fn start(ctx: &egui::Context, path: &Path, find: Find, start: Start) -> Player {
        let shared = Arc::new(Mutex::new(Played {
            duration: start.duration,
            ..Played::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let thread = {
            let (ctx, path) = (ctx.clone(), path.to_path_buf());
            let (shared, stop, child) = (shared.clone(), stop.clone(), child.clone());
            std::thread::Builder::new()
                .name("media-player".into())
                .spawn(move || {
                    let result = play(&ctx, &path, &find(), start, &shared, &stop, &child);
                    let mut p = shared.relock();
                    p.error = result.err();
                    p.ended = true;
                    drop(p);
                    ctx.request_repaint();
                })
                .ok()
        };
        Player {
            shared,
            start,
            stop,
            child,
            thread,
        }
    }

    /// Seconds into the file of the picture on show.
    pub fn position(&self) -> f64 {
        let frames = self.shared.relock().frames;
        self.start.at + frames.saturating_sub(1) as f64 / f64::from(FPS)
    }

    /// Tells ffmpeg to end without waiting for it.
    fn ask_to_end(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(c) = self.child.relock().as_mut() {
            stream::ask_to_end(c);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.ask_to_end();
        // The thread goes on draining the pipe, so ffmpeg is not stuck writing to it.
        let end = Instant::now() + TERM;
        while self.thread.as_ref().is_some_and(|t| !t.is_finished()) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(2));
        }
        if self.thread.as_ref().is_some_and(|t| !t.is_finished()) {
            if let Some(c) = self.child.relock().as_mut() {
                let _ = c.kill();
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Runs one ffmpeg: what it writes goes to `out`, and the end of what it says comes back
/// with whether it ended well. The child is where `stop` can reach it, and is reaped.
fn run(
    ffmpeg: &str,
    args: &[String],
    stop: &AtomicBool,
    child: &Mutex<Option<Child>>,
    mut out: impl FnMut(&[u8]),
) -> Result<(bool, String), &'static str> {
    let mut command = ffmpeg::command(ffmpeg);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut spawned = command.spawn().map_err(|_| NO_FFMPEG)?;
    let (stdout, stderr) = (spawned.stdout.take(), spawned.stderr.take());
    {
        let mut slot = child.relock();
        if stop.load(Ordering::SeqCst) {
            let _ = spawned.kill();
        }
        *slot = Some(spawned);
    }
    // Read beside the pictures: a full pipe would hold ffmpeg.
    let said = std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buf = [0u8; 4096];
        if let Some(mut err) = stderr {
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    break;
                }
                kept.extend_from_slice(&buf[..n]);
                // The head holds the duration, the tail the reason: keep both.
                if kept.len() > 4 * SAID_MAX {
                    kept.drain(SAID_MAX..kept.len() - SAID_MAX);
                }
            }
        }
        String::from_utf8_lossy(&kept).into_owned()
    });
    if let Some(mut pipe) = stdout {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = pipe.read(&mut buf) {
            if n == 0 {
                break;
            }
            out(&buf[..n]);
        }
    }
    let good = loop {
        match child.relock().as_mut().map(Child::try_wait) {
            Some(Ok(None)) => {}
            Some(Ok(Some(status))) => break status.success(),
            _ => break false,
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    *child.relock() = None;
    Ok((good, said.join().unwrap_or_default()))
}

/// The file as ffmpeg is to open it: from the disk and nothing else, whatever the
/// file itself may name.
fn input(path: &Path) -> [String; 4] {
    [
        "-protocol_whitelist".into(),
        "file".into(),
        "-i".into(),
        format!("file:{}", path.display()),
    ]
}

fn play(
    ctx: &egui::Context,
    path: &Path,
    ffmpeg: &str,
    start: Start,
    shared: &Mutex<Played>,
    stop: &AtomicBool,
    child: &Mutex<Option<Child>>,
) -> Result<(), &'static str> {
    if ffmpeg.is_empty() {
        return Err(NO_FFMPEG);
    }
    let words = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    if start.duration.is_none() && !start.still {
        let mut args = words(&["-hide_banner", "-nostdin"]);
        args.extend(input(path));
        // Without an output ffmpeg only says what the file is, and "fails".
        let (_, said) = run(ffmpeg, &args, stop, child, |_| {})?;
        shared.relock().duration = duration_of(&said);
    }
    if stop.load(Ordering::SeqCst) {
        return Ok(());
    }
    let mut args = words(&["-hide_banner", "-nostdin", "-loglevel", "error"]);
    if start.at > 0.0 {
        args.extend(["-ss".into(), format!("{:.3}", start.at)]);
    }
    if !start.still {
        // At the speed it was recorded.
        args.push("-re".into());
    }
    args.extend(input(path));
    args.extend(words(&["-an", "-sn", "-dn", "-vf"]));
    let scale = format!("scale='min({VIDEO_WIDTH},iw)':-2");
    if start.still {
        args.extend([scale, "-frames:v".into(), "1".into()]);
    } else {
        args.push(format!("fps={FPS},{scale}"));
    }
    args.extend(words(&["-f", "mjpeg", "-q:v", "4", "pipe:1"]));
    let mut cut = JpegSplitter::new(MAX_FRAME_BYTES);
    let (good, _) = run(ffmpeg, &args, stop, child, |bytes| {
        let frames = cut.feed(bytes);
        // Behind by several: only the newest is worth decoding.
        let count = frames.len() as u64;
        let Some(newest) = frames.last() else { return };
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(img) = image::load_from_memory(newest) else {
            return;
        };
        let img = color_image(&img.to_rgba8());
        let mut p = shared.relock();
        p.frame = Some(img);
        p.frames += count;
        drop(p);
        ctx.request_repaint();
    })?;
    let nothing_came = shared.relock().frames == 0;
    if !stop.load(Ordering::SeqCst) && nothing_came && (!good || start.at == 0.0) {
        return Err(UNREADABLE);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_of_a_file_says_what_it_is() {
        for (name, kind) in [
            ("a.JPG", Kind::Picture),
            ("a.webp", Kind::Picture),
            ("a.tif", Kind::OtherPicture),
            ("DJI_0001.MP4", Kind::Video),
            ("a.gif", Kind::Video),
            ("a.pdf", Kind::Other),
            ("a", Kind::Other),
        ] {
            assert_eq!(kind_of(Path::new(name)), kind, "{name}");
        }
    }

    #[test]
    fn the_system_is_never_asked_to_open_what_could_run() {
        for ok in ["a.mp4", "a.png", "a.pdf", "a.csv"] {
            assert!(system_may_open(Path::new(ok)), "{ok}");
        }
        for no in [
            "a.sh",
            "a.command",
            "a.bat",
            "a.exe",
            "a.app",
            "a.desktop",
            "a.lnk",
            "a",
            "a.mp4.exe",
        ] {
            assert!(!system_may_open(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn only_a_file_on_this_computer_is_opened() {
        assert_eq!(local_file("photo.jpg"), Err(NOT_ABSOLUTE));
        assert_eq!(local_file("\\\\server\\share\\a.jpg"), Err(NETWORK));
        assert_eq!(local_file("//server/share/a.jpg"), Err(NETWORK));
        assert_eq!(local_file("http://example.org/a.jpg"), Err(NOT_ABSOLUTE));
        let dir = std::env::temp_dir();
        assert_eq!(local_file(&dir.to_string_lossy()), Err(NOT_THERE));
        let file = dir.join(format!("corvus-media-{}.txt", std::process::id()));
        std::fs::write(&file, "x").unwrap();
        assert_eq!(local_file(&format!(" \"{}\" ", file.display())), Ok(file.clone()));
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn a_picture_is_read_and_made_to_fit() {
        let file = std::env::temp_dir().join(format!("corvus-media-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(400, 200, image::Rgba([10, 200, 30, 255]))
            .save(&file)
            .unwrap();
        assert_eq!(picture(&file, 4096).unwrap().size, [400, 200]);
        assert_eq!(picture(&file, 100).unwrap().size, [100, 50]);
        std::fs::write(&file, b"not a picture").unwrap();
        assert_eq!(picture(&file, 100), Err(UNREADABLE));
        std::fs::remove_file(&file).unwrap();
        assert_eq!(picture(&file, 100), Err(NOT_THERE));
    }

    #[test]
    fn the_length_is_read_from_what_ffmpeg_says() {
        assert_eq!(
            duration_of("  Duration: 00:01:02.50, start: 0.0, bitrate: 1 kb/s"),
            Some(62.5)
        );
        assert_eq!(duration_of("Duration: 01:00:00.00"), Some(3600.0));
        assert_eq!(duration_of("Duration: N/A, bitrate: N/A"), None);
        assert_eq!(duration_of("nothing"), None);
    }

    fn wait(player: &Player, what: &str, done: impl Fn(&Played) -> bool) {
        let end = Instant::now() + Duration::from_secs(20);
        while !done(&player.shared.relock()) {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn without_ffmpeg_a_video_says_so() {
        let player = Player::start(
            &egui::Context::default(),
            Path::new("/no/such/video.mp4"),
            Box::new(String::new),
            Start::default(),
        );
        wait(&player, "the end", |p| p.ended);
        assert_eq!(player.shared.relock().error, Some(NO_FFMPEG));
    }

    /// With the ffmpeg of the computer the tests run on; says so where there is none.
    #[test]
    fn a_video_is_played_to_its_end_and_a_still_is_one_picture() {
        let (found, _) = ffmpeg::resolve_ffmpeg("");
        if found.is_empty() {
            eprintln!("no ffmpeg here: playing a video is not tested");
            return;
        }
        let file = std::env::temp_dir().join(format!("corvus-media-{} clip.mp4", std::process::id()));
        let made = ffmpeg::command(&found)
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=25:duration=1",
            ])
            .arg(&file)
            .status()
            .unwrap();
        assert!(made.success(), "ffmpeg did not make the test video");
        let ctx = egui::Context::default();
        let find = |path: String| -> Find { Box::new(move || path) };
        let player = Player::start(&ctx, &file, find(found.clone()), Start::default());
        wait(&player, "the end", |p| p.ended);
        {
            let mut p = player.shared.relock();
            assert_eq!(p.error, None);
            assert_eq!(p.frame.take().map(|f| f.size), Some([320, 240]));
            assert!(p.frames >= 20, "{} frames", p.frames);
            assert!(p.duration.is_some_and(|d| (d - 1.0).abs() < 0.1), "{:?}", p.duration);
        }
        assert!((player.position() - 1.0).abs() < 0.2, "{}", player.position());

        let still = Start {
            at: 0.5,
            still: true,
            duration: Some(1.0),
        };
        let player = Player::start(&ctx, &file, find(found.clone()), still);
        wait(&player, "the still", |p| p.ended);
        assert_eq!(player.shared.relock().frames, 1);
        assert_eq!(player.position(), 0.5);

        // Dropped in the middle: ffmpeg is gone and the thread joined, at once.
        let player = Player::start(&ctx, &file, find(found.clone()), Start::default());
        wait(&player, "a first picture", |p| p.frames > 0);
        let began = Instant::now();
        drop(player);
        assert!(began.elapsed() < Duration::from_secs(2), "{:?}", began.elapsed());

        std::fs::write(&file, b"not a video").unwrap();
        let player = Player::start(&ctx, &file, find(found), Start::default());
        wait(&player, "the end", |p| p.ended);
        assert_eq!(player.shared.relock().error, Some(UNREADABLE));
        std::fs::remove_file(file).unwrap();
    }
}
