//! Regression for the live-stdout bug (docs/found-bugs.md U101): vendor log lines reached the
//! console only at the next core flush, so nothing was printed during a long test. Kotlin's
//! console appender prints each record as it is logged.
use jcompat::logging::Level;
use marathon_core::{
    error::VendorError,
    executor::shared::failure,
    logging::{LogQueue, LogRecord, OutputRecord},
    marathon::{Marathon, RunServices},
    model::{ExecutionCommand, TestStatus},
    output::{ConsoleSink, OutputDriver},
    runtime::{VendorFuture, VendorRuntime, VendorTask, VendorTaskBody},
    vendor::DeviceEvent,
    wiring::RunDependencies,
};
use marathon_error::FailureKind;
use marathon_test_support::{
    factory::MarathonFactory,
    scenario,
    stub_device::{Recorder, StubDevice, StubDeviceOptions},
};
use std::{
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const PROBE: &str = "LiveOutputProbe";

fn probe_record() -> LogRecord {
    LogRecord {
        logger: PROBE.into(),
        at_millis: 0,
        wall_millis: 0,
        level: Level::Error,
        message: "vendor line during the test".into(),
        cause: None,
    }
}

/// A live run prints a vendor line while the (fake, 3 s) test is still executing, not at its end.
#[test]
fn live_run_prints_a_vendor_log_line_before_the_test_ends() {
    let device = StubDevice::new(StubDeviceOptions {
        prepare_time_millis: 100,
        test_time_millis: 3000,
        ..StubDeviceOptions::default()
    });
    let test = scenario::test("pkg", "SimpleTest", "test");
    device.set_execution_results(HashMap::from([(test.clone(), vec![TestStatus::Passed])]));
    let calls = Arc::new(Mutex::new(Vec::new()));
    device.record_to(Recorder {
        log: calls.clone(),
        now: Arc::new(|| 0),
    });
    let mut factory = MarathonFactory::new();
    factory.tests = vec![test];
    factory.live_output = true;

    // (line printed while the device was still executing, seconds the line took to appear)
    let observed = Arc::new(Mutex::new(None::<(bool, Duration)>));
    let seen = observed.clone();
    let outcome = marathon_core::executor::CoreExecutor::run(move |core| {
        Box::pin(async move {
            let built = factory.build(&core).await?;
            let console = built.console.clone();
            let sender = core.log_queue().sender();
            let executing = move |name: &str| calls.lock().unwrap().iter().any(|c| c["call"] == name);
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(60);
                while !executing("execute") && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                let pushed = Instant::now();
                sender.push(probe_record());
                // Live means printed before the 3 s test ends, not within a fixed time a loaded
                // host could outlast; the deadline only catches hangs.
                let deadline = Instant::now() + Duration::from_secs(60);
                while console.messages(PROBE).is_empty() && !executing("executed") && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                let printed = !console.messages(PROBE).is_empty();
                *seen.lock().unwrap() = Some((printed && !executing("executed"), pushed.elapsed()));
            });
            built.provider.send(DeviceEvent::Connected(device.reference()));
            built.marathon.run(ExecutionCommand::Run).await
        })
    });
    assert!(matches!(outcome, Ok(true)), "{outcome:?}");
    let (live, waited) = observed.lock().unwrap().expect("the observer finished");
    assert!(
        live,
        "the vendor line was not printed within {waited:?} of being logged, while the test ran"
    );
}

/// Flush points of a replay run, as the sink sees them. Recorded before live output existed.
#[test]
fn replay_run_keeps_its_flush_points() {
    let outcome = scenario::run("success.json", |_| {});
    assert!(outcome.completed);
    // The device asks for its next batch after the vendor worker completes the batch, which can
    // be after the queue has closed (Kotlin's actors race the same way); the pool then logs one
    // dropped request. That line is not a flush point, so it is not counted.
    let dropped = |line: &marathon_test_support::console::Line| {
        matches!(line, marathon_test_support::console::Line::Log { message, .. }
            if message == "pool DevicePoolId(name=omni): queue is closed, RequestBatch is dropped")
    };
    assert!(outcome.lines.iter().filter(|line| dropped(line)).count() <= 1);
    let mut start = 0;
    let batches: Vec<usize> = outcome
        .batches
        .iter()
        .map(|&size| {
            let batch = &outcome.lines[start..start + size];
            start += size;
            size - batch.iter().filter(|line| dropped(line)).count()
        })
        .collect();
    assert_eq!(batches, [3, 2, 3, 9, 3, 1, 1], "{:?}", outcome.lines);
}

struct Batches(Mutex<Vec<Vec<String>>>);
impl ConsoleSink for Batches {
    fn write(&self, records: Vec<OutputRecord>) -> VendorFuture<'_, ()> {
        let batch = records
            .into_iter()
            .map(|record| match record {
                OutputRecord::Log(log) => log.message.to_owned(),
                OutputRecord::Raw(text) => text.to_owned(),
            })
            .collect();
        self.0.lock().unwrap().push(batch);
        Box::pin(async { Ok(()) })
    }
}

