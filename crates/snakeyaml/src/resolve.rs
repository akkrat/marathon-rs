//! Scalar typing: SnakeYAML 2.2's YAML 1.1 `Resolver`, then Jackson 2.17's `YAMLParser._decodeScalar`.
//!
//! The regular expressions of `Resolver` are reimplemented as hand-written matchers (they are small, anchored and
//! ASCII-only); each one names the pattern it implements.

use crate::node::Style;

/// SnakeYAML's implicit tags, as far as Jackson distinguishes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Implicit {
    Bool,
    Int,
    Float,
    Null,
    /// MERGE, TIMESTAMP, YAML and STR: Jackson reads all of them as a string.
    Str,
}

/// `Resolver.resolve(NodeId.scalar, value, implicit = true)`: the first implicit resolver registered for the first
/// character whose pattern matches, if the value is not longer than that resolver's limit.
pub fn resolve_implicit(value: &str) -> Implicit {
    let len16 = value.encode_utf16().count();
    let first = value.chars().next().unwrap_or('\0');
    // Registration order of Resolver.addImplicitResolvers: BOOL(10), INT(1024), FLOAT(1024), MERGE(10), NULL(10),
    // EMPTY (no first character, 10), TIMESTAMP(50), YAML(10).
    if "yYnNtTfFoO".contains(first) && len16 <= 10 && is_bool(value) {
        return Implicit::Bool;
    }
    if "-+0123456789".contains(first) && len16 <= 1024 && is_int(value) {
        return Implicit::Int;
    }
    if "-+0123456789.".contains(first) && len16 <= 1024 && is_float(value) {
        return Implicit::Float;
    }
    if ("~nN".contains(first) || value.is_empty())
        && len16 <= 10
        && matches!(value, "~" | "null" | "Null" | "NULL" | " ")
    {
        return Implicit::Null;
    }
    if value.is_empty() {
        // EMPTY, registered without a first character.
        return Implicit::Null;
    }
    Implicit::Str
}

/// `^(?:yes|Yes|YES|no|No|NO|true|True|TRUE|false|False|FALSE|on|On|ON|off|Off|OFF)$`
fn is_bool(v: &str) -> bool {
    matches!(
        v,
        "yes"
            | "Yes"
            | "YES"
            | "no"
            | "No"
            | "NO"
            | "true"
            | "True"
            | "TRUE"
            | "false"
            | "False"
            | "FALSE"
            | "on"
            | "On"
            | "ON"
            | "off"
            | "Off"
            | "OFF"
    )
}

fn sign(b: &[u8]) -> usize {
    usize::from(matches!(b.first(), Some(b'-' | b'+')))
}

/// `^(?:[-+]?0b_*[0-1][0-1_]*|[-+]?0_*[0-7][0-7_]*|[-+]?(?:0|[1-9][0-9_]*)|[-+]?0x_*[0-9a-fA-F][0-9a-fA-F_]*`
/// `|[-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+)$`
fn is_int(v: &str) -> bool {
    let b = v.as_bytes();
    let r = &b[sign(b)..];
    let radix = |rest: &[u8], digit: fn(u8) -> bool| {
        let rest = skip(rest, |c| c == b'_');
        !rest.is_empty() && digit(rest[0]) && rest[1..].iter().all(|&c| digit(c) || c == b'_')
    };
    if let Some(rest) = r.strip_prefix(b"0b")
        && radix(rest, |c| c == b'0' || c == b'1')
    {
        return true;
    }
    if let Some(rest) = r.strip_prefix(b"0x")
        && radix(rest, |c| c.is_ascii_hexdigit())
    {
        return true;
    }
    if let Some(rest) = r.strip_prefix(b"0")
        && radix(rest, |c| (b'0'..=b'7').contains(&c))
    {
        return true;
    }
    if r == b"0" {
        return true;
    }
    if !r.is_empty() && (b'1'..=b'9').contains(&r[0]) {
        let digits = 1 + r[1..].iter().take_while(|&&c| c.is_ascii_digit() || c == b'_').count();
        if digits == r.len() {
            return true;
        }
        return sexagesimal_tail(&r[digits..]);
    }
    false
}

