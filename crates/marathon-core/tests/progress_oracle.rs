//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/progress/PoolProgressAccumulator.kt
//! and integrations/ci/CIIntegrationFactory.kt (JVM differential corpus).
#[path = "support/progress.rs"]
mod support;

use marathon_config::schema::{CiConfiguration, ExecutionMode};
use marathon_core::{ci::Ci, execution::progress::TestState, logging::OutputRecord, model::*};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use support::*;
fn snapshot(state: TestState) -> Value {
    let (total, done, running) = state.counters();
    let kind = match state {
        TestState::Added { .. } => "Added",
        TestState::Passing { .. } => "Passing",
        TestState::Failing { .. } => "Failing",
        TestState::Passed { .. } => "Passed",
        TestState::Failed { .. } => "Failed",
    };
    json!({"kind":kind,"total":total,"done":done,"running":running})
}
fn seeded(row: &Value) -> TestState {
    let state = &row["before"];
    let total = state["total"].as_i64().unwrap() as i32;
    let done = state["done"].as_i64().unwrap() as i32;
    let running = state["running"].as_i64().unwrap() as i32;
    match state["kind"].as_str().unwrap() {
        "Added" => TestState::Added { total, running },
        "Passing" => TestState::Passing { total, done, running },
        "Failing" => TestState::Failing { total, done, running },
        "Passed" => TestState::Passed { total, done, running },
        _ => TestState::Failed { total, done, running },
    }
}

