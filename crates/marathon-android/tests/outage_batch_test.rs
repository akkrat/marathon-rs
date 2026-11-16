//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/AdamAndroidDevice.kt and
//! AndroidDeviceTestRunner.kt: D70, batches held by an adb server outage or a grace period (D22, D41). Kotlin has neither,
//! so these have no Kotlin counterpart.
//!
//! The vendor clock does not move, so a grace period stays open until the test ends it by the device's own state. Whole
//! runs, where the core gives a held batch back, are in `reconnect_test.rs`.
use marathon_adb::{server::FakeAdbServer, testrunner::TestEvent};
mod support;
use marathon_android::{
    device::{AndroidBatchExecutor, AndroidDevice},
    listener_factory::{AndroidListenerFactory, InstalledListener, ListenerSlots},
    listeners::AndroidTestRunListener,
    provider::AdamDeviceProvider,
    runner::AndroidRunnerExecutor,
    services::AndroidRuntime,
};
use marathon_config::Configuration;
use marathon_core::{
    error::*,
    executor::{
        Signal,
        shared::{SharedDeferred, requested},
    },
    model::*,
    runtime::{Deferred, VendorFuture},
    vendor::{DeviceEvent, DeviceProvider, DeviceRef, VendorContext},
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::*;

const SERIAL: &str = "emulator-5554";
type Scenario = std::pin::Pin<Box<dyn std::future::Future<Output = VendorResult<()>> + Send>>;

async fn until(mut condition: impl FnMut() -> bool) {
    checked(
        tokio::time::timeout(Duration::from_secs(60), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await,
    );
}
async fn connected(provider: &AdamDeviceProvider) -> DeviceRef {
    let channel = provider.subscribe();
    let event = checked(checked(
        tokio::time::timeout(Duration::from_secs(60), channel.receive()).await,
    ));
    let Some(DeviceEvent::Connected(device)) = event else {
        panic!("connection")
    };
    device
}
fn in_grace(device: &DeviceRef) -> bool {
    !device.0.info().healthy
}
fn runtime_with(
    runtime: &Arc<AndroidRuntime>,
    ports: &[u16],
    executor: Arc<dyn AndroidBatchExecutor>,
) -> Arc<AndroidRuntime> {
    let changed = change_runtime(runtime, ports);
    Arc::new(AndroidRuntime {
        dependencies: changed.dependencies.clone(),
        configuration: changed.configuration.clone(),
        file_sync: changed.file_sync.clone(),
        bundles: changed.bundles.clone(),
        identifier: changed.identifier.clone(),
        executor,
        log: changed.log.clone(),
    })
}
fn batch(id: &str, tests: Vec<Test>) -> BatchRef {
    Arc::new(TestBatch { tests, id: id.into() })
}
fn execute(
    device: &DeviceRef,
    configuration: Arc<Configuration>,
    batch: &BatchRef,
    result: Arc<Deferred<TestBatchResults>>,
    context: &VendorContext,
) -> tokio::task::JoinHandle<VendorResult<()>> {
    let (device, batch, context) = (device.clone(), batch.clone(), context.clone());
    tokio::spawn(async move {
        device
            .0
            .execute(
                configuration,
                DevicePoolId { name: "pool".into() },
                batch,
                result,
                context,
            )
            .await
    })
}
async fn outcome(running: tokio::task::JoinHandle<VendorResult<()>>) -> VendorResult<()> {
    checked(checked(tokio::time::timeout(Duration::from_secs(60), running).await))
}

/// A batch that records its submission as the runner does, once let through.
#[derive(Default)]
struct Submitting {
    entered: AtomicUsize,
    proceed: tokio::sync::Notify,
    submitted: AtomicUsize,
}
impl AndroidBatchExecutor for Submitting {
    fn execute<'a>(
        &'a self,
        device: &'a AndroidDevice,
        _: Arc<Configuration>,
        _: DevicePoolId,
        batch: BatchRef,
        result: Arc<Deferred<TestBatchResults>>,
        _: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.entered.fetch_add(1, Ordering::SeqCst);
            self.proceed.notified().await;
            device.record_submission(&batch)?;
            self.submitted.fetch_add(1, Ordering::SeqCst);
            result.complete(Ok(TestBatchResults {
                device: device.reference(),
                finished: vec![],
                failed: vec![],
                uncompleted: vec![],
            }));
            Ok(())
        })
    }
}

