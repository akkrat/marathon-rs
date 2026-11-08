//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/SerializeModule.kt
//! Jackson 2.17 scalar coercions used by the shared binder.
use crate::binder::{INVALID_FORMAT, JSON_MAPPING, MISMATCHED};
use jcompat::jnum::{parse_double, parse_long};
use jcompat::jtime::{duration_of, instant_of, parse_duration, parse_instant};
use snakeyaml::resolve::{JInt, base64_encode, java_trim};
use snakeyaml::tokens::{Tok, Token};
fn ascii_long(s: &str) -> Option<i64> {
    s.is_ascii().then(|| parse_long(s)).flatten()
}
/// `StreamReadConstraints.getMaxNumberLength` (1000): databind rejects a
/// longer number spelling before parsing it.
pub(super) fn check_number_length(text: &str) -> Result<(), &'static str> {
    if text.encode_utf16().count() > 1000 {
        return Err(JSON_MAPPING);
    }
    Ok(())
}

pub(super) fn embedded_text(bytes: &[u8]) -> String {
    base64_encode(bytes)
}

/// `getIntValue` with databind's range check: floats truncate, big integers fail.
pub(super) fn int_value(t: &Token) -> Result<i32, &'static str> {
    match &t.tok {
        Tok::Int {
            value: Ok(JInt::Int(v)),
            ..
        } => Ok(*v),
        Tok::Int {
            value: Ok(JInt::Long(v)),
            ..
        } => i32::try_from(*v).map_err(|_| JSON_MAPPING),
        Tok::Int {
            value: Ok(JInt::Big(_)),
            ..
        } => Err(JSON_MAPPING),
        Tok::Int { value: Err(_), .. } => Err(JSON_MAPPING),
        Tok::Float { cleaned, .. } => match parse_double(cleaned) {
            None => Err(JSON_MAPPING),
            Some(f) => {
                if f < f64::from(i32::MIN) || f > f64::from(i32::MAX) {
                    return Err(JSON_MAPPING);
                }
                Ok(f as i32)
            }
        },
        Tok::Str(s) if java_trim(&s.to_owned()).is_empty() || java_trim(&s.to_owned()) == "null" => Ok(0),
        Tok::Str(s) => {
            check_number_length(java_trim(&s.to_owned()))?;
            ascii_long(java_trim(&s.to_owned()))
                .and_then(|v| i32::try_from(v).ok())
                .ok_or(INVALID_FORMAT)
        }
        Tok::Null(_) => Ok(0),
        Tok::True(_) | Tok::False(_) => Err(MISMATCHED),
        _ => Err(MISMATCHED),
    }
}

pub(super) fn long_value(t: &Token) -> Result<i64, &'static str> {
    match &t.tok {
        Tok::Int {
            value: Ok(JInt::Int(v)),
            ..
        } => Ok(i64::from(*v)),
        Tok::Int {
            value: Ok(JInt::Long(v)),
            ..
        } => Ok(*v),
        Tok::Int {
            value: Ok(JInt::Big(_)),
            ..
        } => Err(JSON_MAPPING),
        Tok::Int { value: Err(_), .. } => Err(JSON_MAPPING),
        Tok::Float { cleaned, .. } => match parse_double(cleaned) {
            None => Err(JSON_MAPPING),
            Some(f) => {
                if f < -9.223372036854776e18 || f > 9.223372036854776e18 {
                    return Err(JSON_MAPPING);
                }
                Ok(f as i64)
            }
        },
        Tok::Str(s) if java_trim(&s.to_owned()).is_empty() || java_trim(&s.to_owned()) == "null" => Ok(0),
        Tok::Str(s) => {
            check_number_length(java_trim(&s.to_owned()))?;
            ascii_long(java_trim(&s.to_owned())).ok_or(INVALID_FORMAT)
        }
        Tok::Null(_) => Ok(0),
        Tok::True(_) | Tok::False(_) => Err(MISMATCHED),
        _ => Err(MISMATCHED),
    }
}

pub(super) fn double_value(t: &Token) -> Result<f64, &'static str> {
    match &t.tok {
        Tok::Int {
            value: Ok(JInt::Int(v)),
            ..
        } => Ok(f64::from(*v)),
        Tok::Int {
            value: Ok(JInt::Long(v)),
            ..
        } => Ok(*v as f64),
        Tok::Int {
            value: Ok(JInt::Big(s)),
            text: _,
        } => s.parse::<f64>().map_err(|_| JSON_MAPPING),
        Tok::Int { value: Err(_), .. } => Err(JSON_MAPPING),
        Tok::Float { cleaned, .. } => parse_double(cleaned).ok_or(JSON_MAPPING),
        Tok::Str(s) if java_trim(&s.to_owned()).is_empty() || java_trim(&s.to_owned()) == "null" => Ok(0.0),
        // No length gate here: a long non-numeric string fails parsing first
        // (C43), unlike the int/long path.
        Tok::Str(s) => java_trim(&s.to_owned())
            .is_ascii()
            .then(|| parse_double(java_trim(&s.to_owned())))
            .flatten()
            .ok_or(INVALID_FORMAT),
        Tok::Null(_) => Ok(0.0),
        _ => Err(MISMATCHED),
    }
}

