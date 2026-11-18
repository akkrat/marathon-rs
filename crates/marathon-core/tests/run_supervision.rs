//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/Scheduler.kt
//! and actor/Actor.kt, Marathon.kt (D84: every unexpected end of the run's own machinery ends the run).
//!
//! The production run graph with the Stub vendor and the real scheduler, through `Marathon.run`. Before D84 a queue
//! handler's own cancellation left the run hanging, a pool's failure and a run task's panic lost their cause, and a
//! failed device subscription left the pools and devices running while the run finished (U206). Each run here keeps
//! the result of the test that finished before the failure, and leaves no actor alive.
use marathon_config::{Configuration, schema::BatchingStrategyConfiguration};
use marathon_core::{
    error::*,
    executor::shared::{SharedChannel, SharedDeferred, Signal, failure, requested},
    model::{BatchRef, DeviceInfo, DevicePoolId, Test, TestBatchResults, TestStatus},
    runtime::{Deferred, EventChannel, VendorFuture},
    time::{ClockResult, Timer},
    vendor::{BundleRef, Device, DeviceEvent, DeviceRef, TestBundleIdentifier, VendorContext},
};
use marathon_error::FailureKind;
use marathon_logging::{Component, Level};
use marathon_test_support::{
    console::Line,
    factory::CounterTimer,
    scenario::{self, Entry, Outcome, Ready, Script},
    stub_device::{StubDevice, StubDeviceOptions},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

const PROBE: &str = "D84 probe";
/// `serial-1` is ready at 1,000 ms and runs test1 until 6,000 ms, then test2 until 11,000 ms.
const SECOND_TEST_ENDS: i64 = 11_000;

fn stub(serial: &str, model: &str, prepare_time_millis: i64) -> Arc<StubDevice> {
    StubDevice::new(StubDeviceOptions {
        serial_number: serial.into(),
        model: model.into(),
        prepare_time_millis,
        ..StubDeviceOptions::default()
    })
}

/// One batch per test, `serial-1` alone in the script, and the run ending at 120 s if nothing ends it before (the hang
/// guard).
fn script() -> (marathon_test_support::factory::MarathonFactory, Script) {
    let (mut factory, mut script) = scenario::load("two-pools-disconnect.json");
    factory.configuration.configuration.batching_strategy = BatchingStrategyConfiguration::FixedSize {
        size: 1,
        duration_millis: None,
        percentile: None,
        time_limit: None,
        last_mile_length: 0,
    };
    script.entry = Entry::Run;
    script.devices.clear();
    let device = stub("serial-1", "modelA", 1_000);
    device.set_execution_results(HashMap::from(
        ["test1", "test2"].map(|method| (scenario::test("test", "SimpleTest", method), vec![TestStatus::Passed])),
    ));
    script.devices.insert("serial-1".into(), device);
    script.events = vec![
        (0, "connect".into(), Some("serial-1".into()), Value::Null),
        (120_000, "end".into(), None, Value::Null),
    ];
    (factory, script)
}

/// The cause of `Marathon run failed`, outermost first.
fn run_failure(outcome: &Outcome) -> Vec<String> {
    let causes = outcome
        .lines
        .iter()
        .filter_map(|line| match line {
            Line::Log {
                level: Level::ERROR,
                message,
                cause: Some(cause),
                ..
            } if message == "Marathon run failed" => Some(cause.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(causes.len(), 1, "{:?}", outcome.lines);
    std::iter::successors(causes.first(), |cause| cause.cause.as_deref())
        .map(|cause| cause.message.clone().unwrap_or_default())
        .collect()
}

/// The run ended, before `ends_before`, as a failed run: `false` without `ignoreFailures`, test1's result in the
/// reports, the provider terminated and no actor alive.
fn assert_failed_run(outcome: &Outcome, ends_before: i64) {
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(
        matches!(outcome.value, Some(Ok(false))),
        "the run's mapped outcome: {:?}",
        outcome.value
    );
    let finished = outcome.completed_at.unwrap();
    assert!(finished < ends_before, "finished at {finished} ms");
    let raw: Value = serde_json::from_str(&outcome.read("test_result/raw.json")).unwrap();
    let test1 = raw
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["method"] == "test1")
        .map(|row| row["status"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(test1, ["PASSED"], "{raw}");
    assert!(
        outcome.provider_calls.contains(&"terminate"),
        "{:?}",
        outcome.provider_calls
    );
    let alive = outcome
        .open_mailboxes
        .iter()
        .filter(|label| label.ends_with("Actor"))
        .collect::<Vec<_>>();
    assert!(alive.is_empty(), "actors still alive after the run: {alive:?}");
}

/// A Stub device with one call changed.
struct Hooked {
    inner: Arc<StubDevice>,
    /// Panics when the pool reads the device's worker, as it does when the device asks for a batch; completes
    /// `panicking` first.
    worker_panics: bool,
    panicking: Option<Arc<SharedDeferred<()>>>,
    /// Fails the provider's device channel this long after preparation starts, or cancels it if `cancel_events`.
    fail_events: Option<(Arc<SharedChannel<DeviceEvent>>, i64)>,
    cancel_events: bool,
    /// What a cancelled batch does before its call returns.
    after_cancel: AfterCancel,
    /// What the device says of how long its cancelled calls can take (`Device::cancellation_cleanup_millis`).
    cleanup_bound: Option<i64>,
    /// Work a cancelled preparation leaves running for this long, as an Apple installation does, and the flag it sets
    /// when it ends; `join_cancelled_work` waits for it.
    retained_preparation: Option<(i64, Arc<AtomicBool>)>,
    clock: std::sync::OnceLock<Arc<dyn marathon_core::runtime::VendorClock>>,
    /// Set when the device is asked to prepare.
    prepared: Option<Arc<AtomicBool>>,
    /// Completed when a batch starts.
    started: Option<Arc<SharedDeferred<()>>>,
}
#[derive(Default)]
enum AfterCancel {
    #[default]
    Nothing,
    /// Cleans up for this long, ignoring the cancellation, then sets the flag.
    CleansUp(i64, Arc<AtomicBool>),
    /// Waits until this is completed, as a call waiting for a connection that only the provider's termination closes.
    WaitsFor(Arc<SharedDeferred<()>>),
    /// Raises SIGTERM, then cleans up until the signal's reports have started and this long after.
    SignalsThenCleansUp {
        signal: Arc<SharedDeferred<i32>>,
        reports_started: Arc<SharedDeferred<()>>,
        millis: i64,
    },
}
fn hooked(inner: Arc<StubDevice>) -> Hooked {
    Hooked {
        inner,
        worker_panics: false,
        panicking: None,
        fail_events: None,
        cancel_events: false,
        after_cancel: AfterCancel::Nothing,
        cleanup_bound: None,
        retained_preparation: None,
        clock: std::sync::OnceLock::new(),
        prepared: None,
        started: None,
    }
}
impl Device for Hooked {
    fn info(&self) -> DeviceInfo {
        self.inner.info()
    }
    fn abi(&self) -> String {
        self.inner.abi()
    }
    fn worker_id(&self) -> Option<String> {
        if self.worker_panics {
            if let Some(panicking) = &self.panicking {
                panicking.complete(Ok(()));
            }
            panic!("{PROBE}: the pool's handler panicked");
        }
        self.inner.worker_id()
    }
    fn component(&self) -> &Component {
        self.inner.component()
    }
    fn prepare<'a>(&'a self, configuration: Arc<Configuration>, context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let _ = self.clock.set(context.clock.clone());
            if let Some(prepared) = &self.prepared {
                prepared.store(true, Ordering::SeqCst);
            }
            if let Some((channel, after)) = &self.fail_events {
                context.clock.delay(*after, context.cancellation.clone()).await?;
                if self.cancel_events {
                    channel.cancel(requested(&format!("{PROBE}: the device events were cancelled")));
                } else {
                    channel.close(Some(failure(
                        FailureKind::IllegalState,
                        &format!("{PROBE}: the device events failed"),
                    )));
                }
            }
            self.inner.prepare(configuration, context).await
        })
    }
    fn execute<'a>(
        &'a self,
        configuration: Arc<Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if let Some(started) = &self.started {
                started.complete(Ok(()));
            }
            let outcome = self
                .inner
                .execute(configuration, pool, batch, result, context.clone())
                .await;
            if let Err(VendorError::Cancelled(_)) = &outcome {
                match &self.after_cancel {
                    AfterCancel::Nothing => {}
                    AfterCancel::CleansUp(millis, done) => {
                        let _ = context.clock.delay(*millis, Signal::new()).await;
                        done.store(true, Ordering::SeqCst);
                    }
                    AfterCancel::WaitsFor(released) => {
                        let _ = released.wait().await;
                    }
                    AfterCancel::SignalsThenCleansUp {
                        signal,
                        reports_started,
                        millis,
                    } => {
                        signal.complete(Ok(15));
                        let _ = reports_started.wait().await;
                        let _ = context.clock.delay(*millis, Signal::new()).await;
                    }
                }
            }
            outcome
        })
    }
    fn cancellation_cleanup_millis(&self) -> Option<i64> {
        self.cleanup_bound
    }
    fn join_cancelled_work(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if let (Some((millis, done)), Some(clock)) = (&self.retained_preparation, self.clock.get())
                && !done.load(Ordering::SeqCst)
            {
                let _ = clock.delay(*millis, Signal::new()).await;
                done.store(true, Ordering::SeqCst);
            }
            Ok(())
        })
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        self.inner.dispose()
    }
}

