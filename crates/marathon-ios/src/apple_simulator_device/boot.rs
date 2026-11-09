//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt

use super::*;
use crate::simulator::State as SimulatorState;
use marathon_apple::{cmd::failure, plist::PlistValue};
use marathon_error::{AppleKind, FailureKind};

impl AppleSimulatorDevice {
    pub async fn simulator_state(&self, context: &VendorContext) -> VendorResult<SimulatorState> {
        self.fetch_device_descriptor(context).await?;
        let number = self.descriptor_state()?.and_then(|n| i32::try_from(n).ok());
        Ok(match number {
            Some(0) => SimulatorState::Creating,
            Some(1) => SimulatorState::Shutdown,
            Some(3) => {
                if self.is_running(context).await? {
                    SimulatorState::Booted
                } else {
                    SimulatorState::Booting
                }
            }
            _ => SimulatorState::Unknown,
        })
    }
    fn descriptor_state(&self) -> VendorResult<Option<i128>> {
        let value = self.state().descriptor.as_ref().and_then(|p| p.get_cloned("state"));
        // The erased Kotlin generic cast fails at its caller for wrong types (A73).
        Ok(match value {
            None => None,
            Some(PlistValue::Integer(n)) => Some(n),
            Some(other) => {
                return Err(marathon_apple::xctestrun::test_root_factory::FactoryError::from(
                    marathon_apple::plist::PlistError::WrongType {
                        key: "state".into(),
                        expected: "integer",
                        found: other.type_name(),
                    },
                )
                .into_vendor());
            }
        })
    }
    pub async fn is_running(&self, context: &VendorContext) -> VendorResult<bool> {
        let version = self
            .state()
            .runtime_version
            .clone()
            .ok_or_else(|| setup::uninitialized("runtimeVersion"))?;
        self.bin
            .xcrun
            .simctl
            .simulator
            .is_running(&self.udid, &version, context)
            .await
    }
    pub async fn monitor_status(&self, context: &VendorContext) -> VendorResult<bool> {
        self.bin
            .xcrun
            .simctl
            .simulator
            .monitor_status(&self.udid, context)
            .await
    }
    pub async fn boot(&self, context: &VendorContext) -> VendorResult<bool> {
        Ok(self.boot_failure(context).await?.is_none())
    }
    fn nanos(&self, label: &'static str) -> VendorResult<i64> {
        self.services.monotonic.nanos(label).map_err(clock_error)
    }
    pub(super) async fn boot_for_preparation(
        &self,
        context: &VendorContext,
    ) -> Result<(), super::prepare::PrepareError> {
        for attempt in 1..=2 {
            let started = self.nanos("ios.AppleSimulatorDevice.bootForPreparation")?;
            let Some(failure) = self.boot_failure(context).await? else {
                return Ok(());
            };
            let reason = format!(
                "{failure} after {} ms, attempt {attempt} of 2",
                self.nanos("ios.AppleSimulatorDevice.bootForPreparation")?
                    .wrapping_sub(started)
                    / 1_000_000
            );
            self.log(
                Level::WARN,
                format!(
                    "Simulator {} on worker {} failed to boot: {reason}",
                    self.udid,
                    self.executor.host().id
                ),
                None,
            );
            self.shutdown_after_failed_boot(context).await?;
            if attempt == 2 {
                return Err(super::prepare::PrepareError::Boot(format!(
                    "it failed to boot: {reason}"
                )));
            }
            context
                .clock
                .delay(
                    self.services.timeouts.boot_retry_pause_millis,
                    context.cancellation.clone(),
                )
                .await?;
        }
        Ok(())
    }
    async fn boot_failure(&self, context: &VendorContext) -> VendorResult<Option<String>> {
        let wait = self.services.vendor.timeout_configuration.boot_wait;
        let started = self.nanos("ios.AppleSimulatorDevice.nanoTime")?;
        let nanos = wait
            .seconds()
            .checked_mul(1_000_000_000)
            .and_then(|n| n.checked_add(wait.nanos()))
            .ok_or_else(|| VendorError::Other(failure(FailureKind::Arithmetic, "long overflow")))?;
        let deadline = started.saturating_add(nanos);
        match self.simulator_state(context).await? {
            SimulatorState::Booted => return Ok(None),
            SimulatorState::Creating | SimulatorState::Booting => {}
            SimulatorState::Shutdown => {
                if let Err(error) = self.bin.xcrun.simctl.simulator.boot(&self.udid, &[], context).await {
                    if error.failure().kind != FailureKind::Apple(AppleKind::Command) {
                        return Err(error);
                    }
                    let now = self.simulator_state(context).await?;
                    if !matches!(now, SimulatorState::Booting | SimulatorState::Booted) {
                        return Ok(Some(format!(
                            "simctl boot failed ({}), its state is {}",
                            error
                                .failure()
                                .message
                                .as_ref()
                                .map(|s| s.to_owned().lines().map(str::trim).collect::<Vec<_>>().join(" "))
                                .unwrap_or_else(|| "null".into()),
                            now.name()
                        )));
                    }
                }
            }
            SimulatorState::Unknown => return Ok(Some("its state is UNKNOWN".into())),
        }
        loop {
            let state = self.simulator_state(context).await?;
            if state == SimulatorState::Booted {
                self.log(Level::DEBUG, format!("Device {} booted!", self.udid), None);
                return Ok(None);
            }
            let left = deadline.saturating_sub(self.nanos("ios.AppleSimulatorDevice.nanoTime")?);
            if left <= 0 {
                return Ok(Some(format!(
                    "it has not booted within {wait}, its state is {}",
                    state.name()
                )));
            }
            context
                .clock
                .delay(
                    self.services
                        .timeouts
                        .boot_poll_interval_millis
                        .min(left.saturating_add(999_999) / 1_000_000),
                    context.cancellation.clone(),
                )
                .await?;
            self.log(Level::DEBUG, format!("Device {} is still booting...", self.udid), None);
        }
    }
    async fn shutdown_after_failed_boot(&self, context: &VendorContext) -> VendorResult<()> {
        let timeouts = &self.services.vendor.timeout_configuration;
        let budget = jcompat::jtime::duration_of(
            timeouts
                .shutdown
                .seconds()
                .checked_add(timeouts.shell.seconds())
                .ok_or_else(|| VendorError::Other(failure(FailureKind::Arithmetic, "long overflow")))?,
            timeouts.shutdown.nanos() + timeouts.shell.nanos(),
        )
        .ok_or_else(|| VendorError::Other(failure(FailureKind::Arithmetic, "long overflow")))?;
        let outcome = async {
            let millis = budget
                .to_millis()
                .ok_or_else(|| VendorError::Other(failure(FailureKind::Arithmetic, "long overflow")))?;
            context
                .clock
                .with_timeout_or_null(
                    context.cancellation.clone(),
                    millis,
                    Box::new(|signal| {
                        Box::pin(async move {
                            let ctx = VendorContext {
                                cancellation: signal,
                                ..context.clone()
                            };
                            self.fetch_device_descriptor(&ctx).await?;
                            let shutdown = self.descriptor_state()? == Some(1);
                            if !shutdown {
                                self.bin.xcrun.simctl.simulator.shutdown(&self.udid, &ctx).await?;
                            }
                            Ok(())
                        })
                    }),
                )
                .await
        }
        .await;
        match outcome {
            Ok(Some(())) => {}
            Ok(None) => self.log(
                Level::WARN,
                format!(
                    "Simulator {} on worker {}: not shut down within {budget} after a failed boot",
                    self.udid,
                    self.executor.host().id
                ),
                None,
            ),
            Err(e @ VendorError::Cancelled(_)) => return Err(e),
            Err(e) => self.log(
                Level::WARN,
                format!(
                    "Simulator {} on worker {}: unable to shut it down after a failed boot",
                    self.udid,
                    self.executor.host().id
                ),
                Some(e.failure().clone()),
            ),
        }
        Ok(())
    }
    pub async fn wait_for_state(&self, target: SimulatorState, context: &VendorContext) -> VendorResult<bool> {
        for _ in 0..30 {
            if self.simulator_state(context).await? == target {
                return Ok(true);
            }
            context.clock.delay(1_000, context.cancellation.clone()).await?;
        }
        Ok(false)
    }
    pub(super) async fn shutdown_device(&self, context: &VendorContext) -> VendorResult<bool> {
        match self.simulator_state(context).await? {
            SimulatorState::Shutdown => Ok(true),
            SimulatorState::Unknown => Ok(false),
            _ => {
                if self.bin.xcrun.simctl.simulator.shutdown(&self.udid, context).await? {
                    self.wait_for_state(SimulatorState::Shutdown, context).await
                } else {
                    Ok(false)
                }
            }
        }
    }
    pub(super) async fn erase_device(&self, context: &VendorContext) -> VendorResult<bool> {
        if matches!(
            self.simulator_state(context).await?,
            SimulatorState::Creating | SimulatorState::Booting | SimulatorState::Booted
        ) && !self.shutdown_device(context).await?
        {
            return Ok(false);
        }
        self.bin.xcrun.simctl.simulator.erase_udid(&self.udid, context).await
    }
}
