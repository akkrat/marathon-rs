//! Port of core/src/main/kotlin/com/malinskiy/marathon/test/Test.kt (Kotlin string operations).

/// Kotlin `Char.isWhitespace()`: `Character.isWhitespace` plus every Unicode
/// space character (`isSpaceChar`), i.e. the Zs/Zl/Zp categories, the ASCII
/// controls `\t`..`\r`, and the file/group/record/unit separators. Every such
/// character is in the BMP, so a per-unit test is exact.
pub fn is_whitespace(c: char) -> bool {
    matches!(c as u32,
        0x0009..=0x000D | 0x001C..=0x001F | 0x0020 | 0x00A0 | 0x1680 |
        0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000)
}

pub fn trim(text: &str) -> &str {
    text.trim_matches(is_whitespace)
}

/// Output names use Kotlin's unit limit. A cut through a pair used to reach
/// the JVM UTF-8 encoder as a lone high surrogate, encoded as `?`. Preserve
/// that filename byte; D55 replaces malformed *input* with U+FFFD instead.
pub fn take_utf16(text: &str, count: usize) -> String {
    char::decode_utf16(text.encode_utf16().take(count))
        .map(|c| c.unwrap_or('?'))
        .collect()
}

/// Kotlin `String.lines()`: splits around `\r\n`, `\n` and `\r`. The segment
/// after the last separator is always present, so an empty string is one
/// empty line and a trailing separator yields a trailing empty line.
pub fn lines(text: &str) -> Vec<String> {
    let units = text.as_bytes();
    let mut result = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < units.len() {
        match units[i] {
            0x0D => {
                result.push(text[start..i].to_owned());
                i += if units.get(i + 1) == Some(&0x0A) { 2 } else { 1 };
                start = i;
            }
            0x0A => {
                result.push(text[start..i].to_owned());
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    result.push(text[start..].to_owned());
    result
}

/// Kotlin Char.isLetter uses Unicode letter categories on one UTF-16 unit.
pub fn is_letter(c: char) -> bool {
    static LETTER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    if c.len_utf16() != 1 {
        return false;
    }
    let mut buffer = [0; 4];
    LETTER
        .get_or_init(|| regex::Regex::new(r"\A\p{L}\z").unwrap())
        .is_match(c.encode_utf8(&mut buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn js(s: &str) -> String {
        s.into()
    }

    #[test]
    fn take_counts_utf16_and_preserves_split_pair_filename_bytes() {
        assert_eq!(take_utf16("a😀z", 0), "");
        assert_eq!(take_utf16("a😀z", 2), "a?");
        assert_eq!(take_utf16("a😀z", 3), "a😀");
        assert_eq!(take_utf16("a😀z", 8), "a😀z");
        assert_eq!(take_utf16("a�z", 2), "a�");
        assert_eq!(crate::file_manager::path_diagnostic(&[97, 0xd800]), "a?");
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
        assert_eq!(trim("\t\u{a0}x\u{3000}\u{1c}"), "x");
        assert!(!is_whitespace('\u{200b}')); // zero-width space is not Kotlin whitespace
        assert!(is_whitespace('\u{a0}'));
        assert!(!is_whitespace('\u{200b}'));
        assert!(!is_whitespace('😀'));
    }
}
