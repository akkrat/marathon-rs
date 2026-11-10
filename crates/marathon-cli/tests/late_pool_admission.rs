//! Port of core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt,
//! execution/Scheduler.kt and cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt.
//! D65: manually poll runAsync to hold its final-join continuation while actual
//! actor/vendor work drains. The CLI maps the real result and standard reporters
//! write real files through the same console as CLI startup and exit messages.
use marathon_cli::{
    application::{CliOutput, CliRunner, Console, MarathonRun, Process},
    core_output::RunConsoleSink,
    run_cli,
};
use marathon_config::{Configuration, VendorConfiguration, schema::PoolingStrategyConfiguration};
use marathon_core::{
    analytics::Analytics,
    error::*,
    executor::{CoreExecutor, SharedDeferred, shared::requested},
    model::{ExecutionCommand, Test},
    runtime::*,
    strategy::MetricsLookup,
    time::ReplayDriver,
    vendor::DeviceEvent,
};
use marathon_logging::{LogSettings, OutputTarget};
use marathon_test_support::{
    factory::{FakeTimer, MarathonFactory},
    scenario,
    stub_device::{Recorder, StubDevice, StubDeviceOptions},
};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

fn replay_error(error: marathon_core::time::ReplayError) -> OperationError {
    Failure::from_kind(
        FailureKind::ReplayClock,
        Some(format!("{}: {}", error.label, error.message)),
    )
    .into()
}
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        lock(&self.0).extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Buffer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&lock(&self.0)).into_owned()
    }
}
type Gate = Arc<SharedDeferred<()>>;
struct NoHistory;
impl MetricsLookup for NoHistory {
    fn success_rate(&self, _: &Test, _: jcompat::jtime::Instant) -> f64 {
        0.0
    }
    fn execution_time(&self, _: &Test, _: f64, _: jcompat::jtime::Instant) -> f64 {
        0.0
    }
}
struct Runner {
    admitted: bool,
    fails: bool,
}
struct Run {
    factory: MarathonFactory,
    admitted: bool,
    fails: bool,
}
impl CliRunner for Runner {
    fn construct(
        &self,
        configuration: Arc<Configuration>,
        logging: Arc<Mutex<LogSettings>>,
        console: Arc<Console>,
    ) -> Result<Box<dyn MarathonRun>, Failure> {
        let mut factory = MarathonFactory::new();
        factory.configuration.configuration = (*configuration).clone();
        // The CLI binding/exit path is real; only the vendor is substituted.
        factory.configuration.configuration.vendor_configuration = VendorConfiguration::Stub;
        factory.configuration.configuration.pooling_strategy = PoolingStrategyConfiguration::Model;
        factory.tests = vec![scenario::test("pkg", "Admission", "test")];
        factory.logging = logging.clone();
        factory.console_sink = Some(Arc::new(RunConsoleSink::new(console, logging, Box::new(|_| 0))));
        factory.timer = Some(Arc::new(FakeTimer(Arc::new(|| 0))));
        Ok(Box::new(Run {
            factory,
            admitted: self.admitted,
            fails: self.fails,
        }))
    }
}
fn scripted(serial: &str, model: &str, recorder: &Recorder) -> Arc<StubDevice> {
    let device = StubDevice::new(StubDeviceOptions {
        serial_number: serial.into(),
        model: model.into(),
        scripted: true,
        ..StubDeviceOptions::default()
    });
    device.record_to(recorder.clone());
    device
}
fn finish(device: &StubDevice, fails: bool) {
    assert_eq!(
        device.deliver(&json!({"event":"batch-result", "results":[{
            "test":"pkg.Admission#test", "status":if fails { "FAILURE" } else { "PASSED" },
            "start":0, "end":1
        }]})),
        None
    );
}
fn called(recorder: &Recorder, device: &str, call: &str) -> bool {
    lock(&recorder.log)
        .iter()
        .any(|event| event["device"] == device && event["call"] == call)
}
impl MarathonRun for Run {
    fn run(self: Box<Self>, command: ExecutionCommand) -> Result<bool, Failure> {
        let Run {
            mut factory,
            admitted,
            fails,
        } = *self;
        // Keep temporary-directory cleanup on the calling CLI thread.
        let _factory_output = factory.configuration.output.take();
        let result = CoreExecutor::run_virtual(move |core, driver| {
            Box::pin(async move {
                let entered: Gate = SharedDeferred::new();
                let release: Gate = SharedDeferred::new();
                let entered_prefetch = entered.clone();
                let release_prefetch = release.clone();
                factory.analytics = Some(Box::new(move |_| {
                    let calls = AtomicUsize::new(0);
                    Ok(Analytics::from_gated_lookup(
                        Arc::new(NoHistory),
                        Arc::new(move || {
                            let second = calls.fetch_add(1, Ordering::SeqCst) == 1;
                            let entered = entered_prefetch.clone();
                            let release = release_prefetch.clone();
                            Box::pin(async move {
                                if second {
                                    entered.complete(Ok(()));
                                    release
                                        .wait()
                                        .await
                                        .map_err(|e| OperationError::Failed(e.failure().clone()))?;
                                }
                                Ok(())
                            })
                        }),
                    ))
                }));
                let built = factory.build(&core).await?;
                let subscribed: Gate = SharedDeferred::new();
                let ack = subscribed.clone();
                *lock(&built.provider.on_subscribe) = Some(Arc::new(move || {
                    ack.complete(Ok(()));
                }));
                let root = core.job(JobKind::Ordinary, None);
                let context = core.context(root.clone());
                let mut running = Box::pin(built.marathon.run_async(&command, &context));
                // Every drain waits for tracked off-core work, rather than guessing a
                // number of yields or relying on equal-time external event order.
                while subscribed.completed().is_none() {
                    assert!(futures_util::poll!(running.as_mut()).is_pending());
                    driver.drain_current().await.map_err(replay_error)?;
                }
                let recorder = Recorder {
                    log: Arc::default(),
                    now: core.clock().now_source(),
                };
                let a = scripted("a", "A", &recorder);
                let b = scripted("b", "B", &recorder);
                assert!(built.provider.send(DeviceEvent::Connected(a.reference())));
                driver.drain_current().await.map_err(replay_error)?;
                assert!(called(&recorder, "a", "prepare"));
                assert_eq!(a.deliver(&json!({"event":"prepare-done"})), None);
                driver.drain_current().await.map_err(replay_error)?;
                assert!(called(&recorder, "a", "execute"));
                driver.advance_to(100).await.map_err(replay_error)?;
                assert!(
                    futures_util::poll!(running.as_mut()).is_pending(),
                    "final pool join is held"
                );

                if !admitted {
                    assert!(built.provider.send(DeviceEvent::Connected(b.reference())));
                    driver.drain_current().await.map_err(replay_error)?;
                    assert!(
                        entered.completed().is_some(),
                        "Connected(B) reached prefetch before A finishes"
                    );
                }
                finish(&a, !admitted && fails);
                driver.drain_current().await.map_err(replay_error)?;
                let owner = built.marathon.scheduler_job().ok_or_else(|| {
                    Failure::from_kind(FailureKind::IllegalState, Some("Scheduler owner missing".into()))
                })?;
                assert!(owner.children_snapshot().is_empty(), "A actor and supervisor detached");
                if admitted {
                    // A detached while the run's final join continuation was held.
                    assert!(built.provider.send(DeviceEvent::Connected(b.reference())));
                    driver.drain_current().await.map_err(replay_error)?;
                    assert!(entered.completed().is_some());
                    release.complete(Ok(()));
                    driver.drain_current().await.map_err(replay_error)?;
                    assert!(
                        called(&recorder, "b", "prepare"),
                        "B published before traversal resumes"
                    );
                    assert_eq!(owner.children_snapshot().len(), 2, "B actor and supervisor admitted");
                    assert_eq!(b.deliver(&json!({"event":"prepare-done"})), None);
                    driver.drain_current().await.map_err(replay_error)?;
                    assert!(called(&recorder, "b", "execute"));
                    assert!(
                        futures_util::poll!(running.as_mut()).is_pending(),
                        "run joins B before reporting"
                    );
                    finish(&b, fails);
                    driver.drain_current().await.map_err(replay_error)?;
                } else {
                    assert!(release.completed().is_none());
                    assert!(!called(&recorder, "b", "prepare"), "B is not published");
                }
                let value = running.await.map_err(|e| OperationError::Failed(e.failure().clone()))?;
                assert_eq!(value, !fails);
                if !admitted {
                    assert!(
                        release.completed().is_none(),
                        "reports finished while prefetch is still held"
                    );
                    assert!(!called(&recorder, "b", "execute"));
                }
                root.cancel(requested("admission witness teardown"));
                release.complete(Ok(()));
                driver.drain_current().await.map_err(replay_error)?;
                root.join().await?;
                driver.finish().map_err(replay_error)?;
                Ok(value)
            })
        });
        result.map_err(|error| match error {
            OperationError::Failed(cause) => cause,
            OperationError::Cancelled(reason) => reason.failure().clone(),
        })
    }
}

