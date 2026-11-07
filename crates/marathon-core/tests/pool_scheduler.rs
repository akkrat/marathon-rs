//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/Scheduler.kt (JVM characterization).
#[path = "support/pool.rs"]
mod support;
use marathon_config::{Configuration, schema::PoolingStrategyConfiguration};
use marathon_core::{
    analytics::Analytics,
    ci::Ci,
    error::*,
    execution::scheduler::{Scheduler, SchedulerParts},
    executor::shared::SharedChannel,
    model::*,
    output::OutputDriver,
    runtime::*,
    time::*,
    vendor::*,
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
};
use support::*;
struct Provider {
    channel: Arc<SharedChannel<DeviceEvent>>,
}
impl DeviceProvider for Provider {
    fn initialize(&self, _: VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn borrow(&self, _: VendorContext) -> VendorFuture<'_, DeviceRef> {
        Box::pin(async { Err(VendorError::Other(ise("unused"))) })
    }
    fn terminate(&self) -> VendorFuture<'_, ()> {
        self.channel.close(None);
        Box::pin(async { Ok(()) })
    }
    fn subscribe(&self) -> Arc<dyn EventChannel<DeviceEvent>> {
        self.channel.clone()
    }
}
type ScheduledResult = Rc<RefCell<Option<(i64, VendorResult<bool>)>>>;
struct Run {
    scheduler: Rc<Scheduler>,
    provider: Arc<Provider>,
    parent: JobRef,
    result: ScheduledResult,
}
impl Run {
    fn new(h: &Harness, c: Configuration) -> Self {
        Self::new_with_tracker(h, c, h.tracker.clone())
    }
    fn new_with_tracker(h: &Harness, c: Configuration, tracker: Arc<dyn marathon_core::wiring::Tracker>) -> Self {
        Self::new_with_shard(h, c, tracker, tests(1, "Scheduler"))
    }
    fn new_with_shard(
        h: &Harness,
        c: Configuration,
        tracker: Arc<dyn marathon_core::wiring::Tracker>,
        shard: Vec<Test>,
    ) -> Self {
        let parent = h.core.job(JobKind::Supervisor, None);
        let provider = Arc::new(Provider {
            channel: h.core.event_channel("core.Scheduler.deviceEvents"),
        });
        let scheduler = Rc::new(Scheduler::new(SchedulerParts {
            core: h.core.clone(),
            provider: provider.clone(),
            analytics: Arc::new(Analytics::from_lookup(Arc::new(NoMetrics))),
            configuration: Arc::new(c),
            shard: TestShard {
                tests: shard,
                flaky_tests: vec![],
            },
            tracker,
            timer: Arc::new(ZeroTimer),
            bundle_identifier: None,
            hooks: h.hooks.clone(),
            barriers: h.barriers.clone(),
            ci: Ci::None,
            output: OutputDriver {
                vendor: h.core.vendor(),
                sink: h.sink.clone(),
            },
            context: h.core.context(parent.clone()),
        }));
        let result = Rc::new(RefCell::new(None));
        let output = result.clone();
        let running = scheduler.clone();
        let clock = h.core.clock();
        h.core.spawn(
            parent.clone(),
            Box::new(move |_| {
                Box::pin(async move {
                    let value = running.execute().await;
                    *output.borrow_mut() = Some((clock.now_millis(), value));
                    Ok(())
                })
            }),
        );
        Self {
            scheduler,
            provider,
            parent,
            result,
        }
    }
    fn connect(&self, d: &Arc<TestDevice>) {
        assert!(
            self.provider
                .channel
                .try_send(DeviceEvent::Connected(d.reference()))
                .is_ok()
        );
    }
    fn disconnect(&self, d: &Arc<TestDevice>) {
        assert!(
            self.provider
                .channel
                .try_send(DeviceEvent::Disconnected(d.reference()))
                .is_ok()
        );
    }
    fn snapshot(&self, h: &Harness) -> Value {
        let result = self.result.borrow();
        let (ended, value) = match result.as_ref() {
            Some((at, Ok(v))) => (json!(at), json!(v)),
            Some((at, Err(e))) => (
                json!(at),
                json!({"exception":e.failure().class.to_string_lossy(),"message":e.failure().message.as_ref().map(|m|m.to_string_lossy())}),
            ),
            None => (Value::Null, Value::Null),
        };
        let names = sorted(
            self.scheduler
                .pools()
                .iter()
                .map(|(id, _)| id.name.to_string_lossy())
                .collect(),
        );
        let logs = h.logs().into_iter().filter(|l| l.logger == "Scheduler").map(|l|
            json!({"t": l.at_millis, "logger": l.logger, "level": level_name(l.level), "message": l.message.to_string_lossy()})
        ).collect::<Vec<_>>();
        json!({"schedulerLogs":logs,"result":value,"ended":ended,"subscriptionAlive":self.parent.children_snapshot().iter().any(|j| matches!(j.state(),JobState::Active)),"pools":names,"connected":lock(&h.tracker.connected).iter().map(|(p,d)|format!("{p}:{d}")).collect::<Vec<_>>(),"disconnected":lock(&h.tracker.disconnected).iter().map(|(p,d)|format!("{p}:{d}")).collect::<Vec<_>>()})
    }
    async fn finish(&self, h: &Harness) {
        self.scheduler.job().cancel(job_was_cancelled());
        self.parent.cancel(job_was_cancelled());
        h.run_current().await;
    }
}
fn oracle(name: &str) -> Value {
    include_str!("../../../fixtures/oracle/pool/scheduler.jsonl")
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .find(|v| v["name"] == name)
        .unwrap_or_else(|| panic!("no JVM row {name}"))
}
fn compare(name: &str, actual: Value) {
    let mut expected = oracle(name);
    expected["schedulerLogs"] = Value::Array(
        expected["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["logger"] == "Scheduler")
            .cloned()
            .collect(),
    );
    for (key, value) in actual.as_object().unwrap() {
        assert_eq!(value, &expected[key], "{name}.{key}");
    }
}
/// D48's allowance, for a JVM row whose pools end with their last device, with tests left. That pool now waits for
/// `deviceInitializationTimeoutMillis`, so the run ends exactly so much later, or at the poll that first saw it, if later.
/// Only `ended` may differ, by exactly this, and everything else stays compared.
fn after_return_wait(mut actual: Value, wait: i64) -> Value {
    assert_eq!(
        actual["ended"],
        json!(wait),
        "D48: the run must end when the wait ends, not sooner or later"
    );
    actual["ended"] = json!(100);
    actual
}
fn configured(timeout: i64, models: bool) -> Configuration {
    let mut c = configuration();
    c.device_initialization_timeout_millis = timeout;
    if models {
        c.pooling_strategy = PoolingStrategyConfiguration::Model;
    }
    c
}
fn device(serial: &str, model: &str, duration: i64, hold_prepare: bool) -> Arc<TestDevice> {
    let execute = on_execute(move |e| async move {
        if duration < 0 {
            return Err(VendorError::Cancelled(e.context.cancellation.cancelled().await));
        }
        e.context.clock.delay(duration, e.context.cancellation.clone()).await?;
        e.result.complete(Ok(e.device.report(&e.batch, TestStatus::Passed)));
        Ok(())
    });
    TestDevice::new_with_model(
        serial,
        model,
        None,
        hold_prepare.then(|| prepare_on(&Gate::default())),
        execute,
    )
}
#[test]
fn no_devices_timeouts_keep_subscription_alive() {
    for timeout in [0, -1, 250] {
        let c = configured(timeout, false);
        scheduling_test(move |h| {
            Box::pin(async move {
                let run = Run::new(&h, c);
                h.advance_until_idle().await;
                compare(&format!("timeout-{timeout}"), run.snapshot(&h));
                run.finish(&h).await;
            })
        });
    }
}
#[test]
fn first_device_is_observed_on_the_hundred_millisecond_poll() {
    for at in [0, 99, 100, 250] {
        let c = configured(1000, false);
        scheduling_test(move |h| {
            Box::pin(async move {
                let run = Run::new(&h, c);
                h.run_current().await;
                replay(h.driver.advance_to(at).await);
                run.connect(&device("serial-A", "test", 0, false));
                h.advance_until_idle().await;
                compare(&format!("first-{at}"), run.snapshot(&h));
                run.finish(&h).await;
            })
        });
    }
}
#[test]
fn terminated_pool_is_not_recreated_but_late_device_is_tracked() {
    let c = configured(1000, false);
    scheduling_test(move |h| {
        Box::pin(async move {
            let run = Run::new(&h, c);
            h.run_current().await;
            let a = device("serial-A", "test", 0, false);
            let b = device("serial-B", "test", 0, false);
            run.connect(&a);
            h.run_current().await;
            replay(h.driver.advance_to(200).await);
            run.connect(&b);
            h.advance_until_idle().await;
            compare("late-connect", run.snapshot(&h));
            assert_eq!(a.batches().len(), 1);
            assert!(b.batches().is_empty());
            run.finish(&h).await;
        })
    });
}
#[test]
fn disconnect_is_broadcast_and_never_tracked() {
    let c = configured(1000, true);
    scheduling_test(move |h| {
        Box::pin(async move {
            let run = Run::new(&h, c);
            h.run_current().await;
            let a = device("serial-A", "A", 0, true);
            let b = device("serial-B", "B", 0, true);
            run.connect(&a);
            run.connect(&b);
            h.run_current().await;
            run.disconnect(&a);
            h.run_current().await;
            run.disconnect(&b);
            h.advance_until_idle().await;
            compare("broadcast", after_return_wait(run.snapshot(&h), 1000));
            let logs = h.logs();
            for pool in ["A", "B"] {
                assert!(logs.iter().any(|l| l.logger == format!("DevicePoolActor[{pool}]")
                    && l.message.to_string_lossy() == "remove device serial-A"));
            }
            run.finish(&h).await;
        })
    });
}
#[test]
fn aggregation_short_circuits_after_the_first_failing_pool() {
    let c = configured(1000, true);
    scheduling_test(move |h| {
        Box::pin(async move {
            let run = Run::new(&h, c);
            h.run_current().await;
            run.connect(&device("serial-A", "A", 0, true));
            run.connect(&device("serial-B", "B", 0, true));
            h.run_current().await;
            for (_, pool) in run.scheduler.pools() {
                assert!(
                    pool.actor
                        .try_send(marathon_core::execution::DevicePoolMessage::FromScheduler(
                            marathon_core::execution::FromScheduler::Terminate
                        ))
                        .is_ok()
                );
            }
            h.advance_until_idle().await;
            compare("short-circuit", run.snapshot(&h));
            let errors = h
                .logs()
                .iter()
                .filter(|l| l.message.to_string_lossy().starts_with("Expected to run"))
                .count();
            assert_eq!(errors, 1);
            run.finish(&h).await;
        })
    });
}
#[test]
fn lazy_child_traversal_joins_a_pool_created_mid_traversal() {
    for _ in 0..50 {
        let c = configured(1000, true);
        scheduling_test(move |h| {
            Box::pin(async move {
                let run = Run::new(&h, c);
                h.run_current().await;
                run.connect(&device("serial-A", "A", 200, false));
                h.run_current().await;
                replay(h.driver.advance_to(150).await);
                run.connect(&device("serial-B", "B", 400, false));
                h.advance_until_idle().await;
                compare("late-pool", run.snapshot(&h));
                run.finish(&h).await;
            })
        });
    }
}
#[test]
fn include_exclude_regexes_match_the_whole_serial() {
    for (i, (includes, excludes)) in [
        (vec![], vec![]),
        (vec!["b"], vec![]),
        (vec!["a.*"], vec![]),
        (vec!["a.*"], vec!["abc"]),
        (vec![], vec!["b"]),
        (vec!["x", "abc"], vec!["x"]),
    ]
    .into_iter()
    .enumerate()
    {
        let mut c = configured(250, false);
        c.include_serial_regexes = includes
            .into_iter()
            .map(|s| jcompat::JavaRegex::compile(s, "include").unwrap())
            .collect();
        c.exclude_serial_regexes = excludes
            .into_iter()
            .map(|s| jcompat::JavaRegex::compile(s, "exclude").unwrap())
            .collect();
        scheduling_test(move |h| {
            Box::pin(async move {
                let run = Run::new(&h, c);
                h.run_current().await;
                let d = device("abc", "test", 0, false);
                run.connect(&d);
                h.run_current().await;
                run.disconnect(&d);
                h.advance_until_idle().await;
                compare(&format!("filter-{i}"), run.snapshot(&h));
                run.finish(&h).await;
            })
        });
    }
}

#[test]
fn external_timeout_preserves_caller_reason_and_cancels_detached_job() {
    scheduling_test(|h| {
        Box::pin(async move {
            let run = Run::new(&h, configured(1000, false));
            h.run_current().await;
            let reason = Cancellation::Timeout {
                scope: 77,
                detail: marathon_core::executor::shared::failure(
                    "kotlinx.coroutines.TimeoutCancellationException",
                    "outer timeout",
                ),
            };
            run.parent.cancel(reason.clone());
            h.run_current().await;
            assert_eq!(*run.result.borrow(), Some((0, Err(VendorError::Cancelled(reason)))));
            assert_eq!(
                run.scheduler.job().state(),
                JobState::Completed(Completion::Cancelled(job_was_cancelled()))
            );
            run.finish(&h).await;
        })
    });
}
thread_local! { static CANCEL_SUBSCRIPTION: RefCell<Option<JobRef>> = const { RefCell::new(None) }; }
struct CancelTracker(Arc<RecordingTracker>);
impl marathon_core::wiring::Tracker for CancelTracker {
    fn device_connected<'a>(&'a self, p: &'a DevicePoolId, d: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        CANCEL_SUBSCRIPTION.with(|j| j.borrow().as_ref().unwrap().cancel(job_was_cancelled()));
        marathon_core::wiring::Tracker::device_connected(self.0.as_ref(), p, d)
    }
    fn device_disconnected<'a>(&'a self, p: &'a DevicePoolId, d: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        marathon_core::wiring::Tracker::device_disconnected(self.0.as_ref(), p, d)
    }
    fn device_provider_init<'a>(
        &'a self,
        s: &'a jcompat::writers::JavaString,
        a: jcompat::jtime::Instant,
        b: jcompat::jtime::Instant,
    ) -> VendorFuture<'a, ()> {
        marathon_core::wiring::Tracker::device_provider_init(self.0.as_ref(), s, a, b)
    }
    fn device_preparing<'a>(
        &'a self,
        s: &'a jcompat::writers::JavaString,
        a: jcompat::jtime::Instant,
        b: jcompat::jtime::Instant,
    ) -> VendorFuture<'a, ()> {
        marathon_core::wiring::Tracker::device_preparing(self.0.as_ref(), s, a, b)
    }
    fn test<'a>(&'a self, p: &'a DevicePoolId, d: &'a DeviceInfo, r: &'a TestResult, f: bool) -> VendorFuture<'a, ()> {
        marathon_core::wiring::Tracker::test(self.0.as_ref(), p, d, r, f)
    }
}
#[test]
fn buffered_disconnect_is_processed_after_cancellation() {
    let c = configured(1000, false);
    scheduling_test(move |h| {
        Box::pin(async move {
            let run = Run::new_with_tracker(&h, c, Arc::new(CancelTracker(h.tracker.clone())));
            CANCEL_SUBSCRIPTION.with(|j| *j.borrow_mut() = Some(run.parent.clone()));
            let d = device("buffered", "test", 0, true);
            run.connect(&d);
            run.disconnect(&d);
            h.run_current().await;
            let processed = h
                .logs()
                .iter()
                .any(|l| l.message.to_string_lossy() == "device buffered disconnected");
            assert!(processed);
            assert_eq!(lock(&h.tracker.connected).len(), 1);
            run.provider.channel.cancel(job_was_cancelled());
            run.finish(&h).await;
            CANCEL_SUBSCRIPTION.with(|j| *j.borrow_mut() = None);
        })
    });
}

