//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/config/PerformanceDefaultConfigTest.kt
//! sshj logs the command line of every exec at DEBUG through the logger of its channel. Only `ssh.debug` enables
//! that logger, which is what lets marathon put file contents into commands without them reaching the log.
mod support;
use jcompat::logging::Level;
use marathon_apple::cmd::remote::ssh::sshj::config::PerformanceDefaultConfig;
use support::ssh;

#[test]
fn exec_channels_log_commands_only_with_ssh_debug() {
    let (logging, _) = ssh::logging(false);
    let verbose = PerformanceDefaultConfig::new(true, logging.clone()).logger_for_class(Some("PatchedExecChannel"));
    assert!(logging.enabled(&verbose, Level::Debug));
    let quiet = PerformanceDefaultConfig::new(false, logging.clone()).logger_for_class(Some("PatchedExecChannel"));
    assert!(!logging.enabled(&quiet, Level::Debug));
    let quiet = PerformanceDefaultConfig::new(false, logging.clone()).logger_for_class(Some("PatchedExecChannel"));
    assert!(!logging.enabled(&quiet, Level::Info));
}

/// Ours: `clazz?.simpleName ?: "SshjCommandExecutor"`, and a null name is the empty logger name.
#[test]
fn logger_names_follow_the_sshj_logger_factory() {
    let (logging, _) = ssh::logging(false);
    let config = PerformanceDefaultConfig::new(false, logging);
    assert_eq!(config.logger_for_class(None).name(), "SshjCommandExecutor");
    assert_eq!(config.logger_for_name(None).name(), "");
    assert_eq!(config.logger_for_class(Some("TransportImpl")).name(), "TransportImpl");
}
