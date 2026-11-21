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
    /// `may_continue`: D87, whether the second attempt may start; false once the preparation is failing beside it.
    pub(super) async fn boot_for_preparation(
        &self,
        context: &VendorContext,
        may_continue: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<(), super::prepare::PrepareError> {
        use super::prepare::PrepareError;
        let pause = self.services.timeouts.boot_retry_pause_millis;
        for attempt in 1..=2 {
            // D87: a replacement's second attempt is held to its preparation's deadline; every other boot is whole.
            let bound = if attempt == 2 { self.held_boot_end() } else { None };
            let started = self.nanos("ios.AppleSimulatorDevice.bootForPreparation")?;
            let Some(failure) = self.boot_failure(context, bound).await? else {
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
                let reason = format!("it failed to boot: {reason}");
                return Err(match bound {
                    Some(_) => PrepareError::OutOfTime(reason),
                    None => PrepareError::Boot(reason),
                });
            }
            // D87: no held attempt whose first state read could not start after the pause.
            let out_of_time = || {
                PrepareError::OutOfTime(format!(
                    "it failed to boot: {reason}, and its preparation time has run out"
                ))
            };
            let bound = self.held_boot_end();
            if !self.may_start_command(bound, pause, self.state_read_cost()) {
                return Err(out_of_time());
            }
            context.clock.delay(pause, context.cancellation.clone()).await?;
            if !self.may_start_command(bound, 0, self.state_read_cost()) || !may_continue() {
                return Err(out_of_time());
            }
        }
        Ok(())
    }
    /// D87: the most a command limited to `timeout` takes to end, from asking for its channel to D30's escalation; the
    /// one measure every check of a held attempt uses.
    fn command_cost(timeout: jcompat::jtime::Duration) -> i64 {
        marathon_apple::cmd::remote::ssh::sshj::client::command_worst_case_millis(
            timeout.to_millis().unwrap_or(i64::MAX),
        )
    }
    /// A state read: the descriptor read and, on a booting simulator, the readiness probe, each under `shell`.
    fn state_read_cost(&self) -> i64 {
        Self::command_cost(self.services.vendor.timeout_configuration.shell).saturating_mul(2)
    }
    /// D87's test for a command of a held attempt: it may start `after` ms from now only if, run to its own limit, it
    /// ends by `bound`. The deadline never cuts a command; it only keeps one from starting. Without a bound, always.
    fn may_start_command(&self, bound: Option<i64>, after: i64, cost: i64) -> bool {
        bound.is_none_or(|bound| self.vendor_now().saturating_add(after).saturating_add(cost) <= bound)
    }
    /// The simulator's state, if a read may start within `bound`.
    async fn state_if_fits(&self, bound: Option<i64>, context: &VendorContext) -> VendorResult<Option<SimulatorState>> {
        if !self.may_start_command(bound, 0, self.state_read_cost()) {
            return Ok(None);
        }
        self.simulator_state(context).await.map(Some)
    }
    /// `bound`: D87's end of a held attempt, on the vendor clock. A read, probe or `simctl boot` of the attempt starts only
    /// if it ends by then, even run to its limit, so that the shutdown after it starts with its reserve intact and sees
    /// the simulator's real state: nothing of the attempt is still running.
    async fn boot_failure(&self, context: &VendorContext, bound: Option<i64>) -> VendorResult<Option<String>> {
        let wait = self.services.vendor.timeout_configuration.boot_wait;
        let started = self.nanos("ios.AppleSimulatorDevice.nanoTime")?;
        let nanos = wait
            .seconds()
            .checked_mul(1_000_000_000)
            .and_then(|n| n.checked_add(wait.nanos()))
            .ok_or_else(|| VendorError::Other(failure(FailureKind::Arithmetic, "long overflow")))?;
        let deadline = started.saturating_add(nanos);
        let out_of_time = |state: Option<SimulatorState>| {
            Some(format!(
                "it has not booted within its preparation time, its state is {}",
                state.map_or("not read", |s| s.name())
            ))
        };
        // D79: a simulator seen booting (device.plist state 3) that later reads Shutdown was shut down or
        // crashed and will not boot by itself, so the wait ends there as a failed attempt instead of at
        // `bootWait`. Shutdown before the first Booting read (e.g. a device.plist not yet rewritten after
        // `simctl boot`, or a Creating simulator) keeps the Kotlin wait.
        let mut seen_booting = false;
        let Some(first) = self.state_if_fits(bound, context).await? else {
            return Ok(out_of_time(None));
        };
        match first {
            SimulatorState::Booted => return Ok(None),
            SimulatorState::Creating => {}
            SimulatorState::Booting => seen_booting = true,
            SimulatorState::Shutdown => {
                let boot = Self::command_cost(self.services.vendor.timeout_configuration.boot);
                if !self.may_start_command(bound, 0, boot) {
                    return Ok(out_of_time(Some(first)));
                }
                match self.bin.xcrun.simctl.simulator.boot(&self.udid, &[], context).await {
                    // D98 (x102): booted from Shutdown, whoever shut it down (a crash, a failed boot's own shutdown):
                    // its video service is fresh.
                    Ok(_) => self.services.busy_video.restarted(&self.video_key),
                    Err(error) => {
                        if error.failure().kind != FailureKind::Apple(AppleKind::Command) {
                            return Err(error);
                        }
                        let Some(now) = self.state_if_fits(bound, context).await? else {
                            return Ok(out_of_time(None));
                        };
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
            }
            SimulatorState::Unknown => return Ok(Some("its state is UNKNOWN".into())),
        }
        loop {
            let Some(state) = self.state_if_fits(bound, context).await? else {
                return Ok(out_of_time(None));
            };
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
                    "it has not booted within {wait}, its state is {}",
                    state.name()
                )));
            }
            let held = bound.map(|end| end.saturating_sub(self.vendor_now()));
            if held.is_some_and(|held| held <= 0) {
                return Ok(out_of_time(Some(state)));
            }
            let pause = self
                .services
                .timeouts
                .boot_poll_interval_millis
                .min(left.saturating_add(999_999) / 1_000_000);
            context
                .clock
                .delay(held.map_or(pause, |held| pause.min(held)), context.cancellation.clone())
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
        let down = self.shut_down(context).await;
        // D98 (x102): a simulator that has shut down gets a fresh video service when its next preparation boots it.
        if matches!(down, Ok(true)) {
            self.services.busy_video.restarted(&self.video_key);
        }
        down
    }
    async fn shut_down(&self, context: &VendorContext) -> VendorResult<bool> {
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
