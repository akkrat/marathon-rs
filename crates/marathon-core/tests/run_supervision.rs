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
}
#[derive(Default)]
enum AfterCancel {
    #[default]
    Nothing,
    /// Cleans up for this long, ignoring the cancellation, then sets the flag.
    CleansUp(i64, Arc<AtomicBool>),
    /// Waits until this is completed, as a call waiting for a connection that only the provider's termination closes.
    WaitsFor(Arc<SharedDeferred<()>>),
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

/// D84, review round 1: after the Scheduler has returned, while the run finishes, a late device starts a pool that then
/// fails. The run had read success; it now fails with the pool's cause, and the late pool is stopped.
#[test]
fn a_pool_that_fails_while_the_run_finishes_fails_the_run_d84() {
    let (factory, script) = script();
    let ready: Option<Ready> = Some(Box::new(|built| {
        let channel = built.provider.channel();
        let panicking = SharedDeferred::new();
        let late = Arc::new(Hooked {
            worker_panics: true,
            panicking: Some(panicking.clone()),
            ..hooked(stub("serial-2", "modelZ", 1_000))
        });
        *built.provider.on_terminate.lock().unwrap() = Some(Arc::new(move || {
            let (channel, late, panicking) = (channel.clone(), late.clone(), panicking.clone());
            Box::pin(async move {
                // Connects while the run terminates the provider, and holds the termination until the device's pool
                // has asked for a batch, which panics.
                let _ = channel.try_send(DeviceEvent::Connected(DeviceRef(late)));
                let _ = panicking.wait().await;
                Ok(())
            })
        }));
    }));
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert!(matches!(outcome.value, Some(Ok(false))), "{:?}", outcome.value);
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the pool's handler panicked")]);
    let raw: Value = serde_json::from_str(&outcome.read("test_result/raw.json")).unwrap();
    assert_eq!(raw.as_array().unwrap().len(), 2, "{raw}");
    let alive = outcome
        .open_mailboxes
        .iter()
        .filter(|label| label.ends_with("Actor"))
        .collect::<Vec<_>>();
    assert!(alive.is_empty(), "actors still alive after the run: {alive:?}");
}

/// D84, review round 2 (P1, I1): the late pool fails while the provider terminates, and the termination then fails too.
/// The pool's failure is the run's error; the termination's is only logged.
#[test]
fn a_cleanup_error_after_the_first_failure_does_not_replace_it_d84() {
    let (factory, script) = script();
    let ready: Option<Ready> = Some(Box::new(|built| {
        let channel = built.provider.channel();
        let panicking = SharedDeferred::new();
        let late = Arc::new(Hooked {
            worker_panics: true,
            panicking: Some(panicking.clone()),
            ..hooked(stub("serial-2", "modelZ", 1_000))
        });
        *built.provider.on_terminate.lock().unwrap() = Some(Arc::new(move || {
            let (channel, late, panicking) = (channel.clone(), late.clone(), panicking.clone());
            Box::pin(async move {
                let _ = channel.try_send(DeviceEvent::Connected(DeviceRef(late)));
                let _ = panicking.wait().await;
                Err(VendorError::Other(failure(
                    FailureKind::IllegalState,
                    &format!("{PROBE}: the termination failed"),
                )))
            })
        }));
    }));
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the pool's handler panicked")]);
    assert!(logged(
        &outcome,
        Level::ERROR,
        "Finishing the run failed after its first failure"
    ));
}

/// D84, review round 3 (R3-1a): while the run terminates the provider, a late device connects and the device channel
/// then fails. The listener is still inside that device's connection (its pool's history prefetch never answers), so it
/// has not read the failure when the run would store its success. The run settles its Scheduler before the reports:
/// the listener ends, the channel's failure is read and is the run's error.
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

/// A Graphite backend that holds the first Track of a pool's result for 5 s, as a slow backend would.
struct SlowBackend(
    Arc<std::sync::OnceLock<Arc<dyn marathon_core::runtime::VendorClock>>>,
    &'static str,
    AtomicBool,
);
impl marathon_core::analytics::TrackerInternalAdapter for SlowBackend {
    fn test<'a>(&'a self, event: &'a marathon_core::report::TestEvent) -> VendorFuture<'a, ()> {
        let hold = event.pool_id.name == self.1 && !self.2.swap(true, Ordering::SeqCst);
        Box::pin(async move {
            if hold && let Some(clock) = self.0.get() {
                clock.delay(5_000, Signal::new()).await?;
            }
            Ok(())
        })
    }
}
struct SlowBackends(Arc<std::sync::OnceLock<Arc<dyn marathon_core::runtime::VendorClock>>>);
impl marathon_core::analytics::BackendFactory for SlowBackends {
    fn create(
        &self,
        _: marathon_core::analytics::BackendRequest,
    ) -> marathon_core::runtime::ServiceFuture<
        '_,
        Result<Option<Arc<dyn marathon_core::analytics::TrackerInternal>>, Failure>,
    > {
        let backend = SlowBackend(self.0.clone(), "modelY", AtomicBool::new(false));
        Box::pin(async move {
            Ok(Ok(Some(
                Arc::new(backend) as Arc<dyn marathon_core::analytics::TrackerInternal>
            )))
        })
    }
}

/// D84, review round 3 (R3-1b): while the run terminates the provider, late devices start two pools. In pool modelY
/// both tests end at once: the first result's Track is held for 5 s and the second waits in the queue. Then pool
/// modelZ's handler panics. The reports are written only after the stop has drained pool modelY's queue, so both of
/// its results are in them, and the run ends with the panic.
#[test]
fn the_reports_wait_for_a_late_failures_drain_d84() {
    use marathon_core::analytics::{Analytics, NoOpMetricsProvider};
    let (mut factory, script) = script();
    factory.analytics = Some(Box::new(|context| {
        Ok(Analytics::noop(NoOpMetricsProvider::new(false, context)?))
    }));
    factory.configuration.configuration.analytics_configuration =
        marathon_config::AnalyticsConfiguration::Graphite(marathon_config::GraphiteConfiguration {
            host: "carbon.invalid".into(),
            port: None,
            prefix: None,
            defaults: marathon_config::Defaults {
                success_rate: 0.0,
                duration: jcompat::jtime::duration_of(0, 0).unwrap(),
                observed_duration_min_tests: None,
            },
            read_only: false,
        });
    let clock = Arc::new(std::sync::OnceLock::new());
    factory.backend = Some(Arc::new(SlowBackends(clock.clone())));
    let prepare: scenario::Prepare = Box::new(move |core, _| {
        let _ = clock.set(core.vendor_clock());
    });
    let ready: Option<Ready> = Some(Box::new(|built| {
        let channel = built.provider.channel();
        let panicking = SharedDeferred::new();
        let late = |serial: &str, model: &str, prepare: i64| {
            let device = StubDevice::new(StubDeviceOptions {
                serial_number: serial.into(),
                model: model.into(),
                prepare_time_millis: prepare,
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
            Arc::new(hooked(late("serial-3", "modelY", 100))),
            Arc::new(hooked(late("serial-5", "modelY", 100))),
            Arc::new(Hooked {
                worker_panics: true,
                panicking: Some(panicking.clone()),
                ..hooked(late("serial-4", "modelZ", 3_000))
            }),
        ];
        *built.provider.on_terminate.lock().unwrap() = Some(Arc::new(move || {
            for device in &devices {
                let _ = channel.try_send(DeviceEvent::Connected(DeviceRef(device.clone())));
            }
            let panicking = panicking.clone();
            Box::pin(async move {
                let _ = panicking.wait().await;
                Ok(())
            })
        }));
    }));
    let outcome = scenario::drive(factory, script, Some(prepare), ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert_eq!(run_failure(&outcome), [format!("{PROBE}: the pool's handler panicked")]);
    let raw: Value = serde_json::from_str(&outcome.read("test_result/raw.json")).unwrap();
    for serial in ["serial-1", "serial-3", "serial-5"] {
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
