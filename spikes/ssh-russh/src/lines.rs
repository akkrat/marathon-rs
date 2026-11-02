//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/LineBuffer.kt

#[derive(Default)]
pub struct LineBuffer {
    buffer: String,
    previous_chunk_ended_with_cr: bool,
}

impl LineBuffer {
    /// Each chunk is decoded on its own, as the Kotlin `String(bytes, 0, count, charset)` does: a UTF-8 sequence split
    /// across two packets becomes replacement characters on both sides.
    pub fn append(&mut self, bytes: &[u8]) {
        let suffix = String::from_utf8_lossy(bytes);
        if suffix.is_empty() {
            return;
        }
        // A CR is emitted immediately; its LF is suppressed even if a flush ran between the chunks
        let text = if self.previous_chunk_ended_with_cr && suffix.starts_with('\n') {
            &suffix[1..]
        } else {
            &suffix[..]
        };
        self.previous_chunk_ended_with_cr = suffix.ends_with('\r');
        self.buffer.push_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
    }

    /// The complete lines; the trailing partial line stays buffered
    pub fn flush(&mut self) -> Vec<String> {
        let parts: Vec<&str> = self.buffer.split('\n').collect();
        if parts.len() < 2 {
            return Vec::new();
        }
        let (last, lines) = parts.split_last().unwrap_or((&"", &[]));
        let lines = lines.iter().map(|s| s.to_string()).collect();
        self.buffer = last.to_string();
        lines
    }

    /// All lines, a trailing partial line included
    pub fn drain(&mut self) -> Vec<String> {
        if !self.buffer.is_empty() && !self.buffer.ends_with('\n') && !self.buffer.ends_with('\r') {
            self.buffer.push('\n');
        }
        self.flush()
    }
}
