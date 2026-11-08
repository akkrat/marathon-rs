//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamAndroidDevice.kt
//! (file transfer, package installation, screenshot and recording methods).
use crate::{
    device::AndroidDevice,
    models::{ShellCommandResult, to_screen_recorder_command},
    support::*,
};
use jcompat::{
    file_manager::{JavaPath, escape},
    jtime::Duration,
    logging::Level,
};
use marathon_adb::client::sync::listing::{AndroidFileType, ListFilesRequest};
use marathon_adb::client::{
    AdbError, Feature, framebuffer::ScreenCaptureRequest, pkg::*, sync::directory::PushRequest, sync::*,
};
use marathon_core::{
    error::*,
    runtime::*,
    screenshot::{RgbaFrame, Rotation, Screenshottable},
    transfer::TransferMeasurement,
    vendor::VendorContext,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
impl AndroidDevice {
    fn measurement(&self) -> TransferMeasurement {
        TransferMeasurement {
            io: self.runtime().dependencies.io.clone(),
            clock: self.runtime().dependencies.hooks.clone(),
            logger: self.runtime().logger("AndroidDevice"),
            serial: self.serial_number(),
            log: self.runtime().log.clone(),
        }
    }
    pub async fn pull_file(&self, remote: &str, local: &str, context: VendorContext) -> VendorResult<()> {
        let result = self
            .measurement()
            .measure_file_transfer(
                JavaPath::from_text(local),
                Box::pin(async {
                    let stat = cancellable(context.cancellation.clone(), async {
                        CompatStatFileRequest::new(remote, self.supported_features())
                            .execute(self.bounded_client(), Some(self.adb_serial()))
                            .await
                            .map_err(adb_error)
                    })
                    .await?;
                    if !stat.exists() {
                        return Err(transfer(
                            format!(
                                "Couldn't pull file {remote} from device {} because it doesn't exist",
                                self.serial_number().to_owned()
                            )
                            .as_str(),
                        ));
                    }
                    if stat.size() == 0 {
                        let path = local.to_owned();
                        blocking(
                            self.runtime().dependencies.io.as_ref(),
                            move || match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
                                Ok(_) => Ok(()),
                                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                                Err(e) => Err(io(e)),
                            },
                        )
                        .await?;
                        return Ok(());
                    }
                    let request = CompatPullFileRequest::new(
                        remote,
                        PathBuf::from(local),
                        self.supported_features(),
                        Some(stat.size() as i64),
                    );
                    let completion = request.0.completion();
                    let result = async {
                        let mut stream = request
                            .execute(self.bounded_client(), Some(self.adb_serial()))
                            .map_err(adb_error)?;
                        let mut progress = 0.0;
                        while let Some(update) = cancellable(context.cancellation.clone(), async {
                            stream.next().await.transpose().map_err(adb_error)
                        })
                        .await?
                        {
                            progress = update;
                        }
                        if progress != 1.0 {
                            return Err(transfer(
                                format!(
                                    "Couldn't pull file {remote} from device {}",
                                    self.serial_number().to_owned()
                                )
                                .as_str(),
                            ));
                        }
                        Ok(())
                    }
                    .await;
                    completion.close().await.map_err(adb_error)?;
                    result
                }),
            )
            .await;
        match result {
            Err(e) if e.failure().class == "com.malinskiy.adam.exception.PullFailedException" => Err(with_cause(
                transfer(
                    format!(
                        "Couldn't pull file {remote} from device {}",
                        self.serial_number().to_owned()
                    )
                    .as_str(),
                ),
                e,
            )),
            Err(e) if e.failure().class == "com.malinskiy.adam.exception.UnsupportedSyncProtocolException" => {
                Err(with_cause(
                    transfer(
                        format!(
                            "Device {} does not support sync: file transfer",
                            self.serial_number().to_owned()
                        )
                        .as_str(),
                    ),
                    e,
                ))
            }
            other => other,
        }
    }
    pub async fn push_file(&self, local: &str, remote: &str, verify: bool, context: VendorContext) -> VendorResult<()> {
        let result = self
            .measurement()
            .measure_file_transfer(
                JavaPath::from_text(local),
                Box::pin(async {
                    let request =
                        CompatPushFileRequest::prepare(PathBuf::from(local), remote, self.supported_features(), "0777")
                            .await
                            .map_err(adb_error)?;
                    let completion = request.0.completion();
                    let result = async {
                        let mut stream = request
                            .execute(self.bounded_client(), Some(self.adb_serial()))
                            .map_err(adb_error)?;
                        let mut progress = 0.0;
                        while let Some(update) = cancellable(context.cancellation.clone(), async {
                            stream.next().await.transpose().map_err(adb_error)
                        })
                        .await?
                        {
                            progress = update;
                        }
                        if progress != 1.0 {
                            return Err(transfer(
                                format!(
                                    "Couldn't push file {local} to device {}:{remote}. Last progress: {}",
                                    self.serial_number().to_owned(),
                                    jcompat::jnum::double_to_string(progress)
                                )
                                .as_str(),
                            ));
                        }
                        Ok(())
                    }
                    .await;
                    if let Some(completion) = completion {
                        completion.close().await.map_err(adb_error)?;
                    }
                    result
                }),
            )
            .await;
        match result {
            Err(e) if e.failure().class == "com.malinskiy.adam.exception.PushFailedException" => {
                return Err(with_cause(transfer(e.failure().to_java_string()), e));
            }
            other => other?,
        }
        if verify {
            let path = local.to_owned();
            let hash = blocking(self.runtime().dependencies.io.as_ref(), move || {
                let bytes = std::fs::read(path).map_err(io)?;
                Ok(format!("{:x}", md5::compute(bytes)))
            })
            .await?;
            self.wait_for_remote_file_sync(&hash, remote, context).await?;
        }
        Ok(())
    }
    pub async fn wait_for_remote_file_sync(&self, md5: &str, remote: &str, context: VendorContext) -> VendorResult<()> {
        let command = self.md5_command();
        if command.is_empty() || md5.is_empty() {
            self.log(
                Level::Warn,
                &format!("no md5 was calculated for {remote}. unable to sync"),
                None,
            );
            return Ok(());
        }
        let start = context
            .hooks
            .millis("android.BaseAndroidDevice.waitForRemoteFile")
            .map_err(|e| other("ReplayError", e.message))?;
        // D23: the push timeout bounds the wait for every caller; Kotlin's loop has no bound of its own (N5), and a
        // file sync push has no other. Legacy install's enclosing timeout has the same length and started earlier, so
        // it fires first and keeps its message.
        let timeout = millis(self.runtime().configuration.timeout_configuration.push_file)?;
        let synced = timed(&context, timeout, |signal| {
            let context = VendorContext {
                cancellation: signal,
                ..context.clone()
            };
            let command = &command;
            Box::pin(async move {
                loop {
                    let response = self
                        .safe_execute_shell_command(&format!("{command} {remote}"), "", context.clone())
                        .await?;
                    // The delay occurs even after the matching hash.
                    context.clock.delay(10, context.cancellation.clone()).await?;
                    if response.is_some_and(|r| r.output.to_owned().contains(md5)) {
                        return Ok(());
                    }
                }
            })
        })
        .await?;
        if synced.is_none() {
            return Err(transfer(
                format!(
                    "Remote file {remote} on device {} did not reach md5 {md5} within {timeout} ms",
                    self.serial_number().to_owned()
                )
                .as_str(),
            ));
        }
        let elapsed = context
            .hooks
            .millis("android.BaseAndroidDevice.waitForRemoteFile")
            .map_err(|e| other("ReplayError", e.message))?
            .wrapping_sub(start);
        self.log(Level::Debug, &format!("{remote} synced in {elapsed}ms"), None);
        Ok(())
    }
    pub async fn safe_pull_file(&self, remote: &str, local: &str, context: VendorContext) -> VendorResult<()> {
        let result = timed(
            &context,
            millis(self.runtime().configuration.timeout_configuration.pull_file)?,
            |signal| {
                Box::pin(self.pull_file(
                    remote,
                    local,
                    VendorContext {
                        cancellation: signal,
                        ..context.clone()
                    },
                ))
            },
        )
        .await;
        match result {
            Ok(None) => self.log(Level::Warn, &format!("Pulling {remote} timed out. Ignoring"), None),
            Err(VendorError::Transfer(f)) => {
                self.log(Level::Warn, &format!("Pulling {remote} failed. Ignoring"), Some(&f))
            }
            Err(e) => return Err(e),
            _ => {}
        }
        Ok(())
    }
    pub async fn pull_folder(&self, remote: &str, local: &str, context: VendorContext) -> VendorResult<()> {
        if !tokio::fs::metadata(local).await.is_ok_and(|m| m.is_dir()) {
            return Err(transfer(
                format!("Destination {local} is not a directory").as_str().to_owned(),
            ));
        }
        let mut dirs = vec![remote.to_owned()];
        let mut files = Vec::new();
        while !dirs.is_empty() {
            let mut next = Vec::new();
            for dir in dirs {
                let response = timed(
                    &context,
                    millis(self.runtime().configuration.timeout_configuration.list_files)?,
                    |s| {
                        Box::pin(cancellable(s, async {
                            self.bounded_client()
                                .execute(ListFilesRequest::new(&dir), Some(self.adb_serial()))
                                .await
                                .map_err(adb_error)
                        }))
                    },
                )
                .await?;
                match response {
                    None => self.log(
                        Level::Warn,
                        &format!("Listing {dir} timed out. Ignoring the rest"),
                        None,
                    ),
                    Some(entries) => {
                        for file in entries {
                            if file.file_type == AndroidFileType::Directory {
                                next.push(format!("{}/{}", file.directory, file.name));
                            } else if file.file_type == AndroidFileType::RegularFile {
                                files.push((file.directory, file.name));
                            }
                        }
                    }
                }
            }
            dirs = next;
        }
        for (directory, name) in files {
            let relative = directory
                .split_once(remote)
                .map(|(_, tail)| tail)
                .unwrap_or(&directory)
                .split('\\')
                .collect::<Vec<_>>()
                .join("/");
            let target = format!("{local}/{relative}/");
            tokio::fs::create_dir_all(&target).await.map_err(io)?;
            let source = format!("{directory}/{name}");
            let target = format!("{target}{name}");
            if timed(
                &context,
                millis(self.runtime().configuration.timeout_configuration.pull_file)?,
                |signal| {
                    Box::pin(self.pull_file(
                        &source,
                        &target,
                        VendorContext {
                            cancellation: signal,
                            ..context.clone()
                        },
                    ))
                },
            )
            .await?
            .is_none()
            {
                self.log(Level::Warn, &format!("Pulling {source} timed out. Ignoring"), None);
            }
        }
        Ok(())
    }
    pub async fn safe_pull_folder(&self, remote: &str, local: &str, context: VendorContext) -> VendorResult<()> {
        match self.pull_folder(remote, local, context).await {
            Err(VendorError::Transfer(f)) => {
                self.log(Level::Warn, &format!("Pulling {remote} failed. Ignoring"), Some(&f));
                Ok(())
            }
            r => r,
        }
    }
    pub async fn push_folder(&self, local: &str, remote: &str, context: VendorContext) -> VendorResult<()> {
        if !tokio::fs::metadata(local).await.is_ok_and(|m| m.is_dir()) {
            return Err(transfer(
                format!("Source {local} is not a directory").as_str().to_owned(),
            ));
        }
        let result = timed(
            &context,
            millis(self.runtime().configuration.timeout_configuration.push_folder)?,
            |signal| {
                Box::pin(async move {
                    let request = PushRequest::new(PathBuf::from(local), remote.to_owned(), self.supported_features());
                    let completion = request.completion();
                    let result = cancellable(signal, async {
                        request
                            .execute(self.bounded_client(), Some(self.adb_serial()))
                            .await
                            .map_err(adb_error)
                    })
                    .await;
                    completion.close().await.map_err(adb_error)?;
                    result
                })
            },
        )
        .await?;
        if result.is_none() {
            self.log(Level::Warn, &format!("Pushing {local} timed out. Ignoring"), None)
        }
        Ok(())
    }
    pub async fn safe_uninstall_package(
        &self,
        package: &str,
        keep_data: bool,
        context: VendorContext,
    ) -> VendorResult<Option<ShellCommandResult>> {
        timed(
            &context,
            millis(self.runtime().configuration.timeout_configuration.uninstall)?,
            |signal| {
                Box::pin(cancellable(signal, async {
                    self.bounded_client()
                        .execute(
                            UninstallRemotePackageRequest::new(package, keep_data),
                            Some(self.adb_serial()),
                        )
                        .await
                        .map(Into::into)
                        .map_err(adb_error)
                }))
            },
        )
        .await
    }
    pub async fn install_package(
        &self,
        path: &str,
        reinstall: bool,
        params: &[String],
        context: VendorContext,
    ) -> VendorResult<ShellCommandResult> {
        let features = self.supported_features();
        let args: Vec<_> = params
            .iter()
            .filter(|v| !v.chars().all(jcompat::jstring::is_whitespace))
            .map(String::clone)
            .collect();
        if features.contains(&Feature::AbbExec) || features.contains(&Feature::Cmd) {
            return timed(
                &context,
                millis(self.runtime().configuration.timeout_configuration.install)?,
                |signal| {
                    Box::pin(async move {
                        let request =
                            StreamingPackageInstallRequest::prepare(PathBuf::from(path), features, reinstall, args)
                                .await
                                .map_err(adb_error)?;
                        let completion = request.completion();
                        let result = cancellable(signal, async {
                            self.bounded_client()
                                .execute(request, Some(self.adb_serial()))
                                .await
                                .map_err(adb_error)
                        })
                        .await;
                        if let Some(completion) = completion {
                            completion.close().await.map_err(adb_error)?;
                        }
                        let r = result?;
                        Ok(ShellCommandResult {
                            output: r.output.as_str().into(),
                            exit_code: if r.success { 0 } else { 1 },
                        })
                    })
                },
            )
            .await?
            .ok_or_else(|| install(format!("Timeout installing {path}").as_str().to_owned()));
        }
        let remote = format!(
            "/data/local/tmp/{}",
            escape(
                &Path::new(path)
                    .file_name()
                    .map(|s| s.to_string_lossy())
                    .unwrap_or_default()
            )
        );
        let result = timed(
            &context,
            millis(self.runtime().configuration.timeout_configuration.push_file)?,
            |signal| {
                Box::pin(self.push_file(
                    path,
                    &remote,
                    true,
                    VendorContext {
                        cancellation: signal,
                        ..context.clone()
                    },
                ))
            },
        )
        .await;
        match result {
            Ok(Some(())) => {}
            Ok(None) => return Err(install(format!("Timeout transferring {path}").as_str().to_owned())),
            Err(e @ VendorError::Transfer(_)) => return Err(with_cause(install(e.failure().to_java_string()), e)),
            Err(e) => return Err(e),
        }
        let r = timed(
            &context,
            millis(self.runtime().configuration.timeout_configuration.install)?,
            |signal| {
                Box::pin(cancellable(signal, async {
                    self.bounded_client()
                        .execute(
                            InstallRemotePackageRequest::new(&remote, reinstall, args),
                            Some(self.adb_serial()),
                        )
                        .await
                        .map_err(adb_error)
                }))
            },
        )
        .await?
        .ok_or_else(|| install(format!("Timeout transferring {path}").as_str().to_owned()))?;
        self.safe_execute_shell_command(&format!("rm {remote}"), "", context)
            .await?;
        Ok(ShellCommandResult {
            output: trim(&r.output()).as_str().into(),
            exit_code: r.exit_code,
        })
    }
    pub async fn install_split_packages(
        &self,
        paths: &[String],
        reinstall: bool,
        params: &[String],
        context: VendorContext,
    ) -> VendorResult<String> {
        timed(
            &context,
            millis(self.runtime().configuration.timeout_configuration.install)?,
            |signal| {
                Box::pin(async move {
                    let request = InstallSplitPackageRequest::prepare(
                        paths.iter().map(PathBuf::from).collect(),
                        vec![Feature::Cmd],
                        reinstall,
                        params.iter().map(String::clone).collect(),
                    )
                    .await
                    .map_err(adb_error)?;
                    let completion = request.completion();
                    let result = cancellable(signal, async {
                        request
                            .execute(self.bounded_client(), Some(self.adb_serial()))
                            .await
                            .map_err(adb_error)
                    })
                    .await;
                    completion.close().await.map_err(adb_error)?;
                    Ok(result?.as_str().to_owned())
                })
            },
        )
        .await?
        .ok_or_else(|| install("Timeout transferring absolutePath"))
    }
    pub async fn get_screenshot(&self, timeout: Duration, context: VendorContext) -> VendorResult<Option<RgbaFrame>> {
        let result = timed(&context, millis(timeout)?, |signal| {
            Box::pin(cancellable(signal, async {
                let raw = self
                    .bounded_client()
                    .execute(ScreenCaptureRequest, Some(self.adb_serial()))
                    .await
                    .map_err(adb_error)?;
                let converted = raw.to_buffered_image().map_err(adb_error)?;
                RgbaFrame::from_raw(converted.width, converted.height, converted.rgba)
                    .ok_or_else(|| adb_error(AdbError::UnsupportedImageProtocol("Invalid image dimensions".into())))
            }))
        })
        .await;
        match result {
            Err(e) if e.failure().class == "com.malinskiy.adam.exception.UnsupportedImageProtocolException" => {
                self.log(
                    Level::Warn,
                    &format!(
                        "Unable to retrieve screenshot from device {}",
                        self.serial_number().to_owned()
                    ),
                    Some(e.failure()),
                );
                Ok(None)
            }
            r => r,
        }
    }
    pub async fn safe_start_screen_recorder(
        &self,
        remote: &str,
        options: &marathon_config::vendor::AndroidVideoConfiguration,
        context: VendorContext,
    ) -> VendorResult<()> {
        let cmd = to_screen_recorder_command(options, &remote, self.api_level() >= 34).to_owned();
        let result = timed(
            &context,
            millis(self.runtime().configuration.timeout_configuration.screenrecorder)?,
            |signal| {
                Box::pin(cancellable(signal, async {
                    self.log(Level::Debug, &format!("Running screenrecorder for {remote}"), None);
                    // A silent recording of up to 180 s is why Kotlin's idle unit bug went unnoticed (N12).
                    let r = self
                        .bounded_client()
                        .execute(
                            marathon_adb::client::shell::v1::ShellCommandRequest::new(cmd),
                            Some(self.adb_serial()),
                        )
                        .await
                        .map_err(adb_error)?;
                    let output = r.output();
                    self.log(
                        Level::Debug,
                        &format!(
                            "screenrecord result: {}exit code={}",
                            if trim(&output).is_empty() {
                                String::new()
                            } else {
                                format!("output='{output}'")
                            },
                            r.exit_code
                        ),
                        None,
                    );
                    Ok(())
                }))
            },
        )
        .await;
        match result {
            Err(e @ VendorError::Cancelled(_)) => {
                self.log(Level::Warn, "screenrecord start was interrupted", Some(e.failure()));
                Err(e)
            }
            Err(e) => {
                self.log(Level::Error, "Unable to start screenrecord", Some(e.failure()));
                Err(e)
            }
            _ => Ok(()),
        }
    }
}
fn with_cause(mut error: VendorError, cause: VendorError) -> VendorError {
    match &mut error {
        VendorError::Transfer(f) | VendorError::Other(f) => f.cause = Some(Box::new(cause.failure().clone())),
        _ => {}
    }
    error
}
impl Screenshottable for AndroidDevice {
    fn orientation(&self) -> VendorResult<Rotation> {
        Ok(self.initial_rotation())
    }
    fn screenshot<'a>(
        &'a self,
        timeout: Duration,
        cancellation: Arc<dyn CancellationSignal>,
    ) -> VendorFuture<'a, Option<RgbaFrame>> {
        Box::pin(self.get_screenshot(
            timeout,
            VendorContext {
                cancellation,
                ..self.cleanup_context()
            },
        ))
    }
}