/// Connects a hooked device before the run starts.
fn connect(make: impl FnOnce(Arc<SharedChannel<DeviceEvent>>) -> Hooked + Send + 'static) -> Option<Ready> {
    Some(Box::new(move |built| {
        let device = make(built.provider.channel());
        assert!(built.provider.send(DeviceEvent::Connected(DeviceRef(Arc::new(device)))));
    }))
}

/// Identifies the first batch's test; the second lookup returns a cancellation of its own, as a lookup whose own call
/// was cancelled would.
struct SecondCallCancelled(AtomicU32);
impl TestBundleIdentifier for SecondCallCancelled {
    fn identify(&self, _test: &Test) -> VendorResult<Option<BundleRef>> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 1 {
            return Err(VendorError::Cancelled(requested(&format!(
                "{PROBE}: the bundle lookup was cancelled"
            ))));
        }
        Ok(None)
    }
}

/// U206 → D84: the queue's handler returns a cancellation its actor was not given when `serial-1` asks for its second
/// batch at 6,000 ms. The queue fails instead of closing silently, and the run ends there through D68's path.
#[test]
fn a_queue_handler_cancelled_on_its_own_ends_the_run_with_the_cause_d84() {
    let (mut factory, script) = script();
    factory.bundle_identifier = Some(Arc::new(SecondCallCancelled(AtomicU32::new(0))));
    let outcome = scenario::drive(factory, script, None, None);
    assert_failed_run(&outcome, SECOND_TEST_ENDS);
    assert_eq!(
        run_failure(&outcome),
        [
            "QueueActor stopped: its handler was cancelled, but the actor was not".to_owned(),
            format!("{PROBE}: the bundle lookup was cancelled"),
        ]
    );
}

