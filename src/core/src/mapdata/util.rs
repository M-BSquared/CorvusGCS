//! Small helpers that spell Python's own behaviour: `int()` and `float()` of a
//! text, `urllib.parse.quote`, `round()` and the like, so a query string or a
//! JSON value is read the same way the Python routes read it.

use serde_json::Value;

fn strip_underscores(s: &str) -> Option<String> {
    // PEP 515: an underscore only between two digits.
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in b.iter().enumerate() {
        if c == b'_' {
            let before = i > 0 && b[i - 1].is_ascii_digit();
            let after = i + 1 < b.len() && b[i + 1].is_ascii_digit();
            if !(before && after) {
                return None;
            }
        } else {
            out.push(c as char);
        }
    }
    Some(out)
}

/// `int(text.strip())`.
pub fn py_int(text: &str) -> Option<i64> {
    let t = text.trim();
    let t = strip_underscores(t)?;
    let digits = t.strip_prefix(['+', '-']).unwrap_or(&t);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    t.parse::<i64>().ok()
}

/// `float(text)`: the text may carry spaces, a sign, an exponent, `inf` and
/// `nan` in any case, and single underscores between digits.
pub fn py_float(text: &str) -> Option<f64> {
    let t = text.trim();
    let t = strip_underscores(t)?;
    let lower = t.to_ascii_lowercase();
    let body = lower.strip_prefix(['+', '-']).unwrap_or(&lower);
    if matches!(body, "inf" | "infinity" | "nan") {
        return lower.parse::<f64>().ok();
    }
    if body.is_empty()
        || !body
            .bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'+' | b'-'))
    {
        return None;
    }
    // Rust accepts "1.", ".5" and "1e5" as Python does.
    t.parse::<f64>().ok()
}

/// `float(value)` of a JSON value as Python would take it.
pub fn json_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => py_float(s),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

/// Python truthiness of a JSON value (`a or b`).
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `float(value)` that is finite, `None` otherwise (`geocode._finite`).
pub fn finite(v: &Value) -> Option<f64> {
    json_float(v).filter(|x| x.is_finite())
}

/// Python's `round(x)`: halves go to the even neighbour.
pub fn round_half_even(x: f64) -> i64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
        (r - x.signum()) as i64
    } else {
        r as i64
    }
}

fn unreserved(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-' | b'~')
}

/// `urllib.parse.quote(text, safe="")`.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for &b in text.as_bytes() {
        if unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `urllib.parse.quote_plus`, which `urlencode` uses.
pub fn quote_plus(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for &b in text.as_bytes() {
        if b == b' ' {
            out.push('+');
        } else if unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())` for the seconds since
/// the epoch.
pub fn iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The seconds since the epoch, now.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `int(value)` of a JSON value as Python would take it (`None` where Python
/// would raise).
pub fn json_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => match n.as_i64() {
            Some(i) => Some(i),
            None => {
                let f = n.as_f64()?;
                (f.is_finite() && f.abs() < 9.2e18).then(|| f.trunc() as i64)
            }
        },
        Value::String(s) => py_int(s),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

/// `str(value)` of a JSON value (`None`, `True`, `1.5`, ...).
pub fn json_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => match n.as_f64() {
                Some(f) if f.fract() == 0.0 && f.abs() < 1e16 => format!("{f:.1}"),
                Some(f) => format!("{f}"),
                None => n.to_string(),
            },
        },
        other => other.to_string(),
    }
}

/// `os.path.expanduser` for a leading `~`.
pub fn expand_user(path: &str) -> String {
    if path == "~" || path.starts_with("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        if !home.is_empty() {
            return format!("{}{}", home.trim_end_matches('/'), &path[1..]);
        }
    }
    path.to_string()
}

/// `os.path.normpath`: `.` and `..` and doubled separators folded lexically.
pub fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let rooted = path.starts_with('/');
    let lead = if path.starts_with("//") && !path.starts_with("///") {
        "//"
    } else if rooted {
        "/"
    } else {
        ""
    };
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|l| *l != "..") {
                    out.pop();
                } else if !rooted {
                    out.push("..");
                }
            }
            p => out.push(p),
        }
    }
    let joined = format!("{lead}{}", out.join("/"));
    if joined.is_empty() {
        ".".into()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_and_floats_read_like_python() {
        assert_eq!(py_int(" 12 "), Some(12));
        assert_eq!(py_int("+7"), Some(7));
        assert_eq!(py_int("1_000"), Some(1000));
        assert_eq!(py_int("1__0"), None);
        assert_eq!(py_int("1.5"), None);
        assert_eq!(py_int(""), None);
        assert_eq!(py_float("1e3"), Some(1000.0));
        assert_eq!(py_float(" -2.5 "), Some(-2.5));
        assert_eq!(py_float("Infinity"), Some(f64::INFINITY));
        assert!(py_float("nan").unwrap().is_nan());
        assert_eq!(py_float("abc"), None);
        assert_eq!(py_float("1_0.5"), Some(10.5));
        assert_eq!(py_float("e"), None);
    }

    #[test]
    fn rounding_goes_to_even() {
        assert_eq!(round_half_even(0.5), 0);
        assert_eq!(round_half_even(1.5), 2);
        assert_eq!(round_half_even(2.5), 2);
        assert_eq!(round_half_even(-0.5), 0);
        assert_eq!(round_half_even(-1.5), -2);
        assert_eq!(round_half_even(2.4), 2);
    }

    #[test]
    fn quoting_matches_urllib() {
        assert_eq!(quote("a b/&é"), "a%20b%2F%26%C3%A9");
        assert_eq!(quote_plus("a b/&é"), "a+b%2F%26%C3%A9");
        assert_eq!(quote("A-z_0.9~"), "A-z_0.9~");
    }

    #[test]
    fn paths_are_folded() {
        assert_eq!(normpath("/a/b/../c//d/./"), "/a/c/d");
        assert_eq!(normpath("a/../.."), "..");
        assert_eq!(normpath("/.."), "/");
        assert_eq!(normpath(""), ".");
    }

    #[test]
    fn json_values_read_like_python() {
        use serde_json::json;
        assert_eq!(json_int(&json!(5.9)), Some(5));
        assert_eq!(json_int(&json!("7")), Some(7));
        assert_eq!(json_int(&json!(null)), None);
        assert_eq!(json_str(&json!(null)), "None");
        assert_eq!(json_str(&json!(1.0)), "1.0");
        assert_eq!(json_str(&json!(true)), "True");
    }

    #[test]
    fn iso_time() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
