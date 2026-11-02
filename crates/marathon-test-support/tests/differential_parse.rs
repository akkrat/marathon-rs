//! Independent full-tree JVM evidence for the injected parse path.
use anyhow::{Result, ensure};
use std::{path::Path, process::Command};
#[test]
fn complete_parse_manifest_matches_independent_jvm_bytes() -> Result<()> {
    if marathon_test_support::differential::launch_capture_child(
        "complete_parse_manifest_matches_independent_jvm_bytes",
    )? {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let raw = temp.path().join("raw");
    marathon_test_support::differential::capture_parse(&raw)?;
    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/differential/run");
    let output = Command::new("python3")
        .arg("-I")
        .arg(driver)
        .arg("verify-parse")
        .arg(raw)
        .output()?;
    ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Ok(())
}