/// U206 → D84: the pool's handler panics at 8,000 ms, when `serial-2` is ready and asks for a batch while `serial-1`
/// runs test2. The run ends there with the panic as its cause, not with a result read from the stopped pool.
#[test]
fn a_pool_that_panics_ends_the_run_with_the_panic_d84() {
    let (factory, script) = script();
    let ready = connect(|_| Hooked {
        worker_panics: true,
        ..hooked(stub("serial-2", "modelA", 8_000))
    });
    let outcome = scenario::drive(factory, script, None, ready);
    assert_failed_run(&outcome, SECOND_TEST_ENDS);
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the pool's handler panicked")]);
}

/// U206 → D84: the provider's device channel fails at 8,000 ms, while `serial-1` runs test2 and `serial-2`'s pool is
/// still preparing it. The run ends there with the cause, and its pools and devices are stopped and waited for before
/// it finishes, as D68's are.
#[test]
fn a_failed_device_subscription_stops_the_pools_before_the_run_ends_d84() {
    let (factory, script) = script();
    let ready = connect(|channel| Hooked {
        fail_events: Some((channel, 8_000)),
        ..hooked(stub("serial-2", "modelZ", 600_000))
    });
    let outcome = scenario::drive(factory, script, None, ready);
    assert_failed_run(&outcome, SECOND_TEST_ENDS);
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the device events failed")]);
}

/// U206 → D84: the provider cancels its device channel at 8,000 ms. The subscription ended silently with that
/// cancellation, and the run went on without hearing of any device again; it now fails the run as a failed channel does.
#[test]
fn a_cancelled_device_subscription_ends_the_run_with_the_cause_d84() {
    let (factory, script) = script();
    let ready = connect(|channel| Hooked {
        fail_events: Some((channel, 8_000)),
        cancel_events: true,
        ..hooked(stub("serial-2", "modelZ", 600_000))
    });
    let outcome = scenario::drive(factory, script, None, ready);
    assert_failed_run(&outcome, SECOND_TEST_ENDS);
    assert_eq!(
        run_failure(&outcome),
        [
            "The device subscription stopped: it was cancelled, but the run was not".to_owned(),
            format!("{PROBE}: the device events were cancelled"),
        ]
    );
}

/// Panics on the first elapsed-time reading, which the run task takes before it initializes the provider.
struct FirstElapsedPanics(AtomicBool, CounterTimer);
impl Timer for FirstElapsedPanics {
    fn start_time_millis(&self) -> i64 {
        self.1.start_time_millis()
    }
    fn elapsed_time_millis(&self) -> ClockResult<i64> {
        if !self.0.swap(true, Ordering::SeqCst) {
            panic!("{PROBE}: the run task panicked");
        }
        self.1.elapsed_time_millis()
    }
    fn current_time_millis(&self) -> ClockResult<i64> {
        self.1.current_time_millis()
    }
    fn measure(&self, block: &mut dyn FnMut()) -> ClockResult<i64> {
        self.1.measure(block)
    }
}

/// U206 → D84: the run task panics before it stores an outcome. `Marathon run failed` names the panic, where it said
/// only that the run was cancelled.
#[test]
fn a_run_task_that_panics_fails_the_run_with_the_panic_d84() {
    let (mut factory, mut script) = script();
    // The run has ended before any device could connect.
    script.events.retain(|(_, event, ..)| event == "end");
    factory.timer = Some(Arc::new(FirstElapsedPanics(
        AtomicBool::new(false),
        CounterTimer::default(),
    )));
    let outcome = scenario::drive(factory, script, None, None);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(matches!(outcome.value, Some(Ok(false))), "{:?}", outcome.value);
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the run task panicked")]);
    assert!(outcome.open_mailboxes.iter().all(|label| !label.ends_with("Actor")));
}