/// `(?::[0-5]?[0-9])+` matching the whole input.
fn sexagesimal_tail(mut r: &[u8]) -> bool {
    if r.is_empty() {
        return false;
    }
    while !r.is_empty() {
        let Some(rest) = r.strip_prefix(b":") else {
            return false;
        };
        r = match rest {
            [a, b, tail @ ..] if (b'0'..=b'5').contains(a) && b.is_ascii_digit() => tail,
            [a, tail @ ..] if a.is_ascii_digit() => tail,
            _ => return false,
        };
    }
    true
}

fn skip(b: &[u8], f: impl Fn(u8) -> bool) -> &[u8] {
    let n = b.iter().take_while(|&&c| f(c)).count();
    &b[n..]
}

/// `^([-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+]?[0-9]+)?|[-+]?(?:[0-9][0-9_]*)(?:[eE][-+]?[0-9]+)`
/// `|[-+]?\.[0-9_]+(?:[eE][-+]?[0-9]+)?|[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*|[-+]?\.(?:inf|Inf|INF)`
/// `|\.(?:nan|NaN|NAN))$`
fn is_float(v: &str) -> bool {
    let b = v.as_bytes();
    if matches!(b, b".nan" | b".NaN" | b".NAN") {
        return true;
    }
    let r = &b[sign(b)..];
    if matches!(r, b".inf" | b".Inf" | b".INF") {
        return true;
    }
    let exponent = |e: &[u8]| -> bool {
        match e {
            [] => true,
            [b'e' | b'E', rest @ ..] => {
                let rest = &rest[sign(rest)..];
                !rest.is_empty() && rest.iter().all(u8::is_ascii_digit)
            }
            _ => false,
        }
    };
    let digits_us = |s: &[u8]| s.iter().take_while(|&&c| c.is_ascii_digit() || c == b'_').count();
    if let Some(rest) = r.strip_prefix(b".") {
        // [-+]?\.[0-9_]+(exp)?
        let n = digits_us(rest);
        return n > 0 && exponent(&rest[n..]);
    }
    if r.is_empty() || !r[0].is_ascii_digit() {
        return false;
    }
    let n = 1 + digits_us(&r[1..]);
    let tail = &r[n..];
    match tail.first() {
        // [0-9][0-9_]*\.[0-9_]*(exp)?
        Some(b'.') => {
            let frac = &tail[1..];
            let m = digits_us(frac);
            exponent(&frac[m..])
        }
        // [0-9][0-9_]*[eE][-+]?[0-9]+
        Some(b'e' | b'E') => exponent(tail),
        // [0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*
        Some(b':') => {
            let Some(dot) = tail.iter().position(|&c| c == b'.') else {
                return false;
            };
            sexagesimal_tail(&tail[..dot]) && tail[dot + 1..].iter().all(|&c| c.is_ascii_digit() || c == b'_')
        }
        _ => false,
    }
}

/// A Jackson integer value with the number type Jackson gives it (`JsonParser.NumberType`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JInt {
    Int(i32),
    Long(i64),
    /// Decimal digits with an optional leading `-`.
    Big(String),
}

impl JInt {
    pub fn type_name(&self) -> &'static str {
        match self {
            JInt::Int(_) => "INT",
            JInt::Long(_) => "LONG",
            JInt::Big(_) => "BIG_INTEGER",
        }
    }

    pub fn to_decimal(&self) -> String {
        match self {
            JInt::Int(v) => v.to_string(),
            JInt::Long(v) => v.to_string(),
            JInt::Big(s) => s.clone(),
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            JInt::Int(v) => Some(i64::from(*v)),
            JInt::Long(v) => Some(*v),
            JInt::Big(_) => None,
        }
    }
}

/// The token Jackson makes of a scalar, with the number already decoded the way `getNumberValue` would.
#[derive(Clone, Debug, PartialEq)]
pub enum ScalarToken {
    Null,
    True,
    False,
    Str,
    /// `VALUE_NUMBER_INT` with its decoded value, or the error `getNumberValue` raises (StreamConstraints, malformed).
    Int(Result<JInt, NumberError>),
    /// `VALUE_NUMBER_FLOAT` with the cleaned text Jackson parses on demand (`_cleanedTextValue`).
    Float(String),
    /// `VALUE_EMBEDDED_OBJECT` (`!!binary`).
    Binary(Vec<u8>),
}

