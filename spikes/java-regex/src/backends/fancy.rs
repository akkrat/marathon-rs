//! Backend (b): fancy-regex behind the translator (docs/research/regex.md §3).

use crate::backend::{Backend, CompileError, Compiled, Found, RunError};
use crate::translate::{self, Dialect, ParseError};

/// fancy-regex counts backtracking steps; past the limit a match returns `BacktrackLimitExceeded`.
pub const BACKTRACK_LIMIT: usize = 100_000_000;

pub struct FancyBackend;

pub struct JavaRegex {
    pattern: String,
    find: fancy_regex::Regex,
    full: fancy_regex::Regex,
    names: Vec<(String, usize)>,
}

pub fn translate(pattern: &str, d: Dialect) -> Result<(translate::Parsed, String, String), CompileError> {
    let parsed = translate::parse(pattern).map_err(|e| match e {
        ParseError::Syntax(m) => CompileError::Syntax(m),
        ParseError::Rejected { construct, reason } => CompileError::Rejected { construct, reason },
    })?;
    let find = translate::emit(&parsed, d, false);
    let full = translate::emit(&parsed, d, true);
    Ok((parsed, find, full))
}

fn build(p: &str, overrides: bool) -> Result<fancy_regex::Regex, CompileError> {
    fancy_regex::RegexBuilder::new(p)
        .backtrack_limit(BACKTRACK_LIMIT)
        .allow_input_assertion_overrides(overrides)
        .delegate_size_limit(1 << 26)
        .build()
        .map_err(|e| match e {
            fancy_regex::Error::ParseError(_, fancy_regex::ParseError::RecursionExceeded) => {
                CompileError::Syntax("Stack overflow during pattern compilation".into())
            }
            e => CompileError::Backend(e.to_string()),
        })
}

impl Backend for FancyBackend {
    fn name(&self) -> &'static str {
        "fancy-regex"
    }

    fn compile(&self, pattern: &str) -> Result<Box<dyn Compiled>, CompileError> {
        Ok(Box::new(JavaRegex::compile(pattern)?))
    }
}

impl JavaRegex {
    pub fn compile(pattern: &str) -> Result<Self, CompileError> {
        let (parsed, find, full) = translate(pattern, Dialect::Fancy)?;
        Ok(Self {
            pattern: pattern.to_string(),
            find: build(&find, find.contains(r"\G"))?,
            full: build(&full, false)?,
            names: parsed.names,
        })
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }
    pub fn matches(&self, input: &str) -> Result<bool, RunError> {
        self.full_match(input)
    }
    pub fn find(&self, input: &str) -> Result<Option<Found>, RunError> {
        crate::backend::find(self, input)
    }
    pub fn split(&self, input: &str, limit: usize) -> Result<Vec<String>, RunError> {
        crate::backend::split(self, input, limit)
    }
}

impl std::fmt::Display for JavaRegex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.pattern())
    }
}
impl serde::Serialize for JavaRegex {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.pattern())
    }
}

fn run_err(e: fancy_regex::Error) -> RunError {
    match e {
        fancy_regex::Error::RuntimeError(_) => RunError::Aborted,
        e => RunError::Other(e.to_string()),
    }
}

impl Compiled for JavaRegex {
    fn full_match(&self, input: &str) -> Result<bool, RunError> {
        self.full.is_match(input).map_err(run_err)
    }

    fn find_at(&self, input: &str, from: usize) -> Result<Option<Found>, RunError> {
        let caps = self.find.captures_from_pos(input, from).map_err(run_err)?;
        let found = caps.map(|c| Found {
            groups: (0..c.len()).map(|i| c.get(i).map(|m| (m.start(), m.end()))).collect(),
        });
        Ok(found)
    }

    fn find_next(&self, input: &str, from: usize, previous_end: usize) -> Result<Option<Found>, RunError> {
        let input = fancy_regex::RegexInput::new(input)
            .from_pos(from)
            .continue_from_previous_match_end(from == previous_end);
        let caps = self.find.captures_input(input).map_err(run_err)?;
        Ok(caps.map(|c| Found {
            groups: (0..c.len()).map(|i| c.get(i).map(|m| (m.start(), m.end()))).collect(),
        }))
    }

    fn group_index(&self, name: &str) -> Option<usize> {
        self.names.iter().find(|n| n.0 == name).map(|n| n.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_engine_resource_error_is_aborted() {
        for error in [
            fancy_regex::RuntimeError::StackOverflow,
            fancy_regex::RuntimeError::BacktrackLimitExceeded,
        ] {
            assert_eq!(run_err(fancy_regex::Error::RuntimeError(error)), RunError::Aborted);
        }
    }
}
