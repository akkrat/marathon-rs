//! Java and Kotlin semantics that marathon's observable behaviour depends on: `JavaPriorityQueue`,
//! `JavaRegex`, Java/Kotlin number and string formatting, and similar helpers.
//!
//! See PLAN.md §4.1 and §4.3.

mod priority_queue;
pub use priority_queue::JavaPriorityQueue;
mod string_substitutor;
pub use string_substitutor::{StringSubstitutor, SubstitutionError};
pub mod file_manager;
pub mod formatting;
pub mod fragment_hash;
pub mod jackson;
pub mod jnum;
pub mod jstring;
pub mod jtime;
pub mod logging;
pub mod writers;

mod regex;
pub use regex::{JavaRegex, RegexCompileError, RegexOperationError};
pub mod regex_patterns;

pub mod report_text;
pub mod throwable;
