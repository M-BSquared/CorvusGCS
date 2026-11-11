//! The asynchronous half of the SSH transport: connecting, host key policy and
//! authentication, on a private single threaded runtime that the caller's own
//! thread drives. Nothing here outlives the call that made the runtime.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, Handle};
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use tokio::net::TcpStream;

use super::hostkeys::{self, Decision, HostKeyFiles};

/// The connect, banner and login limit of paramiko's calls here (`timeout=8`).
pub const TIMEOUT_S: u64 = 8;

/// What a failed connect is, for the operator's wording and for `auth_failed`.
#[derive(Debug, Clone)]
pub struct ConnectError {
    /// The text a caller shows.
    pub message: String,
    /// The host answered and refused the login.
    pub auth_failed: bool,
    /// A host key mismatch, which has its own wording.
    pub bad_host_key: bool,
    /// What Python's `str(exc)` would read, for the one shot command.
    pub plain: String,
}

impl ConnectError {
    fn new(message: impl Into<String>) -> ConnectError {
        let m: String = message.into();
        ConnectError {
            plain: m.clone(),
            message: m,
            auth_failed: false,
            bad_host_key: false,
        }
    }

    /// A failure that is only its text.
    pub fn plain(message: impl Into<String>) -> ConnectError {
        ConnectError::new(message)
    }

    fn auth(message: &str) -> ConnectError {
        ConnectError {
            auth_failed: true,
            ..ConnectError::new(message)
        }
    }
}

/// How to log in: a key file, a password, or neither (then the agent and the
/// default key files are tried, as paramiko does).
#[derive(Debug, Clone, Default)]
pub struct Login {
    pub username: String,
    pub password: Option<String>,
    pub key_path: Option<String>,
    /// Where the default keys are looked for when there is no password and no
    /// key file; `None` is `~/.ssh`.
    pub ssh_dir: Option<std::path::PathBuf>,
}

/// The host key decision, shared between the handler and the connect call.
#[derive(Default)]
struct Verdict {
    decision: Option<Decision>,
    remembered: Option<String>,
}

pub struct KeyHandler {
    host: String,
    port: u16,
    files: HostKeyFiles,
    verdict: Arc<Mutex<Verdict>>,
}

impl client::Handler for KeyHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        let decision = hostkeys::decide(&self.files, &self.host, self.port, key);
        let mut v = self.verdict.lock().unwrap_or_else(|e| e.into_inner());
        let ok = match &decision {
            Decision::Known => true,
            Decision::New => {
                v.remembered = Some(format!(
                    "accepting a new SSH host key for {} ({} {}) without verification; it is remembered in {}, and a different key from this host will be refused from now on. Set {}=strict to refuse unknown hosts instead.",
                    hostkeys::host_name(&self.host, self.port),
                    key.algorithm(),
                    hostkeys::base64_of(key),
                    self.files.corvus.display(),
                    hostkeys::HOST_KEY_POLICY_ENV,
                ));
                let _ = hostkeys::remember(&self.files, &self.host, self.port, key);
                true
            }
            Decision::Rejected(_) | Decision::Mismatch { .. } => false,
        };
        v.decision = Some(decision);
        Ok(ok)
    }
}

/// `exc.strerror` of a refused or unreachable address, without Rust's tail.
fn strerror(e: &std::io::Error) -> String {
    crate::peripherals::util::os_error_text(e)
}

fn lookup_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    text.rsplit(": ").next().unwrap_or(&text).to_string()
}

/// Opens the TCP connection the way `socket.create_connection` does: every
/// address of the host in turn, the errors kept for the message.
async fn open_tcp(host: &str, port: u16) -> Result<TcpStream, ConnectError> {
    let where_ = format!("{host}:{port}");
    let addrs: Vec<SocketAddr> = match tokio::net::lookup_host((host, port)).await {
        Ok(a) => a.collect(),
        Err(e) => {
            return Err(ConnectError::new(format!("{} ({where_}).", lookup_text(&e))));
        }
    };
    let mut errors: Vec<(SocketAddr, std::io::Error)> = Vec::new();
    let mut timed_out = false;
    for addr in addrs {
        match tokio::time::timeout(Duration::from_secs(TIMEOUT_S), TcpStream::connect(addr)).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) => errors.push((addr, e)),
            Err(_) => timed_out = true,
        }
    }
    if errors.is_empty() {
        let mut e = ConnectError::new(format!("No answer from {where_} within {TIMEOUT_S} s."));
        if timed_out {
            e.plain = "timed out".into();
        }
        return Err(e);
    }
    errors.sort_by_key(|a| a.0.ip().to_string());
    let mut causes: Vec<String> = errors.iter().map(|(_, e)| strerror(e)).collect();
    causes.sort();
    causes.dedup();
    let ips: Vec<String> = errors.iter().map(|(a, _)| a.ip().to_string()).collect();
    let tail = ips.last().cloned().unwrap_or_default();
    let body = ips[..ips.len() - 1].join(", ");
    let plain = if body.is_empty() {
        format!("[Errno None] Unable to connect to port {port} on {tail}")
    } else {
        format!("[Errno None] Unable to connect to port {port} on {body} or {tail}")
    };
    Err(ConnectError {
        plain,
        ..ConnectError::new(format!("{} ({where_}).", causes.join(", ")))
    })
}

