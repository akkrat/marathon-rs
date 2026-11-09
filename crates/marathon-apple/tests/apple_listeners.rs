//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/listener/{CompositeTestRunListenerTest,TestResultsListenerTest}.kt
mod support;
use jcompat::logging::{Logger, MarathonLogging};
use marathon_apple::{
    bin::{Tools, xcrun::xcresulttool::Xcresulttool},
    cmd::{
        CommandExecutor, CommandHost, CommandRequest, CommandResult, CommandSession,
        remote::ssh::sshj::config::SshLogging, test_support::FakeCommandExecutor,
    },
    listener::*,
    logparser::parser::DeviceFailureReason,
    model::XcodeVersion,
};
use marathon_config::{
    Configuration,
    vendor::{AppleTimeoutParameters, apple_timeout_configuration},
};
use marathon_core::{
    executor::SharedDeferred,
    listener::AttachmentListener,
    logging::LogQueue,
    model::*,
    runtime::{Deferred, VendorFuture},
    time::{ClockResult, Timer},
    vendor::{Device, DeviceRef, VendorContext},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

struct FixedTimer;
impl Timer for FixedTimer {
    fn start_time_millis(&self) -> i64 {
        1537187696000
    }
    fn elapsed_time_millis(&self) -> ClockResult<i64> {
        Ok(0)
    }
    fn current_time_millis(&self) -> ClockResult<i64> {
        Ok(1537187696000)
    }
    fn measure(&self, f: &mut dyn FnMut()) -> ClockResult<i64> {
        f();
        Ok(0)
    }
}
struct TestDevice(Logger);
impl Device for TestDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            serial_number: "serial".into(),
            operating_system: OperatingSystem { version: "27".into() },
            model: "Mac".into(),
            manufacturer: "Apple".into(),
            network_state: NetworkState::Connected,
            device_features: vec![],
            healthy: true,
        }
    }
    fn abi(&self) -> String {
        "arm64".into()
    }
    fn worker_id(&self) -> Option<String> {
        None
    }
    fn logger(&self) -> &Logger {
        &self.0
    }
    fn prepare<'a>(&'a self, _: Arc<Configuration>, _: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn execute<'a>(
        &'a self,
        _: Arc<Configuration>,
        _: DevicePoolId,
        _: BatchRef,
        _: Arc<dyn Deferred<TestBatchResults>>,
        _: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn dispose(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
fn test(pkg: &str, clazz: &str, method: &str) -> Test {
    Test {
        pkg: pkg.into(),
        clazz: clazz.into(),
        method: method.into(),
        meta_properties: vec![],
    }
}
fn ok(text: impl Into<String>) -> CommandResult {
    CommandResult {
        stdout: text.into().lines().map(str::to_owned).collect(),
        stderr: vec![],
        exit_code: Some(0),
    }
}
fn fixture(name: &str) -> String {
    match name {
        "sample_1.json" => include_str!("../../../fixtures/vendor-apple-base/fixtures/xctesttool/sample_1.json"),
        "sample_2.json" => include_str!("../../../fixtures/vendor-apple-base/fixtures/xctesttool/sample_2.json"),
        "testplanrunsummary_1.json" => {
            include_str!("../../../fixtures/vendor-apple-base/fixtures/xctesttool/testplanrunsummary_1.json")
        }
        "xcode27_failure.json" => {
            include_str!("../../../fixtures/vendor-apple-base/fixtures/xctesttool/xcode27_failure.json")
        }
        "xcode27_testplanrunsummaries.json" => {
            include_str!("../../../fixtures/vendor-apple-base/fixtures/xctesttool/xcode27_testplanrunsummaries.json")
        }
        _ => panic!("unknown fixture"),
    }
    .into()
}
fn tree(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).unwrap()
}
fn ref_id(record: &Value) -> String {
    record["actions"]["_values"][0]["actionResult"]["testsRef"]["id"]["_value"]
        .as_str()
        .unwrap()
        .into()
}
struct Queries {
    root: FakeCommandExecutor,
    objects: HashMap<String, FakeCommandExecutor>,
    reject: FakeCommandExecutor,
    commands: Mutex<Vec<Vec<String>>>,
    legacy: Option<bool>,
}
impl CommandExecutor for Queries {
    fn host(&self) -> &CommandHost {
        self.root.host()
    }
    fn logger(&self) -> &Logger {
        self.root.logger()
    }
    fn connected(&self) -> bool {
        true
    }
    fn log_error(&self, _: String) {}
    fn execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession> {
        self.commands.lock().unwrap().push(request.command.clone());
        let id = request
            .command
            .iter()
            .position(|a| a == "--id")
            .map(|i| &request.command[i + 1]);
        let executor = if self
            .legacy
            .is_some_and(|required| required != request.command.iter().any(|a| a == "--legacy"))
        {
            &self.reject
        } else if let Some(id) = id {
            self.objects.get(id).unwrap_or_else(|| panic!("unexpected id {id}"))
        } else {
            &self.root
        };
        executor.execute(request, context)
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Harness {
    logging: SshLogging,
    device: DeviceRef,
    listener: Arc<TestResultsListener>,
    deferred: Arc<SharedDeferred<TestBatchResults>>,
    queries: Arc<Queries>,
    logs: LogQueue,
    messages: Arc<Mutex<Vec<(String, String)>>>,
}
fn harness(
    batch: Vec<Test>,
    root: CommandResult,
    objects: Vec<(String, CommandResult)>,
    version: Option<XcodeVersion>,
    legacy: Option<bool>,
) -> Harness {
    let mut registry = MarathonLogging::default();
    registry.configure(true, jcompat::logging::Vendor::Apple { compact_output: false });
    let device = DeviceRef(Arc::new(TestDevice(registry.logger("device", None))));
    let queries = Arc::new(Queries {
        root: FakeCommandExecutor::new(Arc::new(support::Runtime), root, &mut registry),
        objects: objects
            .into_iter()
            .map(|(id, result)| {
                (
                    id,
                    FakeCommandExecutor::new(Arc::new(support::Runtime), result, &mut registry),
                )
            })
            .collect(),
        reject: FakeCommandExecutor::new(
            Arc::new(support::Runtime),
            CommandResult {
                stdout: vec![],
                stderr: vec!["legacy query rejected".into()],
                exit_code: Some(64),
            },
            &mut registry,
        ),
        commands: Mutex::new(vec![]),
        legacy,
    });
    let messages = Arc::new(Mutex::new(vec![]));
    let output = messages.clone();
    let logging = SshLogging::new(
        Arc::new(Mutex::new(registry)),
        Arc::new(move |logger, _, msg, _| output.lock().unwrap().push((logger.name().into(), msg))),
    );
    let tools = Tools {
        executor: queries.clone(),
        timeouts: Arc::new(apple_timeout_configuration(AppleTimeoutParameters::empty())),
        runtime: Arc::new(support::Runtime),
        logging: logging.clone(),
    };
    let logs = LogQueue::default();
    let deferred = Arc::new(SharedDeferred::default());
    let listener = Arc::new(
        TestResultsListener::new(TestResultsListenerConfig {
            batch: Arc::new(TestBatch {
                tests: batch,
                id: "batch".into(),
            }),
            device: device.clone(),
            deferred: deferred.clone(),
            timer: Arc::new(FixedTimer),
            wall: Arc::new(|| 77),
            logs: logs.sender(),
            remote_xcresult: "/tmp/result.xcresult".into(),
            xcresulttool: Arc::new(Xcresulttool::new(tools)),
            xcode_version: version,
            context: support::context(),
            logging: logging.clone(),
            attachment_providers: vec![],
        })
        .unwrap(),
    );
    Harness {
        logging,
        device,
        listener,
        deferred,
        queries,
        logs,
        messages,
    }
}
impl Harness {
    fn result(&self) -> Arc<TestBatchResults> {
        self.deferred.completed().unwrap().unwrap()
    }
    fn assert_queries(&self, ids: &[String], version: Option<XcodeVersion>) {
        let mut root = vec!["xcrun", "xcresulttool", "get"];
        if version.is_some_and(|v| v >= XcodeVersion::new(16, 0, 0)) {
            root.push("--legacy");
        }
        root.extend(["--format", "json", "--path", "/tmp/result.xcresult"]);
        let root: Vec<String> = root.into_iter().map(str::to_owned).collect();
        let mut queries = vec![root.clone()];
        for id in ids {
            let mut q = root.clone();
            q.extend(["--id".into(), id.clone()]);
            queries.push(q);
        }
        assert_eq!(*self.queries.commands.lock().unwrap(), queries);
    }
}
fn failing_tests() -> Vec<Test> {
    vec![
        test("sample-appUITests", "FailingTests", "testAlwaysFailing"),
        test("sample-appUITests", "SlowTests", "testTextSlow"),
        test("sample-appUITests", "SlowTests", "testTextSlow1"),
        test("OtherTests", "SlowTests", "testTextSlow1"),
        test("sample-appUITests", "SlowTests", "testTextSlow2"),
    ]
}
fn attachment() -> Attachment {
    Attachment {
        file: std::path::PathBuf::from("/tmp/testPassed.log"),
        kind: AttachmentType::Log,
        name: Some("log".into()),
    }
}
async fn run_failing(h: &Harness) {
    let tests = failing_tests();
    let l = &h.listener;
    l.test_run_started().await.unwrap();
    l.on_attachment(&tests[1], attachment()).unwrap();
    for (i, t) in tests[..4].iter().enumerate() {
        l.test_started(t).await.unwrap();
        if i == 0 {
            l.test_failed(t, 0, 1, Some("stdout trace")).await.unwrap();
        } else {
            l.test_passed(t, i as i64, i as i64 + 1).await.unwrap();
        }
    }
    l.test_run_ended().await.unwrap();
    l.after_test_run().await.unwrap();
}
fn assert_enriched(h: &Harness, single_target: bool) {
    let r = h.result();
    let tests = failing_tests();
    let count = if single_target { 3 } else { 2 };
    assert_eq!(
        r.failed.iter().map(|r| r.test.clone()).collect::<Vec<_>>(),
        tests[..count]
    );
    let names = [
        ("FailingTests", "30", "XCTAssertTrue failed"),
        (
            "SlowTests",
            "46",
            "XCTAssertEqual failed: (\"First Button\") is not equal to (\"Label\")",
        ),
        (
            "SlowTests",
            "57",
            "XCTAssertEqual failed: (\"First Button\") is not equal to (\"Label\")",
        ),
    ];
    for (r, (class, line, msg)) in r.failed.iter().zip(names) {
        assert_eq!(
            r.stacktrace.as_ref().unwrap().to_owned(),
            format!(
                "{msg}\nfile:///Users/malinskiy/Development/marathon/sample/ios-app/sample-appUITests/{class}.swift#CharacterRangeLen=0&EndingLineNumber={line}&StartingLineNumber={line}"
            )
        );
    }
    assert_eq!(
        r.failed.iter().map(|r| r.attachments.clone()).collect::<Vec<_>>(),
        vec![vec![], vec![attachment()], vec![]][..count]
    );
    assert_eq!(
        r.finished.iter().map(|r| r.test.clone()).collect::<Vec<_>>(),
        tests[count..4]
    );
    assert_eq!(
        r.uncompleted.iter().map(|r| r.test.clone()).collect::<Vec<_>>(),
        tests[4..]
    );
}
fn assert_stdout(h: &Harness) {
    let r = h.result();
    let tests = failing_tests();
    assert_eq!(r.failed.len(), 1);
    assert_eq!(r.failed[0].stacktrace, Some("stdout trace".into()));
    assert_eq!(
        r.finished.iter().map(|r| r.test.clone()).collect::<Vec<_>>(),
        tests[1..4]
    );
    assert_eq!(r.finished[0].attachments, vec![attachment()]);
    assert_eq!(r.uncompleted[0].test, tests[4]);
}
#[tokio::test]
async fn failure_summaries_resolve_target_and_enrich_traces() {
    let record = tree("sample_1.json");
    let id = ref_id(&record);
    let h = harness(
        failing_tests(),
        ok(record.to_string()),
        vec![(id.clone(), ok(fixture("testplanrunsummary_1.json")))],
        None,
        None,
    );
    run_failing(&h).await;
    h.assert_queries(&[id], None);
    assert_enriched(&h, true);
}
async fn multiple_actions(other: bool) {
    let mut record = tree("sample_1.json");
    let id = ref_id(&record);
    let mut action = record["actions"]["_values"][0].clone();
    action["actionResult"]["testsRef"]["id"]["_value"] = json!("second-action");
    record["actions"]["_values"].as_array_mut().unwrap().push(action);
    let mut summaries = tree("testplanrunsummary_1.json");
    if other {
        summaries["summaries"]["_values"][0]["testableSummaries"]["_values"][0]["targetName"]["_value"] =
            json!("OtherTests");
    }
    let h = harness(
        failing_tests(),
        ok(record.to_string()),
        vec![
            (id.clone(), ok(fixture("testplanrunsummary_1.json"))),
            ("second-action".into(), ok(summaries.to_string())),
        ],
        None,
        None,
    );
    run_failing(&h).await;
    h.assert_queries(&[id, "second-action".into()], None);
    assert_enriched(&h, !other);
}
#[tokio::test]
async fn multiple_targets_match_failures_by_class_and_method() {
    multiple_actions(true).await;
}
#[tokio::test]
async fn multiple_actions_of_one_target_keep_target_matching() {
    multiple_actions(false).await;
}
#[tokio::test]
async fn missing_tests_ref_matches_failures_by_class_and_method() {
    let mut root = tree("sample_1.json");
    root["actions"]["_values"][0]["actionResult"]
        .as_object_mut()
        .unwrap()
        .remove("testsRef");
    let h = harness(failing_tests(), ok(root.to_string()), vec![], None, None);
    run_failing(&h).await;
    h.assert_queries(&[], None);
    assert_enriched(&h, false);
}
#[tokio::test]
async fn unreadable_target_summaries_match_failures_by_class_and_method() {
    let s = fixture("testplanrunsummary_1.json");
    for result in [
        ok(&s[..s.len() / 2]),
        CommandResult {
            stdout: s.lines().map(str::to_owned).collect(),
            stderr: vec![],
            exit_code: None,
        },
        CommandResult {
            stdout: vec![],
            stderr: vec!["Error: unknown object id".into()],
            exit_code: Some(1),
        },
        ok(""),
    ] {
        let id = ref_id(&tree("sample_1.json"));
        let h = harness(
            failing_tests(),
            ok(fixture("sample_1.json")),
            vec![(id.clone(), result)],
            None,
            None,
        );
        run_failing(&h).await;
        h.assert_queries(&[id], None);
        assert_enriched(&h, false);
    }
}
#[tokio::test]
async fn unreadable_invocation_record_keeps_log_results() {
    for result in [
        CommandResult {
            exit_code: None,
            ..ok(fixture("sample_1.json"))
        },
        ok("{"),
        ok(""),
    ] {
        let h = harness(failing_tests(), result, vec![], None, None);
        run_failing(&h).await;
        h.assert_queries(&[], None);
        assert_stdout(&h);
    }
}
async fn worker_version(version: XcodeVersion) {
    let id = ref_id(&tree("sample_1.json"));
    let h = harness(
        failing_tests(),
        ok(fixture("sample_1.json")),
        vec![(id.clone(), ok(fixture("testplanrunsummary_1.json")))],
        Some(version),
        Some(version >= XcodeVersion::new(16, 0, 0)),
    );
    run_failing(&h).await;
    h.assert_queries(&[id], Some(version));
    assert_enriched(&h, true);
}
#[tokio::test]
async fn failure_summaries_are_read_with_the_query_of_the_workers_xcode_15_4() {
    worker_version(XcodeVersion::new(15, 4, 0)).await;
}
#[tokio::test]
async fn failure_summaries_are_read_with_the_query_of_the_workers_xcode_16_0() {
    worker_version(XcodeVersion::new(16, 0, 0)).await;
}
#[tokio::test]
async fn failure_summaries_are_read_with_the_query_of_the_workers_xcode_26_6() {
    worker_version(XcodeVersion::new(26, 6, 0)).await;
}
#[tokio::test]
async fn failure_summaries_are_read_with_the_query_of_the_workers_xcode_27_0() {
    worker_version(XcodeVersion::new(27, 0, 0)).await;
}
#[tokio::test]
async fn query_without_legacy_flag_rejected_by_xcode16_keeps_log_results() {
    let h = harness(failing_tests(), ok(fixture("sample_1.json")), vec![], None, Some(true));
    run_failing(&h).await;
    h.assert_queries(&[], None);
    assert_stdout(&h);
}
#[tokio::test]
async fn xcode27_result_bundle_gives_the_trace_of_a_failed_test() {
    let test = test("sample_appUITests", "FailingTests", "testAlwaysFailing");
    let record = tree("xcode27_failure.json");
    let id = ref_id(&record);
    let version = XcodeVersion::new(27, 0, 0);
    let h = harness(
        vec![test.clone()],
        ok(record.to_string()),
        vec![(id.clone(), ok(fixture("xcode27_testplanrunsummaries.json")))],
        Some(version),
        Some(true),
    );
    h.listener.test_run_started().await.unwrap();
    h.listener.test_started(&test).await.unwrap();
    h.listener
        .test_failed(
            &test,
            0,
            1,
            Some("/Users/runner/marathon/sample/ios-app/sample-appUITests/FailingTests.swift:31:XCTAssertTrue failed"),
        )
        .await
        .unwrap();
    h.listener.test_run_ended().await.unwrap();
    h.listener.after_test_run().await.unwrap();
    h.assert_queries(&[id], Some(version));
    assert_eq!(h.result().failed[0].stacktrace,Some("XCTAssertTrue failed\nfile:///Users/runner/marathon/sample/ios-app/sample-appUITests/FailingTests.swift#EndingLineNumber=30&StartingLineNumber=30".into()));
}
#[tokio::test]
async fn clean_invocation_record_skips_target_summary_lookups() {
    for shape in [
        None,
        Some(json!({"_type":{"_name":"Array"},"_values":[]})),
        Some(Value::Null),
    ] {
        let mut root = tree("sample_2.json");
        if let Some(value) = shape {
            root["issues"]["testFailureSummaries"] = value;
        }
        let tests: Vec<_> = ["testPassed", "testFailed", "testSkipped", "testCrashed", "testNotRun"]
            .iter()
            .map(|m| test("com.example", "SampleTests", m))
            .collect();
        let h = harness(tests.clone(), ok(root.to_string()), vec![], None, None);
        let l = &h.listener;
        l.test_run_started().await.unwrap();
        l.on_attachment(&tests[0], attachment()).unwrap();
        l.test_started(&tests[0]).await.unwrap();
        l.test_passed(&tests[0], 0, 1).await.unwrap();
        l.test_started(&tests[1]).await.unwrap();
        l.test_failed(&tests[1], 1, 2, Some("assertion failed")).await.unwrap();
        l.test_started(&tests[2]).await.unwrap();
        l.test_ignored(&tests[2], 2, 3).await.unwrap();
        l.test_started(&tests[3]).await.unwrap();
        l.test_run_ended().await.unwrap();
        l.after_test_run().await.unwrap();
        h.assert_queries(&[], None);
        let r = h.result();
        assert_eq!(
            r.finished.iter().map(|r| r.status).collect::<Vec<_>>(),
            vec![TestStatus::Passed, TestStatus::Ignored]
        );
        assert_eq!(r.finished[0].attachments, vec![attachment()]);
        assert_eq!(r.failed[0].stacktrace, Some("assertion failed".into()));
        assert_eq!(
            r.uncompleted.iter().map(|r| r.test.clone()).collect::<Vec<_>>(),
            tests[3..]
        );
    }
}
struct Called(AtomicBool);
impl AppleTestRunListener for Called {
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}
#[tokio::test]
async fn keeps_log_results_on_truncated_xcresult() {
    let tests = vec![
        test("com.example", "SampleTests", "testPassed"),
        test("com.example", "SampleTests", "testFailed"),
    ];
    let record = fixture("sample_1.json");
    let h = harness(tests.clone(), ok(&record[..record.len() / 2]), vec![], None, None);
    let next = Arc::new(Called(AtomicBool::new(false)));
    let l = CompositeTestRunListener::new(vec![h.listener.clone(), next.clone()]);
    l.test_run_started().await.unwrap();
    l.test_started(&tests[0]).await.unwrap();
    l.test_passed(&tests[0], 0, 1).await.unwrap();
    l.test_started(&tests[1]).await.unwrap();
    l.test_failed(&tests[1], 1, 2, Some("assertion failed")).await.unwrap();
    l.test_run_ended().await.unwrap();
    l.after_test_run().await.unwrap();
    let r = h.result();
    assert_eq!(r.finished[0].test, tests[0]);
    assert_eq!(r.failed[0].test, tests[1]);
    assert_eq!(r.failed[0].stacktrace, Some("assertion failed".into()));
    assert!(r.uncompleted.is_empty());
    assert!(next.0.load(Ordering::SeqCst));
}
#[tokio::test]
async fn forwards_the_failure_reason_to_every_listener() {
    struct Reasons(Mutex<Vec<DeviceFailureReason>>);
    impl AppleTestRunListener for Reasons {
        fn test_run_failed<'a>(&'a self, _: &'a str, reason: DeviceFailureReason) -> VendorFuture<'a, ()> {
            Box::pin(async move {
                self.0.lock().unwrap().push(reason);
                Ok(())
            })
        }
    }
    let reasons = Arc::new(Reasons(Mutex::new(vec![])));
    let composite = CompositeTestRunListener::new(vec![reasons.clone(), reasons.clone()]);
    composite
        .test_run_failed("error", DeviceFailureReason::ServicesUnavailable)
        .await
        .unwrap();
    assert_eq!(
        *reasons.0.lock().unwrap(),
        vec![DeviceFailureReason::ServicesUnavailable; 2]
    );
}

fn oracle_rows() -> Vec<Value> {
    include_str!("../../../fixtures/oracle/apple-logparser/probes.jsonl")
        .split('\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
fn projection(results: &[TestResult]) -> Value {
    let hooks = marathon_core::executor::hooks::RuntimeHooks::live();
    json!(results.iter().map(|r|json!({"test":r.test.kotlin_value(&*hooks).unwrap().render().to_owned(),"status":match r.status {TestStatus::Passed=>"PASSED",TestStatus::Failure=>"FAILURE",TestStatus::Ignored=>"IGNORED",TestStatus::Incomplete=>"INCOMPLETE",TestStatus::AssumptionFailure=>"ASSUMPTION_FAILURE"},"start":r.start_time,"end":r.end_time,"trace":r.stacktrace.clone()})).collect::<Vec<_>>())
}
#[tokio::test]
async fn jvm_listener_production_and_test_sequences_keep_a6_and_incomplete_zero_end() {
    for row in oracle_rows().into_iter().filter(|r| r["kind"] == "projection") {
        let tests: Vec<_> = ["passed", "failed", "ignored", "crashed", "missing", "extra"]
            .iter()
            .map(|m| test("p", "C", m))
            .collect();
        let mut batch = tests[..5].to_vec();
        batch[0].meta_properties.push(MetaProperty {
            name: "annotation".into(),
            values: MetaMap::default(),
        });
        let h = harness(batch, ok(""), vec![], None, None);
        let l = &h.listener;
        let run_events = row["runEvents"].as_bool().unwrap();
        if run_events {
            l.test_run_started().await.unwrap();
        }
        l.test_started(&tests[0]).await.unwrap();
        l.test_passed(&tests[0], 10, 20).await.unwrap();
        l.test_started(&tests[1]).await.unwrap();
        l.test_failed(&tests[1], 30, 40, Some("stdout")).await.unwrap();
        l.test_started(&tests[2]).await.unwrap();
        l.test_ignored(&tests[2], 50, 60).await.unwrap();
        l.test_started(&tests[3]).await.unwrap();
        l.test_passed(&tests[5], 70, 80).await.unwrap();
        if run_events {
            l.test_run_ended().await.unwrap();
        }
        l.after_test_run().await.unwrap();
        let result = h.result();
        assert_eq!(projection(&result.finished), row["finished"]);
        assert_eq!(projection(&result.failed), row["failed"]);
        assert_eq!(projection(&result.uncompleted), row["uncompleted"]);
        let actual: Vec<_> = h
            .messages
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, m)| m.starts_with("uncompleted ="))
            .map(|(_, m)| m.clone())
            .collect();
        let expected: Vec<_> = row["logs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["level"] == "WARN")
            .map(|r| r["message"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(actual, expected);
        assert!(h.logs.records().is_empty());
    }
}
#[tokio::test]
async fn jvm_debug_lines_and_approved_artifact_order() {
    use marathon_core::listener::LineListener;
    let h = harness(vec![], ok(""), vec![], None, None);
    // Reuse the same per-run registry/sink as a device's listener construction.
    let logging = h.logging.clone();
    let t = test("p", "C", "m");
    let debug = DebugTestRunListener::new(h.device.clone(), logging.clone(), support::context().hooks);
    debug.before_test_run().await.unwrap();
    debug.test_run_started().await.unwrap();
    debug.test_started(&t).await.unwrap();
    let mut annotated = t.clone();
    annotated.meta_properties.push(MetaProperty {
        name: "tag".into(),
        values: vec![("value".into(), MetaValue::String("v".into()))]
            .into_iter()
            .collect(),
    });
    debug.test_started(&annotated).await.unwrap();
    debug.test_failed(&t, 10, 20, None).await.unwrap();
    debug.test_failed(&t, 10, 20, Some("first\nsecond")).await.unwrap();
    debug.test_passed(&t, 10, 20).await.unwrap();
    debug.test_ignored(&t, 10, 20).await.unwrap();
    debug
        .test_run_failed("error", DeviceFailureReason::ServicesUnavailable)
        .await
        .unwrap();
    debug.test_run_ended().await.unwrap();
    debug.after_test_run().await.unwrap();
    let row = oracle_rows().into_iter().find(|r| r["kind"] == "debug").unwrap();
    let actual: Vec<_> = h
        .messages
        .lock()
        .unwrap()
        .iter()
        .filter(|(n, _)| n == "DebugTestRunListener")
        .map(|(_, m)| m.clone())
        .collect();
    let expected: Vec<_> = row["logs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["message"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(actual, expected);
    for row in oracle_rows().into_iter().filter(|r| r["kind"] == "artifact") {
        let finder = SessionResultsPathFinder::new(logging.clone());
        h.messages.lock().unwrap().clear();
        let paths: Vec<_> = row["paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap().to_owned())
            .collect();
        for path in &paths {
            finder.on_line(&format!("    {path}")).await.unwrap();
            finder.on_line(&format!("    {path}")).await.unwrap();
        }
        finder.after_test_run().await.unwrap();
        assert_eq!(
            h.messages.lock().unwrap().as_slice(),
            &[(
                "com.malinskiy.marathon.apple.listener.ArtifactFinderListener".into(),
                format!("Session results available at {}", paths.join(", "))
            )]
        );
    }
}

#[tokio::test]
async fn composite_stops_on_first_error_and_adapter_synthesizes_only_terminal_callbacks() {
    use marathon_core::{error::VendorError, listener::TestRunListener};
    struct Fail;
    impl AppleTestRunListener for Fail {
        fn after_test_run(&self) -> VendorFuture<'_, ()> {
            Box::pin(async { Err(VendorError::Other(marathon_apple::cmd::failure("test.Error", "stop"))) })
        }
    }
    let next = Arc::new(Called(AtomicBool::new(false)));
    let composite = CompositeTestRunListener::new(vec![Arc::new(Fail), next.clone()]);
    assert!(composite.after_test_run().await.is_err());
    assert!(!next.0.load(Ordering::SeqCst));
    struct CoreRecorder(Mutex<Vec<&'static str>>);
    impl TestRunListener for CoreRecorder {
        fn before_test_run(&self) -> VendorFuture<'_, ()> {
            Box::pin(async move {
                self.0.lock().unwrap().push("before");
                Ok(())
            })
        }
        fn test_started<'a>(&'a self, _: &'a Test) -> VendorFuture<'a, ()> {
            Box::pin(async move {
                self.0.lock().unwrap().push("start");
                Ok(())
            })
        }
        fn test_ended<'a>(&'a self, _: &'a Test) -> VendorFuture<'a, ()> {
            Box::pin(async move {
                self.0.lock().unwrap().push("end");
                Ok(())
            })
        }
        fn after_test_run(&self) -> VendorFuture<'_, ()> {
            Box::pin(async move {
                self.0.lock().unwrap().push("after");
                Ok(())
            })
        }
    }
    let core = Arc::new(CoreRecorder(Mutex::new(vec![])));
    let adapter = TestRunListenerAdapter { listener: core.clone() };
    let t = test("p", "C", "m");
    adapter.before_test_run().await.unwrap();
    adapter.test_run_started().await.unwrap();
    adapter.test_started(&t).await.unwrap();
    adapter.test_failed(&t, 1, 2, None).await.unwrap();
    adapter.test_passed(&t, 1, 2).await.unwrap();
    adapter.test_ignored(&t, 1, 2).await.unwrap();
    adapter
        .test_run_failed("failure", DeviceFailureReason::Unknown)
        .await
        .unwrap();
    adapter.test_run_ended().await.unwrap();
    adapter.after_test_run().await.unwrap();
    assert_eq!(
        *core.0.lock().unwrap(),
        vec!["before", "start", "end", "end", "end", "after"]
    );
}
#[tokio::test]
async fn accumulator_forwards_stamped_logs_even_when_callback_fails() {
    use marathon_core::{execution::result::TestRunResultsAccumulator, time::ReplayError};
    struct FailAfterCreation(std::sync::atomic::AtomicUsize);
    impl Timer for FailAfterCreation {
        fn start_time_millis(&self) -> i64 {
            0
        }
        fn elapsed_time_millis(&self) -> ClockResult<i64> {
            Ok(0)
        }
        fn current_time_millis(&self) -> ClockResult<i64> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(0)
            } else {
                Err(ReplayError {
                    label: "clock".into(),
                    message: "exhausted".into(),
                })
            }
        }
        fn measure(&self, _: &mut dyn FnMut()) -> ClockResult<i64> {
            Ok(0)
        }
    }
    let logs = LogQueue::default();
    let a = TestRunResultsAccumulator::new(
        Arc::new(FailAfterCreation(std::sync::atomic::AtomicUsize::new(0))),
        Arc::new(|| 77),
    )
    .unwrap();
    let listener = AccumulatingTestResultListener::new(1, a, logs.sender());
    assert!(listener.test_failed(&test("p", "C", "m"), 1, 2, None).await.is_err());
    let records = logs.take();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].wall_millis, 77);
    assert_eq!(records[0].at_millis, 0);
    assert_eq!(
        records[0].logger,
        "com.malinskiy.marathon.execution.result.TestRunResultsAccumulator"
    );
    assert_eq!(
        records[0].message.to_owned(),
        "received test event without test start for p.C.m"
    );
}
#[tokio::test]
async fn diagnostic_lines_and_debug_printer_share_arc_and_initialize_lazily() {
    use marathon_apple::logparser::parser::DebugLogPrinter;
    use marathon_core::listener::LineListener;
    let h = harness(vec![], ok(""), vec![], None, None);
    let hidden = DebugLogPrinter::new(Some("hidden".into()), true, h.logging.clone());
    hidden.on_line("secret").await.unwrap();
    assert!(!h.logging.registry.lock().unwrap().is_registered("hidden"));
    let printer = DebugLogPrinter::new(Some("stdout".into()), false, h.logging.clone());
    assert!(!h.logging.registry.lock().unwrap().is_registered("stdout"));
    printer.on_line("first").await.unwrap();
    printer.on_line("second").await.unwrap();
    let finder = Arc::new(DiagnosticLogsPathFinder::new(h.logging.clone()));
    let line: Arc<dyn LineListener> = finder.clone();
    let run: Arc<dyn AppleTestRunListener> = finder;
    line.on_line("prefix /tmp/diagnostic.log\r").await.unwrap();
    line.on_line(" /tmp/diagnostic.log").await.unwrap();
    run.after_test_run().await.unwrap();
    assert_eq!(
        *h.messages.lock().unwrap(),
        vec![
            ("stdout".into(), "first".into()),
            ("stdout".into(), "second".into()),
            (
                "com.malinskiy.marathon.apple.listener.ArtifactFinderListener".into(),
                "Diagnostic logs available at /tmp/diagnostic.log".into()
            )
        ]
    );
}
#[tokio::test]
async fn null_failure_entries_propagate_and_null_messages_follow_jvm_defaults() {
    for shape in ["missing", "null", "wrapped-null"] {
        let mut root = tree("sample_1.json");
        let summary = &mut root["issues"]["testFailureSummaries"]["_values"][0];
        if shape == "missing" {
            summary.as_object_mut().unwrap().remove("message");
        } else if shape == "null" {
            summary["message"] = Value::Null;
        } else {
            summary["message"]["_value"] = Value::Null;
        }
        let parsed = marathon_apple::xcresult::decode_legacy::<marathon_apple::xcresult::ActionsInvocationRecord>(
            &root.to_string(),
        );
        let oracle = oracle_rows()
            .into_iter()
            .find(|r| r["case"] == format!("message/{shape}"))
            .unwrap();
        if oracle.get("exception").is_some() {
            assert!(parsed.is_err());
        } else {
            assert_eq!(
                parsed.unwrap().unwrap().issues.test_failure_summaries[0]
                    .as_ref()
                    .unwrap()
                    .message
                    .to_owned(),
                oracle["message"].as_str().unwrap()
            );
        }
    }
    let mut root = tree("sample_1.json");
    root["issues"]["testFailureSummaries"]["_values"] = json!([null]);
    root["actions"]["_values"] = json!([]);
    let h = harness(vec![], ok(root.to_string()), vec![], None, None);
    let completed = test("p", "C", "m");
    h.listener.test_passed(&completed, 1, 2).await.unwrap();
    assert!(h.listener.after_test_run().await.is_err());
    assert!(h.deferred.completed().is_none());
}
#[tokio::test]
async fn enrichment_appends_summary_messages_and_urls_in_source_order() {
    let mut root = tree("sample_1.json");
    root["actions"]["_values"] = json!([]);
    let mut summary = root["issues"]["testFailureSummaries"]["_values"][0].clone();
    summary["message"]["_value"] = json!("second");
    summary["documentLocationInCreatingWorkspace"]["url"]["_value"] = json!(" \t ");
    root["issues"]["testFailureSummaries"]["_values"]
        .as_array_mut()
        .unwrap()
        .push(summary);
    let h = harness(failing_tests(), ok(root.to_string()), vec![], None, None);
    run_failing(&h).await;
    let trace = h.result().failed[0].stacktrace.as_ref().unwrap().to_owned();
    assert!(trace.ends_with("\nsecond"));
    assert_eq!(trace.matches("XCTAssertTrue failed").count(), 1);
    let messages = h.messages.lock().unwrap();
    assert!(
        messages
            .iter()
            .any(|(_, m)| m == "No testable targets found in xcresult")
    );
    assert!(messages.iter().any(|(_, m)| m
        == "Multiple matching test cases found in xcresult. Impossible to possible to add stacktrace to report"));
    assert!(
        messages
            .iter()
            .any(|(_, m)| m == "No matching test cases found in xcresult")
    );
}

#[tokio::test]
async fn disabled_debug_listener_does_not_log_decoded_null_metadata() {
    let h = harness(vec![], ok(""), vec![], None, None);
    h.logging
        .registry
        .lock()
        .unwrap()
        .configure(false, jcompat::logging::Vendor::Apple { compact_output: false });
    let debug = DebugTestRunListener::new(h.device, h.logging, support::context().hooks);
    let mut t = test("p", "C", "m");
    t.meta_properties.push(MetaProperty {
        name: "annotation".into(),
        values: vec![("null".into(), MetaValue::Decoded(DecodedWrapper::DecodedNull))]
            .into_iter()
            .collect(),
    });
    debug.test_started(&t).await.unwrap();
    assert!(h.messages.lock().unwrap().is_empty());
}