#[test]
fn every_state_event_mode_and_fast_setting_replays_the_jvm() {
    run(|core| {
        Box::pin(async move {
            for line in include_str!("../../../fixtures/oracle/progress/pool.jsonl").lines() {
                let row: Value = serde_json::from_str(line).unwrap();
                let mut original = test("method");
                original.meta_properties.push(MetaProperty {
                    name: "original".into(),
                    values: MetaMap::default(),
                });
                let callback = test(if row["unknown"] == true { "unknown" } else { "method" });
                let d = device("1");
                let rd = device("result");
                let mode = if row["mode"] == "ANY_SUCCESS" {
                    ExecutionMode::AnySuccess
                } else {
                    ExecutionMode::AllSuccess
                };
                core.log_queue().take_output();
                let mut h = Harness::new(&core, vec![original.clone(); 3], mode, row["fast"].as_bool().unwrap());
                h.reporter.replace_state_for_test(&original, seeded(&row));
                h.capture
                    .fail
                    .store(row["failTracker"].as_bool().unwrap(), Ordering::SeqCst);
                let event = row["event"].as_str().unwrap();
                let mut action = None;
                let mut error = None;
                match event {
                    "started" => {
                        if let Err(e) = h.reporter.test_started(&d, &callback, h.signal.clone()).await {
                            error = Some(e);
                        }
                    }
                    "retry" => action = h.retry(&callback),
                    "remove" => action = h.remove(&callback, 1),
                    "removeNegative" => action = h.remove(&callback, -1),
                    "removeMax" => action = h.remove(&callback, i32::MAX),
                    _ => {
                        let status = match event {
                            "failed" => TestStatus::Failure,
                            "ignored" => TestStatus::Ignored,
                            "assumption" => TestStatus::AssumptionFailure,
                            "incomplete" | "finalIncomplete" => TestStatus::Incomplete,
                            _ => TestStatus::Passed,
                        };
                        match h
                            .reporter
                            .test_ended(
                                &d,
                                &result(&callback, &rd, "batch", status, 10, 20),
                                event == "finalIncomplete",
                                h.signal.clone(),
                            )
                            .await
                        {
                            Ok(a) => action = a,
                            Err(e) => error = Some(e),
                        }
                    }
                }
                let aggregate = h.aggregate();
                assert_eq!(snapshot(h.reporter.state(&original).unwrap()), row["after"], "{line}");
                assert_eq!(json!(action.map(|_| "Complete")), row["action"], "{line}");
                assert_eq!(json!(error.is_none()), row["returned"], "{line}");
                let error = error.map(|e| e.failure().to_string());
                let expected = row["error"]
                    .as_str()
                    .map(|value| marathon_test_support::recorded_errors::rendered(value).unwrap());
                assert_eq!(json!(error), json!(expected), "{line}");
                assert_eq!(
                    json!(h.reporter.progress().to_bits() as i32),
                    row["progressBits"],
                    "{line}"
                );
                assert_eq!(json!(aggregate), row["aggregate"], "{line}");
                let stdout = h.raw().iter().cloned().collect::<String>();
                assert_eq!(json!(stdout), row["stdout"], "{line}");
                let effects = h.capture.effects.lock().unwrap().clone();
                let mut logs = Vec::new();
                let mut tracks = Vec::new();
                let mut order = Vec::new();
                for e in effects {
                    match e {
                        Effect::Output(OutputRecord::Log(log)) => {
                            let log = json!({"logger":log.logger,"level":match log.level {marathon_logging::Level::WARN=>"WARN",marathon_logging::Level::DEBUG=>"DEBUG",_=>"ERROR"},"message":log.message});
                            order.push(json!({"log":log}));
                            logs.push(log);
                        }
                        Effect::Track(pool, d, r, final_result) => {
                            let track = json!({"pool":pool.name,"device":d.serial_number,"resultDevice":r.device.serial_number,"test":r.test.method,"metadata":r.test.meta_properties.iter().map(|m|m.name.clone()).collect::<Vec<_>>(),"status":match r.status {TestStatus::Failure=>"FAILURE",TestStatus::Passed=>"PASSED",TestStatus::Ignored=>"IGNORED",TestStatus::AssumptionFailure=>"ASSUMPTION_FAILURE",TestStatus::Incomplete=>"INCOMPLETE"},"batch":r.test_batch_id,"start":r.start_time,"end":r.end_time,"final":final_result});
                            order.push(json!({"track":track}));
                            tracks.push(track);
                        }
                        _ => {}
                    }
                }
                for log in core.log_queue().take() {
                    let log = json!({"logger":log.logger,"level":if log.level==marathon_logging::Level::WARN {"WARN"}else{"ERROR"},"message":log.message});
                    order.push(json!({"log":log}));
                    logs.push(log);
                }
                assert_eq!(
                    json!(logs),
                    marathon_test_support::recorded_logs::native_logs(&row["logs"]),
                    "{line}"
                );
                assert_eq!(json!(tracks), row["tracks"], "{line}");
                assert_eq!(
                    json!(order),
                    marathon_test_support::recorded_logs::native_logs(&row["effects"]),
                    "{line}"
                );
            }
            Ok(())
        })
    });
}
#[test]
fn auto_ci_checks_values_and_teamcity_bytes_are_literal() {
    for line in include_str!("../../../fixtures/oracle/progress/ci.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let env: Vec<(String, String)> = row["environment"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_str().unwrap().into(), p[1].as_str().unwrap().into()))
            .collect();
        let ci = Ci::from_config(CiConfiguration::Auto, &env);
        assert_eq!(
            ci.build_progress(-1).unwrap_or_default(),
            row["stdout"].as_str().unwrap()
        );
        assert_eq!(
            if ci == Ci::None { "None" } else { "Teamcity" },
            row["selected"].as_str().unwrap()
        );
    }
    assert_eq!(Ci::from_config(CiConfiguration::None, &[]), Ci::None);
    assert_eq!(Ci::from_config(CiConfiguration::Teamcity, &[]), Ci::Teamcity);
    assert_eq!(Ci::None.build_progress(2), None);
    assert_eq!(
        Ci::Teamcity.build_progress(-1),
        Some("##teamcity[buildStatus text='Marathon run: -1 %']\n".into())
    );
}
#[test]
fn empty_reduce_nan_and_rounding_match_the_jvm() {
    let row: Value =
        serde_json::from_str(include_str!("../../../fixtures/oracle/progress/edges.jsonl").trim()).unwrap();
    for case in row["round"].as_array().unwrap() {
        let value = f32::from_bits(case["bits"].as_i64().unwrap() as u32) * 100.0;
        let actual = jcompat::jnum::kotlin_round_to_int(value)
            .map(|n| n.to_string())
            .unwrap_or_else(|| "invalid argument: Cannot round NaN value.".into());
        let expected = case["rounded"].as_str().unwrap();
        let expected = if value.is_nan() {
            marathon_test_support::recorded_errors::rendered(expected).unwrap()
        } else {
            expected.to_owned()
        };
        assert_eq!(actual, expected);
    }
    run(move |core| {
        Box::pin(async move {
            let h = Harness::new(&core, vec![], ExecutionMode::AnySuccess, false);
            assert!(h.reporter.progress().is_nan());
            assert_eq!(
                h.reporter.aggregate_result().unwrap_err().failure().to_string().clone(),
                marathon_test_support::recorded_errors::rendered(row["emptyAggregate"].as_str().unwrap()).unwrap()
            );
            Ok(())
        })
    });
}

