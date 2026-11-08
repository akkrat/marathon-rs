//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/transform/ResponseTransformer.kt`
//! and `.../request/transform/StringResponseTransformer.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.

use jcompat::utf8::decode_utf8;

use super::error::AdbError;

pub trait ResponseTransformer {
    type Output;
    /// One read's bytes.
    fn process(&mut self, bytes: &[u8]);
    fn transform(&mut self) -> Result<Self::Output, AdbError>;
}

/// The response as text, Kotlin-`trim()`med.
///
/// adam decodes each read on its own (`StringResponseTransformer.kt:23`), so a character split across two
/// reads becomes replacement characters. Here the bytes are decoded once, at the end (D11).
#[derive(Debug, Default)]
pub struct StringResponseTransformer {
    bytes: Vec<u8>,
}

impl ResponseTransformer for StringResponseTransformer {
    type Output = String;

    fn process(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn transform(&mut self) -> Result<String, AdbError> {
        Ok(kotlin_trim(&decode_utf8(&self.bytes)).to_string())
    }
}

/// Kotlin `String.trim()`. Every Kotlin whitespace character is in the BMP, so testing chars is exact.
pub(crate) fn kotlin_trim(s: &str) -> &str {
    s.trim_matches(jcompat::jstring::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_transformer_trims_and_decodes_across_reads() {
        let mut t = StringResponseTransformer::default();
        let text = " \u{3000}ж\n".as_bytes();
        t.process(&text[..3]);
        t.process(&text[3..]);
        assert_eq!(t.transform().unwrap(), "ж");
    }
}
