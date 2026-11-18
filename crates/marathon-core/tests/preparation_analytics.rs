//! U212 witnesses for the analytics boundary shared by Apple setup and preparation.

use jcompat::jtime::Instant;
use marathon_core::{
    analytics::{DeviceTimingLog, track_device_operation},
    error::{Cancellation, Failure, VendorError},
    executor::shared::{Signal, failure, requested},
    runtime::CancellationSignal,
};
use marathon_error::FailureKind;
use marathon_logging::{Level, LogSettings};
use std::{
    cell::Cell,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

fn failure_detail() -> Failure {
    failure(FailureKind::IllegalState, "failed")
}

#[tokio::test]
async fn preparation_instrumentation_failures_warn_once_and_keep_the_body_result_u212() {
    let warnings = Arc::new(AtomicUsize::new(0));
    let count = warnings.clone();
    let log = DeviceTimingLog::new(
        Arc::new(move |component, level, message, cause| {
            assert_eq!(component.target(), "marathon_core::analytics::event_pipeline");
            assert_eq!(level, Level::WARN);
            assert_eq!(message, "Could not record preparation timing for device sim@worker");
            assert!(cause.is_some());
            count.fetch_add(1, Ordering::SeqCst);
        }),
        &Mutex::new(LogSettings::default()),
    );
    enum Fault {
        Clock(usize),
        ConstructTracker,
        PollTracker,
        CancelTracker,
    }
    for (index, fault) in [
        Fault::Clock(0),
        Fault::Clock(1),
        Fault::ConstructTracker,
        Fault::PollTracker,
        Fault::CancelTracker,
    ]
    .into_iter()
    .enumerate()
    {
        let reads = Cell::new(0);
        let operations = Cell::new(0);
        let tracking = Cell::new(0);
        let result = track_device_operation(
            &Signal::default(),
            || async {
                let read = reads.get();
                reads.set(read + 1);
                assert!(!matches!(fault, Fault::Clock(at) if read == at), "clock panicked");
                Ok(Instant::EPOCH)
            },
            async {
                operations.set(operations.get() + 1);
                Ok(42)
            },
            |_, _| {
                tracking.set(tracking.get() + 1);
                assert!(
                    !matches!(fault, Fault::ConstructTracker),
                    "constructing the tracker future panicked"
                );
                async {
                    assert!(
                        !matches!(fault, Fault::PollTracker),
                        "polling the tracker future panicked"
                    );
                    Err(VendorError::Cancelled(Cancellation::Requested(failure_detail())))
                }
            },
            |error| log.warn("preparation", "sim@worker", error),
        )
        .await;
        assert_eq!(result, Ok(42));
        assert_eq!(operations.get(), 1);
        assert_eq!(reads.get(), if matches!(fault, Fault::Clock(0)) { 1 } else { 2 });
        assert_eq!(tracking.get(), u32::from(!matches!(fault, Fault::Clock(_))));
        assert_eq!(warnings.load(Ordering::SeqCst), index + 1);
    }

    let failed = VendorError::Other(failure_detail());
    let outcome = track_device_operation(
        &Signal::default(),
        || async { Err(VendorError::Other(Failure::from_kind(FailureKind::ReplayClock, None))) },
        async { Err::<(), _>(failed.clone()) },
        |_, _| async { panic!("a failed body has no preparation event") },
        |error| log.warn("preparation", "sim@worker", error),
    )
    .await;
    assert_eq!(outcome, Err(failed));
    assert_eq!(warnings.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn a_failed_body_with_healthy_instrumentation_keeps_its_error_u212() {
    let reads = Cell::new(0);
    let tracked = Cell::new(false);
    let warned = Cell::new(false);
    let failed = VendorError::Other(failure_detail());
    let outcome = track_device_operation(
        &Signal::default(),
        || async {
            reads.set(reads.get() + 1);
            Ok(Instant::EPOCH)
        },
        async { Err::<(), _>(failed.clone()) },
        |_, _| async {
            tracked.set(true);
            Ok(())
        },
        |_| warned.set(true),
    )
    .await;
    assert_eq!(outcome, Err(failed));
    assert_eq!(reads.get(), 1);
    assert!(!tracked.get());
    assert!(!warned.get());
}

#[tokio::test]
async fn preparation_propagates_the_callers_cancellation_u212() {
    let signal = Signal::new();
    let reason = requested("the caller stopped preparation");
    let tracking_entered = tokio::sync::Notify::new();
    let warned = Cell::new(false);
    let operation = track_device_operation(
        signal.as_ref(),
        || async { Ok(Instant::EPOCH) },
        async { Ok(()) },
        |_, _| async {
            tracking_entered.notify_one();
            signal.cancelled().await;
            Err(VendorError::Cancelled(Cancellation::Requested(failure_detail())))
        },
        |_| warned.set(true),
    );
    let cancel = async {
        tracking_entered.notified().await;
        signal.cancel(reason.clone());
    };
    let (outcome, _) = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::join!(operation, cancel)
    })
    .await
    .expect("caller cancellation must finish preparation");
    assert_eq!(outcome, Err(VendorError::Cancelled(reason)));
    assert!(!warned.get());
}
