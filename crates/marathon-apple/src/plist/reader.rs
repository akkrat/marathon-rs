//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/plist/PropertyList.kt
//!
//! XML reading, mirroring what dd-plist's SAX-based `XMLPropertyListParser` accepts on the
//! paths marathon feeds it (probes `cdata-read`, `encodings`, `control-on-parse`). The
//! `plist` crate is not used for XML: its reader silently drops CDATA sections
//! (`plist` 1.10.3 `stream/xml_reader.rs` skips `XmlEvent::CData` in `read_content`), so
//! a value our own writer CDATA-wraps — anything containing `&`, `<` or `>` — read back
//! empty. Binary plists still go through the `plist` crate in `super`.

use quick_xml::Reader;
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::{BytesRef, Event};

use super::{PlistDict, PlistError, PlistResult, PlistValue};

fn not_a_plist(message: impl Into<String>) -> PlistError {
    PlistError::NotAPlist {
        message: message.into(),
    }
}

/// XML 1.0's `Char` production, which the SAX parser enforces on literal characters and
/// character references alike (probe `control-on-parse`).
fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r') || matches!(c as u32, 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)
}

fn decode_utf16(bytes: &[u8], big_endian: bool) -> PlistResult<String> {
    if !bytes.len().is_multiple_of(2) {
        return Err(not_a_plist("odd byte count in UTF-16 plist"));
    }
    let units = bytes.as_chunks::<2>().0.iter().map(|pair| {
        if big_endian {
            u16::from_be_bytes([pair[0], pair[1]])
        } else {
            u16::from_le_bytes([pair[0], pair[1]])
        }
    });
    Ok(char::decode_utf16(units).map(|c| c.unwrap_or('�')).collect())
}

/// The `encoding` pseudo-attribute of a leading `<?xml ... ?>` declaration (the
/// declaration itself is ASCII in every encoding handled here).
fn declared_encoding(head: &str) -> Option<String> {
    let decl = head.trim_start().strip_prefix("<?xml")?;
    let decl = &decl[..decl.find("?>")?];
    let lower = decl.to_ascii_lowercase();
    let after = &decl[lower.find("encoding")? + "encoding".len()..];
    let after = after.trim_start().strip_prefix('=')?.trim_start();
    let mut chars = after.chars();
    let quote = chars.next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value: String = chars.take_while(|c| *c != quote).collect();
    Some(value.to_ascii_uppercase())
}

/// Decode an XML plist to text the way the JDK's SAX front end does under dd-plist (probes
/// `encodings`, `encoding-matrix`). The BOM, else the byte pattern of `<?`, only picks the
/// family; the declaration then decides. A UTF-16 document reads only when it declares
/// `UTF-16` or its own byte order; with no declaration, or any other name, the parser
/// fails. An 8-bit document reads per its declaration
/// even after a UTF-8 BOM (UTF-8 BOM + `ISO-8859-1` gives `cafÃ©`): UTF-8 (also when
/// undeclared), ISO-8859-1, US-ASCII (a byte above 0x7f is an error). Names match
/// case-insensitively, as the parser upper-cases them. Forbidden XML characters are a
/// parse error, and CR/CRLF normalise to LF as XML line-ending handling prescribes
/// (probe `cdata-read`: a written `a\rb\r\nc` reads back `a\nb\nc`).
fn decode_xml(bytes: &[u8]) -> PlistResult<String> {
    if bytes.starts_with(&[0xff, 0xfe, 0x00, 0x00])
        || bytes.starts_with(&[0x00, 0x00, 0xfe, 0xff])
        || bytes.starts_with(&[0x00, 0x00, 0x00, b'<'])
        || bytes.starts_with(&[b'<', 0x00, 0x00, 0x00])
    {
        return Err(not_a_plist("UTF-32 plist"));
    }
    let (utf16_big_endian, body) = if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf][..]) {
        (None, rest)
    } else if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe][..]) {
        (Some(false), rest)
    } else if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff][..]) {
        (Some(true), rest)
    } else if bytes.starts_with(&[0x00, b'<', 0x00, b'?']) {
        (Some(true), bytes)
    } else if bytes.starts_with(&[b'<', 0x00, b'?', 0x00]) {
        (Some(false), bytes)
    } else {
        (None, bytes)
    };
    let decoded = match utf16_big_endian {
        Some(big_endian) => {
            let text = decode_utf16(body, big_endian)?;
            let own_order = if big_endian { "UTF-16BE" } else { "UTF-16LE" };
            match declared_encoding(&text) {
                Some(name) if name == "UTF-16" || name == own_order => text,
                _ => return Err(not_a_plist("UTF-16 plist without a matching encoding declaration")),
            }
        }
        None => {
            let head: String = body[..body.len().min(1024)].iter().map(|b| *b as char).collect();
            match declared_encoding(&head).as_deref() {
                None | Some("UTF-8") => String::from_utf8(body.to_vec()).map_err(|e| not_a_plist(e.to_string()))?,
                Some("ISO-8859-1" | "ISO8859-1") => body.iter().map(|b| *b as char).collect(),
                Some("US-ASCII" | "ASCII") => {
                    if let Some(byte) = body.iter().find(|b| !b.is_ascii()) {
                        return Err(not_a_plist(format!("byte {byte} is not 7-bit ASCII")));
                    }
                    body.iter().map(|b| *b as char).collect()
                }
                Some(other) => return Err(not_a_plist(format!("unsupported XML encoding {other}"))),
            }
        }
    };
    let decoded = decoded.strip_prefix('\u{feff}').map_or(decoded.as_str(), |rest| rest);
    if let Some(bad) = decoded.chars().find(|c| !is_xml_char(*c)) {
        return Err(not_a_plist(format!("invalid XML character U+{:04X}", bad as u32)));
    }
    Ok(decoded.replace("\r\n", "\n").replace('\r', "\n"))
}

