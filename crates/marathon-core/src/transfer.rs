//! Port of core/src/main/kotlin/com/malinskiy/marathon/device/file/FileTransferExtensions.kt

use crate::{artifact_io::*, error::VendorResult, runtime::*, time::WallClock};
use jcompat::{
    file_manager::JavaPath,
    formatting::format_fixed,
    logging::{Level, Logger},
    writers::JavaString,
};
use std::{path::Path, sync::Arc};

#[derive(Clone)]
pub struct TransferMeasurement {
    pub io: Arc<dyn VendorRuntime>,
    pub clock: Arc<dyn WallClock>,
    pub logger: Logger,
    pub serial: JavaString,
    pub log: ServiceLog,
}
impl TransferMeasurement {
    pub async fn measure_file_transfer<'a, T: Send + 'a>(
        &self,
        file: JavaPath,
        body: VendorFuture<'a, T>,
    ) -> VendorResult<T> {
        self.measure(file, body, false).await
    }
    pub async fn measure_folder_transfer<'a, T: Send + 'a>(
        &self,
        file: JavaPath,
        body: VendorFuture<'a, T>,
    ) -> VendorResult<T> {
        self.measure(file, body, true).await
    }
    async fn measure<'a, T: Send + 'a>(
        &self,
        file: JavaPath,
        body: VendorFuture<'a, T>,
        folder: bool,
    ) -> VendorResult<T> {
        let label = if folder {
            "core.FileTransferExtensions.measureFolderTransfer"
        } else {
            "core.FileTransferExtensions.measureFileTransfer"
        };
        let clock = self.clock.clone();
        let start = on_io(
            self.io.as_ref(),
            Box::pin(async move { clock.millis(label).map_err(clock_error) }),
        )
        .await?;
        // Vendor work belongs to the caller, not a detached I/O task.
        let result = body.await;
        if folder && result.is_err() {
            return result;
        }
        let clock = self.clock.clone();
        let elapsed = on_io(
            self.io.as_ref(),
            Box::pin(async move { clock.millis(label).map_err(clock_error) }),
        )
        .await?
        .wrapping_sub(start);
        let this = self.clone();
        blocking(self.io.as_ref(), move || {
            this.emit(file, elapsed, folder);
            Ok(())
        })
        .await?;
        result
    }

    fn emit(&self, file: JavaPath, elapsed: i64, folder: bool) {
        let Ok(path) = file.to_file_path_buf() else {
            return;
        };
        let Ok(metadata) = std::fs::metadata(&path) else {
            return;
        };
        let (dirs, files, bytes) = if folder {
            if !metadata.is_dir() {
                return;
            }
            let mut counts = (0i32, 0i32, 0i64);
            walk(&path, &mut counts);
            counts
        } else {
            (0, 0, metadata.len() as i64)
        };
        let seconds = elapsed as f64 / 1000.0;
        if seconds <= 0.0 || bytes <= 0 {
            return;
        }
        let (Some(speed), Some(seconds)) = (
            format_fixed((bytes / 1000) as f64 / seconds, 2),
            format_fixed(seconds, 4),
        ) else {
            return;
        };
        // Retain UTF-16 basename and serial until the log sink's encoding boundary.
        let mut name = file.0.as_slice();
        while name.len() > 1 && name.last() == Some(&47) {
            name = &name[..name.len() - 1];
        }
        let base = name.iter().rposition(|&u| u == 47).map_or(name, |i| &name[i + 1..]);
        let mut message = JavaString::from("Transferred ");
        message.0.extend_from_slice(base);
        if folder {
            message.push_str(" with ");
            message.push_str(&dirs.to_string());
            message.push_str(" dirs and ");
            message.push_str(&files.to_string());
            message.push_str(" files");
        }
        message.push_str(" to/from ");
        message.append(self.serial.clone());
        message.push_str(". ");
        message.push_str(&speed);
        message.push_str(" KB/s (");
        message.push_str(&bytes.to_string());
        message.push_str(" bytes in ");
        message.push_str(&seconds);
        message.push_str(")");
        (self.log)(&self.logger, Level::Debug, &message, None);
    }
}
fn walk(path: &Path, counts: &mut (i32, i32, i64)) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        counts.0 = counts.0.wrapping_add(1);
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                walk(&entry.path(), counts);
            }
        }
    } else if metadata.is_file() {
        counts.1 = counts.1.wrapping_add(1);
        counts.2 = counts.2.wrapping_add(metadata.len() as i64);
    }
}