/// Before the run starts: a hooked device that fails the device channel at 3,000 ms, while `serial-1` and `serial-3`
/// run test1 and test2 (1,000 to 6,000 ms); `serial-3` does `after_cancel` when its batch is cancelled. `terminate`
/// runs when the provider is terminated.
fn fail_while_running(
    after_cancel: AfterCancel,
    cleanup_bound: Option<i64>,
    terminate: impl Fn() + Send + Sync + 'static,
) -> Option<Ready> {
    let terminate = Arc::new(terminate);
    Some(Box::new(move |built| {
        let channel = built.provider.channel();
        for device in [
            Hooked {
                fail_events: Some((channel, 3_000)),
                ..hooked(stub("serial-2", "modelZ", 600_000))
            },
            Hooked {
                after_cancel,
                cleanup_bound,
                ..hooked(stub("serial-3", "modelA", 1_000))
            },
        ] {
            assert!(built.provider.send(DeviceEvent::Connected(DeviceRef(Arc::new(device)))));
        }
        *built.provider.on_terminate.lock().unwrap() = Some(Arc::new(move || {
            terminate();
            Box::pin(async { Ok(()) })
        }));
    }))
}

fn logged(outcome: &Outcome, level: Level, text: &str) -> bool {
    outcome
        .lines
        .iter()
        .any(|line| matches!(line, Line::Log { level: l, message, .. } if *l == level && message.contains(text)))
}

/// D84, review round 1: the run waits for its devices to stop after a failure before it closes anything. `serial-3`'s
/// cancelled batch takes 3 s to clean up; the provider is terminated, and the reports written, only after that.
#[test]
fn the_run_waits_for_a_cancelled_batch_to_clean_up_before_it_finishes_d84() {
    let (factory, script) = script();
    let cleaned = Arc::new(AtomicBool::new(false));
    let cleaned_before_terminate = Arc::new(AtomicBool::new(false));
    let (done, seen) = (cleaned.clone(), cleaned_before_terminate.clone());
    let ready = fail_while_running(AfterCancel::CleansUp(3_000, cleaned.clone()), None, move || {
        seen.store(done.load(Ordering::SeqCst), Ordering::SeqCst);
    });
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(cleaned.load(Ordering::SeqCst));
    assert!(
        cleaned_before_terminate.load(Ordering::SeqCst),
        "the provider was terminated while a cancelled batch was still cleaning up"
    );
    assert_eq!(outcome.completed_at, Some(6_000));
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the device events failed")]);
}

/// D84, review rounds 1 and 2: a cancelled batch whose call only ends when the provider is terminated cannot hold the
/// run for ever. The failure is logged when it happens; the run waits for the device as long as it says a cancelled call
/// can take (30 s here) and 5 s more, then finishes without it, and its termination releases the call.
#[test]
fn a_device_call_waiting_for_the_runs_end_holds_it_only_for_its_own_bound_d84() {
    let (factory, script) = script();
    let released = SharedDeferred::new();
    let release = released.clone();
    let ready = fail_while_running(AfterCancel::WaitsFor(released.clone()), Some(30_000), move || {
        release.complete(Ok(()));
    });
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert_eq!(outcome.completed_at, Some(3_000 + 30_000 + 5_000));
    assert!(released.completed().is_some());
    assert!(logged(
        &outcome,
        Level::ERROR,
        "The run's own machinery failed; stopping the run"
    ));
    assert!(logged(
        &outcome,
        Level::ERROR,
        "Device serial-3 did not stop within 35000 ms of the run's stop; finishing the run without it"
    ));
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the device events failed")]);
}

/// D84, review round 2 (R2-2): a cancelled batch whose cleanup takes longer than D52's 24 s, but within what its device
/// says a cancelled call can take (60 s), is waited for: the provider is terminated only after it.
#[test]
fn a_cleanup_within_the_devices_own_bound_is_waited_for_d84() {
    let (factory, script) = script();
    let cleaned = Arc::new(AtomicBool::new(false));
    let cleaned_before_terminate = Arc::new(AtomicBool::new(false));
    let (done, seen) = (cleaned.clone(), cleaned_before_terminate.clone());
    let ready = fail_while_running(
        AfterCancel::CleansUp(30_000, cleaned.clone()),
        Some(60_000),
        move || {
            seen.store(done.load(Ordering::SeqCst), Ordering::SeqCst);
        },
    );
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(
        cleaned_before_terminate.load(Ordering::SeqCst),
        "the provider was terminated while a cancelled batch was still cleaning up"
    );
    assert_eq!(outcome.completed_at, Some(3_000 + 30_000));
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the device events failed")]);
}

