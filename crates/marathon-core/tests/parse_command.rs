//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/command/parse/MarathonTestParseCommandTest.kt
//!
//! The command alone, not through Marathon's no-tests guard. Compare each file's
//! ordered YAML data with fixtures/oracle/run-flow/parse-command.jsonl (WP 8.9).
use marathon_core::{
    analytics::MetricsContext,
    execution::parse_command::{EXTENSION, MarathonTestParseCommand},
    executor::{CoreExecutor, hooks::RuntimeHooks, shared::Signal},
    model::Test,
    wiring::ParseOutput,
};
use marathon_logging::LogSettings;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

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
fn execute(tests: Vec<Test>, flaky_tests: Vec<Test>) -> TestResult<String> {
    let root = tempfile::tempdir()?;
    let output = PathBuf::from(root.path().to_string_lossy().into_owned());
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let context = MetricsContext {
                clock: core.vendor_clock(),
                vendor: core.vendor(),
                monotonic: RuntimeHooks::live(),
                cancellation: Signal::new(),
                registry: Arc::new(Mutex::new(LogSettings::default())),
                logs: core.log_queue().sender(),
                stamp: Arc::new(|| (0, 0)),
            };
            let command = MarathonTestParseCommand::new(core.vendor(), output, PathBuf::from("/"), &context);
            let name = String::from(RESULT_FILE_NAME);
            let (tests, flaky_tests) = (tests, flaky_tests);
            let io = core.vendor();
            marathon_core::executor::bridge::call_vendor(io.as_ref(), Signal::new(), move |_| {
                Box::pin(async move { command.execute(&tests, &flaky_tests, Some(&name)).await })
            })
            .await
            .map_err(|e| marathon_core::error::OperationError::Failed(e.failure().clone()))
        })
    })
    .map_err(|e| format!("{e:?}"))?;
    let file = root.path().join(format!("{RESULT_FILE_NAME}{EXTENSION}"));
    assert!(file.exists());
    Ok(std::fs::read_to_string(file)?)
}

fn oracle(name: &str) -> String {
    let text = include_str!("../../../fixtures/oracle/run-flow/parse-command.jsonl");
    for line in text.lines().skip(1) {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        if row["case"] == name {
            return row["content"].as_str().unwrap().to_owned();
        }
    }
    panic!("no oracle row {name}")
}

#[test]
fn test_parse_command_should_create_a_file_with_zero_tests() -> TestResult {
    marathon_test_support::yaml::same_data(&execute(vec![], vec![])?, &oracle("zero"))
}
#[test]
fn test_parse_command_should_create_a_file_with_one_test() -> TestResult {
    marathon_test_support::yaml::same_data(&execute(vec![test("method1")], vec![])?, &oracle("one"))
}
#[test]
fn test_parse_command_should_create_a_file_with_several_tests() -> TestResult {
    marathon_test_support::yaml::same_data(
        &execute(vec![test("method1"), test("method2"), test("method3")], vec![])?,
        &oracle("several"),
    )
}
#[test]
fn test_parse_command_should_create_a_file_with_several_tests_and_flaky_tests() -> TestResult {
    marathon_test_support::yaml::same_data(
        &execute(
            vec![test("method1"), test("method2"), test("method3")],
            vec![test("method1"), test("method2")],
        )?,
        &oracle("several-flaky"),
    )
}
