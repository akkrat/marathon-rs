//! Kotlin string operations on UTF-16 unit slices, for ports whose inputs are
//! `JavaString`s that may carry supplementary characters or lone surrogates.

use crate::writers::JavaString;

/// Kotlin `Char.isWhitespace()`: `Character.isWhitespace` plus every Unicode
/// space character (`isSpaceChar`), i.e. the Zs/Zl/Zp categories, the ASCII
/// controls `\t`..`\r`, and the file/group/record/unit separators. Every such
/// character is in the BMP, so a per-unit test is exact.
pub fn is_whitespace(c: u16) -> bool {
    matches!(
        c,
        0x0009..=0x000D
            | 0x001C..=0x001F
            | 0x0020
            | 0x00A0
            | 0x1680
            | 0x2000..=0x200A
            | 0x2028
            | 0x2029
            | 0x202F
            | 0x205F
            | 0x3000
    )
}

/// Kotlin whitespace for native Rust strings.
pub fn is_whitespace_char(c: char) -> bool {
    u16::try_from(u32::from(c)).is_ok_and(is_whitespace)
}

/// Kotlin `String.trim()` over UTF-16 units.
pub fn trim(units: &[u16]) -> &[u16] {
    trim_end(trim_start(units))
}

/// Kotlin `String.trimStart()` over UTF-16 units.
pub fn trim_start(units: &[u16]) -> &[u16] {
    let start = units.iter().position(|&c| !is_whitespace(c)).unwrap_or(units.len());
    &units[start..]
}

/// Kotlin `String.trimEnd()` over UTF-16 units.
pub fn trim_end(units: &[u16]) -> &[u16] {
    let end = units.iter().rposition(|&c| !is_whitespace(c)).map_or(0, |i| i + 1);
    &units[..end]
}

/// Kotlin `String.lines()`: splits around `\r\n`, `\n` and `\r`. The segment
/// after the last separator is always present, so an empty string is one
/// empty line and a trailing separator yields a trailing empty line.
pub fn lines(text: &JavaString) -> Vec<JavaString> {
    let units = &text.0;
    let mut result = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < units.len() {
        match units[i] {
            0x0D => {
                result.push(JavaString(units[start..i].to_vec()));
                i += if units.get(i + 1) == Some(&0x0A) { 2 } else { 1 };
                start = i;
            }
            0x0A => {
                result.push(JavaString(units[start..i].to_vec()));
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    result.push(JavaString(units[start..].to_vec()));
    result
}

/// Kotlin Char.isLetter uses Unicode letter categories on one UTF-16 unit.
pub fn is_letter(unit: u16) -> bool {
    static LETTER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let Some(c) = char::from_u32(u32::from(unit)) else {
        return false;
    };
    let mut buffer = [0; 4];
    LETTER
        .get_or_init(|| regex::Regex::new(r"\A\p{L}\z").unwrap())
        .is_match(c.encode_utf8(&mut buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn js(s: &str) -> JavaString {
        s.into()
    }

    #[test]
    fn lines_split_on_all_separators_and_keep_final_segment() {
        assert_eq!(lines(&js("")), vec![js("")]);
        assert_eq!(lines(&js("a")), vec![js("a")]);
        assert_eq!(lines(&js("a\n")), vec![js("a"), js("")]);
        assert_eq!(lines(&js("a\r\nb\rc\nd")), vec![js("a"), js("b"), js("c"), js("d")]);
        assert_eq!(lines(&js("\n\n")), vec![js(""), js(""), js("")]);
    }

    #[test]
    fn trim_uses_kotlin_whitespace() {
        let units: Vec<u16> = "\t\u{a0}x\u{3000}\u{1c}".encode_utf16().collect();
        assert_eq!(trim(&units), "x".encode_utf16().collect::<Vec<_>>().as_slice());
        assert!(!is_whitespace(0x200B)); // zero-width space is not Kotlin whitespace
        assert!(is_whitespace_char('\u{a0}'));
        assert!(!is_whitespace_char('\u{200b}'));
        assert!(!is_whitespace_char('😀'));
    }
}
