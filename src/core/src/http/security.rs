//! What keeps a web page from flying the aircraft (`server.py`: the CORS,
//! Host and cross-site rules and the response headers).
//!
//! It listens on loopback only unless told otherwise, answers only names that
//! are loopback (or one the operator named), trusts an `Origin` only when it is
//! this server's own or the one named, and takes a write only with the key of
//! this Corvus ([`ApiKey`]). What starts programs, holds passwords or sets up
//! what an AI client may do answers this computer alone, key or not.
//!
//! `/mcp` has a token and rules of its own ([`super::mcp`]).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

use super::mcp::constant_time_eq;
use crate::request::Method;

/// A key set from outside, so that a second station or a script can write.
pub const API_KEY_ENV: &str = "CORVUS_API_KEY";
pub const API_KEY_HEADER: &str = "X-Corvus-Key";
/// The shortest key [`API_KEY_ENV`] may set.
pub const MIN_API_KEY: usize = 24;

pub const NEEDS_KEY: &str = "This needs the key of this Corvus GCS, which only its own window has. \
For a second station or a script, set CORVUS_API_KEY to the same value on both sides.";
pub const THIS_COMPUTER_ONLY: &str = "This can only be done on the computer Corvus GCS runs on.";

/// What lets an HTTP client change things. Random with every start and kept in
/// memory only: no route hands it out and no file holds it, so neither a web
/// page nor an AI client that can send requests has it.
pub struct ApiKey(String);

impl ApiKey {
    /// The key [`API_KEY_ENV`] sets, or a new random one.
    pub fn from_env_or_random() -> Result<ApiKey, String> {
        ApiKey::from_setting(&std::env::var(API_KEY_ENV).unwrap_or_default())
    }

    fn from_setting(set: &str) -> Result<ApiKey, String> {
        let set = set.trim();
        if set.is_empty() {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|e| format!("no random numbers for the API key: {e}"))?;
            return Ok(ApiKey(URL_SAFE_NO_PAD.encode(bytes)));
        }
        if set.len() < MIN_API_KEY || !set.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(format!(
                "{API_KEY_ENV} must be at least {MIN_API_KEY} printable ASCII characters without spaces"
            ));
        }
        Ok(ApiKey(set.to_string()))
    }

    /// For the window of this process. Never log it or answer a request with it.
    pub fn secret(&self) -> &str {
        &self.0
    }

    /// Whether a request carried the key, compared in constant time.
    pub fn matches(&self, header: Option<&str>) -> bool {
        header.is_some_and(|h| constant_time_eq(h.trim().as_bytes(), self.0.as_bytes()))
    }
}

/// Answers whether the key a client holds is this server's, and nothing else.
pub const KEY_CHECK: &str = "/api/key";

/// Reads that give out a token or a terminal, and so need the key like a write,
/// and the check of the key itself.
const KEYED_READS: [&str; 4] = [KEY_CHECK, "/api/mcp", "/api/ssh/stream", "/api/ssh/ws"];

pub fn needs_key(method: Method, path: &str) -> bool {
    path.starts_with("/api/") && (method != Method::Get || KEYED_READS.contains(&path))
}

/// What runs programs on this computer, uses its saved passwords or decides
/// what others may do.
const OWN_AREAS: [&str; 4] = ["/api/local/", "/api/ssh/", "/api/mcp/", "/api/settings/"];
const OWN_ROUTES: [&str; 5] = [
    "/api/mcp",
    "/api/plugins/folder",
    "/api/plugins/settings",
    "/api/update/open",
    "/api/video/settings",
];

/// Takes every permission that changes the vehicle away from an AI client and
/// holds it: whoever may fly from elsewhere may also press this.
const MCP_STOP: &str = "/api/mcp/stop";

pub fn this_computer_only(path: &str) -> bool {
    if path == MCP_STOP {
        return false;
    }
    OWN_AREAS.iter().any(|a| path.starts_with(a)) || OWN_ROUTES.contains(&path)
}

pub const REMOTE_ORIGIN_ENV: &str = "CORVUS_ALLOWED_ORIGIN";
pub const BIND_HOST_ENV: &str = "CORVUS_BIND";
pub const DEFAULT_BIND_HOST: &str = "127.0.0.1";

const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; \
worker-src 'self' blob:; child-src 'self' blob:; media-src 'self' blob:; object-src 'none'; \
base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Sent with every response unless the route set the same header itself.
pub const SECURITY_HEADERS: [(&str, &str); 4] = [
    ("Content-Security-Policy", CONTENT_SECURITY_POLICY),
    ("X-Content-Type-Options", "nosniff"),
    ("X-Frame-Options", "DENY"),
    ("Referrer-Policy", "no-referrer"),
];

