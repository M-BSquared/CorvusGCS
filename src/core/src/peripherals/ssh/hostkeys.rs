//! Host key verification (`_new_client` and `_RememberAndWarnPolicy` in
//! `ssh_bridge.py`, with paramiko's `SSHClient` rules).
//!
//! Keys are read from the operator's own `~/.ssh/known_hosts` (read only) and
//! from Corvus' own `~/.corvus/known_hosts`, where an accepted key is written
//! back. The field default stays trust on first use, because an operator
//! connecting to a fresh companion computer over a point to point link has no
//! entry. What changes is the second connection: a key that does not match the
//! one recorded is refused. `CORVUS_SSH_HOST_KEYS=strict` turns the first
//! connection into a refusal too.
//!
//! paramiko's order: the system file wins when it knows the host at all (a
//! different key type there is a mismatch, not a miss), Corvus' file is asked
//! only otherwise.

use std::path::{Path, PathBuf};

use russh::keys::known_hosts::known_host_keys_path;
use russh::keys::PublicKey;

pub const HOST_KEY_POLICY_ENV: &str = "CORVUS_SSH_HOST_KEYS";

/// Where the keys are read from and the accepted ones written to.
#[derive(Debug, Clone)]
pub struct HostKeyFiles {
    /// The operator's own file, read only.
    pub system: Option<PathBuf>,
    /// Corvus' own file.
    pub corvus: PathBuf,
    pub strict: bool,
}

impl HostKeyFiles {
    /// The files of this computer: `~/.ssh/known_hosts` and `<corvus>/known_hosts`.
    pub fn standard(corvus_dir: &Path) -> HostKeyFiles {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        HostKeyFiles {
            system: home.map(|h| PathBuf::from(h).join(".ssh").join("known_hosts")),
            corvus: corvus_dir.join("known_hosts"),
            strict: strict_host_keys(),
        }
    }
}

/// True when the operator asked for no auto accept at all.
pub fn strict_host_keys() -> bool {
    std::env::var(HOST_KEY_POLICY_ENV)
        .map(|v| crate::peripherals::util::py_strip(&v).eq_ignore_ascii_case("strict"))
        .unwrap_or(false)
}

/// What to do with the key a host presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Recorded and equal.
    Known,
    /// Not recorded anywhere: accepted, and written down.
    New,
    /// Not recorded, and the policy is strict.
    Rejected(String),
    /// Recorded, and different. `got` and `expected` are the base64 of the keys.
    Mismatch { got: String, expected: String },
}

/// The base64 part of a key's OpenSSH line, as paramiko's `get_base64`.
pub fn base64_of(key: &PublicKey) -> String {
    key.to_openssh()
        .unwrap_or_default()
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string()
}

/// `server_hostkey_name`: the host as written in the file.
pub fn host_name(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

fn recorded(path: &Path, host: &str, port: u16) -> Vec<PublicKey> {
    known_host_keys_path(host, port, path)
        .map(|rows| rows.into_iter().map(|(_, k)| k).collect())
        .unwrap_or_default()
}

/// Judges the key `host:port` presented.
pub fn decide(files: &HostKeyFiles, host: &str, port: u16, key: &PublicKey) -> Decision {
    let mut keys = files
        .system
        .as_deref()
        .map(|p| recorded(p, host, port))
        .unwrap_or_default();
    if keys.is_empty() {
        keys = recorded(&files.corvus, host, port);
    }
    if keys.is_empty() {
        if files.strict {
            return Decision::Rejected(format!("Server '{}' not found in known_hosts", host_name(host, port)));
        }
        return Decision::New;
    }
    if keys.iter().any(|k| k.key_data() == key.key_data()) {
        return Decision::Known;
    }
    let expected = keys
        .iter()
        .find(|k| k.algorithm() == key.algorithm())
        .unwrap_or(&keys[0]);
    Decision::Mismatch {
        got: base64_of(key),
        expected: base64_of(expected),
    }
}

/// Writes an accepted key to Corvus' own file (created owner only, as the file
/// has to exist before the first host is accepted).
pub fn remember(files: &HostKeyFiles, host: &str, port: u16, key: &PublicKey) -> std::io::Result<()> {
    use std::io::Write;
    ensure_file(&files.corvus);
    let existing = std::fs::read_to_string(&files.corvus).unwrap_or_default();
    let mut line = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        line.push('\n');
    }
    let openssh = key.to_openssh().map_err(|e| std::io::Error::other(e.to_string()))?;
    // Name and key only, as paramiko writes it: the comment is not part of the key.
    let mut words = openssh.split_whitespace();
    let (kind, data) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    line.push_str(&format!("{} {kind} {data}\n", host_name(host, port)));
    std::fs::OpenOptions::new()
        .append(true)
        .open(&files.corvus)?
        .write_all(line.as_bytes())
}

/// Creates the file, owner only, if it is not there.
pub fn ensure_file(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let _ = opts.open(path);
}

/// The operator facing explanation for a host key that did not match.
pub fn host_key_hint(message: &str, corvus_file: &Path) -> String {
    format!(
        "{message}\nThe key this host presented is not the one recorded for it. Either the companion computer was reinstalled (remove its line from {} and connect again), or something else is answering on that address.",
        corvus_file.display()
    )
}

/// paramiko's `BadHostKeyException` text.
pub fn bad_host_key_message(host: &str, got: &str, expected: &str) -> String {
    format!("Host key for server '{host}' does not match: got '{got}', expected '{expected}'")
}
