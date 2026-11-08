//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/SharedTransfersTest.kt
mod support;
use marathon_apple::cmd::{failure, shared_transfers::SharedTransfers};
use marathon_core::error::{LeavePoolReason, VendorError};
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
        marathon_apple::cmd::TransferStop::never(),
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
                marathon_apple::cmd::TransferStop::never(),
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
            marathon_apple::cmd::TransferStop::never(),
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
            marathon_apple::cmd::TransferStop::never(),
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
            marathon_apple::cmd::TransferStop::never(),
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
fn stalled() -> VendorError {
    VendorError::LeavePool {
        reason: LeavePoolReason::TransferStalled,
        detail: failure("TransferStalled", "no progress"),
    }
}
/// The first send, held until the test has the others waiting, ends in `first`. Returns what each of the two waiters got and
/// how many transfers ran.
async fn behind(
    first: VendorError,
    other_destination: &'static str,
) -> (Result<bool, VendorError>, Result<bool, VendorError>, usize) {
    let t = Arc::new(transfers());
    let count = Arc::new(AtomicUsize::new(0));
    let (entered, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release, release_rx) = tokio::sync::oneshot::channel::<()>();
    let head = {
        let (t, count) = (t.clone(), count.clone());
        tokio::spawn(async move {
            t.send(
                "worker-1:22",
                Path::new(SRC),
                DST,
                marathon_apple::cmd::TransferStop::never(),
                || async { Ok(Some(true)) },
                || async {
                    count.fetch_add(1, Ordering::SeqCst);
                    entered.send(()).unwrap();
                    release_rx.await.unwrap();
                    Err(first)
                },
            )
            .await
        })
    };
    entered_rx.await.unwrap();
    let waiter = |dst: &'static str| {
        let (t, count) = (t.clone(), count.clone());
        tokio::spawn(async move {
            t.send(
                "worker-1:22",
                Path::new(SRC),
                dst,
                marathon_apple::cmd::TransferStop::never(),
                || async { Ok(Some(true)) },
                || async {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(true)
                },
            )
            .await
        })
    };
    let same = waiter(DST);
    let other = waiter(other_destination);
    // Single-threaded runtime: once this yields, both have run up to the lock or through their transfer.
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    release.send(()).unwrap();
    assert!(head.await.unwrap().is_err());
    (same.await.unwrap(), other.await.unwrap(), count.load(Ordering::SeqCst))
}
#[tokio::test]
async fn a_stall_fails_who_waited_for_that_destination_and_nobody_else_u163() {
    let (same, other, transfers) = behind(stalled(), "/tmp/marathon/shared/other.app").await;
    assert_eq!(same, Err(stalled()));
    assert_eq!(other, Ok(true));
    assert_eq!(transfers, 2);
}
#[tokio::test]
async fn any_other_failure_lets_the_next_caller_try_u163() {
    let error = VendorError::Other(failure("java.io.IOException", "broken"));
    let (same, other, transfers) = behind(error, "/tmp/marathon/shared/other.app").await;
    assert_eq!((same, other), (Ok(true), Ok(true)));
    assert_eq!(transfers, 3);
}
#[tokio::test]
async fn a_send_that_starts_after_a_stall_tries_again_u163() {
    let t = transfers();
    let count = AtomicUsize::new(0);
    let failed = t
        .send(
            "worker-1:22",
            Path::new(SRC),
            DST,
            marathon_apple::cmd::TransferStop::never(),
            || async { Ok(Some(true)) },
            || async { Err::<bool, _>(stalled()) },
        )
        .await;
    assert_eq!(failed, Err(stalled()));
    assert!(push(&t, "worker-1:22", SRC, DST, Some(true), true, &count).await);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn a_cancelled_or_lost_send_lets_who_waited_behind_it_send_d54() {
    // Unlike a stall, a cancellation says nothing about the worker, and a lost connection is that device's alone.
    for error in [
        VendorError::Cancelled(marathon_apple::cmd::cancelled("the caller was cancelled")),
        VendorError::Other(failure(
            marathon_apple::cmd::remote::ssh::sshj::error::CONNECTION_EXCEPTION,
            "Lost the connection to worker-1:22 during a transfer",
        )),
    ] {
        let (same, other, transfers) = behind(error, "/tmp/marathon/shared/other.app").await;
        assert_eq!((same, other), (Ok(true), Ok(true)));
        assert_eq!(transfers, 3);
    }
}
/// D54 (review round 1): a caller waiting behind another simulator's transfer to the same destination observes its own stop,
/// here its 180 s scope while the transfer ahead runs to 240 s, or the loss of its own connection. The transfer ahead is left
/// alone and records nothing, so a caller that waited alongside goes on after it.
#[tokio::test(start_paused = true)]
async fn a_shared_send_waiter_observes_its_own_timeout_d54() {
    use marathon_apple::cmd::{TransferStop, remote::ssh::sshj::error::CONNECTION_EXCEPTION};
    use marathon_core::error::Cancellation;
    use std::time::Duration;
    let (logging, events) = support::ssh::logging(false);
    let t = Arc::new(SharedTransfers::new(Arc::new(Runtime), logging));
    let started = tokio::time::Instant::now();
    let (entered, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let send = |stop: TransferStop, runs: Option<tokio::sync::oneshot::Sender<()>>| {
        let t = t.clone();
        tokio::spawn(async move {
            t.send(
                "worker-1:22",
                Path::new(SRC),
                DST,
                stop,
                || async { Ok(Some(true)) },
                || async move {
                    let Some(entered) = runs else {
                        panic!("a send behind the first one reuses it");
                    };
                    entered.send(()).unwrap();
                    tokio::time::sleep(Duration::from_secs(240)).await;
                    Ok(true)
                },
            )
            .await
        })
    };
    let holder = send(TransferStop::never(), Some(entered));
    entered_rx.await.unwrap();
    let scope = marathon_core::executor::shared::Signal::new();
    let expired = Cancellation::Timeout {
        scope: 7,
        detail: failure(
            "kotlinx.coroutines.TimeoutCancellationException",
            "Timed out waiting for 180000 ms",
        ),
    };
    {
        let (scope, expired) = (scope.clone(), expired.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(180)).await;
            scope.cancel(expired);
        });
    }
    let loss = failure(
        CONNECTION_EXCEPTION,
        "Lost the connection to worker-1:22 during a transfer",
    );
    let lost = {
        let loss = loss.clone();
        send(
            TransferStop::never().or_lost(async move {
                tokio::time::sleep(Duration::from_secs(100)).await;
                loss
            }),
            None,
        )
    };
    let alongside = send(TransferStop::never(), None);
    let timed_out = send(TransferStop::new(scope), None);
    assert_eq!(lost.await.unwrap(), Err(VendorError::Other(loss)));
    assert_eq!(started.elapsed(), Duration::from_secs(100));
    assert_eq!(timed_out.await.unwrap(), Err(VendorError::Cancelled(expired)));
    assert_eq!(started.elapsed(), Duration::from_secs(180));
    assert!(!holder.is_finished());
    assert_eq!(holder.await.unwrap(), Ok(true));
    assert_eq!(started.elapsed(), Duration::from_secs(240));
    assert_eq!(alongside.await.unwrap(), Ok(true));
    let errors: Vec<_> = support::ssh::lines(&events)
        .into_iter()
        .filter(|(_, level, _)| *level == jcompat::logging::Level::Error)
        .map(|(.., message)| message)
        .collect();
    assert_eq!(
        errors,
        [format!(
            "Not sending {SRC} to worker-1:22:{DST}: the device's connection was lost while this one waited"
        )]
    );
}