/// `host[:port]` of an authority, the host lowercased and without brackets.
/// `None` for a port that is not a number in range or an empty host.
fn split_authority(authority: &str) -> Option<(String, Option<u16>)> {
    let a = authority.trim();
    let a = a.rsplit_once('@').map_or(a, |(_, h)| h);
    let (host, port) = if let Some(rest) = a.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        (h, after.strip_prefix(':'))
    } else {
        match a.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h, Some(p)),
            _ => (a, None),
        }
    };
    let port = match port {
        None => None,
        Some("") => None,
        Some(p) => Some(p.parse::<u16>().ok()?),
    };
    if host.is_empty() {
        return None;
    }
    Some((host.to_lowercase(), port))
}

fn configured_origin() -> String {
    std::env::var(REMOTE_ORIGIN_ENV).unwrap_or_default().trim().to_string()
}

/// `origin` when this server trusts it, else `""`. Never `*`: this header is
/// all that stands between a visited page and a vehicle's live telemetry, so
/// the answer is a name or nothing.
pub fn cors_allowed_origin(origin: &str, host_header: Option<&str>) -> String {
    trusted_origin(origin, host_header, &configured_origin())
}

/// The configured origin, or this server's own: a loopback page of exactly the
/// host and port the request went to. Another port on this computer is another
/// program, whose pages get nothing.
pub fn trusted_origin(origin: &str, host_header: Option<&str>, configured: &str) -> String {
    let allowed = allowed_origin(origin, configured);
    if allowed.is_empty() || origin == configured {
        return allowed;
    }
    let own = origin
        .strip_prefix("http://")
        .and_then(|rest| split_authority(rest.trim_end_matches('/')));
    let asked = host_header.and_then(split_authority);
    match (own, asked) {
        (Some((a, p)), Some((b, q))) if a == b && p.unwrap_or(80) == q.unwrap_or(80) => allowed,
        _ => String::new(),
    }
}