/// An error raised when Jackson parses a number token's text (`_parseNumericValue`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NumberError {
    /// `NumberFormatException` turned into "Malformed numeric value" (`JsonParseException`).
    Malformed,
    /// `StreamReadConstraints`: more than 1000 characters (`StreamConstraintsException`).
    TooLong,
}

/// What `YAMLParser.nextToken` does with a scalar event.
#[derive(Clone, Debug, PartialEq)]
pub enum Decoded {
    Token(ScalarToken),
    /// `_decodeNumberScalar` returns null for a lone sign under `!!int`; `nextToken` then reports end of input.
    EndOfInput,
    /// `nextToken` throws: the Jackson class and our message.
    Error(&'static str, String),
}

pub const JSON_PARSE_EXCEPTION: &str = "com.fasterxml.jackson.core.JsonParseException";
pub const STREAM_CONSTRAINTS_EXCEPTION: &str = "com.fasterxml.jackson.core.exc.StreamConstraintsException";

/// Jackson's `_decodeScalar`. `tag` is SnakeYAML's tag string (see [`crate::node::Node::tag`]).
pub fn decode_scalar(text: &str, style: Style, tag: Option<&str>) -> Decoded {
    // EMPTY_STRING_AS_NULL is on (the default), so the early "empty is a string" exit is skipped.
    match tag {
        None | Some("!") => {
            // SnakeYAML's ImplicitTuple: plain scalars, and any scalar tagged with the non-specific `!`, are typed
            // by the resolver; everything else is a string.
            let implicit = (tag.is_none() && style == Style::Plain) || tag == Some("!");
            if !implicit {
                return Decoded::Token(ScalarToken::Str);
            }
            match resolve_implicit(text) {
                Implicit::Str => Decoded::Token(ScalarToken::Str),
                Implicit::Int => decode_number(text),
                Implicit::Float => Decoded::Token(ScalarToken::Float(clean_float(text))),
                Implicit::Bool => match match_bool(text) {
                    Some(true) => Decoded::Token(ScalarToken::True),
                    Some(false) => Decoded::Token(ScalarToken::False),
                    None => Decoded::Token(ScalarToken::Str),
                },
                Implicit::Null => Decoded::Token(ScalarToken::Null),
            }
        }
        Some(tag) => {
            let mut t = tag;
            let owned;
            if let Some(rest) = tag.strip_prefix("tag:yaml.org,2002:") {
                t = rest;
                if rest.contains(',') {
                    owned = rest.split(',').next().unwrap_or("").to_owned();
                    t = &owned;
                }
            }
            if t == "binary" {
                return match base64_mime_decode(java_trim(text)) {
                    Ok(bytes) => Decoded::Token(ScalarToken::Binary(bytes)),
                    Err(m) => Decoded::Error(JSON_PARSE_EXCEPTION, m),
                };
            }
            if t == "bool" {
                return match match_bool(text) {
                    Some(true) => Decoded::Token(ScalarToken::True),
                    Some(false) => Decoded::Token(ScalarToken::False),
                    None => Decoded::Token(ScalarToken::Str),
                };
            }
            if !text.is_empty() {
                match t {
                    "int" => return decode_number(text),
                    "float" => return Decoded::Token(ScalarToken::Float(clean_float(text))),
                    "null" => return Decoded::Token(ScalarToken::Null),
                    _ => {}
                }
            }
            Decoded::Token(ScalarToken::Str)
        }
    }
}

/// `_matchYAMLBoolean` (PARSE_BOOLEAN_LIKE_WORDS_AS_STRINGS off).
fn match_bool(v: &str) -> Option<bool> {
    let eq = |w: &str| java_equals_ignore_case(v, w);
    match v.encode_utf16().count() {
        1 => match v {
            "y" | "Y" => Some(true),
            "n" | "N" => Some(false),
            _ => None,
        },
        2 if eq("no") => Some(false),
        2 if eq("on") => Some(true),
        3 if eq("yes") => Some(true),
        3 if eq("off") => Some(false),
        4 if eq("true") => Some(true),
        5 if eq("false") => Some(false),
        _ => None,
    }
}

/// `String.equalsIgnoreCase`: per UTF-16 unit, equal after `Character.toUpperCase`, or after `toUpperCase` then
/// `toLowerCase` (so `ſ` matches `s`).
fn java_equals_ignore_case(a: &str, b: &str) -> bool {
    fn one(c: char, f: fn(char) -> Vec<char>) -> char {
        match f(c).as_slice() {
            [single] => *single,
            _ => c,
        }
    }
    let upper = |c: char| one(c, |c| c.to_uppercase().collect());
    let lower = |c: char| one(c, |c| c.to_lowercase().collect());
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    a.len() == b.len()
        && a.iter()
            .zip(&b)
            .all(|(&x, &y)| x == y || upper(x) == upper(y) || lower(upper(x)) == lower(upper(y)))
}

/// `_cleanYamlFloat`.
fn clean_float(s: &str) -> String {
    if !s.contains('_') || s.is_empty() {
        return s.to_owned();
    }
    let s = s.strip_prefix('+').unwrap_or(s);
    s.chars().filter(|&c| c != '_').collect()
}

/// `_decodeNumberScalar`, including the radix decoders and, for decimals, `_parseNumericValue(NR_UNKNOWN)`.
fn decode_number(value: &str) -> Decoded {
    let b = value.as_bytes();
    let (negative, mut i) = match b.first() {
        Some(b'-') => (true, 1),
        Some(b'+') => {
            if b.len() == 1 {
                return Decoded::EndOfInput;
            }
            (false, 1)
        }
        _ => (false, 0),
    };
    if b.len() == i {
        return Decoded::EndOfInput;
    }
    if b[i] == b'0' {
        i += 1;
        if i == b.len() {
            return Decoded::Token(ScalarToken::Int(Ok(JInt::Int(0))));
        }
        return match b[i] {
            b'b' | b'B' => radix_int(value, i + 1, 2, negative),
            b'x' | b'X' => radix_int(value, i + 1, 16, negative),
            b'0'..=b'9' | b'_' => radix_int(value, i, 8, negative),
            _ => Decoded::Token(ScalarToken::Str),
        };
    }
    let mut underscores = false;
    for &c in &b[i..] {
        if c == b'_' {
            underscores = true;
        } else if !c.is_ascii_digit() {
            return Decoded::Token(ScalarToken::Str);
        }
    }
    let cleaned = if underscores {
        // _cleanYamlInt: drop a leading '+' and every '_'.
        let s = value.strip_prefix('+').unwrap_or(value);
        let c: String = s.chars().filter(|&c| c != '_').collect();
        if c.is_empty() || c == "-" {
            return Decoded::Error(JSON_PARSE_EXCEPTION, format!("invalid number '{value}'"));
        }
        c
    } else {
        value.to_owned()
    };
    Decoded::Token(ScalarToken::Int(decimal_value(&cleaned, negative)))
}

/// `_parseNumericValue` for a decimal `VALUE_NUMBER_INT`: the type depends on the digit count, with the
/// sign-in-the-length quirk for a leading `+`.
fn decimal_value(cleaned: &str, negative: bool) -> Result<JInt, NumberError> {
    let len = cleaned.len() - usize::from(negative);
    let digits = cleaned.trim_start_matches(['+', '-']);
    let neg = cleaned.starts_with('-');
    if len <= 9 {
        return java_parse_i64(digits, neg)
            .map(|v| JInt::Int(v as i32))
            .ok_or(NumberError::Malformed);
    }
    if len <= 18 {
        let v = java_parse_i64(digits, neg).ok_or(NumberError::Malformed)?;
        if len == 10 && i32::try_from(v).is_ok() {
            return Ok(JInt::Int(v as i32));
        }
        return Ok(JInt::Long(v));
    }
    if cleaned.len() > 1000 {
        return Err(NumberError::TooLong);
    }
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return Err(NumberError::Malformed);
    }
    let trimmed = digits.trim_start_matches('0');
    let text = if trimmed.is_empty() {
        "0".to_owned()
    } else {
        format!("{}{}", if neg { "-" } else { "" }, trimmed)
    };
    if len == 19 {
        // `bitLength() <= 63`, which holds for every long, Long.MIN_VALUE included.
        if let Ok(v) = text.parse::<i64>() {
            return Ok(JInt::Long(v));
        }
    }
    Ok(JInt::Big(text))
}

