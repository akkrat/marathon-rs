//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/AndroidVendor.kt
//!
//! See PLAN.md §4.1.

pub mod dex_test_parser;
pub mod logcat;
pub mod logcat_listener;
pub mod test_annotation_parser;

pub mod device;
pub mod installer;
pub mod models;
pub mod provider;
pub mod remote_file_manager;
pub mod requests;
pub mod services;
mod support;
pub mod tracker;
pub mod version;

mod transfers;

pub use support::{adb_error, execution_error};

pub mod arguments;
pub mod listener_factory;
pub mod listeners;
pub mod remote_parser;
pub mod runner;

pub mod artifact_listeners;