/// How an origin is read: well formed, and loopback or the configured one.
/// [`trusted_origin`] asks for more before anything is trusted.
pub fn allowed_origin(origin: &str, configured: &str) -> String {
    if origin.is_empty() || origin.len() > 256 {
        return String::new();
    }
    let Some((scheme, rest)) = origin.split_once("://") else {
        return String::new();
    };
    if scheme != "http" && scheme != "https" {
        return String::new();
    }
    let (authority, path) = match rest.find(['/', '?', '#', ';']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if !matches!(path, "" | "/") || authority.contains('@') {
        return String::new();
    }
    let Some((host, _)) = split_authority(authority) else {
        return String::new();
    };
    if LOOPBACK_HOSTS.contains(&host.as_str()) || origin == configured {
        origin.to_string()
    } else {
        String::new()
    }
}

/// Is the `Host` of a request one this server answers to? The defence against
/// DNS rebinding: a page on `evil.example` re-pointed at 127.0.0.1 makes
/// requests the browser calls same-origin, and the one thing it cannot forge is
/// the name in the URL bar. Reads are covered too: `GET /api/config` hands back
/// SSH hosts and key paths.
pub fn host_header_allowed(host_header: Option<&str>) -> bool {
    host_allowed(
        host_header,
        &std::env::var(BIND_HOST_ENV).unwrap_or_default(),
        &configured_origin(),
    )
}

pub fn host_allowed(host_header: Option<&str>, bind_env: &str, configured: &str) -> bool {
    let bind = bind_env.trim();
    if !bind.is_empty() && bind != DEFAULT_BIND_HOST {
        return true;
    }
    let Some(h) = host_header.map(str::trim).filter(|h| !h.is_empty()) else {
        return true;
    };
    if h.len() > 256 {
        return false;
    }
    let Some((host, _)) = split_authority(h) else {
        return false;
    };
    if LOOPBACK_HOSTS.contains(&host.as_str()) {
        return true;
    }
    if configured.is_empty() {
        return false;
    }
    configured
        .split_once("://")
        .and_then(|(_, rest)| split_authority(rest.split(['/', '?', '#']).next().unwrap_or("")))
        .is_some_and(|(c, _)| c == host)
}

/// Is a write allowed to reach an endpoint? A request carrying an `Origin`
/// needs an allowed one; without it, `Sec-Fetch-Site: cross-site` is refused.
pub fn mutating_allowed(origin: Option<&str>, fetch_site: Option<&str>, host_header: Option<&str>) -> bool {
    match origin.filter(|o| !o.is_empty()) {
        Some(o) => !cors_allowed_origin(o, host_header).is_empty(),
        None => fetch_site.map(|s| s.trim().to_lowercase()).as_deref() != Some("cross-site"),
    }
}

/// The interface to listen on: loopback unless `CORVUS_BIND` says otherwise,
/// with the loud warning once.
pub fn bind_host() -> String {
    let host = std::env::var(BIND_HOST_ENV).unwrap_or_default();
    let host = host.trim();
    if host.is_empty() || host == DEFAULT_BIND_HOST {
        return DEFAULT_BIND_HOST.to_string();
    }
    eprintln!(
        "corvus: {BIND_HOST_ENV}={host}: the HTTP API will accept requests from the network. Reading needs no key: anyone who \
can reach this port sees telemetry and settings. Changing anything needs {API_KEY_ENV}, and it travels unencrypted: \
whoever has it can arm the vehicle, so use a network you trust or an SSH tunnel. Browser writes from a remote UI remain \
blocked unless {REMOTE_ORIGIN_ENV} is set to that UI's exact origin"
    );
    let allowed = configured_origin();
    if !allowed.is_empty() {
        eprintln!("corvus: remote browser origin allowed to control the vehicle: {allowed}");
    }
    host.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_loopback_or_the_one_named() {
        for ok in [
            "http://localhost:8000",
            "http://127.0.0.1",
            "https://[::1]:9",
            "http://LOCALHOST/",
        ] {
            assert_eq!(allowed_origin(ok, ""), ok, "{ok}");
        }
        for bad in [
            "",
            "null",
            "ftp://localhost",
            "http://evil.example",
            "http://localhost.evil.example",
            "http://u@localhost",
            "http://localhost/path",
            "http://localhost?x=1",
            "http://localhost:99999",
            "http://localhost:abc",
            "localhost",
        ] {
            assert_eq!(allowed_origin(bad, ""), "", "{bad}");
        }
        assert_eq!(allowed_origin("https://gcs.lan", "https://gcs.lan"), "https://gcs.lan");
        assert_eq!(allowed_origin("https://gcs.lan", ""), "");
        assert_eq!(allowed_origin(&format!("http://localhost/{}", "a".repeat(300)), ""), "");
    }

    #[test]
    fn the_host_header_is_checked_against_rebinding() {
        for ok in [
            None,
            Some(""),
            Some("localhost:8000"),
            Some("127.0.0.1"),
            Some("[::1]:8000"),
            Some("LocalHost"),
        ] {
            assert!(host_allowed(ok, "", ""), "{ok:?}");
        }
        for bad in [
            Some("evil.example"),
            Some("evil.example:8000"),
            Some("localhost:junk"),
            Some(":80"),
            Some("127.0.0.1.evil.example"),
        ] {
            assert!(!host_allowed(bad, "", ""), "{bad:?}");
        }
        assert!(!host_allowed(Some(&"a".repeat(300)), "", ""));
        assert!(host_allowed(Some("gcs.lan:8000"), "", "https://gcs.lan"));
        assert!(!host_allowed(Some("other.lan"), "", "https://gcs.lan"));
        assert!(host_allowed(Some("anything"), "0.0.0.0", ""), "CORVUS_BIND opts out");
        assert!(!host_allowed(Some("anything"), "127.0.0.1", ""));
    }

    #[test]
    fn a_cross_site_write_is_refused() {
        let host = Some("localhost:8000");
        assert!(mutating_allowed(None, None, host));
        assert!(mutating_allowed(None, Some("same-origin"), host));
        assert!(!mutating_allowed(None, Some(" Cross-Site "), host));
        assert!(mutating_allowed(
            Some("http://localhost:8000"),
            Some("cross-site"),
            host
        ));
        assert!(!mutating_allowed(Some("http://evil.example"), None, host));
        assert!(!mutating_allowed(
            Some("http://localhost:9999"),
            Some("same-site"),
            host
        ));
    }

    /// Where this differs from the Python server on purpose: it trusted a page
    /// of any loopback port, which let another program's page run programs.
    #[test]
    fn only_this_servers_own_pages_and_the_named_one_are_trusted() {
        for (origin, host) in [
            ("http://127.0.0.1:8000", "127.0.0.1:8000"),
            ("http://LOCALHOST:8000/", "localhost:8000"),
            ("http://[::1]:8000", "[::1]:8000"),
            ("http://localhost", "localhost:80"),
        ] {
            assert_eq!(trusted_origin(origin, Some(host), ""), origin, "{origin}");
        }
        for (origin, host) in [
            ("http://localhost:9999", Some("localhost:8000")),
            ("http://localhost", Some("localhost:8000")),
            ("http://localhost:8000", Some("127.0.0.1:8000")),
            ("https://localhost:8000", Some("localhost:8000")),
            ("http://localhost:8000", None),
            ("http://evil.example:8000", Some("evil.example:8000")),
        ] {
            assert_eq!(trusted_origin(origin, host, ""), "", "{origin} at {host:?}");
        }
        assert_eq!(
            trusted_origin("https://gcs.lan", Some("10.0.0.2:8000"), "https://gcs.lan"),
            "https://gcs.lan"
        );
    }

    #[test]
    fn writes_and_the_reads_that_give_something_away_need_the_key() {
        assert!(needs_key(Method::Post, "/api/mavlink/arm"));
        assert!(needs_key(Method::Delete, "/api/anything"));
        for path in KEYED_READS {
            assert!(needs_key(Method::Get, path), "{path}");
        }
        assert!(needs_key(Method::Post, MCP_STOP));
        assert!(!needs_key(Method::Get, "/api/state"));
        assert!(!needs_key(Method::Post, "/mcp"), "its own token");
    }

    #[test]
    fn programs_passwords_and_permissions_stay_on_this_computer() {
        for path in [
            "/api/local/run",
            "/api/local/connect",
            "/api/ssh/send",
            "/api/ssh/ws",
            "/api/ssh/stream",
            "/api/ssh/run",
            "/api/ssh/power",
            "/api/ssh/connections",
            "/api/mcp",
            "/api/mcp/confirm",
            "/api/mcp/settings",
            "/api/settings/import",
            "/api/settings/export",
            "/api/plugins/settings",
            "/api/video/settings",
        ] {
            assert!(this_computer_only(path), "{path}");
        }
        for path in [
            "/api/state",
            "/api/mavlink/arm",
            "/api/mission/upload",
            "/api/plugins",
            KEY_CHECK,
            MCP_STOP,
        ] {
            assert!(!this_computer_only(path), "{path}");
        }
    }

    #[test]
    fn the_key_is_new_every_time_unless_set() {
        let a = ApiKey::from_setting("").unwrap();
        let b = ApiKey::from_setting("  ").unwrap();
        assert_eq!(a.secret().len(), 43);
        assert_ne!(a.secret(), b.secret());
        assert!(a.matches(Some(a.secret())));
        assert!(!a.matches(Some(b.secret())));
        assert!(!a.matches(Some("")));
        assert!(!a.matches(None));
        let set = "0123456789abcdefghijklmn";
        assert_eq!(ApiKey::from_setting(set).unwrap().secret(), set);
        for bad in ["short", "0123456789 abcdefghijklmn"] {
            assert!(ApiKey::from_setting(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_security_headers_are_the_pythons() {
        assert_eq!(SECURITY_HEADERS.len(), 4);
        assert!(CONTENT_SECURITY_POLICY.starts_with("default-src 'self'; script-src"));
        assert!(CONTENT_SECURITY_POLICY.ends_with("frame-ancestors 'none'"));
        assert_eq!(CONTENT_SECURITY_POLICY.split("; ").count(), 13);
    }

    fn cases() -> serde_json::Value {
        serde_json::from_str(include_str!("../../tests/data/http_cases.json")).unwrap()
    }

    #[test]
    fn origins_and_hosts_answer_as_the_python_server_does() {
        let c = cases();
        for (key, origins) in c["cors"].as_object().unwrap() {
            let (configured, _) = key.split_once('|').unwrap();
            for (origin, want) in origins.as_object().unwrap() {
                assert_eq!(
                    allowed_origin(origin, configured),
                    want.as_str().unwrap(),
                    "origin {origin:?} under {key}"
                );
            }
        }
        for (key, hosts) in c["host"].as_object().unwrap() {
            let (configured, bind) = key.split_once('|').unwrap();
            for (host, want) in hosts.as_object().unwrap() {
                let h = (host != "<none>").then_some(host.as_str());
                assert_eq!(
                    host_allowed(h, bind, configured),
                    want.as_bool().unwrap(),
                    "host {host:?} under {key}"
                );
            }
        }
    }

    #[test]
    fn the_headers_are_the_pythons() {
        let c = cases();
        assert_eq!(CONTENT_SECURITY_POLICY, c["csp"].as_str().unwrap());
        for (i, h) in c["security_headers"].as_array().unwrap().iter().enumerate() {
            assert_eq!(
                (SECURITY_HEADERS[i].0, SECURITY_HEADERS[i].1),
                (h[0].as_str().unwrap(), h[1].as_str().unwrap())
            );
        }
    }
}
