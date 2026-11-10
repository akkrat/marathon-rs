//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDeviceSessionTest.kt
mod simulator_support;
mod support;
use marathon_apple::{
    apple_device::AppleDevice, cmd::cancelled, test::TestRequest, xcodebuild_session::XcodebuildSession,
};
use marathon_core::{
    error::VendorError,
    executor::shared::{SharedDeferred, Signal},
    vendor::{Device, VendorContext},
};
use simulator_support::*;
use std::sync::atomic::Ordering;
#[tokio::test(start_paused = true)]
async fn a_kept_simulator_starts_the_next_batch_only_after_the_xcodebuild_session_of_the_previous_one_has_closed()
-> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    let gate = SharedDeferred::new();
    *s.plan.lock().unwrap() = OutputPlan {
        lines: vec!["Test runner exited before starting test execution".into()],
        close_gate: Some(gate.clone()),
        ..Default::default()
    };
    let first = spawn_batch(&h, d.clone(), h.services.context.clone());
    s.closing.wait().await.unwrap();
    assert!(!first.is_finished());
    assert_eq!(s.count.load(Ordering::SeqCst), 1);
    gate.complete(Ok(()));
    assert!(first.await?.0.is_ok());
    s.plan.lock().unwrap().close_gate = None;
    assert!(run(&h, &d).await.0.is_ok());
    assert_eq!(
        *s.records.lock().unwrap(),
        [
            "xcodebuild 1 started",
            "xcodebuild 1 closed",
            "xcodebuild 2 started",
            "xcodebuild 2 closed"
        ]
    );
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_simulator_whose_xcodebuild_session_has_not_closed_in_time_is_lost() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    let gate = SharedDeferred::new();
    s.plan.lock().unwrap().close_gate = Some(gate.clone());
    assert!(matches!(run(&h, &d).await.0, Err(VendorError::DeviceLost(_))));
    assert_eq!(*s.records.lock().unwrap(), ["xcodebuild 1 started"]);
    gate.complete(Ok(()));
    assert!(
        d.await_session_closed(support::duration(1000), &h.services.context)
            .await
            .unwrap()
    );
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_batch_cancelled_while_its_xcodebuild_session_is_open_ends_only_after_the_session_has_closed() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    let gate = SharedDeferred::new();
    *s.plan.lock().unwrap() = OutputPlan {
        hang: true,
        close_gate: Some(gate.clone()),
        ..Default::default()
    };
    let signal = Signal::new();
    let batch = spawn_batch(
        &h,
        d.clone(),
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    s.opened.wait().await.unwrap();
    signal.cancel(cancelled("caller"));
    s.closing.wait().await.unwrap();
    assert!(!batch.is_finished());
    gate.complete(Ok(()));
    assert!(matches!(batch.await?.0, Err(VendorError::Cancelled(_))));
    assert_eq!(
        *s.records.lock().unwrap(),
        ["xcodebuild 1 started", "xcodebuild 1 closed"]
    );
    assert!(!d.lost());
    assert!(!d.retired());
    h.finish(&[d]).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn disposal_during_the_cancellation_of_a_batch_returns_only_after_its_xcodebuild_session_has_closed() -> TestResult
{
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    let gate = SharedDeferred::new();
    *s.plan.lock().unwrap() = OutputPlan {
        hang: true,
        close_gate: Some(gate.clone()),
        ..Default::default()
    };
    let signal = Signal::new();
    let batch = spawn_batch(
        &h,
        d.clone(),
        VendorContext {
            cancellation: signal.clone(),
            ..h.services.context.clone()
        },
    );
    s.opened.wait().await.unwrap();
    signal.cancel(cancelled("caller"));
    s.closing.wait().await.unwrap();
    let disposal = d.dispose();
    tokio::pin!(disposal);
    assert!(futures_util::poll!(disposal.as_mut()).is_pending());
    assert!(!d.disposed());
    gate.complete(Ok(()));
    disposal.await.unwrap();
    assert!(batch.await?.0.is_err());
    assert!(d.disposed());
    assert_eq!(
        *s.records.lock().unwrap(),
        ["xcodebuild 1 started", "xcodebuild 1 closed"]
    );
    h.finish(std::slice::from_ref(&d)).await;
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn a_simulator_starts_no_xcodebuild_while_its_previous_session_has_not_been_closed() -> TestResult {
    let h = Harness::new(|_| {})?;
    let (d, s) = prepared(&h).await?;
    let gate = SharedDeferred::new();
    *s.plan.lock().unwrap() = OutputPlan {
        lines: vec!["Test runner exited".into()],
        close_gate: Some(gate.clone()),
        ..Default::default()
    };
    let parsing = XcodebuildSession::new(h.services.runtime.clone(), h.services.logging.clone());
    let events = d
        .execute_test_request(
            TestRequest::new("/tmp/marathon", "/tmp/marathon/parse.xctestrun", false),
            parsing.clone(),
            &h.services.context,
        )
        .await
        .unwrap();
    events.receive(&h.services.context).await.unwrap();
    assert!(parsing.stop(support::duration(20), &h.services.context).await.is_some());
    assert!(matches!(run(&h, &d).await.0, Err(VendorError::DeviceLost(_))));
    assert_eq!(s.count.load(Ordering::SeqCst), 1);
    gate.complete(Ok(()));
    parsing.join_producer().await.unwrap();
    s.plan.lock().unwrap().close_gate = None;
    assert!(run(&h, &d).await.0.is_ok());
    assert_eq!(s.count.load(Ordering::SeqCst), 2);
    h.finish(&[d]).await;
    Ok(())
}
