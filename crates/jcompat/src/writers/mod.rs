//! Port of core/src/main/kotlin/com/malinskiy/marathon/di/Modules.kt,
//! core/src/main/kotlin/com/malinskiy/marathon/json/FileSerializer.kt and
//! core/src/main/kotlin/com/malinskiy/marathon/execution/command/parse/MarathonTestParseCommand.kt.
//!
//! Callers project YAML models into declaration-order values; numeric spellings
//! come from jcompat's Java formatters.

mod kotlin;
mod yaml;
pub use kotlin::KotlinValue;
pub use yaml::JacksonYamlWriter;

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
    String(String),
    Array(Vec<WriterValue>),
    /// Jackson ByteArraySerializer uses a tagged YAML binary scalar.
    Binary(Vec<u8>),
    Object(Vec<(String, WriterValue)>),
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