fn surrogate_ref(reference: &BytesRef<'_>) -> Option<u16> {
    let raw: &str = reference.as_ref();
    let unit = raw
        .strip_prefix("#x")
        .and_then(|s| u32::from_str_radix(s, 16).ok())
        .or_else(|| raw.strip_prefix('#').and_then(|s| s.parse::<u32>().ok()))?;
    (0xd800..=0xdfff).contains(&unit).then_some(unit as u16)
}

fn resolve_ref(reference: &BytesRef<'_>) -> PlistResult<String> {
    if surrogate_ref(reference).is_some() {
        return Ok("�".into());
    }
    match reference.resolve_char_ref().map_err(|e| not_a_plist(e.to_string()))? {
        Some(ch) => {
            if is_xml_char(ch) {
                Ok(ch.to_string())
            } else {
                Err(not_a_plist(format!(
                    "character reference to invalid XML character U+{:04X}",
                    ch as u32
                )))
            }
        }
        None => resolve_xml_entity(reference.as_ref())
            .map(str::to_owned)
            .ok_or_else(|| not_a_plist(format!("unknown XML entity &{};", reference.as_ref()))),
    }
}

fn base64_decode(text: &str) -> PlistResult<Vec<u8>> {
    let mut table = [255u8; 256];
    for (i, c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
        .iter()
        .enumerate()
    {
        table[*c as usize] = i as u8;
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in text.bytes() {
        if c == b'=' {
            continue;
        }
        let value = table[c as usize];
        if value == 255 {
            return Err(not_a_plist("invalid base64 in <data>"));
        }
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// Java's `String.trim`: dd-plist's number parsing falls back to `Double.parseDouble`,
/// which trims, so `<integer> 42 </integer>` and `<real> 2.5 </real>` parse (probe
/// `number-text`). The hex form is matched on the untrimmed text, as dd-plist checks
/// `startsWith("0x")` before any trimming: ` 0x10 ` is rejected there too.
fn java_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c <= ' ')
}

fn parse_integer(text: &str) -> PlistResult<i128> {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16)
            .map(i128::from)
            .map_err(|_| not_a_plist(format!("invalid integer {text:?}")));
    }
    let trimmed = java_trim(text);
    trimmed
        .parse::<i64>()
        .map(i128::from)
        .or_else(|_| trimmed.parse::<u64>().map(i128::from))
        .map_err(|_| not_a_plist(format!("invalid integer {text:?}")))
}

/// dd-plist parses dates with `SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss'Z'")`, so a
/// fractional second or an offset fails (probe `date-text`); only the whole-second UTC
/// form is handed on.
fn is_whole_second_utc_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 20
        && bytes.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            10 => *b == b'T',
            13 | 16 => *b == b':',
            19 => *b == b'Z',
            _ => b.is_ascii_digit(),
        })
}

