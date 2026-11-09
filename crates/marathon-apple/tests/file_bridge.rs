//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/local/JvmFileBridgeTest.kt
mod support;
use marathon_apple::cmd::{FileBridge, local::JvmFileBridge};
use marathon_core::error::VendorError;
use std::sync::Arc;
use support::Runtime;
#[tokio::test]
async fn receiving_a_missing_file_fails_without_throwing() {
    let root = tempfile::tempdir().unwrap();
    let bridge = JvmFileBridge::new(Arc::new(Runtime), true);
    assert!(
        !bridge
            .receive(
                root.path().join("missing").to_str().unwrap(),
                &root.path().join("destination")
            )
            .await
            .unwrap()
    );
    assert!(!root.path().join("destination").exists());
}
// Ours: the Kotlin suite has only the missing-source receive test.
#[tokio::test]
async fn send_receive_overwrite_and_trailing_slash_follow_kotlin_stdlib() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("source");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("file"), "original").unwrap();
    let dst = root.path().join("destination");
    let bridge = JvmFileBridge::new(Arc::new(Runtime), true);
    assert!(bridge.send(&src, dst.to_str().unwrap()).await.unwrap());
    assert_eq!(std::fs::read_to_string(dst.join("file")).unwrap(), "original");
    std::fs::write(src.join("file"), "changed").unwrap();
    assert!(bridge.send(&src, dst.to_str().unwrap()).await.unwrap());
    assert_eq!(std::fs::read_to_string(dst.join("file")).unwrap(), "changed");
    let receive = root.path().join("receive");
    std::fs::create_dir(&receive).unwrap();
    assert!(
        bridge
            .receive(src.join("file").to_str().unwrap(), &receive)
            .await
            .unwrap()
    );
    assert_eq!(std::fs::read_to_string(receive.join("file")).unwrap(), "changed");
    assert!(bridge.receive(&format!("{}/", src.display()), &receive).await.unwrap());
    assert_eq!(std::fs::read_to_string(receive.join("file")).unwrap(), "changed");
    let conflict = root.path().join("conflict");
    std::fs::write(&conflict, "old").unwrap();
    assert!(bridge.send(&src, conflict.to_str().unwrap()).await.unwrap());
    assert!(conflict.is_dir());
    assert!(
        bridge
            .send(&src.join("file"), conflict.to_str().unwrap())
            .await
            .unwrap()
    );
    assert_eq!(std::fs::read_to_string(conflict).unwrap(), "changed");
}
#[tokio::test]
async fn default_copy_errors_propagate_instead_of_becoming_false() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("source");
    let dst = root.path().join("destination");
    std::fs::write(&src, "new").unwrap();
    std::fs::write(&dst, "old").unwrap();
    let bridge = JvmFileBridge::new(Arc::new(Runtime), false);
    assert!(
        matches!(bridge.send(&src, dst.to_str().unwrap()).await, Err(VendorError::Other(f)) if f.kind.legacy_class() == "kotlin.io.FileAlreadyExistsException")
    );
    assert!(
        matches!(bridge.send(&root.path().join("missing"), dst.to_str().unwrap()).await, Err(VendorError::Other(f)) if f.kind.legacy_class() == "kotlin.io.NoSuchFileException")
    );
    assert!(bridge.send(&src, src.join("child").to_str().unwrap()).await.is_err());
}
#[tokio::test]
async fn copy_uses_new_file_permissions_instead_of_preserving_executable_bits() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("executable");
    let dst = root.path().join("copy");
    std::fs::write(&src, "content").unwrap();
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bridge = JvmFileBridge::new(Arc::new(Runtime), true);
    assert!(bridge.send(&src, dst.to_str().unwrap()).await.unwrap());
    assert_eq!(std::fs::metadata(dst).unwrap().permissions().mode() & 0o111, 0);
}
