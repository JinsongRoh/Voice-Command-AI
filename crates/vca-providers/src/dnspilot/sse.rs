//! Minimal server-sent events parser (WHATWG HTML §9.2 event stream).

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    /// `id:` field of this event, if any.
    pub id: Option<String>,
    /// `event:` field; `"message"` when absent.
    pub event: String,
    pub data: String,
}

/// Incremental parser: feed bytes, collect complete events.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    id: Option<String>,
    event: Option<String>,
    data: Vec<String>,
    started: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk and returns every event completed by it.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n' || b == b'\r') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            let terminator = line.pop();
            // Treat CRLF as one line break.
            if terminator == Some(b'\r') && self.buf.first() == Some(&b'\n') {
                self.buf.remove(0);
            }
            let mut line = String::from_utf8_lossy(&line).into_owned();
            if !self.started {
                self.started = true;
                if let Some(stripped) = line.strip_prefix('\u{feff}') {
                    line = stripped.to_owned();
                }
            }
            if let Some(ev) = self.line(&line) {
                out.push(ev);
            }
        }
        out
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.find(':') {
            Some(i) => {
                let v = &line[i + 1..];
                (&line[..i], v.strip_prefix(' ').unwrap_or(v))
            }
            None => (line, ""),
        };
        match field {
            "data" => self.data.push(value.to_owned()),
            "event" => self.event = Some(value.to_owned()),
            "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = self.event.take();
        let id = self.id.take();
        if self.data.is_empty() && event.is_none() {
            return None;
        }
        let data = std::mem::take(&mut self.data).join("\n");
        Some(SseEvent {
            id,
            event: event.unwrap_or_else(|| "message".into()),
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_split_chunks_and_crlf() {
        let mut p = SseParser::new();
        let mut events = p.feed(b"event: hello\r\nid: 7\r\ndata: {\"boot_id\":");
        assert!(events.is_empty());
        events.extend(p.feed(b"\"b1\"}\r\n\r\n: comment\n\ndata: a\ndata: b\n\n"));
        assert_eq!(
            events,
            vec![
                SseEvent {
                    id: Some("7".into()),
                    event: "hello".into(),
                    data: "{\"boot_id\":\"b1\"}".into()
                },
                SseEvent {
                    id: None,
                    event: "message".into(),
                    data: "a\nb".into()
                }
            ]
        );
    }
}
