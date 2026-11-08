//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt

use super::*;
use jcompat::jtime::Duration;
use marathon_apple::{
    cmd::{
        CommandResult, failure,
        write_file::{MAX_WRITTEN_FILE_SIZE, write_file},
    },
    worker_commands,
};
use marathon_config::vendor::IosPermission;
use marathon_core::{
    runtime::CancellationSignal,
    screenshot::{RgbaFrame, Rotation, Screenshottable},
    transfer::TransferMeasurement,
};
use std::path::Path;

pub(super) async fn blocking<T: Send + 'static>(
    runtime: &dyn VendorRuntime,
    body: impl FnOnce() -> VendorResult<T> + Send + 'static,
) -> VendorResult<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = runtime.spawn_blocking(Box::new(move || {
        let _ = tx.send(body());
        Ok(())
    }));
    task.join().await?;
    rx.await
        .map_err(|e| VendorError::Other(failure("java.lang.IllegalStateException", e.to_string())))?
}
fn io_error(error: std::io::Error) -> VendorError {
    VendorError::Other(failure("java.io.IOException", error.to_string()))
}
impl AppleSimulatorDevice {
    pub async fn critical_listener_command(
        &self,
        command: Vec<String>,
        context: &VendorContext,
    ) -> VendorResult<CommandResult> {
        self.executor
            .critical_execute(
                marathon_apple::cmd::CommandRequest::new(command, Duration::of_seconds(7 * 24 * 60 * 60)),
                context.clone(),
            )
            .await
    }
    pub fn services(&self) -> &SimulatorServices {
        &self.services
    }
    pub(crate) fn setup_failure(&self, message: String) -> VendorError {
        super::setup::setup_error(message)
    }
    /// D54: a send to this simulator ends on its caller's cancellation or when the simulator's own connection is lost.
    fn send_stop(&self, context: &VendorContext) -> marathon_apple::cmd::TransferStop {
        marathon_apple::cmd::send_stop(self.executor.as_ref(), context)
    }
    /// D54: a receive from it ends only when that connection is lost.
    fn receive_stop(&self) -> marathon_apple::cmd::TransferStop {
        marathon_apple::cmd::receive_stop(self.executor.as_ref())
    }
    fn measurement(&self) -> TransferMeasurement {
        let logging = self.services.logging.clone();
        TransferMeasurement {
            io: self.services.runtime.clone(),
            clock: self.services.wall.clone(),
            logger: self.logger.clone(),
            serial: self.serial.as_str().into(),
            log: Arc::new(move |logger, level, message, cause| {
                if logging.enabled(logger, level) {
                    (logging.sink)(logger, level, message.to_string_lossy(), cause.cloned());
                }
            }),
        }
    }
    pub async fn import_media(&self, remote_path: &str, context: &VendorContext) -> VendorResult<bool> {
        Ok(self
            .bin
            .xcrun
            .simctl
            .media_service
            .add_media(&self.udid, remote_path, context)
            .await?
            .successful())
    }
    pub async fn grant(&self, permission: IosPermission, bundle: &str, context: &VendorContext) -> VendorResult<bool> {
        match permission {
            IosPermission::UserTracking | IosPermission::Pasteboard => {
                let env = self.state().env.clone().ok_or_else(|| setup::uninitialized("env"))?;
                let home = super::setup::env_value(&env, "HOME").unwrap_or_else(|| "null".into());
                let path = format!(
                    "{home}/Library/Developer/CoreSimulator/Devices/{}/data/Library/TCC/TCC.db",
                    self.udid
                );
                self.bin.sqlite3.query(&path, &format!("replace into access (service, client, client_type, auth_value, auth_reason, auth_version, flags) values ('{}','{bundle}',0,2,2,1,0);", permission.value()), true, context).await?;
                Ok(true)
            }
            _ => Ok(self
                .bin
                .xcrun
                .simctl
                .privacy
                .grant(&self.udid, permission, bundle, context)
                .await?
                .successful()),
        }
    }
    pub async fn clear_app_container(&self, bundle: &str, context: &VendorContext) -> VendorResult<()> {
        self.bin
            .xcrun
            .simctl
            .application
            .terminate_application(&self.udid, bundle, context)
            .await?;
        let path = self
            .bin
            .xcrun
            .simctl
            .application
            .container_path(
                &self.udid,
                bundle,
                marathon_apple::bin::xcrun::simctl::service::ContainerType::Data,
                context,
            )
            .await?;
        if path.successful() {
            self.remote
                .remove_remote_path(path.combined_stdout().trim(), context)
                .await?;
        } else {
            self.log(
                Level::Warn,
                format!(
                    "Failed to clear app container:\nstdout: {}\nstderr: {}",
                    path.combined_stdout(),
                    path.combined_stderr()
                ),
                None,
            );
        }
        Ok(())
    }
}
impl AppleDevice for AppleSimulatorDevice {
    fn execute_test_request<'a>(
        &'a self,
        request: marathon_apple::test::TestRequest,
        session: Arc<marathon_apple::xcodebuild_session::XcodebuildSession>,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, marathon_apple::xcodebuild_session::EventReceiver> {
        Box::pin(async move {
            let previous = self.state().last_session.clone();
            if let Some(previous) = previous {
                previous.check_closed(&self.serial)?;
            }
            self.state().last_session = Some(session.clone());
            let args = self
                .services
                .vendor
                .xcodebuild_test_args
                .iter()
                .map(|(k, v)| (k.clone(), v.to_string_lossy()))
                .collect::<Vec<_>>();
            Ok(marathon_apple::execute_test_request::execute_test_request(
                self.this()?,
                request,
                session,
                context.clone(),
                marathon_apple::execute_test_request::TestOutput {
                    label: format!("simulator {}", self.udid),
                    args: marathon_apple::bin::xcrun::xcodebuild::simulator_test_args(&args, self.xcode_version()),
                    timer: self.services.timer.clone(),
                    listeners: self.lines.clone(),
                    logging: self.services.logging.clone(),
                },
            ))
        })
    }
    fn udid(&self) -> &str {
        &self.udid
    }
    fn remote_file_manager(&self) -> &RemoteFileManager {
        &self.remote
    }
    fn storage_path(&self) -> &str {
        &self.storage_path
    }
    fn transport(&self) -> &Transport {
        &self.transport
    }
    fn sdk(&self) -> Sdk {
        self.sdk
    }
    fn binary_environment(&self) -> &AppleBinaryEnvironment {
        &self.bin
    }
    fn setup<'a>(&'a self, context: &'a VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let cycle = Arc::new(WorkerProvisioningContext::new(
                self.transport.clone(),
                self.services.monotonic.clone(),
            ));
            let record = cycle
                .simulator(&self.udid, &self.bin, false, context)
                .await
                .map_err(|e| e.into_vendor())?;
            self.setup_record(&record, cycle, context).await
        })
    }
    fn execute_worker_command<'a>(
        &'a self,
        command: Vec<String>,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, Option<CommandResult>> {
        worker_commands::execute_worker_command(
            self.executor.as_ref(),
            &self.services.vendor.timeout_configuration,
            command,
            context,
        )
    }
    fn push_file<'a>(&'a self, src: &'a Path, dst: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.measurement()
                .measure_file_transfer(
                    jcompat::file_manager::file_path(src.to_string_lossy().as_ref()),
                    self.bridge.send_with(src, dst, self.send_stop(context)),
                )
                .await
        })
    }
    fn pull_file<'a>(&'a self, src: &'a str, dst: &'a Path, _: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.measurement()
                .measure_file_transfer(
                    jcompat::file_manager::file_path(dst.to_string_lossy().as_ref()),
                    self.bridge.receive_with(src, dst, self.receive_stop()),
                )
                .await
        })
    }
    fn push_folder<'a>(&'a self, src: &'a Path, dst: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.measurement()
                .measure_folder_transfer(
                    jcompat::file_manager::file_path(src.to_string_lossy().as_ref()),
                    self.bridge.send_with(src, dst, self.send_stop(context)),
                )
                .await
        })
    }
    fn pull_folder<'a>(&'a self, src: &'a str, dst: &'a Path, _: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.measurement()
                .measure_folder_transfer(
                    jcompat::file_manager::file_path(dst.to_string_lossy().as_ref()),
                    self.bridge.receive_with(src, dst, self.receive_stop()),
                )
                .await
        })
    }
    fn push_generated_file<'a>(
        &'a self,
        src: &'a Path,
        dst: &'a str,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            if matches!(self.transport, Transport::Ssh { .. }) && !self.services.vendor.ssh.debug {
                let path = src.to_owned();
                let bytes = blocking(self.services.runtime.as_ref(), move || {
                    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_WRITTEN_FILE_SIZE as u64 {
                        return Ok(None);
                    }
                    std::fs::read(path).map(Some).map_err(io_error)
                })
                .await?;
                if let Some(bytes) = bytes
                    && write_file(
                        self.executor.as_ref(),
                        &bytes,
                        dst,
                        self.services.vendor.timeout_configuration.shell,
                        context.clone(),
                        &self.services.logging,
                    )
                    .await?
                {
                    return Ok(true);
                }
            }
            self.push_file(src, dst, context).await
        })
    }
    fn push_shared<'a>(&'a self, src: &'a Path, dst: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            let path = src.to_owned();
            let folder = blocking(self.services.runtime.as_ref(), move || Ok(path.is_dir())).await?;
            self.shared_transfers
                .send(
                    &self.transport.id().to_string_lossy(),
                    src,
                    dst,
                    self.send_stop(context),
                    || self.remote.exists(dst, context),
                    || async {
                        if folder {
                            self.push_folder(src, dst, context).await
                        } else {
                            self.push_file(src, dst, context).await
                        }
                    },
                )
                .await
        })
    }
    fn install<'a>(&'a self, path: &'a str, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            Ok(self
                .bin
                .xcrun
                .simctl
                .application
                .install(&self.udid, path, context)
                .await?
                .successful())
        })
    }
    fn get_screenshot<'a>(&'a self, _: Duration, dst: &'a Path, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            let cfg = &self
                .services
                .vendor
                .screen_record_configuration
                .screenshot_configuration;
            let path = self.remote.remote_screenshot(&self.udid, cfg.screenshot_type);
            if !self
                .bin
                .xcrun
                .simctl
                .io
                .screenshot(&self.udid, &path, cfg.screenshot_type, cfg.display, cfg.mask, context)
                .await?
            {
                return Ok(false);
            }
            self.pull_file(&path, dst, context).await
        })
    }
    fn start_video_recording<'a>(
        &'a self,
        path: &'a str,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, Option<CommandResult>> {
        Box::pin(async move {
            let cfg = &self.services.vendor.screen_record_configuration.video_configuration;
            self.bin
                .xcrun
                .simctl
                .io
                .record_video(
                    &self.udid,
                    path,
                    cfg.codec,
                    cfg.display,
                    cfg.mask,
                    &self.remote.remote_video_pidfile(),
                    context,
                )
                .await
        })
    }
    fn stop_video_recording<'a>(&'a self, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            let path = self.remote.remote_video_pidfile();
            let result = self
                .execute_worker_command(
                    vec![
                        "sh".into(),
                        "-c".into(),
                        format!("kill -s INT $(cat {path}) && lsof -p $(cat {path}) +r 1 &>/dev/null"),
                    ],
                    context,
                )
                .await?;
            // D17: `lsof +r 1` exits 1 when the recorder is already gone at its first pass (it exits
            // 6-31 ms after the INT, before lsof starts), so Kotlin reports failure, keeps the pidfile and the next
            // recording is lost (A96). A failed stop counts as done only when `ps -p` finds no such pid; probing the
            // pid rather than trusting the INT keeps a live recorder, a probe error and a reused pid on the
            // warn-and-keep path.
            let stopped = result.as_ref().is_some_and(CommandResult::successful) || {
                let probe = self
                    .execute_worker_command(
                        vec!["sh".into(), "-c".into(), format!("ps -p $(cat {path}) -o pid=")],
                        context,
                    )
                    .await;
                match probe {
                    // `ps` on an invalid pid also exits 1 with empty stdout, but it says so on stderr.
                    Ok(probe) => probe.is_some_and(|r| {
                        r.exit_code == Some(1)
                            && r.combined_stdout().trim().is_empty()
                            && r.combined_stderr().is_empty()
                    }),
                    Err(e @ VendorError::Cancelled(_)) => return Err(e),
                    // A probe that could not run keeps the warn-and-keep path instead of aborting the listener.
                    Err(_) => false,
                }
            };
            if !stopped {
                self.log(
                    Level::Warn,
                    format!(
                        "error while killing recording process, stdout={} stderr={}, exitCode={}",
                        result
                            .as_ref()
                            .map(|r| r.combined_stdout())
                            .unwrap_or_else(|| "null".into()),
                        result
                            .as_ref()
                            .map(|r| r.combined_stderr())
                            .unwrap_or_else(|| "null".into()),
                        result
                            .as_ref()
                            .and_then(|r| r.exit_code)
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "null".into())
                    ),
                    None,
                );
                return Ok(false);
            }
            self.remote.remove_remote_path(&path, context).await?;
            Ok(true)
        })
    }
    fn shutdown<'a>(&'a self, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move { self.shutdown_device(context).await })
    }
    fn erase<'a>(&'a self, context: &'a VendorContext) -> VendorFuture<'a, bool> {
        Box::pin(async move { self.erase_device(context).await })
    }
}
impl Screenshottable for AppleSimulatorDevice {
    fn orientation(&self) -> VendorResult<Rotation> {
        Ok(Rotation::Rotation0)
    }
    fn screenshot<'a>(
        &'a self,
        timeout: Duration,
        cancellation: Arc<dyn CancellationSignal>,
    ) -> VendorFuture<'a, Option<RgbaFrame>> {
        Box::pin(async move {
            let extension = format!(
                ".{}",
                self.services
                    .vendor
                    .screen_record_configuration
                    .screenshot_configuration
                    .screenshot_type
                    .value()
            );
            let file = blocking(self.services.runtime.as_ref(), move || {
                tempfile::Builder::new().suffix(&extension).tempfile().map_err(io_error)
            })
            .await?;
            let context = VendorContext {
                cancellation,
                ..self.services.context.clone()
            };
            let result = self.get_screenshot(timeout, file.path(), &context).await;
            blocking(self.services.runtime.as_ref(), move || {
                let image = match result {
                    Ok(true) => std::fs::read(file.path()).map_err(io_error).and_then(|bytes| {
                        match image::load_from_memory(&bytes) {
                            Ok(image) => Ok(Some(image.to_rgba8())),
                            Err(image::ImageError::Unsupported(_)) => Ok(None),
                            Err(error) => Err(VendorError::Other(failure(
                                "javax.imageio.IIOException",
                                error.to_string(),
                            ))),
                        }
                    }),
                    Ok(false) => Ok(None),
                    Err(error) => Err(error),
                };
                // Kotlin ignores the Boolean result of File.delete in finally.
                let _ = file.close();
                image
            })
            .await
        })
    }
}
