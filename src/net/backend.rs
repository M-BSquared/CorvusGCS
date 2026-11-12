//! The Corvus backend this app talks to.
//!
//! By default it is `corvus_core` running in this very process, with its
//! HTTP server on a free loopback port that the interface's client reads
//! (the first step of the port; an area that is ported gets called directly
//! later). An area that is not ported yet answers 501 "Not available in this
//! build", which the pages show as their error text.
//!
//! The field laptop is rebooted between flights, so the app must never leave a
//! server behind: on every way out (window closed, SIGINT, SIGTERM, a panic
//! unwinding through `main`) the core is stopped, which joins every thread it
//! started.

use std::path::{Path, PathBuf};

use crate::net::http::{self, Server};

/// A backend of an earlier run that still holds the instance lock and answers:
/// its address when there is one. The core refuses to start beside
/// it ("already running"), so a second start joins it instead of exiting.
pub fn running_peer() -> Option<Server> {
    if std::env::var("CORVUS_ALLOW_MULTI")
        .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
    {
        return None;
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    peer_from_lock(&PathBuf::from(home).join(".corvus").join("corvus.lock"))
}

fn peer_from_lock(lock: &Path) -> Option<Server> {
    let text = std::fs::read_to_string(lock).ok()?;
    let record: serde_json::Value = serde_json::from_str(text.trim_end_matches('\0').trim()).ok()?;
    let port = u16::try_from(record.get("port")?.as_u64()?).ok().filter(|p| *p != 0)?;
    let server = Server::local(port);
    http::get_json(&server, "/api/version").ok()?;
    Some(server)
}

/// A running backend.
pub struct Backend {
    core: Option<corvus_core::Core>,
    server: Server,
}

impl Backend {
    /// Starts the core in this process, serving on a free loopback port. A
    /// second Corvus on this computer is refused (`CORVUS_ALLOW_MULTI=1`
    /// allows it).
    pub fn start_core() -> Result<Backend, String> {
        let options = corvus_core::Options::default();
        let core = corvus_core::CoreBuilder::new(options)
            .module(corvus_core::VehicleModule::with_options(corvus_core::VehicleOptions {
                autostart: true,
                connection: None,
            }))
            .module(corvus_core::PeripheralsModule::new())
            .module(corvus_core::LogsModule::new())
            .module(corvus_core::FirmwareModule::new())
            .module(corvus_core::PluginsModule::new())
            .module(corvus_core::McpModule::new())
            .module(corvus_core::SetupSchemasModule::new())
            .module(corvus_core::MapDataModule::new())
            .module(corvus_core::Http::new(corvus_core::HttpOptions {
                port: 0,
                ..corvus_core::HttpOptions::default()
            }))
            .build()?;
        core.claim_instance()?;
        core.start().map_err(|e| format!("could not start the backend: {e}"))?;
        let port = core
            .service::<corvus_core::HttpInfo>()
            .and_then(|i| i.addr())
            .map(|a| a.port())
            .ok_or("the backend has no port")?;
        Ok(Backend {
            core: Some(core),
            server: Server::local(port),
        })
    }

    /// The server to talk to.
    pub fn server(&self) -> &Server {
        &self.server
    }

    /// Stops the core; every thread it started is joined. Idempotent.
    pub fn stop(&mut self) {
        if let Some(core) = self.core.take() {
            core.stop();
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn answer_version_once() -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let body = r#"{"product":"Corvus GCS","version":"1"}"#;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        port
    }

    fn lock_file(text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("corvus-lock-{}-{}", std::process::id(), text.len()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("corvus.lock");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn a_running_instance_that_answers_is_joined() {
        let port = answer_version_once();
        let lock = lock_file(&format!(r#"{{"pid": 1, "port": {port}, "started": 1.0}}"#));
        assert_eq!(peer_from_lock(&lock).map(|s| s.url()), Some(Server::local(port).url()));
    }

    #[test]
    fn a_lock_that_names_a_port_nobody_answers_on_is_ignored() {
        let free = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let lock = lock_file(&format!(r#"{{"pid": 1, "port": {free}, "started": 1.0}}"#));
        assert!(peer_from_lock(&lock).is_none());
    }

    #[test]
    fn a_missing_empty_or_broken_lock_is_ignored() {
        assert!(peer_from_lock(Path::new("/nonexistent/corvus.lock")).is_none());
        assert!(peer_from_lock(&lock_file("")).is_none());
        assert!(peer_from_lock(&lock_file("not json")).is_none());
        assert!(peer_from_lock(&lock_file(r#"{"pid": 1, "port": 0}"#)).is_none());
    }

    fn core_in(dir: &str) -> Result<Backend, String> {
        let dir = std::env::temp_dir().join(format!("corvus-backend-{dir}-{}", std::process::id()));
        let core = corvus_core::CoreBuilder::new(corvus_core::Options::in_dir(&dir))
            .module(corvus_core::Http::new(corvus_core::HttpOptions {
                port: 0,
                ..corvus_core::HttpOptions::default()
            }))
            .build()?;
        core.claim_instance()?;
        core.start().map_err(|e| e.to_string())?;
        let port = core
            .service::<corvus_core::HttpInfo>()
            .and_then(|i| i.addr())
            .map(|a| a.port())
            .unwrap();
        Ok(Backend {
            core: Some(core),
            server: Server::local(port),
        })
    }

    #[test]
    fn the_core_answers_the_interface_client() {
        let _ =
            std::fs::remove_dir_all(std::env::temp_dir().join(format!("corvus-backend-client-{}", std::process::id())));
        let mut b = core_in("client").unwrap();
        let v = http::get_json(b.server(), "/api/version").unwrap();
        assert_eq!(v["product"], "Corvus GCS");
        let err = http::get_json(b.server(), "/api/mavlink/modes").unwrap_err();
        assert_eq!(err, "Not available in this build");
        let err = http::post_json(b.server(), "/api/mavlink/arm", &serde_json::json!({})).unwrap_err();
        assert_eq!(err, "Not available in this build");
        let port = b.server().url();
        b.stop();
        b.stop();
        assert!(
            http::get_json(&Server::parse(&port).unwrap(), "/api/version").is_err(),
            "the core is gone"
        );
    }

    #[test]
    fn a_second_backend_on_the_same_state_is_refused() {
        let _ =
            std::fs::remove_dir_all(std::env::temp_dir().join(format!("corvus-backend-twice-{}", std::process::id())));
        let first = core_in("twice").unwrap();
        let second = core_in("twice").err().expect("refused");
        assert!(second.starts_with("CORVUS GCS is already running"), "{second}");
        drop(first);
    }
}
