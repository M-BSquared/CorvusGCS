//! Just enough HTML for the manual and the website checks: escaping as the
//! frame writes it, and a scanner for tags, ids, links and the prose between
//! them.

/// `&`, `<`, `>`, `"` and `'` as entities, as the frame of the manual writes
/// them.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

/// What a page holds: every `id`, every local `href`, `src` and `srcset`
/// target, and the text outside `code`, `pre`, `script`, `style` and `kbd`.
#[derive(Default, Debug)]
pub struct Scan {
    pub ids: Vec<String>,
    pub refs: Vec<String>,
    pub prose: String,
}

fn attrs(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = tag.as_bytes();
    let mut i = 0;
    // Skip the tag name.
    while i < b.len() && !b[i].is_ascii_whitespace() {
        i += 1;
    }
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        let name = tag[start..i].to_ascii_lowercase();
        if name.is_empty() {
            break;
        }
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            let value = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                let vs = i + 1;
                i = vs;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                let v = &tag[vs..i.min(b.len())];
                i += 1;
                v
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                &tag[vs..i]
            };
            out.push((name, unescape(value)));
        } else {
            out.push((name, String::new()));
        }
    }
    out
}

/// The common entities back to their characters.
pub fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", "\u{a0}")
        .replace("&mdash;", "\u{2014}")
        .replace("&ndash;", "\u{2013}")
        .replace("&amp;", "&")
}

/// Scans `html`.
pub fn scan(html: &str) -> Scan {
    const QUIET: &[&str] = &["code", "pre", "script", "style", "kbd"];
    let mut s = Scan::default();
    let mut quiet = 0usize;
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        if quiet == 0 {
            s.prose.push_str(&unescape(&rest[..lt]));
            s.prose.push(' ');
        }
        rest = &rest[lt..];
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |e| &rest[e + 3..]);
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        if let Some(name) = tag.strip_prefix('/') {
            if QUIET.contains(&name.trim().to_ascii_lowercase().as_str()) {
                quiet = quiet.saturating_sub(1);
            }
            continue;
        }
        let name = tag
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_ascii_lowercase();
        for (k, v) in attrs(tag) {
            match k.as_str() {
                "id" if !v.is_empty() => s.ids.push(v),
                "href" | "src" if !v.is_empty() => s.refs.push(v),
                "srcset" => s.refs.extend(
                    v.split(',')
                        .filter_map(|p| p.split_whitespace().next())
                        .map(str::to_string),
                ),
                _ => {}
            }
        }
        if QUIET.contains(&name.as_str()) && !tag.ends_with('/') {
            quiet += 1;
        }
    }
    if quiet == 0 {
        s.prose.push_str(&unescape(rest));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_is_the_frames() {
        assert_eq!(
            escape(r#"A & B <"c"> 'd'"#),
            "A &amp; B &lt;&quot;c&quot;&gt; &#x27;d&#x27;"
        );
    }

    #[test]
    fn a_page_is_scanned_for_ids_links_and_prose() {
        let s = scan(
            r#"<h2 id="a">Title &amp; more</h2><!-- <a href="no"> --><p>Text <code>x - y</code> and <a href='b.html#c'>link</a></p>
               <img src="i.jpg" srcset="i-800.jpg 800w, i.jpg 1600w"><pre>-- quiet</pre> end"#,
        );
        assert_eq!(s.ids, ["a"]);
        assert_eq!(s.refs, ["b.html#c", "i.jpg", "i-800.jpg", "i.jpg"]);
        assert!(s.prose.contains("Title & more") && s.prose.contains("link") && s.prose.contains("end"));
        assert!(!s.prose.contains("x - y") && !s.prose.contains("quiet"));
    }
}
