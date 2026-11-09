//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/internal/sub/ExecutionReport.kt
//! and ExecutionReportGenerator.kt, analytics/internal/pub/Track.kt, analytics/TrackerFactory.kt.
use marathon_error::{FailureKind, InjectedKind};
mod event_support;
use event_support::*;
use jcompat::{jtime::instant_of, logging::MarathonLogging};
use marathon_core::{
    analytics::*,
    error::*,
    executor::{
        CoreExecutor,
        hooks::{ClockMode, RuntimeHooks, Tape},
    },
    model::*,
    report::*,
    runtime::*,
    time::*,
    wiring::Tracker,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
fn oracle(id: &str) -> Value {
    include_str!("../../../fixtures/oracle/event-pipeline/probes.jsonl")
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .find(|r| r["id"] == id)
        .unwrap()
}
fn fail() -> Failure {
    marathon_core::executor::shared::failure(FailureKind::IllegalState, "failed")
}
fn assert_worker() {
    assert_ne!(std::thread::current().name(), Some("marathon-core"));
}
async fn close_generator(io: &dyn VendorRuntime, generator: &ExecutionReportGenerator) -> OperationResult<()> {
    let generator = generator.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = io.spawn(Box::pin(async move {
        let _ = tx.send(generator.close().await);
        Ok(())
    }));
    task.join()
        .await
        .map_err(|e| OperationError::Failed(e.failure().clone()))?;
    checked(rx.await)
}
struct RecordReporter {
    calls: Arc<Mutex<Vec<String>>>,
    name: &'static str,
    fail: bool,
    reports: Arc<Mutex<Vec<ExecutionReport>>>,
}
impl Reporter for RecordReporter {
    fn generate<'a>(&'a self, report: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            assert_worker();
            self.calls.lock().unwrap().push(self.name.into());
            self.reports.lock().unwrap().push(report.clone());
            if self.fail { Err(fail().into()) } else { Ok(()) }
        })
    }
}
#[test]
fn summary_matches_jvm_and_keeps_all_finals_and_raw_attempts() {
    let mut events = vec![];
    for (name, status, start, end) in [
        ("x", TestStatus::Incomplete, 1, 100),
        ("x", TestStatus::Passed, 1, 100),
        ("x", TestStatus::Failure, 1, 100),
        ("a", TestStatus::AssumptionFailure, 1, 100),
        ("zero", TestStatus::Passed, 0, 100),
        ("negative", TestStatus::Passed, 100, 1),
        ("overflow", TestStatus::Passed, i64::MIN, i64::MAX),
    ] {
        let mut r = result(name, status);
        r.start_time = start;
        r.end_time = end;
        events.push(event(Arc::new(r), true, 0));
    }
    let mut orphan = event(Arc::new(result("orphan", TestStatus::Passed)), true, 0);
    orphan.pool_id = pool("test-only");
    events.insert(0, orphan);
    let report = report(events);
    let summary = &report.summary().pools[0];
    let expected = oracle("summary");
    assert_eq!(
        json!(summary.tests.iter().map(|r| r.test.method.clone()).collect::<Vec<_>>()),
        expected["tests"]
    );
    assert_eq!(json!(names(&summary.passed)), expected["passed"]);
    assert_eq!(json!(names(&summary.ignored)), expected["ignored"]);
    assert_eq!(json!(names(&summary.failed)), expected["failed"]);
    assert_eq!(json!(summary.duration_millis), expected["duration"]);
    assert_eq!(json!(summary.raw_duration_millis), expected["rawDuration"]);
    assert_eq!(json!(names(&summary.raw_incomplete)), expected["rawIncomplete"]);
    assert_eq!(summary.flaky, 0);
    assert_eq!(report.test_events().len(), 8);
    assert_eq!(report.summary().pools.len(), 1);
    assert!(std::ptr::eq(report.summary(), report.summary()));
}
#[test]
fn connections_keep_first_serial_snapshot_and_pool_order() {
    let mut first = device();
    first.model = "first".into();
    let mut last = first.clone();
    last.model = "last".into();
    let report = ExecutionReport::new(
        vec![
            DeviceConnectedEvent {
                instant: instant_of(0, 0).unwrap(),
                pool_id: pool("b"),
                device: first.clone(),
            },
            DeviceConnectedEvent {
                instant: instant_of(0, 0).unwrap(),
                pool_id: pool("a"),
                device: first,
            },
            DeviceConnectedEvent {
                instant: instant_of(0, 0).unwrap(),
                pool_id: pool("b"),
                device: last,
            },
        ],
        vec![],
        vec![],
        vec![],
        vec![],
    );
    assert_eq!(
        json!(
            report
                .summary()
                .pools
                .iter()
                .map(|p| p.pool_id.name.clone())
                .collect::<Vec<_>>()
        ),
        oracle("connections")["pools"]
    );
    assert_eq!(
        json!(
            report
                .summary()
                .pools
                .iter()
                .flat_map(|p| &p.devices)
                .map(|d| d.model.clone())
                .collect::<Vec<_>>()
        ),
        oracle("connections")["models"]
    );
}
#[test]
fn retry_identity_and_structural_map_replacement_match_jvm() {
    for alias in [true, false] {
        let r = Arc::new(result("test", TestStatus::Passed));
        let other = if alias { r.clone() } else { Arc::new((*r).clone()) };
        let report = report(vec![event(r, true, 0), event(other, true, 0)]);
        let p = &report.summary().pools[0];
        let expected = oracle(if alias { "same-object" } else { "equal-distinct" });
        assert_eq!(json!(p.tests.len()), expected["tests"]);
        assert_eq!(json!(p.retries.len()), expected["keys"]);
        let indexes: Vec<Vec<usize>> = p
            .retries
            .iter()
            .map(|(_, list)| {
                list.iter()
                    .map(|e| {
                        report
                            .test_events()
                            .iter()
                            .position(|candidate| Arc::ptr_eq(&candidate.test_result, &e.test_result))
                            .unwrap()
                    })
                    .collect()
            })
            .collect();
        assert_eq!(json!(indexes), expected["retryIndexes"]);
    }
    let r = Arc::new(result("same", TestStatus::Passed));
    let mut second = (*r).clone();
    second.test.meta_properties.push(MetaProperty {
        name: "extra".into(),
        values: MetaMap::default(),
    });
    let report = report(vec![event(r.clone(), true, 0), event(Arc::new(second), true, 1)]);
    let p = &report.summary().pools[0];
    assert_eq!(p.tests.len(), 2);
    assert_eq!(p.retries.len(), 1);
    assert!(Arc::ptr_eq(&p.retries[0].0, &r));
    assert!(Arc::ptr_eq(&p.retries[0].1[0].test_result, &r));
}
#[test]
fn generator_sorts_stably_and_explicit_close_regenerates() {
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let calls = Arc::new(Mutex::new(vec![]));
            let reports = Arc::new(Mutex::new(vec![]));
            let generator = ExecutionReportGenerator::new(vec![Arc::new(RecordReporter {
                calls: calls.clone(),
                name: "report",
                fail: false,
                reports: reports.clone(),
            })]);
            for (name, time) in [("late", 2), ("early", 1), ("equal", 1)] {
                generator
                    .track(&Event::Test(event(
                        Arc::new(result(name, TestStatus::Passed)),
                        true,
                        time,
                    )))
                    .await
                    .map_err(|e| OperationError::Failed(e.failure().clone()))?;
            }
            for time in [2, 1, 1] {
                let i = instant_of(0, time).unwrap();
                for e in [
                    Event::DeviceConnected(DeviceConnectedEvent {
                        instant: i,
                        pool_id: pool(&time.to_string()),
                        device: device(),
                    }),
                    Event::DeviceDisconnected(DeviceDisconnectedEvent {
                        instant: i,
                        pool_id: pool(&time.to_string()),
                        device: device(),
                    }),
                    Event::DevicePreparing(DevicePreparingEvent {
                        start: i,
                        finish: i,
                        serial_number: time.to_string(),
                    }),
                    Event::DeviceProviderPreparing(DeviceProviderPreparingEvent {
                        start: i,
                        finish: i,
                        serial_number: time.to_string(),
                    }),
                ] {
                    generator.track(&e).await.unwrap();
                }
            }
            close_generator(core.vendor().as_ref(), &generator).await?;
            close_generator(core.vendor().as_ref(), &generator).await?;
            let reports = reports.lock().unwrap();
            assert_eq!(reports.len(), 2);
            let calls: Vec<String> = reports
                .iter()
                .map(|r| {
                    r.test_events()
                        .iter()
                        .map(|e| e.test_result.test.method.clone())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .collect();
            assert_eq!(json!(calls), oracle("sort-repeat-close")["calls"]);
            assert_eq!(
                reports[0]
                    .device_connected_events()
                    .iter()
                    .map(|e| e.instant.nanos())
                    .collect::<Vec<_>>(),
                [1, 1, 2]
            );
            assert_eq!(reports[0].device_disconnected_events()[0].instant.nanos(), 1);
            assert_eq!(reports[0].device_preparing_events()[0].start.nanos(), 1);
            assert_eq!(reports[0].device_provider_preparing_events()[0].start.nanos(), 1);
            Ok(())
        })
    })
    .unwrap();
}
struct Remote {
    calls: Arc<Mutex<Vec<String>>>,
    fail_track: bool,
}
impl TrackerInternalAdapter for Remote {
    fn test<'a>(&'a self, _: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            assert_worker();
            self.calls.lock().unwrap().push("remote-test".into());
            if self.fail_track {
                Err(VendorError::Other(fail()))
            } else {
                Ok(())
            }
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async move {
            assert_worker();
            self.calls.lock().unwrap().push("remote-close".into());
            Ok(())
        })
    }
}
struct Backend {
    calls: Arc<Mutex<Vec<String>>>,
    outcome: u8,
}
impl BackendFactory for Backend {
    fn create(&self, request: BackendRequest) -> ServiceFuture<'_, Result<Option<Arc<dyn TrackerInternal>>, Failure>> {
        Box::pin(async move {
            assert_worker();
            match request {
                BackendRequest::Graphite { port } => assert_eq!(port, Some(2003)),
                BackendRequest::Influx { .. } => {}
            }
            self.calls.lock().unwrap().push("setup".into());
            match self.outcome {
                1 => Ok(Err(fail())),
                2 => Err(fail().into()),
                _ => Ok(Ok(Some(Arc::new(Remote {
                    calls: self.calls.clone(),
                    fail_track: self.outcome == 3,
                }) as Arc<dyn TrackerInternal>))),
            }
        })
    }
}
#[test]
fn factory_orders_reporters_before_remote_and_only_catches_influx_setup() {
    for (outcome, reporter_failure) in [(0, false), (0, true), (1, false), (2, false), (3, false)] {
        CoreExecutor::run(move |core| {
            Box::pin(async move {
                let calls = Arc::new(Mutex::new(vec![]));
                let hooks = RuntimeHooks::live();
                let track = Arc::new(Track::new(core.vendor(), hooks.clone()));
                let reports = Arc::new(Mutex::new(vec![]));
                let log_calls = calls.clone();
                let log = Arc::new(
                    move |_: &jcompat::logging::Logger,
                          _: jcompat::logging::Level,
                          message: &str,
                          _: Option<&Failure>| {
                        assert_worker();
                        log_calls.lock().unwrap().push(message.to_owned());
                    },
                );
                let reporter = |slot, name| ReporterRegistration {
                    slot,
                    reporter: Arc::new(RecordReporter {
                        calls: calls.clone(),
                        name,
                        fail: reporter_failure && slot == ReporterSlot::Billing,
                        reports: reports.clone(),
                    }),
                };
                let pipeline = TrackerFactory {
                    io: core.vendor(),
                    track: track.clone(),
                    clock: hooks,
                    reporters: vec![
                        reporter(ReporterSlot::Stdout, "stdout"),
                        reporter(ReporterSlot::Html, "html"),
                        reporter(ReporterSlot::Allure, "allure"),
                        reporter(ReporterSlot::TestJson, "test-json"),
                        reporter(ReporterSlot::Raw, "raw"),
                        reporter(ReporterSlot::Timeline, "timeline"),
                        reporter(ReporterSlot::JUnit, "junit"),
                        reporter(ReporterSlot::Billing, "billing"),
                        reporter(ReporterSlot::DeviceInfo, "devices"),
                    ],
                    backend: Some(BackendRegistration {
                        request: if outcome == 1 {
                            BackendRequest::Influx {
                                url: "http://unreachable".into(),
                            }
                        } else {
                            BackendRequest::Graphite { port: None }
                        },
                        factory: Arc::new(Backend {
                            calls: calls.clone(),
                            outcome,
                        }),
                    }),
                    logger: MarathonLogging::default().logger("TrackerFactory", None),
                    log,
                }
                .create()
                .await;
                assert_eq!(pipeline.is_err(), outcome == 2);
                if let Ok(pipeline) = pipeline {
                    assert!(
                        pipeline
                            .tracker()
                            .test(&pool("myPool"), &device(), &result("x", TestStatus::Passed), true)
                            .await
                            .is_ok(),
                        "D51: a backend's failure never reaches the queue"
                    );
                    assert_eq!(pipeline.reports().close().await.is_err(), reporter_failure);
                    let got = calls.lock().unwrap().clone();
                    assert_eq!(got[0], "setup");
                    if outcome == 1 {
                        assert_eq!(got[1], "Failed to reach InfluxDB at http://unreachable");
                        assert!(!got.contains(&"remote-close".to_owned()));
                    } else if !reporter_failure {
                        assert_eq!(got.last().unwrap(), "remote-close");
                    }
                    if reporter_failure {
                        // D51: Kotlin stops at the failing reporter (C11).
                        assert_eq!(
                            &got[2..],
                            [
                                "devices",
                                "billing",
                                "junit",
                                "timeline",
                                "raw",
                                "test-json",
                                "allure",
                                "html",
                                "stdout",
                                "remote-close"
                            ]
                        );
                    } else {
                        // Outcome 3 logs its failure once, and the lost metrics at close.
                        let start = if outcome == 3 { 3 } else { 2 };
                        assert_eq!(
                            &got[start..start + 9],
                            [
                                "devices",
                                "billing",
                                "junit",
                                "timeline",
                                "raw",
                                "test-json",
                                "allure",
                                "html",
                                "stdout"
                            ]
                        );
                    }
                }
                Ok(())
            })
        })
        .unwrap();
    }
}
#[test]
fn public_mapping_uses_tapes_and_preparation_has_no_finish_on_failure() {
    CoreExecutor::run_virtual(|core, driver| {
        Box::pin(async move {
            let mut tape = Tape {
                wall: ClockMode::Frozen(0),
                ..Tape::default()
            };
            for label in [
                "core.MappingTracker.deviceConnected",
                "core.MappingTracker.deviceDisconnected",
                "core.MappingTracker.test",
            ] {
                tape.reads.insert(label.into(), vec![7]);
            }
            tape.reads
                .insert("core.Track.trackDevicePreparing".into(), vec![10, 20]);
            tape.reads
                .insert("core.Track.trackProviderDevicePreparing".into(), vec![30]);
            let hooks = RuntimeHooks::taped(tape, &core.clock());
            driver.set_hooks(hooks.clone());
            let reports = Arc::new(Mutex::new(vec![]));
            let generator = Arc::new(ExecutionReportGenerator::new(vec![Arc::new(RecordReporter {
                calls: Arc::default(),
                name: "report",
                fail: false,
                reports: reports.clone(),
            })]));
            let track = Track::new(core.vendor(), hooks.clone());
            track.add(Arc::new(MappingTracker::new(hooks.clone(), generator.clone())));
            track.device_connected(&pool("myPool"), &device()).await.unwrap();
            track.device_disconnected(&pool("myPool"), &device()).await.unwrap();
            track
                .test(&pool("myPool"), &device(), &result("x", TestStatus::Passed), true)
                .await
                .unwrap();
            assert_eq!(
                track
                    .track_device_preparing(
                        || "serial".into(),
                        Box::pin(async {
                            assert_eq!(std::thread::current().name(), Some("marathon-core"));
                            Ok(42)
                        })
                    )
                    .await,
                Ok(42)
            );
            assert!(
                track
                    .track_provider_device_preparing(
                        || "serial".into(),
                        Box::pin(async { Err::<(), _>(VendorError::Other(fail())) })
                    )
                    .await
                    .is_err()
            );
            close_generator(core.vendor().as_ref(), &generator).await?;
            let captured = reports.lock().unwrap()[0].clone();
            assert_eq!(captured.device_connected_events()[0].instant.nanos(), 7_000_000);
            assert_eq!(captured.device_preparing_events()[0].start.nanos(), 10_000_000);
            assert_eq!(captured.device_preparing_events()[0].finish.nanos(), 20_000_000);
            assert!(captured.device_provider_preparing_events().is_empty());
            hooks.verify_consumed().unwrap();
            Ok(())
        })
    })
    .unwrap();
}
struct Fanout {
    calls: Arc<Mutex<Vec<String>>>,
    name: &'static str,
    track: Option<std::sync::Weak<Track>>,
    extra: Option<Arc<dyn Tracker>>,
    error: Option<VendorError>,
}
impl Tracker for Fanout {
    fn device_connected<'a>(&'a self, _: &'a DevicePoolId, _: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn device_disconnected<'a>(&'a self, _: &'a DevicePoolId, _: &'a DeviceInfo) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn device_provider_init<'a>(
        &'a self,
        _: &'a str,
        _: jcompat::jtime::Instant,
        _: jcompat::jtime::Instant,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn device_preparing<'a>(
        &'a self,
        _: &'a str,
        _: jcompat::jtime::Instant,
        _: jcompat::jtime::Instant,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn test<'a>(&'a self, _: &'a DevicePoolId, _: &'a DeviceInfo, _: &'a TestResult, _: bool) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            assert_worker();
            self.calls.lock().unwrap().push(self.name.into());
            if let (Some(track), Some(extra)) = (&self.track, &self.extra)
                && let Some(track) = track.upgrade()
            {
                track.add(extra.clone());
            }
            if let Some(error) = &self.error {
                Err(error.clone())
            } else {
                Ok(())
            }
        })
    }
}
#[test]
fn track_duplicates_failure_snapshot_and_typed_cancellation() {
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let calls = Arc::new(Mutex::new(vec![]));
            let track = Arc::new(Track::new(core.vendor(), RuntimeHooks::live()));
            let a = Arc::new(Fanout {
                calls: calls.clone(),
                name: "a",
                track: None,
                extra: None,
                error: None,
            });
            track.add(a.clone());
            track.add(a);
            let f = marathon_core::executor::shared::failure(FailureKind::IllegalArgument, "track");
            track.add(Arc::new(Fanout {
                calls: calls.clone(),
                name: "bad",
                track: None,
                extra: None,
                error: Some(VendorError::Other(f.clone())),
            }));
            track.add(Arc::new(Fanout {
                calls: calls.clone(),
                name: "never",
                track: None,
                extra: None,
                error: None,
            }));
            assert_eq!(
                track
                    .test(&pool("myPool"), &device(), &result("x", TestStatus::Passed), true)
                    .await,
                Err(VendorError::Other(f))
            );
            assert_eq!(json!(*calls.lock().unwrap()), oracle("fanout")["calls"]);
            calls.lock().unwrap().clear();
            let track = Arc::new(Track::new(core.vendor(), RuntimeHooks::live()));
            let extra: Arc<dyn Tracker> = Arc::new(Fanout {
                calls: calls.clone(),
                name: "added",
                track: None,
                extra: None,
                error: None,
            });
            let weak = Arc::downgrade(&track);
            track.add(Arc::new(Fanout {
                calls: calls.clone(),
                name: "register",
                track: Some(weak),
                extra: Some(extra),
                error: None,
            }));
            track
                .test(&pool("myPool"), &device(), &result("x", TestStatus::Passed), true)
                .await
                .unwrap();
            assert_eq!(json!(*calls.lock().unwrap()), oracle("track-snapshot")["first"]);
            track
                .test(&pool("myPool"), &device(), &result("x", TestStatus::Passed), true)
                .await
                .unwrap();
            assert_eq!(json!(*calls.lock().unwrap()), oracle("track-snapshot")["second"]);
            let cancelled = Cancellation::Timeout {
                scope: 42,
                detail: fail(),
            };
            let track = Track::new(core.vendor(), RuntimeHooks::live());
            track.add(Arc::new(Fanout {
                calls: Arc::default(),
                name: "cancel",
                track: None,
                extra: None,
                error: Some(VendorError::Cancelled(cancelled.clone())),
            }));
            assert_eq!(
                track
                    .test(&pool("myPool"), &device(), &result("x", TestStatus::Passed), true)
                    .await,
                Err(VendorError::Cancelled(cancelled))
            );
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn duration_sum_wraps_as_kotlin_long() {
    let events = (0..2)
        .map(|_| {
            let mut r = result("overflow", TestStatus::Passed);
            r.start_time = 1;
            r.end_time = i64::MAX;
            event(Arc::new(r), true, 0)
        })
        .collect();
    let report = report(events);
    assert_eq!(
        json!(report.summary().pools[0].duration_millis),
        oracle("sum-overflow")["duration"]
    );
    assert_eq!(
        json!(report.summary().pools[0].raw_duration_millis),
        oracle("sum-overflow")["rawDuration"]
    );
}
struct DelayedReporter {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    cancellation: Cancellation,
}
impl Reporter for DelayedReporter {
    fn generate<'a>(&'a self, _: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            assert_worker();
            self.entered.lock().unwrap().take().unwrap().send(()).unwrap();
            let release = self.release.lock().unwrap().take().unwrap();
            release.await.unwrap();
            Err(OperationError::Cancelled(self.cancellation.clone()))
        })
    }
}
#[test]
fn reporter_close_joins_owned_work_and_preserves_typed_cancellation() {
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let cancellation = Cancellation::Timeout {
                scope: 99,
                detail: fail(),
            };
            let generator = ExecutionReportGenerator::new(vec![Arc::new(DelayedReporter {
                entered: Mutex::new(Some(entered_tx)),
                release: Mutex::new(Some(release_rx)),
                cancellation: cancellation.clone(),
            })]);
            let io = core.vendor();
            let (close, ()) = futures_util::join!(close_generator(io.as_ref(), &generator), async move {
                entered_rx.await.unwrap();
                assert_eq!(std::thread::current().name(), Some("marathon-core"));
                release_tx.send(()).unwrap();
            });
            assert_eq!(close, Err(OperationError::Cancelled(cancellation)));
            Ok(())
        })
    })
    .unwrap();
}

