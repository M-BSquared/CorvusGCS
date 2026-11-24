//! The small piece of XML Cursor on Target needs: elements, attributes, text
//! and the five predefined entities. It reads what a stranger on the network
//! sends, so it is bounded (size, depth, node count) and refuses a document
//! type declaration, which is where entity expansion attacks live.

pub const MAX_XML_BYTES: usize = 64 * 1024;
const MAX_DEPTH: usize = 12;
const MAX_NODES: usize = 400;

/// One element.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub text: String,
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(name: &str) -> Node {
        Node {
            name: name.to_string(),
            ..Node::default()
        }
    }

    /// Builder: one more attribute.
    pub fn attr(mut self, key: &str, value: impl Into<String>) -> Node {
        self.attrs.push((key.to_string(), value.into()));
        self
    }

    /// Builder: the text between the tags.
    pub fn with_text(mut self, text: &str) -> Node {
        self.text = text.to_string();
        self
    }

    /// Builder: one more child.
    pub fn child(mut self, child: Node) -> Node {
        self.children.push(child);
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn find(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    /// The document text of this element.
    pub fn to_xml(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        out.push('<');
        out.push_str(&self.name);
        for (k, v) in &self.attrs {
            out.push(' ');
            out.push_str(k);
            out.push_str("=\"");
            out.push_str(&escape(v));
            out.push('"');
        }
        if self.text.is_empty() && self.children.is_empty() {
            out.push_str("/>");
            return;
        }
        out.push('>');
        out.push_str(&escape(&self.text));
        for c in &self.children {
            c.write(out);
        }
        out.push_str("</");
        out.push_str(&self.name);
        out.push('>');
    }
}

/// `&`, `<`, `>` and both quotes as entities; control characters XML cannot
/// carry are dropped.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity.strip_prefix('#').and_then(|n| {
                let code = match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse().ok(),
                };
                code.and_then(char::from_u32)
            }),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
    nodes: usize,
}

/// The root element of `input`.
pub fn parse(input: &str) -> Result<Node, String> {
    if input.len() > MAX_XML_BYTES {
        return Err("the document is too large".into());
    }
    let mut p = Parser {
        s: input,
        i: 0,
        nodes: 0,
    };
    p.skip_misc()?;
    let root = p.element(0)?;
    p.skip_misc()?;
    if p.i < p.s.len() {
        return Err("text after the root element".into());
    }
    Ok(root)
}