#[test]
fn ended_only_and_terminal_retry_keep_the_source_bookkeeping() {
    use marathon_config::schema::RetryStrategyConfiguration;
    use marathon_core::strategy::retry::RetryStrategy;
    run(|core| {
        Box::pin(async move {
            let t = test("method");
            let d = device("1");
            let mut h = Harness::with_ci(&core, vec![t.clone()], ExecutionMode::AnySuccess, false, Ci::Teamcity);
            assert_eq!(
                h.end(result(&t, &d, "1", TestStatus::Passed, 0, 1), false).await,
                Some(TestAction::Complete)
            );
            assert_eq!(h.reporter.state(&t).unwrap().counters(), (1, 1, -1));
            assert_eq!(h.retry(&t), Some(TestAction::Complete));
            assert_eq!(h.reporter.state(&t).unwrap().counters(), (1, 1, -1));
            assert_eq!(
                h.raw(),
                vec![String::from("00% | [test]-[1] com.example.Class#method passed\n")]
            );
            h.end(result(&t, &d, "2", TestStatus::Failure, 1, 2), false).await;
            assert!(h.aggregate());
            let tracks = h
                .capture
                .effects
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    Effect::Track(_, _, _, f) => Some(*f),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(tracks, vec![true, false]);
            let mut pending = Harness::new(&core, vec![t.clone()], ExecutionMode::AnySuccess, false);
            let mut retry = RetryStrategy::from_config(&RetryStrategyConfiguration::FixedQuota {
                total_allowed_retry_quota: 0,
                retry_per_test_quota: 3,
            });
            let failed = result(&t, &d, "1", TestStatus::Failure, 0, 1);
            let shard = TestShard {
                tests: vec![t.clone()],
                flaky_tests: vec![],
            };
            assert!(
                retry
                    .process(
                        &DevicePoolId { name: "test".into() },
                        std::slice::from_ref(&failed),
                        &shard,
                        &mut pending.reporter
                    )
                    .is_empty()
            );
            assert_eq!(pending.reporter.state(&t).unwrap().counters(), (2, 0, 0));
            pending.end(failed, false).await;
            assert_eq!(pending.reporter.state(&t).unwrap().counters(), (2, 1, -1));
            Ok(())
        })
    });
}
#[test]
fn started_ci_passes_fraction_and_prints_after_ci() {
    run(|core| {
        Box::pin(async move {
            let t = test("method");
            let d = device("1");
            let mut h = Harness::with_ci(
                &core,
                vec![t.clone(); 2],
                ExecutionMode::AnySuccess,
                false,
                Ci::Teamcity,
            );
            h.end(result(&t, &d, "1", TestStatus::Passed, 0, 1), false).await;
            h.start(&d, &t).await;
            assert_eq!(
                h.raw(),
                vec![
                    String::from("00% | [test]-[1] com.example.Class#method passed\n"),
                    String::from("##teamcity[buildStatus text='Marathon run: 0 %']\n"),
                    String::from("50% | [test]-[1] com.example.Class#method started\n")
                ]
            );
            Ok(())
        })
    });
}
#[test]
fn grouping_retains_first_metadata_and_name_collisions_overwrite() {
    use marathon_core::{
        execution::progress::{PoolProgressAccumulator, ProgressServices},
        output::OutputDriver,
    };
    use std::sync::Arc;
    run(|core| {
        Box::pin(async move {
            let mut first = test("method");
            first.meta_properties.push(MetaProperty {
                name: "first".into(),
                values: MetaMap::default(),
            });
            let mut alias = first.clone();
            alias.meta_properties[0].name = "second".into();
            let capture = Arc::new(Capture::default());
            let mut p = PoolProgressAccumulator::new(
                DevicePoolId { name: "test".into() },
                &TestShard {
                    tests: vec![first.clone()],
                    flaky_tests: vec![alias.clone()],
                },
                marathon_config::schema::ExecutionStrategyConfiguration {
                    mode: ExecutionMode::AnySuccess,
                    fast: false,
                },
                Ci::None,
                ProgressServices {
                    tracker: capture.clone(),
                    logs: core.log_queue(),
                    clock: core.clock(),
                    output: OutputDriver {
                        vendor: core.vendor(),
                        sink: capture.clone(),
                    },
                },
            );
            assert_eq!(p.state(&alias).unwrap().counters(), (2, 0, 0));
            let d = device("1");
            p.test_ended(
                &d,
                &result(&alias, &d, "b", TestStatus::Passed, 0, 1),
                false,
                marathon_core::executor::Signal::new(),
            )
            .await
            .unwrap();
            let effects = capture.effects.lock().unwrap();
            let r = effects
                .iter()
                .find_map(|e| match e {
                    Effect::Track(_, _, r, _) => Some(r),
                    _ => None,
                })
                .unwrap();
            assert_eq!(r.test.meta_properties[0].name, String::from("first"));
            drop(effects);
            let mut collision = first.clone();
            collision.pkg = "com".into();
            collision.clazz = "example.Class".into();
            assert_ne!(first, collision);
            assert_eq!(first.to_test_name('.', '#'), collision.to_test_name('.', '#'));
            let h = Harness::new(
                &core,
                vec![first.clone(), first.clone(), collision],
                ExecutionMode::AnySuccess,
                false,
            );
            assert_eq!(h.reporter.state(&first).unwrap().counters(), (1, 0, 0));
            Ok(())
        })
    });
}