struct InvalidSerial(Arc<TestDevice>);
impl Device for InvalidSerial {
    fn info(&self) -> DeviceInfo {
        let mut i = self.0.info();
        i.serial_number = jcompat::writers::JavaString(vec![0xd800]);
        i
    }
    fn abi(&self) -> jcompat::writers::JavaString {
        self.0.abi()
    }
    fn worker_id(&self) -> Option<jcompat::writers::JavaString> {
        self.0.worker_id()
    }
    fn logger(&self) -> &jcompat::logging::Logger {
        self.0.logger()
    }
    fn prepare<'a>(&'a self, c: Arc<Configuration>, x: VendorContext) -> VendorFuture<'a, ()> {
        self.0.prepare(c, x)
    }
    fn execute<'a>(
        &'a self,
        c: Arc<Configuration>,
        p: DevicePoolId,
        b: BatchRef,
        r: Arc<dyn Deferred<TestBatchResults>>,
        x: VendorContext,
    ) -> VendorFuture<'a, ()> {
        self.0.execute(c, p, b, r, x)
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        self.0.dispose()
    }
}

#[test]
fn serial_filters_reject_unpaired_surrogates_for_both_provider_events() {
    for connected in [true, false] {
        scheduling_test(move |h| {
            Box::pin(async move {
                let mut c = configured(1000, false);
                c.include_serial_regexes = vec![jcompat::JavaRegex::compile("\u{fffd}", "include").unwrap()];
                let run = Run::new(&h, c);
                h.run_current().await;
                let children = run.parent.children_snapshot();
                let d = DeviceRef(Arc::new(InvalidSerial(device("actual", "test", 0, true))));
                let event = if connected {
                    DeviceEvent::Connected(d)
                } else {
                    DeviceEvent::Disconnected(d)
                };
                assert!(run.provider.channel.try_send(event).is_ok());
                h.run_current().await;
                assert!(run.scheduler.pools().is_empty());
                assert!(lock(&h.tracker.connected).is_empty());
                let expected = marathon_core::executor::shared::failure(
                    "java.lang.IllegalArgumentException",
                    "Scheduler serial filter: device serial contains unpaired UTF-16 surrogates",
                );
                assert!(
                    children
                        .iter()
                        .any(|job| job.state() == JobState::Completed(Completion::Failed(expected.clone())))
                );
                run.finish(&h).await;
            })
        });
    }
}

