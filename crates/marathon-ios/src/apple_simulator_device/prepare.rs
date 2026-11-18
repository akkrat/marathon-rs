//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt

use super::*;
use crate::{
    apple_simulator_application_installer::AppleSimulatorApplicationInstaller,
    apple_simulator_media_importer::AppleSimulatorMediaImporter,
};
use marathon_apple::cmd::{cancelled, failure, retained_tasks::panic_as_error};
use marathon_config::vendor::LifecycleAction;
use marathon_core::{error::LeavePoolReason, executor::shared::Signal};
use marathon_error::{AppleKind, FailureKind};

#[derive(Clone)]
pub(super) enum PrepareError {
    Boot(String),
    /// D87: a boot that failed because the preparation's time ran out: a held attempt, or one refused. As `Boot`, but
    /// the installation beside it is left to finish rather than cancelled, so that the deadline never stops a transfer.
    OutOfTime(String),
    /// D87: the installation failed and the deadline refused to start its retry. Not a failure that stops the boot beside
    /// it: the preparation lets the boot end on its own, then reports this error as `Vendor`.
    Refused(VendorError),
    Vendor(VendorError),
}
impl PrepareError {
    /// What a refusal is once nothing runs beside it any more: the error it refused to retry.
    fn settled(self) -> Self {
        match self {
            Self::Refused(error) => Self::Vendor(error),
            error => error,
        }
    }
}
impl From<VendorError> for PrepareError {
    fn from(error: VendorError) -> Self {
        Self::Vendor(error)
    }
}
/// U179: one preparation, which disposal can stop and wait for without joining the retained task that runs it.
pub(super) struct PreparationJob {
    pub(super) signal: Arc<Signal>,
    /// Completed when the preparation's work has ended, whatever its outcome.
    pub(super) done: Arc<SharedDeferred<()>>,
}
/// D87, U163: a transfer that stalled takes the simulator out of the run whatever its boot did at the same time, so it is
/// what the preparation reports, rather than a boot that failed or ran out of time beside it.
fn terminal_transfer<T>(installation: Result<T, PrepareError>) -> Option<PrepareError> {
    match installation {
        Err(PrepareError::Vendor(
            error @ VendorError::LeavePool {
                reason: LeavePoolReason::TransferStalled,
                ..
            },
        )) => Some(PrepareError::Vendor(error)),
        _ => None,
    }
}
impl AppleSimulatorDevice {
    pub(super) async fn prepare_device(&self, context: VendorContext) -> VendorResult<()> {
        let started = self.timing.elapsed()?;
        self.services
            .artifacts
            .await_owner(self.serial.as_str(), &context)
            .await?;
        let keep = self
            .services
            .artifacts
            .kept_sources(self.serial.as_str())
            .iter()
            .map(String::clone)
            .collect::<Vec<_>>();
        let device = self.this()?;
        // U179: registered under the lock that disposal installs itself with, and with no await before the spawn, so
        // disposal either sees this preparation or this one sees disposal.
        let job = {
            let mut state = self.state();
            if state.disposal.is_some() {
                return Err(VendorError::Cancelled(marathon_core::error::Cancellation::Requested(
                    failure(
                        FailureKind::IllegalState,
                        format!("Device {} has been disposed", self.serial),
                    ),
                )));
            }
            let job = Arc::new(PreparationJob {
                signal: Signal::new(),
                done: SharedDeferred::new(),
            });
            state.preparations.push(job.clone());
            job
        };
        let ctx = VendorContext {
            cancellation: job.signal.clone(),
            ..context.clone()
        };
        let completed = SharedDeferred::new();
        let result = completed.clone();
        let running = job.clone();
        self.retained.spawn(self.services.runtime.as_ref(), async move {
            // D84: a panic is this preparation's error too, so that the caller and disposal hear of its end.
            let outcome = panic_as_error(device.services.track.track_device_preparing(
                || device.info().serial_number,
                Box::pin(async {
                    device
                        .prepare_body(started, keep, &ctx)
                        .await
                        .map_err(|error| match error {
                            PrepareError::Vendor(error) | PrepareError::Refused(error) => error,
                            PrepareError::Boot(message) | PrepareError::OutOfTime(message) => {
                                VendorError::Other(failure(FailureKind::Apple(AppleKind::Boot), message))
                            }
                        })
                }),
            ))
            .await
            .map_err(|error| {
                if error.failure().kind == FailureKind::Apple(AppleKind::Boot) {
                    PrepareError::Boot(error.failure().message.to_owned().unwrap_or_default())
                } else {
                    PrepareError::Vendor(error)
                }
            });
            result.complete(Ok(outcome));
            device.state().preparations.retain(|p| !Arc::ptr_eq(p, &running));
            running.done.complete(Ok(()));
            Ok(())
        });
        let result = match completed.wait_in(context.cancellation.clone()).await {
            Ok(outcome) => (*outcome).clone(),
            Err(error) => {
                job.signal.cancel(marathon_core::error::Cancellation::Requested(failure(
                    FailureKind::JobCancelled,
                    "DeferredCoroutine was cancelled",
                )));
                return Err(error);
            }
        };
        match result {
            Ok(()) => {}
            Err(PrepareError::Vendor(error) | PrepareError::Refused(error)) => {
                // U163: out of the run, like a simulator that is not admitted, so recovery doesn't set it up again.
                if matches!(
                    error,
                    VendorError::LeavePool {
                        reason: LeavePoolReason::TransferStalled,
                        ..
                    }
                ) {
                    self.retire();
                }
                return Err(error);
            }
            Err(PrepareError::Boot(reason) | PrepareError::OutOfTime(reason)) => {
                self.mark_lost();
                return Err(VendorError::LeavePool {
                    reason: LeavePoolReason::NotBooted,
                    detail: failure(
                        FailureKind::Apple(AppleKind::NotBooted),
                        format!(
                            "Simulator {} on worker {} is taken out of its pool: {reason}",
                            self.udid,
                            self.executor.host().id
                        ),
                    ),
                });
            }
        }
        let admission = self.state().admission.take();
        if let Some(reason) = admission.map(|check| check()).transpose()?.flatten() {
            self.retire();
            return Err(VendorError::LeavePool {
                reason: LeavePoolReason::NotAdmitted,
                detail: failure(
                    FailureKind::Apple(AppleKind::NotAdmitted),
                    format!("{} is not admitted to the run: {reason}", self.serial),
                ),
            });
        }
        // D87: ready, so a later loss starts afresh.
        self.finish_preparation();
        Ok(())
    }
    async fn prepare_body(&self, started: i64, keep: Vec<String>, context: &VendorContext) -> Result<(), PrepareError> {
        self.remote.remove_remote_directory(&keep, context).await?;
        self.remote.create_remote_directory(None, context).await?;
        self.remote.create_remote_shared_directory(context).await?;
        self.execute_worker_command(
            vec![
                "pkill".into(),
                "-f".into(),
                format!("'simctl io {} recordVideo'", self.udid),
            ],
            context,
        )
        .await?;
        let cleaned = self.timing.elapsed()?;
        let installation_signal = Signal::new();
        let simulator_signal = Signal::new();
        let installation_ctx = VendorContext {
            cancellation: Arc::new(LinkedSignal {
                parent: context.cancellation.clone(),
                local: installation_signal.clone(),
            }),
            ..context.clone()
        };
        let simulator_ctx = VendorContext {
            cancellation: Arc::new(LinkedSignal {
                parent: context.cancellation.clone(),
                local: simulator_signal.clone(),
            }),
            ..context.clone()
        };
        let installation = async {
            let start = self.timing.elapsed()?;
            let installer = AppleSimulatorApplicationInstaller::new(self);
            match installer.prepare_installation(&installation_ctx).await {
                Ok(()) => {}
                Err(error) if installer.refused() => return Err(PrepareError::Refused(error)),
                Err(error) => return Err(error.into()),
            }
            let bundle = self.load_bundle().await?;
            self.state().bundle = Some(Arc::new(bundle));
            Ok::<_, PrepareError>(self.timing.elapsed()?.wrapping_sub(start))
        };
        let simulator = async {
            let start = self.timing.elapsed()?;
            self.handle_lifecycle(&self.services.vendor.lifecycle_configuration.on_prepare, &simulator_ctx)
                .await?;
            self.disable_hardware_keyboard(&simulator_ctx).await?;
            self.boot_for_preparation(&simulator_ctx).await?;
            AppleSimulatorMediaImporter::new(self)
                .import_media(&simulator_ctx)
                .await?;
            Ok::<_, PrepareError>(self.timing.elapsed()?.wrapping_sub(start))
        };
        tokio::pin!(installation, simulator);
        let (installation_time, simulator_time) = tokio::select! {
            result = &mut installation => match result {
                Ok(time) => (time, simulator.await?),
                // D87: a retry the deadline refused stops nothing beside it; the boot ends on its own first.
                Err(error @ PrepareError::Refused(_)) => { let _ = simulator.await; return Err(error.settled()); },
                Err(error) => { simulator_signal.cancel(cancelled("Preparation sibling failed")); let _ = simulator.await; return Err(error); },
            },
            result = &mut simulator => match result {
                Ok(time) => (installation.await.map_err(PrepareError::settled)?, time),
                // D87: the installation runs on; a transfer in progress ends as it would.
                Err(PrepareError::OutOfTime(reason)) => return Err(terminal_transfer(installation.await).unwrap_or(PrepareError::Boot(reason))),
                Err(error) => { installation_signal.cancel(cancelled("Preparation sibling failed")); return Err(terminal_transfer(installation.await).unwrap_or(error.settled())); },
            },
            reason = context.cancellation.cancelled() => {
                installation_signal.cancel(reason.clone()); simulator_signal.cancel(reason.clone());
                let _ = tokio::join!(installation, simulator);
                return Err(VendorError::Cancelled(reason).into());
            },
        };
        self.timing.info(|| Ok(format!("Simulator {} prepared in {} ms: cleanup {} ms, installation {installation_time} ms, boot {simulator_time} ms", self.serial, self.timing.elapsed()?.wrapping_sub(started), cleaned.wrapping_sub(started))))?;
        Ok(())
    }
    async fn load_bundle(&self) -> VendorResult<AppleTestBundle> {
        let bundle = self
            .services
            .vendor
            .bundle
            .as_ref()
            .ok_or_else(|| VendorError::Other(failure(FailureKind::IllegalArgument, "No test bundle provided")))?;
        AppleTestBundle::new(
            bundle.app.clone(),
            bundle.test_app.clone(),
            bundle
                .xctest
                .as_ref()
                .ok_or_else(|| VendorError::Other(failure(FailureKind::IllegalArgument, "No test bundle provided")))?
                .clone(),
            self.sdk,
            self.services.runtime.clone(),
            self.services.logging.clone(),
        )
        .await
    }
    pub(super) async fn handle_lifecycle(
        &self,
        actions: &[LifecycleAction],
        context: &VendorContext,
    ) -> VendorResult<()> {
        if actions.contains(&LifecycleAction::Terminate) {
            self.terminate_running_simulator(context).await?;
        }
        if actions.contains(&LifecycleAction::Shutdown) || actions.contains(&LifecycleAction::Erase) {
            let success = self.shutdown_device(context).await?;
            self.log(
                if success { Level::INFO } else { Level::WARN },
                if success {
                    format!("Simulator {} shutdown", self.udid)
                } else {
                    format!("Exception shutting down simulator {}", self.udid)
                },
                None,
            );
        }
        if actions.contains(&LifecycleAction::Erase) {
            let success = self.erase_device(context).await?;
            self.log(
                if success { Level::INFO } else { Level::WARN },
                if success {
                    format!("Erased simulator {}", self.udid)
                } else {
                    format!("Exception erasing simulator {}", self.udid)
                },
                None,
            );
        }
        Ok(())
    }
    async fn terminate_running_simulator(&self, context: &VendorContext) -> VendorResult<()> {
        let command = vec![
            "sh".into(),
            "-c".into(),
            format!("ps aux | grep {} | grep -v grep", self.udid),
        ];
        let ps = self
            .execute_worker_command(command.clone(), context)
            .await?
            .map(|r| r.combined_stdout())
            .unwrap_or_default();
        if ps.trim().is_empty() {
            return Ok(());
        }
        let result = self
            .execute_worker_command(
                vec![
                    "/usr/bin/pkill".into(),
                    "-9".into(),
                    "-l".into(),
                    "-f".into(),
                    self.udid.clone(),
                ],
                context,
            )
            .await?;
        if result.as_ref().is_some_and(|r| r.successful()) {
            self.log(Level::DEBUG, "Terminated loaded simulators", None);
        } else {
            self.log(
                Level::WARN,
                format!(
                    "Failed to terminate loaded simulators, stdout={}, stderr={}",
                    result
                        .as_ref()
                        .map(|r| r.combined_stdout())
                        .unwrap_or_else(|| "null".into()),
                    result
                        .as_ref()
                        .map(|r| r.combined_stderr())
                        .unwrap_or_else(|| "null".into())
                ),
                None,
            );
        }
        let ps = self
            .execute_worker_command(command, context)
            .await?
            .map(|r| r.combined_stdout().trim().to_owned())
            .unwrap_or_default();
        if !ps.is_empty() {
            self.log(
                Level::DEBUG,
                format!("Terminated loaded simulators, but there are still some processes with simulator udid: \n{ps}"),
                None,
            );
        }
        Ok(())
    }
    async fn disable_hardware_keyboard(&self, context: &VendorContext) -> VendorResult<()> {
        let home = self.state().home.clone().ok_or_else(|| setup::uninitialized("home"))?;
        let file = format!("{home}/Library/Preferences/com.apple.iphonesimulator.plist");
        let key = format!(":DevicePreferences:{}:ConnectHardwareKeyboard", self.udid);
        self.bin.plist_buddy.add(&file, &key, "bool", "false", context).await?;
        self.bin.plist_buddy.set(&file, &key, "false", context).await?;
        self.log(
            Level::DEBUG,
            format!("Disabled hardware keyboard for {}", self.udid),
            None,
        );
        Ok(())
    }
}