/// D70 (U160): the core may give a batch back uncharged only while no instrumentation of it has been submitted, and once
/// it has, the batch never starts. Whichever comes first, the submission or the core's withdrawal, exactly one holds.
#[test]
fn a_batch_given_back_before_it_starts_never_starts_d70() {
    run_with_stopped_clock(configuration(), |runtime, context| {
        Box::pin(async move {
            let mut server = server(base_script(27, vec![])).await;
            let submitting = Arc::new(Submitting::default());
            let provider = AdamDeviceProvider::new(runtime_with(&runtime, &[server.port()], submitting.clone()))?;
            provider.initialize(context.clone()).await?;
            let device = connected(&provider).await;
            logcat_streams(&[(&server, 1)]).await;
            let configuration = runtime.dependencies.configuration.clone();
            let entered = |count: usize| {
                let submitting = submitting.clone();
                async move { until(|| submitting.entered.load(Ordering::SeqCst) >= count).await }
            };

            // Held at the gate by a grace period, and given back there: it does not reach the executor once released.
            checked(server.set_device_state(SERIAL, "offline"));
            until(|| in_grace(&device)).await;
            let held = batch("held", vec![]);
            let running = execute(&device, configuration.clone(), &held, SharedDeferred::new(), &context);
            assert!(device.0.withdraw_unstarted(&held));
            checked(server.set_device_state(SERIAL, "device"));
            assert!(matches!(outcome(running).await, Err(VendorError::Cancelled(_))));
            until(|| !in_grace(&device)).await;
            assert_eq!(submitting.entered.load(Ordering::SeqCst), 0);

            // Given back while the executor is on its way to the submission, which is then refused.
            let early = batch("early", vec![]);
            let running = execute(&device, configuration.clone(), &early, SharedDeferred::new(), &context);
            entered(1).await;
            assert!(device.0.withdraw_unstarted(&early));
            submitting.proceed.notify_one();
            assert!(matches!(outcome(running).await, Err(VendorError::Cancelled(_))));
            assert_eq!(submitting.submitted.load(Ordering::SeqCst), 0);

            // Submitted first: the core cannot take it back, and keeps the batch's own result.
            let late = batch("late", vec![]);
            let result = SharedDeferred::new();
            let running = execute(&device, configuration.clone(), &late, result.clone(), &context);
            entered(2).await;
            submitting.proceed.notify_one();
            until(|| submitting.submitted.load(Ordering::SeqCst) == 1).await;
            assert!(!device.0.withdraw_unstarted(&late));
            assert!(outcome(running).await.is_ok());
            assert!(matches!(result.completed(), Some(Ok(_))));
            // Nor once it has run.
            assert!(!device.0.withdraw_unstarted(&late));
            // The device does not keep the result, which holds the device: neither would ever be freed.
            let cell = Arc::downgrade(&result);
            drop(result);
            assert!(cell.upgrade().is_none(), "the device keeps the batch's result");

            // Given back before `execute` was even called, as the core can when the device leaves at once.
            let unseen = batch("unseen", vec![]);
            assert!(device.0.withdraw_unstarted(&unseen));
            let running = execute(&device, configuration.clone(), &unseen, SharedDeferred::new(), &context);
            assert!(matches!(outcome(running).await, Err(VendorError::Cancelled(_))));
            assert_eq!(submitting.entered.load(Ordering::SeqCst), 2);
            provider.terminate().await?;
            checked(server.shutdown().await);
            Ok(())
        })
    });
}

