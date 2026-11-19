//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/local/JvmFileBridge.kt

use super::super::{FileBridge, TransferStop, blocking, failure, io_error};
use marathon_core::{
    error::{Cancellation, VendorError, VendorResult},
    runtime::{VendorFuture, VendorRuntime},
};
use marathon_error::FailureKind;
use std::io::{Read, Write};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct JvmFileBridge {
    runtime: Arc<dyn VendorRuntime>,
    overwrite: bool,
}
impl JvmFileBridge {
    pub fn new(runtime: Arc<dyn VendorRuntime>, overwrite: bool) -> Self {
        Self { runtime, overwrite }
    }
}
/// D93: why a copy is to stop, looked at between files and between chunks of one; never for an unstoppable copy.
type Stopped<'a> = &'a dyn Fn() -> Option<Cancellation>;
fn copy_recursively(src: &Path, dst: &Path, overwrite: bool, stopped: Stopped<'_>) -> VendorResult<bool> {
    if let Some(reason) = stopped() {
        return Err(VendorError::Cancelled(reason));
    }
    if !src.exists() {
        return Err(VendorError::Other(failure(
            FailureKind::CopySourceMissing,
            src.to_string_lossy(),
        )));
    }
    if dst.exists() && !(src.is_dir() && dst.is_dir()) {
        if !overwrite {
            return Err(VendorError::Other(failure(
                FailureKind::CopyDestinationExists,
                dst.to_string_lossy(),
            )));
        }
        if dst.is_dir() {
            std::fs::remove_dir_all(dst).map_err(io_error)?;
        } else {
            std::fs::remove_file(dst).map_err(io_error)?;
        }
    }
    if src.is_dir() {
        std::fs::create_dir_all(dst).map_err(io_error)?;
        for entry in std::fs::read_dir(src).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            copy_recursively(&entry.path(), &dst.join(entry.file_name()), overwrite, stopped)?;
        }
    } else {
        if let Some(parent) = dst.parent().filter(|p| !p.as_os_str().is_empty()) {
            let _ = std::fs::create_dir_all(parent);
        }
        // Kotlin copyTo uses streams: source permission bits are not copied.
        let open_error =
            |error: std::io::Error| VendorError::Other(failure(FailureKind::FileNotFound, error.to_string()));
        let mut input = std::fs::File::open(src).map_err(open_error)?;
        let mut output = std::fs::File::create(dst).map_err(open_error)?;
        let mut chunk = vec![0u8; 1 << 20];
        loop {
            if let Some(reason) = stopped() {
                return Err(VendorError::Cancelled(reason));
            }
            let read = input.read(&mut chunk).map_err(io_error)?;
            if read == 0 {
                break;
            }
            output.write_all(&chunk[..read]).map_err(io_error)?;
        }
    }
    Ok(true)
}
impl FileBridge for JvmFileBridge {
    fn send<'a>(&'a self, src: &'a Path, dst: &'a str) -> VendorFuture<'a, bool> {
        let src = src.to_owned();
        let dst = PathBuf::from(dst);
        let overwrite = self.overwrite;
        Box::pin(async move {
            blocking(self.runtime.as_ref(), move || {
                copy_recursively(&src, &dst, overwrite, &|| None)
            })
            .await
        })
    }
    /// D93: a local copy honours its stop as the rsync bridge does: one already stopped (the device sealed or disposed of,
    /// the caller cancelled) does not start, and one in progress ends at the next chunk. The copy owns the stop, so the
    /// device's receipt ends only once the copy has really ended, whoever still waits for it.
    fn send_with<'a>(&'a self, src: &'a Path, dst: &'a str, mut stop: TransferStop) -> VendorFuture<'a, bool> {
        if let Some(stopped) = stop.now() {
            return Box::pin(async move { Err(stopped.into_error()) });
        }
        let src = src.to_owned();
        let dst = PathBuf::from(dst);
        let overwrite = self.overwrite;
        let check = stop.cancellation_check();
        Box::pin(async move {
            blocking(self.runtime.as_ref(), move || {
                let _owner = stop;
                copy_recursively(&src, &dst, overwrite, &check)
            })
            .await
        })
    }
    /// D93 (round 8): a local copy honours a receive's stop as a send's: one already stopped (the device's pulls closed)
    /// does not start, and one in progress ends at the next chunk; the copy owns the stop to its end.
    fn receive_with<'a>(&'a self, src: &'a str, dst: &'a Path, mut stop: TransferStop) -> VendorFuture<'a, bool> {
        if let Some(stopped) = stop.now() {
            return Box::pin(async move { Err(stopped.into_error()) });
        }
        let filename = src.split('/').next_back().unwrap_or_default().to_owned();
        let src = PathBuf::from(src);
        let dst = dst.to_owned();
        let overwrite = self.overwrite;
        let check = stop.cancellation_check();
        Box::pin(async move {
            blocking(self.runtime.as_ref(), move || {
                let _owner = stop;
                let destination = if dst.is_dir() { dst.join(filename) } else { dst };
                if !src.exists() {
                    return Ok(false);
                }
                copy_recursively(&src, &destination, overwrite, &check)
            })
            .await
        })
    }
    fn receive<'a>(&'a self, src: &'a str, dst: &'a Path) -> VendorFuture<'a, bool> {
        let filename = src.split('/').next_back().unwrap_or_default().to_owned();
        let src = PathBuf::from(src);
        let dst = dst.to_owned();
        let overwrite = self.overwrite;
        Box::pin(async move {
            blocking(self.runtime.as_ref(), move || {
                let destination = if dst.is_dir() { dst.join(filename) } else { dst };
                if !src.exists() {
                    return Ok(false);
                }
                copy_recursively(&src, &destination, overwrite, &|| None)
            })
            .await
        })
    }
}
