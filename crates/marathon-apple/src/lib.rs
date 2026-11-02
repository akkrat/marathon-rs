//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/CommandExecutor.kt

pub mod bin;
pub mod cmd;
pub mod device;
pub mod extensions;
pub mod listener;
pub mod logparser;
pub mod mach_o;
pub mod model;
pub mod plist;
pub mod test;
pub mod xcresult;
pub mod xctestrun;

pub mod apple_application_installer;
pub mod apple_device;
pub mod apple_device_registry;
pub mod apple_log_configurator;
pub mod artifact_processor;
pub mod remote_file_manager;
pub mod worker_commands;

pub mod distributed_derived_data;
pub mod generated_file;

pub mod apple_device_test_runner;
pub mod apple_test_bundle_identifier;
pub mod execute_test_request;
pub mod nm_test_parser;
pub mod xcodebuild_session;
pub mod xctest_parser;

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
pub(crate) mod test_support;
