//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/html/HtmlSummaryReporter.kt
//! and report/allure/AllureReporter.kt (Commons Text and Jackson string boundaries).

use crate::writers::{JavaString, WriterValue};

/// Commons Text 1.12 escapeXml11, including removal of invalid XML code points.
pub fn escape_xml11(text: &JavaString) -> JavaString {
    let mut out = JavaString::default();
    for ch in char::decode_utf16(text.0.iter().copied()).flatten() {
        match ch {
            '\0' | '\u{fffe}' | '\u{ffff}' => {}
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\u{1}'..='\u{8}' | '\u{b}'..='\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}'..='\u{84}' | '\u{86}'..='\u{9f}' => {
                out.push_str("&#");
                out.push_str(&(ch as u32).to_string());
                out.push_str(";");
            }
            _ => out.push_code_point(ch as u32),
        }
    }
    out
}

/// Java URLEncoder with UTF-8; spaces become plus, not percent-20.
pub fn form_encode(text: &JavaString) -> JavaString {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut out = JavaString::default();
    for byte in text.to_utf8_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'*' | b'_' => out.0.push(byte as u16),
            b' ' => out.0.push(b'+' as u16),
            _ => {
                out.0.extend([
                    b'%' as u16,
                    HEX[(byte >> 4) as usize] as u16,
                    HEX[(byte & 15) as usize] as u16,
                ]);
            }
        }
    }
    out
}
pub fn replace(text: &JavaString, pattern: &str, replacement: &JavaString) -> JavaString {
    let pattern: Vec<_> = pattern.encode_utf16().collect();
    let mut out = JavaString::default();
    let mut i = 0;
    while i < text.0.len() {
        if text.0[i..].starts_with(&pattern) {
            out.0.extend(&replacement.0);
            i += pattern.len();
        } else {
            out.0.push(text.0[i]);
            i += 1;
        }
    }
    out
}

/// Restricted Allure/Jackson model writer (P2). Callers decide NON_NULL/NON_EMPTY
/// inclusion; objects such as an empty profiling Link must remain present.
pub fn jackson_json(value: &WriterValue) -> JavaString {
    fn string(s: &JavaString, out: &mut JavaString) {
        const HEX: &[u8] = b"0123456789ABCDEF";
        out.push_str("\"");
        for &u in &s.0 {
            match u {
                34 => out.push_str("\\\""),
                92 => out.push_str("\\\\"),
                8 => out.push_str("\\b"),
                9 => out.push_str("\\t"),
                10 => out.push_str("\\n"),
                12 => out.push_str("\\f"),
                13 => out.push_str("\\r"),
                0..=31 | 0xd800..=0xdfff => {
                    out.push_str("\\u");
                    out.0.extend([
                        HEX[(u >> 12) as usize] as u16,
                        HEX[((u >> 8) & 15) as usize] as u16,
                        HEX[((u >> 4) & 15) as usize] as u16,
                        HEX[(u & 15) as usize] as u16,
                    ]);
                }
                _ => out.0.push(u),
            }
        }
        out.push_str("\"");
    }
    fn write(v: &WriterValue, out: &mut JavaString) {
        match v {
            WriterValue::Null => out.push_str("null"),
            WriterValue::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
            WriterValue::Number(v) => out.push_str(v.as_str()),
            WriterValue::String(v) => string(v, out),
            // Jackson's default variant, MIME_NO_LINEFEEDS; Allure models carry no byte arrays.
            WriterValue::Binary(v) => {
                const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                out.push_str("\"");
                for chunk in v.chunks(3) {
                    let n = chunk
                        .iter()
                        .enumerate()
                        .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
                    for i in 0..4 {
                        out.0.push(if i <= chunk.len() {
                            u16::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize])
                        } else {
                            u16::from(b'=')
                        });
                    }
                }
                out.push_str("\"");
            }
            WriterValue::Array(v) => {
                out.push_str("[");
                for (i, v) in v.iter().enumerate() {
                    if i > 0 {
                        out.push_str(",")
                    }
                    write(v, out)
                }
                out.push_str("]");
            }
            WriterValue::Object(v) => {
                out.push_str("{");
                for (i, (k, v)) in v.iter().enumerate() {
                    if i > 0 {
                        out.push_str(",")
                    }
                    string(k, out);
                    out.push_str(":");
                    write(v, out)
                }
                out.push_str("}");
            }
        }
    }
    let mut out = JavaString::default();
    write(value, &mut out);
    out
}