/// D84, review rounds 1 and 4 (F6): a device that connects while the run finishes (here, while the provider terminates)
/// is not admitted: the Scheduler settles before the provider terminates, so no late pool can start, run or fail beside
/// the finish. The run passes with its two results, and the late device is never prepared. (Rounds 1 to 3 admitted such
/// a device and stopped its pool when it failed.)
#[test]
fn a_device_connecting_while_the_run_finishes_is_not_admitted_d84() {
    let (factory, script) = script();
    let prepared = Arc::new(AtomicBool::new(false));
    let seen = prepared.clone();
    let ready: Option<Ready> = Some(Box::new(move |built| {
        let channel = built.provider.channel();
        let late = Arc::new(Hooked {
            prepared: Some(seen),
            ..hooked(stub("serial-2", "modelZ", 0))
        });
        *built.provider.on_terminate.lock().unwrap() = Some(Arc::new(move || {
            let _ = channel.try_send(DeviceEvent::Connected(DeviceRef(late.clone())));
            Box::pin(async { Ok(()) })
        }));
    }));
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(matches!(outcome.value, Some(Ok(true))), "{:?}", outcome.value);
    assert!(
        !prepared.load(Ordering::SeqCst),
        "a device that connected while the run finished was started"
    );
    let raw: Value = serde_json::from_str(&outcome.read("test_result/raw.json")).unwrap();
    assert_eq!(raw.as_array().unwrap().len(), 2, "{raw}");
}

/// D84, review rounds 2 and 4 (I1): the provider's device channel fails while the provider terminates, and the
/// termination then fails too. The channel's failure is the run's error, decided once the finish is over; the
/// termination's error is only logged.
#[test]
fn a_cleanup_error_after_the_first_failure_does_not_replace_it_d84() {
    let (factory, script) = script();
    let ready: Option<Ready> = Some(Box::new(|built| {
        let channel = built.provider.channel();
        *built.provider.on_terminate.lock().unwrap() = Some(Arc::new(move || {
            channel.close(Some(failure(
                FailureKind::IllegalState,
                &format!("{PROBE}: the device events failed"),
            )));
            Box::pin(async {
                Err(VendorError::Other(failure(
                    FailureKind::IllegalState,
                    &format!("{PROBE}: the termination failed"),
                )))
            })
        }));
    }));
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the device events failed")]);
    assert!(logged(
        &outcome,
        Level::ERROR,
        "The run also failed after its first failure"
    ));
    let raw: Value = serde_json::from_str(&outcome.read("test_result/raw.json")).unwrap();
    assert_eq!(raw.as_array().unwrap().len(), 2, "{raw}");
}

/// D84, review round 4 (F2): the provider's termination ends with a cancellation of its own, which nothing in the run
/// asked for. It used to stop the finish before the reports, and the run's protected finish then did nothing, so the
/// two passes that had finished reached no report file. The finish now logs it and goes on: the reports are written,
/// and the run fails with that error.
#[test]
fn a_cancelled_provider_termination_still_writes_the_reports_d84() {
    let (factory, script) = script();
    let ready: Option<Ready> = Some(Box::new(|built| {
        *built.provider.on_terminate.lock().unwrap() = Some(Arc::new(|| {
            Box::pin(async {
                Err(VendorError::Cancelled(requested(&format!(
                    "{PROBE}: the termination was cancelled"
                ))))
            })
        }));
    }));
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(matches!(outcome.value, Some(Ok(false))), "{:?}", outcome.value);
    let raw: Value = serde_json::from_str(&outcome.read("test_result/raw.json")).unwrap();
    assert_eq!(raw.as_array().unwrap().len(), 2, "{raw}");
    assert!(logged(&outcome, Level::ERROR, "failed during cleanup"));
}

/// D84, review round 4 (F1): the provider's device channel is cancelled while the provider terminates; nothing in the
/// run asked for that. The finish reads how the channel ended and the run fails with it, where it used to pass.
#[test]
fn a_device_channel_cancelled_while_the_provider_terminates_fails_the_run_d84() {
    let (factory, script) = script();
    let ready: Option<Ready> = Some(Box::new(|built| {
        let channel = built.provider.channel();
        *built.provider.on_terminate.lock().unwrap() = Some(Arc::new(move || {
            channel.cancel(requested(&format!("{PROBE}: the device events were cancelled")));
            Box::pin(async { Ok(()) })
        }));
    }));
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(matches!(outcome.value, Some(Ok(false))), "{:?}", outcome.value);
    assert_eq!(
        run_failure(&outcome),
        [
            "The provider's device channel was cancelled, but the run was not".to_owned(),
            format!("{PROBE}: the device events were cancelled"),
        ]
    );
    let raw: Value = serde_json::from_str(&outcome.read("test_result/raw.json")).unwrap();
    assert_eq!(raw.as_array().unwrap().len(), 2, "{raw}");
}

