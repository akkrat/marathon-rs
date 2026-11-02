//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/plist/PropertyList.kt
//!
//! The byte-exact half: this reproduces dd-plist 1.28's `XMLPropertyListWriter` (which
//! `saveAsXML` delegates to) byte for byte, as pinned by the `expect_*.xctestrun` goldens,
//! the 0.4b pushed-xctestrun evidence and `tools/oracle/plist-probes`. The `plist` crate's
//! own writer is never used (PLAN §4.2).

use std::io::Write;

use super::{PlistDict, PlistResult, PlistValue};

const HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n";
const FOOTER: &str = "\n</plist>";

pub fn write_dict_xml<W: Write>(dict: &PlistDict, mut out: W) -> PlistResult<()> {
    out.write_all(HEADER.as_bytes())?;
    write_dict(dict, 0, &mut out)?;
    out.write_all(FOOTER.as_bytes())?;
    Ok(())
}

/// Any value as a whole document; dd-plist writes scalar roots too (probe `writer-root-string`).
pub fn write_value_xml<W: Write>(value: &PlistValue, mut out: W) -> PlistResult<()> {
    out.write_all(HEADER.as_bytes())?;
    write_value(value, 0, &mut out)?;
    out.write_all(FOOTER.as_bytes())?;
    Ok(())
}

fn indent<W: Write>(level: usize, out: &mut W) -> PlistResult<()> {
    for _ in 0..level {
        out.write_all(b"\t")?;
    }
    Ok(())
}

fn write_dict<W: Write>(dict: &PlistDict, level: usize, out: &mut W) -> PlistResult<()> {
    indent(level, out)?;
    out.write_all(b"<dict>\n")?;
    for (key, value) in dict.snapshot() {
        indent(level + 1, out)?;
        out.write_all(b"<key>")?;
        write_key(&key, out)?;
        out.write_all(b"</key>\n")?;
        write_value(&value, level + 1, out)?;
        out.write_all(b"\n")?;
    }
    indent(level, out)?;
    out.write_all(b"</dict>")?;
    Ok(())
}

fn write_value<W: Write>(value: &PlistValue, level: usize, out: &mut W) -> PlistResult<()> {
    match value {
        PlistValue::Dict(dict) => write_dict(dict, level, out),
        PlistValue::Array(items) => {
            indent(level, out)?;
            out.write_all(b"<array>\n")?;
            for item in items {
                write_value(item, level + 1, out)?;
                out.write_all(b"\n")?;
            }
            indent(level, out)?;
            out.write_all(b"</array>")?;
            Ok(())
        }
        PlistValue::String(s) => {
            indent(level, out)?;
            out.write_all(b"<string>")?;
            write_escaped(s, out)?;
            out.write_all(b"</string>")?;
            Ok(())
        }
        PlistValue::Integer(i) => {
            indent(level, out)?;
            write!(out, "<integer>{i}</integer>")?;
            Ok(())
        }
        PlistValue::Real(r) => {
            indent(level, out)?;
            // dd-plist renders the specials as plist literals, the rest as Java Double.toString.
            let text = if r.is_nan() {
                "nan".to_owned()
            } else if *r == f64::INFINITY {
                "+infinity".to_owned()
            } else if *r == f64::NEG_INFINITY {
                "-infinity".to_owned()
            } else {
                jcompat::jnum::double_to_string(*r)
            };
            write!(out, "<real>{text}</real>")?;
            Ok(())
        }
        PlistValue::Bool(b) => {
            indent(level, out)?;
            out.write_all(if *b { b"<true/>" } else { b"<false/>" })?;
            Ok(())
        }
        PlistValue::Data(bytes) => {
            indent(level, out)?;
            out.write_all(b"<data>\n")?;
            indent(level + 1, out)?;
            out.write_all(base64(bytes).as_bytes())?;
            out.write_all(b"\n")?;
            indent(level, out)?;
            out.write_all(b"</data>")?;
            Ok(())
        }
        PlistValue::Date(date) => {
            indent(level, out)?;
            write!(out, "<date>{}</date>", whole_seconds(*date).to_xml_format())?;
            Ok(())
        }
        PlistValue::Uid(uid) => {
            // dd-plist writes a UID as a CF$UID dictionary (probe `Uid`).
            let dict = PlistDict::new();
            dict.insert("CF$UID", PlistValue::Integer(i128::from(*uid)));
            write_dict(&dict, level, out)
        }
    }
}

/// dd-plist's `SimpleDateFormat` (`yyyy-MM-dd'T'HH:mm:ss'Z'`) writes whole seconds only,
/// so a sub-second date truncates toward the start of its second (probe
/// `writer-date-truncate`: epoch-ms 1700000000123 writes `2023-11-14T22:13:20Z`).
fn whole_seconds(date: plist::Date) -> plist::Date {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let time: SystemTime = date.into();
    let truncated = match time.duration_since(UNIX_EPOCH) {
        Ok(since) => UNIX_EPOCH + Duration::from_secs(since.as_secs()),
        Err(before) => {
            let before = before.duration();
            if before.subsec_nanos() == 0 {
                UNIX_EPOCH - before
            } else {
                UNIX_EPOCH - Duration::from_secs(before.as_secs() + 1)
            }
        }
    };
    truncated.into()
}

/// Key writing (`NSDictionary.toXML` in dd-plist, probe `writer-key-control`): keys are
/// only CDATA-wrapped when they contain `&`, `<` or `>` — unlike string values, their
/// control characters are NOT filtered, so a key carrying U+0001 or U+FFFE writes those
/// bytes raw (and the result is not well-formed XML; kept as dd-plist writes it).
fn write_key<W: Write>(key: &str, out: &mut W) -> PlistResult<()> {
    if key.contains(['&', '<', '>']) {
        out.write_all(b"<![CDATA[")?;
        out.write_all(key.replace("]]>", "]]]]><![CDATA[>").as_bytes())?;
        out.write_all(b"]]>")?;
    } else {
        out.write_all(key.as_bytes())?;
    }
    Ok(())
}

/// dd-plist escaping (probes `escape-rules`, `control-chars-arrays`): characters XML 1.0
/// cannot carry are dropped, and a value containing `&`, `<` or `>` is CDATA-wrapped,
/// splitting any literal `]]>` across two CDATA sections. Quotes are left alone.
fn write_escaped<W: Write>(s: &str, out: &mut W) -> PlistResult<()> {
    let filtered: String = s
        .chars()
        .filter(|c| matches!(c, '\t' | '\n' | '\r') || (*c >= ' ' && !matches!(c, '\u{fffe}' | '\u{ffff}')))
        .collect();
    if filtered.contains(['&', '<', '>']) {
        out.write_all(b"<![CDATA[")?;
        out.write_all(filtered.replace("]]>", "]]]]><![CDATA[>").as_bytes())?;
        out.write_all(b"]]>")?;
    } else {
        out.write_all(filtered.as_bytes())?;
    }
    Ok(())
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = chunk.len();
        out.push(ALPHABET[(b[0] >> 2) as usize] as char);
        out.push(ALPHABET[(((b[0] & 0x03) << 4) | (b[1] >> 4)) as usize] as char);
        out.push(if n > 1 {
            ALPHABET[(((b[1] & 0x0f) << 2) | (b[2] >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if n > 2 {
            ALPHABET[(b[2] & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}
