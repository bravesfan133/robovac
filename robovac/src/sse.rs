//! Incremental `text/event-stream` parser.
//!
//! Deliberately minimal and separated from the I/O so it can be tested without
//! a socket. Only the parts of the spec the Valetudo stream actually uses are
//! handled; anything unrecognised is skipped rather than treated as an error,
//! since an unparseable keep-alive should never take the listener down.

/// One dispatched event.
#[derive(Debug, PartialEq, Eq)]
pub struct Event {
    pub name: String,
    pub data: String,
}

/// Buffers bytes and yields complete events as they arrive.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of decoded text. Any events it completes are returned.
    ///
    /// Text decoding errors on a partial multi-byte character are expected and
    /// ignored; the remaining bytes arrive in the next chunk.
    pub fn push(&mut self, chunk: &str) -> Vec<Event> {
        self.buffer.push_str(chunk);

        let mut events = Vec::new();
        // Events are separated by a blank line. Tolerate CRLF, which some
        // proxies rewrite.
        while let Some(split) = find_separator(&self.buffer) {
            let (block, rest) = self.buffer.split_at(split.0);
            let block = block.to_string();
            self.buffer = rest[split.1..].to_string();

            if let Some(event) = parse_block(&block) {
                events.push(event);
            }
        }
        events
    }

    /// Bytes buffered but not yet terminated by a blank line.
    pub fn pending_bytes(&self) -> usize {
        self.buffer.len()
    }
}

/// Returns `(index_of_separator, length_of_separator)` for the first blank line.
fn find_separator(buffer: &str) -> Option<(usize, usize)> {
    let bytes = buffer.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            // "\n\n"
            if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                return Some((i, 2));
            }
            // "\n\r\n"
            if i + 2 < bytes.len() && bytes[i + 1] == b'\r' && bytes[i + 2] == b'\n' {
                return Some((i, 3));
            }
        }
        i += 1;
    }
    None
}

fn parse_block(block: &str) -> Option<Event> {
    let mut name: Option<String> = None;
    let mut data = String::new();

    for line in block.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        // Comment, used by servers as a keep-alive.
        if line.starts_with(':') {
            continue;
        }

        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            // A bare field name with no colon means an empty value.
            None => (line, ""),
        };

        match field {
            "event" => name = Some(value.to_string()),
            "data" => {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            // `id`, `retry` and anything a future firmware adds: ignored.
            _ => {}
        }
    }

    // A block with no data is not an event: comments and bare `retry:`
    // directives share the same framing, and dispatching empty events for them
    // would turn a keep-alive into spurious map updates.
    if data.is_empty() {
        return None;
    }

    // An event with no name defaults to "message" per the spec.
    Some(Event {
        name: name.unwrap_or_else(|| "message".to_string()),
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(parser: &mut SseParser, chunk: &str) -> Vec<String> {
        parser.push(chunk).into_iter().map(|e| e.name).collect()
    }

    #[test]
    fn parses_a_single_event() {
        let mut p = SseParser::new();
        let events = p.push("event: map\ndata: {\"a\":1}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "map");
        assert_eq!(events[0].data, r#"{"a":1}"#);
    }

    #[test]
    fn waits_for_the_blank_line() {
        let mut p = SseParser::new();
        assert!(p.push("event: map\ndata: partial").is_empty());
        assert!(p.pending_bytes() > 0);
        let events = p.push("\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "partial");
    }

    #[test]
    fn splits_multiple_events_in_one_chunk() {
        let mut p = SseParser::new();
        let names = names(
            &mut p,
            "event: a\ndata: 1\n\nevent: b\ndata: 2\n\nevent: c\ndata: 3\n\n",
        );
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn handles_an_event_split_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.push("event: ma").is_empty());
        assert!(p.push("p\ndata: pay").is_empty());
        let events = p.push("load\n\n");
        assert_eq!(events[0].name, "map");
        assert_eq!(events[0].data, "payload");
    }

    #[test]
    fn ignores_comments_and_keepalives() {
        let mut p = SseParser::new();
        assert!(p.push(": keep-alive\n\n").is_empty());
        assert_eq!(
            p.pending_bytes(),
            0,
            "comment-only blocks must not accumulate"
        );
    }

    #[test]
    fn defaults_the_event_name() {
        let mut p = SseParser::new();
        let events = p.push("data: bare\n\n");
        assert_eq!(events[0].name, "message");
    }

    #[test]
    fn joins_multiline_data() {
        let mut p = SseParser::new();
        let events = p.push("data: line one\ndata: line two\n\n");
        assert_eq!(events[0].data, "line one\nline two");
    }

    #[test]
    fn tolerates_crlf_line_endings() {
        let mut p = SseParser::new();
        let events = p.push("event: map\r\ndata: payload\r\n\r\n");
        assert_eq!(events[0].name, "map");
        assert_eq!(events[0].data, "payload");
    }

    #[test]
    fn handles_a_value_with_no_space_after_the_colon() {
        let mut p = SseParser::new();
        let events = p.push("data:tight\n\n");
        assert_eq!(events[0].data, "tight");
    }

    #[test]
    fn ignores_unknown_fields() {
        let mut p = SseParser::new();
        let events = p.push("id: 7\nretry: 3000\nevent: map\ndata: x\n\n");
        assert_eq!(events[0].name, "map");
        assert_eq!(events[0].data, "x");
    }

    #[test]
    fn fields_accumulate_until_the_blank_line() {
        let mut p = SseParser::new();
        assert!(p.push("event: map\n").is_empty());
        assert!(p.push("data: x\n").is_empty());
        let events = p.push("\n");
        assert_eq!(events.len(), 1, "only the blank line dispatches");
        assert_eq!(events[0].name, "map");
    }

    #[test]
    fn a_bare_retry_directive_does_not_dispatch() {
        let mut p = SseParser::new();
        assert!(p.push("retry: 2000\n\n").is_empty());
        assert_eq!(p.pending_bytes(), 0);
    }

    #[test]
    fn survives_garbage_without_losing_the_next_event() {
        let mut p = SseParser::new();
        assert!(p.push("\u{fffd}\u{0}\n\n").is_empty());
        let events = p.push("event: map\ndata: still works\n\n");
        assert_eq!(events[0].name, "map");
    }
}
