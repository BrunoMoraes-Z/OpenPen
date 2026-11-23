//! Incremental Server-Sent Events parser. Chunks may split anywhere (inside
//! a line, a field name, a UTF-8 sequence or a `\r\n`).

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// `event:` field (empty when absent).
    pub event: String,
    /// `data:` lines joined with `\n`.
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: String,
    data: String,
    has_data: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds bytes; returns the events completed by them.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(rel) = self.buf[start..].iter().position(|&b| b == b'\n') {
            let end = start + rel;
            let mut line = &self.buf[start..end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            let line = String::from_utf8_lossy(line).into_owned();
            start = end + 1;
            if let Some(e) = self.line(&line) {
                out.push(e);
            }
        }
        self.buf.drain(..start);
        out
    }

    /// End of stream: dispatches a last event without its blank line.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&rest)
                .trim_end_matches('\r')
                .to_string();
            if let Some(e) = self.line(&line) {
                return Some(e);
            }
        }
        self.dispatch()
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if !self.has_data && self.event.is_empty() {
            return None;
        }
        self.has_data = false;
        Some(SseEvent {
            event: std::mem::take(&mut self.event),
            data: std::mem::take(&mut self.data),
        })
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
            "event" => self.event = value.to_string(),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: &str = "event: message_start\ndata: {\"a\":1}\n\n: ping comment\nevent: content_block_delta\r\ndata: {\"t\":\"h\u{e9}llo \u{1F600}\"}\r\n\r\nevent: ping\ndata: {}\n\ndata: line1\ndata: line2\n\n";

    fn expected() -> Vec<SseEvent> {
        vec![
            SseEvent {
                event: "message_start".into(),
                data: "{\"a\":1}".into(),
            },
            SseEvent {
                event: "content_block_delta".into(),
                data: "{\"t\":\"h\u{e9}llo \u{1F600}\"}".into(),
            },
            SseEvent {
                event: "ping".into(),
                data: "{}".into(),
            },
            SseEvent {
                event: String::new(),
                data: "line1\nline2".into(),
            },
        ]
    }

    #[test]
    fn whole_stream() {
        let mut p = SseParser::new();
        assert_eq!(p.push(STREAM.as_bytes()), expected());
        assert_eq!(p.finish(), None);
    }

    #[test]
    fn every_split_point_and_byte_by_byte() {
        let bytes = STREAM.as_bytes();
        for cut in 0..=bytes.len() {
            let mut p = SseParser::new();
            let mut got = p.push(&bytes[..cut]);
            got.extend(p.push(&bytes[cut..]));
            assert_eq!(got, expected(), "cut at {cut}");
        }
        let mut p = SseParser::new();
        let got: Vec<_> = bytes
            .iter()
            .flat_map(|b| p.push(std::slice::from_ref(b)))
            .collect();
        assert_eq!(got, expected());
    }

    #[test]
    fn unterminated_last_event() {
        let mut p = SseParser::new();
        assert!(p.push(b"event: x\ndata: 1").is_empty());
        assert_eq!(
            p.finish(),
            Some(SseEvent {
                event: "x".into(),
                data: "1".into()
            })
        );
    }
}
