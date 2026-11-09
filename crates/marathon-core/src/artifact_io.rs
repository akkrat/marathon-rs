//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/logs/LogWriter.kt
//! and analytics/internal/sub/ExecutionReportGenerator.kt (off-core service calls).

use crate::{error::*, runtime::*};
use jcompat::logging::{Level, Logger};
use marathon_error::FailureKind;
use std::sync::Arc;

/// The run's output task supplies the existing registry/console sink (3.10).
/// Invoked on I/O workers, never while holding a listener collection lock.
pub type ServiceLog = Arc<dyn Fn(&Logger, Level, &str, Option<&Failure>) + Send + Sync>;

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
    let kind = match error.kind() {
        std::io::ErrorKind::NotFound => FailureKind::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => FailureKind::AccessDenied,
        std::io::ErrorKind::NotADirectory => FailureKind::FileSystem,
        std::io::ErrorKind::AlreadyExists => FailureKind::FileAlreadyExists,
        std::io::ErrorKind::InvalidInput => FailureKind::InvalidPath,
        _ => FailureKind::Io,
    };
    VendorError::Other(Failure::from_kind(kind, Some(error.to_string())))
}
pub(crate) fn validate_path(path: &std::path::Path) -> VendorResult<()> {
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(io_error(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Nul character not allowed",
        )));
    }
    Ok(())
}
fn native_reason(error: &std::io::Error) -> String {
    let text = error.to_string();
    text.split(" (os error ").next().unwrap_or(&text).to_owned()
}
pub(crate) fn nio_error(error: std::io::Error, path: &std::path::Path) -> VendorError {
    use std::io::ErrorKind;
    if error.kind() == ErrorKind::InvalidInput {
        return io_error(error);
    }
    let kind = match error.kind() {
        ErrorKind::NotFound => FailureKind::NoSuchFile,
        ErrorKind::PermissionDenied => FailureKind::AccessDenied,
        ErrorKind::AlreadyExists => FailureKind::FileAlreadyExists,
        _ => FailureKind::FileSystem,
    };
    let mut message = path.to_string_lossy().into_owned();
    // UnixFileSystemProvider.createDirectories first tries the whole path,
    // then checks its parent. ENOTDIR escapes that parent access check.
    if error.kind() == ErrorKind::NotADirectory
        && let Some(parent) = path.parent()
        && parent.metadata().is_err_and(|e| e.kind() == ErrorKind::NotADirectory)
        && let Some(index) = message.rfind('/')
    {
        message.truncate(index);
    }
    if kind == FailureKind::FileSystem {
        message.push_str(": ");
        message.push_str(&native_reason(&error));
    }
    VendorError::Other(Failure::from_kind(kind, Some(message)))
}
pub(crate) fn file_open_error(error: std::io::Error, path: &std::path::Path) -> VendorError {
    let mut message = path.to_string_lossy().into_owned();
    message.push_str(" (");
    message.push_str(&native_reason(&error));
    message.push(')');
    VendorError::Other(Failure::from_kind(FailureKind::FileNotFound, Some(message)))
}
pub(crate) fn clock_error(error: crate::time::ReplayError) -> VendorError {
    let mut message = error.label;
    message.push_str(": ");
    message.push_str(&error.message);
    VendorError::Other(Failure::from_kind(FailureKind::ReplayClock, Some(message)))
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
        VendorError::Other(Failure::from_kind(
            FailureKind::IllegalState,
            Some("I/O task finished without a result".into()),
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
        VendorError::Other(Failure::from_kind(
            FailureKind::IllegalState,
            Some("blocking task finished without a result".into()),
        ))
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::ErrorKind, path::Path};

    #[test]
    fn io_funnel_kinds_preserve_each_legacy_class_and_message() {
        for (source, kind, class) in [
            (
                ErrorKind::NotFound,
                FailureKind::NoSuchFile,
                "java.nio.file.NoSuchFileException",
            ),
            (
                ErrorKind::PermissionDenied,
                FailureKind::AccessDenied,
                "java.nio.file.AccessDeniedException",
            ),
            (
                ErrorKind::NotADirectory,
                FailureKind::FileSystem,
                "java.nio.file.FileSystemException",
            ),
            (
                ErrorKind::AlreadyExists,
                FailureKind::FileAlreadyExists,
                "java.nio.file.FileAlreadyExistsException",
            ),
            (
                ErrorKind::InvalidInput,
                FailureKind::InvalidPath,
                "java.nio.file.InvalidPathException",
            ),
            (ErrorKind::Other, FailureKind::Io, "java.io.IOException"),
        ] {
            let error = io_error(std::io::Error::new(source, "reason"));
            let failure = error.failure();
            assert_eq!(failure.kind.legacy_class(), kind.legacy_class());
            assert_eq!(failure.kind.legacy_class(), class);
            assert_eq!(failure.to_java_string(), format!("{class}: reason"));
            assert!(matches!(error, VendorError::Other(_)));
        }
    }

    #[test]
    fn nio_funnel_adds_the_native_reason_only_for_filesystem_failures() {
        for (source, kind, class, message) in [
            (
                ErrorKind::NotFound,
                FailureKind::NoSuchFile,
                "java.nio.file.NoSuchFileException",
                "output",
            ),
            (
                ErrorKind::PermissionDenied,
                FailureKind::AccessDenied,
                "java.nio.file.AccessDeniedException",
                "output",
            ),
            (
                ErrorKind::AlreadyExists,
                FailureKind::FileAlreadyExists,
                "java.nio.file.FileAlreadyExistsException",
                "output",
            ),
            (
                ErrorKind::Other,
                FailureKind::FileSystem,
                "java.nio.file.FileSystemException",
                "output: reason",
            ),
            (
                ErrorKind::InvalidInput,
                FailureKind::InvalidPath,
                "java.nio.file.InvalidPathException",
                "reason (os error 7)",
            ),
        ] {
            let error = nio_error(std::io::Error::new(source, "reason (os error 7)"), Path::new("output"));
            let failure = error.failure();
            assert_eq!(failure.kind.legacy_class(), kind.legacy_class());
            assert_eq!(failure.to_java_string(), format!("{class}: {message}"));
        }
    }

    #[test]
    fn file_open_and_clock_funnels_keep_their_distinct_legacy_names() {
        let error = file_open_error(
            std::io::Error::new(ErrorKind::NotFound, "missing (os error 2)"),
            Path::new("output"),
        );
        assert_eq!(
            error.failure().kind.legacy_class(),
            FailureKind::FileNotFound.legacy_class()
        );
        assert_eq!(
            error.failure().to_java_string(),
            "java.io.FileNotFoundException: output (missing)"
        );

        let error = clock_error(crate::time::ReplayError {
            label: "clock".into(),
            message: "exhausted".into(),
        });
        assert_eq!(
            error.failure().kind.legacy_class(),
            FailureKind::ReplayClock.legacy_class()
        );
        assert_eq!(error.failure().to_java_string(), "ReplayError: clock: exhausted");
        assert_ne!(
            FailureKind::ReplayClock.legacy_class(),
            FailureKind::Replay.legacy_class()
        );
    }
}
