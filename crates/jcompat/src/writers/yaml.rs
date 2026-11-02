//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/command/parse/MarathonTestParseCommand.kt.
//! Formatting follows Jackson 2.17.2 YAMLGenerator and SnakeYAML 2.2 Emitter.
//! SnakeYAML portions: Copyright (c) 2008, SnakeYAML; Apache-2.0, see LICENSE-SNAKEYAML.
use super::{JavaString, WriterValue};

#[derive(Clone, Copy, Debug, Default)]
pub struct JacksonYamlWriter;
impl JacksonYamlWriter {
    pub fn write(&self, value: &WriterValue) -> JavaString {
        let mut out = Output {
            text: JavaString::default(),
            column: 0,
        };
        out.push("---");
        if collection(value) {
            out.push("\n");
            block(value, 0, "", &mut out);
        } else {
            out.push(" ");
            scalar(value, 2, &mut out);
            out.push("\n");
        }
        out.text
    }
}
struct Output {
    text: JavaString,
    column: usize,
}
impl Output {
    fn push(&mut self, s: &str) {
        self.units(&s.encode_utf16().collect::<Vec<_>>());
    }
    fn units(&mut self, s: &[u16]) {
        for &c in s {
            self.column = if c == 10 { 0 } else { self.column + 1 };
        }
        self.text.0.extend_from_slice(s);
    }
}
fn entries(v: &[(JavaString, WriterValue)]) -> impl Iterator<Item = &(JavaString, WriterValue)> {
    v.iter().filter(|(_, v)| !matches!(v, WriterValue::Null))
}
fn collection(value: &WriterValue) -> bool {
    match value {
        WriterValue::Object(e) => entries(e).next().is_some(),
        WriterValue::Array(v) => !v.is_empty(),
        _ => false,
    }
}
fn block(value: &WriterValue, indent: usize, prefix: &str, out: &mut Output) {
    match value {
        WriterValue::Object(e) => {
            for (i, (key, value)) in entries(e).enumerate() {
                out.push(if i == 0 { prefix } else { "" });
                if out.column == 0 {
                    out.push(&" ".repeat(indent));
                }
                // SnakeYAML excludes CR from the breaks that make a scalar multiline.
                let explicit = key.0.is_empty()
                    || key.0.len() >= 128
                    || key.0.iter().any(|c| matches!(c, 10 | 0x85 | 0x2028 | 0x2029));
                if explicit {
                    out.push("? ");
                }
                key_scalar(key, indent + 2, explicit, out);
                if explicit {
                    out.push("\n");
                    out.push(&" ".repeat(indent));
                }
                out.push(":");
                after_indicator(value, indent, explicit, out);
            }
        }
        WriterValue::Array(v) => {
            for (i, value) in v.iter().enumerate() {
                if i == 0 {
                    out.push(prefix);
                }
                if out.column == 0 {
                    out.push(&" ".repeat(indent));
                }
                out.push("-");
                if collection(value) {
                    out.push(" ");
                    block(value, indent + 2, "", out);
                } else {
                    out.push(" ");
                    scalar(value, indent + 2, out);
                    out.push("\n");
                }
            }
        }
        _ => unreachable!("block called only for nonempty collections"),
    }
}
fn after_indicator(value: &WriterValue, indent: usize, explicit: bool, out: &mut Output) {
    if collection(value) {
        if explicit {
            out.push(" ");
            block(value, indent + 2, "", out);
            return;
        }
        out.push("\n");
        let next = if matches!(value, WriterValue::Array(_)) {
            indent
        } else {
            indent + 2
        };
        block(value, next, "", out);
    } else {
        out.push(" ");
        scalar(value, indent + 2, out);
        out.push("\n");
    }
}
fn scalar(value: &WriterValue, indent: usize, out: &mut Output) {
    match value {
        WriterValue::Null => out.push("null"),
        WriterValue::Bool(b) => out.push(if *b { "true" } else { "false" }),
        WriterValue::Number(n) => out.push(n.as_str()),
        WriterValue::String(s) => quoted(s, indent, true, out),
        WriterValue::Binary(bytes) => binary(bytes, indent, out),
        WriterValue::Array(_) => out.push("[]"),
        WriterValue::Object(_) => out.push("{}"),
    }
}
fn key_scalar(key: &JavaString, indent: usize, split: bool, out: &mut Output) {
    let text = key.as_utf8().ok();
    let reserved = text.as_deref().is_some_and(|s| {
        matches!(
            s,
            "" | "false"
                | "False"
                | "FALSE"
                | "n"
                | "N"
                | "no"
                | "No"
                | "NO"
                | "null"
                | "Null"
                | "NULL"
                | "on"
                | "On"
                | "ON"
                | "off"
                | "Off"
                | "OFF"
                | "true"
                | "True"
                | "TRUE"
                | "y"
                | "Y"
                | "yes"
                | "Yes"
                | "YES"
        ) || s.starts_with('~')
    });
    let forced =
        reserved || key.0.first().is_some_and(|c| matches!(c, 43 | 45 | 46 | 48..=57)) || key.0.iter().any(|c| *c < 32);
    if forced || text.is_none() {
        quoted(key, indent, split, out);
        return;
    }
    let s = text.as_deref().unwrap_or_default();
    let whitespace = |c: char| matches!(c, '\0' | ' ' | '\t' | '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}');
    let chars: Vec<char> = s.chars().collect();
    let special = chars
        .iter()
        .any(|c| !matches!(*c as u32, 0x20..=0x7e | 0x85 | 0xa0..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff));
    let linebreak = |c: char| matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}');
    if chars
        .windows(2)
        .any(|p| (p[0] == ' ' && linebreak(p[1])) || (linebreak(p[0]) && p[1] == ' '))
    {
        quoted(key, indent, split, out);
        return;
    }
    let leading_indicator =
        chars.first().is_some_and(|c| matches!(c, '?' | ':' | '-')) && chars.get(1).is_none_or(|c| whitespace(*c));
    let plain = !leading_indicator
        && !s.starts_with([' ', '\u{85}', '\u{2028}', '\u{2029}'])
        && !s.ends_with([' ', '\u{85}', '\u{2028}', '\u{2029}'])
        && !s.starts_with([
            '#', ',', '[', ']', '{', '}', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`',
        ])
        && !s.starts_with("---")
        && !s.starts_with("...")
        && !special
        && !chars.iter().enumerate().any(|(i, c)| {
            (*c == ':'
                && (if i == 0 {
                    chars.get(i + 1).is_none_or(|c| whitespace(*c))
                } else {
                    // SnakeYAML uses the preceding iteration's isLineBreak here.
                    chars.get(i + 1).is_none_or(|c| matches!(c, '\0' | ' ' | '\t')) || linebreak(chars[i - 1])
                }))
                || (*c == '#' && (i == 0 || whitespace(chars[i - 1])))
        });
    if plain {
        plain_or_single(key, indent, split, false, out);
    } else if !special {
        plain_or_single(key, indent, split, true, out);
    } else {
        quoted(key, indent, split, out);
    }
}

/// SnakeYAML iterates UTF-16 chars here, including its unchecked high-surrogate
/// combination with the next char. Keep that behaviour rather than normalizing.
fn quoted(text: &JavaString, indent: usize, split: bool, out: &mut Output) {
    out.push("\"");
    let mut start = 0;
    let mut end = 0;
    while end <= text.0.len() {
        let ch = text.0.get(end).copied();
        if ch.is_none_or(|c| matches!(c, 0x22 | 0x5c | 0x85 | 0x2028 | 0x2029 | 0xfeff) || !(0x20..=0x7e).contains(&c))
        {
            if start < end {
                out.units(&text.0[start..end]);
                start = end;
            }
            if let Some(c) = ch {
                let escape = match c {
                    0 => Some("0"),
                    7 => Some("a"),
                    8 => Some("b"),
                    9 => Some("t"),
                    10 => Some("n"),
                    11 => Some("v"),
                    12 => Some("f"),
                    13 => Some("r"),
                    27 => Some("e"),
                    34 => Some("\""),
                    92 => Some("\\"),
                    0x85 => Some("N"),
                    0xa0 => Some("_"),
                    0x2028 => Some("L"),
                    0x2029 => Some("P"),
                    _ => None,
                };
                if let Some(e) = escape {
                    out.push("\\");
                    out.push(e);
                } else {
                    let cp = if (0xd800..=0xdbff).contains(&c) && end + 1 < text.0.len() {
                        (u32::from(c) - 0xd800) * 1024 + u32::from(text.0[end + 1]) + 0x2400
                    } else {
                        u32::from(c)
                    };
                    if matches!(cp, 0x20..=0x7e | 0xa0..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff) {
                        if let Some(c) = char::from_u32(cp) {
                            out.push(&c.to_string());
                        }
                        if cp >= 0x10000 {
                            end += 1;
                        }
                    } else if c <= 0xff {
                        out.push(&format!("\\x{c:02x}"));
                    } else if cp >= 0x10000 {
                        end += 1;
                        out.push(&format!("\\U{cp:08x}"));
                    } else {
                        out.push(&format!("\\u{c:04x}"));
                    }
                }
                start = end + 1;
            }
        }
        if end > 0
            && end + 1 < text.0.len()
            && (ch == Some(32) || start >= end)
            && out.column as isize + end as isize - start as isize > 80
            && split
        {
            if start < end {
                out.units(&text.0[start..end]);
                start = end;
            }
            out.push("\\\n");
            out.push(&" ".repeat(indent));
            if text.0.get(start) == Some(&32) {
                out.push("\\");
            }
        }
        end += 1;
    }
    out.push("\"");
}

fn plain_or_single(text: &JavaString, indent: usize, split: bool, single: bool, out: &mut Output) {
    if single {
        out.push("'");
    }
    let mut spaces = false;
    let mut breaks = false;
    let mut start = 0;
    let mut end = 0;
    let linebreak = |c: u16| matches!(c, 10 | 13 | 0x85 | 0x2028 | 0x2029);
    while end <= text.0.len() {
        let ch = text.0.get(end).copied().unwrap_or(0);
        if spaces {
            if ch != 32 {
                if start + 1 == end && out.column > 80 && split && (!single || (start != 0 && end != text.0.len())) {
                    out.push("\n");
                    out.push(&" ".repeat(indent));
                } else {
                    out.units(&text.0[start..end]);
                }
                start = end;
            }
        } else if breaks {
            if !linebreak(ch) {
                if text.0[start] == 10 {
                    out.push("\n");
                }
                for &c in &text.0[start..end] {
                    out.units(&[c]);
                    out.column = 0;
                }
                out.push(&" ".repeat(indent));
                start = end;
            }
        } else if ch == 0 || ch == 32 || linebreak(ch) || (single && ch == 39) {
            out.units(&text.0[start..end]);
            start = end;
        }
        if single && ch == 39 {
            out.push("''");
            start = end + 1;
        }
        if ch != 0 {
            spaces = ch == 32;
            breaks = linebreak(ch);
        }
        end += 1;
    }
    if single {
        out.push("'");
    }
}

fn binary(bytes: &[u8], indent: usize, out: &mut Output) {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for shift in [18, 12] {
            encoded.push(ALPHABET[((n >> shift) & 63) as usize] as char);
        }
        encoded.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    if encoded.is_empty() {
        out.push("!!binary \"\"");
        return;
    }
    out.push(if bytes.len().is_multiple_of(57) {
        "!!binary |\n"
    } else {
        "!!binary |-\n"
    });
    for (i, line) in encoded.as_bytes().chunks(76).enumerate() {
        if i > 0 {
            out.push("\n");
        }
        out.push(&" ".repeat(indent));
        // Base64 is ASCII.
        for &c in line {
            out.units(&[u16::from(c)]);
        }
    }
}
