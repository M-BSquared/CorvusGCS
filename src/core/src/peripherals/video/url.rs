//! The part of `urllib.parse` the camera code reads addresses with:
//! `urlsplit`, `urlunsplit`, `.hostname` and `.port`, with Python's answers
//! (including the cases where it raises), so an address is accepted or refused
//! exactly as it was.

use std::net::Ipv6Addr;

/// `SplitResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

/// Raised where Python raises `ValueError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invalid;

const USES_NETLOC: [&str; 24] = [
    "", "ftp", "http", "gopher", "nntp", "telnet", "imap", "wais", "file", "mms", "https", "shttp", "snews",
    "prospero", "rtsp", "rtspu", "rsync", "svn", "svn+ssh", "sftp", "nfs", "git", "git+ssh", "ws",
];

fn scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

fn check_bracketed(host: &str) -> Result<(), Invalid> {
    if let Some(rest) = host.strip_prefix('v') {
        let hex = rest.chars().take_while(char::is_ascii_hexdigit).count();
        if hex == 0 || !rest[hex..].starts_with('.') || rest[hex + 1..].is_empty() {
            return Err(Invalid);
        }
        return Ok(());
    }
    let bare = host.split('%').next().unwrap_or("");
    if host.contains('%') && host.ends_with('%') {
        return Err(Invalid);
    }
    bare.parse::<Ipv6Addr>().map(|_| ()).map_err(|_| Invalid)
}

/// `urlsplit(url)`.
pub fn urlsplit(url: &str) -> Result<Split, Invalid> {
    let trimmed = url.trim_start_matches(|c: char| c <= ' ');
    let cleaned: String = trimmed.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut url = cleaned.as_str();
    let mut scheme = String::new();
    if let Some(i) = url.find(':') {
        let head = &url[..i];
        if i > 0 && head.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && head.chars().all(scheme_char) {
            scheme = head.to_ascii_lowercase();
            url = &url[i + 1..];
        }
    }
    let mut netloc = "";
    if let Some(rest) = url.strip_prefix("//") {
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        netloc = &rest[..end];
        url = &rest[end..];
        let open = netloc.contains('[');
        let close = netloc.contains(']');
        if open != close {
            return Err(Invalid);
        }
        if open {
            let inner = netloc.split_once('[').map_or("", |(_, r)| r);
            let inner = inner.split_once(']').map_or(inner, |(l, _)| l);
            check_bracketed(inner)?;
        }
    }
    let mut fragment = "";
    if let Some((l, r)) = url.split_once('#') {
        url = l;
        fragment = r;
    }
    let mut query = "";
    if let Some((l, r)) = url.split_once('?') {
        url = l;
        query = r;
    }
    Ok(Split {
        scheme,
        netloc: netloc.to_string(),
        path: url.to_string(),
        query: query.to_string(),
        fragment: fragment.to_string(),
    })
}

/// `urlunsplit`.
pub fn urlunsplit(s: &Split) -> String {
    let mut url = s.path.clone();
    if !s.netloc.is_empty()
        || (!s.scheme.is_empty() && USES_NETLOC.contains(&s.scheme.as_str()) && !url.starts_with("//"))
    {
        if !url.is_empty() && !url.starts_with('/') {
            url.insert(0, '/');
        }
        url = format!("//{}{}", s.netloc, url);
    }
    if !s.scheme.is_empty() {
        url = format!("{}:{}", s.scheme, url);
    }
    if !s.query.is_empty() {
        url = format!("{url}?{}", s.query);
    }
    if !s.fragment.is_empty() {
        url = format!("{url}#{}", s.fragment);
    }
    url
}

impl Split {
    /// `_hostinfo`: the host as written and the port text, if any.
    fn hostinfo(&self) -> (String, Option<String>) {
        let hostinfo = self.netloc.rsplit_once('@').map_or(self.netloc.as_str(), |(_, h)| h);
        let (host, port) = if let Some((_, bracketed)) = hostinfo.split_once('[') {
            let (host, rest) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            let port = rest.split_once(':').map_or("", |(_, p)| p);
            (host, port)
        } else {
            hostinfo.split_once(':').unwrap_or((hostinfo, ""))
        };
        (
            host.to_string(),
            if port.is_empty() { None } else { Some(port.to_string()) },
        )
    }

    /// `.hostname`.
    pub fn hostname(&self) -> Option<String> {
        let (host, _) = self.hostinfo();
        if host.is_empty() {
            return None;
        }
        Some(match host.split_once('%') {
            Some((h, zone)) => format!("{}%{zone}", h.to_lowercase()),
            None => host.to_lowercase(),
        })
    }

    /// `.port`; `Err` where Python raises.
    pub fn port(&self) -> Result<Option<u16>, Invalid> {
        let (_, port) = self.hostinfo();
        let Some(port) = port else { return Ok(None) };
        if !port.chars().all(|c| c.is_ascii_digit()) {
            return Err(Invalid);
        }
        let digits = port.trim_start_matches('0');
        if digits.len() > 5 {
            return Err(Invalid);
        }
        let n: u32 = if digits.is_empty() {
            0
        } else {
            digits.parse().map_err(|_| Invalid)?
        };
        u16::try_from(n).map(Some).map_err(|_| Invalid)
    }
}
