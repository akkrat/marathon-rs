//! Capture helpers for the differential harness (new verification infrastructure).
//!
//! Call these synchronous helpers only outside the core executor, after its run and
//! vendor cleanup have joined. Expected bytes always come from an independent JVM run.
use anyhow::{Result, ensure};
use std::{fs, path::Path};

/// Run an isolated test process with the capture umask. Returns true in the
/// parent after a successful child run; false in the child that should capture.
/// Reexecution avoids changing process-global umask across runtime threads.
pub fn launch_capture_child(test_name: &str) -> Result<bool> {
    if std::env::var("MRS_DIFFERENTIAL_CHILD").as_deref() == Ok(test_name) {
        return Ok(false);
    }
    let output = std::process::Command::new("/bin/sh")
        .args(["-c", "umask 022; exec \"$@\"", "differential-probe"])
        .arg(std::env::current_exe()?)
        .args(["--exact", test_name, "--nocapture"])
        .env("MRS_DIFFERENTIAL_CHILD", test_name)
        .output()?;
    ensure!(
        output.status.success(),
        "capture child failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(true)
}

/// Preserve all entries, including empty directories, modes and symlinks, without
/// dereferencing a link into the host filesystem.
pub fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
        #[cfg(not(unix))]
        anyhow::bail!("differential symlink capture requires Unix");
    } else if metadata.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(destination, metadata.permissions())?;
    } else {
        ensure!(metadata.is_file(), "unsupported capture file kind");
        fs::copy(source, destination)?;
        fs::set_permissions(destination, metadata.permissions())?;
    }
    Ok(())
}

/// Export the named parse replay using the shared 3.10 scenario driver. No public
/// CLI Stub registration is added. This is explicitly an injected-core contract.
pub fn capture_parse(destination: &Path) -> Result<()> {
    fs::create_dir(destination)?;
    let run = crate::scenario::run("parse.json", |_| {});
    ensure!(run.completed, "scenario did not complete");
    let value = match run.value {
        Some(Ok(value)) => value,
        other => anyhow::bail!("scenario outcome: {other:?}"),
    };
    copy_tree(&run.output_dir, &destination.join("reports"))?;
    fs::write(
        destination.join("observation.json"),
        serde_json::to_vec(&serde_json::json!({
            "outcome": {"completed": run.completed, "value": value, "completedAt": run.completed_at},
            "log": run.log,
        }))?,
    )?;
    Ok(())
}