fn default_key_files(dir: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => {
            let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
            let Some(home) = home else { return Vec::new() };
            std::path::PathBuf::from(home).join(".ssh")
        }
    };
    ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"]
        .iter()
        .map(|n| dir.join(n))
        .collect()
}

async fn try_key(
    session: &mut Handle<KeyHandler>,
    user: &str,
    path: &std::path::Path,
) -> Result<Option<bool>, ConnectError> {
    let key = match russh::keys::load_secret_key(path, None) {
        Ok(k) => k,
        Err(_) => return Ok(None),
    };
    let hash = session.best_supported_rsa_hash().await.ok().flatten().flatten();
    let result = session
        .authenticate_publickey(user.to_string(), PrivateKeyWithHashAlg::new(Arc::new(key), hash))
        .await
        .map_err(|e| ConnectError::new(e.to_string()))?;
    Ok(Some(result.success()))
}

/// Connects, judges the host key and logs in. The returned handle is open.
pub async fn connect(
    host: &str,
    port: u16,
    login: &Login,
    files: &HostKeyFiles,
) -> Result<Handle<KeyHandler>, ConnectError> {
    let stream = open_tcp(host, port).await?;
    let verdict = Arc::new(Mutex::new(Verdict::default()));
    let handler = KeyHandler {
        host: host.to_string(),
        port,
        files: files.clone(),
        verdict: Arc::clone(&verdict),
    };
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        ..client::Config::default()
    });
    let opened = tokio::time::timeout(
        Duration::from_secs(TIMEOUT_S),
        client::connect_stream(config, stream, handler),
    )
    .await;
    let mut session = match opened {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(key_failure(&verdict, files, host, port, e)),
        Err(_) => {
            let mut e = ConnectError::new(format!("No answer from {host}:{port} within {TIMEOUT_S} s."));
            e.plain = "timed out".into();
            return Err(e);
        }
    };
    if let Some(note) = verdict.lock().unwrap_or_else(|e| e.into_inner()).remembered.take() {
        let _ = note;
    }
    let user = login.username.clone();
    let auth = async {
        if let Some(key_path) = &login.key_path {
            let key = russh::keys::load_secret_key(expand_user(key_path), None)
                .map_err(|e| ConnectError::new(e.to_string()))?;
            let hash = session.best_supported_rsa_hash().await.ok().flatten().flatten();
            let r = session
                .authenticate_publickey(user.clone(), PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(|e| ConnectError::new(e.to_string()))?;
            return if r.success() {
                Ok(())
            } else {
                Err(ConnectError::auth("Authentication failed."))
            };
        }
        if let Some(password) = login.password.as_deref().filter(|p| !p.is_empty()) {
            let r = session
                .authenticate_password(user.clone(), password.to_string())
                .await
                .map_err(|e| ConnectError::new(e.to_string()))?;
            return if r.success() {
                Ok(())
            } else {
                Err(ConnectError::auth("Authentication failed."))
            };
        }
        let mut tried = false;
        for path in default_key_files(login.ssh_dir.as_deref()) {
            if !path.is_file() {
                continue;
            }
            match try_key(&mut session, &user, &path).await? {
                Some(true) => return Ok(()),
                Some(false) => tried = true,
                None => {}
            }
        }
        if tried {
            Err(ConnectError::auth("Authentication failed."))
        } else {
            Err(ConnectError::auth("No authentication methods available"))
        }
    };
    match tokio::time::timeout(Duration::from_secs(TIMEOUT_S), auth).await {
        Ok(Ok(())) => Ok(session),
        Ok(Err(e)) => {
            let _ = session.disconnect(russh::Disconnect::ByApplication, "", "en").await;
            Err(e)
        }
        Err(_) => {
            let _ = session.disconnect(russh::Disconnect::ByApplication, "", "en").await;
            let mut e = ConnectError::new(format!("No answer from {host}:{port} within {TIMEOUT_S} s."));
            e.plain = "timed out".into();
            Err(e)
        }
    }
}

fn expand_user(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

/// The failure of the key exchange: a refused host key has the decision it
/// came from, anything else is the library's own text.
fn key_failure(
    verdict: &Arc<Mutex<Verdict>>,
    files: &HostKeyFiles,
    host: &str,
    port: u16,
    e: russh::Error,
) -> ConnectError {
    let decision = verdict.lock().unwrap_or_else(|e| e.into_inner()).decision.clone();
    match decision {
        Some(Decision::Mismatch { got, expected }) => {
            let plain = hostkeys::bad_host_key_message(host, &got, &expected);
            ConnectError {
                message: hostkeys::host_key_hint(&plain, &files.corvus),
                plain: hostkeys::host_key_hint(&plain, &files.corvus),
                auth_failed: false,
                bad_host_key: true,
            }
        }
        Some(Decision::Rejected(why)) => ConnectError::new(why),
        _ => match e {
            russh::Error::IO(io) => ConnectError::new(format!("{} ({host}:{port}).", strerror(&io))),
            russh::Error::Disconnect | russh::Error::HUP => ConnectError::new("Error reading SSH protocol banner"),
            other => ConnectError::new(other.to_string()),
        },
    }
}
