//! Paired actual-local command/file contracts; this is not an injected Apple world.
mod support;
use anyhow::{Result, ensure};
use marathon_apple::cmd::{
    CommandExecutor, CommandRequest, FileBridge,
    local::{JvmFileBridge, KotlinProcessCommandExecutor},
};
use marathon_test_support::differential::{copy_tree, launch_capture_child};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[tokio::test(flavor = "multi_thread")]
async fn capture_actual_local_contract() -> Result<()> {
    if launch_capture_child("capture_actual_local_contract")? {
        return Ok(());
    }
    let root = tempfile::tempdir()?;
    let root = root.path().canonicalize()?;
    let tree = root.join("tree");
    fs::create_dir(&tree)?;
    fs::create_dir(tree.join("empty"))?;
    fs::write(tree.join("source"), [0, 1, 255, b'\n'])?;
    let runtime = Arc::new(support::Runtime);
    let executor = KotlinProcessCommandExecutor::new(runtime.clone(), Arc::new(Mutex::default()), |_, _| {}, false, 64);
    let args = [
        "/bin/sh",
        "-c",
        "printf '%s\\n' \"$MRS_VALUE\" \"$PWD\" \"$@\"; printf 'stderr\\n' >&2; exit 7",
        "probe",
        "\"'x'\"",
        "'",
        "\"",
        "''",
        "x'",
        "",
    ];
    let mut request = CommandRequest::new(args, support::duration(5000));
    request.env.insert(
        "MRS_VALUE".into(),
        "literal 12345678-1234-1234-1234-123456789abc".into(),
    );
    request.workdir = Some(root.to_string_lossy().into_owned());
    let session = executor
        .execute(request, support::context())
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let result = session.await_result(&support::context()).await;
    session.close().await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let result = result.map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let bridge = JvmFileBridge::new(runtime, true);
    let sent = bridge
        .send(&tree.join("source"), &tree.join("copy").to_string_lossy())
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    fs::create_dir(tree.join("receive"))?;
    let received = bridge
        .receive(&tree.join("source").to_string_lossy(), &tree.join("receive"))
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let missing = bridge
        .receive(&tree.join("absent").to_string_lossy(), &tree.join("missing-copy"))
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let observation = json!({"argv": args, "stdout": result.stdout, "stderr": result.stderr,
        "exit": result.exit_code, "send": sent, "receive": received, "missing": missing, "connected": executor.connected()});
    ensure!(
        result.exit_code == Some(7) && sent && received && !missing,
        "local probe failed"
    );
    if let Some(destination) = std::env::var_os("MRS_DIFFERENTIAL_OUT") {
        let destination = PathBuf::from(destination);
        fs::create_dir(&destination)?;
        copy_tree(&tree, &destination.join("tree"))?;
        fs::write(destination.join("observation.json"), serde_json::to_vec(&observation)?)?;
        fs::write(
            destination.join("identities.json"),
            serde_json::to_vec(&json!({"sandbox": root.to_string_lossy()}))?,
        )?;
    } else {
        // The ordinary workspace test also compares a fresh capture to the JVM golden.
        let destination = root.join("native");
        fs::create_dir(&destination)?;
        copy_tree(&tree, &destination.join("tree"))?;
        fs::write(destination.join("observation.json"), serde_json::to_vec(&observation)?)?;
        fs::write(
            destination.join("identities.json"),
            serde_json::to_vec(&json!({"sandbox": root.to_string_lossy()}))?,
        )?;
        let driver = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/differential/run");
        let output = std::process::Command::new("python3")
            .arg("-I")
            .arg(driver)
            .arg("verify-local")
            .arg(&destination)
            .output()?;
        ensure!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}
