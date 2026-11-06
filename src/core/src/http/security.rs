//! What keeps a web page from flying the aircraft (`server.py`: the CORS,
//! Host and cross-site rules and the response headers).
//!
//! The API has no authentication, so the line is drawn here: it listens on
//! loopback only, answers only names that are loopback (or one the operator
//! named), echoes an `Origin` only when it is loopback or the one named, and
//! refuses a write that a browser made from another site.

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

/// `origin` when it is a configured or loopback web origin, else `""`. Never
/// `*`: this header is all that stands between a visited page and a vehicle's
/// live telemetry, so the answer is a name or nothing.
pub fn cors_allowed_origin(origin: &str) -> String {
    allowed_origin(origin, &configured_origin())
}

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
pub fn mutating_allowed(origin: Option<&str>, fetch_site: Option<&str>) -> bool {
    match origin.filter(|o| !o.is_empty()) {
        Some(o) => !cors_allowed_origin(o).is_empty(),
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
        "corvus: {BIND_HOST_ENV}={host}: the HTTP API will accept requests from the network, and it has no authentication. \
Any non-browser client that can reach this port can arm the vehicle. Browser writes from a remote UI remain blocked \
unless {REMOTE_ORIGIN_ENV} is set to that UI's exact origin"
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
        assert!(mutating_allowed(None, None));
        assert!(mutating_allowed(None, Some("same-origin")));
        assert!(!mutating_allowed(None, Some(" Cross-Site ")));
        assert!(mutating_allowed(Some("http://localhost:8000"), Some("cross-site")));
        assert!(!mutating_allowed(Some("http://evil.example"), None));
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
