//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/ResultBundleRunListener.kt

use marathon_logging::{Component, Level};

use super::AppleTestRunListener;
use crate::{
    apple_device::AppleDevice,
    artifact_processor::{ArtifactProcessor, Undelivered},
    cmd::remote::ssh::sshj::config::SshLogging,
    logparser::parser::DeviceFailureReason,
};
use jcompat::file_manager::{FileManager, FolderType};
use marathon_config::vendor::{PullingPolicy, XcresultConfiguration};
use marathon_core::{
    error::{VendorError, VendorResult},
    executor::shared::failure,
    model::{BatchRef, DevicePoolId, Test},
    runtime::VendorFuture,
    vendor::VendorContext,
};
use marathon_error::FailureKind;
use std::sync::{
    Arc, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
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
    logger: Component,
    failed: AtomicBool,
    delivered: AtomicBool,
    remote_path: OnceLock<String>,
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
        let logger = logging.component("marathon_apple::listener::result_bundle_run_listener");
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
                delivered: AtomicBool::new(false),
                remote_path: OnceLock::new(),
            }),
            processor,
        }
    }
}
impl Bundle {
    fn remote(&self) -> &str {
        self.remote_path
            .get_or_init(|| self.device.remote_file_manager().remote_xcresult_file(&self.batch))
    }
    fn owner(&self) -> String {
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
    fn log(&self, level: Level, message: String) {
        if self.logging.enabled(&self.logger, level) {
            (self.logging.sink)(&self.logger, level, message.to_owned(), None);
        }
    }
    fn now(&self, label: &'static str) -> VendorResult<i64> {
        self.context
            .hooks
            .nanos(label)
            .map_err(|e| VendorError::Other(failure(FailureKind::Replay, &e.message.to_owned())))
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
                .remove_remote_path(self.remote(), &context)
                .await?;
            let elapsed = self
                .now("apple.ResultBundleRunListener.processBundle")?
                .wrapping_sub(start)
                / 1_000_000;
            self.log(
                Level::DEBUG,
                format!(
                    "xcresult cleanup for batch {} took {elapsed} ms",
                    self.batch.id.to_owned()
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
                .plan_folder(FolderType::DeviceFiles, Some(&self.pool.name), Some(&owner));
            let fm_logger = {
                let registry = self.logging.registry.lock().unwrap_or_else(|e| e.into_inner());
                let name = "FileManager";
                registry.component(name)
            };
            let local = plan
                .materialize(|message| {
                    if self.logging.enabled(&fm_logger, Level::ERROR) {
                        (self.logging.sink)(&fm_logger, Level::ERROR, message.to_owned(), None);
                    }
                })
                .map_err(|e| io_failure(e, ""))?
                .join("xcresult");
            std::fs::create_dir_all(&local).map_err(|e| io_failure(e, &local.to_string_lossy()))?;
            let destination = local.join(format!("{}.xcresult", self.batch.id));
            let pulled = self.device.pull_bundle(self.remote(), &destination, context).await?;
            if pulled {
                self.delivered.store(true, Ordering::Release);
            }
            let elapsed = self.now("apple.ResultBundleRunListener.deliver")?.wrapping_sub(start) / 1_000_000;
            if pulled {
                self.log(
                    Level::DEBUG,
                    format!("xcresult pull for batch {} took {elapsed} ms", self.batch.id.to_owned())
                        .as_str()
                        .into(),
                );
            }
            Ok((!pulled).then_some(String::from("pull failed")))
        }
        .await;
        match outcome {
            Ok(None) => Ok(true),
            Ok(Some(reason)) => {
                self.undelivered(&reason);
                Ok(false)
            }
            Err(error @ VendorError::Cancelled(_)) => {
                if self.processor().is_none() {
                    self.undelivered(&error.failure().to_string());
                }
                Err(error)
            }
            Err(error) => {
                self.undelivered(&error.failure().to_string());
                Ok(false)
            }
        }
    }
    fn undelivered(&self, reason: &str) {
        if self.delivered.load(Ordering::Acquire) {
            return;
        }
        let mut diagnostic = String::from("xcresult bundle of batch ");
        diagnostic.push_str(&self.batch.id.clone());
        diagnostic.push_str(" was not delivered (");
        diagnostic.push_str(reason);
        diagnostic.push_str("); remote bundle kept on worker ");
        diagnostic.push_str(&self.device.transport().id());
        diagnostic.push_str(" (");
        diagnostic.push_str(&self.owner());
        diagnostic.push_str(") at ");
        diagnostic.push_str(self.remote());
        diagnostic.push_str("; local copy: none");
        if self
            .processor()
            .is_some_and(|p| !p.report_undelivered(&self.owner(), Some(self.remote()), diagnostic.clone()))
        {
            return;
        }
        self.log(Level::ERROR, diagnostic);
    }
}
fn io_failure(error: std::io::Error, path: &str) -> VendorError {
    let class = match error.kind() {
        std::io::ErrorKind::NotFound => FailureKind::NoSuchFile,
        std::io::ErrorKind::AlreadyExists => FailureKind::FileAlreadyExists,
        std::io::ErrorKind::PermissionDenied => FailureKind::AccessDenied,
        _ => FailureKind::FileSystem,
    };
    VendorError::Other(failure(class, path))
}
impl AppleTestRunListener for ResultBundleRunListener {
    fn before_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if let Some(processor) = &self.processor {
                processor
                    .protect(
                        self.inner.owner(),
                        self.inner.remote().to_owned(),
                        self.inner.callback(),
                    )
                    .map_err(VendorError::Other)?;
            }
            Ok(())
        })
    }
    fn test_failed<'a>(&'a self, _: &'a Test, _: i64, _: i64, _: Option<&'a str>) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.inner.failed.store(true, Ordering::Release);
            Ok(())
        })
    }
    fn test_run_failed<'a>(&'a self, _: &'a str, _: DeviceFailureReason) -> VendorFuture<'a, ()> {
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
                    Arc::new(|_: &str| Ok(()))
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
