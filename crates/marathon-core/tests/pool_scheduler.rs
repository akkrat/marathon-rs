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
use std::{cell::RefCell, rc::Rc, sync::Arc};
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
                tests: tests(1, "Scheduler"),
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
            compare("broadcast", run.snapshot(&h));
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
