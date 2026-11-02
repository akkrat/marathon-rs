//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/RegexSerializer.kt
//! and Kotlin Regex operations used by marathon. Pattern syntax follows D6 (Rust `regex`).

use std::{
    error::Error,
    fmt,
    hash::{Hash, Hasher},
};

/// A source-preserving regex with Kotlin operations and Rust pattern syntax.
/// Equality and hashing are by pattern only (F7, WP 2.2 S1): Kotlin compares
/// serial-regex patterns in `equals` but hashes `Regex` identity, so the Rust
/// replacement is one consistent pattern-based rule for both.
#[derive(Clone, Debug)]
pub struct JavaRegex {
    pattern: String,
    search: regex::Regex,
    full: regex::Regex,
}

impl PartialEq for JavaRegex {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern
    }
}
impl Eq for JavaRegex {}
impl Hash for JavaRegex {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.pattern.hash(state);
    }
}

#[derive(Debug)]
pub struct RegexCompileError {
    pub field: String,
    pub pattern: String,
    pub source: regex::Error,
}

impl fmt::Display for RegexCompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid regex in {}: pattern {:?}: {}",
            self.field, self.pattern, self.source
        )
    }
}

impl Error for RegexCompileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexOperationError(pub String);
impl fmt::Display for RegexOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl Error for RegexOperationError {}

impl fmt::Display for JavaRegex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.pattern)
    }
}

impl JavaRegex {
    pub fn compile(pattern: &str, field: &str) -> Result<Self, RegexCompileError> {
        let error = |source| RegexCompileError {
            field: field.into(),
            pattern: pattern.into(),
            source,
        };
        let search = regex::Regex::new(pattern).map_err(error)?;
        // Checking a search span alone fails on alternatives such as a|ab on "ab".
        // Absolute anchors also remain absolute when the user enables multiline mode.
        let full = regex::Regex::new(&format!(r"\A(?:{pattern})\z"))
            // A final verbose-mode comment can consume our closing group. The newline
            // is ignored in verbose mode and terminates that comment.
            .or_else(|_| regex::Regex::new(&format!("\\A(?:{pattern}\n)\\z")))
            .map_err(error)?;
        Ok(Self {
            pattern: pattern.into(),
            search,
            full,
        })
    }

    /// The original pattern, suitable for RegexSerializer's string value.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
    pub fn matches(&self, input: &str) -> bool {
        self.full.is_match(input)
    }
    pub fn contains(&self, input: &str) -> bool {
        self.search.is_match(input)
    }
    /// Captures use UTF-8 byte offsets; unmatched groups are None.
    pub fn find<'a>(&self, input: &'a str) -> Option<regex::Captures<'a>> {
        self.search.captures(input)
    }

    /// Kotlin permits an empty match immediately after a nonempty match. The
    /// crate's captures_iter suppresses that match, so advance explicitly instead.
    /// Empty matches advance one Unicode scalar (D6's UTF-8 string domain).
    pub fn find_all<'r, 'a>(&'r self, input: &'a str) -> impl Iterator<Item = regex::Captures<'a>> + 'r
    where
        'a: 'r,
    {
        let mut position = Some(0);
        std::iter::from_fn(move || {
            let captures = self.search.captures_at(input, position?)?;
            let m = captures.get(0)?;
            position = if m.is_empty() {
                input[m.end()..].chars().next().map(|c| m.end() + c.len_utf8())
            } else {
                Some(m.end())
            };
            Some(captures)
        })
    }

    /// Zero means unlimited, retaining trailing empties. Negative limits fail.
    pub fn split<'a>(&self, input: &'a str, limit: i32) -> Result<Vec<&'a str>, RegexOperationError> {
        if limit < 0 {
            return Err(RegexOperationError("Limit must be non-negative".into()));
        }
        if limit == 1 {
            return Ok(vec![input]);
        }
        let mut parts = Vec::new();
        let mut end = 0;
        for captures in self.find_all(input) {
            let m = captures.get(0).expect("a match always has group zero");
            parts.push(&input[end..m.start()]);
            end = m.end();
            if limit > 0 && parts.len() == limit as usize - 1 {
                break;
            }
        }
        parts.push(&input[end..]);
        Ok(parts)
    }

    /// Kotlin replacement strings use $0/$1/${name} and backslash escapes,
    /// unlike the crate's $name syntax. Invalid expansions fail only on a match.
    pub fn replace(&self, input: &str, replacement: &str) -> Result<String, RegexOperationError> {
        let mut output = String::new();
        let mut end = 0;
        for captures in self.find_all(input) {
            let m = captures.get(0).expect("a match always has group zero");
            output.push_str(&input[end..m.start()]);
            self.expand(&captures, replacement, &mut output)?;
            end = m.end();
        }
        output.push_str(&input[end..]);
        Ok(output)
    }

    fn expand(
        &self,
        captures: &regex::Captures<'_>,
        replacement: &str,
        output: &mut String,
    ) -> Result<(), RegexOperationError> {
        let bad = || RegexOperationError(format!("invalid replacement: {replacement:?}"));
        let mut chars = replacement.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' => output.push(chars.next().ok_or_else(bad)?),
                '$' => {
                    let first = chars.next().ok_or_else(bad)?;
                    let group = if first == '{' {
                        let mut name = String::new();
                        loop {
                            match chars.next() {
                                Some('}') => break,
                                Some(c) => name.push(c),
                                None => return Err(bad()),
                            }
                        }
                        if !self.search.capture_names().flatten().any(|n| n == name) {
                            return Err(bad());
                        }
                        captures.name(&name)
                    } else if first.is_ascii_digit() {
                        let mut index = first as usize - '0' as usize;
                        if index >= captures.len() {
                            return Err(bad());
                        }
                        while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
                            let Some(next) = index.checked_mul(10).and_then(|n| n.checked_add(digit as usize)) else {
                                break;
                            };
                            if next >= captures.len() {
                                break;
                            }
                            chars.next();
                            index = next;
                        }
                        captures.get(index)
                    } else {
                        return Err(bad());
                    };
                    if let Some(group) = group {
                        output.push_str(group.as_str());
                    }
                }
                c => output.push(c),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
