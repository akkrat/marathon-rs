//! U212 witnesses through the simulator provider, factory and device.

mod provider_support;
mod simulator_support;
mod support;

use marathon_core::{analytics::Track, vendor::DeviceProvider};
use marathon_test_support::apple::preparation_analytics::TimingFault;
use provider_support::{ProviderHarness, count, home, test_error};
use simulator_support::TestResult;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn check(fault: TimingFault, setup: bool) -> TestResult {
    let label = if setup {
        "core.Track.trackProviderDevicePreparing"
    } else {
        "core.Track.trackDevicePreparing"
    };
    let (hooks, tracker, events) = fault.instrument(label);
    let mut p = ProviderHarness::new(|_| {})?;
    p.h.services.track = Track::new(p.h.services.runtime.clone(), hooks.clone());
    p.h.services.track.add(tracker);
    p.provider = ProviderHarness::build(&p.h, p.connections.clone(), p.recovery.clone())?;
    p.update(&["a"]).await?;
    assert_eq!(
        p.provider.devices().len(),
        1,
        "the simulator must be registered without a retry"
    );
    let device = p.connected(1).await.remove(0);
    let admissions = Arc::new(AtomicUsize::new(0));
    let admitted = admissions.clone();
    device.set_admission(Some(Arc::new(move || {
        admitted.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    })));
    p.h.prepare(&device).await.map_err(test_error)?;
    assert_eq!(admissions.load(Ordering::SeqCst), 1);
    assert_eq!(home(&p.h.worker, "a"), 1);
    assert_eq!(count(&p.h.worker, &["pkill", "-f", "'simctl io a recordVideo'"]), 1);
    // One lease for planning and one for the simulator. Only planning has ended.
    assert_eq!(p.connections.acquired.load(Ordering::SeqCst), 2);
    assert_eq!(p.connections.released.load(Ordering::SeqCst), 1);
    assert!(p.recovery.failed.lock().unwrap().is_empty());
    assert_eq!(p.provider.subscribe().state().queued, 0);
    assert!(!device.disposed());
    assert!(!device.lost());
    fault.verify(hooks.as_ref(), label, &events);
    p.finish().await
}

#[tokio::test(start_paused = true)]
async fn simulator_setup_survives_the_first_clock_failure_u212() -> TestResult {
    check(TimingFault::FirstRead, true).await
}

#[tokio::test(start_paused = true)]
async fn simulator_setup_survives_the_last_clock_failure_u212() -> TestResult {
    check(TimingFault::LastRead, true).await
}

#[tokio::test(start_paused = true)]
async fn simulator_setup_survives_a_tracker_panic_u212() -> TestResult {
    check(TimingFault::TrackerPanic, true).await
}

#[tokio::test(start_paused = true)]
async fn simulator_preparation_survives_the_first_clock_failure_u212() -> TestResult {
    check(TimingFault::FirstRead, false).await
}

#[tokio::test(start_paused = true)]
async fn simulator_preparation_survives_the_last_clock_failure_u212() -> TestResult {
    check(TimingFault::LastRead, false).await
}

#[tokio::test(start_paused = true)]
async fn simulator_preparation_survives_a_tracker_panic_u212() -> TestResult {
    check(TimingFault::TrackerPanic, false).await
}