// ---- D48: an emptied pool waits for a device to return --------------------------------------------------------------

const WAIT: i64 = 60_000;
fn waiting() -> Configuration {
    let mut c = configured(WAIT, false);
    c.batching_strategy = fixed_size(1);
    c
}
fn waiting_with_models() -> Configuration {
    let mut c = configured(WAIT, true);
    c.batching_strategy = fixed_size(1);
    c
}
fn logged(h: &Harness, text: &str) -> usize {
    h.logs()
        .iter()
        .filter(|l| l.message.to_string_lossy().contains(text))
        .count()
}
/// Runs a test for a long time, so that a device can be taken away in the middle of it.
fn slow(serial: &str, millis: i64) -> Arc<TestDevice> {
    device(serial, "test", millis, false)
}
fn slow_execute(millis: i64) -> OnExecute {
    on_execute(move |e| async move {
        e.context.clock.delay(millis, e.context.cancellation.clone()).await?;
        e.result.complete(Ok(e.device.report(&e.batch, TestStatus::Passed)));
        Ok(())
    })
}
async fn advance(h: &Harness, to: i64) {
    replay(h.driver.advance_to(to).await);
    h.run_current().await;
}
fn ended_at(run: &Run) -> Option<i64> {
    run.result.borrow().as_ref().map(|(at, _)| *at)
}
fn passed(run: &Run) -> bool {
    matches!(run.result.borrow().as_ref(), Some((_, Ok(true))))
}
/// What a second device runs when it takes over at 1500 from one that had just started the second test of four.
/// `alone`: after the pool was empty for 30 s. Otherwise it was in the pool all along, in prepare, and the queue
/// never knew of any wait.
fn order_after_a_takeover(alone: bool) -> Vec<Vec<String>> {
    let order: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let output = order.clone();
    scheduling_test(move |h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, waiting(), h.tracker.clone(), tests(4, "Scheduler"));
            h.run_current().await;
            let first = slow("first", 1000);
            run.connect(&first);
            let gate = Gate::default();
            let second = TestDevice::new("second", None, Some(prepare_on(&gate)), slow_execute(1000));
            if !alone {
                run.connect(&second);
            }
            advance(&h, 1500).await;
            assert_eq!(first.batches().len(), 2);
            run.disconnect(&first);
            advance(&h, 2000).await;
            assert!(ended_at(&run).is_none());
            if alone {
                assert_eq!(
                    logged(&h, "no device left, waiting up to 60000 ms for one to return"),
                    1
                );
                assert_eq!(logged(&h, "remaining tests will not run"), 0);
                advance(&h, 30_000).await;
                assert!(ended_at(&run).is_none());
                run.connect(&second);
            } else {
                assert_eq!(logged(&h, "waiting up to"), 0);
            }
            gate.open();
            h.advance_until_idle().await;
            assert!(passed(&run), "every test ran: {:?}", run.result.borrow());
            assert_eq!(logged(&h, "remaining tests will not run"), 0);
            let start = if alone { 30_000 } else { 2000 };
            assert_eq!(ended_at(&run), Some(start + 3000), "no wait left behind");
            *lock(&output) = second.batches();
            run.finish(&h).await;
        })
    });
    let taken = lock(&order).clone();
    drop(order);
    taken
}

