//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/simctl/service/SimulatorService.kt

use super::SimctlService;
use crate::bin::{argv, kt};
use jcompat::{logging::Logger, regex_patterns::UDID};
use marathon_core::{error::VendorResult, vendor::VendorContext};
use std::collections::BTreeMap;

pub struct SimulatorService {
    service: SimctlService,
    _logger: Logger,
}
impl SimulatorService {
    pub fn new(service: SimctlService) -> Self {
        // Kotlin's only use is in an unreachable `when` branch.
        let _logger = service
            .tools
            .logger("com.malinskiy.marathon.apple.bin.xcrun.simctl.service.SimulatorService");
        Self { service, _logger }
    }
    /// The last stdout line whose trimmed text is a UDID, untrimmed; an empty line also matches (A4).
    pub async fn create(
        &self,
        name: &str,
        device_type_id: &str,
        runtime_id: Option<&str>,
        context: &VendorContext,
    ) -> VendorResult<Option<String>> {
        let mut args = argv(["create", name, device_type_id]);
        args.extend(runtime_id.map(str::to_owned));
        let timeout = self.service.timeouts().create;
        let result = self.service.critical_exec(timeout, args, context).await?;
        Ok(result
            .stdout
            .into_iter()
            .rev()
            .find(|line| UDID.matches(kt::trim(line))))
    }
    /// `env` is the Kotlin map in iteration order. `mapKeys { "SIMCTL_CHILD_$it" }` interpolates the whole
    /// entry, so the key is `SIMCTL_CHILD_<key>=<value>` (A10).
    pub async fn boot(&self, udid: &str, env: &[(String, String)], context: &VendorContext) -> VendorResult<bool> {
        let env: BTreeMap<String, String> = env
            .iter()
            .map(|(key, value)| (format!("SIMCTL_CHILD_{key}={value}"), value.clone()))
            .collect();
        let timeout = self.service.timeouts().boot;
        Ok(self
            .service
            .critical_exec_env(timeout, env, argv(["boot", udid]), context)
            .await?
            .successful())
    }
    pub async fn getenv(&self, udid: &str, key: &str, context: &VendorContext) -> VendorResult<Option<String>> {
        let timeout = self.service.timeouts().shell;
        let result = self
            .service
            .safe_execute(timeout, argv(["getenv", udid, key]), context)
            .await?;
        Ok(result
            .and_then(|result| result.successful_or_null())
            .map(|result| kt::trim(&result.combined_stdout()).to_owned()))
    }
    pub async fn monitor_status(&self, udid: &str, context: &VendorContext) -> VendorResult<bool> {
        self.mutate(self.service.timeouts().boot, argv(["bootstatus", udid]), context)
            .await
    }
    pub async fn shutdown(&self, udid: &str, context: &VendorContext) -> VendorResult<bool> {
        self.mutate(self.service.timeouts().shutdown, argv(["shutdown", udid]), context)
            .await
    }
    /// The simulator should be shut down.
    pub async fn delete(&self, udid: &str, context: &VendorContext) -> VendorResult<bool> {
        self.mutate(self.service.timeouts().delete, argv(["delete", udid]), context)
            .await
    }
    pub async fn delete_unavailable(&self, context: &VendorContext) -> VendorResult<bool> {
        self.mutate(self.service.timeouts().delete, argv(["delete", "unavailable"]), context)
            .await
    }
    pub async fn erase_all(&self, context: &VendorContext) -> VendorResult<bool> {
        self.mutate(self.service.timeouts().erase, argv(["erase", "all"]), context)
            .await
    }
    pub async fn erase_udid(&self, udid: &str, context: &VendorContext) -> VendorResult<bool> {
        self.erase(&[udid.to_owned()], context).await
    }
    /// An empty list still runs `xcrun simctl erase`.
    pub async fn erase(&self, udids: &[String], context: &VendorContext) -> VendorResult<bool> {
        let mut args = argv(["erase"]);
        args.extend_from_slice(udids);
        self.mutate(self.service.timeouts().erase, args, context).await
    }
    async fn mutate(
        &self,
        timeout: jcompat::jtime::Duration,
        args: Vec<String>,
        context: &VendorContext,
    ) -> VendorResult<bool> {
        Ok(self.service.critical_exec(timeout, args, context).await?.successful())
    }
    /// D38: a failed or timed-out probe is not running on every runtime. Kotlin skips the exit
    /// check from runtime 16 on (A41), but a healthy simulator exits 0 there as well: measured on
    /// iOS 18.6 and 27.0, see docs/wp/x17-findings.md.
    pub async fn is_running(&self, udid: &str, runtime_version: &str, context: &VendorContext) -> VendorResult<bool> {
        // Approved P1: ASCII digits only.
        let major = kt::substring_before(runtime_version, ".").parse::<i32>().ok();
        let timeout = self.service.timeouts().shell;
        let args = match major {
            Some(major) if major >= 16 => argv([
                "spawn",
                udid,
                "launchctl",
                "dumpstate",
                "user/foreground/com.apple.SpringBoard",
            ]),
            _ => argv(["spawn", udid, "launchctl", "print", "system"]),
        };
        let Some(result) = self
            .service
            .safe_execute(timeout, args, context)
            .await?
            .filter(|result| result.successful())
        else {
            return Ok(false);
        };
        // A: started, D: starting.
        Ok(result
            .stdout
            .iter()
            .filter(|line| line.contains("com.apple.springboard.services"))
            .filter_map(|line| {
                kt::substring_before(line, "com.apple.springboard.services")
                    .split(' ')
                    .map(kt::trim)
                    .rfind(|token| !kt::is_blank(token))
            })
            .any(|state| state == "A"))
    }
}
