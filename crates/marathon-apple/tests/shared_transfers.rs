//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/SharedTransfersTest.kt
mod support;
use marathon_apple::cmd::{failure, shared_transfers::SharedTransfers};
use marathon_core::error::VendorError;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use support::{Runtime, ssh};
fn transfers() -> SharedTransfers {
    SharedTransfers::new(Arc::new(Runtime), ssh::logging(true).0)
}
const SRC: &str = "/tmp/derived-data/app.app";
const DST: &str = "/tmp/marathon/shared/appUnderTest.app";
async fn push(
    t: &SharedTransfers,
    worker: &str,
    src: &str,
    dst: &str,
    exists: Option<bool>,
    success: bool,
    count: &AtomicUsize,
) -> bool {
    t.send(
        worker,
        Path::new(src),
        dst,
        || async { Ok(exists) },
        || async {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(success)
        },
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn concurrent_pushes_to_the_same_worker_transfer_once() {
    let t = Arc::new(transfers());
    let count = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let (t, count, entered, release) = (t.clone(), count.clone(), entered.clone(), release.clone());
        tasks.push(tokio::spawn(async move {
            t.send(
                "worker-1:22",
                Path::new(SRC),
                DST,
                || async { Ok(Some(true)) },
                || async {
                    count.fetch_add(1, Ordering::SeqCst);
                    entered.add_permits(1);
                    release.acquire().await.unwrap().forget();
                    Ok(true)
                },
            )
            .await
            .unwrap()
        }));
    }
    entered.acquire().await.unwrap().forget();
    release.add_permits(1);
    for task in tasks {
        assert!(task.await.unwrap());
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn each_worker_gets_its_own_transfer() {
    let t = transfers();
    let n = AtomicUsize::new(0);
    for w in ["worker-1:22", "worker-2:22", "worker-1:22", "worker-2:22"] {
        assert!(push(&t, w, SRC, DST, Some(true), true, &n).await);
    }
    assert_eq!(n.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn different_destinations_are_transferred_separately() {
    let t = transfers();
    let n = AtomicUsize::new(0);
    for dst in [DST, "/tmp/marathon/shared/other.app"] {
        assert!(push(&t, "worker-1:22", SRC, dst, Some(true), true, &n).await);
    }
    assert_eq!(n.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn failed_transfer_is_retried_by_the_next_caller() {
    let t = transfers();
    let n = AtomicUsize::new(0);
    for success in [false, true, true] {
        assert_eq!(
            push(&t, "worker-1:22", SRC, DST, Some(true), success, &n).await,
            success
        );
    }
    assert_eq!(n.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn transfer_is_repeated_when_the_destination_is_gone() {
    let t = transfers();
    let n = AtomicUsize::new(0);
    for exists in [Some(true), Some(false)] {
        assert!(push(&t, "worker-1:22", SRC, DST, exists, true, &n).await);
    }
    assert_eq!(n.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn failed_existence_check_reuses_the_previous_transfer() {
    let t = transfers();
    let n = AtomicUsize::new(0);
    for exists in [Some(true), None] {
        assert!(push(&t, "worker-1:22", SRC, DST, exists, true, &n).await);
    }
    assert_eq!(n.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn another_source_for_the_same_destination_is_transferred_again() {
    let t = transfers();
    let n = AtomicUsize::new(0);
    for src in [
        "/one/Helper.app",
        "/two/Helper.app",
        "/one/Helper.app",
        "/two/Helper.app",
    ] {
        assert!(push(&t, "worker-1:22", src, DST, Some(true), true, &n).await);
    }
    assert_eq!(n.load(Ordering::SeqCst), 4);
}
#[tokio::test]
async fn existence_is_not_checked_before_the_first_transfer() {
    let t = transfers();
    assert!(
        t.send(
            "worker-1:22",
            Path::new(SRC),
            DST,
            || async { panic!("first exists") },
            || async { Ok(true) }
        )
        .await
        .unwrap()
    );
}
#[tokio::test]
async fn callback_errors_clear_only_a_new_transfer_and_release_the_destination() {
    let t = transfers();
    let n = AtomicUsize::new(0);
    let error = VendorError::Other(failure("java.io.IOException", "test"));
    assert_eq!(
        t.send(
            "worker",
            Path::new(SRC),
            DST,
            || async { Ok(Some(true)) },
            || async { Err(error.clone()) }
        )
        .await,
        Err(error.clone())
    );
    assert!(push(&t, "worker", SRC, DST, Some(true), true, &n).await);
    assert_eq!(
        t.send(
            "worker",
            Path::new(SRC),
            DST,
            || async { Err(error.clone()) },
            || async { panic!("transfer after exists error") }
        )
        .await,
        Err(error)
    );
    assert!(push(&t, "worker", SRC, DST, Some(true), true, &n).await);
    assert_eq!(n.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn source_path_reuse_ignores_changed_bytes_but_retains_dot_dot() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source");
    std::fs::write(&src, b"first").unwrap();
    let t = transfers();
    let n = AtomicUsize::new(0);
    assert!(push(&t, "worker", src.to_str().unwrap(), DST, Some(true), true, &n).await);
    std::fs::write(&src, b"changed").unwrap();
    assert!(push(&t, "worker", src.to_str().unwrap(), DST, Some(true), true, &n).await);
    let other = dir.path().join("sub/../source");
    assert!(push(&t, "worker", other.to_str().unwrap(), DST, Some(true), true, &n).await);
    assert_eq!(n.load(Ordering::SeqCst), 2);
}