fn runner_configuration() -> Configuration {
    let mut c = configuration();
    let a = android(&mut c);
    a.application_pm_clear = false;
    a.test_application_pm_clear = false;
    a.screen_record_configuration.video_configuration.enabled = false;
    a.screen_record_configuration.screenshot_configuration.enabled = false;
    a.test_application_output = Some(PathBuf::from(
        fixture("vendor-android/apk/app-debug-androidTest.apk")
            .to_string_lossy()
            .into_owned(),
    ));
    c
}
fn test(method: &str) -> Test {
    Test {
        pkg: "pkg".into(),
        clazz: "Clazz".into(),
        method: method.into(),
        meta_properties: vec![],
    }
}
/// `one` runs under the main test APK's instrumentation, `two` under another one's.
fn two_instrumentations(runtime: &Arc<AndroidRuntime>) -> (Test, Test) {
    let (one, two) = (test("one"), test("two"));
    runtime.identifier.put(&one, &runtime.bundles[0]);
    let second = Arc::new(marathon_android::models::AndroidTestBundle::new(
        marathon_android::dex_test_parser::DexTestBundleFiles {
            application: None,
            test_application: PathBuf::from(
                fixture("dex/mutants/manifest-relative-bare.apk")
                    .to_string_lossy()
                    .into_owned(),
            ),
            extra_applications: vec![],
            split_apks: vec![],
        },
        &PathBuf::from("/"),
    ));
    runtime.identifier.put(&two, &second);
    (one, two)
}
fn completes(method: &str) -> String {
    format!(
        "INSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test={method}\nINSTRUMENTATION_STATUS: numtests=1\nINSTRUMENTATION_STATUS_CODE: 1\nINSTRUMENTATION_STATUS: class=pkg.Clazz\nINSTRUMENTATION_STATUS: test={method}\nINSTRUMENTATION_STATUS_CODE: 0\nINSTRUMENTATION_CODE: -1\n"
    )
}
fn instrumentations(server: &FakeAdbServer) -> Vec<String> {
    commands(server)
        .into_iter()
        .filter(|s| s.starts_with("am instrument"))
        .collect()
}
fn names(results: &[TestResult]) -> Vec<String> {
    results.iter().map(|r| r.test.method.to_owned()).collect()
}

/// Puts the device into a grace period as the first instrumentation's run ends, before the runner gets to the second, and
/// tells when the runner stops at the gate, which is when it takes the results so far.
struct Blip {
    server: Arc<FakeAdbServer>,
    device: std::sync::Mutex<Option<DeviceRef>>,
    fired: AtomicBool,
    parked: AtomicBool,
}
impl AndroidTestRunListener for Blip {
    fn on_event<'a>(&'a self, event: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if matches!(event, TestEvent::TestRunEnded { .. }) && !self.fired.swap(true, Ordering::SeqCst) {
                checked(self.server.set_device_state(SERIAL, "offline"));
                let device = self.device.lock().unwrap().clone().unwrap();
                until(|| in_grace(&device)).await;
            }
            Ok(())
        })
    }
    fn results_so_far(&self) -> VendorResult<Option<TestBatchResults>> {
        self.parked.store(true, Ordering::SeqCst);
        Ok(None)
    }
}
struct BlipFactory(Arc<Blip>);
impl AndroidListenerFactory for BlipFactory {
    fn create(
        &self,
        _: &AndroidDevice,
        _: &Configuration,
        _: &DevicePoolId,
        _: &BatchRef,
    ) -> VendorResult<InstalledListener> {
        Ok(InstalledListener {
            listener: self.0.clone(),
            attachments: None,
        })
    }
}

