//! Server-sent events, parsed line by line.
//!
//! Only what the Corvus server sends is needed (`event:` and `data:` lines,
//! a blank line ending each event), but the parser follows the spec for the
//! rest so a comment, a `data:` split over several lines or a CRLF line end
//! does not turn into a wrong event.

/// The longest line, and the most data of one event, taken in: a stream that
/// never ends a line or an event is dropped and followed again, not buffered.
pub const LINE_MAX: usize = 16 * 1024 * 1024;
const EVENT_MAX: usize = 16 * 1024 * 1024;

/// One event off the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The `event:` name, `"message"` when the server gave none.
    pub name: String,
    /// The `data:` lines joined with `\n`.
    pub data: String,
}

/// Collects lines into events.
#[derive(Debug, Default)]
pub struct Parser {
    name: String,
    data: Vec<String>,
    size: usize,
    over: bool,
}

impl Parser {
    /// Feeds one line without its line end; returns an event when the line
    /// completes one.
    pub fn line(&mut self, line: &str) -> Option<Event> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            let over = std::mem::take(&mut self.over);
            self.size = 0;
            if self.data.is_empty() || over {
                self.name.clear();
                self.data.clear();
                return None;
            }
            let name = if self.name.is_empty() {
                "message".to_string()
            } else {
                std::mem::take(&mut self.name)
            };
            let data = std::mem::take(&mut self.data).join("\n");
            return Some(Event { name, data });
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.name = value.to_string(),
            "data" if !self.over => {
                self.size += value.len();
                if self.size > EVENT_MAX {
                    self.over = true;
                    self.data.clear();
                } else {
                    self.data.push(value.to_string());
                }
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(text: &str) -> Vec<Event> {
        let mut p = Parser::default();
        text.split('\n').filter_map(|l| p.line(l)).collect()
    }

    #[test]
    fn an_event_past_the_size_limit_is_dropped_and_the_next_is_read() {
        let mut p = Parser::default();
        let big = "x".repeat(1024 * 1024);
        let mut got = None;
        for _ in 0..20 {
            got = p.line(&format!("data: {big}")).or(got);
        }
        assert!(p.line("").is_none(), "the oversized event is not delivered");
        assert!(p.data.is_empty());
        p.line("data: ok");
        assert_eq!(p.line("").map(|e| e.data), Some("ok".into()));
    }

    #[test]
    fn a_named_event_with_one_data_line() {
        let events = feed("event: state\ndata: {\"connected\": true}\n\n");
        assert_eq!(
            events,
            vec![Event {
                name: "state".into(),
                data: "{\"connected\": true}".into()
            }]
        );
    }

    #[test]
    fn data_over_several_lines_is_joined_and_crlf_is_dropped() {
        let events = feed("data: a\r\ndata:b\r\n\r\n");
        assert_eq!(
            events,
            vec![Event {
                name: "message".into(),
                data: "a\nb".into()
            }]
        );
    }

    #[test]
    fn comments_and_unknown_fields_are_ignored() {
        let events = feed(": keepalive\nid: 4\nretry: 10\nevent: ping\ndata: {}\n\n");
        assert_eq!(
            events,
            vec![Event {
                name: "ping".into(),
                data: "{}".into()
            }]
        );
    }

    #[test]
    fn a_name_without_data_does_not_leak_into_the_next_event() {
        let events = feed("event: lonely\n\ndata: x\n\n");
        assert_eq!(
            events,
            vec![Event {
                name: "message".into(),
                data: "x".into()
            }]
        );
    }
}
