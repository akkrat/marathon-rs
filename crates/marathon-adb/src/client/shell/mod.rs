//! Shell services: v1 (`shell:`), v2 (`shell,v2,raw:`) and the compat stream that picks one by feature.

pub mod compat;
pub mod v1;
pub mod v2;

use jcompat::utf8::decode_utf8;

/// UTF-8 decoding across reads (D11): a sequence cut off at the end of one read is kept for the next, and
/// whatever is complete is decoded with the JDK's replacement rules.
#[derive(Debug, Default)]
pub(crate) struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let complete = complete_prefix(&self.pending);
        let text = decode_utf8(&self.pending[..complete]);
        self.pending.drain(..complete);
        text
    }

    /// The end of the stream: an unfinished sequence becomes a replacement character.
    pub(crate) fn finish(&mut self) -> String {
        decode_utf8(&std::mem::take(&mut self.pending))
    }
}

/// The length of `bytes` without a trailing sequence that more bytes could still complete.
fn complete_prefix(bytes: &[u8]) -> usize {
    let mut offset = 0;
    loop {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(_) => return bytes.len(),
            Err(e) => match e.error_len() {
                // Invalid here and now: decoding it later would not change it.
                Some(len) => offset += e.valid_up_to() + len,
                None => return offset + e.valid_up_to(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_across_reads() {
        let mut d = Utf8Stream::default();
        let bytes = "aж€😀".as_bytes();
        let mut out = String::new();
        for b in bytes {
            out.push_str(&d.push(std::slice::from_ref(b)));
        }
        out.push_str(&d.finish());
        assert_eq!(out, "aж€😀");
        // Invalid bytes are decoded at once; a cut-off sequence at the very end is replaced on finish.
        assert_eq!(d.push(b"\xffa\xe2\x82"), "\u{fffd}a");
        assert_eq!(d.finish(), "\u{fffd}");
    }
}