#[test]
fn dropping_preparation_cancels_borrowed_body_without_emitting_an_event() {
    use std::sync::atomic::{AtomicBool, Ordering};
    CoreExecutor::run(|core| {
        Box::pin(async move {
            for provider in [false, true] {
                let label = if provider {
                    "core.Track.trackProviderDevicePreparing"
                } else {
                    "core.Track.trackDevicePreparing"
                };
                let mut tape = Tape::default();
                tape.reads.insert(label.into(), vec![10]);
                let hooks = RuntimeHooks::taped(tape, &core.clock());
                let reports = Arc::new(Mutex::new(vec![]));
                let generator = Arc::new(ExecutionReportGenerator::new(vec![Arc::new(RecordReporter {
                    calls: Arc::default(),
                    name: "report",
                    fail: false,
                    reports: reports.clone(),
                })]));
                let track = Track::new(core.vendor(), hooks.clone());
                track.add(Arc::new(MappingTracker::new(hooks.clone(), generator.clone())));
                let dropped = AtomicBool::new(false);
                let completed = AtomicBool::new(false);
                let serial_read = AtomicBool::new(false);
                let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
                let (release_tx, release_rx) = tokio::sync::oneshot::channel();
                let body = Box::pin(async {
                    let _guard = DropFlag(&dropped);
                    checked(entered_tx.send(()));
                    checked(release_rx.await);
                    completed.store(true, Ordering::SeqCst);
                    Ok(())
                });
                let serial = || {
                    serial_read.store(true, Ordering::SeqCst);
                    "serial".into()
                };
                let mut caller: VendorFuture<'_, ()> = if provider {
                    Box::pin(track.track_provider_device_preparing(serial, body))
                } else {
                    Box::pin(track.track_device_preparing(serial, body))
                };
                match futures_util::future::select(caller.as_mut(), entered_rx).await {
                    futures_util::future::Either::Left((result, _)) => {
                        panic!("body returned before release: {result:?}")
                    }
                    futures_util::future::Either::Right((entered, _)) => checked(entered),
                }
                drop(caller);
                assert!(dropped.load(Ordering::SeqCst));
                assert!(release_tx.send(()).is_err());
                assert!(!completed.load(Ordering::SeqCst));
                assert!(!serial_read.load(Ordering::SeqCst));
                close_generator(core.vendor().as_ref(), &generator).await?;
                let report = reports.lock().unwrap()[0].clone();
                assert!(report.device_preparing_events().is_empty());
                assert!(report.device_provider_preparing_events().is_empty());
                hooks.verify_consumed().unwrap();
            }
            Ok(())
        })
    })
    .unwrap();
}

