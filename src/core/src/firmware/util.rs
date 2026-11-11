//! The pieces of Python's standard library the firmware code leans on.

use serde_json::Value;

/// `str.isspace()` for one character: Unicode white space, and `\x1c` to `\x1f`.
pub fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// `str.split()` with no argument.
pub fn py_split(s: &str) -> Vec<&str> {
    s.split(py_space).filter(|p| !p.is_empty()).collect()
}

/// `str.title()`: the first letter of every run of letters upper case, the rest lower.
pub fn py_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_word = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            if in_word {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            in_word = true;
        } else {
            in_word = false;
            out.push(c);
        }
    }
    out
}

/// `urllib.parse.quote(text, safe="")`.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `urllib.parse.unquote(text)`: `%xx` runs decoded as UTF-8, bad bytes replaced.
pub fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// What `urlparse` finds in a URL.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
}

impl Url {
    /// `urlparse(url).hostname`, lower case; `None` where Python has none.
    pub fn hostname(&self) -> Option<String> {
        let (_, _, hostinfo) = rpartition(&self.netloc, '@');
        let host = if let Some(rest) = hostinfo.strip_prefix('[') {
            rest.split(']').next().unwrap_or("").to_string()
        } else {
            hostinfo.split(':').next().unwrap_or("").to_string()
        };
        if host.is_empty() {
            None
        } else {
            Some(host.to_lowercase())
        }
    }
}

fn rpartition(s: &str, sep: char) -> (&str, &str, &str) {
    match s.rfind(sep) {
        Some(i) => (&s[..i], &s[i..i + sep.len_utf8()], &s[i + sep.len_utf8()..]),
        None => ("", "", s),
    }
}

/// `urllib.parse.urlparse(url)` as far as the scheme, the host and the path go;
/// `None` where Python raises `ValueError`.
pub fn urlparse(url: &str) -> Option<Url> {
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let cleaned: String = url.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut rest: &str = &cleaned;
    let mut scheme = String::new();
    if let Some(i) = rest.find(':') {
        let head = &rest[..i];
        let first = head.chars().next();
        if i > 0
            && first.is_some_and(|c| c.is_ascii_alphabetic())
            && head
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            scheme = head.to_lowercase();
            rest = &rest[i + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..end].to_string();
        rest = &after[end..];
        if (netloc.contains('[') && !netloc.contains(']')) || (netloc.contains(']') && !netloc.contains('[')) {
            return None;
        }
    }
    let rest = rest.split('#').next().unwrap_or("");
    let rest = rest.split('?').next().unwrap_or("");
    let path = match if rest.contains('/') {
        rest.rfind('/')
            .and_then(|slash| rest[slash..].find(';').map(|p| p + slash))
    } else {
        rest.find(';')
    } {
        Some(i) => &rest[..i],
        None => rest,
    };
    Some(Url {
        scheme,
        netloc,
        path: path.to_string(),
    })
}

/// `str(value or "")` for the JSON values a catalogue holds.
pub fn text_or_empty(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => String::new(),
        Some(Value::Number(n)) => {
            if n.as_f64() == Some(0.0) {
                String::new()
            } else if let Some(f) = n.as_f64().filter(|_| !n.is_i64() && !n.is_u64()) {
                crate::logs::pytext::py_float_repr(f)
            } else {
                n.to_string()
            }
        }
        Some(other) => other.to_string(),
    }
}

/// `int(value or 0)`: the text Python's `ValueError` or `TypeError` carries on failure.
pub fn int_or_zero(v: Option<&Value>) -> Result<i64, String> {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(0),
        Some(Value::Bool(true)) => Ok(1),
        Some(Value::Number(n)) => Ok(n.as_i64().unwrap_or_else(|| n.as_f64().map_or(0, |f| f as i64))),
        Some(Value::String(s)) => {
            if s.is_empty() {
                return Ok(0);
            }
            let t = py_strip(s);
            let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                t.parse::<i64>().map_err(|e| e.to_string())
            } else {
                Err(format!("invalid literal for int() with base 10: '{s}'"))
            }
        }
        Some(Value::Array(a)) if a.is_empty() => Ok(0),
        Some(Value::Object(o)) if o.is_empty() => Ok(0),
        Some(other) => Err(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            if other.is_array() { "list" } else { "dict" }
        )),
    }
}

/// `str(value)` for a JSON value that may be missing (`dict.get`).
pub fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => match n.as_f64().filter(|_| !n.is_i64() && !n.is_u64()) {
            Some(f) => crate::logs::pytext::py_float_repr(f),
            None => n.to_string(),
        },
        Some(other) => other.to_string(),
    }
}

/// Whether a JSON value is truthy as Python reads it.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Bool(true)) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_follows_python() {
        assert_eq!(py_title("sky-drones"), "Sky-Drones");
        assert_eq!(py_title("xc fly"), "Xc Fly");
        assert_eq!(py_title("3dr"), "3Dr");
        assert_eq!(py_title("UPPER"), "Upper");
    }

    #[test]
    fn quote_leaves_only_the_unreserved() {
        assert_eq!(quote("a b/c~d.e_f-g"), "a%20b%2Fc~d.e_f-g");
        assert_eq!(quote("\u{e4}"), "%C3%A4");
    }

    #[test]
    fn unquote_decodes_what_is_there() {
        assert_eq!(unquote("a%20b%2e%2e%"), "a b..%");
        assert_eq!(unquote("%41%4"), "A%4");
        assert_eq!(unquote("%zz"), "%zz");
    }

    #[test]
    fn urlparse_reads_scheme_host_and_path() {
        let u = urlparse("HTTPS://User@GitHub.com:443/a/b;p?q#f").unwrap();
        assert_eq!(
            (u.scheme.as_str(), u.hostname().as_deref(), u.path.as_str()),
            ("https", Some("github.com"), "/a/b")
        );
        assert_eq!(urlparse("https:github.com/x").unwrap().hostname(), None);
        assert!(urlparse("https://[::1/x").is_none());
    }
}
