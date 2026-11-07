//! The text side of the NSH shell: what a SERIAL_CONTROL stream becomes. A
//! character split across 70 byte chunks survives, lines are assembled with
//! carriage returns dropped and backspaces applied, and a replay backlog is
//! trimmed at a line break. Plain Rust, shared by the Python engine
//! (`protocol::shell`) and `corvus_core::protocol::shell`.

pub const CHUNK: usize = 70;

/// Python's incremental UTF-8 decoder with `errors="replace"`: bytes of a
/// character split across chunks wait for the rest; invalid ones become
/// U+FFFD, one per maximal invalid subpart.
#[derive(Default)]
pub struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    pub fn decode(&mut self, input: &[u8]) -> String {
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(input);
        let mut out = String::new();
        let mut rest: &[u8] = &bytes;
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    out.push_str(text);
                    break;
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    out.push_str(std::str::from_utf8(&rest[..valid]).unwrap());
                    match err.error_len() {
                        Some(bad) => {
                            out.push('\u{FFFD}');
                            rest = &rest[valid + bad..];
                        }
                        None => {
                            self.pending = rest[valid..].to_vec();
                            break;
                        }
                    }
                }
            }
        }
        out
    }

    pub fn reset(&mut self) {
        self.pending.clear();
    }
}

/// The console's view of a chunk: CR dropped, BS applied (to this chunk, or
/// to the buffer's tail when it crosses a chunk boundary), complete lines
/// returned, and a buffer that ran past `max_chars` flushed as a line.
pub fn assemble_lines(buffer: &mut String, text: &str, max_chars: usize) -> Vec<String> {
    let mut chunk = String::new();
    for ch in text.chars() {
        match ch {
            '\r' => {}
            '\u{8}' => {
                if chunk.pop().is_none() {
                    buffer.pop();
                }
            }
            other => chunk.push(other),
        }
    }
    buffer.push_str(&chunk);
    let mut lines = Vec::new();
    while let Some(at) = buffer.find('\n') {
        let line: String = buffer[..at].to_string();
        *buffer = buffer[at + 1..].to_string();
        if !line.is_empty() {
            lines.push(line);
        }
    }
    if buffer.chars().count() >= max_chars {
        lines.push(std::mem::take(buffer));
    }
    lines
}

/// Keep the newest `max_chars` characters of the backlog, cut at a line
/// break when one is in reach, so a replay does not open on half an escape.
pub fn trim_backlog(backlog: &str, max_chars: usize) -> String {
    let total = backlog.chars().count();
    if total <= max_chars {
        return backlog.to_string();
    }
    let cut_chars = total - max_chars;
    let cut_byte = backlog
        .char_indices()
        .nth(cut_chars)
        .map(|(i, _)| i)
        .unwrap_or(backlog.len());
    match backlog[cut_byte..].find('\n') {
        Some(nl) => backlog[cut_byte + nl + 1..].to_string(),
        None => backlog[cut_byte..].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_character_split_across_chunks_survives() {
        let mut d = Utf8Stream::default();
        let euro = "€".as_bytes();
        assert_eq!(d.decode(&euro[..1]), "");
        assert_eq!(d.decode(&euro[1..]), "€");
        assert_eq!(d.decode(b"ok\xff!"), "ok\u{FFFD}!");
    }

    #[test]
    fn the_console_gets_whole_lines_without_cr_and_with_backspaces_applied() {
        let mut buffer = String::from("lis");
        let lines = assemble_lines(&mut buffer, "\u{8}st\r\nnsh> top\u{8}\u{8}p\n\npart", 8192);
        assert_eq!(lines, vec!["list".to_string(), "nsh> tp".to_string()]);
        assert_eq!(buffer, "part");
    }

    #[test]
    fn a_line_without_end_is_flushed_at_the_cap() {
        let mut buffer = String::new();
        let lines = assemble_lines(&mut buffer, &"x".repeat(12), 10);
        assert_eq!(lines, vec!["x".repeat(12)]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn the_backlog_keeps_the_newest_text_from_a_line_break() {
        assert_eq!(trim_backlog("short", 10), "short");
        assert_eq!(trim_backlog("aaaa\nbbbb\ncc", 7), "cc");
        assert_eq!(trim_backlog("abcdefghij", 4), "ghij");
    }
}