#[test]
fn output_batches_keep_source_order_when_vendor_first_polls_them_backwards() {
    use marathon_core::{error::VendorResult, output::OutputDriver, runtime::*};
    use std::{
        future::Future,
        pin::Pin,
        sync::{Arc, atomic::AtomicUsize},
        task::{Context, Poll},
    };
    struct AfterPoll {
        body: VendorFuture<'static, ()>,
        started: Arc<tokio::sync::Notify>,
    }
    impl Future for AfterPoll {
        type Output = VendorResult<()>;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let result = self.body.as_mut().poll(cx);
            self.started.notify_one();
            result
        }
    }
    struct Backwards {
        inner: Arc<dyn VendorRuntime>,
        count: AtomicUsize,
        started: Arc<tokio::sync::Notify>,
    }
    impl VendorRuntime for Backwards {
        fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask> {
            self.inner.spawn_cancellable(body)
        }
        fn spawn_blocking(&self, body: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask> {
            self.inner.spawn_blocking(body)
        }
        fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
            let started = self.started.clone();
            if self.count.fetch_add(1, Ordering::SeqCst) == 0 {
                self.inner.spawn(Box::pin(async move {
                    started.notified().await;
                    body.await
                }))
            } else {
                self.inner.spawn(Box::pin(AfterPoll { body, started }))
            }
        }
    }
    run(|core| {
        Box::pin(async move {
            let capture = Arc::new(Capture::default());
            let output = OutputDriver {
                vendor: Arc::new(Backwards {
                    inner: core.vendor(),
                    count: AtomicUsize::new(0),
                    started: Arc::new(tokio::sync::Notify::new()),
                }),
                sink: capture.clone(),
            };
            let queue = core.log_queue();
            queue.raw("first".into());
            let signal = marathon_core::executor::Signal::new();
            let (a, b) = futures_util::join!(output.flush(&queue, signal.clone()), async {
                queue.raw("second".into());
                output.flush(&queue, signal.clone()).await
            });
            assert!(a.is_ok());
            assert!(b.is_ok());
            assert_eq!(
                *capture.effects.lock().unwrap(),
                vec![
                    Effect::Output(OutputRecord::Raw("first".into())),
                    Effect::Output(OutputRecord::Raw("second".into()))
                ]
            );
            Ok(())
        })
    });
}

