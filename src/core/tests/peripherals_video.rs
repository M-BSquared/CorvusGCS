//! The video code against what Python answered (`dump_peripherals_cases.py
//! video`), and the decoders and WHEP signalling against simulations: a fake
//! ffmpeg script and a loopback WHEP server. No camera, no real ffmpeg.

use corvus_core::peripherals::video::ffmpeg::{self, JpegSplitter};
use corvus_core::peripherals::video::model::*;
use corvus_core::peripherals::video::service::{auth_header, urljoin, whep_error};
use serde_json::{json, Value};

fn load() -> Value {
    let path = format!("{}/tests/data/peripherals_video_cases.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap()
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn addresses_are_split_shown_and_judged_as_python_does() {
    let d = load();
    for row in d["urlparts"].as_array().unwrap() {
        let url = text(&row["url"]);
        let (c, u, p) = split_credentials(url);
        assert_eq!(json!([c, u, p]), row["split"], "split {url:?}");
        assert_eq!(display_address(url), text(&row["display"]), "display {url:?}");
        for kind in KINDS {
            assert_eq!(url_problem(url, kind), text(&row["problem"][kind]), "{kind} {url:?}");
        }
    }
    for row in d["with_credentials"].as_array().unwrap() {
        assert_eq!(
            with_credentials(text(&row["url"]), text(&row["user"]), text(&row["password"])),
            text(&row["result"]),
            "{row}"
        );
    }
    for row in d["redact"].as_array().unwrap() {
        assert_eq!(
            redact(text(&row["text"]), text(&row["secret"])),
            text(&row["result"]),
            "{row}"
        );
    }
    for row in d["origins"].as_array().unwrap() {
        assert_eq!(same_origin(text(&row["a"]), text(&row["b"])), row["result"], "{row}");
    }
    for row in d["urljoin"].as_array().unwrap() {
        assert_eq!(
            urljoin(text(&row["base"]), text(&row["ref"])),
            text(&row["result"]),
            "{row}"
        );
    }
}

#[test]
fn what_an_operator_reads_is_the_pythons() {
    let d = load();
    for row in d["explain"].as_array().unwrap() {
        assert_eq!(explain_error(text(&row["line"])), text(&row["result"]), "{row}");
    }
    for row in d["whep_error"].as_array().unwrap() {
        let body = text(&row["body"]);
        assert_eq!(
            whep_error(row["code"].as_u64().unwrap() as u16, body.as_bytes()),
            text(&row["result"]),
            "{} {body:?}",
            row["code"]
        );
    }
    for row in d["auth"].as_array().unwrap() {
        assert_eq!(auth_header(&row["stream"]), text(&row["result"]), "{row}");
    }
}

fn mask(v: &Value, expected: &Value) -> Value {
    let mut v = v.clone();
    if expected["id"] == "<new>" {
        let id = v["id"].as_str().unwrap().to_string();
        assert!(
            id.len() == 9 && id.starts_with('v') && id[1..].chars().all(|c| c.is_ascii_hexdigit()),
            "{id}"
        );
        v["id"] = json!("<new>");
    }
    v
}

#[test]
fn the_camera_list_is_coerced_and_shown_as_python_does() {
    let d = load();
    for row in d["streams"].as_array().unwrap() {
        let got = coerce_stream(&row["raw"]);
        match (&got, &row["result"]) {
            (None, Value::Null) => {}
            (Some(g), expected) => {
                let g = mask(g, expected);
                assert_eq!(&g, expected, "{}", row["raw"]);
                let public = public_stream(&g);
                assert_eq!(public, row["public"], "{}", row["raw"]);
            }
            (None, expected) => panic!("dropped {} but Python kept {expected}", row["raw"]),
        }
    }
    for row in d["settings"].as_array().unwrap() {
        assert_eq!(settings(&row["raw"]), row["result"], "{}", row["raw"]);
    }
    let c = &d["constants"];
    assert_eq!(c["max_streams"], MAX_STREAMS);
    assert_eq!(c["max_name"], MAX_NAME_LEN);
    assert_eq!(c["max_url"], MAX_URL_LEN);
    assert_eq!(c["schemes"], json!(SCHEMES));
    assert_eq!(c["transports"], json!(TRANSPORTS));
    assert_eq!(c["kinds"], json!(KINDS));
    assert_eq!(c["max_frame"], ffmpeg::MAX_FRAME_BYTES);
    assert_eq!(c["max_width"], ffmpeg::MAX_WIDTH);
    assert_eq!(c["quality"], ffmpeg::JPEG_QUALITY);
    assert_eq!(c["socket_timeout"].as_f64(), Some(ffmpeg::SOCKET_TIMEOUT_S));
    let t = corvus_core::peripherals::video::Timing::default();
    assert_eq!(c["idle_stop"].as_f64(), Some(t.idle_stop.as_secs_f64()));
    assert_eq!(c["stall"].as_f64(), Some(t.stall.as_secs_f64()));
    assert_eq!(c["watchdog"].as_f64(), Some(t.watchdog.as_secs_f64()));
    assert_eq!(c["frame_wait"].as_f64(), Some(t.frame_wait.as_secs_f64()));
    assert_eq!(c["term"].as_f64(), Some(t.term_timeout.as_secs_f64()));
    assert_eq!(c["list_keep"].as_f64(), Some(t.list_keep.as_secs_f64()));
    assert_eq!(c["resolve_retry"].as_f64(), Some(t.resolve_retry.as_secs_f64()));
    assert_eq!(c["whep_timeout"].as_f64(), Some(t.whep_timeout.as_secs_f64()));
    assert_eq!(c["whep_close"].as_f64(), Some(t.whep_close_timeout.as_secs_f64()));
    let backoff: Vec<f64> = t.retry_backoff.iter().map(|d| d.as_secs_f64()).collect();
    assert_eq!(c["retry"], json!(backoff));
    assert_eq!(c["max_sdp"], corvus_core::peripherals::video::service::MAX_SDP_BYTES);
    assert_eq!(
        c["max_whep"],
        corvus_core::peripherals::video::service::MAX_WHEP_SESSIONS
    );
}

#[test]
fn the_ffmpeg_arguments_are_the_pythons() {
    let d = load();
    for row in d["versions"].as_array().unwrap() {
        let v = text(&row["version"]);
        assert_eq!(
            ffmpeg::ffmpeg_major(v).map(Value::from).unwrap_or(Value::Null),
            row["major"],
            "{v:?}"
        );
        assert_eq!(ffmpeg::hides_password(v), row["hides"], "{v:?}");
    }
    for row in d["input_options"].as_array().unwrap() {
        let got: Vec<Value> = ffmpeg::input_options(text(&row["url"]), text(&row["transport"]), text(&row["version"]))
            .into_iter()
            .map(|(k, v)| json!([k, v]))
            .collect();
        assert_eq!(Value::Array(got), row["result"], "{row}");
    }
    for row in d["concat_list"].as_array().unwrap() {
        assert_eq!(
            ffmpeg::concat_list(text(&row["url"]), text(&row["transport"]), text(&row["version"])),
            text(&row["result"]),
            "{row}"
        );
    }
    for row in d["command"].as_array().unwrap() {
        let got = ffmpeg::ffmpeg_command(
            "/usr/bin/ffmpeg",
            text(&row["url"]),
            text(&row["transport"]),
            text(&row["list_path"]),
            text(&row["version"]),
        );
        assert_eq!(json!(got), row["result"], "{row}");
    }
    for row in d["windows_paths"].as_array().unwrap() {
        let env = row["env"].clone();
        let got = ffmpeg::windows_ffmpeg_paths(&|k| env[k].as_str().unwrap_or("").to_string());
        assert_eq!(json!(got), row["result"], "{row}");
    }
}

#[test]
fn the_byte_stream_is_cut_into_images_as_python_does() {
    let d = load();
    for row in d["splitter"].as_array().unwrap() {
        let max = row["max"]
            .as_u64()
            .map(|m| m as usize)
            .unwrap_or(ffmpeg::MAX_FRAME_BYTES);
        let mut sp = JpegSplitter::new(max);
        let mut got = Vec::new();
        for chunk in row["chunks"].as_array().unwrap() {
            let frames: Vec<String> = sp.feed(&hex(text(chunk))).iter().map(|f| to_hex(f)).collect();
            got.push(frames);
        }
        assert_eq!(json!(got), row["frames"], "{row}");
    }
    for row in d["jpeg_size"].as_array().unwrap() {
        let got = ffmpeg::jpeg_size(&hex(text(&row["data"])));
        assert_eq!(
            got.map(|(w, h)| json!([w, h])).unwrap_or(Value::Null),
            row["result"],
            "{row}"
        );
    }
}

#[cfg(unix)]
mod simulated {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use corvus_core::peripherals::video::service::{Frame, FrameError, VideoService, WhepResult};
    use corvus_core::peripherals::video::Timing;
    use serde_json::{json, Value};

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("corvus-video-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A stand-in for ffmpeg: `-version` answers, anything else records its
    /// arguments, the address list it was given and its pid, then behaves as
    /// `body` says.
    fn fake_ffmpeg(dir: &Path, body: &str) -> String {
        let path = dir.join("ffmpeg");
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = "-hide_banner" ] && [ "$2" = "-version" ]; then echo "ffmpeg version 7.1 Copyright"; exit 0; fi
here="$(dirname "$0")"
echo "$$" >> "$here/pids.txt"
printf '%s\n' "$@" > "$here/args.txt"
prev=""
for a in "$@"; do if [ "$prev" = "-i" ]; then cp "$a" "$here/list.txt" 2>/dev/null; fi; prev="$a"; done
{body}
"#
        );
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The first run of a new script can take a while where the system
        // scans it (macOS); that is paid here, not inside the timings.
        let _ = std::process::Command::new(&path)
            .args(["-hide_banner", "-version"])
            .output();
        path.to_string_lossy().into_owned()
    }

    const FRAMES: &str = "trap 'exit 0' TERM\nwhile true; do printf '\\377\\330abc\\377\\331'; sleep 0.02; done";
    const SILENT: &str = "trap 'exit 0' TERM\nwhile true; do sleep 0.05; done";
    const DIES: &str = "echo '[tcp @ 0x7f1] Connection refused' >&2\necho 'Error opening input file x' >&2\nexit 1";
    const DEAF: &str = "trap '' TERM\nwhile true; do sleep 0.05; done";

    fn quick() -> Timing {
        Timing {
            idle_stop: Duration::from_millis(600),
            stall: Duration::from_millis(300),
            watchdog: Duration::from_millis(500),
            retry_backoff: vec![Duration::from_millis(40), Duration::from_millis(80)],
            frame_wait: Duration::from_millis(1500),
            term_timeout: Duration::from_millis(400),
            list_keep: Duration::from_millis(300),
            resolve_retry: Duration::from_millis(50),
            whep_timeout: Duration::from_secs(3),
            whep_close_timeout: Duration::from_secs(2),
            poll: Duration::from_millis(20),
        }
    }

    fn service(config: Value, ffmpeg: &str) -> Arc<VideoService> {
        let path = ffmpeg.to_string();
        VideoService::with(
            &config,
            quick(),
            Arc::new(move |configured: &str| {
                if !path.is_empty() && (configured.is_empty() || configured == path) {
                    (path.clone(), "environment".into())
                } else {
                    (String::new(), String::new())
                }
            }),
            Arc::new(|_| "7.1".to_string()),
        )
    }

    fn camera(id: &str) -> Value {
        json!({"streams": [{"id": id, "name": "Front", "url": "rtsp://10.9.8.7:8554/main.264",
                            "username": "pilot", "password": "s3cr3t-pw", "transport": "tcp"}], "ffmpeg": ""})
    }

    fn not_stopping() -> bool {
        false
    }

    fn until(what: &str, mut f: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(8);
        while Instant::now() < end {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}");
    }

    fn pid_alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc_kill(pid, 0) == 0 }
    }

    extern "C" {
        #[link_name = "kill"]
        fn libc_kill(pid: i32, sig: i32) -> i32;
    }

    fn pids(d: &Path) -> Vec<i32> {
        std::fs::read_to_string(d.join("pids.txt"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }

    #[test]
    fn a_camera_is_decoded_and_its_password_stays_off_the_command_line() {
        let d = dir("frames");
        let svc = service(camera("cam1"), &fake_ffmpeg(&d, FRAMES));
        let Ok(Frame::Jpeg { seq, data }) = svc.frame("cam1", 0, Duration::from_millis(1500), &not_stopping) else {
            panic!("no frame");
        };
        assert!(seq > 0 && data.starts_with(&[0xff, 0xd8]) && data.ends_with(&[0xff, 0xd9]));
        let Ok(Frame::Jpeg { seq: next, .. }) = svc.frame("cam1", seq, Duration::from_millis(1500), &not_stopping)
        else {
            panic!("no second frame");
        };
        assert!(next > seq, "a window asking again gets a newer frame");

        let args = std::fs::read_to_string(d.join("args.txt")).unwrap();
        assert!(!args.contains("s3cr3t-pw") && !args.contains("pilot"), "{args}");
        let list = std::fs::read_to_string(d.join("list.txt")).unwrap();
        assert!(list.contains("rtsp://pilot:s3cr3t-pw@10.9.8.7:8554/main.264"), "{list}");
        let status = svc.status();
        assert_eq!(status["available"], true);
        assert_eq!(status["ffmpeg"]["hides_password"], true);
        let cam = &status["streams"][0];
        assert_eq!(cam["state"], "live");
        assert_eq!(cam["has_password"], true);
        assert!(
            !status.to_string().contains("s3cr3t-pw"),
            "the status carries no password"
        );
        assert_eq!(cam["width"], 0, "the fake frames have no size header");

        // The address list is gone once ffmpeg has opened its input.
        let list_path = PathBuf::from(args.lines().skip_while(|l| *l != "-i").nth(1).expect("the list file"));
        let work = list_path.parent().unwrap().to_path_buf();
        until("the address list to be removed", || !list_path.exists());
        svc.shutdown();
        assert!(!svc.any_alive());
        assert!(!work.exists(), "the private folder is removed on shutdown");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn nobody_asking_ends_the_decoder() {
        let d = dir("idle");
        let svc = service(camera("cam1"), &fake_ffmpeg(&d, FRAMES));
        assert!(matches!(
            svc.frame("cam1", 0, Duration::from_millis(1500), &not_stopping),
            Ok(Frame::Jpeg { .. })
        ));
        let pid = pids(&d)[0];
        until("the idle stop", || !svc.any_alive());
        until("the process to be gone", || !pid_alive(pid));
        let status = svc.status();
        assert_eq!(status["streams"][0]["state"], "idle");
        assert_eq!(status["streams"][0]["running"], false);
        svc.shutdown();
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_camera_that_goes_silent_is_restarted_and_says_so() {
        let d = dir("silent");
        let svc = service(camera("cam1"), &fake_ffmpeg(&d, SILENT));
        let got = svc.frame("cam1", 0, Duration::from_millis(100), &not_stopping);
        assert!(matches!(got, Ok(Frame::State { .. })));
        until("a second start", || {
            let _ = svc.frame("cam1", 0, Duration::from_millis(50), &not_stopping);
            pids(&d).len() >= 2
        });
        let status = svc.status();
        let message = status["streams"][0]["message"].as_str().unwrap().to_string();
        assert!(
            message.starts_with("No picture after 0 s. Check the address")
                || status["streams"][0]["state"] == "starting",
            "{message}"
        );
        svc.shutdown();
        for p in pids(&d) {
            assert!(!pid_alive(p), "pid {p} outlived the shutdown");
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_failing_ffmpeg_is_explained_in_plain_words_and_retried() {
        let d = dir("dies");
        let svc = service(camera("cam1"), &fake_ffmpeg(&d, DIES));
        until("the error", || {
            let _ = svc.frame("cam1", 0, Duration::from_millis(50), &not_stopping);
            svc.status()["streams"][0]["state"] == "error"
        });

        let message = loop {
            let st = svc.status();
            if st["streams"][0]["state"] == "error" {
                break st["streams"][0]["message"].as_str().unwrap().to_string();
            }
            let _ = svc.frame("cam1", 0, Duration::from_millis(10), &not_stopping);
        };
        assert_eq!(
            message,
            "Nothing answered at that address and port. (Connection refused)"
        );
        until("a retry", || {
            let _ = svc.frame("cam1", 0, Duration::from_millis(50), &not_stopping);
            pids(&d).len() >= 2
        });
        svc.shutdown();
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn an_ffmpeg_that_ignores_sigterm_is_killed_on_shutdown() {
        let d = dir("deaf");
        let svc = service(camera("cam1"), &fake_ffmpeg(&d, DEAF));
        let _ = svc.frame("cam1", 0, Duration::from_millis(100), &not_stopping);
        until("the start", || !pids(&d).is_empty());
        let pid = pids(&d)[0];
        assert!(pid_alive(pid));
        let began = Instant::now();
        svc.shutdown();
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "shutdown took {:?}",
            began.elapsed()
        );
        until("the process to be killed", || !pid_alive(pid));
        assert!(!svc.any_alive());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_rename_keeps_the_decoder_and_a_new_password_replaces_it() {
        let d = dir("apply");
        let svc = service(camera("cam1"), &fake_ffmpeg(&d, FRAMES));
        assert!(matches!(
            svc.frame("cam1", 0, Duration::from_millis(1500), &not_stopping),
            Ok(Frame::Jpeg { .. })
        ));
        let mut renamed = camera("cam1");
        renamed["streams"][0]["name"] = json!("Back");
        svc.apply_settings(&renamed);
        assert_eq!(svc.status()["streams"][0]["name"], "Back");
        assert!(matches!(
            svc.frame("cam1", 0, Duration::from_millis(1500), &not_stopping),
            Ok(Frame::Jpeg { .. })
        ));
        assert_eq!(pids(&d).len(), 1, "the running decoder was kept");
        let mut changed = camera("cam1");
        changed["streams"][0]["password"] = json!("other");
        svc.apply_settings(&changed);
        assert!(matches!(
            svc.frame("cam1", 0, Duration::from_millis(1500), &not_stopping),
            Ok(Frame::Jpeg { .. })
        ));
        assert_eq!(pids(&d).len(), 2, "new credentials start a new decoder");
        until("the old process to end", || !pid_alive(pids(&d)[0]));
        svc.apply_settings(&json!({"streams": [], "ffmpeg": ""}));
        assert!(matches!(
            svc.frame("cam1", 0, Duration::from_millis(10), &not_stopping),
            Err(FrameError::NoSuchCamera)
        ));
        svc.shutdown();
        for p in pids(&d) {
            assert!(!pid_alive(p));
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn no_ffmpeg_is_reported_not_hidden() {
        let svc = service(camera("cam1"), "");
        let Ok(Frame::State { state, message, .. }) = svc.frame("cam1", 0, Duration::from_millis(10), &not_stopping)
        else {
            panic!("expected a state");
        };
        assert_eq!(state, "unavailable");
        assert!(message.contains("ffmpeg was not found"));
        let status = svc.status();
        assert_eq!(status["available"], false);
        assert!(status["reason"].as_str().unwrap().starts_with("ffmpeg was not found."));
        let configured = service(json!({"streams": [], "ffmpeg": "/no/such/ffmpeg"}), "");
        assert_eq!(configured.status()["reason"], "No ffmpeg at /no/such/ffmpeg.");
        svc.shutdown();
        configured.shutdown();
    }

    #[test]
    fn a_webrtc_camera_has_no_frames_and_an_rtsp_one_no_offer() {
        let svc = service(
            json!({"streams": [
                {"id": "w1", "url": "http://127.0.0.1:1/whep", "kind": "webrtc"},
                {"id": "r1", "url": "rtsp://h/x"}]}),
            "",
        );
        assert!(matches!(
            svc.frame("w1", 0, Duration::from_millis(10), &not_stopping),
            Err(FrameError::WrongKind(_))
        ));
        assert!(matches!(svc.whep_offer("r1", "v=0\r\n"), WhepResult::WrongKind(_)));
        assert!(matches!(svc.whep_offer("zz", "v=0\r\n"), WhepResult::NoSuchCamera));
        match svc.whep_offer("w1", "not sdp") {
            WhepResult::Failed { status, error } => {
                assert_eq!((status, error.as_str()), (400, "That is not an SDP offer."))
            }
            _ => panic!("expected a refusal"),
        }
        match svc.whep_offer("w1", "v=0\r\n") {
            WhepResult::Failed { status, error } => {
                assert_eq!(status, 502);
                assert!(error.contains("The camera server did not answer"), "{error}");
            }
            _ => panic!("nothing listens there"),
        }
        svc.shutdown();
    }

    type Request = (String, String, Vec<(String, String)>, String);
    type Answer = (u16, Vec<(String, String)>, String);
    type Seen = Arc<Mutex<Vec<Request>>>;

    /// A WHEP server on a loopback port; `reply` decides every answer.
    struct Whep {
        port: u16,
        seen: Seen,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Whep {
        fn start(reply: impl Fn(&str, &str, u16) -> Answer + Send + 'static) -> Whep {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let seen: Seen = Arc::default();
            let stop = Arc::new(AtomicBool::new(false));
            let (s, st) = (Arc::clone(&seen), Arc::clone(&stop));
            let thread = std::thread::spawn(move || {
                while !st.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((conn, _)) => {
                            let _ = conn.set_nonblocking(false);
                            serve(conn, &s, port, &reply);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            });
            Whep {
                port,
                seen,
                stop,
                thread: Some(thread),
            }
        }

        fn requests(&self) -> Vec<Request> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Drop for Whep {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn serve(mut conn: TcpStream, seen: &Seen, port: u16, reply: &dyn Fn(&str, &str, u16) -> Answer) {
        let _ = conn.set_read_timeout(Some(Duration::from_secs(3)));
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
            match conn.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let mut lines = head.lines();
        let first = lines.next().unwrap_or("").to_string();
        let mut parts = first.split(' ');
        let (method, path) = (
            parts.next().unwrap_or("").to_string(),
            parts.next().unwrap_or("").to_string(),
        );
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| {
                l.split_once(':')
                    .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
            })
            .collect();
        let length: usize = headers
            .iter()
            .find(|(k, _)| k == "content-length")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        let mut body = buf[head_end + 4..].to_vec();
        while body.len() < length {
            match conn.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => body.extend_from_slice(&chunk[..n]),
            }
        }
        seen.lock().unwrap().push((
            method.clone(),
            path.clone(),
            headers,
            String::from_utf8_lossy(&body).into_owned(),
        ));
        let (status, extra, answer) = reply(&method, &path, port);
        let mut out = format!(
            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
            answer.len()
        );
        for (k, v) in extra {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        out.push_str("\r\n");
        out.push_str(&answer);
        let _ = conn.write_all(out.as_bytes());
    }

    fn webrtc(port: u16, username: &str, password: &str) -> Value {
        json!({"streams": [{"id": "w1", "url": format!("http://127.0.0.1:{port}/cam/whep"),
                            "kind": "webrtc", "username": username, "password": password}]})
    }

    #[test]
    fn whep_signalling_keeps_the_credentials_here_and_closes_the_session() {
        let server = Whep::start(|method, path, _| match (method, path) {
            ("POST", "/cam/whep") => (
                201,
                vec![("Location".into(), "/cam/whep/session-1".into())],
                "v=0\r\ns=answer\r\n".into(),
            ),
            ("DELETE", "/cam/whep/session-1") => (200, vec![], String::new()),
            _ => (404, vec![], String::new()),
        });
        let svc = service(webrtc(server.port, "pilot", "hunter2"), "");
        let WhepResult::Ok { sdp, session } = svc.whep_offer("w1", "v=0\r\ns=offer\r\n") else {
            panic!("the offer was refused");
        };
        assert_eq!(sdp, "v=0\r\ns=answer\r\n");
        assert!(
            !session.contains("session-1") && session.len() >= 16,
            "the token is opaque"
        );
        let first = &server.requests()[0];
        assert_eq!(first.0, "POST");
        let get = |name: &str| first.2.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        assert_eq!(get("content-type").as_deref(), Some("application/sdp"));
        assert_eq!(get("authorization").as_deref(), Some("Basic cGlsb3Q6aHVudGVyMg=="));
        assert_eq!(first.3, "v=0\r\ns=offer\r\n");
        assert_eq!(svc.status()["streams"][0]["sessions"], 1);

        assert!(svc.whep_close(&session));
        assert!(!svc.whep_close(&session), "closing twice is harmless");
        let deleted = server
            .requests()
            .into_iter()
            .find(|r| r.0 == "DELETE")
            .expect("the session was deleted");
        assert_eq!(deleted.1, "/cam/whep/session-1");
        assert!(deleted
            .2
            .iter()
            .any(|(k, v)| k == "authorization" && v.starts_with("Basic ")));
        svc.shutdown();
    }

    #[test]
    fn whep_sessions_end_when_the_camera_changes_or_the_station_stops() {
        let server = Whep::start(|method, _, _| match method {
            "POST" => (201, vec![("Location".into(), "/s/1".into())], "v=0\r\n".into()),
            _ => (200, vec![], String::new()),
        });
        let svc = service(webrtc(server.port, "", "tok"), "");
        let WhepResult::Ok { .. } = svc.whep_offer("w1", "v=0\r\n") else {
            panic!()
        };
        assert_eq!(
            server.requests()[0]
                .2
                .iter()
                .find(|(k, _)| k == "authorization")
                .map(|(_, v)| v.as_str()),
            Some("Bearer tok")
        );
        let mut moved = webrtc(server.port, "", "tok");
        moved["streams"][0]["password"] = json!("new");
        svc.apply_settings(&moved);
        assert_eq!(
            svc.status()["streams"][0]["sessions"],
            0,
            "a session holding old credentials is closed"
        );
        assert!(server.requests().iter().any(|r| r.0 == "DELETE"));

        let WhepResult::Ok { .. } = svc.whep_offer("w1", "v=0\r\n") else {
            panic!()
        };
        let before = server.requests().iter().filter(|r| r.0 == "DELETE").count();
        svc.shutdown();
        let after = server.requests().iter().filter(|r| r.0 == "DELETE").count();
        assert_eq!(after, before + 1, "shutdown closes the open session");
    }

    #[test]
    fn whep_refusals_are_worded_for_the_operator_and_never_followed() {
        let server = Whep::start(|method, path, port| match (method, path) {
            ("POST", "/cam/whep") => (401, vec![], "{\"error\": \"bad credentials hunter2\"}".into()),
            _ => (
                200,
                vec![("Location".into(), format!("http://127.0.0.1:{port}/x"))],
                String::new(),
            ),
        });
        let svc = service(webrtc(server.port, "pilot", "hunter2"), "");
        match svc.whep_offer("w1", "v=0\r\n") {
            WhepResult::Failed { status, error } => {
                assert_eq!(status, 502);
                assert_eq!(
                    error,
                    "The camera server refused the user or password. (bad credentials ***)"
                );
            }
            _ => panic!("expected a refusal"),
        }
        svc.shutdown();

        let redirect = Whep::start(|_, _, port| {
            (
                307,
                vec![("Location".into(), format!("http://127.0.0.1:{port}/elsewhere"))],
                String::new(),
            )
        });
        let svc = service(webrtc(redirect.port, "", ""), "");
        match svc.whep_offer("w1", "v=0\r\n") {
            WhepResult::Failed { status, error } => {
                assert_eq!(status, 502);
                assert!(error.starts_with("The camera server redirected the request"), "{error}");
            }
            _ => panic!("a redirect is not followed"),
        }
        assert_eq!(redirect.requests().len(), 1, "no second request was made");
        svc.shutdown();

        let wrong = Whep::start(|_, _, _| (200, vec![], "this is html".into()));
        let svc = service(webrtc(wrong.port, "", ""), "");
        match svc.whep_offer("w1", "v=0\r\n") {
            WhepResult::Failed { error, .. } => {
                assert_eq!(
                    error,
                    "The camera server answered with something that is not an SDP answer."
                );
            }
            _ => panic!(),
        }
        svc.shutdown();
    }

    #[test]
    fn a_session_url_on_another_origin_is_not_used() {
        let server = Whep::start(|method, _, _| match method {
            "POST" => (
                201,
                vec![("Location".into(), "http://127.0.0.1:1/elsewhere".into())],
                "v=0\r\n".into(),
            ),
            _ => (200, vec![], String::new()),
        });
        let svc = service(webrtc(server.port, "u", "p"), "");
        let WhepResult::Ok { session, .. } = svc.whep_offer("w1", "v=0\r\n") else {
            panic!()
        };
        assert!(svc.whep_close(&session));
        assert!(server.requests().iter().all(|r| r.0 != "DELETE"));
        svc.shutdown();
    }
}
