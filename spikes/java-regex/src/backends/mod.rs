pub mod fancy;
pub mod java_regex_backend;
pub mod pcre2_backend;

use crate::backend::Backend;

pub fn all() -> Vec<&'static dyn Backend> {
    vec![
        &java_regex_backend::JavaRegexBackend,
        &fancy::FancyBackend,
        &pcre2_backend::Pcre2Backend,
    ]
}