fn witness(admitted: bool, fails: bool) -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    std::fs::write(
        directory.path().join("Marathonfile"),
        "name: admission\noutputDir: out\nuncompletedTestRetryQuota: 0\nvendorConfiguration:\n  type: Android\n  applicationApk: app.apk\n",
    )?;
    let stdout = Buffer::default();
    let stderr = Buffer::default();
    let output = CliOutput::new(
        Box::new(stdout.clone()),
        Box::new(stderr.clone()),
        OutputTarget::Pipe,
        Box::new(|_| "00:00:00.000".into()),
    )?;
    let process = Process {
        cwd: directory.path().into(),
        environment: vec![("ANDROID_HOME".into(), "/android-sdk".into())],
        now: jcompat::jtime::instant_of(0, 0).ok_or("epoch is representable")?,
    };
    let status = run_cli(&[], &process, &Runner { admitted, fails }, &output);
    assert_eq!(status, i32::from(fails), "{}\n{}", stdout.text(), stderr.text());
    assert_eq!(stderr.text(), if fails { "Marathon execution failed\n" } else { "" });
    let text = stdout.text();
    assert_eq!(text.contains("Marathon execution finished\n"), !fails);
    let a_status = if !admitted && fails { "failed" } else { "passed" };
    let b_status = if fails { "failed" } else { "passed" };
    let progress: Vec<_> = text.lines().filter(|line| line.contains(" | [")).collect();
    assert_eq!(progress.len(), if admitted { 2 } else { 1 }, "{text}");
    assert!(progress[0].ends_with(&format!(" | [A]-[a] pkg.Admission#test {a_status}")));
    assert_eq!(text.matches("Device pool A:\n").count(), 1);
    assert!(text.contains(if a_status == "passed" {
        "Device pool A:\n\t1 passed, 0 failed, 0 ignored tests\n"
    } else {
        "Device pool A:\n\t0 passed, 1 failed, 0 ignored tests\n"
    }));
    let out = directory.path().join("out");
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(out.join("test_result/raw.json"))?)?;
    let rows = raw.as_array().ok_or("raw report is an array")?;
    assert_eq!(rows.len(), if admitted { 2 } else { 1 });
    assert_eq!(rows[0]["deviceSerial"], "a");
    assert_eq!(
        rows[0]["status"],
        if a_status == "passed" { "PASSED" } else { "FAILURE" }
    );
    let a_xml = std::fs::read_to_string(out.join("tests/A/marathon_junit_report.xml"))?;
    assert!(a_xml.contains(&format!(
        "name=\"A\" tests=\"1\" failures=\"{}\"",
        i32::from(!admitted && fails)
    )));
    if admitted {
        assert!(progress[1].ends_with(&format!(" | [B]-[b] pkg.Admission#test {b_status}")));
        assert_eq!(text.matches("Device pool B:\n").count(), 1);
        assert!(text.contains(if fails {
            "Device pool B:\n\t0 passed, 1 failed, 0 ignored tests\n"
        } else {
            "Device pool B:\n\t1 passed, 0 failed, 0 ignored tests\n"
        }));
        assert_eq!(rows[1]["deviceSerial"], "b");
        assert_eq!(rows[1]["status"], if fails { "FAILURE" } else { "PASSED" });
        let b_xml = std::fs::read_to_string(out.join("tests/B/marathon_junit_report.xml"))?;
        assert!(b_xml.contains(&format!("name=\"B\" tests=\"1\" failures=\"{}\"", i32::from(fails))));
        assert!(out.join("test_result/B").is_dir());
    } else {
        assert!(!text.contains("[B]-[b]"));
        assert!(!text.contains("Device pool B:"));
        assert!(!out.join("tests/B").exists());
        assert!(!out.join("test_result/B").exists());
    }
    assert_eq!(text.matches("Marathon run finished:\n").count(), 1);
    assert!(!text.contains("but no events received"));
    Ok(())
}
#[test]
fn admitted_late_pool_reports_files_progress_counts_and_cli_status_d65() -> Result<(), Box<dyn std::error::Error>> {
    for fails in [false, true] {
        witness(true, fails)?;
    }
    Ok(())
}
#[test]
fn held_prefetch_is_excluded_from_files_progress_summary_and_cli_status_d65() -> Result<(), Box<dyn std::error::Error>>
{
    for fails in [false, true] {
        witness(false, fails)?;
    }
    Ok(())
}
