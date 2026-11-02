//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/DistributedDerivedData.kt

use crate::apple_device::AppleDevice;
use jcompat::writers::JavaString;
use marathon_core::{
    error::{VendorError, VendorResult},
    vendor::VendorContext,
};
use std::path::Path;

/// The caller supplies the exception constructor: installer and factory catches differ.
pub async fn require_distributed(
    device: &dyn AppleDevice,
    local: &Path,
    remote_bundle: &str,
    required: &str,
    error: impl FnOnce(JavaString) -> VendorError,
    context: &VendorContext,
) -> VendorResult<()> {
    let problem = match device.remote_file_manager().exists(required, context).await? {
        Some(true) => return Ok(()),
        Some(false) => format!("{required} is missing"),
        None => format!("unable to check {required}"),
    };
    let mut message = JavaString::from(format!("skipCopyingDerivedData is enabled, but {problem} on ").as_str());
    message.append(device.info().serial_number);
    message.push_str(&format!(
        ". Distribute {} to {remote_bundle} on every worker before marathon starts or disable skipCopyingDerivedData",
        local.display()
    ));
    Err(error(message))
}

/// Kotlin's general relativePathTo, including non-descendants, uses the jcompat implementation.
pub(crate) async fn remote_test_bundle(
    device: &dyn AppleDevice,
    bundle: &crate::model::AppleTestBundle,
    runtime: &dyn marathon_core::runtime::VendorRuntime,
) -> VendorResult<String> {
    use jcompat::file_manager::{JavaPath, relative_file};
    let fm = device.remote_file_manager();
    let Some(runner) = &bundle.test_application else {
        return Ok(fm.remote_xctest_file());
    };
    let local = JavaPath::from_text(&bundle.xctest_bundle.to_string_lossy());
    let base = JavaPath::from_text(&runner.to_string_lossy());
    let relative = crate::cmd::blocking(runtime, move || {
        let cwd = std::env::current_dir()
            .map_err(|e| VendorError::Other(crate::cmd::failure("java.io.IOException", e.to_string())))?;
        Ok(relative_file(
            &local,
            &base,
            &JavaPath::from_text(&cwd.to_string_lossy()),
        ))
    })
    .await?;
    Ok(fm.join_path(
        &fm.remote_test_runner_application(),
        &relative.to_string_lossy().split('/').collect::<Vec<_>>(),
    ))
}

pub async fn require_distributed_test_bundle(
    device: &dyn AppleDevice,
    bundle: &crate::model::AppleTestBundle,
    runtime: &dyn marathon_core::runtime::VendorRuntime,
    error: impl Fn(JavaString) -> VendorError,
    context: &VendorContext,
) -> VendorResult<String> {
    let fm = device.remote_file_manager();
    let remote = remote_test_bundle(device, bundle, runtime).await?;
    let binary_path = |base: &str, binary: &Path| {
        let mut parts = bundle.relative_binary_path();
        parts.push(binary.file_name().and_then(|n| n.to_str()).unwrap_or_default());
        fm.join_path(base, &parts)
    };
    if let Some(runner) = &bundle.test_application {
        let runner_remote = fm.remote_test_runner_application();
        let runner_binary = bundle.test_runner_binary().await?;
        let test = bundle.test_binary().await?;
        require_distributed(
            device,
            runner,
            &runner_remote,
            &binary_path(&runner_remote, &runner_binary),
            &error,
            context,
        )
        .await?;
        require_distributed(
            device,
            runner,
            &runner_remote,
            &binary_path(&remote, &test),
            error,
            context,
        )
        .await?;
    } else {
        let test = bundle.test_binary().await?;
        require_distributed(
            device,
            &bundle.xctest_bundle,
            &remote,
            &binary_path(&remote, &test),
            error,
            context,
        )
        .await?;
    }
    Ok(remote)
}
