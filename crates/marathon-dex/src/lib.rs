//! DEX reader, JUnit3/4 test discovery, binary AndroidManifest (AXML)
//! reading and marathon's ApkParser.
//!
//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/ApkParser.kt
//! and vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/InstrumentationInfo.kt,
//! over ports of the two replaced libraries (PLAN §2): dex-test-parser
//! 2.3.4 (`spec` + `parser`, BSD-2-Clause, Copyright (c) LinkedIn
//! Corporation) and the ApkParser-driven subset of axmlparser 1.0
//! (`axml`). DexTestParser's conversion to core types lives in
//! marathon-android (WP 6.4 decision 1: this crate stays a
//! vendor-independent leaf).
//!
//! Discovery is synchronous CPU+file work; callers run it off the core
//! executor (docs/design/lifecycle.md).

pub mod apk_parser;
pub mod axml;
pub mod error;
pub mod parser;
pub mod spec;

pub use apk_parser::{InstrumentationInfo, parse_app_package_name, parse_instrumentation_info};
pub use error::{DexError, DexResult};
pub use parser::{DecodedValue, TestAnnotation, TestMethod, find_test_methods};
