//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/listener/TestRunListener.kt
//! and LineListener.kt, report/logs/ and report/attachment/.

use crate::{
    error::VendorResult,
    model::{Attachment, DeviceInfo, DevicePoolId, Test},
    runtime::{ServiceFuture, VendorFuture},
};
use jcompat::file_manager::JavaPath;
use std::sync::Arc;

/// Sequential awaited callbacks. testEnded may be absent on interruption.
/// Composite order is registration order, with no isolation of thrown errors.
pub trait TestRunListener: Send + Sync {
    fn before_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn test_started<'a>(&'a self, test: &'a Test) -> VendorFuture<'a, ()>;
    fn test_ended<'a>(&'a self, test: &'a Test) -> VendorFuture<'a, ()>;
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
pub trait LineListener: Send + Sync {
    fn on_line<'a>(&'a self, line: &'a str) -> VendorFuture<'a, ()>;
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
/// Retain the same Arc for removal: Kotlin removes the listener object, not
/// every registration with a matching name. Duplicate adds remain observable.
pub trait LogProducer: Send + Sync {
    fn add_line_listener(&self, listener: Arc<dyn LineListener>) -> VendorResult<()>;
    fn remove_line_listener(&self, listener: &Arc<dyn LineListener>) -> VendorResult<()>;
}
pub struct LogTarget {
    pub pool: DevicePoolId,
    pub batch_id: String,
    pub device: DeviceInfo,
    /// None selects batch logs, retaining C1's batchId.batchId filename in 3.9a.
    pub test: Option<Test>,
}
/// Backend failures propagate as OperationError::Failed; cancellation stays typed.
pub trait LogWriter: Send + Sync {
    fn save_logs<'a>(&'a self, target: &'a LogTarget, logs: &'a [String]) -> ServiceFuture<'a, JavaPath>;
    fn append_logs<'a>(
        &'a self,
        pool: &'a DevicePoolId,
        device: &'a DeviceInfo,
        test: &'a Test,
        log: &'a str,
    ) -> ServiceFuture<'a, ()>;
}
/// In-memory synchronous callback. Must not perform file I/O or block on core.
/// Collector keys use Test equality (metadata ignored); list insertion order stays.
pub trait AttachmentListener: Send + Sync {
    fn on_attachment(&self, test: &Test, attachment: Attachment) -> VendorResult<()>;
}
pub trait AttachmentProvider: Send + Sync {
    fn register_listener(&self, listener: Arc<dyn AttachmentListener>) -> VendorResult<()>;
}
/// Empty checks are filesystem work, hence awaitable; File.exists/length rules
/// and their handling of missing/unreadable files belong to WP 3.9a.
pub trait AttachmentFiles: Send + Sync {
    fn is_empty<'a>(&'a self, attachment: &'a Attachment) -> VendorFuture<'a, bool>;
}
