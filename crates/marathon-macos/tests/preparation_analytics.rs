//! U212 witnesses through the Mac provider and device.

mod support;

use marathon_config::devices::Transport;
use marathon_core::vendor::{Device, DeviceEvent, DeviceProvider};
use marathon_test_support::apple::preparation_analytics::TimingFault;
use std::sync::atomic::Ordering;
use support::*;

async fn check(fault: TimingFault, setup: bool) -> R {
    let label = if setup {
        "core.Track.trackProviderDevicePreparing"
    } else {
        "core.Track.trackDevicePreparing"
    };
    let (hooks, tracker, events) = fault.instrument(label);
    let mut h = Harness::new("")?;
    h.context.hooks = hooks.clone();
    h.options.dependencies.hooks = hooks.clone();
    h.options.dependencies.tracker = tracker;
    let (provider, connections) = h.provider();
    v(provider.initialize_transport(Transport::Local, h.context.clone()).await)?;
    v(provider.join_pending().await)?;
    assert_eq!(provider.subscribe().state().queued, 1);
    let Some(DeviceEvent::Connected(connected)) = v(provider.subscribe().receive().await)? else {
        panic!("expected the Mac to be admitted");
    };
    let device = provider.snapshot().remove(0);
    assert_eq!(connected, marathon_core::vendor::DeviceRef(device.clone()));
    let before_prepare = h.worker.requests.lock().unwrap().len();
    v(device
        .prepare(h.options.dependencies.configuration.clone(), h.context.clone())
        .await)?;
    v(device.join_pending().await)?;
    {
        let requests = h.worker.requests.lock().unwrap();
        assert_eq!(requests.iter().filter(|request| request.command == ["env"]).count(), 1);
        let preparation = &requests[before_prepare..];
        let reset = &preparation[0].command;
        assert_eq!(reset[..2], ["rm", "-rf"]);
        assert_eq!(
            preparation.iter().filter(|request| &request.command == reset).count(),
            1
        );
    }
    assert_eq!(connections.creates.load(Ordering::SeqCst), 1);
    assert_eq!(connections.releases.load(Ordering::SeqCst), 0);
    assert_eq!(provider.subscribe().state().queued, 0);
    assert_eq!(provider.snapshot().len(), 1);
    fault.verify(hooks.as_ref(), label, &events);
    let warnings = h
        .logs
        .records()
        .into_iter()
        .filter(|record| record.target == "marathon_core::analytics::event_pipeline")
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1);
    let warning = &warnings[0];
    let operation = if setup { "setup" } else { "preparation" };
    assert_eq!(
        warning.message,
        format!("Could not record {operation} timing for device udid@local")
    );
    assert_eq!(warning.level, marathon_logging::Level::WARN);
    assert!(warning.cause.is_some());
    v(provider.terminate().await)?;
    v(provider.join_pending().await)?;
    Ok(())
}

#[tokio::test]
async fn mac_setup_survives_the_first_clock_failure_u212() -> R {
    check(TimingFault::FirstRead, true).await
}

#[tokio::test]
async fn mac_setup_survives_the_last_clock_failure_u212() -> R {
    check(TimingFault::LastRead, true).await
}

#[tokio::test]
async fn mac_setup_survives_a_tracker_panic_u212() -> R {
    check(TimingFault::TrackerPanic, true).await
}

#[tokio::test]
async fn mac_preparation_survives_the_first_clock_failure_u212() -> R {
    check(TimingFault::FirstRead, false).await
}

#[tokio::test]
async fn mac_preparation_survives_the_last_clock_failure_u212() -> R {
    check(TimingFault::LastRead, false).await
}

#[tokio::test]
async fn mac_preparation_survives_a_tracker_panic_u212() -> R {
    check(TimingFault::TrackerPanic, false).await
}