/// `Long.parseLong` on ASCII digits (the callers have already checked the characters).
fn java_parse_i64(digits: &str, negative: bool) -> Option<i64> {
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut v: i64 = 0;
    for c in digits.bytes() {
        let d = i64::from(c - b'0');
        v = v.checked_mul(10)?;
        v = if negative { v.checked_sub(d)? } else { v.checked_add(d)? };
    }
    Some(v)
}

/// `_decodeNumberIntBinary` / `_decodeNumberIntOctal` / `_decodeNumberIntHex` with `_decodeFromLong`.
fn radix_int(value: &str, from: usize, radix: u32, negative: bool) -> Decoded {
    let cleaned: String = value[from..].chars().filter(|&c| c != '_').collect();
    let (int_max, long_max, may_be_int_len) = match radix {
        2 => (31, 63, 32),
        8 => (10, 21, usize::MAX),
        _ => (7, 15, 8),
    };
    let invalid = || Decoded::Error(JSON_PARSE_EXCEPTION, format!("invalid base-{radix} number '{cleaned}'"));
    let len = cleaned.encode_utf16().count();
    if len <= long_max {
        // Integer.parseInt / Long.parseLong with a radix; an empty string or a bad digit is a NumberFormatException.
        let Some(v) = java_parse_radix(&cleaned, radix) else {
            return invalid();
        };
        if len <= int_max {
            let v = v as i32;
            return Decoded::Token(ScalarToken::Int(Ok(JInt::Int(if negative {
                v.wrapping_neg()
            } else {
                v
            }))));
        }
        let may_be_int = len == may_be_int_len;
        if negative {
            let v = v.wrapping_neg();
            if may_be_int && v >= i64::from(i32::MIN) {
                return Decoded::Token(ScalarToken::Int(Ok(JInt::Int(v as i32))));
            }
            return Decoded::Token(ScalarToken::Int(Ok(JInt::Long(v))));
        }
        if may_be_int && v < i64::from(i32::MAX) {
            return Decoded::Token(ScalarToken::Int(Ok(JInt::Int(v as i32))));
        }
        return Decoded::Token(ScalarToken::Int(Ok(JInt::Long(v))));
    }
    if len > 1000 {
        return Decoded::Error(
            STREAM_CONSTRAINTS_EXCEPTION,
            format!("number of {len} digits is longer than 1000"),
        );
    }
    // new BigInteger(cleaned, radix) also takes a sign.
    let (own_negative, body) = match cleaned.as_bytes().first() {
        Some(b'-') => (true, &cleaned[1..]),
        Some(b'+') => (false, &cleaned[1..]),
        _ => (false, &cleaned[..]),
    };
    if body.is_empty() || !body.chars().all(|c| crate::jnum::digit(c as u32, radix).is_some()) {
        return invalid();
    }
    let mut decimal = big_from_radix(body, radix);
    if negative != own_negative && decimal != "0" {
        decimal.insert(0, '-');
    }
    Decoded::Token(ScalarToken::Int(Ok(JInt::Big(decimal))))
}