/// Without the live pump (every replay and oracle run) queued records wait for an explicit
/// flush, which writes them as one batch in push order.
#[test]
fn without_live_output_records_wait_for_the_serial_flush() {
    let written = marathon_core::executor::CoreExecutor::run(|core| {
        Box::pin(async move {
            let sink = Arc::new(Batches(Mutex::default()));
            let driver = OutputDriver {
                vendor: core.vendor(),
                sink: sink.clone(),
            };
            let queue: Rc<LogQueue> = core.log_queue();
            queue.raw("a".into());
            let sender = queue.sender();
            std::thread::spawn(move || sender.push(probe_record())).join().unwrap();
            queue.raw("b".into());
            tokio::time::sleep(Duration::from_millis(100)).await;
            let before = sink.0.lock().unwrap().len();
            driver
                .flush(&queue, marathon_core::executor::Signal::new())
                .await
                .unwrap();
            driver
                .flush(&queue, marathon_core::executor::Signal::new())
                .await
                .unwrap();
            let after = sink.0.lock().unwrap().clone();
            Ok((before, after))
        })
    })
    .unwrap();
    assert_eq!(written.0, 0, "nothing is written before the flush");
    assert_eq!(written.1, [["a", "vendor line during the test", "b"]]);
}

/// Lets a failed vendor task finish late: its batch barrier is long released by then.
struct LateFailures(Arc<dyn VendorRuntime>);
impl VendorRuntime for LateFailures {
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
        self.0.spawn(Box::pin(async move {
            let result = body.await;
            if result.is_err() {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            result
        }))
    }
    fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        self.0.spawn_cancellable(body)
    }
    fn spawn_blocking(
        &self,
        body: Box<dyn FnOnce() -> marathon_core::error::VendorResult<()> + Send>,
    ) -> Arc<dyn VendorTask> {
        self.0.spawn_blocking(body)
    }
}

/// Fails its first write, then lets the run (parked in provider initialization) go on.
struct FailsOnce {
    failed: AtomicBool,
    failure_written: tokio::sync::Notify,
    /// Messages of the causes on every later record, i.e. what the run itself reported.
    reported: Mutex<Vec<String>>,
}
impl ConsoleSink for FailsOnce {
    fn write(&self, records: Vec<OutputRecord>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if self.failed.swap(true, Ordering::SeqCst) {
                for record in records {
                    if let OutputRecord::Log(LogRecord { cause: Some(cause), .. }) = record {
                        let message = cause.message.map(|m| m.to_owned()).unwrap_or_default();
                        self.reported.lock().unwrap().push(message);
                    }
                }
                return Ok(());
            }
            self.failure_written.notify_one();
            Err(VendorError::Other(failure(
                FailureKind::IllegalState,
                "injected output failure",
            )))
        })
    }
}

/// The pump's write fails, which releases its batch barrier, and the run's serial flushes
/// then complete before the failed vendor task has returned to core. The failure must still
/// reach the run's result, as a serial flush that wrote the same batch would have reported it.
#[test]
fn pump_write_failure_is_reported_even_when_the_vendor_task_finishes_late() {
    let outcome = marathon_core::executor::CoreExecutor::run(|core| {
        Box::pin(async move {
            let mut factory = MarathonFactory::new();
            let test = scenario::test("pkg", "Test", "method");
            factory.tests = vec![test.clone()];
            let device = StubDevice::new(StubDeviceOptions {
                prepare_time_millis: 0,
                test_time_millis: 0,
                ..StubDeviceOptions::default()
            });
            device.set_execution_results(HashMap::from([(test, vec![TestStatus::Passed])]));
            let built = factory.build(&core).await?;
            let sink = Arc::new(FailsOnce {
                failed: AtomicBool::new(false),
                failure_written: tokio::sync::Notify::new(),
                reported: Mutex::default(),
            });
            let written = sink.clone();
            *built.provider.on_initialize.lock().unwrap() = Some(Arc::new(move |_| {
                let written = written.clone();
                Box::pin(async move {
                    written.failure_written.notified().await;
                    Ok(())
                })
            }));
            built.provider.send(DeviceEvent::Connected(device.reference()));
            let services = built.marathon.services();
            let d = built.marathon.dependencies();
            let marathon = Marathon::new(
                RunDependencies {
                    vendor: d.vendor.clone(),
                    provider: d.provider.clone(),
                    parser: d.parser.clone(),
                    bundle_identifier: d.bundle_identifier.clone(),
                    log_configurator: d.log_configurator.clone(),
                    analytics: d.analytics.clone(),
                    reports: d.reports.clone(),
                    usage: d.usage.clone(),
                    parse_output: d.parse_output.clone(),
                    core: d.core.clone(),
                    scheduler_clock: d.scheduler_clock.clone(),
                    timeouts: d.timeouts.clone(),
                },
                RunServices {
                    core: core.clone(),
                    output: OutputDriver {
                        vendor: Arc::new(LateFailures(core.vendor())),
                        sink: sink.clone(),
                    },
                    ci: services.ci,
                    filters: services.filters.clone(),
                    system_information: services.system_information.clone(),
                    signals: None,
                    live_output: true,
                },
            );
            let outcome = marathon.run(ExecutionCommand::Run).await?;
            let reported = sink.reported.lock().unwrap().clone();
            Ok((outcome, reported))
        })
    });
    let (completed, reported) = outcome.expect("the run itself returns");
    // A failed flush inside the run is caught like any other failure: logged, and mapped to
    // `ignoreFailures` (false here).
    assert!(!completed, "the run reported success");
    assert!(
        reported.iter().any(|m| m.contains("injected output failure")),
        "the pump's write failure was lost: {reported:?}"
    );
}
