//! Backend (a): the `java_regex` crate, a Rust port of OpenJDK's `java.util.regex`.
//!
//! The crate works on `char` indices and has a fixed internal budget (5,000,000 steps and a recursion depth of 500
//! per matcher) whose exhaustion reads as "no match": nothing in its API reports it, so this backend can never
//! return `RunError::Aborted`.

use crate::backend::{Backend, CompileError, Compiled, Found, RunError};

pub struct JavaRegexBackend;

struct JavaRegexCompiled(java_regex::Regex);

impl Backend for JavaRegexBackend {
    fn name(&self) -> &'static str {
        "java_regex"
    }

    fn compile(&self, pattern: &str) -> Result<Box<dyn Compiled>, CompileError> {
        // Bound the candidate's recursive compiler as well as the chosen translator. This guard is a spike
        // resource policy, not evidence that java_regex itself safely handles hostile compilation depth.
        let mut depth = 0usize;
        let mut escaped = false;
        for c in pattern.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' => escaped = true,
                '(' | '[' => {
                    depth += 1;
                    if depth > 64 {
                        return Err(CompileError::Syntax("Stack overflow during pattern compilation".into()));
                    }
                }
                ')' | ']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        java_regex::Regex::new(pattern)
            .map(|re| Box::new(JavaRegexCompiled(re)) as Box<dyn Compiled>)
            .map_err(|e| CompileError::Syntax(e.message))
    }
}

/// Byte offset of every char index, plus the end.
fn char_to_byte(input: &str) -> Vec<usize> {
    input
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(input.len()))
        .collect()
}

impl Compiled for JavaRegexCompiled {
    fn full_match(&self, input: &str) -> Result<bool, RunError> {
        Ok(self.0.matches(input))
    }

    fn find_at(&self, input: &str, from: usize) -> Result<Option<Found>, RunError> {
        let bytes = char_to_byte(input);
        let from_char = bytes.partition_point(|&b| b < from);
        Ok(self.0.find_at(input, from_char).map(|m| {
            let mut groups = vec![Some((bytes[m.start], bytes[m.end]))];
            groups.extend(m.group_positions.iter().map(|g| g.map(|(s, e)| (bytes[s], bytes[e]))));
            Found { groups }
        }))
    }

    fn group_index(&self, _name: &str) -> Option<usize> {
        None
    }
}
