//! Port of the Kotlin stdlib text functions (`trim`, `isBlank`, `lines`, `trimIndent`) as the
//! bin/ and model/ sources call them on command output. Kotlin's whitespace set is all BMP.

pub use jcompat::jstring::{is_whitespace, trim};

pub fn is_blank(text: &str) -> bool {
    text.chars().all(is_whitespace)
}

pub fn lines(text: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let bytes = text.as_bytes();
    let (mut start, mut i) = (0, 0);
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                result.push(&text[start..i]);
                i += if bytes.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                start = i;
            }
            b'\n' => {
                result.push(&text[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    result.push(&text[start..]);
    result
}

/// `String.trimIndent()`. Kotlin applies it after interpolation, so multi-line values take part
/// in the common-indent computation.
pub fn trim_indent(text: &str) -> String {
    let lines = lines(text);
    let indent = |line: &str| {
        line.chars()
            .position(|c| !is_whitespace(c))
            .unwrap_or(line.chars().count())
    };
    let common = lines
        .iter()
        .filter(|line| !is_blank(line))
        .map(|line| indent(line))
        .min()
        .unwrap_or(0);
    let last = lines.len() - 1;
    lines
        .iter()
        .enumerate()
        .filter(|(index, line)| !((*index == 0 || *index == last) && is_blank(line)))
        .map(|(_, line)| line.chars().skip(common).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Kotlin `substringBefore(delimiter)`: the whole string when the delimiter is absent.
pub fn substring_before<'a>(text: &'a str, delimiter: &str) -> &'a str {
    text.find(delimiter).map_or(text, |at| &text[..at])
}

/// `List<String>.toString()`.
pub fn list_to_string(items: &[String]) -> String {
    format!("[{}]", items.join(", "))
}