#[test]
fn preparation_reads_the_updated_serial_after_the_borrowed_body() {
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let reports = Arc::new(Mutex::new(vec![]));
            let generator = Arc::new(ExecutionReportGenerator::new(vec![Arc::new(RecordReporter {
                calls: Arc::default(),
                name: "report",
                fail: false,
                reports: reports.clone(),
            })]));
            let hooks = RuntimeHooks::live();
            let track = Track::new(core.vendor(), hooks.clone());
            track.add(Arc::new(MappingTracker::new(hooks, generator.clone())));
            let serial = Mutex::new(String::from("adbSerial"));
            let value = String::from("borrowed result");
            for provider in [false, true] {
                *serial.lock().unwrap() = "adbSerial".into();
                let read_serial = || serial.lock().unwrap().clone();
                let body = Box::pin(async {
                    *serial.lock().unwrap() = "realSerialNumber".into();
                    Ok(value.as_str())
                });
                let result = if provider {
                    track.track_provider_device_preparing(read_serial, body).await
                } else {
                    track.track_device_preparing(read_serial, body).await
                };
                assert_eq!(result, Ok(value.as_str()));
            }
            close_generator(core.vendor().as_ref(), &generator).await?;
            let report = reports.lock().unwrap()[0].clone();
            assert_eq!(
                report.device_preparing_events()[0].serial_number,
                "realSerialNumber".to_owned()
            );
            assert_eq!(
                report.device_provider_preparing_events()[0].serial_number,
                "realSerialNumber".to_owned()
            );
            Ok(())
        })
    })
    .unwrap();
}