struct Parser<'a> {
    reader: Reader<&'a [u8]>,
}

impl<'a> Parser<'a> {
    fn next_event(&mut self) -> PlistResult<Event<'a>> {
        self.reader.read_event().map_err(|e| not_a_plist(e.to_string()))
    }

    /// The next structural event: declarations, comments, processing instructions and
    /// inter-element whitespace are skipped; real character data where an element is
    /// expected is a parse error (as with the `plist` crate's streaming reader).
    fn next_structural(&mut self) -> PlistResult<Event<'a>> {
        loop {
            match self.next_event()? {
                Event::Decl(_) | Event::DocType(_) | Event::Comment(_) | Event::PI(_) | Event::CData(_) => {}
                Event::Text(text) => {
                    let text: &str = text.as_ref();
                    if !text.chars().all(char::is_whitespace) {
                        return Err(not_a_plist("character data where an element was expected"));
                    }
                }
                Event::GeneralRef(reference) => {
                    if !resolve_ref(&reference)?.chars().all(char::is_whitespace) {
                        return Err(not_a_plist("character data where an element was expected"));
                    }
                }
                event => return Ok(event),
            }
        }
    }

    /// The text of a scalar element, up to its end tag: plain text, entity and character
    /// references, and CDATA sections concatenate (`getWholeText` in dd-plist).
    fn read_content(&mut self) -> PlistResult<String> {
        let mut content = Vec::new();
        loop {
            match self.next_event()? {
                Event::Text(text) => content.extend(text.as_ref().encode_utf16()),
                Event::CData(cdata) => content.extend(cdata.as_ref().encode_utf16()),
                Event::GeneralRef(reference) => {
                    if let Some(unit) = surrogate_ref(&reference) {
                        content.push(unit);
                    } else {
                        content.extend(resolve_ref(&reference)?.encode_utf16());
                    }
                }
                Event::End(_) => return Ok(String::from_utf16_lossy(&content)),
                Event::Eof => return Err(not_a_plist("unclosed XML element")),
                Event::Start(_) => return Err(not_a_plist("nested element in scalar value")),
                Event::Empty(_) => return Err(not_a_plist("nested element in scalar value")),
                Event::Decl(_) | Event::DocType(_) | Event::Comment(_) | Event::PI(_) => {}
            }
        }
    }

    /// `<true>`/`<false>` carry no value in dd-plist: their content, nested elements
    /// included, is ignored up to the end tag, but it is still parsed, so a forbidden
    /// character reference or an undeclared entity fails as before (probe `boolean-content`).
    fn skip_to_end(&mut self) -> PlistResult<()> {
        let mut depth = 0usize;
        loop {
            match self.next_event()? {
                Event::Start(_) => depth += 1,
                Event::End(_) if depth == 0 => return Ok(()),
                Event::End(_) => depth -= 1,
                Event::GeneralRef(reference) => {
                    resolve_ref(&reference)?;
                }
                Event::Eof => return Err(not_a_plist("unclosed XML element")),
                _ => {}
            }
        }
    }

    fn parse_value(&mut self) -> PlistResult<PlistValue> {
        match self.next_structural()? {
            Event::Start(start) => self.parse_value_from(start.name().local_name().as_ref()),
            Event::Eof => Err(not_a_plist("unexpected end of plist")),
            _ => Err(not_a_plist("expected a plist value element")),
        }
    }

    fn parse_value_from(&mut self, name: &str) -> PlistResult<PlistValue> {
        match name {
            "dict" => {
                let dict = PlistDict::new();
                loop {
                    match self.next_structural()? {
                        Event::End(_) => return Ok(PlistValue::Dict(dict)),
                        Event::Start(start) if start.name().local_name().as_ref() == "key" => {
                            let key = self.read_content()?;
                            let value = self.parse_value()?;
                            // Duplicate keys: last value at the first position (dd-plist).
                            dict.insert(&key, value);
                        }
                        Event::Eof => return Err(not_a_plist("unclosed <dict>")),
                        _ => return Err(not_a_plist("expected <key> in <dict>")),
                    }
                }
            }
            "array" => {
                let mut items = Vec::new();
                loop {
                    match self.next_structural()? {
                        Event::End(_) => return Ok(PlistValue::Array(items)),
                        Event::Start(start) => {
                            items.push(self.parse_value_from(start.name().local_name().as_ref())?);
                        }
                        Event::Eof => return Err(not_a_plist("unclosed <array>")),
                        _ => return Err(not_a_plist("expected a value element in <array>")),
                    }
                }
            }
            "string" => Ok(PlistValue::String(self.read_content()?)),
            "data" => {
                let mut text = self.read_content()?;
                text.retain(|c| !c.is_ascii_whitespace());
                Ok(PlistValue::Data(base64_decode(&text)?))
            }
            "date" => {
                let text = self.read_content()?;
                Some(&text)
                    .filter(|text| is_whole_second_utc_date(text))
                    .and_then(|text| plist::Date::from_xml_format(text).ok())
                    .map(PlistValue::Date)
                    .ok_or_else(|| not_a_plist(format!("invalid date {text:?}")))
            }
            // The tag, not the text, decides int vs real here; dd-plist infers from the
            // text instead (its `NSNumber(String)`: `<real>1</real>` is an integer there,
            // `<integer> 42 </integer>` a real), an approved simplification recorded in
            // the WP 4.5a findings (review round 2).
            "integer" => Ok(PlistValue::Integer(parse_integer(&self.read_content()?)?)),
            "real" => {
                let text = self.read_content()?;
                java_trim(&text)
                    .parse::<f64>()
                    .map(PlistValue::Real)
                    .map_err(|_| not_a_plist(format!("invalid real {text:?}")))
            }
            "true" => {
                self.skip_to_end()?;
                Ok(PlistValue::Bool(true))
            }
            "false" => {
                self.skip_to_end()?;
                Ok(PlistValue::Bool(false))
            }
            other => Err(not_a_plist(format!("unknown plist element <{other}>"))),
        }
    }
}

