//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/html/HtmlSummaryReporter.kt
//! and report/allure/AllureReporter.kt (Commons Text and Jackson string boundaries).

use crate::writers::WriterValue;

/// Commons Text 1.12 escapeXml11, including removal of invalid XML code points.
pub fn escape_xml11(text: &str) -> String {
    let mut out = String::default();
    for ch in text.chars() {
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
                out.push(';');
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Java URLEncoder with UTF-8; spaces become plus, not percent-20.
pub fn form_encode(text: &str) -> String {
    form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

pub fn replace(text: &str, pattern: &str, replacement: &str) -> String {
    text.replace(pattern, replacement)
}

/// Restricted Allure/Jackson model writer (P2). Callers decide NON_NULL/NON_EMPTY
/// inclusion; objects such as an empty profiling Link must remain present.
pub fn jackson_json(value: &WriterValue) -> String {
    fn string(s: &str, out: &mut String) {
        const HEX: &[u8] = b"0123456789ABCDEF";
        out.push('"');
        for u in s.encode_utf16() {
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
                    out.extend([
                        HEX[(u >> 12) as usize] as char,
                        HEX[((u >> 8) & 15) as usize] as char,
                        HEX[((u >> 4) & 15) as usize] as char,
                        HEX[(u & 15) as usize] as char,
                    ]);
                }
                _ => out.push(char::from_u32(u32::from(u)).unwrap()),
            }
        }
        out.push('"');
    }
    fn write(v: &WriterValue, out: &mut String) {
        match v {
            WriterValue::Null => out.push_str("null"),
            WriterValue::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
            WriterValue::Number(v) => out.push_str(v.as_str()),
            WriterValue::String(v) => string(v, out),
            // Jackson's default variant, MIME_NO_LINEFEEDS; Allure models carry no byte arrays.
            WriterValue::Binary(v) => {
                const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                out.push('"');
                for chunk in v.chunks(3) {
                    let n = chunk
                        .iter()
                        .enumerate()
                        .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
                    for i in 0..4 {
                        out.push(if i <= chunk.len() {
                            char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize])
                        } else {
                            '='
                        });
                    }
                }
                out.push('"');
            }
            WriterValue::Array(v) => {
                out.push('[');
                for (i, v) in v.iter().enumerate() {
                    if i > 0 {
                        out.push(',')
                    }
                    write(v, out)
                }
                out.push(']');
            }
            WriterValue::Object(v) => {
                out.push('{');
                for (i, (k, v)) in v.iter().enumerate() {
                    if i > 0 {
                        out.push(',')
                    }
                    string(k, out);
                    out.push(':');
                    write(v, out)
                }
                out.push('}');
            }
        }
    }
    let mut out = String::default();
    write(value, &mut out);
    out
}

/// ResultsUtils.generateMethodSignatureHash updates MD5 with class bytes then method bytes (no separators).
pub fn allure_history_id(class: &str, method: &str) -> String {
    let mut signature = class.to_owned();
    signature.push_str(method);
    format!("{:x}", md5::compute(signature.as_bytes()))
}

/// Why the JDK's Xalan serializer rejects a text node (environment-writer catches it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmlTextError {
    /// SAXException: a control character.
    InvalidCharacter(u16),
}
impl XmlTextError {
    /// The SAXException message.
    pub fn sax_message(self) -> String {
        match self {
            Self::InvalidCharacter(u) => {
                format!("An invalid XML character (Unicode: 0x{u:x}) was found in the node's character data content.")
            }
        }
    }
}

/// DOM text-node serialization used by environment-writer's identity transform.
pub fn xml_text(text: &str) -> Result<String, XmlTextError> {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' | '\n' => out.push(c),
            '\r' | '\u{7f}'..='\u{9f}' | '\u{10000}'..='\u{10ffff}' => out.push_str(&format!("&#{};", c as u32)),
            '\0'..='\u{1f}' => return Err(XmlTextError::InvalidCharacter(c as u16)),
            _ => out.push(c),
        }
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
