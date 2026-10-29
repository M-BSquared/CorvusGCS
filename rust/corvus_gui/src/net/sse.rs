//! Server-sent events, parsed line by line.
//!
//! Only what the Corvus server sends is needed (`event:` and `data:` lines,
//! a blank line ending each event), but the parser follows the spec for the
//! rest so a comment, a `data:` split over several lines or a CRLF line end
//! does not turn into a wrong event.

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
}

impl Parser {
    /// Feeds one line without its line end; returns an event when the line
    /// completes one.
    pub fn line(&mut self, line: &str) -> Option<Event> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if self.data.is_empty() {
                self.name.clear();
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
            "data" => self.data.push(value.to_string()),
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
