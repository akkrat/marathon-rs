//! Port of core/src/main/kotlin/com/malinskiy/marathon/io/FileManager.kt
//! and io/FileType.kt, io/FolderType.kt. WP 3.9a adapts the merged jcompat file manager off core.

use crate::{
    model::{DeviceInfo, DevicePoolId, Test},
    runtime::ServiceFuture,
};
use jcompat::file_manager::JavaPath;

pub use jcompat::file_manager::{FileType, FolderType};

pub struct FileTarget {
    pub kind: FileType,
    pub pool: DevicePoolId,
    pub device: DeviceInfo,
    pub test: Option<Test>,
    pub test_batch_id: Option<String>,
    pub id: Option<String>,
    pub chunk: Option<String>,
}
impl FileTarget {
    /// Test.toTestName is supplied by the model formatter; path escaping and
    /// truncation remain the merged file manager's responsibility.
    pub fn request<'a>(&'a self, test_name: Option<&'a str>) -> jcompat::file_manager::FileRequest<'a> {
        jcompat::file_manager::FileRequest {
            file_type: self.kind,
            pool: &self.pool.name,
            serial: &self.device.serial_number,
            test_name,
            batch_id: self.test_batch_id.as_deref(),
            id: self.id.as_deref(),
            chunk: self.chunk.as_deref(),
        }
    }
}
/// create_file returns a path after creating directories, not an opened file.
/// WP 3.9a uses FileManager plans and materializes them off core, preserving
/// the merged path rules and C1/C2 suffix bugs.
/// Backend failures propagate as OperationError::Failed; cancellation stays typed.
pub trait OutputFiles: Send + Sync {
    fn output_dir(&self) -> &JavaPath;
    fn create_file<'a>(&'a self, target: &'a FileTarget) -> ServiceFuture<'a, JavaPath>;
    fn create_folder<'a>(
        &'a self,
        kind: FolderType,
        pool: Option<&'a DevicePoolId>,
        device: Option<&'a DeviceInfo>,
    ) -> ServiceFuture<'a, JavaPath>;
    fn create_test_result_file<'a>(&'a self, filename: &'a String) -> ServiceFuture<'a, JavaPath>;
}

/// Called off core. Render Log through the shared console's write_event/Jansi
/// path and Raw through write_text, in vector order. Swallow stdout errors as
/// PrintStream does; retain actual task/cancellation failures.
pub trait ConsoleSink: Send + Sync {
    fn write(&self, records: Vec<crate::logging::OutputRecord>) -> crate::runtime::VendorFuture<'_, ()>;
}

/// Serial owners await each flush before the next transition or tracker call.
#[derive(Clone)]
pub struct OutputDriver {
    pub vendor: std::sync::Arc<dyn crate::runtime::VendorRuntime>,
    pub sink: std::sync::Arc<dyn ConsoleSink>,
}
/// Where a live pump's failed write is left, for the serial flush that waits on the same batch.
pub type PumpErrors = std::sync::Arc<std::sync::Mutex<Option<crate::error::VendorError>>>;

impl OutputDriver {
    pub async fn flush(
        &self,
        queue: &crate::logging::LogQueue,
        cancellation: std::sync::Arc<dyn crate::runtime::CancellationSignal>,
    ) -> crate::error::VendorResult<()> {
        self.flush_into(queue, cancellation, None).await
    }
    /// The live pump's flush. A failed write is stored in `errors` by the write itself, before
    /// the batch's completion barrier is released, so a serial flush that waited on this batch
    /// always finds it there; the flush itself then succeeds.
    pub async fn flush_recording(
        &self,
        queue: &crate::logging::LogQueue,
        cancellation: std::sync::Arc<dyn crate::runtime::CancellationSignal>,
        errors: PumpErrors,
    ) -> crate::error::VendorResult<()> {
        self.flush_into(queue, cancellation, Some(errors)).await
    }
    async fn flush_into(
        &self,
        queue: &crate::logging::LogQueue,
        cancellation: std::sync::Arc<dyn crate::runtime::CancellationSignal>,
        errors: Option<PumpErrors>,
    ) -> crate::error::VendorResult<()> {
        let Some(batch) = queue.serial_output() else {
            return Ok(());
        };
        let sink = self.sink.clone();
        crate::executor::bridge::call_vendor(self.vendor.as_ref(), cancellation, move |_| {
            Box::pin(async move {
                let _done = batch.done;
                if let Some(previous) = batch.previous {
                    let _ = previous.await;
                }
                if batch.records.is_empty() {
                    return Ok(());
                }
                match (sink.write(batch.records).await, errors) {
                    (Err(error), Some(errors)) if !matches!(error, crate::error::VendorError::Cancelled(_)) => {
                        // Published before `_done` drops at the end of this body.
                        if let Ok(mut slot) = errors.lock() {
                            slot.get_or_insert(error);
                        }
                        Ok(())
                    }
                    (result, _) => result,
                }
            })
        })
        .await
    }
}