/// ResultsUtils.generateMethodSignatureHash updates MD5 with class bytes then method bytes (no separators).
pub fn allure_history_id(class: &JavaString, method: &JavaString) -> String {
    let mut signature = class.clone();
    signature.append(method.clone());
    format!("{:x}", md5::compute(signature.to_utf8_bytes()))
}

/// Why the JDK's Xalan serializer rejects a text node (environment-writer catches it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmlTextError {
    /// SAXException: a control character or an unpaired low surrogate.
    InvalidCharacter(u16),
    /// IOException wrapped in SAXException: a high surrogate followed by a non-low unit.
    InvalidSurrogate(u16, u16),
}
impl XmlTextError {
    /// The SAXException message; InvalidSurrogate's is the wrapped IOException's toString.
    pub fn sax_message(self) -> String {
        match self {
            Self::InvalidCharacter(u) => {
                format!("An invalid XML character (Unicode: 0x{u:x}) was found in the node's character data content.")
            }
            Self::InvalidSurrogate(..) => format!("java.io.IOException: {}", self.io_message().unwrap_or_default()),
        }
    }
    pub fn io_message(self) -> Option<String> {
        match self {
            Self::InvalidSurrogate(high, next) => {
                Some(format!("Invalid UTF-16 surrogate detected: {high:x} {next:x} ?"))
            }
            Self::InvalidCharacter(_) => None,
        }
    }
}

/// DOM text-node serialization used by environment-writer's identity transform
/// (JBR 21 Xalan ToStream, XML 1.0, UTF-8), mapped over every BMP unit on the JVM.
/// A high surrogate ending the node is dropped without error.
pub fn xml_text(text: &JavaString) -> Result<JavaString, XmlTextError> {
    let mut out = JavaString::default();
    let mut i = 0;
    while i < text.0.len() {
        let u = text.0[i];
        match u {
            0xd800..=0xdbff => match text.0.get(i + 1) {
                Some(&low @ 0xdc00..=0xdfff) => {
                    let code = 0x10000 + ((u32::from(u) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
                    out.push_str(&format!("&#{code};"));
                    i += 1;
                }
                Some(&next) => return Err(XmlTextError::InvalidSurrogate(u, next)),
                None => {}
            },
            0xdc00..=0xdfff => return Err(XmlTextError::InvalidCharacter(u)),
            38 => out.push_str("&amp;"),
            60 => out.push_str("&lt;"),
            62 => out.push_str("&gt;"),
            9 | 10 => out.0.push(u),
            13 | 0x7f..=0x9f => out.push_str(&format!("&#{u};")),
            0..=0x1f => return Err(XmlTextError::InvalidCharacter(u)),
            _ => out.0.push(u),
        }
        i += 1;
    }
    Ok(out)
}

/// AWT ScaledBlit nearest-neighbour sampling: the source is walked in 32-bit fixed
/// point from a truncated half step by a truncated increment, so exact ties round up
/// on 2→1 but down on 2→3. Matches every map in the JVM `resize-axis-*` rows.
pub fn awt_nearest_index(destination: u32, source_size: u32, target_size: u32) -> u32 {
    let scaled = u128::from(source_size) << 32;
    let increment = scaled / u128::from(target_size);
    let start = scaled / (2 * u128::from(target_size));
    ((start + u128::from(destination) * increment) >> 32) as u32
}