pub(super) fn parse_xml_value(bytes: &[u8]) -> PlistResult<PlistValue> {
    let text = decode_xml(bytes)?;
    let mut reader = Reader::from_str(&text);
    let config = reader.config_mut();
    config.trim_text(false);
    config.check_end_names = true;
    config.expand_empty_elements = true;
    let mut parser = Parser { reader };

    let value = match parser.next_structural()? {
        Event::Start(start) if start.name().local_name().as_ref() == "plist" => {
            let value = parser.parse_value()?;
            match parser.next_structural()? {
                Event::End(_) => value,
                Event::Eof => return Err(not_a_plist("unclosed <plist>")),
                _ => return Err(not_a_plist("more than one root element")),
            }
        }
        Event::Start(start) => parser.parse_value_from(start.name().local_name().as_ref())?,
        Event::Eof => return Err(not_a_plist("no root element")),
        _ => return Err(not_a_plist("expected a plist root element")),
    };
    match parser.next_structural()? {
        Event::Eof => Ok(value),
        _ => Err(not_a_plist("content after the plist root element")),
    }
}

#[cfg(test)]
mod native_string_tests {
    use super::*;

    #[test]
    fn xml_encodings_and_numeric_escapes_decode_lossily() -> PlistResult<()> {
        let expected = "�x�😀";
        let escaped = "<plist><dict><key>k</key><string>&#xD800;x&#56320;&#xD83D;&#xDE00;</string></dict></plist>";
        let super::PlistValue::Dict(dict) = parse_xml_value(escaped.as_bytes())? else {
            panic!("dict")
        };
        assert_eq!(dict.get_cloned("k"), Some(super::PlistValue::String(expected.into())));
        for big_endian in [false, true] {
            let mut units: Vec<u16> = r#"<?xml version="1.0" encoding="UTF-16"?><plist><dict><key>k</key><string>"#
                .encode_utf16()
                .collect();
            units.extend([0xd800, 120, 0xdc00, 0xd83d, 0xde00]);
            units.extend("</string></dict></plist>".encode_utf16());
            let mut bytes = if big_endian { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
            bytes.extend(
                units
                    .into_iter()
                    .flat_map(|u| if big_endian { u.to_be_bytes() } else { u.to_le_bytes() }),
            );
            let super::PlistValue::Dict(dict) = parse_xml_value(&bytes)? else {
                panic!("dict")
            };
            assert_eq!(dict.get_cloned("k"), Some(super::PlistValue::String(expected.into())));
        }
        Ok(())
    }
}
