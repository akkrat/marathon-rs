//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/extensions/Duration.kt

pub mod poll_backoff;
pub mod reader;

pub mod string_extensions;
use jcompat::jtime::Duration;
pub use string_extensions::bash_escape;

pub struct Durations;
impl Durations {
    // Seven days, despite the Kotlin name.
    pub const INFINITE: Duration = Duration::of_seconds(7 * 24 * 60 * 60);
}
