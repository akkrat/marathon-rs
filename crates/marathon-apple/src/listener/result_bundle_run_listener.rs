//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/ResultBundleRunListener.kt

use super::AppleTestRunListener;
use crate::{
    apple_device::AppleDevice,
    artifact_processor::{ArtifactProcessor, Undelivered},
    cmd::remote::ssh::sshj::config::SshLogging,
    logparser::parser::DeviceFailureReason,
};
use jcompat::{
    file_manager::{FileManager, FolderType},
    logging::{Level, Logger},
    writers::JavaString,
};
use marathon_config::vendor::{PullingPolicy, XcresultConfiguration};
use marathon_core::{
    error::{Failure, VendorError, VendorResult},
    executor::shared::failure,
    model::{BatchRef, DevicePoolId, Test},
    runtime::VendorFuture,
    vendor::VendorContext,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

/// Listener callbacks, including the synchronous macOS path, run on vendor I/O.
/// Callbacks retained by the queue hold a weak processor to avoid a Rust Arc cycle.
pub struct ResultBundleRunListener {
    inner: Arc<Bundle>,
    processor: Option<Arc<ArtifactProcessor>>,
}
struct Bundle {
    device: Arc<dyn AppleDevice>,
    configuration: XcresultConfiguration,
    pool: DevicePoolId,
    batch: BatchRef,
    files: Arc<FileManager>,
    processor: Option<Weak<ArtifactProcessor>>,
    context: VendorContext,
    logging: SshLogging,
    logger: Logger,
    failed: AtomicBool,
    local_copy: Mutex<Option<PathBuf>>,
    remote_path: OnceLock<JavaString>,
}
impl ResultBundleRunListener {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: Arc<dyn AppleDevice>,
        configuration: XcresultConfiguration,
        pool: DevicePoolId,
        batch: BatchRef,
        files: Arc<FileManager>,
        processor: Option<Arc<ArtifactProcessor>>,
        context: VendorContext,
        logging: SshLogging,
    ) -> Self {
        let logger = logging.logger("com.malinskiy.marathon.apple.listener.ResultBundleRunListener");
        Self {
            inner: Arc::new(Bundle {
                device,
                configuration,
                pool,
                batch,
                files,
                processor: processor.as_ref().map(Arc::downgrade),
                context,
                logging,
                logger,
                failed: AtomicBool::new(false),
                local_copy: Mutex::new(None),
                remote_path: OnceLock::new(),
            }),
            processor,
        }
    }
}
impl Bundle {
    fn remote(&self) -> &JavaString {
        self.remote_path
            .get_or_init(|| self.device.remote_file_manager().remote_xcresult_file(&self.batch))
    }
    fn owner(&self) -> JavaString {
        self.device.info().serial_number
    }
    fn processor(&self) -> Option<Arc<ArtifactProcessor>> {
        self.processor.as_ref().and_then(Weak::upgrade)
    }
    fn callback(self: &Arc<Self>) -> Undelivered {
        let this = self.clone();
        Arc::new(move |reason| {
            this.undelivered(reason);
            Ok(())
        })
    }
    fn log(&self, level: Level, message: JavaString) {
        if self.logging.enabled(&self.logger, level) {
            (self.logging.sink)(&self.logger, level, message.to_string_lossy(), None);
        }
    }
    fn now(&self, label: &'static str) -> VendorResult<i64> {
        self.context
            .hooks
            .nanos(label)
            .map_err(|e| VendorError::Other(failure("marathon.ReplayError", &e.message.to_string_lossy())))
    }
    fn pull_needed(&self) -> bool {
        match self.configuration.pulling_policy {
            PullingPolicy::Always => true,
            PullingPolicy::Never => false,
            PullingPolicy::OnFailure => self.failed.load(Ordering::Acquire),
        }
    }
    async fn process(&self, pull: bool, context: VendorContext) -> VendorResult<()> {
        if pull {
            if !self.deliver(&context).await? {
                return Ok(());
            }
            if let Some(processor) = self.processor() {
                processor.release(&self.owner(), self.remote());
            }
        }
        if self.configuration.remote_clean {
            let start = self.now("apple.ResultBundleRunListener.processBundle")?;
            self.device
                .remote_file_manager()
                .remove_remote_path(&self.remote().to_string_lossy(), &context)
                .await?;
            let elapsed = self
                .now("apple.ResultBundleRunListener.processBundle")?
                .wrapping_sub(start)
                / 1_000_000;
            self.log(
                Level::Debug,
                format!(
                    "xcresult cleanup for batch {} took {elapsed} ms",
                    self.batch.id.to_string_lossy()
                )
                .as_str()
                .into(),
            );
        }
        Ok(())
    }
    async fn deliver(&self, context: &VendorContext) -> VendorResult<bool> {
        let outcome = async {
            let start = self.now("apple.ResultBundleRunListener.deliver")?;
            let owner = self.owner();
            let plan = self
                .files
                .plan_folder_utf16(FolderType::DeviceFiles, Some(&self.pool.name.0), Some(&owner.0));
            let fm_logger = {
                let mut registry = self.logging.registry.lock().unwrap_or_else(|e| e.into_inner());
                let name = "com.malinskiy.marathon.io.FileManager";
                if registry.is_registered(name) {
                    registry.slf4j_logger(name)
                } else {
                    registry.logger(name, None)
                }
            };
            let local = plan
                .materialize(|message| {
                    if self.logging.enabled(&fm_logger, Level::Error) {
                        (self.logging.sink)(
                            &fm_logger,
                            Level::Error,
                            JavaString(message.to_vec()).to_string_lossy(),
                            None,
                        );
                    }
                })
                .map_err(|e| io_failure(e, ""))?
                .to_path_buf()
                .map_err(|e| io_failure(e, ""))?
                .join("xcresult");
            // File.mkdirs ignores its Boolean result; the pull/move supplies the failure.
            let _ = std::fs::create_dir_all(&local);
            let remote = self.remote().to_string_lossy();
            let copy = local.join(remote.rsplit('/').next().unwrap_or(&remote));
            *self.local_copy.lock().unwrap_or_else(|e| e.into_inner()) = Some(copy.clone());
            let pulled = self.device.pull_folder(&remote, &local, context).await?;
            let elapsed = self.now("apple.ResultBundleRunListener.deliver")?.wrapping_sub(start) / 1_000_000;
            self.log(
                Level::Debug,
                format!(
                    "xcresult pull for batch {} took {elapsed} ms",
                    self.batch.id.to_string_lossy()
                )
                .as_str()
                .into(),
            );
            if !pulled {
                return Ok(Some(JavaString::from("pull failed")));
            }
            let destination = local.join(format!("{}.xcresult", self.batch.id.to_string_lossy()));
            move_without_replace(&copy, &destination).map_err(VendorError::Other)?;
            Ok(None)
        }
        .await;
        match outcome {
            Ok(None) => Ok(true),
            Ok(Some(reason)) => {
                self.undelivered(&reason);
                Ok(false)
            }
            Err(error @ VendorError::Cancelled(_)) => Err(error),
            Err(error) => {
                self.undelivered(&error.failure().to_java_string());
                Ok(false)
            }
        }
    }
    fn undelivered(&self, reason: &JavaString) {
        let copy = self.local_copy.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let local = copy
            .filter(|p| p.exists())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "none".into());
        let mut diagnostic = JavaString::from("xcresult bundle of batch ");
        diagnostic.append(self.batch.id.clone());
        diagnostic.push_str(" was not delivered (");
        diagnostic.append(reason.clone());
        diagnostic.push_str("); remote bundle kept on worker ");
        diagnostic.append(self.device.transport().id());
        diagnostic.push_str(" (");
        diagnostic.append(self.owner());
        diagnostic.push_str(") at ");
        diagnostic.append(self.remote().clone());
        diagnostic.push_str("; local copy: ");
        diagnostic.push_str(&local);
        if self
            .processor()
            .is_some_and(|p| !p.report_undelivered(&self.owner(), Some(self.remote()), diagnostic.clone()))
        {
            return;
        }
        self.log(Level::Error, diagnostic);
    }
}
fn io_failure(error: std::io::Error, path: &str) -> VendorError {
    let class = match error.kind() {
        std::io::ErrorKind::NotFound => "java.nio.file.NoSuchFileException",
        std::io::ErrorKind::AlreadyExists => "java.nio.file.FileAlreadyExistsException",
        std::io::ErrorKind::PermissionDenied => "java.nio.file.AccessDeniedException",
        _ => "java.nio.file.FileSystemException",
    };
    VendorError::Other(failure(class, path))
}
fn move_without_replace(source: &Path, destination: &Path) -> Result<(), Failure> {
    // UnixCopyFile.move checks the source before the destination, then renames.
    std::fs::symlink_metadata(source).map_err(|e| io_failure(e, &source.to_string_lossy()).failure().clone())?;
    match std::fs::symlink_metadata(destination) {
        Ok(_) => {
            return Err(failure(
                "java.nio.file.FileAlreadyExistsException",
                &destination.to_string_lossy(),
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_failure(e, &destination.to_string_lossy()).failure().clone()),
    }
    std::fs::rename(source, destination).map_err(|e| {
        io_failure(
            e,
            &format!("{} -> {}", source.to_string_lossy(), destination.to_string_lossy()),
        )
        .failure()
        .clone()
    })
}
impl AppleTestRunListener for ResultBundleRunListener {
    fn before_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if let Some(processor) = &self.processor {
                processor
                    .protect(self.inner.owner(), self.inner.remote().clone(), self.inner.callback())
                    .map_err(VendorError::Other)?;
            }
            Ok(())
        })
    }
    fn test_failed<'a>(&'a self, _: &'a Test, _: i64, _: i64, _: Option<&'a JavaString>) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.inner.failed.store(true, Ordering::Release);
            Ok(())
        })
    }
    fn test_run_failed<'a>(&'a self, _: &'a JavaString, _: DeviceFailureReason) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.inner.failed.store(true, Ordering::Release);
            Ok(())
        })
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let pull = self.inner.pull_needed();
            if !pull && let Some(processor) = &self.processor {
                processor.release(&self.inner.owner(), self.inner.remote());
            }
            if !pull && !self.inner.configuration.remote_clean {
                return Ok(());
            }
            if let Some(processor) = &self.processor {
                let inner = self.inner.clone();
                let callback = if pull {
                    inner.callback()
                } else {
                    Arc::new(|_: &JavaString| Ok(()))
                };
                processor
                    .submit(
                        inner.owner(),
                        inner.batch.id.clone(),
                        inner.device.transport().id(),
                        Some(callback),
                        Arc::new(move |context| {
                            let inner = inner.clone();
                            Box::pin(async move { inner.process(pull, context).await })
                        }),
                        self.inner.context.clone(),
                    )
                    .await?;
                Ok(())
            } else {
                self.inner.process(pull, self.inner.context.clone()).await
            }
        })
    }
}
