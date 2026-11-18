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
fn fail_while_running(after_cancel: AfterCancel, terminate: impl Fn() + Send + Sync + 'static) -> Option<Ready> {
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
    let ready = fail_while_running(AfterCancel::CleansUp(3_000, cleaned.clone()), move || {
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

/// D84, review round 1: a cancelled batch whose call only ends when the provider is terminated cannot hold the run for
/// ever. The failure is logged when it happens; the run waits D52's limit (24 s) for the devices, then finishes, and its
/// termination releases the call.
#[test]
fn a_device_call_waiting_for_the_runs_end_holds_it_only_for_a_limited_time_d84() {
    let (factory, script) = script();
    let released = SharedDeferred::new();
    let release = released.clone();
    let ready = fail_while_running(AfterCancel::WaitsFor(released.clone()), move || {
        release.complete(Ok(()));
    });
    let outcome = scenario::drive(factory, script, None, ready);
    assert!(outcome.completed, "the run did not end: {:?}", outcome.lines);
    assert_eq!(outcome.completed_at, Some(3_000 + 24_000));
    assert!(released.completed().is_some());
    assert!(logged(
        &outcome,
        Level::ERROR,
        "The run's own machinery failed; stopping the run"
    ));
    assert!(logged(
        &outcome,
        Level::ERROR,
        "The pools and devices did not stop within 24 s of the failure; finishing the run anyway"
    ));
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