/// D84, review round 4 (F1): no device ever gets a pool: the only one's history prefetch never answers, and the device
/// channel's failure waits behind its connection. The run reaches its no-device timeout; its finish then reads the
/// channel's failure, which is the run's error, not NoDevices, so `ignoreFailures` maps it (exit 0) where NoDevices
/// would have failed the run.
#[test]
fn a_channel_failure_waiting_at_the_no_device_timeout_is_the_runs_cause_d84() {
    use marathon_core::analytics::{Analytics, NoOpMetricsProvider};
    let (mut factory, mut script) = script();
    factory.configuration.configuration.device_initialization_timeout_millis = 10_000;
    factory.configuration.configuration.ignore_failures = true;
    factory.analytics = Some(Box::new(|context| {
        let prefetch: Arc<dyn Fn() -> marathon_core::runtime::ServiceFuture<'static, ()> + Send + Sync> =
            Arc::new(|| Box::pin(std::future::pending()));
        Ok(Analytics::from_gated_lookup(
            Arc::new(NoOpMetricsProvider::new(false, context)?),
            prefetch,
        ))
    }));
    script.events.retain(|(_, event, ..)| event == "end");
    let ready: Option<Ready> = Some(Box::new(|built| {
        let channel = built.provider.channel();
        let only = Arc::new(hooked(stub("serial-2", "modelZ", 1_000)));
        assert!(channel.try_send(DeviceEvent::Connected(DeviceRef(only))).is_ok());
        channel.close(Some(failure(
            FailureKind::IllegalState,
            &format!("{PROBE}: the device events failed"),
        )));
    }));
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(matches!(outcome.value, Some(Ok(true))), "{:?}", outcome.value);
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the device events failed")]);
    assert!(!logged(&outcome, Level::WARN, "No devices found"));
}

/// D84, review rounds 3 and 4 (R3-1a, I1): while the run terminates the provider, a late device connects and the device
/// channel then fails. The run task has stored its success by the time anything could read that failure; the finish
/// reads the channel's end after the termination, and the run's one decision point, after the finish, makes the
/// failure the run's error. Without that decision point the run would pass.
#[test]
fn a_channel_failure_the_listener_still_holds_at_the_end_fails_the_run_d84() {
    use marathon_core::analytics::{Analytics, NoOpMetricsProvider};
    let (mut factory, script) = script();
    let prefetches = Arc::new(AtomicU32::new(0));
    factory.analytics = Some(Box::new(move |context| {
        let prefetch: Arc<dyn Fn() -> marathon_core::runtime::ServiceFuture<'static, ()> + Send + Sync> =
            Arc::new(move || {
                if prefetches.fetch_add(1, Ordering::SeqCst) == 0 {
                    Box::pin(async { Ok(()) })
                } else {
                    Box::pin(std::future::pending())
                }
            });
        Ok(Analytics::from_gated_lookup(
            Arc::new(NoOpMetricsProvider::new(false, context)?),
            prefetch,
        ))
    }));
    let ready: Option<Ready> = Some(Box::new(|built| {
        let channel = built.provider.channel();
        let late = Arc::new(hooked(stub("serial-2", "modelZ", 1_000)));
        *built.provider.on_terminate.lock().unwrap() = Some(Arc::new(move || {
            let _ = channel.try_send(DeviceEvent::Connected(DeviceRef(late.clone())));
            channel.close(Some(failure(
                FailureKind::IllegalState,
                &format!("{PROBE}: the device events failed"),
            )));
            Box::pin(async { Ok(()) })
        }));
    }));
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(matches!(outcome.value, Some(Ok(false))), "{:?}", outcome.value);
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the device events failed")]);
}

/// D84, review rounds 3, 4 and the gate (R3-1b, I3): in pool modelY both tests end at 1,100 ms; the device channel fails
/// at 2,000 ms, while `serial-1`'s test runs in pool modelA. The stop drains every queue before anything closes, so both
/// of pool modelY's results are in the reports, and the run ends with the channel's cause. (A queue held in its Track
/// with a result waiting behind is witnessed where the tracker can be held: `pool_scheduler`'s
/// `a_result_delivered_to_the_queue_before_a_failure_is_still_tracked_d84` and
/// `a_pool_failure_leaves_every_queue_the_results_delivered_to_it_d84`. Here the analytics backend's queue would take
/// test results asynchronously, so a backend cannot hold the run's queue.)
#[test]
fn the_reports_hold_the_results_a_stop_drains_d84() {
    let (factory, script) = script();
    let ready: Option<Ready> = Some(Box::new(|built| {
        let channel = built.provider.channel();
        let device = |serial: &str| {
            let device = StubDevice::new(StubDeviceOptions {
                serial_number: serial.into(),
                model: "modelY".into(),
                prepare_time_millis: 100,
                test_time_millis: 1_000,
                ..StubDeviceOptions::default()
            });
            device.set_execution_results(HashMap::from(
                ["test1", "test2"]
                    .map(|method| (scenario::test("test", "SimpleTest", method), vec![TestStatus::Passed])),
            ));
            device
        };
        let devices: Vec<Arc<dyn Device>> = vec![
            Arc::new(hooked(device("serial-3"))),
            Arc::new(hooked(device("serial-5"))),
            Arc::new(Hooked {
                fail_events: Some((channel, 2_000)),
                ..hooked(stub("serial-4", "modelZ", 600_000))
            }),
        ];
        for device in devices {
            assert!(built.provider.send(DeviceEvent::Connected(DeviceRef(device))));
        }
    }));
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the device events failed")]);
    let raw: Value = serde_json::from_str(&outcome.read("test_result/raw.json")).unwrap();
    for serial in ["serial-3", "serial-5"] {
        assert!(
            raw.as_array().unwrap().iter().any(|row| row["deviceSerial"] == serial),
            "no result of {serial} in the reports: {raw}"
        );
    }
}

