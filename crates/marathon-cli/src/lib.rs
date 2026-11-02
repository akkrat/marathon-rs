//! Port of core/src/main/kotlin/com/malinskiy/marathon/di/Modules.kt
//! and cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt.
//!
//! Explicit vendor dispatch; concrete factories arrive in vendor WPs. No
//! provider initialization, logging configuration or analytics query occurs here.
//! The command line (WP 2.5) is in `args`, `application` and `config_dump`.

pub mod application;
pub mod args;
pub mod config_dump;
pub mod core_output;
pub mod runner;

use application::{CliOutput, CliRunner, Process};
use args::Invocation;
use marathon_config::VendorConfiguration;
use marathon_core::{runtime::VendorFuture, vendor::VendorServices, wiring::VendorDependencies};

pub type VendorFactory = dyn Fn(VendorDependencies) -> VendorFuture<'static, VendorServices> + Send + Sync;
/// Factory slots avoid config -> core -> vendor dependency cycles. Empty remains
/// explicit so WP 2.5 can produce the source's configuration error, not a stub.
pub struct VendorFactories {
    pub android: Box<VendorFactory>,
    pub ios: Box<VendorFactory>,
    pub macos: Box<VendorFactory>,
    pub stub: Box<VendorFactory>,
    pub empty: Box<VendorFactory>,
}
impl VendorFactories {
    /// WP 2.5's CLI caller accepts only Android/iOS/macOS before dispatch.
    /// Stub/Empty fail there with "No vendor config present in <marathonfile path>";
    /// Empty normally already fails parse with "No vendor configuration specified".
    /// Stub/Empty factory slots remain available for non-CLI/test construction.
    pub fn construct(&self, dependencies: VendorDependencies) -> VendorFuture<'static, VendorServices> {
        let factory = match &dependencies.configuration.vendor_configuration {
            VendorConfiguration::Android(_) => &self.android,
            VendorConfiguration::Ios(_) => &self.ios,
            VendorConfiguration::Macos(_) => &self.macos,
            VendorConfiguration::Stub => &self.stub,
            VendorConfiguration::Empty => &self.empty,
        };
        factory(dependencies)
    }
}

/// `main(args)`: parses the command line and runs it; returns the exit status.
pub fn run_cli(args: &[String], process: &Process, runner: &dyn CliRunner, output: &CliOutput) -> i32 {
    match args::parse(args) {
        Invocation::Help(text) => {
            output.out(&text);
            0
        }
        Invocation::UsageError(text) => {
            output.err(&text);
            1
        }
        Invocation::ConfigDump(args) => config_dump::config_dump(&args, process, output),
        Invocation::Execute(configuration) => application::execute(configuration, process, runner, output),
    }
}