#[test]
fn a_device_that_returns_carries_on_with_the_tests_in_their_order_d48() {
    let control = order_after_a_takeover(false);
    assert_eq!(control.len(), 3);
    assert_eq!(order_after_a_takeover(true), control);
}
#[test]
fn a_run_ends_when_no_device_returns_d48() {
    scheduling_test(|h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, waiting(), h.tracker.clone(), tests(4, "Scheduler"));
            h.run_current().await;
            let only = slow("only", 1000);
            run.connect(&only);
            advance(&h, 1500).await;
            run.disconnect(&only);
            advance(&h, 1500 + WAIT - 1).await;
            assert!(ended_at(&run).is_none(), "not a moment before its limit");
            h.advance_until_idle().await;
            assert_eq!(ended_at(&run), Some(1500 + WAIT));
            assert!(matches!(run.result.borrow().as_ref(), Some((_, Ok(false)))));
            assert_eq!(logged(&h, "remaining tests will not run"), 1);
            assert_eq!(only.batches().len(), 2);
            run.finish(&h).await;
        })
    });
}
#[test]
fn a_device_after_the_limit_is_dropped_and_still_tracked_d48() {
    scheduling_test(|h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, waiting(), h.tracker.clone(), tests(2, "Scheduler"));
            h.run_current().await;
            let only = slow("only", 1000);
            run.connect(&only);
            advance(&h, 500).await;
            run.disconnect(&only);
            advance(&h, 500 + WAIT).await;
            assert_eq!(ended_at(&run), Some(500 + WAIT));
            let late = slow("only", 1000);
            run.connect(&late);
            h.advance_until_idle().await;
            assert!(late.batches().is_empty(), "C50 stands after the limit");
            assert_eq!(lock(&h.tracker.connected).len(), 2);
            run.finish(&h).await;
        })
    });
}
#[test]
fn a_pool_with_nothing_left_to_run_does_not_wait_d48() {
    scheduling_test(|h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, waiting(), h.tracker.clone(), tests(1, "Scheduler"));
            h.run_current().await;
            // The test ends at 1000, and the device goes in the same instant as the batch is reported.
            let only = slow("only", 1000);
            run.connect(&only);
            advance(&h, 1000).await;
            run.disconnect(&only);
            h.advance_until_idle().await;
            assert!(passed(&run));
            assert_eq!(ended_at(&run), Some(1000));
            assert_eq!(logged(&h, "waiting up to"), 0);
            run.finish(&h).await;
        })
    });
}
/// The queue is alive here, having been asked for nothing, and answers that it is empty.
#[test]
fn a_pool_with_no_tests_does_not_wait_d48() {
    scheduling_test(|h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, waiting(), h.tracker.clone(), vec![]);
            h.run_current().await;
            let only = TestDevice::new("only", None, Some(prepare_on(&Gate::default())), report_passed());
            run.connect(&only);
            advance(&h, 500).await;
            run.disconnect(&only);
            h.advance_until_idle().await;
            assert_eq!(ended_at(&run), Some(500));
            assert_eq!(logged(&h, "waiting up to"), 0);
            run.finish(&h).await;
        })
    });
}
#[test]
fn the_last_test_coming_back_with_no_retries_left_does_not_wait_d48() {
    let mut c = waiting();
    c.uncompleted_test_retry_quota = 0;
    scheduling_test(move |h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, c, h.tracker.clone(), tests(1, "Scheduler"));
            h.run_current().await;
            let only = slow("only", 1000);
            run.connect(&only);
            advance(&h, 500).await;
            run.disconnect(&only);
            h.advance_until_idle().await;
            assert_eq!(
                ended_at(&run),
                Some(500),
                "the queue is finished, so there is nothing to wait for"
            );
            assert_eq!(logged(&h, "waiting up to"), 0);
            run.finish(&h).await;
        })
    });
}
#[test]
fn a_device_that_keeps_returning_without_finishing_a_test_cannot_hold_the_run_open_d48() {
    let mut c = waiting();
    c.uncompleted_test_retry_quota = i32::MAX;
    scheduling_test(move |h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, c, h.tracker.clone(), tests(2, "Scheduler"));
            h.run_current().await;
            let mut now = 0;
            let mut cycles = 0;
            let mut device_now = slow("flapping", 1_000_000);
            run.connect(&device_now);
            while ended_at(&run).is_none() {
                assert!(cycles < 1000, "the run did not end");
                now += 5000;
                advance(&h, now).await;
                run.disconnect(&device_now);
                now += 5000;
                advance(&h, now).await;
                device_now = slow("flapping", 1_000_000);
                run.connect(&device_now);
                cycles += 1;
            }
            // Its time with the device counts too: the limit is measured from the first time it went away.
            assert!(
                ended_at(&run).unwrap() <= 5000 + WAIT + 5000,
                "ended at {:?}",
                ended_at(&run)
            );
            assert!(cycles >= 2, "returns were accepted before the limit, {cycles}");
            assert_eq!(logged(&h, "remaining tests will not run"), 1);
            h.advance_until_idle().await;
            run.finish(&h).await;
        })
    });
}
#[test]
fn a_test_that_ends_gives_the_pool_its_whole_wait_again_d48() {
    scheduling_test(|h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, waiting(), h.tracker.clone(), tests(4, "Scheduler"));
            h.run_current().await;
            let first = slow("same", 1000);
            run.connect(&first);
            advance(&h, 1500).await;
            run.disconnect(&first);
            advance(&h, 1500 + WAIT - 100).await;
            let second = slow("same", 1000);
            run.connect(&second);
            // One test ends a second later: this pool works, and its next loss has the full wait.
            advance(&h, 1500 + WAIT + 1500).await;
            assert_eq!(second.batches().len(), 2);
            run.disconnect(&second);
            let lost = 1500 + WAIT + 1500;
            advance(&h, lost + WAIT - 1).await;
            assert!(ended_at(&run).is_none(), "the second wait is whole");
            assert_eq!(logged(&h, "waiting up to 60000 ms"), 2);
            h.advance_until_idle().await;
            assert_eq!(ended_at(&run), Some(lost + WAIT));
            run.finish(&h).await;
        })
    });
}
#[test]
fn a_device_failing_prepare_after_it_returned_uses_what_is_left_of_the_wait_d48() {
    scheduling_test(|h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, waiting(), h.tracker.clone(), tests(2, "Scheduler"));
            h.run_current().await;
            let first = slow("same", 1000);
            run.connect(&first);
            advance(&h, 500).await;
            run.disconnect(&first);
            advance(&h, 10_500).await;
            let second = TestDevice::new(
                "same",
                None,
                Some(on_prepare(|_| async {
                    Err(VendorError::LeavePool {
                        reason: LeavePoolReason::NotBooted,
                        detail: ise("still not booted"),
                    })
                })),
                report_passed(),
            );
            run.connect(&second);
            h.run_current().await;
            assert!(second.batches().is_empty());
            assert_eq!(logged(&h, "waiting up to 50000 ms"), 1, "what the first wait left");
            h.advance_until_idle().await;
            assert_eq!(ended_at(&run), Some(500 + WAIT));
            run.finish(&h).await;
        })
    });
}
#[test]
fn a_pool_created_during_a_wait_is_joined_and_the_run_waits_for_both_d48() {
    scheduling_test(|h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, waiting_with_models(), h.tracker.clone(), tests(2, "Scheduler"));
            h.run_current().await;
            let a = device("serial-A", "A", 1000, false);
            run.connect(&a);
            advance(&h, 500).await;
            run.disconnect(&a);
            advance(&h, 1000).await;
            // Pool B arrives while the scheduler is joining pool A, and outlives A's wait.
            let b = device("serial-B", "B", 100_000, false);
            run.connect(&b);
            advance(&h, 500 + WAIT).await;
            assert_eq!(logged(&h, "remaining tests will not run"), 1, "pool A ended");
            assert!(ended_at(&run).is_none(), "the run joins pool B too");
            h.advance_until_idle().await;
            assert_eq!(ended_at(&run), Some(1000 + 2 * 100_000));
            assert_eq!(b.batches().len(), 2);
            // Pool A's tests did not run.
            assert!(matches!(run.result.borrow().as_ref(), Some((_, Ok(false)))));
            run.finish(&h).await;
        })
    });
}
#[test]
fn pools_wait_on_their_own_and_a_healthy_one_carries_on_d48() {
    scheduling_test(|h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, waiting_with_models(), h.tracker.clone(), tests(3, "Scheduler"));
            h.run_current().await;
            let a = device("serial-A", "A", 1000, false);
            let b = device("serial-B", "B", 1000, false);
            run.connect(&a);
            run.connect(&b);
            advance(&h, 500).await;
            run.disconnect(&a);
            advance(&h, 3000).await;
            // B has done everything, its pool is over. A's still waits, and B's end didn't affect it.
            assert_eq!(b.batches().len(), 3);
            assert_eq!(logged(&h, "waiting up to 60000 ms"), 1);
            assert!(ended_at(&run).is_none());
            let again = device("serial-A", "A", 1000, false);
            run.connect(&again);
            h.advance_until_idle().await;
            assert_eq!(again.batches().len(), 3);
            assert!(passed(&run));
            run.finish(&h).await;
        })
    });
}
#[test]
fn another_pools_disconnect_does_not_restart_or_end_a_wait_d48() {
    scheduling_test(|h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, waiting_with_models(), h.tracker.clone(), tests(2, "Scheduler"));
            h.run_current().await;
            let a = device("serial-A", "A", 1000, false);
            let b = device("serial-B", "B", 1_000_000, false);
            run.connect(&a);
            run.connect(&b);
            advance(&h, 500).await;
            run.disconnect(&a);
            advance(&h, 10_000).await;
            run.disconnect(&b);
            advance(&h, 500 + WAIT).await;
            assert_eq!(logged(&h, "no device left, waiting"), 2, "one wait per pool");
            assert_eq!(
                logged(&h, "remaining tests will not run"),
                1,
                "A's wait ran from its own start"
            );
            h.advance_until_idle().await;
            assert_eq!(ended_at(&run), Some(10_000 + WAIT));
            run.finish(&h).await;
        })
    });
}
#[test]
fn what_the_vendor_already_waited_counts_against_the_limit_d48() {
    for (used, ended) in [
        (20_000, 500 + WAIT - 20_000),
        (WAIT, 500),
        (WAIT + 1, 500),
        (i64::MAX, 500),
    ] {
        scheduling_test(move |h| {
            Box::pin(async move {
                let run = Run::new_with_shard(&h, waiting(), h.tracker.clone(), tests(2, "Scheduler"));
                h.run_current().await;
                let only = slow("only", 1000);
                only.set_return_wait_used(used);
                run.connect(&only);
                advance(&h, 500).await;
                run.disconnect(&only);
                h.advance_until_idle().await;
                assert_eq!(ended_at(&run), Some(ended), "used {used}");
                assert!(matches!(run.result.borrow().as_ref(), Some((_, Ok(false)))));
                run.finish(&h).await;
            })
        });
    }
}
#[test]
fn a_cancelled_run_does_not_wait_out_the_limit_d48() {
    scheduling_test(|h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, waiting(), h.tracker.clone(), tests(2, "Scheduler"));
            h.run_current().await;
            let only = slow("only", 1000);
            run.connect(&only);
            advance(&h, 500).await;
            run.disconnect(&only);
            advance(&h, 1000).await;
            let reason = job_was_cancelled();
            run.parent.cancel(reason.clone());
            h.run_current().await;
            assert_eq!(ended_at(&run), Some(1000));
            assert!(matches!(
                run.result.borrow().as_ref(),
                Some((_, Err(VendorError::Cancelled(_))))
            ));
            run.finish(&h).await;
        })
    });
}
#[test]
fn a_late_return_wait_timer_is_ignored_d48() {
    scheduling_test(|h| {
        Box::pin(async move {
            let run = Run::new_with_shard(&h, waiting(), h.tracker.clone(), tests(2, "Scheduler"));
            h.run_current().await;
            let first = slow("same", 1000);
            run.connect(&first);
            advance(&h, 500).await;
            run.disconnect(&first);
            advance(&h, 1000).await;
            let second = slow("same", 1000);
            run.connect(&second);
            h.run_current().await;
            // What a timer that fired just before the return leaves in the mailbox.
            let (_, pool) = run.scheduler.pools().remove(0);
            assert!(
                pool.actor
                    .try_send(marathon_core::execution::DevicePoolMessage::ReturnWaitOver(1))
                    .is_ok()
            );
            h.advance_until_idle().await;
            assert!(passed(&run));
            run.finish(&h).await;
        })
    });
}
