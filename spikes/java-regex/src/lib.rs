//! WP 0.3c regex spike: three candidate backends for `JavaRegex` behind one trait, and the translator that the
//! fancy-regex and PCRE2 backends share. See docs/research/regex-spike-results.md.

pub mod backend;
pub mod backends;
pub mod translate;
