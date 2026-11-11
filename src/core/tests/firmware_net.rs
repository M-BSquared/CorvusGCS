//! The firmware catalogue over real HTTP, against a stub server on this
//! computer standing in for GitHub and for ArduPilot's server.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use corvus_core::firmware::net::HttpNet;
use corvus_core::firmware::FirmwareCatalog;
use serde_json::json;

struct Stub {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl Stub {
    /// Serves `(path, status, body)`; the path is `/<host><path>` as the rewrite writes it.
    fn start(routes: Vec<(String, u16, Vec<u8>)>) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (seen2, stop2) = (Arc::clone(&seen), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                let Ok((mut conn, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                conn.set_nonblocking(false).unwrap();
                let mut buf = [0u8; 4096];
                let n = conn.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                seen2.lock().unwrap().push(head.clone());
                let (status, body) = routes
                    .iter()
                    .find(|(p, _, _)| *p == path)
                    .map_or((404, b"nothing here".to_vec()), |(_, s, b)| (*s, b.clone()));
                let reason = if status == 200 { "OK" } else { "Not Found" };
                let _ = write!(
                    conn,
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = conn.write_all(&body);
            }
        });
        Stub { port, seen, stop }
    }

    fn net(&self) -> HttpNet {
        let port = self.port;
        HttpNet::with_rewrite(move |url| match url.strip_prefix("https://") {
            Some(rest) => format!("http://127.0.0.1:{port}/{rest}"),
            None => url.to_string(),
        })
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("corvus-fw-net-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn the_catalogue_is_read_cached_and_flashable_over_http() {
    let image = vec![7u8; 100_000];
    let releases = json!([{
        "tag_name": "v1.17.0", "name": "v1.17.0", "prerelease": false, "published_at": "2026-01-01T00:00:00Z",
        "assets": [{"name": "px4_fmu-v6x_default.px4", "size": image.len(),
                    "browser_download_url": "https://github.com/PX4/PX4-Autopilot/releases/download/v1.17.0/px4_fmu-v6x_default.px4"}],
    }]);
    let mut routes = vec![
        (
            "/api.github.com/repos/PX4/PX4-Autopilot/releases".to_string(),
            200,
            serde_json::to_vec(&releases).unwrap(),
        ),
        (
            "/github.com/PX4/PX4-Autopilot/releases/download/v1.17.0/px4_fmu-v6x_default.px4".to_string(),
            200,
            image.clone(),
        ),
    ];
    routes.push((
        "/firmware.ardupilot.org/Copter/stable/".to_string(),
        200,
        b"<a href=\"/Copter/stable/CubeOrange\">x</a>".to_vec(),
    ));
    let stub = Stub::start(routes);
    let folder = dir("ok");
    let catalog = FirmwareCatalog::new(folder.to_string_lossy(), Arc::new(stub.net()));

    let answer = catalog.catalog(true).unwrap();
    let tags: Vec<&str> = answer["releases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["tag"].as_str().unwrap())
        .collect();
    assert_eq!(tags, ["v1.17.0", "ardupilot:Copter/stable"]);
    let error = answer["error"].as_str().unwrap();
    assert!(error.is_empty(), "{error}");
    assert!(folder.join("catalog.json").is_file() && folder.join("catalog-ardupilot.json").is_file());
    assert!(stub.seen.lock().unwrap()[0]
        .to_lowercase()
        .contains("user-agent: corvusgcs/"));

    let entry = catalog.resolve("v1.17.0", "px4_fmu-v6x_default.px4").unwrap();
    let seen = Mutex::new(Vec::new());
    let report = |done: i64, total: i64| seen.lock().unwrap().push((done, total));
    let bytes = catalog.download(&entry, Some(&report), None).unwrap();
    assert_eq!(bytes, image);
    let seen = seen.into_inner().unwrap();
    assert_eq!(seen.last(), Some(&(100_000, 100_000)));
    assert!(seen.len() > 1, "the body arrives in chunks");
    assert!(folder.join("px4_fmu-v6x_default.px4").is_file());
    let before = stub.seen.lock().unwrap().len();
    assert_eq!(catalog.download(&entry, None, None).unwrap(), image);
    assert_eq!(
        stub.seen.lock().unwrap().len(),
        before,
        "a cached image needs no network"
    );
}

#[test]
fn an_unreachable_server_and_a_404_read_as_failures_with_the_cache_kept() {
    let folder = dir("down");
    let catalog = FirmwareCatalog::new(
        folder.to_string_lossy(),
        Arc::new(HttpNet::with_rewrite(|url| {
            url.replacen("https://", "http://127.0.0.1:1/", 1)
        })),
    );
    let answer = catalog.catalog(true).unwrap();
    assert_eq!(answer["releases"], json!([]));
    let error = answer["error"].as_str().unwrap();
    assert!(error.starts_with("could not reach the PX4 release server ("), "{error}");
    assert!(
        error.contains("could not reach the ArduPilot firmware server"),
        "{error}"
    );

    let stub = Stub::start(Vec::new());
    let entry = json!({"name": "a.px4", "url": "https://github.com/PX4/x/a.px4"});
    let catalog = FirmwareCatalog::new(folder.to_string_lossy(), Arc::new(stub.net()));
    let err = catalog.download(&entry, None, None).unwrap_err();
    assert!(err.starts_with("firmware download failed ("), "{err}");
    assert!(!folder.join("a.px4").exists());
}
