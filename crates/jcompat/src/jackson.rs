//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/xcresulttool/AppleJsonTypeAdapterFactory.kt (Jackson library semantics).

use crate::jtime;

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

/// Decode a JSON string token with replacement for unpaired surrogates (D55).
/// The caller validates JSON syntax first; this also checks escapes defensively.
pub fn json_string(token: &str) -> Option<String> {
    let body = token.strip_prefix('"')?.strip_suffix('"')?;
    let mut chars = body.chars();
    let mut result = Vec::new();
    while let Some(character) = chars.next() {
        if character != '\\' {
            result.extend(character.encode_utf16(&mut [0; 2]).iter().copied());
            continue;
        }
        match chars.next()? {
            '"' => result.push(34),
            '\\' => result.push(92),
            '/' => result.push(47),
            'b' => result.push(8),
            'f' => result.push(12),
            'n' => result.push(10),
            'r' => result.push(13),
            't' => result.push(9),
            'u' => {
                let mut unit = 0;
                for _ in 0..4 {
                    unit = unit * 16 + chars.next()?.to_digit(16)? as u16;
                }
                result.push(unit);
            }
            _ => return None,
        }
    }
    Some(String::from_utf16_lossy(&result))
}

pub enum Scalar<'a> {
    Null,
    Bool(bool),
    Number(&'a str),
    String(&'a str),
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarError {
    Shape,
    Invalid,
}

pub fn string(value: Scalar<'_>) -> Result<String, ScalarError> {
    match value {
        Scalar::String(text) => Ok(text.to_owned()),
        Scalar::Bool(value) => Ok(String::from(if value { "true" } else { "false" })),
        Scalar::Number(text) => {
            let text = if text.contains(['.', 'e', 'E']) {
                crate::jnum::double_to_string(text.parse().map_err(|_| ScalarError::Invalid)?)
            } else if text == "-0" {
                "0".to_owned()
            } else {
                text.to_owned()
            };
            Ok(text)
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
            let text = trim(text);
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
            let text = trim(text);
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
        Scalar::String(text) => match trim(text) {
            "true" | "True" | "TRUE" => Ok(Some(true)),
            "false" | "False" | "FALSE" => Ok(Some(false)),
            "" | "null" => Ok(None),
            _ => Err(ScalarError::Invalid),
        },
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
            let text = trim(text);
            if text.is_empty() || text == "null" {
                return Ok(None);
            }
            parse_date(text).map(Some).ok_or(ScalarError::Invalid)
        }
        _ => Err(ScalarError::Shape),
    }
}

/// Normalize lone UTF-16 escapes before a native JSON decoder reads strings or
/// object keys. Replacements keep the six-byte escape width, preserving parser
/// positions. Malformed JSON is left for the decoder to reject.
pub fn normalize_json_strings(text: &str) -> std::borrow::Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut output = None;
    let mut quoted = false;
    let mut i = 0;
    fn unit(bytes: &[u8], at: usize) -> Option<u16> {
        let token = bytes.get(at..at.checked_add(6)?)?;
        if !token.starts_with(b"\\u") {
            return None;
        }
        token[2..]
            .iter()
            .try_fold(0u16, |n, c| Some(n * 16 + char::from(*c).to_digit(16)? as u16))
    }
    while i < bytes.len() {
        match bytes[i] {
            b'"' => quoted = !quoted,
            b'\\' if quoted => {
                if let Some(u) = unit(bytes, i) {
                    if (0xd800..=0xdbff).contains(&u)
                        && unit(bytes, i + 6).is_some_and(|v| (0xdc00..=0xdfff).contains(&v))
                    {
                        i += 12;
                        continue;
                    }
                    if (0xd800..=0xdfff).contains(&u) {
                        let output = output.get_or_insert_with(|| bytes.to_vec());
                        output[i..i + 6].copy_from_slice(b"\\uFFFD");
                    }
                    i += 6;
                    continue;
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    match output {
        // Only ASCII escape bytes were replaced, so UTF-8 validity is unchanged.
        Some(bytes) => std::borrow::Cow::Owned(String::from_utf8(bytes).unwrap()),
        None => std::borrow::Cow::Borrowed(text),
    }
}

#[cfg(test)]
mod native_string_tests {
    use super::*;

    #[test]
    fn json_normalization_preserves_pairs_literals_and_error_positions() {
        let original = r#"{"\uD800":"\udc00x\ud83d\ude00", "literal":"\\uD800", "escaped":"\"\uD800"}"#;
        let expected = r#"{"\uFFFD":"\uFFFDx\ud83d\ude00", "literal":"\\uD800", "escaped":"\"\uFFFD"}"#;
        assert_eq!(normalize_json_strings(original), expected);
        assert_eq!(original.len(), expected.len());
        for text in [r#""😀\ud83d\ude00""#, r#""\uZZZZ""#, r#""\uD80""#] {
            assert!(matches!(normalize_json_strings(text), std::borrow::Cow::Borrowed(_)));
        }
        assert_eq!(json_string(r#""\ud800x\udc00\ud83d\ude00""#), Some("�x�😀".into()));
    }
}
