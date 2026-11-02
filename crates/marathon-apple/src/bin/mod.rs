//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/AppleBinaryEnvironment.kt

pub mod chmod;
pub mod codesign;
pub mod csrutil;
pub mod getconf;
pub mod ioreg;
pub mod kt;
pub mod lipo;
pub mod nm;
pub mod plistbuddy;
pub mod sqlite3;
pub mod swvers;
pub mod systemprofiler;
pub mod xcodeselect;
pub mod xcrun;

use crate::{
    cmd::{CommandRequest, CommandResult, failure, remote::ssh::sshj::config::SshLogging},
    device::connection_factory::Executor,
};
use jcompat::{
    jtime::Duration,
    logging::{Level, Logger},
};
use marathon_config::{Configuration, vendor::AppleTimeoutConfiguration};
use marathon_core::{
    error::{Failure, VendorError, VendorResult},
    runtime::VendorRuntime,
    vendor::VendorContext,
};
use std::{collections::BTreeMap, sync::Arc};

pub const DEVICE_SETUP_EXCEPTION: &str = "com.malinskiy.marathon.exceptions.DeviceSetupException";

pub(crate) fn device_setup(message: impl AsRef<str>) -> VendorError {
    VendorError::Other(failure(DEVICE_SETUP_EXCEPTION, message))
}

/// What Kotlin hands every wrapper constructor (`commandExecutor`, `timeoutConfiguration`), plus the
/// runtime for off-core decoding and the per-run logging registry. Cloning shares all four.
#[derive(Clone)]
pub struct Tools {
    pub executor: Executor,
    pub timeouts: Arc<AppleTimeoutConfiguration>,
    pub runtime: Arc<dyn VendorRuntime>,
    pub logging: SshLogging,
}
impl Tools {
    /// `MarathonLogging.logger {}` inside a class: named after it, looked up at construction.
    pub(crate) fn logger(&self, name: &str) -> Logger {
        self.logging.logger(name)
    }
    pub(crate) fn log(&self, logger: &Logger, level: Level, message: impl FnOnce() -> String, cause: Option<Failure>) {
        if self.logging.enabled(logger, level) {
            (self.logging.sink)(logger, level, message(), cause);
        }
    }
    /// The vararg overloads: 7-day idle timeout, no env, no workdir, the host's charset.
    pub(crate) fn request(&self, command: Vec<String>, timeout: Duration) -> CommandRequest {
        let mut request = CommandRequest::new(command, timeout);
        request.charset = self.executor.host().charset.clone();
        request
    }
    pub(crate) fn request_with(
        &self,
        command: Vec<String>,
        timeout: Duration,
        idle_timeout: Duration,
        env: BTreeMap<String, String>,
        workdir: Option<String>,
    ) -> CommandRequest {
        let mut request = self.request(command, timeout);
        request.idle_timeout = idle_timeout;
        request.env = env;
        request.workdir = workdir;
        request
    }
    pub(crate) async fn critical(
        &self,
        command: Vec<String>,
        timeout: Duration,
        context: &VendorContext,
    ) -> VendorResult<CommandResult> {
        self.executor
            .critical_execute(self.request(command, timeout), context.clone())
            .await
    }
    pub(crate) async fn safe(
        &self,
        command: Vec<String>,
        timeout: Duration,
        context: &VendorContext,
    ) -> VendorResult<Option<CommandResult>> {
        self.executor
            .safe_execute(self.request(command, timeout), context.clone())
            .await
    }
}

pub(crate) fn argv<const N: usize>(fixed: [&str; N]) -> Vec<String> {
    fixed.into_iter().map(str::to_owned).collect()
}

/// Port of `AppleBinaryEnvironment`: constructing it runs nothing and neither acquires nor
/// disposes of the executor's connection.
pub struct AppleBinaryEnvironment {
    pub command_executor: Executor,
    pub codesign: codesign::Codesign,
    pub csrutil: csrutil::Csrutil,
    pub sqlite3: sqlite3::Sqlite3,
    pub getconf: getconf::Getconf,
    pub lipo: lipo::Lipo,
    pub nm: nm::Nm,
    pub plist_buddy: plistbuddy::PlistBuddy,
    pub xcodeselect: xcodeselect::Xcodeselect,
    pub ioreg: ioreg::Ioreg,
    pub system_profiler: systemprofiler::SystemProfiler,
    pub swvers: swvers::SwVers,
    pub chmod: chmod::Chmod,
    pub xcrun: xcrun::Xcrun,
}
impl AppleBinaryEnvironment {
    pub fn new(
        command_executor: Executor,
        configuration: &Configuration,
        timeout_configuration: AppleTimeoutConfiguration,
        runtime: Arc<dyn VendorRuntime>,
        logging: SshLogging,
    ) -> Self {
        let tools = Tools {
            executor: command_executor,
            timeouts: Arc::new(timeout_configuration),
            runtime,
            logging,
        };
        Self::with_tools(
            tools,
            configuration.test_batch_timeout_millis,
            configuration.test_output_timeout_millis,
        )
    }
    /// [`Self::new`] with the two `Configuration` values Xcodebuild reads.
    pub fn with_tools(tools: Tools, test_batch_timeout_millis: i64, test_output_timeout_millis: i64) -> Self {
        // Kotlin's property order, which is also the order of the logger lookups.
        Self {
            command_executor: tools.executor.clone(),
            codesign: codesign::Codesign::new(tools.clone()),
            csrutil: csrutil::Csrutil::new(tools.clone()),
            sqlite3: sqlite3::Sqlite3::new(tools.clone()),
            getconf: getconf::Getconf::new(tools.clone()),
            lipo: lipo::Lipo::new(tools.clone()),
            nm: nm::Nm::new(tools.clone()),
            plist_buddy: plistbuddy::PlistBuddy::new(tools.clone()),
            xcodeselect: xcodeselect::Xcodeselect::new(tools.clone()),
            ioreg: ioreg::Ioreg::new(tools.clone()),
            system_profiler: systemprofiler::SystemProfiler::new(tools.clone()),
            swvers: swvers::SwVers::new(tools.clone()),
            chmod: chmod::Chmod::new(tools.clone()),
            xcrun: xcrun::Xcrun::new(tools, test_batch_timeout_millis, test_output_timeout_millis),
        }
    }
}

/// `Duration.ofMillis`, total for every `long`.
pub fn java_duration_of_millis(millis: i64) -> Duration {
    jcompat::jtime::duration_of(millis.div_euclid(1000), millis.rem_euclid(1000) * 1_000_000).unwrap_or(Duration::ZERO)
}
