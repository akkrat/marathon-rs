//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/command/parse/MarathonTestParseCommandTest.kt
//!
//! The command alone, not through Marathon's no-tests guard. Besides the source's
//! `trimIndent()` comparison, each file is checked against the JVM's exact bytes
//! (fixtures/oracle/run-flow/parse-command.jsonl).
use jcompat::{file_manager::JavaPath, logging::MarathonLogging, writers::JavaString};
use marathon_core::{
    analytics::MetricsContext,
    execution::parse_command::{EXTENSION, MarathonTestParseCommand},
    executor::{CoreExecutor, hooks::RuntimeHooks, shared::Signal},
    model::Test,
    wiring::ParseOutput,
};
use std::sync::{Arc, Mutex};

const RESULT_FILE_NAME: &str = "result";

fn test(method: &str) -> Test {
    Test {
        pkg: "com.example".into(),
        clazz: "SimpleTest".into(),
        method: method.into(),
        meta_properties: vec![],
    }
}

/// `execute(tests, flakyTests, RESULT_FILE_NAME)` into a fresh temporary root; the file's text.
fn execute(tests: Vec<Test>, flaky_tests: Vec<Test>) -> String {
    let root = tempfile::tempdir().unwrap();
    let output = JavaPath::from_text(&root.path().to_string_lossy());
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let context = MetricsContext {
                clock: core.vendor_clock(),
                vendor: core.vendor(),
                monotonic: RuntimeHooks::live(),
                cancellation: Signal::new(),
                registry: Arc::new(Mutex::new(MarathonLogging::default())),
                logs: core.log_queue().sender(),
                stamp: Arc::new(|| (0, 0)),
            };
            let command = MarathonTestParseCommand::new(core.vendor(), output, JavaPath::from_text("/"), &context);
            let name = JavaString::from(RESULT_FILE_NAME);
            let (tests, flaky_tests) = (tests, flaky_tests);
            let io = core.vendor();
            marathon_core::executor::bridge::call_vendor(io.as_ref(), Signal::new(), move |_| {
                Box::pin(async move { command.execute(&tests, &flaky_tests, Some(&name)).await })
            })
            .await
            .map_err(|e| marathon_core::error::OperationError::Failed(e.failure().clone()))
        })
    })
    .unwrap();
    let file = root.path().join(format!("{RESULT_FILE_NAME}{EXTENSION}"));
    assert!(file.exists());
    std::fs::read_to_string(file).unwrap()
}

fn oracle(name: &str) -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/oracle/run-flow/parse-command.jsonl"
    );
    let text = std::fs::read_to_string(path).unwrap();
    for line in text.lines().skip(1) {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        if row["case"] == name {
            return row["content"].as_str().unwrap().to_owned();
        }
    }
    panic!("no oracle row {name}")
}

/// Kotlin's `String.trimIndent()` on these single-margin documents: blank first/last lines go.
fn trim_indent(text: &str) -> String {
    text.trim_matches('\n').to_owned()
}

#[test]
fn test_parse_command_should_create_a_file_with_zero_tests() {
    let content = execute(vec![], vec![]);
    assert_eq!(trim_indent(&content), "---\ntests: []");
    assert_eq!(content, oracle("zero"));
}

#[test]
fn test_parse_command_should_create_a_file_with_one_test() {
    let content = execute(vec![test("method1")], vec![]);
    assert_eq!(
        trim_indent(&content),
        "---\ntests:\n- pkg: \"com.example\"\n  clazz: \"SimpleTest\"\n  method: \"method1\"\n  metaProperties: []"
    );
    assert_eq!(content, oracle("one"));
}

const SEVERAL: &str = "---\ntests:\n- pkg: \"com.example\"\n  clazz: \"SimpleTest\"\n  method: \"method1\"\n  metaProperties: []\n- pkg: \"com.example\"\n  clazz: \"SimpleTest\"\n  method: \"method2\"\n  metaProperties: []\n- pkg: \"com.example\"\n  clazz: \"SimpleTest\"\n  method: \"method3\"\n  metaProperties: []";

#[test]
fn test_parse_command_should_create_a_file_with_several_tests() {
    let content = execute(vec![test("method1"), test("method2"), test("method3")], vec![]);
    assert_eq!(trim_indent(&content), SEVERAL);
    assert_eq!(content, oracle("several"));
}

#[test]
fn test_parse_command_should_create_a_file_with_several_tests_and_flaky_tests() {
    let content = execute(
        vec![test("method1"), test("method2"), test("method3")],
        vec![test("method1"), test("method2")],
    );
    let expected = format!(
        "{SEVERAL}\nflakyTests:\n- pkg: \"com.example\"\n  clazz: \"SimpleTest\"\n  method: \"method1\"\n  metaProperties: []\n- pkg: \"com.example\"\n  clazz: \"SimpleTest\"\n  method: \"method2\"\n  metaProperties: []"
    );
    assert_eq!(trim_indent(&content), expected);
    assert_eq!(content, oracle("several-flaky"));
}
