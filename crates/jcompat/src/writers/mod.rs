//! Port of core/src/main/kotlin/com/malinskiy/marathon/di/Modules.kt,
//! core/src/main/kotlin/com/malinskiy/marathon/json/FileSerializer.kt and
//! core/src/main/kotlin/com/malinskiy/marathon/execution/command/parse/MarathonTestParseCommand.kt.
//!
//! Callers project models into declaration-order values. Numeric spellings come
//! from jcompat's Java formatters, never serde_json's floating-point formatter.

mod gson;
mod kotlin;
mod yaml;
pub use gson::{GsonError, GsonWriter};
pub use kotlin::KotlinValue;
pub use yaml::JacksonYamlWriter;

/// Java strings retain lone surrogates until the final encoding boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JavaString(pub Vec<u16>);
impl JavaString {
    pub fn to_string_lossy(&self) -> String {
        String::from_utf16_lossy(&self.0)
    }
    pub fn push_code_point(&mut self, c: u32) {
        if c <= 0xffff {
            self.0.push(c as u16);
        } else {
            self.0.push((0xd800 + ((c - 0x10000) >> 10)) as u16);
            self.0.push((0xdc00 + ((c - 0x10000) & 1023)) as u16);
        }
    }
    pub fn append(&mut self, other: Self) {
        self.0.extend(other.0);
    }
    pub fn as_utf8(&self) -> Result<String, std::string::FromUtf16Error> {
        String::from_utf16(&self.0)
    }
    pub fn push_str(&mut self, text: &str) {
        self.0.extend(text.encode_utf16());
    }
    /// Kotlin File.writeText uses the JVM UTF-8 encoder's '?' replacement.
    pub fn to_utf8_bytes(&self) -> Vec<u8> {
        char::decode_utf16(self.0.iter().copied())
            .map(|c| c.unwrap_or('?'))
            .collect::<String>()
            .into_bytes()
    }
}
impl From<&str> for JavaString {
    fn from(value: &str) -> Self {
        Self(value.encode_utf16().collect())
    }
}
impl From<String> for JavaString {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

/// A Java Number.toString spelling, including non-finite Float/Double values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaNumber(String);
impl JavaNumber {
    pub fn new(text: impl Into<String>) -> Result<Self, InvalidNumber> {
        let text = text.into();
        let body = text.strip_prefix('-').unwrap_or(&text);
        let valid = if matches!(body, "NaN" | "Infinity") {
            true
        } else {
            let (mantissa, exponent) = body.split_once(['e', 'E']).map_or((body, None), |(m, e)| (m, Some(e)));
            let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
            let mantissa_ok = mantissa
                .split_once('.')
                .map_or_else(|| digits(mantissa), |(a, b)| digits(a) && digits(b));
            mantissa_ok && exponent.is_none_or(|e| digits(e.strip_prefix(['+', '-']).unwrap_or(e)))
        };
        if valid {
            Ok(Self(text))
        } else {
            Err(InvalidNumber(text))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidNumber(pub String);
impl std::fmt::Display for InvalidNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid Java number: {}", self.0)
    }
}
impl std::error::Error for InvalidNumber {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriterValue {
    Null,
    Bool(bool),
    Number(JavaNumber),
    String(JavaString),
    Array(Vec<WriterValue>),
    /// Jackson ByteArraySerializer uses a tagged YAML binary scalar.
    Binary(Vec<u8>),
    Object(Vec<(JavaString, WriterValue)>),
}
impl From<&str> for WriterValue {
    fn from(value: &str) -> Self {
        Self::String(value.into())
    }
}
impl From<i64> for WriterValue {
    fn from(value: i64) -> Self {
        Self::Number(JavaNumber(value.to_string()))
    }
}