/// A batch of two instrumentations whose device goes into a grace period between them, with the device, its fake server
/// and the batch's result. `scenario` gets them once the runner is waiting at the gate for the second one.
fn between_instrumentations(
    scenario: impl FnOnce(
        Arc<FakeAdbServer>,
        DeviceRef,
        BatchRef,
        Arc<Deferred<TestBatchResults>>,
        tokio::task::JoinHandle<VendorResult<()>>,
        Arc<Signal>,
    ) -> Scenario
    + Send
    + 'static,
) {
    run_with_stopped_clock(runner_configuration(), |runtime, context| {
        Box::pin(async move {
            let mut script = base_script(27, vec![]);
            for method in ["one", "two"] {
                script["shell"].as_array_mut().unwrap().push(json!({
                    "regex": format!("^am instrument.* -e class pkg.Clazz#{method} "),
                    "stdout": completes(method),
                }));
            }
            let server = Arc::new(server(script).await);
            let blip = Arc::new(Blip {
                server: server.clone(),
                device: Default::default(),
                fired: AtomicBool::new(false),
                parked: AtomicBool::new(false),
            });
            let executor = Arc::new(AndroidRunnerExecutor {
                slots: ListenerSlots {
                    recorder: Some(Arc::new(BlipFactory(blip.clone()))),
                    ..ListenerSlots::default()
                },
            });
            let provider = AdamDeviceProvider::new(runtime_with(&runtime, &[server.port()], executor))?;
            provider.initialize(context.clone()).await?;
            let device = connected(&provider).await;
            logcat_streams(&[(&server, 1)]).await;
            *blip.device.lock().unwrap() = Some(device.clone());
            let android = provider.android_device(&device).unwrap();
            let (one, two) = two_instrumentations(android.runtime());
            let batch = batch("batch", vec![one, two]);
            let result = SharedDeferred::new();
            let signal = Signal::new();
            let running = execute(
                &device,
                runtime.dependencies.configuration.clone(),
                &batch,
                result.clone(),
                &VendorContext {
                    cancellation: signal.clone(),
                    ..context.clone()
                },
            );
            // The runner reaches the gate, unless it ran on through it. The listener hears of the gate just before the
            // device records the batch as parked there, so this waits for the record too.
            checked(
                tokio::time::timeout(Duration::from_secs(60), async {
                    while !(blip.parked.load(Ordering::SeqCst) && android.is_parked(&batch))
                        && !running.is_finished()
                        && instrumentations(&server).len() < 2
                    {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await,
            );
            assert!(!running.is_finished(), "the second instrumentation did not wait");
            assert_eq!(instrumentations(&server).len(), 1);
            scenario(server.clone(), device, batch, result, running, signal).await?;
            provider.terminate().await?;
            // The executor's listener factory holds the server too; it stops when the last handle goes.
            Ok(())
        })
    });
}

/// D70 (U166): D28 runs each instrumentation of a batch in turn, and each waits for D22's and D41's gate, not only the
/// first. The second one waits for the device's grace period to end and then runs, so the batch passes whole.
#[test]
fn each_instrumentation_of_a_batch_waits_for_the_device_d70() {
    between_instrumentations(|server, device, _batch, result, running, _| {
        Box::pin(async move {
            checked(server.set_device_state(SERIAL, "device"));
            outcome(running).await?;
            until(|| !in_grace(&device)).await;
            let commands = instrumentations(&server);
            assert_eq!(commands.len(), 2);
            assert!(commands[1].contains(" -e class pkg.Clazz#two "), "{}", commands[1]);
            let results = result.completed().unwrap().unwrap();
            assert_eq!(names(&results.finished), ["one", "two"]);
            assert!(results.failed.is_empty() && results.uncompleted.is_empty());
            Ok(())
        })
    });
}

/// D70: a batch given up between its instrumentations ends with what the first gave: `one` passed, and only `two`, which
/// never started, is uncompleted, as when the device is lost there. Giving the whole batch back would run `one` again.
/// The second instrumentation never starts.
#[test]
fn a_batch_given_up_between_instrumentations_keeps_what_the_first_gave_d70() {
    between_instrumentations(|server, device, batch, result, running, _| {
        Box::pin(async move {
            assert!(!device.0.withdraw_unstarted(&batch));
            let results = result.completed().expect("completed with the results so far").unwrap();
            assert_eq!(names(&results.finished), ["one"]);
            assert_eq!(names(&results.uncompleted), ["two"]);
            assert!(results.failed.is_empty());
            // The core then stops the device; nothing more of the batch runs, released or not.
            checked(server.set_device_state(SERIAL, "device"));
            let outcome = outcome(running).await;
            assert!(
                matches!(outcome, Err(VendorError::Cancelled(_))),
                "{:?}",
                outcome.map_err(|e| e.failure().clone())
            );
            assert_eq!(instrumentations(&server).len(), 1);
            // Completed from the device's record, which does not keep it: the result holds the device.
            let cell = Arc::downgrade(&result);
            drop((results, result));
            assert!(cell.upgrade().is_none(), "the device keeps the batch's result");
            Ok(())
        })
    });
}

/// D52: a run cancelled while an instrumentation waits at the gate ends there. The batch's run is closed with what the
/// first instrumentation gave, and the second never starts.
#[test]
fn a_cancel_ends_an_instrumentation_waiting_at_the_gate_d70() {
    between_instrumentations(|server, _device, _batch, result, running, signal| {
        Box::pin(async move {
            signal.cancel(requested("test"));
            assert!(matches!(outcome(running).await, Err(VendorError::Cancelled(_))));
            let results = result.completed().unwrap().unwrap();
            assert_eq!(names(&results.finished), ["one"]);
            assert_eq!(names(&results.uncompleted), ["two"]);
            assert_eq!(instrumentations(&server).len(), 1);
            Ok(())
        })
    });
}
