//! U267: a checked receive for test doubles whose `receive_with` writes straight to its destination. Rust-only test support.
use marathon_apple::cmd::{FileBridge, ReceiveCheck, Received, TransferStop, failure};
use marathon_core::error::{VendorError, VendorResult};
use marathon_error::FailureKind;
use std::path::Path;

fn io_error(error: std::io::Error) -> VendorError {
    VendorError::Other(failure(FailureKind::Io, error.to_string()))
}

/// `FileBridge::receive_checked` for a double: it receives into a private directory beside `dst`, checks the file
/// there and renames it into place only if the check accepts it, as the production bridges' D97 stage does. A
/// rejected or undelivered file is removed with the directory, and an earlier file at `dst` stays. Its file-system
/// calls block the calling thread, which a test double may do and a production bridge may not.
pub async fn receive_checked_in_stage(
    bridge: &dyn FileBridge,
    src: &str,
    dst: &Path,
    stop: TransferStop,
    check: ReceiveCheck,
) -> VendorResult<Received> {
    let parent = dst
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stage = tempfile::Builder::new()
        .prefix(".marathon-artifact-")
        .tempdir_in(parent)
        .map_err(io_error)?;
    let name = dst
        .file_name()
        .ok_or_else(|| VendorError::Other(failure(FailureKind::FileSystem, dst.to_string_lossy())))?;
    let content = stage.path().join(name);
    if !bridge.receive_with(src, &content, stop).await? {
        return Ok(Received::NotReceived);
    }
    if let Err(why) = check(&content).map_err(io_error)? {
        return Ok(Received::Rejected(why));
    }
    std::fs::rename(&content, dst).map_err(io_error)?;
    Ok(Received::Published)
}
