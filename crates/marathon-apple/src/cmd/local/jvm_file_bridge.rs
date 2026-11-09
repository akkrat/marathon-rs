//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/local/JvmFileBridge.kt

use super::super::{FileBridge, blocking, failure, io_error};
use marathon_core::{
    error::{VendorError, VendorResult},
    runtime::{VendorFuture, VendorRuntime},
};
use marathon_error::FailureKind;
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
fn copy_recursively(src: &Path, dst: &Path, overwrite: bool) -> VendorResult<bool> {
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
            copy_recursively(&entry.path(), &dst.join(entry.file_name()), overwrite)?;
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
        std::io::copy(&mut input, &mut output).map_err(io_error)?;
    }
    Ok(true)
}
impl FileBridge for JvmFileBridge {
    fn send<'a>(&'a self, src: &'a Path, dst: &'a str) -> VendorFuture<'a, bool> {
        let src = src.to_owned();
        let dst = PathBuf::from(dst);
        let overwrite = self.overwrite;
        Box::pin(async move { blocking(self.runtime.as_ref(), move || copy_recursively(&src, &dst, overwrite)).await })
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
                copy_recursively(&src, &destination, overwrite)
            })
            .await
        })
    }
}
