//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/result/TestRunResultsAccumulatorTest.kt
#[path = "support/progress.rs"]
mod support;

use marathon_core::{execution::result::*, logging::host_millis, model::*, time::*};
use std::sync::{Arc, Mutex};
use support::{device, test};
struct ScriptTimer {
    values: Mutex<Vec<i64>>,
    reads: Mutex<usize>,
}
impl ScriptTimer {
    fn new(values: Vec<i64>) -> Arc<Self> {
        Arc::new(Self {
            values: Mutex::new(values.into_iter().rev().collect()),
            reads: Mutex::new(0),
        })
    }
}
impl Timer for ScriptTimer {
    fn start_time_millis(&self) -> i64 {
        0
    }
    fn elapsed_time_millis(&self) -> ClockResult<i64> {
        self.current_time_millis()
    }
    fn current_time_millis(&self) -> ClockResult<i64> {
        *self.reads.lock().unwrap() += 1;
        self.values.lock().unwrap().pop().ok_or_else(|| ReplayError {
            label: "core.di.Clock".into(),
            message: "exhausted".into(),
        })
    }
    fn measure(&self, block: &mut dyn FnMut()) -> ClockResult<i64> {
        let start = self.current_time_millis()?;
        block();
        Ok(self.current_time_millis()?.wrapping_sub(start))
    }
}
fn metrics(key: &str, value: &str) -> Metrics {
    vec![(key.into(), value.into())].into()
}
fn zero() -> TestRunResultsAccumulator {
    TestRunResultsAccumulator::new(ScriptTimer::new(vec![0; 30]), Arc::new(host_millis)).unwrap()
}
fn temporal(status: TestStatus, trace: Option<&str>, m: Option<Metrics>) -> TemporalTestResult {
    TemporalTestResult {
        status,
        start_time: 0,
        end_time: 0,
        stack_trace: trace.map(String::from),
        metrics: m,
    }
}
#[test]
fn test_default() {
    let mut a = zero();
    let t: Vec<_> = (1..=5).map(|i| test(&format!("method{i}"))).collect();
    a.test_run_started("testing".into(), 3);
    a.test_started(&t[0], None).unwrap();
    a.test_ended(&t[0], metrics("metric", "value"), None, None).unwrap();
    a.test_started(&t[1], None).unwrap();
    a.test_failed(&t[1], "trace", None, None).unwrap();
    a.test_started(&t[2], None).unwrap();
    a.test_ignored(&t[2]).unwrap();
    a.test_started(&t[3], None).unwrap();
    a.test_assumption_failure(&t[3], "trace").unwrap();
    a.test_started(&t[4], None).unwrap();
    a.test_run_ended(1234, metrics("metric1", "value1")).unwrap();
    assert_eq!(
        a.temporal_test_results.entries(),
        &[
            (
                t[0].clone(),
                temporal(TestStatus::Passed, None, Some(metrics("metric", "value")))
            ),
            (t[1].clone(), temporal(TestStatus::Failure, Some("trace"), None)),
            (t[2].clone(), temporal(TestStatus::Ignored, None, None)),
            (
                t[3].clone(),
                temporal(TestStatus::AssumptionFailure, Some("trace"), None)
            ),
            (t[4].clone(), temporal(TestStatus::Incomplete, None, None)),
        ]
    );
    assert!(a.is_run_complete);
    assert!(a.has_failed_tests());
    assert!(!a.is_run_failure());
    assert_eq!(a.num_complete_tests(), 4);
    assert_eq!(a.completed_tests(), t[..4]);
    assert_eq!(a.num_tests_in_state(TestStatus::Passed), 1);
    assert_eq!(a.elapsed_time(), 1234);
}
#[test]
fn test_run_failed() {
    let mut a = zero();
    let t = test("method1");
    a.test_run_started("testing".into(), 3);
    a.test_started(&t, None).unwrap();
    a.test_ended(&t, metrics("metric", "value"), None, None).unwrap();
    a.test_run_failed("Problems are everywhere".into()).unwrap();
    assert_eq!(
        a.temporal_test_results.entries(),
        &[(t, temporal(TestStatus::Passed, None, Some(metrics("metric", "value"))))]
    );
    assert!(!a.is_run_complete);
    assert!(!a.has_failed_tests());
    assert!(a.is_run_failure());
    assert_eq!(a.num_tests_in_state(TestStatus::Failure), 0);
    assert_eq!(a.run_failure_message(), Some("Problems are everywhere"));
}
#[test]
fn test_run_stopped() {
    let mut a = zero();
    let t = test("method1");
    a.test_run_started("testing".into(), 3);
    a.test_started(&t, None).unwrap();
    a.test_ended(&t, metrics("metric", "value"), None, None).unwrap();
    a.test_run_stopped(300).unwrap();
    assert_eq!(
        a.temporal_test_results.entries(),
        &[(t, temporal(TestStatus::Passed, None, Some(metrics("metric", "value"))))]
    );
    assert!(a.is_run_complete);
    assert!(!a.has_failed_tests());
    assert!(!a.is_run_failure());
}
#[test]
fn test_only_test_ended() {
    let mut a = zero();
    let t1 = test("method1");
    let t2 = test("method2");
    a.test_run_started("testing".into(), 3);
    a.test_failed(&t1, "trace", None, None).unwrap();
    a.test_ended(&t2, vec![], None, None).unwrap();
    a.test_run_ended(300, vec![]).unwrap();
    assert_eq!(
        a.temporal_test_results.entries(),
        &[
            (t1, temporal(TestStatus::Failure, Some("trace"), None)),
            (t2, temporal(TestStatus::Passed, None, Some(Metrics::default())))
        ]
    );
    assert!(a.is_run_complete);
    assert!(a.has_failed_tests());
    assert!(!a.is_run_failure());
}
#[test]
fn default_end_precedes_missing_start_and_existing_start_argument_is_ignored() {
    let timer = ScriptTimer::new(vec![100, 90, 80, 70]);
    let mut a = TestRunResultsAccumulator::new(timer.clone(), Arc::new(host_millis)).unwrap();
    let t = test("a");
    a.test_ended(&t, vec![], None, None).unwrap();
    assert_eq!(
        (
            a.temporal_test_results.get(&t).unwrap().start_time,
            a.temporal_test_results.get(&t).unwrap().end_time
        ),
        (80, 90)
    );
    a.test_ended(&t, vec![], Some(999), None).unwrap();
    assert_eq!(
        (
            a.temporal_test_results.get(&t).unwrap().start_time,
            a.temporal_test_results.get(&t).unwrap().end_time
        ),
        (80, 70)
    );
    assert_eq!(*timer.reads.lock().unwrap(), 4);
}
#[test]
fn repeated_runs_keep_records_metrics_and_elapsed_and_replacements_keep_first_key() {
    let mut a = zero();
    let mut t = test("a");
    t.meta_properties.push(MetaProperty {
        name: "original".into(),
        values: MetaMap::default(),
    });
    let alias = test("a");
    a.test_started(&t, Some(10)).unwrap();
    a.test_failed(&alias, "trace", Some(11), Some(12)).unwrap();
    a.test_run_ended(i64::MAX, metrics("old", "value")).unwrap();
    a.test_run_failed("oops".into()).unwrap();
    a.test_run_started("again".into(), 0);
    assert_eq!(a.elapsed_time(), i64::MAX);
    assert_eq!(a.run_metrics, metrics("old", "value"));
    assert!(!a.is_run_complete);
    assert!(!a.is_run_failure());
    assert_eq!(a.num_tests(), 1);
    a.test_started(&alias, Some(20)).unwrap();
    let (key, r) = &a.temporal_test_results.entries()[0];
    assert_eq!(key.meta_properties[0].name, String::from("original"));
    assert_eq!(*r, TemporalTestResult::new(20));
    a.test_run_stopped(1).unwrap();
    assert_eq!(a.elapsed_time(), i64::MIN);
}
/// U151: each instrumentation of a batch keeps its own failure for its own tests. A test of no instrumentation that is
/// over has the current failure: its instrumentation is the current one, or never ran as the current one ended the batch.
#[test]
fn a_failure_stays_with_the_tests_of_its_instrumentation_u151() {
    let mut a = TestRunResultsAccumulator::new(ScriptTimer::new(vec![0; 16]), Arc::new(host_millis)).unwrap();
    let d = device("1");
    let batch = TestBatch {
        tests: vec![],
        id: "batch".into(),
    };
    let text = |a: &TestRunResultsAccumulator, name: &str| {
        a.create_uncompleted_test_results(&[test(name)], &batch, &d).unwrap()[0]
            .stacktrace
            .as_ref()
            .unwrap()
            .to_owned()
    };
    const GENERIC: &str =
        "Test didn't complete. Either the test results are missing due to timeout or testing runtime crashed";
    a.start_instrumentation(vec![test("a")]);
    a.test_run_started("run".into(), 1);
    a.test_run_failed("first".into()).unwrap();
    assert_eq!(text(&a, "a"), "first");
    a.start_instrumentation(vec![test("b")]);
    assert!(!a.is_run_failure());
    a.start_instrumentation(vec![test("c")]);
    a.test_run_started("run".into(), 1);
    a.test_run_failed("third".into()).unwrap();
    assert_eq!(text(&a, "a"), "first");
    assert_eq!(text(&a, "b"), GENERIC);
    assert_eq!(text(&a, "c"), "third");
    assert_eq!(text(&a, "never ran"), "third");
}
/// U151 changes nothing for a batch of one instrumentation, or for another vendor: a new run start clears the failure.
#[test]
fn without_instrumentations_a_new_run_clears_the_failure_u151() {
    let mut a = TestRunResultsAccumulator::new(ScriptTimer::new(vec![0; 8]), Arc::new(host_millis)).unwrap();
    let d = device("1");
    let batch = TestBatch {
        tests: vec![],
        id: "batch".into(),
    };
    a.test_run_failed("failed".into()).unwrap();
    let r = a.create_uncompleted_test_results(&[test("a")], &batch, &d).unwrap();
    assert_eq!(r[0].stacktrace, Some("failed".into()));
    a.test_run_started("run".into(), 1);
    let r = a.create_uncompleted_test_results(&[test("a")], &batch, &d).unwrap();
    assert_eq!(
        r[0].stacktrace,
        Some(
            "Test didn't complete. Either the test results are missing due to timeout or testing runtime crashed"
                .into()
        )
    );
}
#[test]
fn synthesis_uses_maximum_even_incomplete_or_zero_and_reads_once_per_requested_test() {
    let timer = ScriptTimer::new(vec![100, 101, 102, 103, 104, 105]);
    let mut a = TestRunResultsAccumulator::new(timer, Arc::new(host_millis)).unwrap();
    let t = test("a");
    let d = device("1");
    let batch = TestBatch {
        tests: vec![t.clone()],
        id: "batch".into(),
    };
    let empty = a
        .create_uncompleted_test_results(std::slice::from_ref(&t), &batch, &d)
        .unwrap();
    assert_eq!((empty[0].start_time, empty[0].end_time), (100, 101));
    a.test_started(&t, Some(900)).unwrap();
    let r = a
        .create_uncompleted_test_results(&[t.clone(), test("b")], &batch, &d)
        .unwrap();
    assert_eq!((r[0].start_time, r[0].end_time, r[1].end_time), (0, 102, 103));
    assert_eq!(a.num_tests(), 1);
    a.temporal_test_results.get_mut(&t).unwrap().end_time = 999;
    a.test_run_failed("crash".into()).unwrap();
    let r = a.create_uncompleted_test_results(&[test("b")], &batch, &d).unwrap();
    assert_eq!((r[0].start_time, r[0].end_time), (999, 104));
    assert_eq!(r[0].stacktrace, Some("crash".into()));
    assert_eq!(r[0].test_batch_id, batch.id);
    assert_eq!(r[0].device, d);
}
#[test]
fn blank_trace_preserves_previous_and_assumptions_have_distinct_success_and_count_predicates() {
    let mut a = zero();
    let t = test("a");
    a.test_failed(&t, "trace", None, None).unwrap();
    a.test_failed(&t, &String::from_utf16_lossy(&vec![0x2003, 0x1c]), None, None)
        .unwrap();
    assert_eq!(
        a.temporal_test_results.get(&t).unwrap().stack_trace,
        Some("trace".into())
    );
    a.test_assumption_failure(&t, "").unwrap();
    assert!(a.temporal_test_results.get(&t).unwrap().is_successful());
    assert!(a.has_failed_tests());
    assert_eq!(
        a.take_logs()[0].message,
        String::from("received test event without test start for com.example.Class.a")
    );
    a.temporal_test_results.get_mut(&t).unwrap().status = TestStatus::Passed;
    assert_eq!(a.num_all_failed_tests(), 0);
}
#[test]
fn missing_start_diagnostic_is_stamped_by_the_injected_wall_source() {
    // Replay overrides the wall clock; the record must not read the host's.
    let mut a = TestRunResultsAccumulator::new(ScriptTimer::new(vec![0; 3]), Arc::new(|| 4242)).unwrap();
    a.test_failed(&test("a"), "trace", None, None).unwrap();
    let logs = a.take_logs();
    assert_eq!((logs.len(), logs[0].wall_millis, logs[0].at_millis), (1, 4242, 0));
}
#[test]
fn clock_exhaustion_propagates_after_prior_mutations() {
    let timer = ScriptTimer::new(vec![1, 2]);
    let mut a = TestRunResultsAccumulator::new(timer, Arc::new(host_millis)).unwrap();
    a.test_started(&test("a"), Some(9)).unwrap();
    a.test_started(&test("b"), Some(9)).unwrap();
    assert!(a.test_run_ended(10, metrics("m", "v")).is_err());
    assert!(a.is_run_complete);
    assert_eq!(a.elapsed_time(), 10);
    assert_eq!(a.temporal_test_results.entries()[0].1.end_time, 2);
    assert_eq!(a.temporal_test_results.entries()[1].1.end_time, 0);
}

