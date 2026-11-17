//! Test-end artifact work that runs beside the batch's later tests (D80); Rust-only support for the port of
//! vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt.
//! Runs on the production executor's vendor runtime, which reports a panicking task as the task's failure.
use marathon_core::{
    error::{OperationResult, VendorError},
    executor::{CoreExecutor, shared::Signal},
    runtime::CancellationSignal,
};
use marathon_ios::listener::pending::PendingArtifacts;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Submits `panics_at` (0-based) as a panicking item among `count`, settles and returns the ledger and whether the
/// last item ran.
fn run(count: usize, panics_at: usize) -> OperationResult<(Result<(), VendorError>, bool)> {
    CoreExecutor::run(move |core| {
        let runtime = core.vendor();
        Box::pin(async move {
            let pending = PendingArtifacts::default();
            let batch: Arc<dyn CancellationSignal> = Signal::new();
            let ran = Arc::new(AtomicBool::new(false));
            for n in 0..count {
                let ran = ran.clone();
                pending.submit(
                    runtime.as_ref(),
                    batch.clone(),
                    Box::pin(async move {
                        if n == panics_at {
                            panic!("artifact work panicked");
                        }
                        ran.store(n + 1 == count, Ordering::SeqCst);
                        Ok(())
                    }),
                );
            }
            pending.settle().await;
            Ok((pending.take_error(), ran.load(Ordering::SeqCst)))
        })
    })
}

#[test]
fn a_panicking_item_before_another_fails_the_settled_batch_d80() {
    let (ledger, last_ran) = run(2, 0).unwrap();
    let error = ledger.unwrap_err();
    assert!(format!("{error:?}").contains("artifact work panicked"), "{error:?}");
    assert!(last_ran, "the queue goes on after a panicking item");
}

#[test]
fn a_panicking_last_item_fails_the_settled_batch_d80() {
    let (ledger, _) = run(2, 1).unwrap();
    let error = ledger.unwrap_err();
    assert!(format!("{error:?}").contains("artifact work panicked"), "{error:?}");
}

#[test]
fn items_that_succeed_leave_the_ledger_empty() {
    let (ledger, last_ran) = run(2, usize::MAX).unwrap();
    assert!(ledger.is_ok());
    assert!(last_ran);
}
