//! Lightweight Server-Sent Events (SSE) stream decoder.
//!
//! Provides a zero-dependency, streaming frame decoder for HTTP/1.1 and HTTP/2
//! Server-Sent Events lines. Designed to safely handle TCP segmentation, CR/LF/CRLF line endings,
//! and multi-line data payloads with minimal allocations.

/// An individual Server-Sent Event frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// Optional nonempty event name emitted via `event: <name>`.
    pub event: Option<String>,
    /// Accumulated data payload emitted via `data: <payload>`.
    pub data: String,
}

/// Streaming line buffer and event assembler.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    current_event: Option<String>,
    current_data: Vec<String>,
    after_cr: bool,
    started: bool,
}

impl SseDecoder {
    /// Creates a new, empty `SseDecoder`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a raw byte slice into the decoder, yielding only blank-line-terminated events.
    /// An unfinished event at EOF is discarded, as required by the SSE framing contract.
    pub fn decode(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let mut events = Vec::new();
        let mut start = 0;
        for (index, byte) in chunk.iter().enumerate() {
            // A CR ends the line immediately. Its optional LF may arrive in the next chunk.
            if self.after_cr {
                self.after_cr = false;
                if *byte == b'\n' {
                    start = index + 1;
                    continue;
                }
            }
            if !matches!(*byte, b'\r' | b'\n') {
                continue;
            }
            self.buffer.extend_from_slice(&chunk[start..index]);
            // Decode complete lines so split UTF-8 characters survive. Retain the allocation
            // for the next line instead of shifting an entire network chunk after each field.
            let line = String::from_utf8_lossy(&self.buffer);
            let line = if self.started {
                line.as_ref()
            } else {
                self.started = true;
                line.strip_prefix('\u{feff}').unwrap_or(&line)
            };
            if line.is_empty() {
                if !self.current_data.is_empty() {
                    events.push(SseEvent {
                        event: self.current_event.take(),
                        data: self.current_data.join("\n"),
                    });
                    self.current_data.clear();
                } else {
                    self.current_event = None;
                }
            } else {
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "data" => self.current_data.push(value.to_string()),
                    // An empty event field resets the name to the default message event.
                    "event" => self.current_event = (!value.is_empty()).then(|| value.to_string()),
                    _ => {} // Comments and unused fields do not affect event data.
                }
            }
            self.buffer.clear();
            self.after_cr = *byte == b'\r';
            start = index + 1;
        }
        self.buffer.extend_from_slice(&chunk[start..]);
        events
    }
}
