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
        Ok(self.boot_failure(context, None).await?.is_none())
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
            // D87: within what is left of the preparation's time, except for its first boot.
            let window = self.boot_window_millis();
            let Some(failure) = self.boot_failure(context, window).await? else {
                return Ok(());
            };
            let suffix = format!(
                "after {} ms, attempt {attempt} of 2",
                self.nanos("ios.AppleSimulatorDevice.bootForPreparation")?
                    .wrapping_sub(started)
                    / 1_000_000
            );
            let reason = format!("{failure} {suffix}");
            self.log(
                Level::WARN,
                format!(
                    "Simulator {} on worker {} failed to boot: {} {suffix}",
                    self.udid,
                    self.executor.host().id,
                    marathon_core::error::boot_failure_for_log(&failure)
                ),
                None,
            );
            self.shutdown_after_failed_boot(context).await?;
            if attempt == 2 {
                return Err(super::prepare::PrepareError::Boot(format!(
                    "it failed to boot: {reason}"
                )));
            }
            // D87: no second attempt that would have no time left to wait once the pause is over.
            if !self.time_for_another_boot(self.services.timeouts.boot_retry_pause_millis) {
                return Err(super::prepare::PrepareError::Boot(format!(
                    "it failed to boot: {reason}, and its preparation time has run out"
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
    /// `window`: D87's limit on the wait, in millis, when it is shorter than `bootWait`; zero or less boots nothing.
    async fn boot_failure(&self, context: &VendorContext, window: Option<i64>) -> VendorResult<Option<String>> {
        let wait = self.services.vendor.timeout_configuration.boot_wait;
        let started = self.nanos("ios.AppleSimulatorDevice.nanoTime")?;
        let nanos = wait
            .seconds()
            .checked_mul(1_000_000_000)
            .and_then(|n| n.checked_add(wait.nanos()))
            .ok_or_else(|| VendorError::Other(failure(FailureKind::Arithmetic, "long overflow")))?;
        let window = window.filter(|millis| millis.saturating_mul(1_000_000) < nanos);
        let nanos = window.map_or(nanos, |millis| millis.max(0).saturating_mul(1_000_000));
        let waited = || match window {
            Some(millis) => format!("the {millis} ms left of its preparation time"),
            None => wait.to_string(),
        };
        let deadline = started.saturating_add(nanos);
        // D79: a simulator seen booting (device.plist state 3) that later reads Shutdown was shut down or
        // crashed and will not boot by itself, so the wait ends there as a failed attempt instead of at
        // `bootWait`. Shutdown before the first Booting read (e.g. a device.plist not yet rewritten after
        // `simctl boot`, or a Creating simulator) keeps the Kotlin wait.
        let mut seen_booting = false;
        let first = self.simulator_state(context).await?;
        if first != SimulatorState::Booted && window.is_some_and(|millis| millis <= 0) {
            return Ok(Some(format!(
                "its preparation time has run out, its state is {}",
                first.name()
            )));
        }
        match first {
            SimulatorState::Booted => return Ok(None),
            SimulatorState::Creating => {}
            SimulatorState::Booting => seen_booting = true,
            SimulatorState::Shutdown => {
                if let Err(error) = self.bin.xcrun.simctl.simulator.boot(&self.udid, &[], context).await {
                    if error.failure().kind != FailureKind::Apple(AppleKind::Command) {
                        return Err(error);
                    }
                    let now = self.simulator_state(context).await?;
                    seen_booting = now == SimulatorState::Booting;
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
            match state {
                SimulatorState::Booting => seen_booting = true,
                SimulatorState::Shutdown if seen_booting => {
                    return Ok(Some("it shut down while booting".into()));
                }
                _ => {}
            }
            let left = deadline.saturating_sub(self.nanos("ios.AppleSimulatorDevice.nanoTime")?);
            if left <= 0 {
                return Ok(Some(format!(
                    "it has not booted within {}, its state is {}",
                    waited(),
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
                    "Simulator {} on worker {}: not shut down within {} after a failed boot",
                    self.udid,
                    self.executor.host().id,
                    marathon_core::error::duration_for_log(budget)
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