pub(super) fn bool_value(t: &Token) -> Result<bool, &'static str> {
    match &t.tok {
        Tok::True(_) => Ok(true),
        Tok::False(_) => Ok(false),
        Tok::Null(_) => Ok(false),
        Tok::Str(s) if java_trim(&s.to_owned()).is_empty() || java_trim(&s.to_owned()) == "null" => Ok(false),
        Tok::Str(s) => match java_trim(&s.to_owned()) {
            "true" | "True" | "TRUE" => Ok(true),
            "false" | "False" | "FALSE" => Ok(false),
            _ => Err(INVALID_FORMAT),
        },
        Tok::Int {
            value: Ok(JInt::Int(v)),
            ..
        } => Ok(*v != 0),
        Tok::Int {
            value: Ok(JInt::Long(v)),
            ..
        } => Ok(*v != 0),
        Tok::Int {
            value: Ok(JInt::Big(s)),
            ..
        } => Ok(s != "0" && s != "-0"),
        Tok::Int { value: Err(_), .. } => Err(JSON_MAPPING),
        _ => Err(MISMATCHED),
    }
}

/// `None` is the null coercion (explicit null, or empty string for the
/// jsr310/file-adjacent targets); the caller maps it per nullability.
pub(super) fn string_value(t: &Token) -> Result<Option<String>, &'static str> {
    match &t.tok {
        Tok::Str(s) => Ok(Some(s.to_owned())),
        Tok::Int {
            text: s, value: Ok(_), ..
        } => Ok(Some(s.clone())),
        Tok::Int { value: Err(_), .. } => Err(JSON_MAPPING),
        // A valid float reads as its raw spelling (`.5`, `1e3`); only an
        // unparseable one fails, as its token already records.
        Tok::Float { text, cleaned } => match parse_double(cleaned) {
            Some(_) => Ok(Some(text.clone())),
            None => Err(JSON_MAPPING),
        },
        Tok::True(s) | Tok::False(s) => Ok(Some(s.clone())),
        Tok::Null(_) => Ok(None),
        Tok::Embedded(b) => Ok(Some(embedded_text(b))),
        _ => Err(MISMATCHED),
    }
}

pub(super) fn instant_value(t: &Token) -> Result<Option<String>, &'static str> {
    match &t.tok {
        Tok::Str(s) if java_trim(&s.to_owned()).is_empty() => Ok(None),
        Tok::Str(s) => {
            let s = s.to_owned();
            if let Some(i) = parse_instant(&s) {
                return Ok(Some(i.to_string()));
            }
            let trimmed = java_trim(&s);
            if let Some(i) = parse_instant(trimmed) {
                return Ok(Some(i.to_string()));
            }
            let plain = !trimmed.is_empty()
                && trimmed.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                && trimmed.bytes().filter(|&b| b == b'.').count() <= 1;
            if !plain {
                return Err(INVALID_FORMAT);
            }
            let decimal = jcompat::jnum::Decimal::parse(trimmed).ok_or(INVALID_FORMAT)?;
            let (s, n) = decimal.checked_seconds_and_nanos().ok_or(INVALID_FORMAT)?;
            instant_of(s, n).map(|i| Some(i.to_string())).ok_or(INVALID_FORMAT)
        }
        Tok::Int {
            value: Ok(JInt::Int(v)),
            ..
        } => instant_of(i64::from(*v), 0)
            .map(|i| Some(i.to_string()))
            .ok_or(JSON_MAPPING),
        Tok::Int {
            value: Ok(JInt::Long(v)),
            ..
        } => instant_of(*v, 0).map(|i| Some(i.to_string())).ok_or(JSON_MAPPING),
        Tok::Int { .. } => Err(JSON_MAPPING),
        Tok::Float { cleaned, .. } => match jcompat::jnum::Decimal::parse(cleaned) {
            None => Err(JSON_MAPPING),
            Some(d) => {
                let (s, n) = d.checked_seconds_and_nanos().ok_or(JSON_MAPPING)?;
                instant_of(s, n).map(|i| Some(i.to_string())).ok_or(JSON_MAPPING)
            }
        },
        Tok::Null(_) => Ok(None),
        Tok::Embedded(_) => Err(JSON_MAPPING),
        _ => Err(MISMATCHED),
    }
}

pub(super) fn duration_value(t: &Token) -> Result<Option<String>, &'static str> {
    match &t.tok {
        Tok::Str(s) if java_trim(&s.to_owned()).is_empty() => Ok(None),
        Tok::Str(s) => parse_duration(java_trim(&s.to_owned()))
            .map(|d| Some(d.to_string()))
            .ok_or(INVALID_FORMAT),
        Tok::Int {
            value: Ok(JInt::Int(v)),
            ..
        } => duration_of(i64::from(*v), 0)
            .map(|d| Some(d.to_string()))
            .ok_or(JSON_MAPPING),
        Tok::Int {
            value: Ok(JInt::Long(v)),
            ..
        } => duration_of(*v, 0).map(|d| Some(d.to_string())).ok_or(JSON_MAPPING),
        Tok::Int { .. } => Err(JSON_MAPPING),
        Tok::Float { cleaned, .. } => match jcompat::jnum::Decimal::parse(cleaned) {
            None => Err(JSON_MAPPING),
            Some(d) => {
                let (s, n) = d.checked_seconds_and_nanos().ok_or(JSON_MAPPING)?;
                duration_of(s, n).map(|d| Some(d.to_string())).ok_or(JSON_MAPPING)
            }
        },
        Tok::Null(_) => Ok(None),
        Tok::Embedded(_) => Err(JSON_MAPPING),
        _ => Err(MISMATCHED),
    }
}