#[test]
fn tracker_cancellation_preserves_the_stored_state_and_callback_payload() {
    use marathon_core::error::VendorError;
    run(|core| {
        Box::pin(async move {
            let t = test("method");
            let d = device("1");
            let mut h = Harness::new(&core, vec![t.clone()], ExecutionMode::AnySuccess, false);
            h.capture.cancel.store(true, Ordering::SeqCst);
            let mut r = result(&t, &d, "batch", TestStatus::Passed, -10, 20);
            r.stacktrace = Some("trace".into());
            r.attachments.push(Attachment {
                file: std::path::PathBuf::from("/tmp/attachment"),
                kind: AttachmentType::Log,
                name: Some("log".into()),
            });
            let failure = h
                .reporter
                .test_ended(&d, &r, false, h.signal.clone())
                .await
                .unwrap_err();
            assert!(matches!(failure, VendorError::Cancelled(_)));
            assert!(matches!(h.reporter.state(&t), Some(TestState::Passed { .. })));
            let effects = h.capture.effects.lock().unwrap();
            assert!(matches!(&effects[0], Effect::Output(OutputRecord::Raw(_))));
            let Effect::Track(_, _, tracked, final_result) = &effects[1] else {
                panic!("expected tracker payload")
            };
            assert_eq!(**tracked, r);
            assert!(*final_result);
            Ok(())
        })
    });
}
#[test]
fn twenty_thousand_tests_construct_and_transition_in_linear_time() {
    // Review round 1: linear Vec scans made this quadratic (minutes in debug builds).
    run(|core| {
        Box::pin(async move {
            let tests: Vec<Test> = (0..20_000).map(|i| test(&format!("m{i}"))).collect();
            let mut shard = tests.clone();
            shard.extend(tests.iter().cloned());
            let started = std::time::Instant::now();
            let mut h = Harness::new(&core, shard, ExecutionMode::AnySuccess, false);
            for t in &tests {
                assert_eq!(h.reporter.state(t).unwrap().counters(), (2, 0, 0));
                assert_eq!(h.retry(t), None);
                assert_eq!(h.remove(t, 1), None);
            }
            let d = device("1");
            for t in tests.iter().take(100) {
                h.end(result(t, &d, "1", TestStatus::Passed, 0, 1), false).await;
            }
            assert_eq!(h.reporter.state(&tests[0]).unwrap().counters(), (2, 1, -1));
            assert!(h.reporter.progress() > 0.0);
            let elapsed = started.elapsed();
            assert!(elapsed < std::time::Duration::from_secs(10), "{elapsed:?}");
            Ok(())
        })
    });
}

#[test]
fn progress_line_contract_for_each_status() {
    run(|core| {
        Box::pin(async move {
            for (status, label) in [
                (TestStatus::Passed, "passed"),
                (TestStatus::Failure, "failed"),
                (TestStatus::Ignored, "ignored"),
                (TestStatus::AssumptionFailure, "ignored"),
                (TestStatus::Incomplete, "incomplete"),
            ] {
                let t = test("method");
                let d = device("1");
                let mut h = Harness::new(&core, vec![t.clone()], ExecutionMode::AnySuccess, false);
                h.end(result(&t, &d, "1", status, 0, 1), false).await;
                assert_eq!(
                    h.raw(),
                    vec![format!("00% | [test]-[1] com.example.Class#method {label}\n")]
                );
            }
            Ok(())
        })
    });
}