#[test]
fn public_pipeline_hops_once_per_event_and_close() {
    use std::sync::atomic::Ordering;
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let io = CountingRuntime::new(core.vendor());
            let hooks = RuntimeHooks::live();
            let track = Arc::new(Track::new(io.clone(), hooks.clone()));
            let calls = Arc::new(Mutex::new(vec![]));
            let pipeline = TrackerFactory {
                io: io.clone(),
                track,
                clock: hooks,
                reporters: vec![ReporterRegistration {
                    slot: ReporterSlot::Stdout,
                    reporter: Arc::new(RecordReporter {
                        calls: calls.clone(),
                        name: "report",
                        fail: false,
                        reports: Arc::default(),
                    }),
                }],
                backend: Some(BackendRegistration {
                    request: BackendRequest::Graphite { port: None },
                    factory: Arc::new(Backend {
                        calls: calls.clone(),
                        outcome: 0,
                    }),
                }),
                logger: MarathonLogging::default().logger("TrackerFactory", None),
                log: Arc::new(|_, _, _, _| assert_worker()),
            }
            .create()
            .await?;
            io.spawns.store(0, Ordering::SeqCst);
            let tracker = pipeline.tracker();
            let (pool, device, result) = (pool("myPool"), device(), result("x", TestStatus::Passed));
            let instant = instant_of(0, 0).unwrap();
            tracker.device_connected(&pool, &device).await.unwrap();
            assert_eq!(io.spawns.load(Ordering::SeqCst), 1);
            tracker.device_disconnected(&pool, &device).await.unwrap();
            assert_eq!(io.spawns.load(Ordering::SeqCst), 2);
            tracker
                .device_preparing(&device.serial_number, instant, instant)
                .await
                .unwrap();
            assert_eq!(io.spawns.load(Ordering::SeqCst), 3);
            tracker
                .device_provider_init(&device.serial_number, instant, instant)
                .await
                .unwrap();
            assert_eq!(io.spawns.load(Ordering::SeqCst), 4);
            tracker.test(&pool, &device, &result, true).await.unwrap();
            assert_eq!(io.spawns.load(Ordering::SeqCst), 5);
            pipeline.reports().close().await?;
            assert_eq!(io.spawns.load(Ordering::SeqCst), 6);
            assert_eq!(io.blocking.load(Ordering::SeqCst), 0);
            assert_eq!(
                *calls.lock().unwrap(),
                ["setup", "remote-test", "report", "remote-close"]
            );
            Ok(())
        })
    })
    .unwrap();
}
/// D51: the JVM row still shows Kotlin stopping at the first reporter (C11). Later reporters now
/// run, and a repeated explicit close still generates again.
#[test]
fn reporter_error_does_not_stop_later_reporters_and_is_not_a_once_guard() {
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let calls = Arc::new(Mutex::new(vec![]));
            let reports = Arc::new(Mutex::new(vec![]));
            let generator = ExecutionReportGenerator::new(vec![
                Arc::new(RecordReporter {
                    calls: calls.clone(),
                    name: "bad",
                    fail: true,
                    reports: reports.clone(),
                }),
                Arc::new(RecordReporter {
                    calls: calls.clone(),
                    name: "never",
                    fail: false,
                    reports,
                }),
            ]);
            let error = close_generator(core.vendor().as_ref(), &generator).await.unwrap_err();
            let OperationError::Failed(error) = error else {
                panic!("expected ordinary failure")
            };
            assert_eq!(json!(error.kind.legacy_class()), oracle("reporter-failure")["error"]);
            assert_eq!(oracle("reporter-failure")["calls"], json!(["bad"]));
            assert_eq!(*calls.lock().unwrap(), ["bad", "never"]);
            assert!(close_generator(core.vendor().as_ref(), &generator).await.is_err());
            assert_eq!(*calls.lock().unwrap(), ["bad", "never", "bad", "never"]);
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn a_later_failed_reporter_is_logged_and_the_first_failure_is_returned() {
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let calls = Arc::new(Mutex::new(vec![]));
            let logged = Arc::new(Mutex::new(vec![]));
            let captured = logged.clone();
            let log: marathon_core::ServiceLog = Arc::new(move |_, level, message, cause| {
                captured.lock().unwrap().push((
                    level,
                    message.to_owned(),
                    cause.is_some_and(|c: &Failure| c.message == Some("second".into())),
                ));
            });
            struct Fails(&'static str, Arc<Mutex<Vec<String>>>);
            impl Reporter for Fails {
                fn generate<'a>(&'a self, _: &'a ExecutionReport) -> ServiceFuture<'a, ()> {
                    Box::pin(async move {
                        self.1.lock().unwrap().push(self.0.into());
                        Err(marathon_core::executor::shared::failure(
                            FailureKind::Injected(InjectedKind::TestClose),
                            self.0,
                        )
                        .into())
                    })
                }
            }
            let generator = ExecutionReportGenerator::new(vec![
                Arc::new(Fails("first", calls.clone())),
                Arc::new(Fails("second", calls.clone())),
            ])
            .with_close_log(log, MarathonLogging::default().logger("TrackerFactory", None));
            let error = close_generator(core.vendor().as_ref(), &generator).await.unwrap_err();
            let OperationError::Failed(error) = error else {
                panic!("expected ordinary failure")
            };
            assert_eq!(error.message, Some("first".into()));
            assert_eq!(*calls.lock().unwrap(), ["first", "second"]);
            assert_eq!(
                *logged.lock().unwrap(),
                [(
                    jcompat::logging::Level::Error,
                    "A later close also failed".to_owned(),
                    true
                )]
            );
            Ok(())
        })
    })
    .unwrap();
}

