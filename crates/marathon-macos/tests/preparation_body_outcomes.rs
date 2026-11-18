//! The Mac keeps the outcome of the setup or preparation body under healthy analytics.

mod support;

use marathon_apple::{
    apple_device::AppleDevice,
    cmd::{cancelled, failure},
};
use marathon_config::devices::Transport;
use marathon_core::{
    error::{LeavePoolReason, VendorError},
    executor::shared::Signal,
    vendor::{Device, DeviceProvider},
};
use marathon_error::{FailureKind, InjectedKind};
use marathon_test_support::apple::preparation_analytics::HeldCommand;
use std::sync::{Arc, atomic::Ordering};
use support::*;

#[tokio::test]
async fn mac_setup_keeps_the_body_failure_u212() -> R {
    let h = Harness::new("")?;
    let error = VendorError::Other(failure(FailureKind::IllegalState, "setup command failed"));
    let (original, injected) = (h.worker.fake.before.lock().unwrap().clone(), error.clone());
    *h.worker.fake.before.lock().unwrap() = Arc::new(move |command| {
        if command == ["env"] {
            let injected = injected.clone();
            Box::pin(async move { Err(injected) })
        } else {
            original(command)
        }
    });
    let (provider, _) = h.provider();
    assert_eq!(
        provider.initialize_transport(Transport::Local, h.context.clone()).await,
        Err(error)
    );
    assert!(provider.snapshot().is_empty());
    v(provider.terminate().await)
}

#[tokio::test]
async fn mac_preparation_keeps_the_body_failure_u212() -> R {
    let h = Harness::new("")?;
    h.bridge.stalled.store(true, Ordering::SeqCst);
    let device = h.device();
    v(device.setup(&h.context).await)?;
    let error = VendorError::LeavePool {
        reason: LeavePoolReason::TransferStalled,
        detail: failure(FailureKind::Injected(InjectedKind::TransferStalled), "no progress"),
    };
    assert_eq!(
        device
            .prepare(h.options.dependencies.configuration.clone(), h.context.clone())
            .await,
        Err(error.clone())
    );
    assert_eq!(h.bridge.stalled_sends.load(Ordering::SeqCst), 1);
    assert_eq!(device.join_pending().await, Err(error));
    v(device.dispose().await)
}

#[tokio::test]
async fn mac_setup_propagates_caller_cancellation_during_the_body_u212() -> R {
    let h = Harness::new("")?;
    let (provider, _) = h.provider();
    let gate = HeldCommand::attach(&h.worker.fake, |command| command == ["env"]);
    let signal = Signal::new();
    let mut context = h.context.clone();
    context.cancellation = signal.clone();
    let reason = cancelled("caller stopped setup");
    let result = gate
        .cancel_during(
            provider.initialize_transport(Transport::Local, context),
            &signal,
            reason.clone(),
        )
        .await;
    assert_eq!(result, Err(VendorError::Cancelled(reason)));
    assert!(provider.snapshot().is_empty());
    v(provider.terminate().await)
}

#[tokio::test]
async fn mac_preparation_propagates_caller_cancellation_during_the_body_u212() -> R {
    let h = Harness::new("")?;
    let device = h.device();
    v(device.setup(&h.context).await)?;
    let gate = HeldCommand::attach(&h.worker.fake, |command| command.first().is_some_and(|op| op == "rm"));
    let signal = Signal::new();
    let mut context = h.context.clone();
    context.cancellation = signal.clone();
    let reason = cancelled("caller stopped preparation");
    let result = gate
        .cancel_during(
            device.prepare(h.options.dependencies.configuration.clone(), context),
            &signal,
            reason.clone(),
        )
        .await;
    assert_eq!(result, Err(VendorError::Cancelled(reason)));
    v(device.join_pending().await)?;
    v(device.dispose().await)
}
