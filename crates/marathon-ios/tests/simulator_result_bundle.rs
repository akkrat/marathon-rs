//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/listener/ResultBundleRunListenerTest.kt
mod simulator_support;
mod support;
use marathon_apple::{
    apple_device::AppleDevice,
    apple_device_test_runner::AppleDeviceTestRunner,
    listener::{
        CompositeTestRunListener, ResultBundleRunListener, TestResultsListener, TestResultsListenerConfig,
        TestRunListenerAdapter,
    },
    model::AppleTestBundle,
};
use marathon_config::vendor::PullingPolicy;
use marathon_core::{
    artifacts::{AttachmentProviderDelegate, FileLogWriter, LogListener},
    executor::shared::{SharedDeferred, Signal},
    listener::LogTarget,
    model::{DevicePoolId, TestBatch, TestBatchResults},
    runtime::Deferred,
    vendor::{Device, DeviceRef, VendorContext},
};
use simulator_support::{artifacts::*, *};
use std::sync::{Arc, atomic::Ordering};
#[tokio::test(start_paused = true)]
async fn closed_queue_does_not_fail_a_completed_runner_batch() -> TestResult {
    let h = Harness::new(|v| v.xcresult.pulling_policy = PullingPolicy::Always)?;
    let bridge = Arc::new(BundleBridge::default());
    let (d, _) = device(&h, bridge.clone()).await?;
    h.services
        .artifacts
        .close_and_join(1000, 1000, &h.services.context)
        .await
        .unwrap();
    let (r, results) = run(&h, &d).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(results.unwrap().finished.len(), 1);
    assert_eq!(bridge.pulls.load(Ordering::SeqCst), 0);
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn next_real_runner_batch_starts_during_blocked_transfer_while_finalization_awaits_cleanup() -> TestResult {
    let h = Harness::new(|v| {
        v.xcresult.pulling_policy = PullingPolicy::Always;
        v.xcresult.remote_clean = true;
    })?;
    let bridge = Arc::new(BundleBridge::default());
    let gate = SharedDeferred::new();
    *bridge.gate.lock().unwrap() = Some(gate.clone());
    let (d, s) = device(&h, bridge.clone()).await?;
    record_cleanup(&h, bridge.trace.clone());
    assert!(run(&h, &d).await.0.is_ok());
    bridge.entered.wait().await.unwrap();
    assert!(run(&h, &d).await.0.is_ok());
    assert_eq!(s.count.load(Ordering::SeqCst), 2);
    assert_eq!(bridge.pulls.load(Ordering::SeqCst), 1);
    assert!(!bridge.trace.lock().unwrap().iter().any(|s| s.starts_with("rm ")));
    let close = h.services.artifacts.close_and_join(1000, 1000, &h.services.context);
    let owner = "a@local";
    let barrier = h.services.artifacts.await_owner(owner, &h.services.context);
    tokio::pin!(close, barrier);
    assert!(futures_util::poll!(close.as_mut()).is_pending());
    assert!(futures_util::poll!(barrier.as_mut()).is_pending());
    gate.complete(Ok(()));
    let (a, b) = tokio::join!(close, barrier);
    assert!(a.unwrap().is_empty());
    b.unwrap();
    assert_eq!(bridge.pulls.load(Ordering::SeqCst), 2);
    assert_eq!(
        bridge
            .trace
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("rm -rf "))
            .count(),
        2
    );
    assert_eq!(result_folders(h.dir.path()).len(), 2);
    h.finish(&[d]).await;
    Ok(())
}
async fn fixed(
    h: &Harness,
    d: Arc<marathon_ios::apple_simulator_device::AppleSimulatorDevice>,
    queued: bool,
) -> Arc<TestBatchResults> {
    let batch = Arc::new(TestBatch {
        id: "matrix".into(),
        tests: tests(),
    });
    let deferred = SharedDeferred::new();
    let pool = DevicePoolId { name: "pool".into() };
    if queued {
        let result = d
            .execute(
                h.services.configuration.clone(),
                pool,
                batch,
                deferred.clone(),
                h.services.context.clone(),
            )
            .await;
        assert!(result.is_ok(), "{result:?}");
        h.services
            .artifacts
            .await_owner("a@local", &h.services.context)
            .await
            .unwrap();
    } else {
        let log = LogListener::new(
            h.services.runtime.clone(),
            d.clone(),
            Arc::new(FileLogWriter::new(
                h.services.runtime.clone(),
                h.services.output.clone(),
            )),
            LogTarget {
                pool: pool.clone(),
                batch_id: batch.id.clone(),
                device: d.info(),
                test: None,
            },
            Some("xcodebuild-log".into()),
        );
        let provider = Arc::new(AttachmentProviderDelegate::default());
        let recorder = Arc::new(Recorder {
            provider: provider.clone(),
            file: std::path::PathBuf::from(h.dir.path().join("test.log").to_str().unwrap()),
        });
        let results = TestResultsListener::new(TestResultsListenerConfig {
            batch: batch.clone(),
            device: DeviceRef(d.clone()),
            deferred: deferred.clone(),
            timer: h.services.timer.clone(),
            wall: h.services.result_wall.clone(),
            logs: h.services.logs.clone(),
            remote_xcresult: d.remote_file_manager().remote_xcresult_file(&batch).to_owned(),
            xcresulttool: Arc::new(d.binary_environment().xcrun.xcresulttool.clone()),
            xcode_version: d.xcode_version(),
            context: h.services.context.clone(),
            logging: h.services.logging.clone(),
            attachment_providers: vec![provider, log.clone()],
        })
        .unwrap();
        let listeners = CompositeTestRunListener::new(vec![
            Arc::new(results),
            Arc::new(TestRunListenerAdapter { listener: log }),
            recorder,
            Arc::new(ResultBundleRunListener::new(
                d.clone(),
                h.services.vendor.xcresult.clone(),
                pool,
                batch.clone(),
                h.services.files.clone(),
                None,
                h.services.context.clone(),
                h.services.logging.clone(),
            )),
        ]);
        let b = h.services.vendor.bundle.as_ref().unwrap();
        let bundle = AppleTestBundle::new(
            b.app.clone(),
            None,
            b.xctest.as_ref().unwrap().clone(),
            marathon_apple::model::Sdk::IphoneSimulator,
            h.services.runtime.clone(),
            h.services.logging.clone(),
        )
        .await
        .unwrap();
        let r = AppleDeviceTestRunner::new(d, h.services.runtime.clone(), h.services.logging.clone())
            .execute(
                &h.services.configuration,
                &bundle,
                &batch,
                &listeners,
                &h.services.context,
            )
            .await;
        if let Err(error) = r {
            assert_eq!(
                error.reason,
                Some(marathon_apple::logparser::parser::DeviceFailureReason::Unknown)
            );
        }
    }
    deferred.completed().unwrap().unwrap()
}
#[tokio::test(start_paused = true)]
async fn policies_results_traces_and_attachments_match_synchronous_baseline_for_every_outcome() -> TestResult {
    for policy in [PullingPolicy::Always, PullingPolicy::OnFailure, PullingPolicy::Never] {
        for outcome in ["passed", "failed", "incomplete"] {
            for clean in [false, true] {
                for delivered in [false, true] {
                    let mut h = Harness::new(|v| {
                        v.xcresult.pulling_policy = policy;
                        v.xcresult.remote_clean = clean;
                    })?;
                    h.services.listeners = Arc::new(Factory {
                        file: std::path::PathBuf::from(h.dir.path().join("test.log").to_str().unwrap()),
                        fail_finalizer: false,
                        cancel_finalizer: false,
                        finalizing: SharedDeferred::new(),
                    });
                    let bridge = Arc::new(BundleBridge::default());
                    bridge.mode.store(usize::from(!delivered), Ordering::SeqCst);
                    let (d, s) = device(&h, bridge.clone()).await?;
                    record_cleanup(&h, bridge.trace.clone());
                    s.output(match outcome {
                        "passed" => passed(1),
                        "failed" => {
                            let mut lines = failed(1);
                            lines.insert(1, "/tmp/Tests.swift:1: error: -[Suite test1] : assertion failed".into());
                            lines
                        }
                        _ => vec!["Test runner exited".into()],
                    });
                    let baseline = fixed(&h, d.clone(), false).await;
                    let baseline_trace = bridge.trace.lock().unwrap().clone();
                    for folder in result_folders(h.dir.path()) {
                        tokio::fs::remove_dir_all(folder).await?;
                    }
                    bridge.trace.lock().unwrap().clear();
                    let queued = fixed(&h, d.clone(), true).await;
                    assert_eq!(queued.finished, baseline.finished, "{policy:?} {outcome}");
                    assert_eq!(queued.failed, baseline.failed);
                    assert_eq!(queued.uncompleted, baseline.uncompleted);
                    if outcome == "failed" {
                        assert!(
                            queued.failed[0]
                                .stacktrace
                                .as_ref()
                                .unwrap()
                                .to_owned()
                                .contains("assertion failed")
                        );
                    }
                    assert_eq!(*bridge.trace.lock().unwrap(), baseline_trace);
                    let expected =
                        policy == PullingPolicy::Always || policy == PullingPolicy::OnFailure && outcome != "passed";
                    assert_eq!(bridge.pulls.load(Ordering::SeqCst), if expected { 2 } else { 0 });
                    assert_eq!(
                        h.services.artifacts.kept_sources("a@local").len(),
                        usize::from(expected && !delivered)
                    );
                    assert_eq!(result_folders(h.dir.path()).len(), usize::from(expected && delivered));
                    if outcome != "incomplete" {
                        let result = if outcome == "passed" {
                            &queued.finished[0]
                        } else {
                            &queued.failed[0]
                        };
                        assert!(
                            result
                                .attachments
                                .iter()
                                .any(|a| a.file.as_os_str() == h.dir.path().join("test.log").as_os_str())
                        );
                        for a in result.attachments.iter().filter(|a| a.name.is_some()) {
                            assert_eq!(a.name.as_ref().unwrap().to_owned(), "xcodebuild-log");
                        }
                    }
                    h.finish(&[d]).await;
                }
            }
        }
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn failed_delivery_does_not_fail_a_completed_runner_batch_synchronously_or_through_the_queue() -> TestResult {
    for queued in [false, true] {
        let h = Harness::new(|v| {
            v.xcresult.pulling_policy = PullingPolicy::Always;
            v.xcresult.remote_clean = true;
        })?;
        let bridge = Arc::new(BundleBridge::default());
        bridge.mode.store(2, Ordering::SeqCst);
        let (d, _) = device(&h, bridge.clone()).await?;
        let result = fixed(&h, d.clone(), queued).await;
        assert_eq!(result.finished.len(), 1);
        assert_eq!(bridge.pulls.load(Ordering::SeqCst), 1);
        assert!(!bridge.trace.lock().unwrap().iter().any(|s| s.starts_with("rm ")));
        if queued {
            assert_eq!(
                h.services
                    .artifacts
                    .close_and_join(1000, 1000, &h.services.context)
                    .await
                    .unwrap()
                    .len(),
                1
            );
        }
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn bundle_skipped_by_a_failing_earlier_finalizer_stays_protected_and_is_reported_at_finalization() -> TestResult {
    for cancelled in [false, true] {
        let mut h = Harness::new(|v| v.xcresult.pulling_policy = PullingPolicy::Always)?;
        let finalizing = SharedDeferred::new();
        h.services.listeners = Arc::new(Factory {
            file: std::path::PathBuf::from("test.log"),
            fail_finalizer: true,
            cancel_finalizer: cancelled,
            finalizing: finalizing.clone(),
        });
        let bridge = Arc::new(BundleBridge::default());
        let (d, _) = device(&h, bridge.clone()).await?;
        let signal = Signal::new();
        let batch = spawn_batch(
            &h,
            d.clone(),
            VendorContext {
                cancellation: signal.clone(),
                ..h.services.context.clone()
            },
        );
        finalizing.wait().await.unwrap();
        if cancelled {
            signal.cancel(marathon_apple::cmd::cancelled("caller"));
        }
        assert!(batch.await?.0.is_err());
        let kept = h.services.artifacts.kept_sources("a@local");
        assert_eq!(kept.len(), 1);
        d.remote_file_manager()
            .remove_remote_directory(
                &kept.iter().map(|s| s.to_owned()).collect::<Vec<_>>(),
                &h.services.context,
            )
            .await
            .unwrap();
        let diagnostics = h
            .services
            .artifacts
            .close_and_join(1000, 1000, &h.services.context)
            .await
            .unwrap();
        assert_eq!(diagnostics.len(), 1);
        let text = diagnostics[0].to_owned();
        for detail in [
            "never handed to the artifact queue",
            "worker local",
            "a@local",
            "local copy: none",
        ] {
            assert!(text.contains(detail), "{text}");
        }
        assert_eq!(bridge.pulls.load(Ordering::SeqCst), 0);
        h.finish(&[d]).await;
    }
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn bundles_registered_around_queue_shutdown_are_kept_and_reported_once() -> TestResult {
    let mut h = Harness::new(|v| v.xcresult.pulling_policy = PullingPolicy::Always)?;
    let finalizing = SharedDeferred::new();
    h.services.listeners = Arc::new(Factory {
        file: std::path::PathBuf::from("test.log"),
        fail_finalizer: true,
        cancel_finalizer: true,
        finalizing: finalizing.clone(),
    });
    let bridge = Arc::new(BundleBridge::default());
    let (d, _) = device(&h, bridge.clone()).await?;
    let signal = Signal::new();
    let first = spawn_batch(
        &h,
        d.clone(),
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    finalizing.wait().await.unwrap();
    assert_eq!(
        h.services
            .artifacts
            .close_and_join(1000, 1000, &h.services.context)
            .await
            .unwrap()
            .len(),
        1
    );
    signal.cancel(marathon_apple::cmd::cancelled("caller"));
    assert!(first.await?.0.is_err());
    let signal = Signal::new();
    signal.cancel(marathon_apple::cmd::cancelled("later"));
    // A fresh factory finalizer fails after a batch has registered with the already-closed queue.
    let mut services = h.services.clone();
    services.listeners = Arc::new(Factory {
        file: std::path::PathBuf::from("test.log"),
        fail_finalizer: true,
        cancel_finalizer: false,
        finalizing: SharedDeferred::new(),
    });
    let old = std::mem::replace(&mut h.services, services);
    let (late, _) = device(&h, bridge.clone()).await?;
    assert!(run(&h, &late).await.0.is_err());
    let reports = h
        .services
        .artifacts
        .close_and_join(1000, 1000, &h.services.context)
        .await
        .unwrap();
    assert_eq!(reports.len(), 2);
    assert!(reports[1].to_owned().contains("artifact queue is closed"));
    assert_eq!(bridge.pulls.load(Ordering::SeqCst), 0);
    drop(old);
    h.finish(&[d, late]).await;
    Ok(())
}
/// D54 (review round 1): a receive does not take its caller's cancellation. The artifact queue is cancelled 300 s after the
/// run and then given 60 s, so a healthy pull that ends at 330 s is delivered, as in Kotlin, though the bridge stops a
/// receive on its stop.
#[tokio::test(start_paused = true)]
async fn review_healthy_artifact_pull_keeps_existing_shutdown_grace() -> TestResult {
    let h = Harness::new(|v| v.xcresult.pulling_policy = PullingPolicy::Always)?;
    let bridge = Arc::new(BundleBridge::default());
    bridge.pull_millis.store(330_000, Ordering::SeqCst);
    let (d, _) = device(&h, bridge.clone()).await?;
    assert!(run(&h, &d).await.0.is_ok());
    bridge.entered.wait().await.unwrap();
    let started = tokio::time::Instant::now();
    let undelivered = h
        .services
        .artifacts
        .close_and_join(300_000, 60_000, &h.services.context)
        .await
        .unwrap();
    assert!(undelivered.is_empty(), "{undelivered:?}");
    assert!(started.elapsed() >= std::time::Duration::from_secs(300));
    assert_eq!(bridge.pulls.load(Ordering::SeqCst), 1);
    assert_eq!(result_folders(h.dir.path()).len(), 1);
    h.finish(&[d]).await;
    Ok(())
}