#[test]
fn temporal_default_keeps_its_label_and_test_started_consumes_only_timer() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Clock(AtomicUsize);
    impl WallClock for Clock {
        fn millis(&self, label: &'static str) -> ClockResult<i64> {
            assert_eq!(label, "core.TemporalTestResult.startTime");
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(42)
        }
        fn instant(&self, _: &'static str) -> ClockResult<jcompat::jtime::Instant> {
            unreachable!()
        }
    }
    let clock = Clock(AtomicUsize::new(0));
    assert_eq!(TemporalTestResult::from_clock(&clock).unwrap().start_time, 42);
    let timer = ScriptTimer::new(vec![10, 20]);
    let mut a = TestRunResultsAccumulator::new(timer.clone(), Arc::new(host_millis)).unwrap();
    a.test_started(&test("a"), None).unwrap();
    a.test_started(&test("b"), Some(30)).unwrap();
    assert_eq!(*timer.reads.lock().unwrap(), 2);
    assert_eq!(clock.0.load(Ordering::SeqCst), 1);
    fn is_send<T: Send>() {}
    is_send::<TestRunResultsAccumulator>();
}
#[test]
fn model_status_addition_and_result_predicates_match_each_source_branch() {
    use TestStatus::*;
    let statuses = [Failure, Passed, Ignored, Incomplete, AssumptionFailure];
    for lhs in statuses {
        for rhs in statuses {
            assert_eq!(lhs + rhs, if lhs == Passed { rhs } else { lhs });
        }
    }
    let t = test("a");
    let d = device("1");
    for status in statuses {
        let r = support::result(&t, &d, "batch", status, 0, 1);
        assert_eq!(r.is_success(), status == Passed);
        assert_eq!(r.is_ignored(), matches!(status, Ignored | AssumptionFailure));
    }
    let a: Metrics = vec![
        ("a".into(), "old".into()),
        ("b".into(), "v".into()),
        ("a".into(), "new".into()),
    ]
    .into();
    let b: Metrics = vec![("b".into(), "v".into()), ("a".into(), "new".into())].into();
    assert_eq!(a, b);
    assert_eq!(a.entries().len(), 2);
}

