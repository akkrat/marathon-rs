//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/simctl/Simctl.kt
//! and simctl/SimctlService.kt.

pub mod model;
pub mod service;

use super::super::Tools;
use crate::cmd::CommandResult;
use jcompat::jtime::Duration;
use marathon_config::vendor::AppleTimeoutConfiguration;
use marathon_core::{error::VendorResult, vendor::VendorContext};
use std::collections::BTreeMap;

pub struct Simctl {
    pub device: service::DeviceService,
    pub simulator: service::SimulatorService,
    pub io: service::IoService,
    pub privacy: service::PrivacyService,
    pub media_service: service::MediaService,
    pub application: service::ApplicationService,
    pub spawn: service::SpawnService,
}
impl Simctl {
    pub fn new(tools: Tools) -> Self {
        Self {
            device: service::DeviceService::new(SimctlService::new(tools.clone())),
            simulator: service::SimulatorService::new(SimctlService::new(tools.clone())),
            io: service::IoService::new(SimctlService::new(tools.clone())),
            privacy: service::PrivacyService::new(SimctlService::new(tools.clone())),
            media_service: service::MediaService::new(SimctlService::new(tools.clone())),
            application: service::ApplicationService::new(SimctlService::new(tools.clone())),
            spawn: service::SpawnService::new(SimctlService::new(tools)),
        }
    }
}

/// The common `xcrun simctl` prefix of every service command.
pub struct SimctlService {
    pub(crate) tools: Tools,
}
impl SimctlService {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
    pub(crate) fn timeouts(&self) -> &AppleTimeoutConfiguration {
        &self.tools.timeouts
    }
    fn command(args: Vec<String>) -> Vec<String> {
        let mut command = vec!["xcrun".to_owned(), "simctl".to_owned()];
        command.extend(args);
        command
    }
    /// nohup requires all streams redirected, otherwise the SSH transport hangs.
    pub(crate) async fn safe_execute_nohup(
        &self,
        pidfile: &str,
        timeout: Duration,
        args: Vec<String>,
        context: &VendorContext,
    ) -> VendorResult<Option<CommandResult>> {
        let request = self.tools.request(Self::command(args), timeout);
        self.tools
            .executor
            .safe_execute_nohup(pidfile, request, context.clone())
            .await
    }
    pub(crate) async fn critical_exec(
        &self,
        timeout: Duration,
        args: Vec<String>,
        context: &VendorContext,
    ) -> VendorResult<CommandResult> {
        self.tools.critical(Self::command(args), timeout, context).await
    }
    pub(crate) async fn critical_exec_env(
        &self,
        timeout: Duration,
        env: BTreeMap<String, String>,
        args: Vec<String>,
        context: &VendorContext,
    ) -> VendorResult<CommandResult> {
        let request = self.tools.request_with(
            Self::command(args),
            timeout,
            AppleTimeoutConfiguration::infinite(),
            env,
            None,
        );
        self.tools.executor.critical_execute(request, context.clone()).await
    }
    pub(crate) async fn safe_execute(
        &self,
        timeout: Duration,
        args: Vec<String>,
        context: &VendorContext,
    ) -> VendorResult<Option<CommandResult>> {
        self.tools.safe(Self::command(args), timeout, context).await
    }
}
