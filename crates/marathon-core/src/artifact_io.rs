//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/logs/LogWriter.kt
//! and analytics/internal/sub/ExecutionReportGenerator.kt (off-core service calls).

use crate::{error::*, runtime::*};
use jcompat::logging::{Level, Logger};
use std::sync::Arc;

/// The run's output task supplies the existing registry/console sink (3.10).
/// Invoked on I/O workers, never while holding a listener collection lock.
pub type ServiceLog = Arc<dyn Fn(&Logger, Level, &String, Option<&Failure>) + Send + Sync>;

pub(crate) fn operation_error(error: VendorError) -> OperationError {
    match error {
        VendorError::Cancelled(c) => OperationError::Cancelled(c),
        other => OperationError::Failed(other.failure().clone()),
    }
}
pub(crate) fn vendor_error(error: OperationError) -> VendorError {
    match error {
        OperationError::Cancelled(c) => VendorError::Cancelled(c),
        OperationError::Failed(f) => VendorError::Other(f),
    }
}
pub(crate) fn io_error(error: std::io::Error) -> VendorError {
    let class = match error.kind() {
        std::io::ErrorKind::NotFound => "java.nio.file.NoSuchFileException",
        std::io::ErrorKind::PermissionDenied => "java.nio.file.AccessDeniedException",
        std::io::ErrorKind::NotADirectory => "java.nio.file.FileSystemException",
        std::io::ErrorKind::AlreadyExists => "java.nio.file.FileAlreadyExistsException",
        std::io::ErrorKind::InvalidInput => "java.nio.file.InvalidPathException",
        _ => "java.io.IOException",
    };
    VendorError::Other(crate::executor::shared::failure(class, &error.to_string()))
}
fn native_reason(error: &std::io::Error) -> String {
    let text = error.to_string();
    text.split(" (os error ").next().unwrap_or(&text).to_owned()
}
pub(crate) fn nio_error(error: std::io::Error, path: &jcompat::file_manager::JavaPath) -> VendorError {
    use std::io::ErrorKind;
    if error.kind() == ErrorKind::InvalidInput {
        return io_error(error);
    }
    let class = match error.kind() {
        ErrorKind::NotFound => "java.nio.file.NoSuchFileException",
        ErrorKind::PermissionDenied => "java.nio.file.AccessDeniedException",
        ErrorKind::AlreadyExists => "java.nio.file.FileAlreadyExistsException",
        _ => "java.nio.file.FileSystemException",
    };
    let mut message = String::from_utf16_lossy(&path.0.clone());
    // UnixFileSystemProvider.createDirectories first tries the whole path,
    // then checks its parent. ENOTDIR escapes that parent access check.
    if error.kind() == ErrorKind::NotADirectory
        && let Ok(native) = path.to_path_buf()
        && let Some(parent) = native.parent()
        && parent.metadata().is_err_and(|e| e.kind() == ErrorKind::NotADirectory)
        && let Some(index) = message.rfind('/')
    {
        message.truncate(index);
    }
    if class == "java.nio.file.FileSystemException" {
        message.push_str(": ");
        message.push_str(&native_reason(&error));
    }
    VendorError::Other(Failure {
        class: class.into(),
        message: Some(message),
        cause: None,
        stacktrace: None,
    })
}
pub(crate) fn file_open_error(error: std::io::Error, path: &jcompat::file_manager::JavaPath) -> VendorError {
    let mut message = String::from_utf16_lossy(&path.0.clone());
    message.push_str(" (");
    message.push_str(&native_reason(&error));
    message.push(')');
    VendorError::Other(Failure {
        class: "java.io.FileNotFoundException".into(),
        message: Some(message),
        cause: None,
        stacktrace: None,
    })
}
pub(crate) fn clock_error(error: crate::time::ReplayError) -> VendorError {
    let mut message = error.label;
    message.push_str(": ");
    message.push_str(&error.message);
    VendorError::Other(Failure {
        class: "ReplayError".into(),
        message: Some(message),
        cause: None,
        stacktrace: None,
    })
}

pub(crate) async fn on_io<T: Send + 'static>(
    io: &dyn VendorRuntime,
    body: VendorFuture<'static, T>,
) -> VendorResult<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = io.spawn(Box::pin(async move {
        let _ = tx.send(body.await);
        Ok(())
    }));
    task.join().await?;
    rx.await.map_err(|_| {
        VendorError::Other(crate::executor::shared::failure(
            "java.lang.IllegalStateException",
            "I/O task finished without a result",
        ))
    })?
}
pub(crate) async fn blocking<T: Send + 'static>(
    io: &dyn VendorRuntime,
    body: impl FnOnce() -> VendorResult<T> + Send + 'static,
) -> VendorResult<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = io.spawn_blocking(Box::new(move || {
        let _ = tx.send(body());
        Ok(())
    }));
    task.join().await?;
    rx.await.map_err(|_| {
        VendorError::Other(crate::executor::shared::failure(
            "java.lang.IllegalStateException",
            "blocking task finished without a result",
        ))
    })?
}
