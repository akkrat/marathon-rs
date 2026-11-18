//! The simulator keeps the outcome of the setup or preparation body under healthy analytics.

mod simulator_support;
mod support;

use marathon_apple::cmd::cancelled;
use marathon_config::devices::Transport;
use marathon_core::{
    error::VendorError,
    executor::shared::Signal,
    vendor::{Device, VendorContext},
};
use marathon_ios::device::{
    error::ProvisioningResult, simulator_factory::SimulatorCreating,
    worker_provisioning_context::WorkerProvisioningContext,
};
use marathon_test_support::apple::{FakeSimulatorWorker, preparation_analytics::HeldCommand};
use simulator_support::{Harness, TestResult};
use std::sync::Arc;
use support::{BridgeMock, error};

async fn setup(h: &Harness, context: VendorContext) -> ProvisioningResult<()> {
    h.factory()
        .create(
            Transport::Local,
            h.worker.clone(),
            Arc::new(BridgeMock),
            "a".into(),
            Arc::new(WorkerProvisioningContext::new(Transport::Local, h.clock.clone())),
            context,
        )
        .await
        .map(|_| ())
}

fn fail_command(worker: &FakeSimulatorWorker, command: &'static [&'static str], injected: VendorError) {
    let mut hook = worker.before.lock().unwrap();
    let original = hook.clone();
    *hook = Arc::new(move |argv| {
        if argv == command {
            let injected = injected.clone();
            Box::pin(async move { Err(injected) })
        } else {
            original(argv)
        }
    });
}

#[tokio::test(start_paused = true)]
async fn simulator_setup_keeps_the_body_failure_u212() -> TestResult {
    let h = Harness::new(|_| {})?;
    let injected = error("setup command failed");
    fail_command(&h.worker, &["xcrun", "simctl", "getenv", "a", "HOME"], injected.clone());
    let outcome = setup(&h, h.services.context.clone()).await.unwrap_err();
    assert_eq!(outcome.into_vendor(), injected);
    h.finish(&[]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn simulator_preparation_keeps_the_body_failure_u212() -> TestResult {
    let h = Harness::new(|_| {})?;
    let device = h.device().await?;
    let injected = error("preparation command failed");
    fail_command(
        &h.worker,
        &["pkill", "-f", "'simctl io a recordVideo'"],
        injected.clone(),
    );
    assert_eq!(h.prepare(&device).await, Err(injected));
    h.finish(&[device]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn simulator_setup_propagates_caller_cancellation_during_the_body_u212() -> TestResult {
    let h = Harness::new(|_| {})?;
    let gate = HeldCommand::attach(&h.worker, |argv| argv == ["xcrun", "simctl", "getenv", "a", "HOME"]);
    let signal = Signal::new();
    let mut context = h.services.context.clone();
    context.cancellation = signal.clone();
    let reason = cancelled("caller stopped setup");
    let outcome = gate.cancel_during(setup(&h, context), &signal, reason.clone()).await;
    assert_eq!(outcome.unwrap_err().into_vendor(), VendorError::Cancelled(reason));
    h.finish(&[]).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn simulator_preparation_propagates_caller_cancellation_during_the_body_u212() -> TestResult {
    let h = Harness::new(|_| {})?;
    let device = h.device().await?;
    let gate = HeldCommand::attach(&h.worker, |argv| argv == ["pkill", "-f", "'simctl io a recordVideo'"]);
    let signal = Signal::new();
    let mut context = h.services.context.clone();
    context.cancellation = signal.clone();
    let reason = cancelled("caller stopped preparation");
    let result = gate
        .cancel_during(
            device.prepare(h.services.configuration.clone(), context),
            &signal,
            reason.clone(),
        )
        .await;
    assert_eq!(result, Err(VendorError::Cancelled(reason)));
    h.finish(&[device]).await;
    Ok(())
}