impl Parser<'_> {
    fn rest(&self) -> &str {
        &self.s[self.i..]
    }

    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.i).copied()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn skip_to(&mut self, end: &str, what: &str) -> Result<(), String> {
        match self.rest().find(end) {
            Some(at) => {
                self.i += at + end.len();
                Ok(())
            }
            None => Err(format!("unterminated {what}")),
        }
    }

    /// Whitespace, the XML declaration and comments around the root.
    fn skip_misc(&mut self) -> Result<(), String> {
        loop {
            self.skip_ws();
            let rest = self.rest();
            if rest.starts_with("<?") {
                self.skip_to("?>", "declaration")?;
            } else if rest.starts_with("<!--") {
                self.skip_to("-->", "comment")?;
            } else if rest.starts_with("<!") {
                return Err("a document type declaration is not accepted".into());
            } else {
                return Ok(());
            }
        }
    }

    fn name(&mut self) -> Result<String, String> {
        let start = self.i;
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.'))
        {
            self.i += 1;
        }
        if self.i == start || self.i - start > 64 {
            return Err("bad name".into());
        }
        Ok(self.s[start..self.i].to_string())
    }

    fn element(&mut self, depth: usize) -> Result<Node, String> {
        if depth > MAX_DEPTH {
            return Err("nested too deep".into());
        }
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err("too many elements".into());
        }
        if self.peek() != Some(b'<') {
            return Err("expected an element".into());
        }
        self.i += 1;
        let mut node = Node::new(&self.name()?);
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'/') => {
                    if !self.rest().starts_with("/>") {
                        return Err("bad tag".into());
                    }
                    self.i += 2;
                    return Ok(node);
                }
                Some(b'>') => {
                    self.i += 1;
                    break;
                }
                Some(_) => {
                    let key = self.name()?;
                    self.skip_ws();
                    if self.peek() != Some(b'=') {
                        return Err("attribute without a value".into());
                    }
                    self.i += 1;
                    self.skip_ws();
                    let quote = match self.peek() {
                        Some(q @ (b'"' | b'\'')) => q as char,
                        _ => return Err("attribute value is not quoted".into()),
                    };
                    self.i += 1;
                    let len = self.rest().find(quote).ok_or("unterminated attribute")?;
                    let value = unescape(&self.s[self.i..self.i + len]);
                    self.i += len + 1;
                    if node.attrs.len() >= 64 {
                        return Err("too many attributes".into());
                    }
                    node.attrs.push((key, value));
                }
                None => return Err("unterminated tag".into()),
            }
        }
        loop {
            let rest = self.rest();
            if rest.starts_with("</") {
                self.i += 2;
                let close = self.name()?;
                self.skip_ws();
                if close != node.name || self.peek() != Some(b'>') {
                    return Err(format!("</{close}> does not close <{}>", node.name));
                }
                self.i += 1;
                return Ok(node);
            } else if rest.starts_with("<!--") {
                self.skip_to("-->", "comment")?;
            } else if rest.starts_with("<![CDATA[") {
                self.i += 9;
                let len = self.rest().find("]]>").ok_or("unterminated CDATA")?;
                node.text.push_str(&self.s[self.i..self.i + len]);
                self.i += len + 3;
            } else if rest.starts_with("<?") {
                self.skip_to("?>", "instruction")?;
            } else if rest.starts_with("<!") {
                return Err("a document type declaration is not accepted".into());
            } else if rest.starts_with('<') {
                let child = self.element(depth + 1)?;
                node.children.push(child);
            } else if rest.is_empty() {
                return Err(format!("<{}> is not closed", node.name));
            } else {
                let len = rest.find('<').unwrap_or(rest.len());
                node.text.push_str(&unescape(&rest[..len]));
                self.i += len;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_attributes_children_and_text() {
        let n = parse("<?xml version='1.0'?><a x=\"1\" y='2'><b/>hi &amp; bye<c k=\"&lt;\">t</c></a>").unwrap();
        assert_eq!(n.name, "a");
        assert_eq!(n.get("x"), Some("1"));
        assert_eq!(n.get("y"), Some("2"));
        assert_eq!(n.text, "hi & bye");
        assert_eq!(n.children.len(), 2);
        assert_eq!(n.find("c").unwrap().get("k"), Some("<"));
        assert_eq!(n.find("c").unwrap().text, "t");
    }

    #[test]
    fn writes_what_it_reads() {
        let n = Node::new("a")
            .attr("q", "\"x\" & <y>")
            .child(Node::new("b").with_text("1 < 2"))
            .child(Node::new("c"));
        let xml = n.to_xml();
        assert_eq!(xml, "<a q=\"&quot;x&quot; &amp; &lt;y&gt;\"><b>1 &lt; 2</b><c/></a>");
        assert_eq!(parse(&xml).unwrap(), n);
    }

    #[test]
    fn numeric_entities_decode_and_bad_ones_stay_text() {
        assert_eq!(
            parse("<a>&#65;&#x42;&bogus;&#xD800;</a>").unwrap().text,
            "AB&bogus;&#xD800;"
        );
    }

    #[test]
    fn refuses_what_a_stranger_could_abuse() {
        assert!(parse("<!DOCTYPE a [<!ENTITY x 'y'>]><a>&x;</a>").is_err());
        assert!(parse("<a><!DOCTYPE b></a>").is_err());
        let deep = format!("{}{}", "<a>".repeat(40), "</a>".repeat(40));
        assert!(parse(&deep).is_err());
        let wide = format!("<a>{}</a>", "<b/>".repeat(500));
        assert!(parse(&wide).is_err());
        assert!(parse(&format!("<a>{}</a>", "x".repeat(MAX_XML_BYTES))).is_err());
    }

    #[test]
    fn damaged_input_is_an_error_not_a_panic() {
        for bad in [
            "",
            "<",
            "<a",
            "<a b>",
            "<a b=1/>",
            "<a></b>",
            "<a>",
            "<a/>x",
            "<a b=\"x/>",
            "<a><![CDATA[x</a>",
            "<é/>",
            "<a>\u{1F600}</b>",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(parse("<a>\u{1F600}é</a>").unwrap().text, "\u{1F600}é");
    }
}
