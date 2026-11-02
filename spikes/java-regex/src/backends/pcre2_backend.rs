//! Backend (c): PCRE2 (the `pcre2` crate, bundled C library) behind the translator in its PCRE2 dialect.
//!
//! The crate exposes no match context, so the limits are set with the pattern-start items `(*LIMIT_MATCH=…)` and
//! `(*LIMIT_DEPTH=…)`, which may only lower PCRE2's compiled-in defaults. JIT stays off: it counts differently.

use crate::backend::{Backend, CompileError, Compiled, Found, RunError};
use crate::backends::fancy::translate;
use crate::translate::Dialect;

pub const MATCH_LIMIT: u32 = 1_000_000;
pub const DEPTH_LIMIT: u32 = 100_000;

pub struct Pcre2Backend;

struct Pcre2Compiled {
    find: pcre2::bytes::Regex,
    full: pcre2::bytes::Regex,
    names: Vec<(String, usize)>,
}

fn build(p: &str) -> Result<pcre2::bytes::Regex, CompileError> {
    let limited = format!("(*LIMIT_MATCH={MATCH_LIMIT})(*LIMIT_DEPTH={DEPTH_LIMIT}){p}");
    pcre2::bytes::RegexBuilder::new()
        .utf(true)
        .build(&limited)
        .map_err(|e| CompileError::Backend(e.to_string()))
}

impl Backend for Pcre2Backend {
    fn name(&self) -> &'static str {
        "pcre2"
    }

    fn compile(&self, pattern: &str) -> Result<Box<dyn Compiled>, CompileError> {
        let (parsed, find, full) = translate(pattern, Dialect::Pcre2)?;
        Ok(Box::new(Pcre2Compiled {
            find: build(&find)?,
            full: build(&full)?,
            names: parsed.names,
        }))
    }
}

fn run_err(e: pcre2::Error) -> RunError {
    // PCRE2_ERROR_MATCHLIMIT (-47), PCRE2_ERROR_DEPTHLIMIT (-53), PCRE2_ERROR_HEAPLIMIT (-63).
    if matches!(e.code(), -47 | -53 | -63) {
        RunError::Aborted
    } else {
        RunError::Other(e.to_string())
    }
}

impl Compiled for Pcre2Compiled {
    fn full_match(&self, input: &str) -> Result<bool, RunError> {
        self.full.is_match(input.as_bytes()).map_err(run_err)
    }

    fn find_at(&self, input: &str, from: usize) -> Result<Option<Found>, RunError> {
        let mut locs = self.find.capture_locations();
        let m = self
            .find
            .captures_read_at(&mut locs, input.as_bytes(), from)
            .map_err(run_err)?;
        Ok(m.map(|_| Found {
            groups: (0..locs.len()).map(|i| locs.get(i)).collect(),
        }))
    }

    fn group_index(&self, name: &str) -> Option<usize> {
        self.names.iter().find(|n| n.0 == name).map(|n| n.1)
    }
}
