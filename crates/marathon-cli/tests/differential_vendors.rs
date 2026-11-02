//! Whole-vendor differential captures against immutable independent JVM recordings.
use std::{path::Path, process::Command};

#[test]
fn full_vendor_manifests_streams_transports_and_final_state() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let runs = repo.join("tools/differential/runs");
    std::fs::create_dir_all(&runs)?;
    let work = tempfile::Builder::new().prefix("native-gate-").tempdir_in(runs)?;
    let output = Command::new("python3")
        .current_dir(&repo)
        .args(["-I", "tools/differential/run", "native-vendors", "--output"])
        .arg(work.path().join("run"))
        .output()?;
    if !output.status.success() {
        let retained = work.keep();
        return Err(format!(
            "vendor differential failed; evidence retained at {}\n{}{}",
            retained.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}
