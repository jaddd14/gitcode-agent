//! Incremental server-sent events decoder with bounded buffering.

/// One SSE record after line-ending normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Optional named event.
    pub event: String,
    /// Joined data lines, without the terminal newline.
    pub data: String,
}

/// Stateful byte decoder; UTF-8 may be split across transport chunks.
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
    event: String,
    data: Vec<String>,
    size: usize,
}

impl Decoder {
    /// Decode complete lines without interpreting the provider payload.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Record>, String> {
        const LIMIT: usize = 8 * 1024 * 1024;
        if self.pending.len() + bytes.len() + self.size > LIMIT {
            return Err("SSE event exceeds size limit".into());
        }
        self.pending.extend_from_slice(bytes);
        let mut result = Vec::new();
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.pending.drain(..=end).collect();
            let line =
                std::str::from_utf8(&line[..line.len() - 1]).map_err(|_| "invalid SSE UTF-8")?;
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() {
                if !self.data.is_empty() {
                    result.push(Record {
                        event: std::mem::take(&mut self.event),
                        data: self.data.join("\n"),
                    });
                    self.data.clear();
                }
                self.event.clear();
                self.size = 0;
            } else if !line.starts_with(':') {
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "event" => self.event = value.into(),
                    "data" => {
                        self.size += value.len();
                        self.data.push(value.into());
                    }
                    _ => {}
                }
            }
        }
        Ok(result)
    }

    /// Reject truncated records rather than silently accepting partial output.
    pub fn finish(&self) -> Result<(), String> {
        if self.pending.is_empty() && self.data.is_empty() {
            Ok(())
        } else {
            Err("truncated SSE event".into())
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_chunk_boundary_preserves_unicode_and_multiline() {
        let bytes = ": ping\r\nevent: test\r\ndata: привет\r\ndata: world\r\n\r\n".as_bytes();
        for boundary in 0..=bytes.len() {
            let mut decoder = Decoder::default();
            let mut events = decoder.push(&bytes[..boundary]).unwrap();
            events.extend(decoder.push(&bytes[boundary..]).unwrap());
            decoder.finish().unwrap();
            assert_eq!(
                events,
                vec![Record {
                    event: "test".into(),
                    data: "привет\nworld".into()
                }]
            );
        }
    }

    #[test]
    fn truncated_and_invalid_input_fail() {
        let mut decoder = Decoder::default();
        decoder.push(b"data: incomplete").unwrap();
        assert!(decoder.finish().is_err());
        assert!(Decoder::default().push(b"data: \xff\n\n").is_err());
    }
}
