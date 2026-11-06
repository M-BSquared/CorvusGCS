//! The web interface's files (`_serve_static`), and the dates HTTP writes.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::request::{unquote, Request};
use crate::response::Response;

/// The content type of every file this server hands out, fixed rather than
/// asked of the host: a registry that says `.js` is `text/plain` would, with
/// `nosniff`, make the browser refuse every script on that one machine.
pub fn content_type(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "html" => "text/html",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/vnd.microsoft.icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "xml" => "text/xml",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `Fri, 06 Nov 2026 15:00:00 GMT`, the date of `formatdate(usegmt=True)`.
pub fn http_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{}, {d:02} {} {y:04} {:02}:{:02}:{:02} GMT",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize],
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Seconds of an HTTP date (`Fri, 06 Nov 2026 15:00:00 GMT`), or `None`.
pub fn parse_http_date(text: &str) -> Option<i64> {
    let t = text.trim();
    let t = t.split_once(", ").map_or(t, |(_, r)| r);
    let parts: Vec<&str> = t.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let d: i64 = parts[0].parse().ok()?;
    let m = MONTHS.iter().position(|n| n.eq_ignore_ascii_case(parts[1]))? as i64 + 1;
    let y: i64 = parts[2].parse().ok()?;
    let hms: Vec<i64> = parts[3].split(':').filter_map(|p| p.parse().ok()).collect();
    if hms.len() != 3 {
        return None;
    }
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hms[0] * 3600 + hms[1] * 60 + hms[2])
}

/// Does the client already hold this exact version? `If-None-Match` wins
/// outright when present (compared weakly, as a GET may); only without it does
/// the date get a say, at one second resolution, which is why the ETag
/// carries nanoseconds and the size.
pub fn unchanged(if_none_match: Option<&str>, if_modified_since: Option<&str>, etag: &str, mtime_secs: i64) -> bool {
    if let Some(inm) = if_none_match.map(str::trim).filter(|s| !s.is_empty()) {
        return inm
            .split(',')
            .map(str::trim)
            .map(|p| p.strip_prefix("W/").unwrap_or(p))
            .any(|c| c == "*" || c == etag);
    }
    match if_modified_since.map(str::trim).filter(|s| !s.is_empty()) {
        Some(ims) => parse_http_date(ims).is_some_and(|since| mtime_secs <= since),
        None => false,
    }
}

