//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
use marathon_error::{FailureKind, InjectedKind};
mod simulator_support;
mod support;
use marathon_apple::cmd::{
    failure,
    remote::ssh::sshj::error::{
        CONNECTION_EXCEPTION, ILLEGAL_STATE_EXCEPTION, OPEN_FAIL_EXCEPTION, TRANSPORT_EXCEPTION,
    },
};
use marathon_config::vendor::{GrantLifecycle, IosPermission};
use marathon_core::{
    error::VendorError,
    executor::shared::SharedDeferred,
    model::{DevicePoolId, TestBatch},
    runtime::Deferred,
    vendor::Device,
};
use simulator_support::*;
use std::sync::Arc;
#[tokio::test(start_paused = true)]
async fn transport_and_illegal_state_failures_lose_the_device_but_other_io_errors_do_not() -> TestResult {
    for class in [
        CONNECTION_EXCEPTION,
        TRANSPORT_EXCEPTION,
        OPEN_FAIL_EXCEPTION,
        ILLEGAL_STATE_EXCEPTION,
        FailureKind::Io,
    ] {
        let h = Harness::new(|v| {
            v.permissions.lifecycle = GrantLifecycle::BeforeEachBatch;
            v.permissions.grant = vec![IosPermission::Calendar];
        })?;
        let (d, _) = prepared(&h).await?;
        let boot = h.boot.clone();
        let expected = class.clone();
        *h.worker.before.lock().unwrap() = Arc::new(move |command| {
            if command.get(2).is_some_and(|s| s == "privacy") {
                let class = class.clone();
                return Box::pin(async move { Err(VendorError::Other(failure(class.clone(), "probe"))) });
            }
            let reply = boot.answer(&command);
            Box::pin(async move { Ok(reply) })
        });
        let (r, results) = run(&h, &d).await;
        let e = r.unwrap_err();
        assert_eq!(d.lost(), expected != FailureKind::Io);
        assert!(!d.retired());
        assert!(results.is_none());
        if expected != FailureKind::Io {
            assert!(matches!(e, VendorError::DeviceLost(_)));
            assert_eq!(e.failure().cause.as_ref().unwrap().kind, expected);
        } else {
            assert_eq!(e.failure().kind, expected);
        }
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn kept_batch_failure_without_completed_results_returns_batch_execution() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    s.output(vec!["Test runner exited".into()]);
    let boot = h.boot.clone();

    *h.worker.before.lock().unwrap() = Arc::new(move |command| {
        if command.get(1).is_some_and(|s| s == "xcresulttool") {
            return Box::pin(async { Err(VendorError::Other(failure(FailureKind::Io, "cannot enrich"))) });
        }
        let reply = boot.answer(&command);
        Box::pin(async move { Ok(reply) })
    });
    let (r, results) = run(&h, &d).await;
    assert!(matches!(r, Err(VendorError::BatchExecution(_))), "{r:?}");
    assert!(results.is_none());
    assert!(!d.lost());
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn exceptionally_completed_result_is_no_progress_even_when_the_runner_passed_tests() -> TestResult {
    let h = Harness::new(|v| v.max_batches_without_progress = 1)?;
    let (d, s) = prepared(&h).await?;
    s.output(passed(1));
    let result = SharedDeferred::new();
    result.complete(Err(VendorError::Other(failure(
        FailureKind::Injected(InjectedKind::Probe),
        "results already failed",
    ))));
    let r = d
        .execute(
            h.services.configuration.clone(),
            DevicePoolId { name: "pool".into() },
            Arc::new(TestBatch {
                id: "exceptional-result".into(),
                tests: tests(),
            }),
            result,
            h.services.context.clone(),
        )
        .await;
    assert!(matches!(r, Err(VendorError::DeviceLost(_))));
    assert!(d.lost());
    h.finish(&[d]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn idle_gap_clock_is_read_only_when_the_debug_message_is_enabled() -> TestResult {
    use marathon_core::time::{ClockResult, MonotonicClock};
    use std::sync::Mutex;
    struct Reads {
        clock: Arc<support::Clock>,
        labels: Mutex<Vec<&'static str>>,
    }
    impl MonotonicClock for Reads {
        fn nanos(&self, label: &'static str) -> ClockResult<i64> {
            self.labels.lock().unwrap().push(label);
            self.clock.nanos(label)
        }
    }
    let mut h = Harness::new(|_| {})?;
    let reads = Arc::new(Reads {
        clock: h.clock.clone(),
        labels: Mutex::default(),
    });
    h.services.monotonic = reads.clone();
    let (d, _) = prepared(&h).await?;
    reads.labels.lock().unwrap().clear();
    for _ in 0..2 {
        assert!(run(&h, &d).await.0.is_ok());
    }
    assert_eq!(reads.labels.lock().unwrap().len(), 2);
    {
        let mut registry = h.services.logging.registry.lock().unwrap();
        registry.debug = true;
        registry.logger(d.logger().name(), None);
    }
    assert!(run(&h, &d).await.0.is_ok());
    assert_eq!(reads.labels.lock().unwrap().len(), 4);
    h.finish(&[d]).await;
    Ok(())
}
