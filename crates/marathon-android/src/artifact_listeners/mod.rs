//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/BaseAndroidDevice.kt:236-335
pub mod adam_capture;
pub mod file_sync;
pub mod profiling;
pub mod screenshot;
pub mod video;

use crate::{device::AndroidDevice, support::*};
use jcompat::file_manager::JavaPath;
use marathon_core::{error::*, listener::*, model::*, output::*, vendor::Device};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct ArtifactContext {
    pub device: AndroidDevice,
    pub pool: DevicePoolId,
    pub batch: BatchRef,
}
impl ArtifactContext {
    pub async fn file(
        &self,
        kind: FileType,
        test: Option<&Test>,
        id: Option<String>,
        chunk: Option<String>,
    ) -> VendorResult<JavaPath> {
        self.device
            .runtime()
            .dependencies
            .files
            .create_file(&FileTarget {
                kind,
                pool: self.pool.clone(),
                device: self.device.info(),
                test: test.cloned(),
                test_batch_id: Some(self.batch.id.clone()),
                id,
                chunk,
            })
            .await
            .map_err(operation_error)
    }
    pub fn now(&self, label: &'static str) -> VendorResult<i64> {
        self.device
            .runtime()
            .dependencies
            .hooks
            .millis(label)
            .map_err(|e| other("ReplayError", e.message))
    }
}
pub(crate) fn operation_error(e: OperationError) -> VendorError {
    match e {
        OperationError::Cancelled(c) => VendorError::Cancelled(c),
        OperationError::Failed(f) => VendorError::Other(f),
    }
}
#[derive(Default)]
pub(crate) struct Attachments(Mutex<Vec<Arc<dyn AttachmentListener>>>);
impl Attachments {
    pub fn register(&self, listener: Arc<dyn AttachmentListener>) -> VendorResult<()> {
        lock(&self.0).push(listener);
        Ok(())
    }
    pub fn listeners(&self) -> Vec<Arc<dyn AttachmentListener>> {
        lock(&self.0).clone()
    }
    pub fn emit(&self, test: &Test, file: JavaPath, kind: AttachmentType, name: &str) -> VendorResult<()> {
        for listener in self.listeners() {
            listener.on_attachment(
                test,
                Attachment {
                    file: file.clone(),
                    kind,
                    name: Some(name.into()),
                },
            )?;
        }
        Ok(())
    }
}

/// Profiling inherits its execute cancellation while retaining an explicit child handle.
pub(crate) struct ChildSignal(
    pub Arc<dyn marathon_core::runtime::CancellationSignal>,
    pub Arc<dyn marathon_core::runtime::CancellationSignal>,
);
impl marathon_core::runtime::CancellationSignal for ChildSignal {
    fn reason(&self) -> Option<Cancellation> {
        self.0.reason().or_else(|| self.1.reason())
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Cancellation> + Send + '_>> {
        Box::pin(async move {
            tokio::select! { biased; c=self.0.cancelled()=>c, c=self.1.cancelled()=>c }
        })
    }
}