/// D84, review round 3 (R3-2): the run stops while `serial-3` is still preparing. Its cancelled preparation returns at
/// once, but leaves 3 s of work running, as an Apple installation does. The stop waits for that work, within the
/// device's own bound, before the provider is terminated.
#[test]
fn the_run_waits_for_a_preparation_left_running_before_terminating_the_provider_d84() {
    let (factory, script) = script();
    let finished = Arc::new(AtomicBool::new(false));
    let finished_before_terminate = Arc::new(AtomicBool::new(false));
    let (done, seen) = (finished.clone(), finished_before_terminate.clone());
    let ready: Option<Ready> = Some(Box::new(move |built| {
        let channel = built.provider.channel();
        for device in [
            Hooked {
                fail_events: Some((channel, 3_000)),
                ..hooked(stub("serial-2", "modelZ", 600_000))
            },
            Hooked {
                cleanup_bound: Some(60_000),
                retained_preparation: Some((3_000, finished.clone())),
                ..hooked(stub("serial-3", "modelY", 600_000))
            },
        ] {
            assert!(built.provider.send(DeviceEvent::Connected(DeviceRef(Arc::new(device)))));
        }
        *built.provider.on_terminate.lock().unwrap() = Some(Arc::new(move || {
            seen.store(done.load(Ordering::SeqCst), Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }));
    }));
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(
        finished_before_terminate.load(Ordering::SeqCst),
        "the provider was terminated under a preparation still running"
    );
    assert_eq!(outcome.completed_at, Some(3_000 + 3_000));
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the device events failed")]);
}

type SharedClock = Arc<std::sync::OnceLock<Arc<dyn marathon_core::runtime::VendorClock>>>;
fn now(clock: &SharedClock) -> i64 {
    clock.get().map_or(-1, |clock| clock.now_millis())
}

/// A reporter that takes 6.5 s, reports when it starts and records when it starts and ends.
struct SlowReports(
    SharedClock,
    Arc<std::sync::Mutex<Vec<(String, i64)>>>,
    Arc<SharedDeferred<()>>,
);
impl marathon_core::report::Reporter for SlowReports {
    fn generate<'a>(
        &'a self,
        _: &'a marathon_core::report::ExecutionReport,
    ) -> marathon_core::runtime::ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.1.lock().unwrap().push(("reports started".into(), now(&self.0)));
            self.2.complete(Ok(()));
            if let Some(clock) = self.0.get() {
                let _ = clock.delay(6_500, Signal::new()).await;
            }
            self.1.lock().unwrap().push(("reports ended".into(), now(&self.0)));
            Ok(())
        })
    }
}
/// The console. When it is given the output only the run's finish flushes (the line of the run's last pool ending,
/// "terminate pool"), it connects the late device and holds that flush until the late device's batch is running: the
/// late device's pool starts after D65's boundary, while the finish is under way.
struct ConnectsLateWhileFinishing {
    late: Arc<std::sync::Mutex<Option<(Arc<SharedChannel<DeviceEvent>>, DeviceRef)>>>,
    running: Arc<SharedDeferred<()>>,
}
impl marathon_core::output::ConsoleSink for ConnectsLateWhileFinishing {
    fn write(&self, records: Vec<marathon_core::logging::OutputRecord>) -> VendorFuture<'_, ()> {
        let finishing = records.iter().any(|record| {
            matches!(record, marathon_core::logging::OutputRecord::Log(log) if log.message.starts_with("terminate pool modelA"))
        });
        let late = if finishing {
            self.late.lock().unwrap().take()
        } else {
            None
        };
        Box::pin(async move {
            if let Some((channel, device)) = late {
                let _ = channel.try_send(DeviceEvent::Connected(device));
                let _ = self.running.wait().await;
            }
            Ok(())
        })
    }
}

