//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt
//! and its commons-text 1.12.0 StringSubstitutor dependency (default settings).

use std::fmt;

/// The unchecked Java cycle exception, kept separate from configuration parse errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstitutionError {
    pub message: String,
}

impl SubstitutionError {
    pub const fn java_class(&self) -> &'static str {
        "java.lang.IllegalStateException"
    }
}

impl fmt::Display for SubstitutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SubstitutionError {}

/// Default `${name}` interpolation with `$` escapes and `:-` defaults.
///
/// The lookup returns `None` for undefined variables and `Some("")` for empty ones.
/// Environment callers can inject a snapshot; profiling callers stringify their map values.
/// Variable names are not interpolated, while replacement values are interpolated recursively.
pub struct StringSubstitutor<F> {
    lookup: F,
}

impl<F: Fn(&str) -> Option<String>> StringSubstitutor<F> {
    pub fn new(lookup: F) -> Self {
        Self { lookup }
    }

    pub fn replace(&self, input: &str) -> Result<String, SubstitutionError> {
        let mut buffer: Vec<char> = input.chars().collect();
        let end = buffer.len();
        self.substitute(&mut buffer, 0, end, &mut None, &mut Vec::new())?;
        Ok(buffer.into_iter().collect())
    }

    fn substitute(
        &self,
        buffer: &mut Vec<char>,
        offset: usize,
        mut end: usize,
        original: &mut Option<String>,
        variables: &mut Vec<String>,
    ) -> Result<usize, SubstitutionError> {
        let mut pos = offset;
        while pos + 1 < end {
            if buffer[pos] != '$' || buffer[pos + 1] != '{' {
                pos += 1;
                continue;
            }
            let start = pos;
            let Some(suffix) = (pos + 2..end).find(|&i| buffer[i] == '}') else {
                break;
            };
            if pos > offset && buffer[pos - 1] == '$' {
                buffer.remove(pos - 1);
                end -= 1;
                // Commons-text resumes inside the escaped prefix, so nested prefixes
                // inside an escaped expression can still be substituted.
                pos = start + 1;
                continue;
            }
            let expression: String = buffer[start + 2..suffix].iter().collect();
            let mut split = None;
            for (i, _) in expression.char_indices() {
                if expression[i..].starts_with("${") {
                    break;
                }
                if expression[i..].starts_with(":-") {
                    split = Some(i);
                    break;
                }
            }
            let (name, default) = match split {
                Some(i) => (&expression[..i], Some(&expression[i + 2..])),
                None => (expression.as_str(), None),
            };
            // The Java implementation captures the buffer lazily, after any escapes.
            let root = original.get_or_insert_with(|| buffer.iter().collect());
            if variables.iter().any(|v| v == name) {
                return Err(SubstitutionError {
                    message: format!(
                        "Infinite loop in property interpolation of {root}: {}",
                        variables.join("->")
                    ),
                });
            }
            variables.push(name.to_owned());
            if let Some(value) = (self.lookup)(name).or_else(|| default.map(str::to_owned)) {
                let replacement: Vec<char> = value.chars().collect();
                let replacement_len = replacement.len();
                let old_len = buffer.len();
                buffer.splice(start..suffix + 1, replacement);
                // Restrict recursion to the inserted value: it must not consume a
                // suffix in the surrounding input or rescan a newly formed prefix.
                pos = self.substitute(buffer, start, start + replacement_len, original, variables)?;
                end = buffer.len() - (old_len - end);
            } else {
                pos = suffix + 1;
            }
            variables.pop();
        }
        Ok(end)
    }
}
