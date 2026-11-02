//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/device/DeviceActorBatchAccountingTest.kt
//!
//! A batch leaving Running is accounted for exactly once: by the results the device
//! reported or, without usable results, by returning the whole batch. `withTimeout`
//! and the dispatcher tricks become virtual time and explicit drains; each scenario
//! is shared with the oracle comparison (tests/device_actor_oracle.rs).
#[path = "support/device_actor.rs"]
mod device_actor;
#[path = "support/device_actor_scenarios.rs"]
mod device_actor_scenarios;
mod support;
use device_actor::*;
use device_actor_scenarios::run_scenario;
use marathon_core::execution::FromDevice;
use std::sync::Arc;

/// After the initial IsReady, the messages the test receives.
fn after_ready(outcome: &Outcome) -> Vec<FromDevice> {
    let received = outcome.received();
    assert!(
        matches!(received.first(), Some(FromDevice::IsReady(d)) if *d == outcome.device),
        "{:?}",
        outcome.describe()
    );
    received[1..].to_vec()
}
fn returned(outcome: &Outcome) -> FromDevice {
    FromDevice::ReturnTestBatch(
        outcome.device.clone(),
        outcome.batch.clone(),
        "Device serial-1 terminated".into(),
    )
}
fn terminated(outcome: &Outcome) -> FromDevice {
    FromDevice::Terminated(outcome.device.clone())
}
fn passed_results(outcome: &Outcome) -> FromDevice {
    FromDevice::CompletedTestBatch(outcome.device.clone(), passed(&outcome.device, &outcome.batch))
}
/// `expectReturnedBatchThenTerminated` and `expectNoMoreMessages`.
fn expect_returned_batch_then_terminated(outcome: &Outcome) {
    let messages = after_ready(outcome);
    assert!(
        messages == [returned(outcome), terminated(outcome)],
        "{:?}",
        outcome.describe()
    );
}

#[test]
fn lost_device_preserves_already_reported_passes() {
    let outcome = run_scenario("lost device preserves already reported passes");
    let messages = after_ready(&outcome);
    let Some(FromDevice::CompletedTestBatch(_, results)) = messages.first() else {
        panic!(
            "Already completed pass was lost; pool received {:?} instead",
            outcome.describe()
        );
    };
    assert_eq!(
        results.finished.iter().map(|r| r.test.clone()).collect::<Vec<_>>(),
        outcome.batch.tests
    );
    assert!(messages[1..] == [terminated(&outcome)], "{:?}", outcome.describe());
}

#[test]
fn lost_device_returns_the_batch_when_the_reported_results_failed() {
    let outcome = run_scenario("lost device returns the batch when the reported results failed");
    // The reason is the reported failure's message, not "terminated".
    let messages = after_ready(&outcome);
    assert!(
        messages
            == [
                FromDevice::ReturnTestBatch(
                    outcome.device.clone(),
                    outcome.batch.clone(),
                    "result bundle is corrupted".into()
                ),
                terminated(&outcome)
            ],
        "{:?}",
        outcome.describe()
    );
}

#[test]
fn lost_device_returns_the_batch_when_the_reported_results_were_cancelled() {
    let outcome = run_scenario("lost device returns the batch when the reported results were cancelled");
    let messages = after_ready(&outcome);
    assert!(
        messages
            == [
                FromDevice::ReturnTestBatch(
                    outcome.device.clone(),
                    outcome.batch.clone(),
                    "Job was cancelled".into()
                ),
                terminated(&outcome)
            ],
        "{:?}",
        outcome.describe()
    );
}

#[test]
fn cancelled_execution_sends_already_reported_results() {
    let outcome = run_scenario("cancelled execution sends already reported results");
    let messages = after_ready(&outcome);
    assert!(
        messages == [passed_results(&outcome), terminated(&outcome)],
        "{:?}",
        outcome.describe()
    );
}

#[test]
fn cancelled_execution_without_results_returns_the_batch() {
    let outcome = run_scenario("cancelled execution without results returns the batch");
    expect_returned_batch_then_terminated(&outcome);
}

#[test]
fn termination_after_reported_results_sends_them_and_cancels_the_execution() {
    let outcome = run_scenario("termination after reported results sends them and cancels the execution");
    let messages = after_ready(&outcome);
    assert!(
        messages == [passed_results(&outcome), terminated(&outcome)],
        "{:?}",
        outcome.describe()
    );
    assert!(outcome.job_completed());
    assert_eq!(outcome.extra["executionCancelled"], true);
}

#[test]
fn termination_before_results_returns_the_batch_once_and_ignores_a_late_report() {
    let outcome = run_scenario("termination before results returns the batch once and ignores a late report");
    expect_returned_batch_then_terminated(&outcome);
    assert!(outcome.job_completed());
    assert_eq!(outcome.extra["lateReportAccepted"], false);
}

#[test]
fn batch_failure_during_termination_does_not_return_the_batch_twice() {
    let outcome = run_scenario("batch failure during termination does not return the batch twice");
    expect_returned_batch_then_terminated(&outcome);
    assert!(outcome.job_completed());
}

/// Kotlin relies on single-thread dispatcher queueing (a `parent.cancel()` launched
/// from inside execute). The Rust test makes that order explicit in replay: the
/// parent is cancelled after the vendor's return is bridged and before readiness runs.
#[test]
fn results_of_a_completed_batch_reach_the_pool_even_if_the_actor_stops_right_after() {
    let outcome = run_scenario("results of a completed batch reach the pool even if the actor stops right after");
    let messages = after_ready(&outcome);
    assert!(
        messages.first() == Some(&passed_results(&outcome)),
        "{:?}",
        outcome.describe()
    );
    assert!(matches!(outcome.parent, marathon_core::runtime::JobState::Completed(_)));
}

/// Kotlin's vendor launches a child into the prepare coroutine that fails later.
/// `VendorContext` has no job and no production vendor does this, so a child the
/// test spawns under the actor job fails it; the actor observes the same failure.
#[test]
fn actor_failing_before_a_dispatched_batch_starts_executing_returns_the_batch() {
    let outcome = run_scenario("actor failing before a dispatched batch starts executing returns the batch");
    expect_returned_batch_then_terminated(&outcome);
    assert!(outcome.job_completed());
    assert_eq!(outcome.calls.1, 0, "execution started");
}

#[test]
fn completed_execution_with_cancelled_results_returns_the_batch_and_stays_ready() {
    let outcome = run_scenario("completed execution with cancelled results returns the batch and stays ready");
    let messages = after_ready(&outcome);
    let [FromDevice::ReturnTestBatch(_, batch, _), ready] = messages.as_slice() else {
        panic!("{:?}", outcome.describe());
    };
    assert!(Arc::ptr_eq(batch, &outcome.batch));
    assert!(*ready == FromDevice::IsReady(outcome.device.clone()));
}