#[test]
fn run_lifecycle_replays_the_jvm_snapshots() {
    use serde_json::{Value, json};
    let timer = ScriptTimer::new((100..=140).collect());
    let mut a = TestRunResultsAccumulator::new(timer.clone(), Arc::new(host_millis)).unwrap();
    let mut t = test("method");
    t.meta_properties.push(MetaProperty {
        name: "original".into(),
        values: MetaMap::default(),
    });
    fn m(map: &Metrics) -> Value {
        Value::Object(map.entries().iter().map(|(k, v)| (k.clone(), json!(v))).collect())
    }
    fn status(s: TestStatus) -> &'static str {
        match s {
            TestStatus::Failure => "FAILURE",
            TestStatus::Passed => "PASSED",
            TestStatus::Ignored => "IGNORED",
            TestStatus::Incomplete => "INCOMPLETE",
            TestStatus::AssumptionFailure => "ASSUMPTION_FAILURE",
        }
    }
    for line in include_str!("../../../fixtures/oracle/progress/run.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let op = row["op"].as_str().unwrap();
        match op {
            "new" => {}
            "runStarted" => a.test_run_started("first".into(), 9),
            "missingEnded" => a.test_ended(&t, metrics("metric", "v"), None, None).unwrap(),
            "existingEndedIgnoresStart" => a.test_ended(&t, vec![], Some(999), None).unwrap(),
            "blankTrace" => {
                a.test_failed(&t, "trace", Some(11), Some(12)).unwrap();
                a.test_failed(&t, "\u{2003}", None, None).unwrap();
            }
            "replace" => a.test_started(&test("method"), Some(20)).unwrap(),
            "assumption" => a.test_assumption_failure(&t, "assumed").unwrap(),
            "runFailed" => {
                let mut b = t.clone();
                b.method = "b".into();
                a.test_started(&b, Some(30)).unwrap();
                a.test_run_failed("crash".into()).unwrap();
            }
            "runEnded" => a.test_run_ended(i64::MAX, metrics("old", "v")).unwrap(),
            "restart" => a.test_run_started("second".into(), 0),
            "stopped" => a.test_run_stopped(1).unwrap(),
            "synthesis" => {
                let mut c = t.clone();
                c.method = "c".into();
                let mut d = t.clone();
                d.method = "d".into();
                let results = a
                    .create_uncompleted_test_results(
                        &[c, d],
                        &TestBatch {
                            tests: vec![t.clone()],
                            id: "batch".into(),
                        },
                        &device("1"),
                    )
                    .unwrap();
                let actual = json!({"op":op,"results":results.iter().map(|r|json!({"test":r.test.method,"start":r.start_time,"end":r.end_time,"trace":r.stacktrace.as_ref().map(|s|s),"batch":r.test_batch_id})).collect::<Vec<_>>(),"reads":(100..100+*timer.reads.lock().unwrap() as i64).collect::<Vec<_>>()});
                assert_eq!(actual, row);
                continue;
            }
            _ => panic!("unknown oracle operation"),
        }
        let records=a.temporal_test_results.entries().iter().map(|(t,r)|json!({"test":t.method,"metadata":t.meta_properties.iter().map(|m|m.name.clone()).collect::<Vec<_>>(),"status":status(r.status),"start":r.start_time,"end":r.end_time,"trace":r.stack_trace.as_ref().map(|s|s),"metrics":r.metrics.as_ref().map(m),"successful":r.is_successful()})).collect::<Vec<_>>();
        let counts = [
            TestStatus::Failure,
            TestStatus::Passed,
            TestStatus::Ignored,
            TestStatus::Incomplete,
            TestStatus::AssumptionFailure,
        ]
        .map(|s| a.num_tests_in_state(s));
        let actual = json!({"op":op,"records":records,"name":a.name(),"complete":a.is_run_complete,"failure":a.run_failure_message().map(|s|s),"elapsed":a.elapsed_time(),"metrics":m(&a.run_metrics),"completed":a.completed_tests().iter().map(|t|t.method.clone()).collect::<Vec<_>>(),"failed":a.num_all_failed_tests(),"counts":counts,"reads":(100..100+*timer.reads.lock().unwrap() as i64).collect::<Vec<_>>()});
        assert_eq!(actual, row, "{op}");
    }
}