/// `Long.parseLong(s, radix)` (and `Integer.parseInt` for the short strings it is used on): an optional sign, then
/// at least one digit.
fn java_parse_radix(s: &str, radix: u32) -> Option<i64> {
    let (negative, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    if body.is_empty() || !body.chars().all(|c| crate::jnum::digit(c as u32, radix).is_some()) {
        return None;
    }
    let mut v: i64 = 0;
    for c in body.chars() {
        let d = i64::from(crate::jnum::digit(c as u32, radix)?);
        v = v.checked_mul(i64::from(radix))?;
        v = if negative { v.checked_sub(d)? } else { v.checked_add(d)? };
    }
    Some(v)
}

/// Converts digits in `radix` to decimal (schoolbook, on base-1e9 limbs).
fn big_from_radix(digits: &str, radix: u32) -> String {
    let mut limbs: Vec<u64> = vec![0]; // little-endian, base 1e9
    for c in digits.chars() {
        let mut carry = u64::from(crate::jnum::digit(c as u32, radix).unwrap_or(0));
        for limb in &mut limbs {
            let v = *limb * u64::from(radix) + carry;
            *limb = v % 1_000_000_000;
            carry = v / 1_000_000_000;
        }
        while carry > 0 {
            limbs.push(carry % 1_000_000_000);
            carry /= 1_000_000_000;
        }
    }
    let mut out = String::new();
    for (i, limb) in limbs.iter().rev().enumerate() {
        if i == 0 {
            out.push_str(&limb.to_string());
        } else {
            out.push_str(&format!("{limb:09}"));
        }
    }
    let t = out.trim_start_matches('0');
    if t.is_empty() { "0".to_owned() } else { t.to_owned() }
}

/// `String.trim()`: strips code points up to U+0020 at both ends.
pub fn java_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= ' ')
}

