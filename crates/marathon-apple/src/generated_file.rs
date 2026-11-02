//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt (pushGeneratedFile).

use crate::{
    apple_device::AppleDevice,
    cmd::{
        failure,
        remote::ssh::sshj::config::SshLogging,
        write_file::{MAX_WRITTEN_FILE_SIZE, write_file},
    },
};
use jcompat::jtime::Duration;
use marathon_config::devices::Transport;
use marathon_core::{
    error::{VendorError, VendorResult},
    vendor::VendorContext,
};
use std::path::Path;

/// Concrete simulator devices delegate here; fallback uses push_file to retain transfer measurement.
pub async fn push_generated_file(
    device: &dyn AppleDevice,
    src: &Path,
    dst: &str,
    ssh_debug: bool,
    shell_timeout: Duration,
    logging: &SshLogging,
    context: &VendorContext,
) -> VendorResult<bool> {
    if matches!(device.transport(), Transport::Ssh { .. }) && !ssh_debug {
        // File.length() is zero on a failed stat; readBytes() then reports the failure.
        let length = tokio::fs::metadata(src).await.map_or(0, |m| m.len());
        if length <= MAX_WRITTEN_FILE_SIZE as u64 {
            let content = tokio::fs::read(src).await.map_err(|error| {
                let reason = match error.kind() {
                    std::io::ErrorKind::NotFound => Some("No such file or directory"),
                    std::io::ErrorKind::PermissionDenied => Some("Permission denied"),
                    std::io::ErrorKind::IsADirectory => Some("Is a directory"),
                    _ => None,
                };
                VendorError::Other(match reason {
                    Some(reason) => failure("java.io.FileNotFoundException", format!("{} ({reason})", src.display())),
                    None => failure("java.io.IOException", error.to_string()),
                })
            })?;
            if write_file(
                device.binary_environment().command_executor.as_ref(),
                &content,
                dst,
                shell_timeout,
                context.clone(),
                logging,
            )
            .await?
            {
                return Ok(true);
            }
        }
    }
    device.push_file(src, dst, context).await
}