/// D52, D84 (review round 5; made event-driven after the gate): SIGTERM arrives while the run's finish is settling,
/// waiting for a cancelled batch to end, and that wait ends inside the signal's reports' head start. The order is held
/// by events, not by times: the console starts a late pool when it is given the output only the finish flushes, and
/// holds that flush until the late pool's batch runs; the settle stops that pool, and the cancelled batch raises the
/// signal and then cleans up until the signal's reports (6.5 s) have started and 4 s more. The run's own finish then
/// hands over: the provider is terminated only after the reports have ended, and the run exits with 143 within D52's
/// 24 s. On 9f744841 the finish went on after the settle and terminated the provider beside the reports.
#[test]
fn a_signal_while_the_run_settles_keeps_the_reports_first_d84() {
    use marathon_core::analytics::{ReporterRegistration, ReporterSlot};
    let (mut factory, script) = script();
    factory.configuration.configuration.debug = true;
    let clock: SharedClock = Arc::new(std::sync::OnceLock::new());
    let trigger = SharedDeferred::<i32>::new();
    let reports_started = SharedDeferred::new();
    let running = SharedDeferred::new();
    let events: Arc<std::sync::Mutex<Vec<(String, i64)>>> = Arc::default();
    factory.signal_trigger = Some(trigger.clone());
    let late: Arc<std::sync::Mutex<Option<(Arc<SharedChannel<DeviceEvent>>, DeviceRef)>>> = Arc::default();
    factory.console_sink = Some(Arc::new(ConnectsLateWhileFinishing {
        late: late.clone(),
        running: running.clone(),
    }));
    factory.reporters = vec![ReporterRegistration {
        slot: ReporterSlot::Html,
        reporter: Arc::new(SlowReports(clock.clone(), events.clone(), reports_started.clone())),
    }];
    let (recorded, exit_clock) = (events.clone(), clock.clone());
    factory.exit = Some(Arc::new(move |status| {
        recorded
            .lock()
            .unwrap()
            .push((format!("exit {status}"), now(&exit_clock)));
    }));
    let prepare_clock = clock.clone();
    let prepare: scenario::Prepare = Box::new(move |core, _| {
        let _ = prepare_clock.set(core.vendor_clock());
    });
    let (terminated, terminate_clock) = (events.clone(), clock.clone());
    let ready: Option<Ready> = Some(Box::new(move |built| {
        let device = Hooked {
            started: Some(running),
            after_cancel: AfterCancel::SignalsThenCleansUp {
                signal: trigger,
                reports_started,
                millis: 4_000,
            },
            ..hooked(stub("serial-l", "modelZ", 0))
        };
        *late.lock().unwrap() = Some((built.provider.channel(), DeviceRef(Arc::new(device))));
        *built.provider.on_terminate.lock().unwrap() = Some(Arc::new(move || {
            terminated
                .lock()
                .unwrap()
                .push(("provider terminated".into(), now(&terminate_clock)));
            Box::pin(async { Ok(()) })
        }));
    }));
    let _outcome = scenario::drive(factory, script, Some(prepare), ready);
    let events = events.lock().unwrap().clone();
    let at = |name: &str| {
        events
            .iter()
            .find(|(event, _)| event == name)
            .map(|(_, at)| *at)
            .unwrap_or_else(|| panic!("no {name}: {events:?}"))
    };
    assert!(
        at("provider terminated") >= at("reports ended"),
        "the provider was terminated while the signal's reports were still being written: {events:?}"
    );
    let exit = at("exit 143");
    assert!(exit <= at("reports started") + 25_000, "{events:?}");
}

struct NeverExtracts;
impl marathon_core::vendor::RemoteTestParser for NeverExtracts {
    fn extract(&self, _: DeviceRef, _: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        Box::pin(async { Ok(vec![]) })
    }
}

/// D84, review round 5 (with the iOS witness
/// `simulator_panic::a_borrow_ends_with_the_monitorings_failure_when_no_simulator_is_available_d84`): before any
/// Scheduler exists, a remote parse's borrow ends with the provider's device-monitoring failure. That failure is the
/// run's cause, and `ignoreFailures` maps it as any failure, where a no-device timeout would have failed the run.
#[test]
fn a_monitoring_failure_a_remote_parse_hears_of_is_the_runs_cause_d84() {
    let (mut factory, mut script) = script();
    factory.configuration.configuration.ignore_failures = true;
    factory.parser = Some(marathon_core::vendor::TestParser::Remote(Arc::new(NeverExtracts)));
    script.events.retain(|(_, event, ..)| event == "end");
    let ready: Option<Ready> = Some(Box::new(|built| {
        *built.provider.borrow_error.lock().unwrap() = Some(VendorError::Other(failure(
            FailureKind::Runtime,
            &format!("{PROBE}: the simulator's setup panicked"),
        )));
    }));
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(matches!(outcome.value, Some(Ok(true))), "{:?}", outcome.value);
    assert_eq!(
        run_failure(&outcome),
        [format!("{PROBE}: the simulator's setup panicked")]
    );
    assert!(!logged(&outcome, Level::WARN, "No devices found"));
}

/// D84, review round 6: a remote parse waits for a device that never comes (the borrow never returns), and the provider
/// has published a failure of its device source by the time the parse's deadline (1,050 ms) ends the wait. The parse
/// ends with that failure, the run's cause, which `ignoreFailures` maps, instead of with no devices.
#[test]
fn a_parse_deadline_ends_with_the_providers_published_failure_d84() {
    let (mut factory, mut script) = script();
    factory.configuration.configuration.ignore_failures = true;
    factory.configuration.configuration.device_initialization_timeout_millis = 1_050;
    factory.parser = Some(marathon_core::vendor::TestParser::Remote(Arc::new(NeverExtracts)));
    script.events.retain(|(_, event, ..)| event == "end");
    let ready: Option<Ready> = Some(Box::new(|built| {
        *built.provider.borrow_delay_millis.lock().unwrap() = i64::MAX;
        *built.provider.published_failure.lock().unwrap() = Some(failure(
            FailureKind::Runtime,
            &format!("{PROBE}: the simulator's setup panicked"),
        ));
    }));
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(matches!(outcome.value, Some(Ok(true))), "{:?}", outcome.value);
    assert_eq!(
        run_failure(&outcome),
        [format!("{PROBE}: the simulator's setup panicked")]
    );
    assert!(!logged(&outcome, Level::WARN, "No devices found"));
}