/// What the guard's backend does on its next `test`.
#[derive(Clone)]
enum Next {
    Pass,
    Fail(&'static str),
    Cancel,
    Hang,
}
struct Scripted {
    next: Mutex<Next>,
    sends: Mutex<u32>,
    closes: Mutex<u32>,
    close_fails: bool,
}
impl Scripted {
    fn new(next: Next, close_fails: bool) -> Arc<Self> {
        Arc::new(Self {
            next: Mutex::new(next),
            sends: Mutex::new(0),
            closes: Mutex::new(0),
            close_fails,
        })
    }
}
impl TrackerInternalAdapter for Scripted {
    fn test<'a>(&'a self, _: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            *self.sends.lock().unwrap() += 1;
            let next = self.next.lock().unwrap().clone();
            match next {
                Next::Pass => Ok(()),
                Next::Fail(text) => Err(VendorError::Other(marathon_core::executor::shared::failure(
                    FailureKind::Injected(InjectedKind::TestBackend),
                    text,
                ))),
                Next::Cancel => Err(VendorError::Cancelled(marathon_core::executor::shared::requested(
                    "stop",
                ))),
                Next::Hang => std::future::pending().await,
            }
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async move {
            *self.closes.lock().unwrap() += 1;
            if self.close_fails {
                Err(
                    marathon_core::executor::shared::failure(FailureKind::Injected(InjectedKind::TestBackend), "close")
                        .into(),
                )
            } else {
                Ok(())
            }
        })
    }
}
type Logged = Arc<Mutex<Vec<(jcompat::logging::Level, String)>>>;
fn guard(backend: Arc<Scripted>, limits: BackendLimits) -> (BackendGuard, Logged) {
    let logged: Logged = Arc::default();
    let captured = logged.clone();
    let guard = BackendGuard::new(
        backend,
        Arc::new(move |_, level, message, _| captured.lock().unwrap().push((level, message.to_owned()))),
        MarathonLogging::default().logger("TrackerFactory", None),
        limits,
    );
    (guard, logged)
}
fn test_event() -> Event {
    Event::Test(TestEvent {
        instant: instant_of(0, 0).unwrap(),
        pool_id: pool("myPool"),
        device: device(),
        test_result: Arc::new(result("x", TestStatus::Passed)),
        final_result: true,
    })
}
fn never_slow() -> BackendLimits {
    BackendLimits {
        send: std::time::Duration::from_secs(3600),
        slow_failure: std::time::Duration::from_secs(3600),
        cool_off: std::time::Duration::from_secs(3600),
    }
}
#[test]
fn a_failing_backend_logs_once_per_kind_and_keeps_being_tried() {
    CoreExecutor::run(|_| {
        Box::pin(async move {
            let backend = Scripted::new(Next::Fail("refused"), false);
            let (guard, logged) = guard(backend.clone(), never_slow());
            for _ in 0..3 {
                guard.track(&test_event()).await.unwrap();
            }
            *backend.next.lock().unwrap() = Next::Fail("reset");
            for _ in 0..3 {
                guard.track(&test_event()).await.unwrap();
            }
            assert_eq!(
                *backend.sends.lock().unwrap(),
                6,
                "a quick failure costs nothing, so it retries"
            );
            let warned = |logged: &Logged| logged.lock().unwrap().len();
            assert_eq!(warned(&logged), 2, "{:?}", logged.lock().unwrap());
            TrackerInternal::close(&guard).await.unwrap();
            assert_eq!(*backend.closes.lock().unwrap(), 1);
            assert_eq!(
                logged.lock().unwrap().last().unwrap().1,
                "The analytics backend lost test metrics: 6 results failed to send, and up to 0 were skipped"
            );
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn a_backend_naming_each_test_in_its_failure_cannot_log_per_test() {
    CoreExecutor::run(|_| {
        Box::pin(async move {
            let backend = Scripted::new(Next::Pass, false);
            let (guard, logged) = guard(backend.clone(), never_slow());
            for text in ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11"] {
                *backend.next.lock().unwrap() = Next::Fail(text);
                guard.track(&test_event()).await.unwrap();
            }
            assert_eq!(logged.lock().unwrap().len(), 8);
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn a_slow_failure_leaves_the_backend_alone_for_the_cool_off() {
    CoreExecutor::run(|_| {
        Box::pin(async move {
            let limits = BackendLimits {
                slow_failure: std::time::Duration::ZERO,
                ..never_slow()
            };
            let backend = Scripted::new(Next::Fail("timed out"), false);
            let (guard, logged) = guard(backend.clone(), limits);
            for _ in 0..5 {
                guard.track(&test_event()).await.unwrap();
            }
            assert_eq!(*backend.sends.lock().unwrap(), 1);
            assert_eq!(logged.lock().unwrap().len(), 2, "the failure, and that it skips");
            TrackerInternal::close(&guard).await.unwrap();
            assert_eq!(
                logged.lock().unwrap().last().unwrap().1,
                "The analytics backend lost test metrics: 1 results failed to send, and up to 4 were skipped"
            );
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn a_backend_that_never_answers_gives_way_and_is_left_alone() {
    CoreExecutor::run(|_| {
        Box::pin(async move {
            let limits = BackendLimits {
                send: std::time::Duration::from_millis(50),
                ..never_slow()
            };
            let backend = Scripted::new(Next::Hang, false);
            let (guard, logged) = guard(backend.clone(), limits);
            for _ in 0..3 {
                guard.track(&test_event()).await.unwrap();
            }
            assert_eq!(*backend.sends.lock().unwrap(), 1);
            assert_eq!(logged.lock().unwrap().len(), 2, "the timeout, and that it skips");
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn the_backend_is_tried_again_when_the_cool_off_ends() {
    CoreExecutor::run(|_| {
        Box::pin(async move {
            let limits = BackendLimits {
                slow_failure: std::time::Duration::ZERO,
                cool_off: std::time::Duration::ZERO,
                ..never_slow()
            };
            let backend = Scripted::new(Next::Fail("timed out"), false);
            let (guard, _) = guard(backend.clone(), limits);
            for _ in 0..3 {
                guard.track(&test_event()).await.unwrap();
            }
            assert_eq!(*backend.sends.lock().unwrap(), 3);
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn a_healthy_backend_is_silent_and_a_failed_close_still_reports() {
    CoreExecutor::run(|_| {
        Box::pin(async move {
            let backend = Scripted::new(Next::Pass, true);
            let (guard, logged) = guard(backend.clone(), BackendLimits::default());
            guard.track(&test_event()).await.unwrap();
            assert!(TrackerInternal::close(&guard).await.is_err());
            assert!(logged.lock().unwrap().is_empty());
            Ok(())
        })
    })
    .unwrap();
}
/// S-x34-1: a close that never finishes, like a final flush to a server that trickles its answer.
#[test]
fn a_close_that_never_finishes_gives_way_and_is_logged_once() {
    struct NeverCloses;
    impl TrackerInternalAdapter for NeverCloses {
        fn close(&self) -> ServiceFuture<'_, ()> {
            Box::pin(std::future::pending())
        }
    }
    CoreExecutor::run(|_| {
        Box::pin(async move {
            let logged: Logged = Arc::default();
            let captured = logged.clone();
            let guard = BackendGuard::new(
                Arc::new(NeverCloses),
                Arc::new(move |_, level, message, _| captured.lock().unwrap().push((level, message.to_owned()))),
                MarathonLogging::default().logger("TrackerFactory", None),
                BackendLimits {
                    send: std::time::Duration::from_millis(50),
                    ..never_slow()
                },
            );
            guard.track(&test_event()).await.unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(60), TrackerInternal::close(&guard))
                    .await
                    .expect("the close gives way at its limit")
                    .is_ok(),
                "an expired close leaves the exit code alone"
            );
            assert_eq!(
                *logged.lock().unwrap(),
                vec![(
                    jcompat::logging::Level::Warn,
                    "The analytics backend did not close in 50 ms; test metrics it still held are lost".to_owned()
                )]
            );
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn cancellation_passes_through_the_backend_guard() {
    CoreExecutor::run(|_| {
        Box::pin(async move {
            let backend = Scripted::new(Next::Cancel, false);
            let (guard, logged) = guard(backend, never_slow());
            assert!(matches!(
                guard.track(&test_event()).await,
                Err(VendorError::Cancelled(_))
            ));
            assert!(logged.lock().unwrap().is_empty());
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn public_notifications_allocate_distinct_occurrences_and_exhausted_tape_never_falls_back() {
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let mut tape = Tape {
                wall: ClockMode::Frozen(0),
                ..Tape::default()
            };
            tape.reads.insert("core.MappingTracker.test".into(), vec![1, 2]);
            let hooks = RuntimeHooks::taped(tape, &core.clock());
            let reports = Arc::new(Mutex::new(vec![]));
            let generator = Arc::new(ExecutionReportGenerator::new(vec![Arc::new(RecordReporter {
                calls: Arc::default(),
                name: "capture",
                fail: false,
                reports: reports.clone(),
            })]));
            let mapping = Track::new(core.vendor(), hooks.clone());
            mapping.add(Arc::new(MappingTracker::new(hooks.clone(), generator.clone())));
            mapping.device_connected(&pool("myPool"), &device()).await.unwrap();
            let r = result("test", TestStatus::Passed);
            mapping.test(&pool("myPool"), &device(), &r, true).await.unwrap();
            mapping.test(&pool("myPool"), &device(), &r, true).await.unwrap();
            assert!(mapping.test(&pool("myPool"), &device(), &r, true).await.is_err());
            assert!(hooks.verify_consumed().is_err());
            close_generator(core.vendor().as_ref(), &generator).await?;
            let reports = reports.lock().unwrap();
            let events = reports[0].test_events();
            assert_eq!(events.len(), 2);
            assert!(!Arc::ptr_eq(&events[0].test_result, &events[1].test_result));
            let summary = &reports[0].summary().pools[0];
            assert_eq!(summary.tests.len(), 2);
            assert_eq!(summary.retries[0].1.len(), 1);
            Ok(())
        })
    })
    .unwrap();
}