/// `path` under `root`, or `None` when it climbs out. Percent-decoded first
/// (so `%2e%2e%2f` means `../`), joined, resolved, and only then checked:
/// the containment check is what makes the decoding safe.
pub fn resolve(root: &Path, url_path: &str) -> Option<PathBuf> {
    let url_path = if url_path == "/" { "/index.html" } else { url_path };
    let decoded = unquote(url_path);
    if decoded.contains('\0') {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let joined = root.join(decoded.trim_start_matches('/'));
    match joined.canonicalize() {
        Ok(p) => p.starts_with(&root).then_some(p),
        // A file that is not there resolves to nothing; whether it would have
        // been outside the root is told by its parent.
        Err(_) => {
            let ok = joined
                .components()
                .all(|c| !matches!(c, std::path::Component::ParentDir));
            ok.then_some(joined)
        }
    }
}

pub fn serve(root: Option<&Path>, req: &Request) -> Response {
    let Some(root) = root else {
        return Response::error(404, "not found");
    };
    let Some(file) = resolve(root, &req.path) else {
        return Response::error(403, "forbidden");
    };
    if !file.is_file() {
        return Response::error(404, "not found");
    }
    let Ok(meta) = file.metadata() else {
        return Response::error(404, "not found");
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    let etag = format!("\"{:x}-{:x}\"", mtime.as_nanos(), meta.len());
    let last_modified = http_date(mtime.as_secs() as i64);
    if unchanged(
        req.headers.get("If-None-Match"),
        req.headers.get("If-Modified-Since"),
        &etag,
        mtime.as_secs() as i64,
    ) {
        return Response::empty(304)
            .with_header("ETag", &etag)
            .with_header("Last-Modified", &last_modified)
            .with_header("Cache-Control", "no-cache");
    }
    match std::fs::read(&file) {
        Ok(body) => Response::bytes(200, content_type(&file.to_string_lossy()), body)
            .with_header("Cache-Control", "no-cache")
            .with_header("ETag", &etag)
            .with_header("Last-Modified", &last_modified),
        Err(_) => Response::error(404, "not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::Method;

    #[test]
    fn dates_are_rfc_1123() {
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(1_700_000_000), "Tue, 14 Nov 2023 22:13:20 GMT");
        assert_eq!(parse_http_date("Tue, 14 Nov 2023 22:13:20 GMT"), Some(1_700_000_000));
        assert_eq!(parse_http_date(&http_date(951_782_400)), Some(951_782_400));
        assert_eq!(parse_http_date("yesterday"), None);
    }

    #[test]
    fn validators_follow_the_python_order() {
        let etag = "\"1a-2b\"";
        assert!(unchanged(Some("\"1a-2b\""), None, etag, 100));
        assert!(unchanged(Some("W/\"1a-2b\", \"x\""), None, etag, 100));
        assert!(unchanged(Some("*"), None, etag, 100));
        assert!(
            !unchanged(Some("\"other\""), Some(&http_date(1000)), etag, 100),
            "ETag wins over the date"
        );
        assert!(unchanged(None, Some(&http_date(100)), etag, 100));
        assert!(!unchanged(None, Some(&http_date(99)), etag, 100));
        assert!(!unchanged(None, Some("junk"), etag, 100));
        assert!(!unchanged(None, None, etag, 100));
    }

    #[test]
    fn a_path_cannot_leave_the_root() {
        let root = std::env::temp_dir().join(format!("corvus-core-static-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("js")).unwrap();
        std::fs::write(root.join("index.html"), "<html>").unwrap();
        std::fs::write(root.join("js/my file.js"), "x").unwrap();
        std::fs::write(
            root.parent()
                .unwrap()
                .join(format!("corvus-core-secret-{}", std::process::id())),
            "s",
        )
        .unwrap();
        assert!(resolve(&root, "/").unwrap().ends_with("index.html"));
        assert!(resolve(&root, "/js/my%20file.js").unwrap().ends_with("my file.js"));
        let secret = format!("corvus-core-secret-{}", std::process::id());
        assert!(resolve(&root, &format!("/../{secret}")).is_none());
        assert!(resolve(&root, &format!("/%2e%2e%2f{secret}")).is_none());
        assert!(resolve(&root, "/a%00b").is_none());
        let got = |p: &str| serve(Some(&root), &Request::new(Method::Get, p));
        assert_eq!(got("/").status, 200);
        assert_eq!(got("/").header("content-type"), Some("text/html"));
        assert_eq!(got("/js/my%20file.js").header("content-type"), Some("text/javascript"));
        assert_eq!(got("/nope.css").status, 404);
        assert_eq!(got(&format!("/../{secret}")).status, 403);
        assert_eq!(serve(None, &Request::new(Method::Get, "/")).status, 404);
        let etag = got("/").header("etag").unwrap().to_string();
        let mut again = Request::new(Method::Get, "/");
        again.headers.push("If-None-Match", &etag);
        let r = serve(Some(&root), &again);
        assert_eq!((r.status, r.header("cache-control")), (304, Some("no-cache")));
        std::fs::remove_file(root.parent().unwrap().join(secret)).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn cases() -> serde_json::Value {
        serde_json::from_str(include_str!("../../tests/data/http_cases.json")).unwrap()
    }

    #[test]
    fn content_types_dates_and_validators_are_the_pythons() {
        let c = cases();
        for (name, want) in c["content_types"].as_object().unwrap() {
            assert_eq!(content_type(name), want.as_str().unwrap(), "{name}");
        }
        for (secs, want) in c["http_date"].as_object().unwrap() {
            assert_eq!(http_date(secs.parse().unwrap()), want.as_str().unwrap());
        }
        for u in c["unchanged"].as_array().unwrap() {
            let got = unchanged(
                u["if_none_match"].as_str(),
                u["if_modified_since"].as_str(),
                u["etag"].as_str().unwrap(),
                u["mtime"].as_f64().unwrap() as i64,
            );
            assert_eq!(got, u["result"].as_bool().unwrap(), "{u}");
        }
    }
}