/// Jackson's `Base64Variants.MIME.decode(String)`: padding required, whitespace allowed only between quads.
pub fn base64_mime_decode(s: &str) -> Result<Vec<u8>, String> {
    fn value(c: char) -> Option<u32> {
        match c {
            'A'..='Z' => Some(c as u32 - 'A' as u32),
            'a'..='z' => Some(c as u32 - 'a' as u32 + 26),
            '0'..='9' => Some(c as u32 - '0' as u32 + 52),
            '+' => Some(62),
            '/' => Some(63),
            _ => None,
        }
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut p = 0;
    let eof = || Err("unexpected end of base64-encoded String".to_owned());
    let illegal = |c: char| Err(format!("illegal character {:?} in base64 content", c));
    loop {
        let mut c;
        loop {
            if p >= chars.len() {
                return Ok(out);
            }
            c = chars[p];
            p += 1;
            if c > ' ' {
                break;
            }
        }
        let Some(a) = value(c) else { return illegal(c) };
        let Some(&c1) = chars.get(p) else { return eof() };
        p += 1;
        let Some(b) = value(c1) else { return illegal(c1) };
        let mut data = (a << 6) | b;
        let Some(&c2) = chars.get(p) else { return eof() };
        p += 1;
        match value(c2) {
            None => {
                if c2 != '=' {
                    return illegal(c2);
                }
                let Some(&c3) = chars.get(p) else { return eof() };
                p += 1;
                if c3 != '=' {
                    return illegal(c3);
                }
                out.push((data >> 4) as u8);
                continue;
            }
            Some(v) => data = (data << 6) | v,
        }
        let Some(&c3) = chars.get(p) else { return eof() };
        p += 1;
        match value(c3) {
            None => {
                if c3 != '=' {
                    return illegal(c3);
                }
                data >>= 2;
                out.push((data >> 8) as u8);
                out.push(data as u8);
            }
            Some(v) => {
                data = (data << 6) | v;
                out.push((data >> 16) as u8);
                out.push((data >> 8) as u8);
                out.push(data as u8);
            }
        }
    }
}

/// Standard base64 with padding (`Base64Variants.MIME_NO_LINEFEEDS`, Jackson's default variant).
pub fn base64_encode(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.len();
        let v = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(A[(v >> 18) as usize & 63] as char);
        out.push(A[(v >> 12) as usize & 63] as char);
        out.push(if n > 1 { A[(v >> 6) as usize & 63] as char } else { '=' });
        out.push(if n > 2 { A[v as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_patterns() {
        for v in [
            "0",
            "-0",
            "+42",
            "1_2",
            "42_",
            "017",
            "0x_1F",
            "0b101",
            "1:20",
            "190:20:30",
            "-1:20",
        ] {
            assert_eq!(resolve_implicit(v), Implicit::Int, "{v}");
        }
        for v in ["08", "0o17", "0X1F", "0xG", "0b102", "_42", "+", "-", "."] {
            assert_ne!(resolve_implicit(v), Implicit::Int, "{v}");
        }
        for v in [
            "1.5", ".5", "1e3", "5.", "1.5E+3", "1_000.5", ".inf", "-.Inf", ".nan", "1:20.5", "._",
        ] {
            assert_eq!(resolve_implicit(v), Implicit::Float, "{v}");
        }
        for v in ["-.nan", "e3", "1e", "1.5e+", "1:60.5"] {
            assert_ne!(resolve_implicit(v), Implicit::Float, "{v}");
        }
        assert_eq!(resolve_implicit(""), Implicit::Null);
        assert_eq!(resolve_implicit("nULL"), Implicit::Str);
        assert_eq!(resolve_implicit("yES"), Implicit::Str);
    }

    #[test]
    fn base64_round_trip() {
        assert_eq!(base64_mime_decode("QUJDRA==").unwrap(), b"ABCD");
        assert_eq!(base64_encode(b"ABCD"), "QUJDRA==");
        assert!(base64_mime_decode("QUJDRA").is_err());
    }
}
