//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/xcresulttool/AppleJsonTypeAdapterFactory.kt (Jackson library semantics).

use crate::{jtime, writers::JavaString};

/// `java.util.Date` stores epoch milliseconds, independently of `java.time.Instant`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JavaDate(pub i64);

/// Approved WP 4.6 P3 subset of Jackson's default Date parser: ISO offset timestamps
/// and signed ASCII epoch milliseconds. Fractional seconds are truncated to millis.
pub fn parse_date(text: &str) -> Option<JavaDate> {
    let text = trim(text);
    if text.is_ascii()
        && text
            .strip_prefix('-')
            .unwrap_or(text)
            .bytes()
            .all(|b| b.is_ascii_digit())
        && !text.is_empty()
    {
        return text.parse().ok().map(JavaDate);
    }
    // Jackson accepts both +HH:mm and +HHmm. Instant.parse requires the colon.
    let mut iso = text.to_owned();
    if iso.len() >= 5 {
        let position = iso.len() - 5;
        let tail = &iso.as_bytes()[position..];
        if matches!(tail[0], b'+' | b'-') && tail[1..].iter().all(u8::is_ascii_digit) {
            iso.insert(position + 3, ':');
        }
    }
    // The Date parser accepts more than nine fractional digits; only three matter.
    if let Some(dot) = iso.find('.') {
        let end = dot
            + 1
            + iso.as_bytes()[dot + 1..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
        if end > dot + 4 {
            iso.replace_range(dot + 4..end, "");
        }
    }
    let instant = jtime::parse_instant(&iso)?;
    Some(JavaDate(
        instant
            .seconds()
            .checked_mul(1000)?
            .checked_add(instant.nanos() / 1_000_000)?,
    ))
}

/// Java String.trim(), used by the default Jackson scalar coercions.
pub fn trim(text: &str) -> &str {
    text.trim_matches(|c| c <= '\u{20}')
}

/// Decode a JSON string token to UTF-16 without discarding unpaired surrogates.
/// The caller validates JSON syntax first; this also checks escapes defensively.
pub fn json_string(token: &str) -> Option<JavaString> {
    let body = token.strip_prefix('"')?.strip_suffix('"')?;
    let mut chars = body.chars();
    let mut result = JavaString::default();
    while let Some(character) = chars.next() {
        if character != '\\' {
            result.push_code_point(character as u32);
            continue;
        }
        match chars.next()? {
            '"' => result.push_str("\""),
            '\\' => result.push_str("\\"),
            '/' => result.push_str("/"),
            'b' => result.0.push(8),
            'f' => result.0.push(12),
            'n' => result.0.push(10),
            'r' => result.0.push(13),
            't' => result.0.push(9),
            'u' => {
                let mut unit = 0;
                for _ in 0..4 {
                    unit = unit * 16 + chars.next()?.to_digit(16)? as u16;
                }
                result.0.push(unit);
            }
            _ => return None,
        }
    }
    Some(result)
}

pub enum Scalar<'a> {
    Null,
    Bool(bool),
    Number(&'a str),
    String(&'a JavaString),
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarError {
    Shape,
    Invalid,
}

pub fn string(value: Scalar<'_>) -> Result<JavaString, ScalarError> {
    match value {
        Scalar::String(text) => Ok(text.clone()),
        Scalar::Bool(value) => Ok(JavaString::from(if value { "true" } else { "false" })),
        Scalar::Number(text) => {
            let text = if text.contains(['.', 'e', 'E']) {
                crate::jnum::double_to_string(text.parse().map_err(|_| ScalarError::Invalid)?)
            } else if text == "-0" {
                "0".to_owned()
            } else {
                text.to_owned()
            };
            Ok(JavaString::from(text))
        }
        _ => Err(ScalarError::Shape),
    }
}

pub fn int(value: Scalar<'_>) -> Result<Option<i32>, ScalarError> {
    match value {
        Scalar::Null => Ok(None),
        Scalar::Number(text) => {
            if !text.contains(['.', 'e', 'E']) {
                return text.parse().map(Some).map_err(|_| ScalarError::Invalid);
            }
            let value: f64 = text.parse().map_err(|_| ScalarError::Invalid)?;
            if value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX) {
                Ok(Some(value as i32))
            } else {
                Err(ScalarError::Invalid)
            }
        }
        Scalar::String(text) => {
            let text = text.as_utf8().map_err(|_| ScalarError::Invalid)?;
            let text = trim(&text);
            if text.is_empty() || text == "null" {
                Ok(None)
            } else if text.is_ascii() {
                text.parse().map(Some).map_err(|_| ScalarError::Invalid)
            } else {
                Err(ScalarError::Invalid)
            }
        }
        _ => Err(ScalarError::Shape),
    }
}

pub fn double(value: Scalar<'_>) -> Result<Option<f64>, ScalarError> {
    match value {
        Scalar::Null => Ok(None),
        Scalar::Number(text) => text.parse().map(Some).map_err(|_| ScalarError::Invalid),
        Scalar::String(text) => {
            let text = text.as_utf8().map_err(|_| ScalarError::Invalid)?;
            let text = trim(&text);
            if text.is_empty() || text == "null" {
                return Ok(None);
            }
            if !text.is_ascii() || text.contains(['x', 'X']) {
                return Err(ScalarError::Invalid);
            }
            crate::jnum::parse_double(text).map(Some).ok_or(ScalarError::Invalid)
        }
        _ => Err(ScalarError::Shape),
    }
}

pub fn boolean(value: Scalar<'_>) -> Result<Option<bool>, ScalarError> {
    match value {
        Scalar::Null => Ok(None),
        Scalar::Bool(value) => Ok(Some(value)),
        Scalar::Number(text) if !text.contains(['.', 'e', 'E']) => {
            Ok(Some(text.bytes().any(|b| matches!(b, b'1'..=b'9'))))
        }
        Scalar::String(text) => {
            let text = text.as_utf8().map_err(|_| ScalarError::Invalid)?;
            match trim(&text) {
                "true" | "True" | "TRUE" => Ok(Some(true)),
                "false" | "False" | "FALSE" => Ok(Some(false)),
                "" | "null" => Ok(None),
                _ => Err(ScalarError::Invalid),
            }
        }
        _ => Err(ScalarError::Shape),
    }
}

pub fn date(value: Scalar<'_>) -> Result<Option<JavaDate>, ScalarError> {
    match value {
        Scalar::Null => Ok(None),
        Scalar::Number(text) if !text.contains(['.', 'e', 'E']) => text
            .parse()
            .map(|millis| Some(JavaDate(millis)))
            .map_err(|_| ScalarError::Invalid),
        Scalar::String(text) => {
            let text = text.as_utf8().map_err(|_| ScalarError::Invalid)?;
            let text = trim(&text);
            if text.is_empty() || text == "null" {
                return Ok(None);
            }
            parse_date(text).map(Some).ok_or(ScalarError::Invalid)
        }
        _ => Err(ScalarError::Shape),
    }
}
